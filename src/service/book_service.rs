use crate::crawler::{
    fetcher::{fetch, FetchResponse, RequestSpec, StrResponse},
    http_client::HttpClient,
    url_analyzer::analyze_url,
};
use crate::error::error::AppError;
use crate::model::{
    book::Book,
    book_chapter::BookChapter,
    book_source::{BookSource, ExploreKind},
    chapter_image::ChapterImages,
    review::{ParaReviewCount, ParaReviewIndex, ReviewPage, ReviewResponse, ReviewSort},
    search::SearchBook,
};
use crate::parser::js::{eval_js, eval_js_with_bindings, with_book_source, with_user_ns};
use crate::parser::rule_engine::RuleEngine;
use crate::storage::cache::file_cache::{
    remove_dir_counting_files, remove_file_counting, CacheUsage, FileCache,
};
use crate::storage::cache::review_cache::ReviewCache;
use crate::storage::db::repo::{BookRepo, RegisterBook, ShelfBookRepo, ShelfBookRow, ShelfEntry};
use crate::util::bounded_map::BoundedMap;
use crate::util::hash::md5_hex;
use crate::util::text::{normalize_source_url, repair_encoded_url};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::fs;
use tokio::sync::RwLock;
use tokio::time::{sleep, Duration, Instant};

/// State for background chapter fetching
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct ChapterPagination {
    pub user_ns: String,
    pub source: BookSource,
    pub toc_url: String,
    pub visited_urls: Vec<String>,
    pub pending_urls: Vec<String>,
    pub seen_chapter_urls: Vec<String>,
    pub next_index: i32,
}

#[derive(Clone)]
pub struct BookService {
    http: HttpClient,
    parser: RuleEngine,
    cache: FileCache,
    review_cache: ReviewCache,
    storage_dir: PathBuf,
    book_repo: BookRepo,
    shelf_repo: ShelfBookRepo,
    source_cookies: Arc<RwLock<HashMap<String, String>>>,
    rate_states: Arc<RwLock<HashMap<String, RateState>>>,
    /// 最近抓到的章节正文响应体，用于求值评论 URL（见 `chapter_source_body`）。
    recent_bodies: Arc<RwLock<HashMap<String, RecentBody>>>,
    /// 单用户书架上限（0 = 不限），来自 `USER_BOOK_LIMIT`。
    user_book_limit: u32,
    /// 单用户本地书上限（0 = 不限），来自 `USER_LOCAL_BOOK_LIMIT`。
    user_local_book_limit: u32,
    /// 正在后台补全目录的 `(user_ns, book_id)` 集合：防止同一本书并发触发
    /// 两个补全任务（竞态下各自抓全部分页并各 append 一次，缓存里出现重复章节）。
    pending_toc_fills: Arc<Mutex<HashSet<(String, String)>>>,
}

/// 内存里暂存的章节正文响应体。
struct RecentBody {
    body: String,
    url: String,
    stored_at: Instant,
}

/// 评论缓存的默认有效期（7 天）与单用户容量上限（64 MiB）。
const DEFAULT_REVIEW_CACHE_TTL_SECS: u64 = 7 * 24 * 60 * 60;
const DEFAULT_REVIEW_CACHE_BYTES: u64 = 64 * 1024 * 1024;
/// 评论缓存在 `<storage>/cache` 下的子树名。
///
/// 它与 `<cache>/<ns>/<book_key>/` 是两棵独立的树，所以 `FileCache::users()`
/// 会把 `reviews` 当成一个用户命名空间列出来——`purge_all_cache` 因此要跳过它。
const REVIEW_CACHE_DIR: &str = "reviews";
/// 评论每页条数；站点通常另有上限（番茄是 50）。
pub const REVIEW_PAGE_SIZE: i32 = 20;

/// 「只看作者」扫描的上限：25 页 × 50 条 = 一章最多扫 1250 条评论。
/// 上游没有作者过滤参数，只能逐页翻；给上限是防止上万条的爆火章节
/// 把请求打穿（25 个串行请求本身已经够慢了）。
const AUTHOR_SCAN_MAX_PAGES: i32 = 25;
const AUTHOR_SCAN_PAGE_SIZE: i32 = 50;
/// 正文响应体内存暂存的条数、单条上限与存活时间。
const RECENT_BODY_LIMIT: usize = 32;
const RECENT_BODY_MAX_BYTES: usize = 1024 * 1024;
const RECENT_BODY_TTL: Duration = Duration::from_secs(600);

/// 书籍详情缓存的存活时间（1 小时）与目录名。
///
/// 详情页、仅传 bookUrl 拉目录、翻章按 index 换算 chapterUrl 都会走
/// `get_book_info`，此前每次都打上游。详情不常变，短 TTL 足够新；
/// `refresh=1` 与 saveBook/换源路径走旁路强刷。文件随 `CacheKind::ChapterList`
/// 清理（详情与目录同属书籍元数据）。
const BOOK_INFO_CACHE_TTL: Duration = Duration::from_secs(3600);
const BOOK_INFO_CACHE_DIR: &str = "bookinfo";

/// 章节列表缓存的存活时间（1 小时）。
///
/// 目录文件本体不带时间戳，过期判定用文件 mtime；只作用于「把缓存直接
/// 喂给响应」的读路径（过期按未命中处理，重抓后整份覆盖写回）。
/// 供反向查找、后台补全使用的 `load_chapter_list_cache` 不做 TTL——
/// 那些路径宁可读到旧目录，也不该顺手删掉还能用的文件。
const CHAPTER_LIST_CACHE_TTL: Duration = Duration::from_secs(3600);

/// 「作者相关」的评论：作者本人发的、作者赞过的、或有内联回复来自作者/被作者赞过。
///
/// 「作者回复过」只能看到内联回复预览里的那几条（站点的完整回复串不在
/// 列表接口里），这是数据源头的限制。
fn is_author_related(item: &crate::model::review::ReviewItem) -> bool {
    item.author
        || item.author_digg
        || item
            .replies
            .iter()
            .any(|reply| reply.author || reply.author_digg)
}

/// 书源的评论 URL 模板是否用到了 `{{sort}}`。
///
/// 用了说明排序由站点自己做；没用的话「最新」只能由客户端对已加载的条目重排。
fn rule_consumes_sort(rule: Option<&str>) -> bool {
    // `{{sort}}` 与带映射的 `{{sort === ...}}` 都算；前缀匹配而非全文比对，
    // 是因为站点取值往往不是 hot/time 本身（番茄的段评枚举叫 Hot/TimeDesc）
    rule.map(|rule| rule.contains("{{sort") || rule.contains("@get:{sort}"))
        .unwrap_or(false)
}

/// 按字符截断，用于段落定位锚点。
fn truncate_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    text.chars().take(limit).collect()
}

#[derive(Clone, Default)]
struct RateState {
    in_flight: bool,
    last_start: Option<Instant>,
    window_starts: Vec<Instant>,
}

/// `in_flight` 的复位保险（RAII）：正常路径 `disarm` 后由 `finish_rate` 复位；
/// fetch panic / 任务被取消走 Drop，另起任务把标志复位，避免限速器永久卡死。
struct SerialRateGuard {
    states: Arc<RwLock<HashMap<String, RateState>>>,
    key: String,
    armed: bool,
}

impl SerialRateGuard {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for SerialRateGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let states = self.states.clone();
        let key = std::mem::take(&mut self.key);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                if let Some(state) = states.write().await.get_mut(&key) {
                    state.in_flight = false;
                }
            });
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BookSourceAvailability {
    pub book_source_url: String,
    pub book_source_name: String,
    pub valid: bool,
    pub search_ok: bool,
    pub explore_ok: bool,
    pub keyword: String,
    pub explore_url: Option<String>,
    pub search_error: Option<String>,
    pub explore_error: Option<String>,
}

/// 缓存清理的粒度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CacheKind {
    Content,
    Cover,
    ChapterList,
    SearchResults,
    Review,
}

/// 各层缓存被删除的文件数。
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CachePurgeResult {
    pub content: u64,
    pub cover: u64,
    pub chapter_list: u64,
    pub search_results: u64,
    pub review: u64,
    pub invalid_sources: u64,
}

impl CachePurgeResult {
    fn add(&mut self, other: Self) {
        self.content += other.content;
        self.cover += other.cover;
        self.chapter_list += other.chapter_list;
        self.search_results += other.search_results;
        self.review += other.review;
        self.invalid_sources += other.invalid_sources;
    }
}

fn internal_error(error: anyhow::Error) -> AppError {
    AppError::Internal(error)
}

/// 把同步的规则解析放到阻塞线程池执行，避免独占 tokio worker 线程。
///
/// 规则解析包含 CSS/XPath/JSONPath/正则以及 QuickJS 求值，全都是同步 CPU 工作。
/// 直接跑在 async 上下文里时，一个 worker 会被一条请求长期占住；配合书源里
/// 可控的 JS 就会演变成服务级 DoS。
async fn parse_blocking<T, F>(f: F) -> Result<T, AppError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("解析任务执行失败: {e}")))
}

impl BookService {
    pub fn new(
        http: HttpClient,
        parser: RuleEngine,
        cache: FileCache,
        storage_dir: &str,
        book_repo: BookRepo,
        shelf_repo: ShelfBookRepo,
    ) -> Self {
        let storage_dir = PathBuf::from(storage_dir);
        let review_cache = ReviewCache::new(
            storage_dir.join("cache").join("reviews"),
            DEFAULT_REVIEW_CACHE_TTL_SECS,
            DEFAULT_REVIEW_CACHE_BYTES,
        );
        Self {
            http,
            parser,
            cache,
            review_cache,
            storage_dir,
            book_repo,
            shelf_repo,
            source_cookies: Arc::new(RwLock::new(HashMap::new())),
            rate_states: Arc::new(RwLock::new(HashMap::new())),
            recent_bodies: Arc::new(RwLock::new(HashMap::new())),
            user_book_limit: 0,
            user_local_book_limit: 0,
            pending_toc_fills: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// 设置评论缓存的有效期与单用户容量上限（0 = 不过期 / 不限制）。
    pub fn with_review_cache(mut self, ttl_secs: u64, max_user_bytes: u64) -> Self {
        self.review_cache = ReviewCache::new(
            self.storage_dir.join("cache").join("reviews"),
            ttl_secs,
            max_user_bytes,
        );
        self
    }

    /// 设置单用户书架上限（`USER_BOOK_LIMIT`；0 = 不限）。
    pub fn with_user_book_limit(mut self, limit: u32) -> Self {
        self.user_book_limit = limit;
        self
    }

    /// 设置单用户本地书上限（`USER_LOCAL_BOOK_LIMIT`；0 = 不限）。
    pub fn with_user_local_book_limit(mut self, limit: u32) -> Self {
        self.user_local_book_limit = limit;
        self
    }

    /// 取某个用户命名空间下的独立 HTTP 客户端（独立 Cookie jar）。
    pub fn http_client(&self, user_ns: &str) -> anyhow::Result<reqwest::Client> {
        self.http.client_for(user_ns)
    }

    // ── 书籍登记表（bookId 的分配与解析） ────────────────────────────────

    pub fn book_repo(&self) -> &BookRepo {
        &self.book_repo
    }

    /// 登记一本书并返回 bookId；URL 已登记时复用原 id 并补全可变字段。
    pub async fn register_book(
        &self,
        user_ns: &str,
        item: RegisterBook<'_>,
    ) -> Result<String, AppError> {
        self.book_repo.register(user_ns, item, None).await
    }

    /// 批量登记搜索/发现结果，把 bookId/bookSourceId 写回结果对象。
    pub async fn register_search_books(
        &self,
        user_ns: &str,
        source: &BookSource,
        books: &mut [SearchBook],
    ) -> Result<(), AppError> {
        let items: Vec<RegisterBook> = books
            .iter()
            .filter(|b| !b.book_url.trim().is_empty())
            .map(|b| RegisterBook {
                book_url: &b.book_url,
                book_source_id: source.book_source_id,
                name: Some(&b.name),
                author: Some(&b.author),
                toc_url: None,
            })
            .collect();
        if items.is_empty() {
            return Ok(());
        }
        let ids = self.book_repo.register_many(user_ns, &items).await?;
        for book in books.iter_mut() {
            book.book_id = ids.get(&book.book_url).cloned();
            book.book_source_id = source.book_source_id;
        }
        Ok(())
    }

    fn source_cookie_key(&self, user_ns: &str, source_url: &str) -> String {
        format!("{}::{}", user_ns, cookie_domain(source_url))
    }

    async fn apply_source_cookie(
        &self,
        user_ns: &str,
        source: &BookSource,
        headers: &mut Vec<(String, String)>,
    ) {
        let key = self.source_cookie_key(user_ns, &source.book_source_url);
        if let Some(cookie) = self.source_cookies.read().await.get(&key).cloned() {
            if !headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("cookie"))
            {
                headers.push(("Cookie".to_string(), cookie));
            }
        }
    }

    pub async fn set_source_cookie(&self, user_ns: &str, source_url: &str, cookie: &str) {
        let cookie = cookie.trim();
        if cookie.is_empty() {
            return;
        }
        let key = self.source_cookie_key(user_ns, source_url);
        self.source_cookies
            .write()
            .await
            .insert(key, cookie.to_string());
    }

    /// 在阻塞线程池里执行 URL 规则分析（内部可能求值 JS）。
    ///
    /// 一并把 `user_ns` 带进阻塞线程，书源 JS 的 cache/kv 与 java.* 请求
    /// 据此隔离到当前用户。
    async fn analyze_url_blocking(
        &self,
        user_ns: &str,
        url_rule: &str,
        key: &str,
        page: i32,
        base_url: &str,
        source: &BookSource,
    ) -> Result<RequestSpec, AppError> {
        let user_ns = user_ns.to_string();
        let url_rule = url_rule.to_string();
        let key = key.to_string();
        let base_url = base_url.to_string();
        let source = source.clone();
        parse_blocking(move || {
            with_user_ns(&user_ns, || {
                analyze_url(&url_rule, &key, page, &base_url, &source)
            })
        })
        .await?
    }

    /// 在阻塞线程池里执行书源登录检查 JS。
    ///
    /// 出口统一校验状态码：上游 403/404/限流在此之前被当成成功结果继续
    /// 解析，错误页 HTML 解析出空串后「成功」返回空白章节。JS 先跑
    /// （书源有机会改写响应自救），之后仍非 2xx 才透传为可读错误。
    async fn login_check_blocking(
        &self,
        user_ns: &str,
        source: &BookSource,
        res: FetchResponse,
    ) -> Result<FetchResponse, AppError> {
        let user_ns = user_ns.to_string();
        let source = source.clone();
        parse_blocking(move || with_user_ns(&user_ns, || apply_login_check_js(&source, res)))
            .await
            .and_then(ensure_successful_response)
    }

    /// 在阻塞线程池里对 (source, body, url) 跑一段同步解析。
    async fn parse_response_blocking<T, F>(
        &self,
        user_ns: &str,
        source: &BookSource,
        res: &FetchResponse,
        f: F,
    ) -> Result<T, AppError>
    where
        F: FnOnce(&RuleEngine, &BookSource, &str, &str) -> T + Send + 'static,
        T: Send + 'static,
    {
        let user_ns = user_ns.to_string();
        let parser = self.parser.clone();
        let source = source.clone();
        let body = res.body.clone();
        let url = res.url.clone();
        parse_blocking(move || with_user_ns(&user_ns, || f(&parser, &source, &body, &url))).await
    }

    /// 在阻塞线程池里对 (source, body, base_url) 求值一段可能跑 JS 的规则。
    ///
    /// 评论 URL 模板（`{{表达式}}`）与段评补段文本（`content`）都走这里。
    /// 必须带 `user_ns`：不带时书源 JS 的 cache/kv 会落进共享的 "public"
    /// 桶（跨用户串号），且同步求值会直接堵在 tokio worker 上。
    async fn parse_body_blocking<T, F>(
        &self,
        user_ns: &str,
        source: &BookSource,
        body: &str,
        base_url: &str,
        ctx: &HashMap<String, String>,
        f: F,
    ) -> Result<T, AppError>
    where
        F: FnOnce(&RuleEngine, &BookSource, &str, &str, &HashMap<String, String>) -> T
            + Send
            + 'static,
        T: Send + 'static,
    {
        let user_ns = user_ns.to_string();
        let parser = self.parser.clone();
        let source = source.clone();
        let body = body.to_string();
        let base_url = base_url.to_string();
        let ctx = ctx.clone();
        parse_blocking(move || {
            with_user_ns(&user_ns, || f(&parser, &source, &body, &base_url, &ctx))
        })
        .await
    }

    async fn fetch_source_url(
        &self,
        user_ns: &str,
        source: &BookSource,
        url_rule: &str,
        base_url: &str,
    ) -> Result<FetchResponse, AppError> {
        let mut spec = self
            .analyze_url_blocking(user_ns, url_rule, "", 1, base_url, source)
            .await?;
        self.apply_source_cookie(user_ns, source, &mut spec.headers)
            .await;
        let res = self.fetch_with_rate(user_ns, source, spec).await?;
        self.login_check_blocking(user_ns, source, res).await
    }

    async fn fetch_with_rate(
        &self,
        user_ns: &str,
        source: &BookSource,
        spec: RequestSpec,
    ) -> anyhow::Result<FetchResponse> {
        self.wait_for_rate(source).await;
        // fetch panic 或任务被取消时靠 Drop 复位 in_flight，
        // 否则该标志永远卡住，书源后续请求会全部自旋挂死
        let mut guard = SerialRateGuard {
            states: self.rate_states.clone(),
            key: source.book_source_url.clone(),
            armed: true,
        };
        let result = fetch(&self.http, user_ns, spec).await;
        guard.disarm();
        self.finish_rate(source).await;
        result
    }

    async fn wait_for_rate(&self, source: &BookSource) {
        let Some(rate) = source.concurrent_rate.as_deref().map(str::trim) else {
            return;
        };
        if rate.is_empty() || rate == "0" {
            return;
        }
        if let Some((limit, window_ms)) = parse_window_rate(rate) {
            self.wait_for_window_rate(&source.book_source_url, limit, window_ms)
                .await;
            return;
        }
        let Ok(delay_ms) = rate.parse::<u64>() else {
            return;
        };
        self.wait_for_serial_rate(&source.book_source_url, delay_ms)
            .await;
    }

    async fn wait_for_serial_rate(&self, source_key: &str, delay_ms: u64) {
        let delay = Duration::from_millis(delay_ms);
        loop {
            let wait = {
                let mut states = self.rate_states.write().await;
                let state = states.entry(source_key.to_string()).or_default();
                let now = Instant::now();
                if state.in_flight {
                    delay
                } else if let Some(last_start) = state.last_start {
                    let elapsed = now.saturating_duration_since(last_start);
                    if elapsed < delay {
                        delay - elapsed
                    } else {
                        state.in_flight = true;
                        state.last_start = Some(now);
                        return;
                    }
                } else {
                    state.in_flight = true;
                    state.last_start = Some(now);
                    return;
                }
            };
            sleep(wait).await;
        }
    }

    async fn wait_for_window_rate(&self, source_key: &str, limit: usize, window_ms: u64) {
        if limit == 0 || window_ms == 0 {
            return;
        }
        let window = Duration::from_millis(window_ms);
        loop {
            let wait = {
                let mut states = self.rate_states.write().await;
                let state = states.entry(source_key.to_string()).or_default();
                let now = Instant::now();
                state
                    .window_starts
                    .retain(|start| now.saturating_duration_since(*start) <= window);
                if state.window_starts.len() >= limit {
                    state
                        .window_starts
                        .first()
                        .map(|start| window.saturating_sub(now.saturating_duration_since(*start)))
                        .unwrap_or(window)
                } else {
                    state.window_starts.push(now);
                    return;
                }
            };
            sleep(wait).await;
        }
    }

    async fn finish_rate(&self, source: &BookSource) {
        let mut states = self.rate_states.write().await;
        if let Some(state) = states.get_mut(&source.book_source_url) {
            state.in_flight = false;
        }
    }

    pub async fn search_book(
        &self,
        user_ns: &str,
        source: &BookSource,
        key: &str,
        page: i32,
    ) -> Result<Vec<SearchBook>, AppError> {
        let search_url = source
            .search_url
            .clone()
            .ok_or_else(|| AppError::BadRequest("missing search_url".to_string()))?;
        tracing::info!(
            "searching book from {}: key={}, page={}, url={}",
            source.book_source_name,
            key,
            page,
            search_url
        );
        let mut spec = self
            .analyze_url_blocking(
                user_ns,
                &search_url,
                key,
                page,
                &source.book_source_url,
                source,
            )
            .await
            .map_err(|e| {
                tracing::error!("analyze_url failed: {:?}", e);
                e
            })?;

        self.apply_source_cookie(user_ns, source, &mut spec.headers)
            .await;

        tracing::debug!("search_book fetched spec: {:?}", spec);
        let res = self
            .fetch_with_rate(user_ns, source, spec)
            .await
            .map_err(|e| {
                tracing::error!("fetch failed: {:?}", e);
                e
            })?;
        let res = self.login_check_blocking(user_ns, source, res).await?;
        tracing::debug!("fetch success, body length: {}", res.body.len());
        let mut books = self
            .parse_response_blocking(user_ns, source, &res, |p, s, b, u| p.search_books(s, b, u))
            .await?;
        // 搜索结果产出即登记：响应必须带 bookId，后续详情/目录/正文请求只认它
        self.register_search_books(user_ns, source, &mut books)
            .await?;
        tracing::info!("found {} books", books.len());
        Ok(books)
    }

    pub async fn explore_book(
        &self,
        user_ns: &str,
        source: &BookSource,
        rule_find_url: &str,
        page: i32,
    ) -> Result<Vec<SearchBook>, AppError> {
        if rule_find_url.trim().is_empty() {
            return Err(AppError::BadRequest("ruleFindUrl required".to_string()));
        }
        let mut spec = self
            .analyze_url_blocking(
                user_ns,
                rule_find_url,
                "",
                page,
                &source.book_source_url,
                source,
            )
            .await?;

        self.apply_source_cookie(user_ns, source, &mut spec.headers)
            .await;

        let res = self.fetch_with_rate(user_ns, source, spec).await?;
        let res = self.login_check_blocking(user_ns, source, res).await?;
        let mut books = self
            .parse_response_blocking(user_ns, source, &res, |p, s, b, u| p.explore_books(s, b, u))
            .await?;
        self.register_search_books(user_ns, source, &mut books)
            .await?;
        Ok(books)
    }

    /// 解析书源的发现分类（exploreUrl）。
    ///
    /// exploreUrl 可含 `@js:` 脚本：必须带用户命名空间进阻塞线程池执行——否则
    /// JS 里的 cache/kv/Cookie 落进共享的 "public" 桶（跨用户串号），且同步
    /// 求值会直接堵在 tokio worker 上。
    pub async fn explore_kinds(
        &self,
        user_ns: &str,
        source: &BookSource,
    ) -> Result<Vec<ExploreKind>, AppError> {
        let user_ns = user_ns.to_string();
        let source = source.clone();
        parse_blocking(move || with_user_ns(&user_ns, || parse_explore_kinds(&source, &user_ns)))
            .await?
    }

    pub async fn test_book_source_availability(
        &self,
        user_ns: &str,
        source: &BookSource,
        keyword: Option<&str>,
    ) -> BookSourceAvailability {
        let keyword = keyword
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .or_else(|| {
                source
                    .rule_search
                    .as_ref()
                    .and_then(|rule| rule.check_key_word.as_deref())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
            })
            .unwrap_or("斗破苍穹")
            .to_string();

        let (search_ok, search_error) = if source
            .search_url
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
            && source.rule_search.is_some()
        {
            match self.search_book(user_ns, source, &keyword, 1).await {
                Ok(books) => (!books.is_empty(), None),
                Err(err) => (false, Some(format!("{err:?}"))),
            }
        } else {
            (false, Some("missing searchUrl or ruleSearch".to_string()))
        };

        let explore_url = self
            .explore_kinds(user_ns, source)
            .await
            .ok()
            .and_then(|kinds| {
                kinds
                    .into_iter()
                    .filter_map(|kind| kind.url)
                    .map(|url| url.trim().to_string())
                    .find(|url| !url.is_empty())
            });
        let (explore_ok, explore_error) = if let Some(url) = explore_url.as_deref() {
            match self.explore_book(user_ns, source, url, 1).await {
                Ok(books) => (!books.is_empty(), None),
                Err(err) => (false, Some(format!("{err:?}"))),
            }
        } else {
            (false, Some("missing explore category url".to_string()))
        };

        BookSourceAvailability {
            book_source_url: source.book_source_url.clone(),
            book_source_name: source.book_source_name.clone(),
            valid: search_ok || explore_ok,
            search_ok,
            explore_ok,
            keyword,
            explore_url,
            search_error,
            explore_error,
        }
    }

    pub async fn login_book_source(
        &self,
        user_ns: &str,
        source: &BookSource,
    ) -> Result<serde_json::Value, AppError> {
        let login_url = source
            .login_url
            .clone()
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| AppError::BadRequest("missing loginUrl".to_string()))?;

        let spec = self
            .analyze_url_blocking(user_ns, &login_url, "", 1, &source.book_source_url, source)
            .await?;

        let res = self.fetch_with_rate(user_ns, source, spec).await?;
        let check_result = if let Some(login_check_js) = source
            .login_check_js
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            let user_ns_owned = user_ns.to_string();
            let js = login_check_js.to_string();
            let source_owned = source.clone();
            let body = res.body.clone();
            let url = res.url.clone();
            Some(
                parse_blocking(move || {
                    with_user_ns(&user_ns_owned, || {
                        with_book_source(&source_owned, || eval_js(&js, &body, &url))
                    })
                    .unwrap_or_default()
                })
                .await?,
            )
        } else {
            None
        };

        Ok(serde_json::json!({
            "success": true,
            "status": res.status,
            "url": res.url,
            "checkResult": check_result,
            "bodyPreview": res.body.chars().take(500).collect::<String>(),
            "bodyHtml": res.body
        }))
    }

    /// 书籍详情。缓存按 bookId 键控；解析结果顺带回填登记表的
    /// 书名/作者/目录地址（登记时可能只知道 bookUrl）。
    pub async fn get_book_info(
        &self,
        user_ns: &str,
        book_id: &str,
        source: &BookSource,
        book_url: &str,
        force_refresh: bool,
    ) -> Result<Book, AppError> {
        if !force_refresh {
            if let Some(mut cached) = self.load_book_info_cache(user_ns, book_id).await {
                cached.book_id = Some(book_id.to_string());
                cached.book_source_id = source.book_source_id;
                return Ok(cached);
            }
        }
        let res = self
            .fetch_source_url(user_ns, source, book_url, &source.book_source_url)
            .await?;
        let book_url_owned = book_url.to_string();
        let mut book = self
            .parse_response_blocking(user_ns, source, &res, move |p, s, b, u| {
                p.book_info(s, b, u, &book_url_owned)
            })
            .await?;
        book.book_id = Some(book_id.to_string());
        book.book_source_id = source.book_source_id;
        let _ = self
            .register_book(
                user_ns,
                RegisterBook {
                    book_url,
                    book_source_id: source.book_source_id,
                    name: Some(&book.name),
                    author: Some(&book.author),
                    toc_url: book.toc_url.as_deref(),
                },
            )
            .await;
        let _ = self.save_book_info_cache(user_ns, book_id, &book).await;
        Ok(book)
    }

    fn book_info_cache_path(&self, user_ns: &str, book_id: &str) -> PathBuf {
        self.storage_dir
            .join("cache")
            .join(BOOK_INFO_CACHE_DIR)
            .join(user_ns)
            .join(format!("{}.json", book_id))
    }

    async fn load_book_info_cache(&self, user_ns: &str, book_id: &str) -> Option<Book> {
        let path = self.book_info_cache_path(user_ns, book_id);
        let Ok(meta) = fs::metadata(&path).await else {
            return None;
        };
        let expired = meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .map(|age| age > BOOK_INFO_CACHE_TTL)
            .unwrap_or(true);
        if expired {
            let _ = fs::remove_file(&path).await;
            return None;
        }
        let raw = fs::read_to_string(&path).await.ok()?;
        serde_json::from_str(&raw).ok()
    }

    async fn save_book_info_cache(&self, user_ns: &str, book_id: &str, book: &Book) {
        let path = self.book_info_cache_path(user_ns, book_id);
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent).await;
        }
        if let Ok(data) = serde_json::to_string(book) {
            let _ = fs::write(&path, data).await;
        }
    }

    pub async fn get_chapter_list(
        &self,
        user_ns: &str,
        book_id: &str,
        source: &BookSource,
        toc_url: &str,
    ) -> Result<Vec<BookChapter>, AppError> {
        self.get_chapter_list_with_cache(user_ns, book_id, source, toc_url, false)
            .await
    }

    pub async fn get_chapter_list_with_cache(
        &self,
        user_ns: &str,
        book_id: &str,
        source: &BookSource,
        toc_url: &str,
        force_refresh: bool,
    ) -> Result<Vec<BookChapter>, AppError> {
        // Check cache first (unless force refresh)
        if !force_refresh {
            if let Ok(Some(cached)) = self.load_fresh_chapter_list_cache(user_ns, book_id).await {
                if !cached.is_empty() {
                    return Ok(cached);
                }
            }
        }
        let (chapters, _) = self
            .get_chapter_list_with_pagination(user_ns, source, toc_url)
            .await?;
        // Save to cache
        let _ = self
            .save_chapter_list_cache(user_ns, book_id, &chapters)
            .await;
        Ok(chapters)
    }

    /// Get first page of chapters and pagination info for background fetching
    pub async fn get_chapter_list_first_page(
        &self,
        user_ns: &str,
        source: &BookSource,
        toc_url: &str,
    ) -> Result<(Vec<BookChapter>, ChapterPagination), AppError> {
        let res = self
            .fetch_source_url(user_ns, source, toc_url, &source.book_source_url)
            .await?;
        let (chapters, next_urls) = self
            .parse_response_blocking(user_ns, source, &res, |p, s, b, u| p.chapter_list(s, b, u))
            .await?;

        let mut chapter_index = 0i32;
        let mut result = Vec::new();
        for mut ch in chapters {
            ch.index = chapter_index;
            chapter_index += 1;
            result.push(ch);
        }

        // The actual URL we fetched (after redirects) should be considered visited
        let actual_visited_url = res.url.clone();

        // Get chapter URLs from first page for deduplication
        let first_page_chapter_urls: std::collections::HashSet<String> =
            result.iter().map(|c| c.url.clone()).collect();

        // Filter out already visited URLs and the current page URL from pending_urls
        // Also filter out URLs that point to the same page (same path but different domain)
        let pending_urls: Vec<String> = next_urls
            .into_iter()
            .filter(|u| {
                // Filter out exact matches
                if u == &actual_visited_url || u == toc_url {
                    return false;
                }
                // Filter out URLs with the same path but different domain
                // This handles cases like m.22biqu.com vs m.22biqu.net
                if let (Ok(parsed_u), Ok(parsed_visited)) =
                    (url::Url::parse(u), url::Url::parse(&actual_visited_url))
                {
                    if parsed_u.path() == parsed_visited.path() {
                        return false;
                    }
                }
                true
            })
            .collect();

        let pagination = ChapterPagination {
            user_ns: user_ns.to_string(),
            source: source.clone(),
            toc_url: toc_url.to_string(),
            visited_urls: vec![toc_url.to_string(), actual_visited_url],
            pending_urls,
            seen_chapter_urls: first_page_chapter_urls.iter().cloned().collect(),
            next_index: chapter_index,
        };

        Ok((result, pagination))
    }

    /// 目录分页抓取的公共骨架：多分支（下拉选单多 URL）抓全、单链顺序跟进，
    /// 整页重复熔断、章节 URL 去重都在此收口。
    /// fetch_remaining_chapters 与 get_chapter_list_with_pagination 此前各持有
    /// 一份逐行复制的 130 行实现，修 bug 必然 drift。
    async fn fetch_paginated_chapters(
        &self,
        user_ns: &str,
        source: &BookSource,
        pending_urls: Vec<String>,
        visited_page_urls: &mut std::collections::HashSet<String>,
        seen_chapter_urls: &mut std::collections::HashSet<String>,
        chapter_index: &mut i32,
    ) -> Result<Vec<BookChapter>, AppError> {
        let mut all_chapters = Vec::new();
        let pending_urls: Vec<String> = pending_urls
            .into_iter()
            .filter(|u| !visited_page_urls.contains(u))
            .collect();

        /// 页内公共动作：整页重复熔断 + 逐章去重追加。
        macro_rules! absorb_page {
            ($chapters:expr, $page_url:expr, $on_dup:expr) => {{
                let all_seen = $chapters
                    .iter()
                    .all(|ch| seen_chapter_urls.contains(&ch.url));
                if all_seen && !$chapters.is_empty() {
                    tracing::debug!("Skipping duplicate page: {}", $page_url);
                    $on_dup
                }
                for ch in $chapters {
                    if seen_chapter_urls.contains(&ch.url) {
                        continue;
                    }
                    seen_chapter_urls.insert(ch.url.clone());
                    all_chapters.push(BookChapter {
                        index: *chapter_index,
                        ..ch
                    });
                    *chapter_index += 1;
                }
            }};
        }

        if pending_urls.len() > 1 {
            // Multiple URLs from option dropdown - fetch all pages
            for url in pending_urls {
                if visited_page_urls.contains(&url) {
                    continue;
                }
                visited_page_urls.insert(url.clone());

                let res = self
                    .fetch_source_url(user_ns, source, &url, &source.book_source_url)
                    .await?;
                let (chapters, _) = self
                    .parse_response_blocking(user_ns, source, &res, |p, s, b, u| {
                        p.chapter_list(s, b, u)
                    })
                    .await?;

                absorb_page!(chapters, url, continue);
            }
        } else if pending_urls.len() == 1 {
            // Single next page link - follow sequentially
            let mut current_url = pending_urls[0].clone();
            loop {
                if visited_page_urls.contains(&current_url) {
                    break;
                }
                visited_page_urls.insert(current_url.clone());

                let res = self
                    .fetch_source_url(user_ns, source, &current_url, &source.book_source_url)
                    .await?;
                let (chapters, next_urls) = self
                    .parse_response_blocking(user_ns, source, &res, |p, s, b, u| {
                        p.chapter_list(s, b, u)
                    })
                    .await?;

                // 整页重复即停
                absorb_page!(chapters, current_url, break);

                // Get next page
                let next = next_urls
                    .into_iter()
                    .find(|u| !visited_page_urls.contains(u));
                match next {
                    Some(url) if !url.is_empty() => current_url = url,
                    _ => break,
                }
            }
        }

        Ok(all_chapters)
    }

    /// Continue fetching remaining chapters from pagination state
    pub async fn fetch_remaining_chapters(
        &self,
        pagination: ChapterPagination,
    ) -> Result<Vec<BookChapter>, AppError> {
        let mut visited_page_urls: std::collections::HashSet<String> =
            pagination.visited_urls.iter().cloned().collect();
        let mut seen_chapter_urls: std::collections::HashSet<String> =
            pagination.seen_chapter_urls.iter().cloned().collect();
        let mut chapter_index = pagination.next_index;
        self.fetch_paginated_chapters(
            &pagination.user_ns,
            &pagination.source,
            pagination.pending_urls,
            &mut visited_page_urls,
            &mut seen_chapter_urls,
            &mut chapter_index,
        )
        .await
    }

    async fn get_chapter_list_with_pagination(
        &self,
        user_ns: &str,
        source: &BookSource,
        toc_url: &str,
    ) -> Result<(Vec<BookChapter>, Vec<String>), AppError> {
        // Fetch first page
        let res = self
            .fetch_source_url(user_ns, source, toc_url, &source.book_source_url)
            .await?;
        let (chapters, next_urls) = self
            .parse_response_blocking(user_ns, source, &res, |p, s, b, u| p.chapter_list(s, b, u))
            .await?;

        let mut visited_page_urls = std::collections::HashSet::new();
        visited_page_urls.insert(toc_url.to_string());
        let mut seen_chapter_urls = std::collections::HashSet::new();
        let mut chapter_index = 0i32;
        let mut all_chapters = Vec::new();

        // Add first page chapters with deduplication
        for ch in chapters {
            if seen_chapter_urls.contains(&ch.url) {
                continue;
            }
            seen_chapter_urls.insert(ch.url.clone());
            all_chapters.push(BookChapter {
                index: chapter_index,
                ..ch
            });
            chapter_index += 1;
        }

        let pending_urls: Vec<String> = next_urls
            .into_iter()
            .filter(|u| !visited_page_urls.contains(u))
            .collect();
        let mut rest = self
            .fetch_paginated_chapters(
                user_ns,
                source,
                pending_urls,
                &mut visited_page_urls,
                &mut seen_chapter_urls,
                &mut chapter_index,
            )
            .await?;
        all_chapters.append(&mut rest);

        Ok((all_chapters, visited_page_urls.into_iter().collect()))
    }

    pub async fn get_content(
        &self,
        user_ns: &str,
        book_id: &str,
        source: &BookSource,
        chapter_url: &str,
    ) -> Result<String, AppError> {
        tracing::debug!(
            "get_content called, chapter_url={}, book_key={}",
            chapter_url,
            book_id
        );
        if let Ok(Some(cached)) = self.cache.get(user_ns, book_id, chapter_url).await {
            tracing::debug!("get_content returning cached content, len={}", cached.len());
            return Ok(cached);
        }
        tracing::debug!("get_content cache miss, fetching from network");

        let mut all_content = String::new();
        let mut visited_urls = std::collections::HashSet::new();
        let mut current_url = chapter_url.to_string();

        // Follow pagination to get all content pages
        // 页数上限：环检测只能挡住重复 URL，恶意/病态书源持续产新 URL 时
        // 单次取正文会发出无限请求。正常分页章节（2-5 页）远够不着上限。
        const MAX_CONTENT_PAGES: usize = 50;
        loop {
            if visited_urls.len() >= MAX_CONTENT_PAGES {
                tracing::warn!(
                    "get_content hit MAX_CONTENT_PAGES ({MAX_CONTENT_PAGES}), truncating: book={book_id}"
                );
                break;
            }
            if visited_urls.contains(&current_url) {
                tracing::debug!("get_content detected loop, breaking");
                break;
            }
            visited_urls.insert(current_url.clone());

            tracing::debug!("get_content fetching: {}", current_url);
            let res = self
                .fetch_source_url(user_ns, source, &current_url, &source.book_source_url)
                .await?;
            tracing::debug!("get_content fetch done, body len={}", res.body.len());
            // 评论地址要先拿到正文响应才能求值（见 `chapter_source_body`），
            // 这里顺手把首屏响应体留在内存里，省掉打开评论时的第二次抓取。
            if all_content.is_empty() {
                self.remember_body(user_ns, chapter_url, &res.body, &res.url)
                    .await;
            }
            let (content, next_url) = self
                .parse_response_blocking(user_ns, source, &res, |p, s, b, u| {
                    // content 与 next_content_url 针对同一份响应体，
                    // 一次解析成 DOM 同时求值，省掉第二次 html5ever 全量建树
                    let content = p.content(s, b, u);
                    let next_url = p.next_content_url(s, b, u);
                    (content, next_url)
                })
                .await?;
            tracing::debug!("get_content parsed content len={}", content.len());

            if !content.is_empty() {
                if !all_content.is_empty() {
                    all_content.push('\n');
                }
                all_content.push_str(&content);
            }

            if let Some(next_url) = next_url {
                tracing::debug!("get_content found next_url: {}", next_url);
                if should_follow_content_page(chapter_url, &current_url, &next_url) {
                    current_url = next_url;
                } else {
                    tracing::debug!("get_content next_url appears to be next chapter, stopping");
                    break;
                }
            } else {
                tracing::debug!("get_content no more pages");
                break;
            }
        }

        tracing::debug!("get_content final content len={}", all_content.len());
        if all_content.is_empty() {
            // 空正文分两种：书源声明了配图规则的纯图章节（正文本来就可能为空，
            // 配图走 ruleContentImage），与「规则失效/上游异常」的真空。
            // 后者此前照常返回成功，用户对着空白章节看不出原因。
            if source.rule_content_image.is_none() {
                return Err(AppError::NotFound(
                    "正文为空：书源规则可能已失效，或上游页面结构有变化".to_string(),
                ));
            }
        } else {
            let _ = self
                .cache
                .put(user_ns, book_id, chapter_url, &all_content)
                .await;
        }
        Ok(all_content)
    }

    /// Delete all chapter content cache for a book
    pub async fn delete_book_cache(&self, user_ns: &str, book_id: &str) -> Result<bool, AppError> {
        self.cache
            .remove_book(user_ns, book_id)
            .await
            .map_err(AppError::Internal)
    }

    /// Delete a single chapter's content cache (used by refresh-this-chapter).
    pub async fn delete_chapter_cache(
        &self,
        user_ns: &str,
        book_id: &str,
        chapter_url: &str,
    ) -> Result<(), AppError> {
        self.cache
            .remove(user_ns, book_id, chapter_url)
            .await
            .map_err(AppError::Internal)
    }

    // ── 评论（章评 / 段评） ─────────────────────────────────────────────

    /// 取章节正文响应体，用于求值评论 URL。
    ///
    /// 评论地址藏在正文响应里（见 `RuleEngine::review_url`），所以拉评论前
    /// 必须先有正文响应。正文刚被抓过时直接命中内存暂存，不重复抓取。
    async fn chapter_source_body(
        &self,
        user_ns: &str,
        source: &BookSource,
        chapter_url: &str,
    ) -> Result<(String, String), AppError> {
        if let Some((body, url)) = self.take_recent_body(user_ns, chapter_url).await {
            return Ok((body, url));
        }
        let res = self
            .fetch_source_url(user_ns, source, chapter_url, &source.book_source_url)
            .await?;
        self.remember_body(user_ns, chapter_url, &res.body, &res.url)
            .await;
        Ok((res.body, res.url))
    }

    /// 把章节正文响应体暂存在内存里（有数量与时间上限，丢了就重抓）。
    async fn remember_body(&self, user_ns: &str, chapter_url: &str, body: &str, url: &str) {
        // 大页面不进内存：评论规则本来就用在小接口上，没必要为整页 HTML 占几十 MB
        if body.is_empty() || body.len() > RECENT_BODY_MAX_BYTES {
            return;
        }
        let key = format!("{user_ns}|{chapter_url}");
        let mut bodies = self.recent_bodies.write().await;
        if !bodies.contains_key(&key) && bodies.len() >= RECENT_BODY_LIMIT {
            let now = Instant::now();
            bodies.retain(|_, entry| now.duration_since(entry.stored_at) < RECENT_BODY_TTL);
            // 仍满则逐出最旧一条：整表清空会把刚记住的相邻章节响应一并丢掉，
            // 打开评论时被迫重新抓取
            while bodies.len() >= RECENT_BODY_LIMIT {
                let Some(oldest_key) = bodies
                    .iter()
                    .min_by_key(|(_, entry)| entry.stored_at)
                    .map(|(k, _)| k.clone())
                else {
                    break;
                };
                bodies.remove(&oldest_key);
            }
        }
        bodies.insert(
            key,
            RecentBody {
                body: body.to_string(),
                url: url.to_string(),
                stored_at: Instant::now(),
            },
        );
    }

    async fn take_recent_body(&self, user_ns: &str, chapter_url: &str) -> Option<(String, String)> {
        let key = format!("{user_ns}|{chapter_url}");
        let bodies = self.recent_bodies.read().await;
        let entry = bodies.get(&key)?;
        if entry.stored_at.elapsed() >= RECENT_BODY_TTL {
            return None;
        }
        Some((entry.body.clone(), entry.url.clone()))
    }

    /// 评论 URL 模板可用的占位符。
    fn review_ctx(
        &self,
        book_url: &str,
        chapter_url: &str,
        page: i32,
        count: i32,
        sort: ReviewSort,
        para_index: Option<i32>,
    ) -> HashMap<String, String> {
        let mut ctx = HashMap::new();
        ctx.insert("page".to_string(), page.max(1).to_string());
        ctx.insert("count".to_string(), count.to_string());
        ctx.insert("bookUrl".to_string(), book_url.to_string());
        ctx.insert("chapterUrl".to_string(), chapter_url.to_string());
        // 站点取值各不相同（番茄的段评排序是 `Hot`/`TimeDesc` 枚举），
        // 规则里可以用 JS 映射：`&sort={{sort === 'hot' ? 'Hot' : 'TimeDesc'}}`
        ctx.insert("sort".to_string(), sort.as_ctx_value().to_string());
        if let Some(index) = para_index {
            ctx.insert("paraIndex".to_string(), index.to_string());
        }
        ctx
    }

    /// 章评：某一章的评论列表。
    #[allow(clippy::too_many_arguments)]
    pub async fn get_chapter_reviews(
        &self,
        user_ns: &str,
        source: &BookSource,
        book_id: &str,
        book_url: &str,
        chapter_url: &str,
        page: i32,
        count: i32,
        sort: ReviewSort,
        refresh: bool,
    ) -> Result<ReviewResponse<ReviewPage>, AppError> {
        if !self.parser.has_chapter_review_rule(source) {
            return Ok(ReviewResponse::new(false, ReviewPage::empty(page)));
        }
        let server_sort = rule_consumes_sort(
            source
                .rule_review
                .as_ref()
                .and_then(|rule| rule.review_url.as_deref()),
        );
        let cache_key = format!(
            "chapter|{chapter_url}|{page}|{count}|{}",
            sort.as_ctx_value()
        );
        let author_marks = self.parser.has_chapter_author_rules(source);
        if !refresh {
            if let Some(cached) = self.load_review_cache(user_ns, book_id, &cache_key).await {
                return Ok(ReviewResponse::new(true, cached)
                    .with_server_sort(server_sort)
                    .with_author_marks(author_marks));
            }
        }
        let (body, base) = self
            .chapter_source_body(user_ns, source, chapter_url)
            .await?;
        let ctx = self.review_ctx(book_url, chapter_url, page, count, sort, None);
        let url = self
            .parse_body_blocking(user_ns, source, &body, &base, &ctx, |p, s, b, u, ctx| {
                p.chapter_review_url(s, b, u, ctx)
            })
            .await?
            .ok_or_else(|| AppError::BadRequest("书源的章评地址解析失败".to_string()))?;
        let result = self
            .fetch_review_page(user_ns, source, &url, page, |p, s, b, u| {
                p.chapter_reviews(s, b, u)
            })
            .await?;
        // 空页不进缓存：上游偶发失败（限流、超时）也长这样，缓存 7 天等于
        // 把一次抖动放大成一整周「没有评论」
        if !result.items.is_empty() {
            self.store_review_cache(user_ns, book_id, &cache_key, &result)
                .await;
        }
        Ok(ReviewResponse::new(true, result)
            .with_server_sort(server_sort)
            .with_author_marks(author_marks))
    }

    /// 段评概览：本章哪些段落有段评、各有多少条。
    pub async fn get_para_review_index(
        &self,
        user_ns: &str,
        source: &BookSource,
        book_id: &str,
        book_url: &str,
        chapter_url: &str,
        refresh: bool,
    ) -> Result<ReviewResponse<ParaReviewIndex>, AppError> {
        if !self.parser.has_para_review_rule(source) {
            return Ok(ReviewResponse::new(false, ParaReviewIndex::default()));
        }
        let cache_key = format!("para-index|{chapter_url}");
        if !refresh {
            if let Some(cached) = self.load_review_cache(user_ns, book_id, &cache_key).await {
                return Ok(ReviewResponse::new(true, cached));
            }
        }
        let (body, base) = self
            .chapter_source_body(user_ns, source, chapter_url)
            .await?;
        let ctx = self.review_ctx(
            book_url,
            chapter_url,
            1,
            REVIEW_PAGE_SIZE,
            ReviewSort::Hot,
            None,
        );
        let url = self
            .parse_body_blocking(user_ns, source, &body, &base, &ctx, |p, s, b, u, ctx| {
                p.para_review_index_url(s, b, u, ctx)
            })
            .await?
            .ok_or_else(|| AppError::BadRequest("书源的段评概览地址解析失败".to_string()))?;
        let res = self
            .fetch_source_url(user_ns, source, &url, &source.book_source_url)
            .await?;
        let mut index = self
            .parse_response_blocking(user_ns, source, &res, |p, s, b, u| ParaReviewIndex {
                paras: p.para_review_index(s, b, u),
            })
            .await?;
        // 段号会因用户的书源替换规则、繁简转换而漂移，补上段落原文做兜底定位。
        self.fill_para_texts(user_ns, source, &body, &base, &mut index.paras)
            .await;
        if !index.paras.is_empty() {
            self.store_review_cache(user_ns, book_id, &cache_key, &index)
                .await;
        }
        Ok(ReviewResponse::new(true, index))
    }

    /// 章节配图：给「配图另走一个接口」的站点用。
    ///
    /// 刻意不做缓存：图片地址普遍带时效签名（番茄的 `x-expires`），存下来过一阵
    /// 就是一堆打不开的死链；配图接口本身很小，每次开章取一次更省心。
    /// 章节正文响应走 [`Self::chapter_source_body`]，紧接着正文请求时命中内存暂存，
    /// 不额外多抓一次正文。
    pub async fn get_chapter_images(
        &self,
        user_ns: &str,
        source: &BookSource,
        chapter_url: &str,
    ) -> Result<ChapterImages, AppError> {
        if !self.parser.has_content_image_rule(source) {
            return Ok(ChapterImages::disabled());
        }
        let (body, base) = self
            .chapter_source_body(user_ns, source, chapter_url)
            .await?;
        let url = self
            .parse_body_blocking(
                user_ns,
                source,
                &body,
                &base,
                &HashMap::new(),
                |p, s, b, u, _ctx| p.content_image_url(s, b, u),
            )
            .await?;
        // 规则求值不出地址（比如书源里写死了别的取法）不算错误：这一章就是没有配图
        let Some(url) = url else {
            return Ok(ChapterImages::new(Vec::new()));
        };
        let res = self
            .fetch_source_url(user_ns, source, &url, &source.book_source_url)
            .await?;
        let images = self
            .parse_response_blocking(user_ns, source, &res, |p, s, b, u| {
                p.content_images(s, b, u)
            })
            .await?;
        Ok(ChapterImages::new(images))
    }

    /// 段评：某一段的评论列表。
    #[allow(clippy::too_many_arguments)]
    pub async fn get_para_reviews(
        &self,
        user_ns: &str,
        source: &BookSource,
        book_id: &str,
        book_url: &str,
        chapter_url: &str,
        para_index: i32,
        page: i32,
        count: i32,
        sort: ReviewSort,
        refresh: bool,
    ) -> Result<ReviewResponse<ReviewPage>, AppError> {
        if !self.parser.has_para_review_rule(source) {
            return Ok(ReviewResponse::new(false, ReviewPage::empty(page)));
        }
        let server_sort = rule_consumes_sort(
            source
                .rule_para_review
                .as_ref()
                .and_then(|rule| rule.review_url.as_deref()),
        );
        let author_marks = self.parser.has_para_author_rules(source);
        let cache_key = format!(
            "para|{chapter_url}|{para_index}|{page}|{count}|{}",
            sort.as_ctx_value()
        );
        if !refresh {
            if let Some(cached) = self.load_review_cache(user_ns, book_id, &cache_key).await {
                return Ok(ReviewResponse::new(true, cached)
                    .with_server_sort(server_sort)
                    .with_author_marks(author_marks));
            }
        }
        let (body, base) = self
            .chapter_source_body(user_ns, source, chapter_url)
            .await?;
        let ctx = self.review_ctx(book_url, chapter_url, page, count, sort, Some(para_index));
        let url = self
            .parse_body_blocking(user_ns, source, &body, &base, &ctx, |p, s, b, u, ctx| {
                p.para_review_url(s, b, u, ctx)
            })
            .await?
            .ok_or_else(|| AppError::BadRequest("书源的段评地址解析失败".to_string()))?;
        let result = self
            .fetch_review_page(user_ns, source, &url, page, |p, s, b, u| {
                p.para_reviews(s, b, u)
            })
            .await?;
        // 空页不进缓存，理由同章评
        if !result.items.is_empty() {
            self.store_review_cache(user_ns, book_id, &cache_key, &result)
                .await;
        }
        Ok(ReviewResponse::new(true, result)
            .with_server_sort(server_sort)
            .with_author_marks(author_marks))
    }

    /// 「只看作者」：扫描本章（`para_index` 为 `Some` 时是某一段）的全部评论页，
    /// 挑出作者评论过、赞过或回复过的**整条**评论（回复完整保留）。
    ///
    /// 上游没有作者过滤参数，只能逐页扫，且**热度序与时间序各扫一遍**——实测
    /// 番茄两种排序返回的评论集合并不一致（作者标记只出现在其中一种的深处），
    /// 按 `comment_id` 去重合并。每种序最多 25 页 × 50 条。扫描结果——包括
    /// 「没有作者相关评论」的空结果——进 7 天评论缓存：一次完整扫描最多 50 个
    /// 上游请求，不能接受每次打开面板都重扫；`refresh=1` 仍可强刷。与单页评论
    /// 的「空页不进缓存」相反，这里的空结果是扫完所有页得出的结论，不是上游
    /// 抖动——中途任何一页失败都会整体报错、不写缓存。
    #[allow(clippy::too_many_arguments)]
    pub async fn get_author_reviews(
        &self,
        user_ns: &str,
        source: &BookSource,
        book_id: &str,
        book_url: &str,
        chapter_url: &str,
        para_index: Option<i32>,
        refresh: bool,
    ) -> Result<ReviewResponse<ReviewPage>, AppError> {
        let (capable, author_marks) = match para_index {
            Some(_) => (
                self.parser.has_para_review_rule(source),
                self.parser.has_para_author_rules(source),
            ),
            None => (
                self.parser.has_chapter_review_rule(source),
                self.parser.has_chapter_author_rules(source),
            ),
        };
        // 没配作者标记规则时扫描注定颗粒无收，直接报不支持，前端藏入口
        if !capable || !author_marks {
            return Ok(ReviewResponse::new(false, ReviewPage::empty(1)));
        }
        let cache_key = match para_index {
            Some(index) => format!("para-author|{chapter_url}|{index}"),
            None => format!("chapter-author|{chapter_url}"),
        };
        if !refresh {
            if let Some(cached) = self
                .load_review_cache::<ReviewPage>(user_ns, book_id, &cache_key)
                .await
            {
                return Ok(ReviewResponse::new(true, cached).with_author_marks(true));
            }
        }
        let (body, base) = self
            .chapter_source_body(user_ns, source, chapter_url)
            .await?;
        let mut items: Vec<crate::model::review::ReviewItem> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for sort in [ReviewSort::Hot, ReviewSort::Time] {
            for page in 1..=AUTHOR_SCAN_MAX_PAGES {
                let ctx = self.review_ctx(
                    book_url,
                    chapter_url,
                    page,
                    AUTHOR_SCAN_PAGE_SIZE,
                    sort,
                    para_index,
                );
                let url = self
                    .parse_body_blocking(
                        user_ns,
                        source,
                        &body,
                        &base,
                        &ctx,
                        move |p, s, b, u, ctx| match para_index {
                            Some(_) => p.para_review_url(s, b, u, ctx),
                            None => p.chapter_review_url(s, b, u, ctx),
                        },
                    )
                    .await?
                    .ok_or_else(|| AppError::BadRequest("书源的评论地址解析失败".to_string()))?;
                let result = self
                    .fetch_review_page(user_ns, source, &url, page, move |p, s, b, u| {
                        match para_index {
                            Some(_) => p.para_reviews(s, b, u),
                            None => p.chapter_reviews(s, b, u),
                        }
                    })
                    .await?;
                let has_more = result.has_more;
                items.extend(
                    result
                        .items
                        .into_iter()
                        .filter(is_author_related)
                        .filter(|item| seen.insert(item.id.clone())),
                );
                if !has_more {
                    break;
                }
            }
        }
        let page_result = ReviewPage {
            total: items.len() as i64,
            has_more: false,
            page: 1,
            items,
        };
        self.store_review_cache(user_ns, book_id, &cache_key, &page_result)
            .await;
        Ok(ReviewResponse::new(true, page_result).with_author_marks(true))
    }

    async fn fetch_review_page<F>(
        &self,
        user_ns: &str,
        source: &BookSource,
        url: &str,
        page: i32,
        parse: F,
    ) -> Result<ReviewPage, AppError>
    where
        F: FnOnce(&RuleEngine, &BookSource, &str, &str) -> ReviewPage + Send + 'static,
    {
        let res = self
            .fetch_source_url(user_ns, source, url, &source.book_source_url)
            .await?;
        let mut parsed = self
            .parse_response_blocking(user_ns, source, &res, parse)
            .await?;
        // 排查评论数据问题时需要能看到实际请求的地址与上游响应形态
        if parsed.items.is_empty() {
            tracing::debug!(
                "评论页为空: url={} 响应前 200 字节={:?}",
                url,
                res.body.chars().take(200).collect::<String>()
            );
        }
        parsed.page = page;
        Ok(parsed)
    }

    /// 用正文规则取出正文并按行切分，给段评补上「这一段是什么」。
    ///
    /// `content` 内部可能跑 JS，与其它解析入口一样必须进阻塞线程池并带
    /// `user_ns`，否则 cache/kv 落进共享桶且堵住 tokio worker。
    async fn fill_para_texts(
        &self,
        user_ns: &str,
        source: &BookSource,
        body: &str,
        base_url: &str,
        paras: &mut [ParaReviewCount],
    ) {
        if paras.is_empty() {
            return;
        }
        let text = self
            .parse_body_blocking(
                user_ns,
                source,
                body,
                base_url,
                &HashMap::new(),
                |p, s, b, u, _| p.content(s, b, u),
            )
            .await
            .unwrap_or_default();
        if text.is_empty() {
            return;
        }
        let lines: Vec<&str> = text.split('\n').collect();
        for para in paras.iter_mut() {
            let Ok(index) = usize::try_from(para.para_index) else {
                continue;
            };
            if let Some(line) = lines.get(index) {
                para.text = truncate_chars(line.trim(), 60);
            }
        }
    }

    async fn load_review_cache<T: serde::de::DeserializeOwned>(
        &self,
        user_ns: &str,
        book_key: &str,
        key: &str,
    ) -> Option<T> {
        let raw = self
            .review_cache
            .get(user_ns, book_key, key)
            .await
            .ok()
            .flatten()?;
        serde_json::from_str(&raw).ok()
    }

    async fn store_review_cache<T: serde::Serialize>(
        &self,
        user_ns: &str,
        book_key: &str,
        key: &str,
        value: &T,
    ) {
        let Ok(raw) = serde_json::to_string(value) else {
            return;
        };
        let _ = self.review_cache.put(user_ns, book_key, key, &raw).await;
    }

    /// 封面缓存目录：`<storage>/cache/<ns>/cover`。
    ///
    /// 封面是匿名资源，抓取时固定用 `public` 命名空间，因此实际只有
    /// `<storage>/cache/public/cover` 一个目录在用。
    fn cover_cache_dir(&self, user_ns: &str) -> PathBuf {
        self.storage_dir.join("cache").join(user_ns).join("cover")
    }

    /// 目录缓存：可再生数据，归位 cache/ 树下（`<storage>/cache/<ns>/toc`）。
    fn chapter_cache_dir(&self, user_ns: &str) -> PathBuf {
        self.storage_dir.join("cache").join(user_ns).join("toc")
    }

    /// 换源候选缓存：可再生数据，归位 cache/ 树下（`<storage>/cache/<ns>/source_candidates`）。
    fn book_sources_cache_dir(&self, user_ns: &str) -> PathBuf {
        self.storage_dir
            .join("cache")
            .join(user_ns)
            .join("source_candidates")
    }

    /// 失效书源清单：`<storage>/cache/invalid_book_sources/<ns>.json`。
    pub fn invalid_sources_path(&self, user_ns: &str) -> PathBuf {
        self.storage_dir
            .join("cache")
            .join("invalid_book_sources")
            .join(format!("{}.json", user_ns))
    }

    /// 清理某个用户的全部缓存。
    ///
    /// 顺序不可调换：`<cache>/<ns>/cover` 与正文的 `<cache>/<ns>/<book_key>/`
    /// 处于同一层深，必须先删封面、最后删正文目录，各层计数才不会互相污染。
    pub async fn purge_user_cache(&self, user_ns: &str) -> Result<CachePurgeResult, AppError> {
        let mut result = CachePurgeResult {
            cover: remove_dir_counting_files(&self.cover_cache_dir(user_ns))
                .await
                .map_err(internal_error)?
                + remove_dir_counting_files(&crate::service::image_service::image_cache_dir(
                    &self.storage_dir,
                ))
                .await
                .map_err(internal_error)?,
            chapter_list: remove_dir_counting_files(&self.chapter_cache_dir(user_ns))
                .await
                .map_err(internal_error)?,
            search_results: remove_dir_counting_files(&self.book_sources_cache_dir(user_ns))
                .await
                .map_err(internal_error)?,
            invalid_sources: remove_file_counting(&self.invalid_sources_path(user_ns))
                .await
                .map_err(internal_error)?,
            review: self
                .review_cache
                .remove_user(user_ns)
                .await
                .map_err(internal_error)?,
            content: 0,
        };
        // 此刻 `<cache>/<ns>` 下只剩正文的 book_key 目录
        result.content = self
            .cache
            .remove_user(user_ns)
            .await
            .map_err(internal_error)?;
        // 联动回收非在架书籍的登记行（在架书籍的 id 保留，缓存已清但身份不动）。
        // 书架读取失败必须中断：失败被吞成空书架会把在架书籍的登记行也一并删掉。
        let shelf_ids: Vec<String> = self
            .get_bookshelf(user_ns)
            .await?
            .iter()
            .filter_map(|b| b.book_id.clone())
            .collect();
        self.book_repo.delete_except(user_ns, &shelf_ids).await?;
        Ok(result)
    }

    /// 清理全部用户的缓存。
    pub async fn purge_all_cache(&self) -> Result<CachePurgeResult, AppError> {
        let mut total = CachePurgeResult::default();
        for user_ns in self.cache.users().await {
            // `<cache>/reviews` 是评论缓存的独立子树，不是用户命名空间，
            // 它的用户清单由 review_cache 自己维护（见下面的 remove_all）。
            if user_ns == REVIEW_CACHE_DIR {
                continue;
            }
            total.add(self.purge_user_cache(&user_ns).await?);
        }
        total.review = self
            .review_cache
            .remove_all()
            .await
            .map_err(internal_error)?;
        // 收尾：删掉可能残留的空目录（含已清空的 invalid_book_sources）
        let _ = fs::remove_dir_all(self.storage_dir.join("cache")).await;
        Ok(total)
    }

    /// 按缓存类型清理某个用户的一层缓存。
    pub async fn purge_user_cache_kind(
        &self,
        user_ns: &str,
        kind: CacheKind,
    ) -> Result<CachePurgeResult, AppError> {
        let mut result = CachePurgeResult::default();
        match kind {
            CacheKind::Content => {
                result.content = self
                    .cache
                    .remove_user(user_ns)
                    .await
                    .map_err(internal_error)?
            }
            CacheKind::Cover => {
                result.cover = remove_dir_counting_files(&self.cover_cache_dir(user_ns))
                    .await
                    .map_err(internal_error)?
                    + remove_dir_counting_files(&crate::service::image_service::image_cache_dir(
                        &self.storage_dir,
                    ))
                    .await
                    .map_err(internal_error)?
            }
            CacheKind::ChapterList => {
                result.chapter_list = remove_dir_counting_files(&self.chapter_cache_dir(user_ns))
                    .await
                    .map_err(internal_error)?
                    + remove_dir_counting_files(
                        &self
                            .storage_dir
                            .join("cache")
                            .join(BOOK_INFO_CACHE_DIR)
                            .join(user_ns),
                    )
                    .await
                    .map_err(internal_error)?;
            }
            CacheKind::SearchResults => {
                result.search_results =
                    remove_dir_counting_files(&self.book_sources_cache_dir(user_ns))
                        .await
                        .map_err(internal_error)?
            }
            CacheKind::Review => {
                result.review = self
                    .review_cache
                    .remove_user(user_ns)
                    .await
                    .map_err(internal_error)?
            }
        }
        Ok(result)
    }

    /// 换源时清理源相关缓存（正文/目录/详情/评论），但保留换源候选清单
    ///——它就是为下一次换源准备的。
    pub async fn purge_book_reading_cache(
        &self,
        user_ns: &str,
        book_id: &str,
    ) -> Result<(), AppError> {
        let _ = self.delete_book_cache(user_ns, book_id).await;
        let _ = self.delete_chapter_list_cache(user_ns, book_id).await;
        let _ = fs::remove_file(self.book_info_cache_path(user_ns, book_id)).await;
        let _ = self.review_cache.remove_book(user_ns, book_id).await;
        Ok(())
    }

    /// 清理单本书的全部按书缓存（正文/目录/详情/换源候选/评论）。
    pub async fn purge_book_cache(
        &self,
        user_ns: &str,
        book_id: &str,
    ) -> Result<CachePurgeResult, AppError> {
        let mut result = CachePurgeResult::default();
        if self
            .cache
            .remove_book(user_ns, book_id)
            .await
            .map_err(internal_error)?
        {
            result.content = 1;
        }
        if self
            .delete_chapter_list_cache(user_ns, book_id)
            .await
            .is_ok()
        {
            result.chapter_list += 1;
        }
        let _ = fs::remove_file(self.book_info_cache_path(user_ns, book_id)).await;
        if self
            .delete_book_sources_cache(user_ns, book_id)
            .await
            .is_ok()
        {
            result.search_results = 1;
        }
        result.review = self
            .review_cache
            .remove_book(user_ns, book_id)
            .await
            .map_err(internal_error)?;
        Ok(result)
    }

    /// 各层缓存的当前占用。`user_ns` 为 `None` 时汇总全部用户。
    pub async fn cache_stats(&self, user_ns: Option<&str>) -> Result<Value, AppError> {
        use crate::storage::cache::file_cache::dir_usage;
        let content = match user_ns {
            Some(ns) => self.cache.user_usage(ns).await,
            None => self.cache.total_usage().await,
        };
        // 封面现在由图片管道统一管理（`cache/image`，全局共享），
        // 旧封面目录（`cache/<ns>/cover`）可能还有历史文件，一并计入
        let image = dir_usage(
            &crate::service::image_service::image_cache_dir(&self.storage_dir),
            usize::MAX,
        )
        .await;
        let legacy_cover = match user_ns {
            Some(ns) => dir_usage(&self.cover_cache_dir(ns), usize::MAX).await,
            None => dir_usage(&self.cover_cache_dir("public"), usize::MAX).await,
        };
        let cover = CacheUsage {
            files: image.files + legacy_cover.files,
            bytes: image.bytes + legacy_cover.bytes,
        };
        let chapter_list = match user_ns {
            Some(ns) => dir_usage(&self.chapter_cache_dir(ns), usize::MAX).await,
            None => self.cache_subdir_usage("toc").await,
        };
        let search_results = match user_ns {
            Some(ns) => dir_usage(&self.book_sources_cache_dir(ns), usize::MAX).await,
            None => self.cache_subdir_usage("source_candidates").await,
        };
        let review = match user_ns {
            Some(ns) => self.review_cache.user_usage(ns).await,
            None => self.review_cache.total_usage().await,
        };
        Ok(serde_json::json!({
            "content": content,
            "cover": cover,
            "chapterList": chapter_list,
            "searchResults": search_results,
            "review": review,
        }))
    }

    /// 汇总 `cache/*/<name>` 各用户子目录的占用。
    ///（cache/ 顶层还混着 bookinfo/image/reviews 等非用户目录，join 不到
    /// 目标子目录时贡献 0，不影响结果。）
    async fn cache_subdir_usage(&self, name: &str) -> CacheUsage {
        use crate::storage::cache::file_cache::dir_usage;
        let mut total = CacheUsage::default();
        let Ok(mut entries) = fs::read_dir(self.storage_dir.join("cache")).await else {
            return total;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let usage = dir_usage(&entry.path().join(name), usize::MAX).await;
            total.files += usage.files;
            total.bytes += usage.bytes;
        }
        total
    }

    /// Check if a specific chapter is cached
    pub async fn is_chapter_cached(&self, user_ns: &str, book_id: &str, chapter_url: &str) -> bool {
        self.cache.exists(user_ns, book_id, chapter_url).await
    }

    pub async fn chapter_list_cache_exists(&self, user_ns: &str, book_id: &str) -> bool {
        let path = self.chapter_list_cache_path(user_ns, book_id);
        path.exists()
    }

    pub async fn get_bookshelf(&self, user_ns: &str) -> Result<Vec<Book>, AppError> {
        let entries = self.shelf_repo.list(user_ns).await?;
        Ok(entries.into_iter().map(assemble_shelf_book).collect())
    }

    /// 按 bookId 取书架条目。
    pub async fn get_shelf_book(
        &self,
        user_ns: &str,
        book_id: &str,
    ) -> Result<Option<Book>, AppError> {
        Ok(self
            .shelf_repo
            .get(user_ns, book_id)
            .await?
            .map(assemble_shelf_book))
    }

    /// 就地更新一本书（按 bookId）：单行读-改-写。
    pub async fn update_shelf_book(
        &self,
        user_ns: &str,
        book_id: &str,
        update: impl FnOnce(&mut Book),
    ) -> Result<Option<Book>, AppError> {
        let Some(entry) = self.shelf_repo.get(user_ns, book_id).await? else {
            return Ok(None);
        };
        let mut book = assemble_shelf_book(entry);
        update(&mut book);
        self.shelf_repo
            .upsert(user_ns, shelf_row_of(&book), None)
            .await?;
        Ok(Some(book))
    }

    pub async fn save_book(&self, user_ns: &str, mut book: Book) -> Result<Book, AppError> {
        sanitize_book_urls(&mut book);
        if book.book_url.trim().is_empty() {
            return Err(AppError::BadRequest("bookUrl required".to_string()));
        }
        // 登记/校验 bookId：书架条目与登记表共用同一身份；origin 是数据字段不再校验非空
        let book_id = self
            .register_book(
                user_ns,
                RegisterBook {
                    book_url: &book.book_url,
                    book_source_id: book.book_source_id,
                    name: Some(&book.name),
                    author: Some(&book.author),
                    toc_url: book.toc_url.as_deref(),
                },
            )
            .await?;
        book.book_id = Some(book_id.clone());

        // 既有条目：进度/分组等字段缺省时继承（saveBook 语义是 upsert 更新）
        if let Some(existing) = self.shelf_repo.get(user_ns, &book_id).await? {
            let exist = assemble_shelf_book(existing);
            if book.dur_chapter_index.is_none() {
                book.dur_chapter_index = exist.dur_chapter_index;
            }
            if book.dur_chapter_title.is_none() {
                book.dur_chapter_title = exist.dur_chapter_title;
            }
            if book.dur_chapter_time.is_none() {
                book.dur_chapter_time = exist.dur_chapter_time;
            }
            if book.dur_chapter_pos.is_none() {
                book.dur_chapter_pos = exist.dur_chapter_pos;
            }
            if book.total_chapter_num.is_none() {
                book.total_chapter_num = exist.total_chapter_num;
            }
            if book.last_check_time.is_none() {
                book.last_check_time = exist.last_check_time;
            }
            if book.group.is_none() {
                book.group = exist.group;
            }
        }

        // 存在性检查、限额与写入收进同一事务（消除 count 与 upsert 分离的并发超限窗口）
        self.shelf_repo
            .upsert_with_limit(
                user_ns,
                shelf_row_of(&book),
                i64::from(self.user_book_limit),
                i64::from(self.user_local_book_limit),
                is_local_book_url(&book.book_url),
            )
            .await?;
        Ok(book)
    }

    /// 整架替换（含 WebDAV 恢复）。`restore=true` 时先清空登记表与书架再按
    /// 备份自带的 bookId 逐条重建——空表无冲突，恢复后书签等按 id 引用的
    /// 数据依然对齐。全路径单事务：两个存储不会半路不一致。
    pub async fn save_books(
        &self,
        user_ns: &str,
        books: Vec<Book>,
        restore: bool,
    ) -> Result<Vec<Book>, AppError> {
        // 预校验（不落库）：URL 清洗与限额都在事务外完成
        let mut prepared = Vec::with_capacity(books.len());
        for mut book in books {
            sanitize_book_urls(&mut book);
            if book.book_url.trim().is_empty() {
                return Err(AppError::BadRequest("bookUrl required".to_string()));
            }
            prepared.push(book);
        }
        // save_books 是整架替换（导入场景），按替换后的总量校验上限
        let limit = self.user_book_limit as usize;
        if limit > 0 && prepared.len() > limit {
            return Err(AppError::BadRequest(format!(
                "书架书籍数量不能超过 {limit} 本"
            )));
        }
        // 本地书限额在整架替换后同样成立
        let local_limit = self.user_local_book_limit as usize;
        if local_limit > 0 {
            let local_count = prepared
                .iter()
                .filter(|b| is_local_book_url(&b.book_url))
                .count();
            if local_count > local_limit {
                return Err(AppError::BadRequest(format!(
                    "本地书籍数量不能超过 {local_limit} 本"
                )));
            }
        }

        let mut tx = self.book_repo.begin().await?;
        // 登记表与书架一起清重建：恢复语义是全量覆盖
        if restore {
            self.book_repo.wipe_on(&mut tx, user_ns).await?;
            self.shelf_repo.wipe_on(&mut tx, user_ns).await?;
        }
        let mut normalized = Vec::with_capacity(prepared.len());
        for mut book in prepared {
            let item = RegisterBook {
                book_url: &book.book_url,
                book_source_id: book.book_source_id,
                name: Some(&book.name),
                author: Some(&book.author),
                toc_url: book.toc_url.as_deref(),
            };
            let desired = if restore {
                book.book_id.as_deref()
            } else {
                None
            };
            let book_id = self
                .book_repo
                .register_on(&mut tx, user_ns, item, desired)
                .await?;
            book.book_id = Some(book_id);
            normalized.push(book);
        }
        let rows = normalized.iter().map(shelf_row_of).collect();
        self.shelf_repo
            .replace_all_on(&mut tx, user_ns, rows)
            .await?;
        tx.commit().await?;
        Ok(normalized)
    }

    /// 按 bookId 从书架删除；连带清掉该书的全部按书缓存与登记行。
    pub async fn delete_book(&self, user_ns: &str, book_id: &str) -> Result<bool, AppError> {
        let deleted = self.shelf_repo.delete(user_ns, book_id).await?;
        if deleted {
            let _ = self.clear_book_related_cache(user_ns, book_id).await;
            let _ = self.book_repo.delete(user_ns, book_id).await;
        }
        Ok(deleted)
    }

    pub async fn delete_books(
        &self,
        user_ns: &str,
        book_ids: &[String],
    ) -> Result<usize, AppError> {
        let deleted = self.shelf_repo.delete_many(user_ns, book_ids).await?;
        if deleted > 0 {
            for book_id in book_ids {
                let _ = self.clear_book_related_cache(user_ns, book_id).await;
                let _ = self.book_repo.delete(user_ns, book_id).await;
            }
        }
        Ok(deleted as usize)
    }

    /// 某本书已缓存的章节数：数缓存目录里的文件即可。
    ///
    /// 此前实现按章节 URL 列表逐个做同步 `exists()`，书架页对每本书执行，
    /// 千章书在书多的场景下是数万次 stat；一次 `read_dir` 计数就够了。
    pub async fn cached_chapter_count(
        &self,
        user_ns: &str,
        book_id: &str,
    ) -> Result<usize, AppError> {
        Ok(self.cache.book_file_count(user_ns, book_id).await as usize)
    }

    pub async fn cache_chapter(
        &self,
        user_ns: &str,
        book_id: &str,
        source: &BookSource,
        chapter_url: &str,
        refresh: bool,
    ) -> Result<(), AppError> {
        if refresh {
            let _ = self.cache.remove(user_ns, book_id, chapter_url).await;
        }
        let _ = self
            .get_content(user_ns, book_id, source, chapter_url)
            .await?;
        Ok(())
    }

    pub async fn load_book_sources_cache(
        &self,
        user_ns: &str,
        book_id: &str,
    ) -> Result<Option<Vec<SearchBook>>, AppError> {
        let path = self.book_source_cache_path(user_ns, book_id);
        if !path.exists() {
            return Ok(None);
        }
        let data = fs::read_to_string(&path)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
        let list: Vec<SearchBook> =
            serde_json::from_str(&data).map_err(|e| AppError::BadRequest(e.to_string()))?;
        Ok(Some(list))
    }

    pub async fn save_book_sources_cache(
        &self,
        user_ns: &str,
        book_id: &str,
        list: &Vec<SearchBook>,
    ) -> Result<(), AppError> {
        let path = self.book_source_cache_path(user_ns, book_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| AppError::Internal(e.into()))?;
        }
        let data = serde_json::to_string(list).map_err(|e| AppError::BadRequest(e.to_string()))?;
        fs::write(&path, data)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
        Ok(())
    }

    pub async fn delete_book_sources_cache(
        &self,
        user_ns: &str,
        book_id: &str,
    ) -> Result<(), AppError> {
        let path = self.book_source_cache_path(user_ns, book_id);
        if path.exists() {
            fs::remove_file(&path)
                .await
                .map_err(|e| AppError::Internal(e.into()))?;
        }
        Ok(())
    }

    fn book_source_cache_path(&self, user_ns: &str, book_id: &str) -> PathBuf {
        self.storage_dir
            .join("cache")
            .join(user_ns)
            .join("source_candidates")
            .join(format!("{}.json", book_id))
    }

    // Chapter list cache methods
    fn chapter_list_cache_path(&self, user_ns: &str, book_id: &str) -> PathBuf {
        self.storage_dir
            .join("cache")
            .join(user_ns)
            .join("toc")
            .join(format!("{}.json", book_id))
    }

    /// 缓存内全文检索：只扫已缓存章节的正文文件（可再生数据），不触发任何
    /// 上游请求。本地书不走这套缓存，自然不在检索范围。`query` 是纯文本
    /// 子串匹配（大小写不敏感），不做正则语法——用户输错正则不该炸掉搜索。
    pub async fn search_cached_content(
        &self,
        user_ns: &str,
        book_id: &str,
        query: &str,
        max_results: usize,
    ) -> Result<CachedSearchOutcome, AppError> {
        let chapters = self
            .load_chapter_list_cache(user_ns, book_id)
            .await?
            .unwrap_or_default();
        let searchable: Vec<&BookChapter> = chapters
            .iter()
            .filter(|chapter| !chapter.is_volume && !chapter.url.is_empty())
            .collect();
        let re = regex::Regex::new(&format!("(?i){}", regex::escape(query)))
            .map_err(|e| AppError::BadRequest(format!("搜索词不可用: {e}")))?;

        let mut outcome = CachedSearchOutcome {
            results: Vec::new(),
            cached_chapters: 0,
            total_chapters: searchable.len(),
            truncated: false,
        };
        for chapter in searchable {
            let Ok(Some(body)) = self.cache.get(user_ns, book_id, &chapter.url).await else {
                continue;
            };
            outcome.cached_chapters += 1;
            // 缓存里可能是 HTML 正文：剥掉标签再匹配，摘要才不带尖括号
            let text = crate::util::text::strip_html_tags(&body);
            let mut snippets = Vec::new();
            for m in re.find_iter(&text) {
                snippets.push(build_search_snippet(&text, m.start(), m.end()));
                if snippets.len() >= MAX_SNIPPETS_PER_CHAPTER {
                    break;
                }
            }
            if snippets.is_empty() {
                continue;
            }
            outcome.results.push(CachedSearchHit {
                chapter_index: chapter.index,
                chapter_title: chapter.title.clone(),
                chapter_url: chapter.url.clone(),
                snippets,
            });
            if outcome.results.len() >= max_results {
                outcome.truncated = true;
                break;
            }
        }
        Ok(outcome)
    }

    pub async fn load_chapter_list_cache(
        &self,
        user_ns: &str,
        book_id: &str,
    ) -> Result<Option<Vec<BookChapter>>, AppError> {
        let path = self.chapter_list_cache_path(user_ns, book_id);
        if !path.exists() {
            return Ok(None);
        }
        let data = fs::read_to_string(&path)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
        let list: Vec<BookChapter> =
            serde_json::from_str(&data).map_err(|e| AppError::BadRequest(e.to_string()))?;
        Ok(Some(list))
    }

    /// 带 TTL 的目录缓存读取：超过 `CHAPTER_LIST_CACHE_TTL` 的文件按未命中
    /// 处理（不删除，留给其他裸读路径），由调用方重抓后覆盖写回。
    /// 仅限「缓存直接喂响应」的路径使用。
    pub async fn load_fresh_chapter_list_cache(
        &self,
        user_ns: &str,
        book_id: &str,
    ) -> Result<Option<Vec<BookChapter>>, AppError> {
        let path = self.chapter_list_cache_path(user_ns, book_id);
        let expired = match fs::metadata(&path).await {
            Ok(meta) => meta
                .modified()
                .ok()
                .and_then(|m| m.elapsed().ok())
                .map(|age| age > CHAPTER_LIST_CACHE_TTL)
                .unwrap_or(true),
            Err(_) => false,
        };
        if expired {
            return Ok(None);
        }
        self.load_chapter_list_cache(user_ns, book_id).await
    }

    pub async fn save_chapter_list_cache(
        &self,
        user_ns: &str,
        book_id: &str,
        chapters: &Vec<BookChapter>,
    ) -> Result<(), AppError> {
        let path = self.chapter_list_cache_path(user_ns, book_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| AppError::Internal(e.into()))?;
        }
        let data =
            serde_json::to_string(chapters).map_err(|e| AppError::BadRequest(e.to_string()))?;
        fs::write(&path, data)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
        Ok(())
    }

    /// 尝试开始某本书的目录后台补全；成功返回守卫，守卫 drop（含任务 panic）
    /// 时自动释放标志位——手动成对调用的旧写法在任务中断时会永久锁死该书。
    pub fn try_begin_toc_fill(&self, user_ns: &str, book_id: &str) -> Option<TocFillGuard> {
        let key = (user_ns.to_string(), book_id.to_string());
        let inserted = self.pending_toc_fills.lock().unwrap().insert(key.clone());
        inserted.then(|| TocFillGuard {
            pending: Arc::clone(&self.pending_toc_fills),
            key,
        })
    }

    pub async fn append_chapter_list_cache(
        &self,
        user_ns: &str,
        book_id: &str,
        new_chapters: &Vec<BookChapter>,
    ) -> Result<Vec<BookChapter>, AppError> {
        let mut existing = self
            .load_chapter_list_cache(user_ns, book_id)
            .await?
            .unwrap_or_default();
        // 按 URL 去重：并发/重试场景下补全任务可能与缓存中已有章节重叠，
        // 重复追加会让 index 漂移、目录出现重复项。空 URL 不参与去重
        //（个别书源的章节确实没有独立 URL）。
        let mut seen: HashSet<String> = existing.iter().map(|c| c.url.clone()).collect();
        let mut added = false;
        for ch in new_chapters {
            if ch.url.is_empty() || seen.insert(ch.url.clone()) {
                let mut ch = ch.clone();
                ch.index = existing.len() as i32;
                existing.push(ch);
                added = true;
            }
        }
        if added {
            self.save_chapter_list_cache(user_ns, book_id, &existing)
                .await?;
        }
        Ok(existing)
    }

    pub async fn delete_chapter_list_cache(
        &self,
        user_ns: &str,
        book_id: &str,
    ) -> Result<(), AppError> {
        let path = self.chapter_list_cache_path(user_ns, book_id);
        if path.exists() {
            fs::remove_file(&path)
                .await
                .map_err(|e| AppError::Internal(e.into()))?;
        }
        Ok(())
    }

    /// 删除一本书的全部源相关缓存（正文/目录/详情/换源候选/评论）。
    async fn clear_book_related_cache(&self, user_ns: &str, book_id: &str) -> Result<(), AppError> {
        let _ = self.delete_book_cache(user_ns, book_id).await;
        let _ = self.delete_book_sources_cache(user_ns, book_id).await;
        let _ = self.delete_chapter_list_cache(user_ns, book_id).await;
        let _ = fs::remove_file(self.book_info_cache_path(user_ns, book_id)).await;
        let _ = self.review_cache.remove_book(user_ns, book_id).await;
        Ok(())
    }
}

/// 单章的命中摘要数上限：一章命中太多时前三条足够判断要不要跳过去。
const MAX_SNIPPETS_PER_CHAPTER: usize = 3;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedSearchHit {
    pub chapter_index: i32,
    pub chapter_title: String,
    pub chapter_url: String,
    pub snippets: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedSearchOutcome {
    pub results: Vec<CachedSearchHit>,
    /// 实际参与检索（有正文缓存文件）的章节数
    pub cached_chapters: usize,
    /// 目录里的可检索章节总数（不含分卷行与无 URL 章）
    pub total_chapters: usize,
    /// 命中达到上限被截断：后面可能还有结果
    pub truncated: bool,
}

/// 命中处前后各取约 40 字符做摘要（按 char 边界切，不劈开多字节字符）。
fn build_search_snippet(text: &str, match_start: usize, match_end: usize) -> String {
    const CTX_CHARS: usize = 40;
    let start = text[..match_start]
        .char_indices()
        .rev()
        .take(CTX_CHARS)
        .last()
        .map(|(i, _)| i)
        .unwrap_or(0);
    let end = text[match_end..]
        .char_indices()
        .take(CTX_CHARS)
        .last()
        .map(|(i, c)| match_end + i + c.len_utf8())
        .unwrap_or(text.len());
    let mut snippet = String::new();
    if start > 0 {
        snippet.push('…');
    }
    snippet.push_str(text[start..end].trim());
    if end < text.len() {
        snippet.push('…');
    }
    snippet
}

fn apply_login_check_js(source: &BookSource, res: FetchResponse) -> FetchResponse {
    let Some(script) = source
        .login_check_js
        .as_deref()
        .filter(|script| !script.trim().is_empty())
    else {
        return res;
    };

    with_book_source(source, || {
        let str_response = StrResponse::from(res.clone());
        let mut bindings = HashMap::new();
        bindings.insert(
            "result".to_string(),
            serde_json::to_value(&str_response).unwrap_or_else(|_| json!({})),
        );
        match eval_js_with_bindings(script, &res.body, &res.url, &bindings) {
            Ok(output) if !output.trim().is_empty() => {
                if let Ok(next) = serde_json::from_str::<StrResponse>(&output) {
                    let mut next = FetchResponse::from(next);
                    inherit_missing_status_fields(&output, &res, &mut next);
                    next
                } else {
                    FetchResponse {
                        body: output,
                        ..res
                    }
                }
            }
            Ok(_) => res,
            Err(err) => {
                tracing::warn!(
                    "loginCheckJs failed for {}: {:?}",
                    source.book_source_name,
                    err
                );
                res
            }
        }
    })
}

/// JS 改写响应只给了部分字段时，状态维度继承原响应——serde 缺省会把
/// `is_successful` 填成 false、`code` 填成 0，否则改写后的成功页会被
/// 出口校验误判为上游失败。
fn inherit_missing_status_fields(output: &str, original: &FetchResponse, next: &mut FetchResponse) {
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(output)
    else {
        next.is_successful = original.is_successful;
        next.status = original.status;
        return;
    };
    let has = |keys: &[&str]| keys.iter().any(|key| map.contains_key(*key));
    if !has(&["is_successful", "isSuccessful"]) {
        next.is_successful = original.is_successful;
    }
    if !has(&["code", "status"]) {
        next.status = original.status;
    }
}

/// 上游失败状态码（403/404/限流…）透传为可读错误，不再让错误页 HTML
/// 进解析管线产出「成功但空白」的结果。
fn ensure_successful_response(res: FetchResponse) -> Result<FetchResponse, AppError> {
    if res.is_successful {
        return Ok(res);
    }
    Err(AppError::Upstream {
        status: res.status,
        message: format!("上游站点返回错误（HTTP {}）", res.status),
    })
}

/// exploreUrl 里 `@js:`/`<js>` 的求值结果缓存。
///
/// 规格 §13 要求按 `MD5(bookSourceUrl + exploreUrl)` 缓存：这类脚本经常要发网络
/// 请求取分类列表，而书海页每次进入都会问一次。键在规格基础上**另加用户命名
/// 空间**——脚本的求值结果可能含用户相关值（`java.androidId`、cache/kv），
/// 不能跨用户共享。另加 1 小时上限——进程长跑时不能永久陈旧的分类列表。
static EXPLORE_URL_CACHE: once_cell::sync::Lazy<std::sync::Mutex<BoundedMap<(String, Instant)>>> =
    once_cell::sync::Lazy::new(|| {
        std::sync::Mutex::new(BoundedMap::new(EXPLORE_URL_CACHE_MAX_ENTRIES))
    });
const EXPLORE_URL_CACHE_TTL: Duration = Duration::from_secs(3600);
const EXPLORE_URL_CACHE_MAX_ENTRIES: usize = 256;

fn cached_explore_script(
    source: &BookSource,
    user_ns: &str,
    raw: &str,
    evaluate: impl FnOnce() -> anyhow::Result<String>,
) -> Result<String, AppError> {
    let key = md5_hex(&format!("{user_ns}|{}|{}", source.book_source_url, raw));
    {
        let cache = EXPLORE_URL_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((value, stored_at)) = cache.get(&key) {
            if stored_at.elapsed() < EXPLORE_URL_CACHE_TTL {
                return Ok(value.clone());
            }
        }
    }

    let value = evaluate().map_err(AppError::Internal)?;
    EXPLORE_URL_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, (value.clone(), Instant::now()));
    Ok(value)
}

/// 本地书（TXT/EPUB 上传）的 bookUrl 形态，限额与清理按它识别。
fn is_local_book_url(url: &str) -> bool {
    crate::service::local_txt_book::is_local_txt_url(url)
        || crate::service::local_epub_book::is_local_epub_url(url)
}

fn parse_explore_kinds(source: &BookSource, user_ns: &str) -> Result<Vec<ExploreKind>, AppError> {
    let Some(raw) = source
        .explore_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(Vec::new());
    };

    let text = with_book_source(source, || {
        if let Some(script) = raw.strip_prefix("@js:") {
            cached_explore_script(source, user_ns, raw, || {
                eval_js(script, "", &source.book_source_url)
            })
        } else if let Some(script) = raw
            .strip_prefix("<js>")
            .and_then(|value| value.strip_suffix("</js>"))
        {
            cached_explore_script(source, user_ns, raw, || {
                eval_js(script, "", &source.book_source_url)
            })
        } else {
            Ok(raw.to_string())
        }
    })?;

    for json_text in [&text, &normalize_relaxed_explore_json(&text)] {
        if let Ok(kinds) = serde_json::from_str::<Vec<ExploreKind>>(json_text) {
            return Ok(kinds
                .into_iter()
                .filter(|kind| !kind.title.trim().is_empty())
                .collect());
        }
    }

    let splitter = regex::Regex::new(r"(&&|\n)+").unwrap();
    Ok(splitter
        .split(&text)
        .filter_map(|item| {
            let item = item.trim();
            if item.is_empty() {
                return None;
            }
            let mut parts = item.splitn(2, "::");
            let title = parts.next().unwrap_or_default().trim();
            if title.is_empty() {
                return None;
            }
            let url = parts
                .next()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            Some(ExploreKind {
                title: title.to_string(),
                url,
                style: None,
            })
        })
        .collect())
}

fn normalize_relaxed_explore_json(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    let mut in_string = false;
    let mut quote = '\0';
    let mut escaped = false;

    for ch in text.chars() {
        if in_string {
            normalized.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == quote {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' | '\'' => {
                in_string = true;
                quote = ch;
                normalized.push(ch);
            }
            '<' => normalized.push('{'),
            '>' => normalized.push('}'),
            _ => normalized.push(ch),
        }
    }

    normalized
}

fn parse_window_rate(rate: &str) -> Option<(usize, u64)> {
    let (limit, window) = rate.split_once('/')?;
    let limit = limit.trim().parse().ok()?;
    let window = window.trim().parse().ok()?;
    Some((limit, window))
}

fn should_follow_content_page(chapter_url: &str, current_url: &str, next_url: &str) -> bool {
    let next_url = strip_fragment(next_url);
    let current_url = strip_fragment(current_url);
    let chapter_url = strip_fragment(chapter_url);

    if next_url == current_url || next_url == chapter_url {
        return false;
    }

    match (
        url::Url::parse(chapter_url),
        url::Url::parse(current_url),
        url::Url::parse(next_url),
    ) {
        (Ok(chapter), Ok(current), Ok(next)) => {
            if chapter.scheme() != next.scheme()
                || chapter.host_str() != next.host_str()
                || chapter.port_or_known_default() != next.port_or_known_default()
            {
                return false;
            }

            let chapter_exact = content_path_exact_base(chapter.path());
            let current_exact = content_path_exact_base(current.path());
            let next_exact = content_path_exact_base(next.path());
            let next_page_base = content_path_page_base(next.path());

            next_exact == chapter_exact
                || next_exact == current_exact
                || next_page_base == chapter_exact
                || next_page_base == current_exact
        }
        _ => {
            let chapter_exact = content_path_exact_base(chapter_url);
            let current_exact = content_path_exact_base(current_url);
            let next_exact = content_path_exact_base(next_url);
            let next_page_base = content_path_page_base(next_url);

            next_exact == chapter_exact
                || next_exact == current_exact
                || next_page_base == chapter_exact
                || next_page_base == current_exact
        }
    }
}

fn strip_fragment(url: &str) -> &str {
    url.split_once('#').map(|(head, _)| head).unwrap_or(url)
}

fn content_path_exact_base(path: &str) -> String {
    content_path_base(path, false)
}

fn content_path_page_base(path: &str) -> String {
    content_path_base(path, true)
}

fn content_path_base(path: &str, strip_page_suffix: bool) -> String {
    let (dir, file) = path.rsplit_once('/').unwrap_or(("", path));
    let (stem, _ext) = file.rsplit_once('.').unwrap_or((file, ""));
    let stem = if strip_page_suffix {
        strip_page_suffix_from_stem(stem)
    } else {
        stem
    };
    if dir.is_empty() {
        stem.to_string()
    } else {
        format!("{dir}/{stem}")
    }
}

fn strip_page_suffix_from_stem(stem: &str) -> &str {
    for sep in ['-', '_'] {
        if let Some(idx) = stem.rfind(sep) {
            let suffix = &stem[idx + sep.len_utf8()..];
            if !suffix.is_empty()
                && suffix.chars().all(|ch| ch.is_ascii_digit())
                && suffix
                    .parse::<usize>()
                    .map(|page| page >= 2)
                    .unwrap_or(false)
            {
                return &stem[..idx];
            }
        }
    }
    stem
}

fn cookie_domain(source_url: &str) -> String {
    let normalized = normalize_source_url(source_url);
    let host = url::Url::parse(&normalized)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or(normalized);
    if host.parse::<std::net::IpAddr>().is_ok() {
        return host;
    }
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let parts = host.split('.').collect::<Vec<_>>();
    if parts.len() <= 2 {
        return host.to_string();
    }
    let second_level = parts[parts.len() - 2];
    let last = parts[parts.len() - 1];
    if last.len() == 2
        && matches!(second_level, "com" | "net" | "org" | "gov" | "edu" | "co")
        && parts.len() >= 3
    {
        parts[parts.len() - 3..].join(".")
    } else {
        parts[parts.len() - 2..].join(".")
    }
}

fn sanitize_book_urls(book: &mut Book) {
    book.book_url = repair_encoded_url(&book.book_url);
    book.origin = normalize_source_url(&book.origin);
    if let Some(toc_url) = &book.toc_url {
        book.toc_url = Some(repair_encoded_url(toc_url));
    }
    if let Some(cover_url) = &book.cover_url {
        book.cover_url = Some(repair_encoded_url(cover_url));
    }
}

/// 本地书的伪书源标记：登记表里它们的 book_source_id 为 NULL。
fn local_origin_of(book_url: &str) -> (String, Option<String>) {
    if crate::service::local_txt_book::is_local_txt_url(book_url) {
        return (
            crate::service::local_txt_book::LOCAL_TXT_ORIGIN.to_string(),
            Some("本地 TXT".to_string()),
        );
    }
    if crate::service::local_epub_book::is_local_epub_url(book_url) {
        return (
            crate::service::local_epub_book::LOCAL_EPUB_ORIGIN.to_string(),
            Some("本地 EPUB".to_string()),
        );
    }
    (String::new(), None)
}

/// 书架行 + 登记表/书源 JOIN 结果 → 前端 Book 对象。
fn assemble_shelf_book(entry: ShelfEntry) -> Book {
    let (origin, origin_name) = match (entry.origin_url, entry.origin_name) {
        (Some(url), name) => (url, name),
        (None, _) => local_origin_of(&entry.book_url),
    };
    let shelf = entry.shelf;
    Book {
        book_id: Some(shelf.book_id),
        book_source_id: entry.book_source_id,
        name: entry.name,
        author: entry.author,
        book_url: entry.book_url,
        origin,
        origin_name,
        cover_url: shelf.cover_url,
        toc_url: entry.toc_url,
        charset: shelf.charset,
        custom_cover_url: shelf.custom_cover_url,
        can_update: shelf.can_update,
        dur_chapter_index: shelf.dur_chapter_index,
        dur_chapter_pos: shelf.dur_chapter_pos,
        dur_chapter_time: shelf.dur_chapter_time,
        dur_chapter_title: shelf.dur_chapter_title,
        intro: shelf.intro,
        latest_chapter_title: shelf.latest_chapter_title,
        last_check_time: shelf.last_check_time,
        total_chapter_num: shelf.total_chapter_num,
        r#type: shelf.book_type,
        group: shelf.group_bits,
        word_count: shelf.word_count,
        info_html: shelf.info_html,
        toc_html: shelf.toc_html,
        kind: shelf.kind,
        update_time: shelf.update_time,
        can_re_name: shelf.can_re_name,
        download_urls: shelf.download_urls,
    }
}

/// Book → 书架行。身份/绑定字段（bookUrl/origin/tocUrl/name/author/bookSourceId）
/// 不落 shelf_books——它们在 books 登记表里，读取时 JOIN 注入。
fn shelf_row_of(book: &Book) -> ShelfBookRow {
    ShelfBookRow {
        book_id: book.book_id.clone().unwrap_or_default(),
        position: 0, // 由 repo 层在 upsert 时解析（既有行保留原位，新行排末）
        group_bits: book.group,
        dur_chapter_index: book.dur_chapter_index,
        dur_chapter_pos: book.dur_chapter_pos,
        dur_chapter_time: book.dur_chapter_time,
        dur_chapter_title: book.dur_chapter_title.clone(),
        cover_url: book.cover_url.clone(),
        custom_cover_url: book.custom_cover_url.clone(),
        can_update: book.can_update,
        intro: book.intro.clone(),
        latest_chapter_title: book.latest_chapter_title.clone(),
        last_check_time: book.last_check_time,
        total_chapter_num: book.total_chapter_num,
        book_type: book.r#type,
        word_count: book.word_count.clone(),
        info_html: book.info_html.clone(),
        toc_html: book.toc_html.clone(),
        kind: book.kind.clone(),
        update_time: book.update_time.clone(),
        can_re_name: book.can_re_name.clone(),
        download_urls: book.download_urls.clone(),
        charset: book.charset.clone(),
    }
}

/// toc_fill 进行中守卫：RAII，drop 即释放（含 spawn 任务 panic/取消）。
pub struct TocFillGuard {
    pending: Arc<Mutex<HashSet<(String, String)>>>,
    key: (String, String),
}

impl Drop for TocFillGuard {
    fn drop(&mut self) {
        self.pending.lock().unwrap().remove(&self.key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db;
    use crate::storage::db::repo::{BookRepo, ShelfBookRepo};

    fn fetch_response(status: u16, is_successful: bool) -> FetchResponse {
        FetchResponse {
            url: "http://example.com".into(),
            status,
            body: String::new(),
            content_type: None,
            headers: vec![],
            is_successful,
        }
    }

    #[test]
    fn ensure_successful_response_passes_2xx_and_flags_failures() {
        assert!(ensure_successful_response(fetch_response(200, true)).is_ok());

        let err = ensure_successful_response(fetch_response(403, false)).unwrap_err();
        match err {
            AppError::Upstream { status, message } => {
                assert_eq!(status, 403);
                assert!(message.contains("403"));
            }
            other => panic!("应为 Upstream，实际 {other:?}"),
        }
    }

    #[test]
    fn inherit_missing_status_fields_keeps_original_status_when_js_omits_them() {
        // 与 apply_login_check_js 同一条路径：JS 输出先按 StrResponse 解析再继承
        let rewrite = |output: &str, original: &FetchResponse| {
            let mut next =
                FetchResponse::from(serde_json::from_str::<StrResponse>(output).unwrap());
            inherit_missing_status_fields(output, original, &mut next);
            next
        };
        let original = fetch_response(200, true);

        // JS 只回 body：状态维度继承原响应，否则改写后的成功页被误判为失败
        let next = rewrite(r#"{"body":"改写后"}"#, &original);
        assert!(next.is_successful);
        assert_eq!(next.status, 200);
        assert_eq!(next.body, "改写后");

        // JS 显式给了状态字段（camelCase 序列化）：尊重 JS 的值，包括显式标记失败
        let next = rewrite(r#"{"body":"x","code":404,"isSuccessful":false}"#, &original);
        assert!(!next.is_successful);
        assert_eq!(next.status, 404);
    }

    async fn test_service(storage_dir: &std::path::Path) -> BookService {
        // init_pool 不会建父目录，先落目录再开库
        std::fs::create_dir_all(storage_dir).unwrap();
        let pool = db::init_pool(&format!(
            "sqlite:{}?mode=rwc",
            storage_dir.join("reader.db").display()
        ))
        .await
        .unwrap();
        BookService::new(
            HttpClient::new(5, None).unwrap(),
            RuleEngine::new().unwrap(),
            FileCache::new(storage_dir.join("cache"), 0),
            storage_dir.to_str().unwrap(),
            BookRepo::new(pool.clone()),
            ShelfBookRepo::new(pool),
        )
    }

    #[tokio::test]
    async fn append_chapter_list_cache_dedups_by_url_and_keeps_index_sequential() {
        let storage_dir =
            std::env::temp_dir().join(format!("reader-rust-toc-dedup-{}", std::process::id()));
        let service = test_service(&storage_dir).await;
        let ns = "test-dedup";
        let book_id = "book_1";

        let mk = |index: i32, url: &str| BookChapter {
            url: url.to_string(),
            title: url.to_string(),
            index,
            ..Default::default()
        };
        service
            .save_chapter_list_cache(ns, book_id, &vec![mk(0, "a"), mk(1, "b")])
            .await
            .unwrap();
        // 补全结果与缓存尾部重叠（竞态重放），另含一个空 URL 章节
        let all = service
            .append_chapter_list_cache(ns, book_id, &vec![mk(9, "b"), mk(9, "c"), mk(9, "")])
            .await
            .unwrap();
        let urls: Vec<&str> = all.iter().map(|c| c.url.as_str()).collect();
        assert_eq!(urls, vec!["a", "b", "c", ""]);
        let indexes: Vec<i32> = all.iter().map(|c| c.index).collect();
        assert_eq!(indexes, vec![0, 1, 2, 3]);
        // 再次追加完全重叠的结果：无新增时不重写缓存
        let again = service
            .append_chapter_list_cache(ns, book_id, &vec![mk(9, "c")])
            .await
            .unwrap();
        assert_eq!(again.len(), 4);

        // 进行中守卫（RAII：drop 即释放）
        let guard = service.try_begin_toc_fill(ns, book_id);
        assert!(guard.is_some());
        assert!(
            service.try_begin_toc_fill(ns, book_id).is_none(),
            "同一本书不允许并发补全"
        );
        drop(guard);
        assert!(
            service.try_begin_toc_fill(ns, book_id).is_some(),
            "守卫释放后可再次开始"
        );
        let _ = std::fs::remove_dir_all(&storage_dir);
    }

    #[tokio::test]
    async fn window_rate_waits_when_existing_starts_reach_limit() {
        let storage_dir =
            std::env::temp_dir().join(format!("reader-rust-window-rate-{}", std::process::id()));
        let service = test_service(&storage_dir).await;
        let now = Instant::now();
        service.rate_states.write().await.insert(
            "source".to_string(),
            RateState {
                window_starts: vec![now, now],
                ..Default::default()
            },
        );

        // 两个起始点已占满限额：必须等到最旧起点滑出窗口（~200ms）后放行。
        // 只断言「20ms 内不通过」证明不了会等——窗口滑出后仍卡住也能让旧断言蒙混过关
        let began = Instant::now();
        let result = tokio::time::timeout(
            Duration::from_millis(2000),
            service.wait_for_window_rate("source", 2, 200),
        )
        .await;

        let _ = tokio::fs::remove_dir_all(&storage_dir).await;
        assert!(result.is_ok(), "窗口滑出后应放行而非卡住");
        assert!(
            began.elapsed() >= Duration::from_millis(180),
            "应等到窗口滑出（200ms）才放行，实际 {:?}",
            began.elapsed()
        );
    }

    fn make_book(url: &str) -> Book {
        Book {
            book_url: url.to_string(),
            origin: "local".to_string(),
            name: format!("书 {url}"),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn bookshelf_list_keeps_intro_but_drops_heavy_snapshots() {
        // 窄列拆分（SHELF_LIST_COLS）只该去掉 info_html/toc_html 两个大快照；
        // intro 是详情弹窗直接吃列表对象的字段，丢了书架上的书简介全空
        let storage_dir =
            std::env::temp_dir().join(format!("reader-rust-shelf-intro-{}", std::process::id()));
        let service = test_service(&storage_dir).await;

        let mut book = make_book("intro-book");
        book.intro = Some("这是一本测试书的简介。".to_string());
        service.save_book("u", book).await.unwrap();

        let shelf = service.get_bookshelf("u").await.unwrap();
        assert_eq!(shelf.len(), 1);
        assert_eq!(
            shelf[0].intro.as_deref(),
            Some("这是一本测试书的简介。"),
            "书架列表必须带 intro"
        );
        // 大快照字段仍然不进列表（窄列的意义所在）
        assert!(shelf[0].info_html.is_none() || shelf[0].info_html.as_deref() == Some(""));

        let _ = tokio::fs::remove_dir_all(&storage_dir).await;
    }

    #[tokio::test]
    async fn shelf_limit_holds_under_concurrent_saves() {
        let storage_dir =
            std::env::temp_dir().join(format!("reader-rust-limit-race-{}", std::process::id()));
        let service = std::sync::Arc::new(test_service(&storage_dir).await.with_user_book_limit(2));

        // 限额检查与写入在同一事务（upsert_with_limit）：并发 8 本只能进 2 本
        let mut handles = Vec::new();
        for i in 0..8 {
            let svc = service.clone();
            handles.push(tokio::spawn(async move {
                svc.save_book("u", make_book(&format!("race-{i}"))).await
            }));
        }
        let mut ok = 0usize;
        let mut rejected = 0usize;
        for h in handles {
            match h.await.unwrap() {
                Ok(_) => ok += 1,
                Err(_) => rejected += 1,
            }
        }
        assert_eq!(ok, 2, "限额 2 下并发 8 本只能成 2 本");
        assert_eq!(rejected, 6);
        assert_eq!(service.get_bookshelf("u").await.unwrap().len(), 2);

        let _ = tokio::fs::remove_dir_all(&storage_dir).await;
    }

    #[tokio::test]
    async fn user_book_limit_blocks_new_books_but_not_updates() {
        let storage_dir =
            std::env::temp_dir().join(format!("reader-rust-book-limit-{}", std::process::id()));
        let service = test_service(&storage_dir).await.with_user_book_limit(2);

        service.save_book("u", make_book("a")).await.unwrap();
        service.save_book("u", make_book("b")).await.unwrap();
        // 第三本新书被拒
        let err = service.save_book("u", make_book("c")).await.unwrap_err();
        assert!(err.to_string().contains("书架书籍数量不能超过"));
        // 已有书的更新（如阅读进度）不受限
        let mut existing = make_book("a");
        existing.dur_chapter_index = Some(3);
        service.save_book("u", existing).await.unwrap();
        assert_eq!(service.get_bookshelf("u").await.unwrap().len(), 2);

        // 整架替换（saveBooks）按替换后总量校验
        service
            .save_books("u", vec![make_book("x"), make_book("y")], false)
            .await
            .unwrap();
        let err = service
            .save_books(
                "u",
                vec![make_book("x"), make_book("y"), make_book("z")],
                false,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("书架书籍数量不能超过"));

        // 0 = 不限
        let unlimited = test_service(&storage_dir).await;
        for i in 0..5 {
            unlimited
                .save_book("u", make_book(&format!("b{i}")))
                .await
                .unwrap();
        }

        let _ = tokio::fs::remove_dir_all(&storage_dir).await;
    }

    #[tokio::test]
    async fn user_local_book_limit_blocks_local_books_saved_directly() {
        let storage_dir =
            std::env::temp_dir().join(format!("reader-rust-local-limit-{}", std::process::id()));
        let service = test_service(&storage_dir)
            .await
            .with_user_local_book_limit(1);

        // 直构 local-txt 书入架（不经上传 handler）同样受本地书限额约束
        service
            .save_book("u", make_book("local-txt:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"))
            .await
            .unwrap();
        let err = service
            .save_book("u", make_book("local-txt:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("本地书籍数量不能超过"));
        // 已在架上的同一本书更新不受限
        service
            .save_book("u", make_book("local-txt:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"))
            .await
            .unwrap();
        // 普通书不受本地书限额影响
        service
            .save_book("u", make_book("https://example.com/book"))
            .await
            .unwrap();

        // 整架替换按替换后的本地书总量校验
        let err = service
            .save_books(
                "u",
                vec![
                    make_book("local-txt:cccccccccccccccccccccccccccccccc"),
                    make_book("local-txt:dddddddddddddddddddddddddddddddd"),
                ],
                false,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("本地书籍数量不能超过"));

        let _ = tokio::fs::remove_dir_all(&storage_dir).await;
    }

    #[tokio::test]
    async fn restore_aligns_backup_book_ids() {
        let storage_dir =
            std::env::temp_dir().join(format!("reader-rust-restore-align-{}", std::process::id()));
        let service = test_service(&storage_dir).await;

        let mut a = make_book("https://example.com/a");
        a.book_id = Some("book_7".to_string());
        let mut b = make_book("https://example.com/b");
        b.book_id = Some("book_3".to_string());
        let saved = service.save_books("u", vec![a, b], true).await.unwrap();

        // 空表重建无冲突，备份自带 id 原位对齐
        let ids: Vec<&str> = saved.iter().filter_map(|b| b.book_id.as_deref()).collect();
        assert_eq!(ids, ["book_7", "book_3"]);
        // 后续登记自动分配不撞已占用的 id
        let c = service
            .register_book(
                "u",
                RegisterBook {
                    book_url: "https://example.com/c",
                    book_source_id: None,
                    name: None,
                    author: None,
                    toc_url: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(c, "book_8");

        let _ = tokio::fs::remove_dir_all(&storage_dir).await;
    }

    #[tokio::test]
    async fn restore_rolls_back_atomically_when_book_id_invalid() {
        let storage_dir =
            std::env::temp_dir().join(format!("reader-rust-restore-tx-{}", std::process::id()));
        let service = test_service(&storage_dir).await;

        // 先在架两本，作为「恢复前状态」
        service
            .save_books(
                "u",
                vec![
                    make_book("https://example.com/a"),
                    make_book("https://example.com/b"),
                ],
                false,
            )
            .await
            .unwrap();

        // 恢复备份：第一本合法，第二本 bookId 非法 → 事务整体回滚
        let good = make_book("https://example.com/c");
        let mut bad = make_book("https://example.com/d");
        bad.book_id = Some("../evil".to_string());
        let err = service
            .save_books("u", vec![good, bad], true)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("bookId 格式非法"));

        // 书架与登记表都还是恢复前的样子（a/b 在架，c 未写入）
        let shelf = service.get_bookshelf("u").await.unwrap();
        let urls: Vec<&str> = shelf.iter().map(|b| b.book_url.as_str()).collect();
        assert_eq!(urls, ["https://example.com/a", "https://example.com/b"]);
        assert!(service
            .book_repo()
            .get_by_url("u", "https://example.com/c")
            .await
            .unwrap()
            .is_none());

        let _ = tokio::fs::remove_dir_all(&storage_dir).await;
    }

    #[tokio::test]
    async fn purge_user_cache_keeps_shelf_registrations_and_recycles_orphans() {
        let storage_dir =
            std::env::temp_dir().join(format!("reader-rust-purge-{}", std::process::id()));
        let service = test_service(&storage_dir).await;

        // 在架一本 + 非在架登记一本
        service
            .save_book("u", make_book("https://example.com/onshelf"))
            .await
            .unwrap();
        service
            .register_book(
                "u",
                RegisterBook {
                    book_url: "https://example.com/orphan",
                    book_source_id: None,
                    name: None,
                    author: None,
                    toc_url: None,
                },
            )
            .await
            .unwrap();

        service.purge_user_cache("u").await.unwrap();

        let repo = service.book_repo();
        // 在架书的登记行保留（身份不动），孤儿行被回收
        assert!(repo
            .get_by_url("u", "https://example.com/onshelf")
            .await
            .unwrap()
            .is_some());
        assert!(repo
            .get_by_url("u", "https://example.com/orphan")
            .await
            .unwrap()
            .is_none());

        // 合法语义保持不变：书架为空的用户 purge 后登记行全清
        service
            .register_book(
                "v",
                RegisterBook {
                    book_url: "https://example.com/orphan2",
                    book_source_id: None,
                    name: None,
                    author: None,
                    toc_url: None,
                },
            )
            .await
            .unwrap();
        service.purge_user_cache("v").await.unwrap();
        assert!(repo
            .get_by_url("v", "https://example.com/orphan2")
            .await
            .unwrap()
            .is_none());

        let _ = tokio::fs::remove_dir_all(&storage_dir).await;
    }
}
