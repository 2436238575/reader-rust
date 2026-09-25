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
    source_cookies: Arc<RwLock<HashMap<String, String>>>,
    rate_states: Arc<RwLock<HashMap<String, RateState>>>,
    /// 最近抓到的章节正文响应体，用于求值评论 URL（见 `chapter_source_body`）。
    recent_bodies: Arc<RwLock<HashMap<String, RecentBody>>>,
    /// 单用户书架上限（0 = 不限），来自 `USER_BOOK_LIMIT`。
    user_book_limit: u32,
    /// 单用户本地书上限（0 = 不限），来自 `USER_LOCAL_BOOK_LIMIT`。
    user_local_book_limit: u32,
    /// 封面缓存目录的容量上限（0 = 不限），来自 `CACHE_COVER_LIMIT_BYTES`。
    cover_cache_limit: u64,
    /// 正在后台补全目录的 `(user_ns, toc_url)` 集合：防止同一本书并发触发
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
/// 正文响应体内存暂存的条数、单条上限与存活时间。
const RECENT_BODY_LIMIT: usize = 32;
const RECENT_BODY_MAX_BYTES: usize = 1024 * 1024;
const RECENT_BODY_TTL: Duration = Duration::from_secs(600);

/// 书籍详情缓存的存活时间（10 分钟）与目录名。
///
/// 详情页、仅传 bookUrl 拉目录、翻章按 index 换算 chapterUrl 都会走
/// `get_book_info`，此前每次都打上游。详情不常变，短 TTL 足够新；
/// `refresh=1` 与 saveBook/换源路径走旁路强刷。文件随 `CacheKind::ChapterList`
/// 清理（详情与目录同属书籍元数据）。
const BOOK_INFO_CACHE_TTL: Duration = Duration::from_secs(600);
const BOOK_INFO_CACHE_DIR: &str = "bookinfo";

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
    pub fn new(http: HttpClient, parser: RuleEngine, cache: FileCache, storage_dir: &str) -> Self {
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
            source_cookies: Arc::new(RwLock::new(HashMap::new())),
            rate_states: Arc::new(RwLock::new(HashMap::new())),
            recent_bodies: Arc::new(RwLock::new(HashMap::new())),
            user_book_limit: 0,
            user_local_book_limit: 0,
            cover_cache_limit: DEFAULT_COVER_CACHE_BYTES,
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

    /// 设置封面缓存目录容量上限（0 = 不限）。
    pub fn with_cover_cache_limit(mut self, limit: u64) -> Self {
        self.cover_cache_limit = limit;
        self
    }

    /// 取某个用户命名空间下的独立 HTTP 客户端（独立 Cookie jar）。
    pub fn http_client(&self, user_ns: &str) -> anyhow::Result<reqwest::Client> {
        self.http.client_for(user_ns)
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
    async fn login_check_blocking(
        &self,
        user_ns: &str,
        source: &BookSource,
        res: FetchResponse,
    ) -> Result<FetchResponse, AppError> {
        let user_ns = user_ns.to_string();
        let source = source.clone();
        parse_blocking(move || with_user_ns(&user_ns, || apply_login_check_js(&source, res))).await
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
        let result = fetch(&self.http, user_ns, spec).await;
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
        let books = self
            .parse_response_blocking(user_ns, source, &res, |p, s, b, u| p.search_books(s, b, u))
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
        self.parse_response_blocking(user_ns, source, &res, |p, s, b, u| p.explore_books(s, b, u))
            .await
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

    pub async fn get_book_info(
        &self,
        user_ns: &str,
        source: &BookSource,
        book_url: &str,
        force_refresh: bool,
    ) -> Result<Book, AppError> {
        if !force_refresh {
            if let Some(cached) = self.load_book_info_cache(user_ns, book_url).await {
                return Ok(cached);
            }
        }
        let res = self
            .fetch_source_url(user_ns, source, book_url, &source.book_source_url)
            .await?;
        let book_url_owned = book_url.to_string();
        let book = self
            .parse_response_blocking(user_ns, source, &res, move |p, s, b, u| {
                p.book_info(s, b, u, &book_url_owned)
            })
            .await?;
        let _ = self.save_book_info_cache(user_ns, book_url, &book).await;
        Ok(book)
    }

    fn book_info_cache_path(&self, user_ns: &str, book_url: &str) -> PathBuf {
        self.storage_dir
            .join("cache")
            .join(BOOK_INFO_CACHE_DIR)
            .join(user_ns)
            .join(format!("{}.json", md5_hex(book_url)))
    }

    async fn load_book_info_cache(&self, user_ns: &str, book_url: &str) -> Option<Book> {
        let path = self.book_info_cache_path(user_ns, book_url);
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

    async fn save_book_info_cache(&self, user_ns: &str, book_url: &str, book: &Book) {
        let path = self.book_info_cache_path(user_ns, book_url);
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
        source: &BookSource,
        toc_url: &str,
    ) -> Result<Vec<BookChapter>, AppError> {
        self.get_chapter_list_with_cache(user_ns, source, toc_url, false)
            .await
    }

    pub async fn get_chapter_list_with_cache(
        &self,
        user_ns: &str,
        source: &BookSource,
        toc_url: &str,
        force_refresh: bool,
    ) -> Result<Vec<BookChapter>, AppError> {
        // Check cache first (unless force refresh)
        if !force_refresh {
            if let Ok(Some(cached)) = self.load_chapter_list_cache(user_ns, toc_url).await {
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
            .save_chapter_list_cache(user_ns, toc_url, &chapters)
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

    /// Continue fetching remaining chapters from pagination state
    pub async fn fetch_remaining_chapters(
        &self,
        pagination: ChapterPagination,
    ) -> Result<Vec<BookChapter>, AppError> {
        let mut all_chapters = Vec::new();
        let mut visited_page_urls: std::collections::HashSet<String> =
            pagination.visited_urls.iter().cloned().collect();
        let mut seen_chapter_urls: std::collections::HashSet<String> =
            pagination.seen_chapter_urls.iter().cloned().collect();
        let mut chapter_index = pagination.next_index;

        let pending_urls: Vec<String> = pagination
            .pending_urls
            .into_iter()
            .filter(|u| !visited_page_urls.contains(u))
            .collect();

        if pending_urls.len() > 1 {
            // Multiple URLs from option dropdown - fetch all pages
            for url in pending_urls {
                if visited_page_urls.contains(&url) {
                    continue;
                }
                visited_page_urls.insert(url.clone());

                let res = self
                    .fetch_source_url(
                        &pagination.user_ns,
                        &pagination.source,
                        &url,
                        &pagination.source.book_source_url,
                    )
                    .await?;
                let (chapters, _) =
                    self.parser
                        .chapter_list(&pagination.source, &res.body, &res.url);

                // Check if this page is a duplicate (all chapters already seen)
                // This handles cases where the first page URL differs from toc_url (e.g., different domain)
                let all_seen = chapters
                    .iter()
                    .all(|ch| seen_chapter_urls.contains(&ch.url));
                if all_seen && !chapters.is_empty() {
                    tracing::debug!("Skipping duplicate page: {}", url);
                    continue;
                }

                for ch in chapters {
                    if seen_chapter_urls.contains(&ch.url) {
                        continue;
                    }
                    seen_chapter_urls.insert(ch.url.clone());

                    all_chapters.push(BookChapter {
                        title: ch.title,
                        url: ch.url,
                        index: chapter_index,
                        ..Default::default()
                    });
                    chapter_index += 1;
                }
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
                    .fetch_source_url(
                        &pagination.user_ns,
                        &pagination.source,
                        &current_url,
                        &pagination.source.book_source_url,
                    )
                    .await?;
                let (chapters, next_urls) =
                    self.parser
                        .chapter_list(&pagination.source, &res.body, &res.url);

                // Check if this page is a duplicate
                let all_seen = chapters
                    .iter()
                    .all(|ch| seen_chapter_urls.contains(&ch.url));
                if all_seen && !chapters.is_empty() {
                    tracing::debug!("Skipping duplicate page: {}", current_url);
                    break; // Stop following pagination if we hit a duplicate page
                }

                for ch in chapters {
                    if seen_chapter_urls.contains(&ch.url) {
                        continue;
                    }
                    seen_chapter_urls.insert(ch.url.clone());

                    all_chapters.push(BookChapter {
                        title: ch.title,
                        url: ch.url,
                        index: chapter_index,
                        ..Default::default()
                    });
                    chapter_index += 1;
                }

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

    async fn get_chapter_list_with_pagination(
        &self,
        user_ns: &str,
        source: &BookSource,
        toc_url: &str,
    ) -> Result<(Vec<BookChapter>, Vec<String>), AppError> {
        let mut all_chapters = Vec::new();
        let mut visited_page_urls = std::collections::HashSet::new();
        let mut seen_chapter_urls = std::collections::HashSet::new();
        let mut chapter_index = 0i32;

        // Fetch first page
        let res = self
            .fetch_source_url(user_ns, source, toc_url, &source.book_source_url)
            .await?;
        let (chapters, next_urls) = self
            .parse_response_blocking(user_ns, source, &res, |p, s, b, u| p.chapter_list(s, b, u))
            .await?;

        visited_page_urls.insert(toc_url.to_string());

        // Add first page chapters with deduplication
        for ch in chapters {
            if seen_chapter_urls.contains(&ch.url) {
                continue;
            }
            seen_chapter_urls.insert(ch.url.clone());
            all_chapters.push(BookChapter {
                title: ch.title,
                url: ch.url,
                index: chapter_index,
                ..Default::default()
            });
            chapter_index += 1;
        }

        // Determine how to handle pagination
        // Filter out already visited URLs
        let pending_urls: Vec<String> = next_urls
            .into_iter()
            .filter(|u| !visited_page_urls.contains(u))
            .collect();

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

                for ch in chapters {
                    if seen_chapter_urls.contains(&ch.url) {
                        continue;
                    }
                    seen_chapter_urls.insert(ch.url.clone());
                    all_chapters.push(BookChapter {
                        title: ch.title,
                        url: ch.url,
                        index: chapter_index,
                        ..Default::default()
                    });
                    chapter_index += 1;
                }
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

                for ch in chapters {
                    if seen_chapter_urls.contains(&ch.url) {
                        continue;
                    }
                    seen_chapter_urls.insert(ch.url.clone());
                    all_chapters.push(BookChapter {
                        title: ch.title,
                        url: ch.url,
                        index: chapter_index,
                        ..Default::default()
                    });
                    chapter_index += 1;
                }

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

        Ok((all_chapters, visited_page_urls.into_iter().collect()))
    }

    pub async fn get_content(
        &self,
        user_ns: &str,
        book_url: &str,
        source: &BookSource,
        chapter_url: &str,
    ) -> Result<String, AppError> {
        let book_key = md5_hex(book_url);
        tracing::debug!(
            "get_content called, chapter_url={}, book_key={}",
            chapter_url,
            book_key
        );
        if let Ok(Some(cached)) = self.cache.get(user_ns, &book_key, chapter_url).await {
            tracing::debug!("get_content returning cached content, len={}", cached.len());
            return Ok(cached);
        }
        tracing::debug!("get_content cache miss, fetching from network");

        let mut all_content = String::new();
        let mut visited_urls = std::collections::HashSet::new();
        let mut current_url = chapter_url.to_string();

        // Follow pagination to get all content pages
        loop {
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
        if !all_content.is_empty() {
            let _ = self
                .cache
                .put(user_ns, &book_key, chapter_url, &all_content)
                .await;
        }
        Ok(all_content)
    }

    /// Delete all chapter content cache for a book
    pub async fn delete_book_cache(&self, user_ns: &str, book_url: &str) -> Result<bool, AppError> {
        let book_key = md5_hex(book_url);
        self.cache
            .remove_book(user_ns, &book_key)
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
            if bodies.len() >= RECENT_BODY_LIMIT {
                bodies.clear();
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
        let book_key = md5_hex(book_url);
        let cache_key = format!(
            "chapter|{chapter_url}|{page}|{count}|{}",
            sort.as_ctx_value()
        );
        if !refresh {
            if let Some(cached) = self.load_review_cache(user_ns, &book_key, &cache_key).await {
                return Ok(ReviewResponse::new(true, cached).with_server_sort(server_sort));
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
            self.store_review_cache(user_ns, &book_key, &cache_key, &result)
                .await;
        }
        Ok(ReviewResponse::new(true, result).with_server_sort(server_sort))
    }

    /// 段评概览：本章哪些段落有段评、各有多少条。
    pub async fn get_para_review_index(
        &self,
        user_ns: &str,
        source: &BookSource,
        book_url: &str,
        chapter_url: &str,
        refresh: bool,
    ) -> Result<ReviewResponse<ParaReviewIndex>, AppError> {
        if !self.parser.has_para_review_rule(source) {
            return Ok(ReviewResponse::new(false, ParaReviewIndex::default()));
        }
        let book_key = md5_hex(book_url);
        let cache_key = format!("para-index|{chapter_url}");
        if !refresh {
            if let Some(cached) = self.load_review_cache(user_ns, &book_key, &cache_key).await {
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
            self.store_review_cache(user_ns, &book_key, &cache_key, &index)
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
        let book_key = md5_hex(book_url);
        let cache_key = format!(
            "para|{chapter_url}|{para_index}|{page}|{count}|{}",
            sort.as_ctx_value()
        );
        if !refresh {
            if let Some(cached) = self.load_review_cache(user_ns, &book_key, &cache_key).await {
                return Ok(ReviewResponse::new(true, cached).with_server_sort(server_sort));
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
            self.store_review_cache(user_ns, &book_key, &cache_key, &result)
                .await;
        }
        Ok(ReviewResponse::new(true, result).with_server_sort(server_sort))
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

    /// 章节列表缓存目录：`<storage>/data/<ns>/chapters`。
    fn chapter_cache_dir(&self, user_ns: &str) -> PathBuf {
        self.storage_dir.join("data").join(user_ns).join("chapters")
    }

    /// 书源搜索结果缓存目录：`<storage>/data/<ns>/book_sources`。
    fn book_sources_cache_dir(&self, user_ns: &str) -> PathBuf {
        self.storage_dir
            .join("data")
            .join(user_ns)
            .join("book_sources")
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

    /// 清理单本书的缓存（正文 + 章节列表 + 搜索结果）。
    ///
    /// 封面按图片 URL 缓存、与书没有稳定对应关系，因此不在此范围内。
    pub async fn purge_book_cache(
        &self,
        user_ns: &str,
        book_url: &str,
        toc_url: Option<&str>,
    ) -> Result<CachePurgeResult, AppError> {
        let mut result = CachePurgeResult::default();
        if self
            .cache
            .remove_book(user_ns, &md5_hex(book_url))
            .await
            .map_err(internal_error)?
        {
            result.content = 1;
        }
        for url in [Some(book_url), toc_url].into_iter().flatten() {
            if self.delete_chapter_list_cache(user_ns, url).await.is_ok() {
                result.chapter_list += 1;
            }
        }
        let _ = fs::remove_file(self.book_info_cache_path(user_ns, book_url)).await;
        if self
            .delete_book_sources_cache(user_ns, book_url)
            .await
            .is_ok()
        {
            result.search_results = 1;
        }
        result.review = self
            .review_cache
            .remove_book(user_ns, &md5_hex(book_url))
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
        let cover = match user_ns {
            Some(ns) => dir_usage(&self.cover_cache_dir(ns), usize::MAX).await,
            None => dir_usage(&self.cover_cache_dir("public"), usize::MAX).await,
        };
        let chapter_list = match user_ns {
            Some(ns) => dir_usage(&self.chapter_cache_dir(ns), usize::MAX).await,
            None => self.data_subdir_usage("chapters").await,
        };
        let search_results = match user_ns {
            Some(ns) => dir_usage(&self.book_sources_cache_dir(ns), usize::MAX).await,
            None => self.data_subdir_usage("book_sources").await,
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

    /// 汇总 `data/*/<name>` 各用户子目录的占用。
    async fn data_subdir_usage(&self, name: &str) -> CacheUsage {
        use crate::storage::cache::file_cache::dir_usage;
        let mut total = CacheUsage::default();
        let Ok(mut entries) = fs::read_dir(self.storage_dir.join("data")).await else {
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
    pub async fn is_chapter_cached(
        &self,
        user_ns: &str,
        book_url: &str,
        chapter_url: &str,
    ) -> bool {
        let book_key = md5_hex(book_url);
        self.cache.exists(user_ns, &book_key, chapter_url).await
    }

    pub async fn chapter_list_cache_exists(&self, user_ns: &str, toc_url: &str) -> bool {
        let path = self.chapter_list_cache_path(user_ns, toc_url);
        path.exists()
    }

    pub async fn get_bookshelf(&self, user_ns: &str) -> Result<Vec<Book>, AppError> {
        self.read_bookshelf(user_ns).await
    }

    pub async fn get_shelf_book(
        &self,
        user_ns: &str,
        book_url: &str,
    ) -> Result<Option<Book>, AppError> {
        let list = self.read_bookshelf(user_ns).await?;
        Ok(list.into_iter().find(|b| b.book_url == book_url))
    }

    /// Find book by chapter URL (chapter URL typically shares domain with book URL)
    pub async fn get_shelf_book_by_chapter(
        &self,
        user_ns: &str,
        chapter_url: &str,
    ) -> Result<Option<Book>, AppError> {
        let list = self.read_bookshelf(user_ns).await?;

        // Extract domain from chapter_url
        let chapter_domain = url::Url::parse(chapter_url)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.to_string()));

        for book in list {
            // Check if chapter URL starts with book URL (common pattern)
            if chapter_url.starts_with(&book.book_url) {
                return Ok(Some(book));
            }

            // Check if they share the same domain
            if let (Some(ref ch_domain), Ok(book_url_parsed)) =
                (&chapter_domain, url::Url::parse(&book.book_url))
            {
                if let Some(book_domain) = book_url_parsed.host_str() {
                    if ch_domain == book_domain {
                        // Check if chapter URL path contains book URL path prefix
                        if let (Ok(ch_parsed), Ok(b_parsed)) = (
                            url::Url::parse(chapter_url),
                            url::Url::parse(&book.book_url),
                        ) {
                            let ch_path = ch_parsed.path();
                            let b_path = b_parsed.path();
                            // Check if paths share a common prefix (e.g., /biqu104/)
                            if ch_path.starts_with(b_path.trim_end_matches('/'))
                                || b_path
                                    .trim_end_matches('/')
                                    .starts_with(ch_path.trim_end_matches('/'))
                            {
                                return Ok(Some(book));
                            }
                        }
                    }
                }
            }
        }
        Ok(None)
    }

    /// Find book by name and author (for cases where book_url might differ)
    pub async fn find_shelf_book_by_name_author(
        &self,
        user_ns: &str,
        name: &str,
        author: &str,
    ) -> Result<Option<Book>, AppError> {
        let list = self.read_bookshelf(user_ns).await?;
        Ok(list
            .into_iter()
            .find(|b| b.name.trim() == name.trim() && b.author.trim() == author.trim()))
    }

    /// 就地更新一本书（按 bookUrl 精确匹配）：单次读-改-写整个书架文件。
    ///
    /// 进度上报这类高频更新用它避免 `get_shelf_book` + `save_book` 的两次
    /// 全量读取；`update` 里只改必要的字段，改动是否落盘由调用方先做
    /// 无变化判断（本方法总是写盘）。
    pub async fn update_shelf_book(
        &self,
        user_ns: &str,
        book_url: &str,
        update: impl FnOnce(&mut Book),
    ) -> Result<Option<Book>, AppError> {
        let mut list = self.read_bookshelf(user_ns).await?;
        let Some(book) = list.iter_mut().find(|b| b.book_url == book_url) else {
            return Ok(None);
        };
        update(book);
        let updated = book.clone();
        self.write_bookshelf(user_ns, &list).await?;
        Ok(Some(updated))
    }

    pub async fn save_book(&self, user_ns: &str, mut book: Book) -> Result<Book, AppError> {
        sanitize_book_urls(&mut book);
        if book.origin.trim().is_empty() {
            return Err(AppError::BadRequest("missing origin".to_string()));
        }
        if book.book_url.trim().is_empty() {
            return Err(AppError::BadRequest("bookUrl required".to_string()));
        }

        let mut list = self.read_bookshelf(user_ns).await?;
        let mut exist_idx: Option<usize> = None;
        for (i, b) in list.iter().enumerate() {
            if books_match_for_save(b, &book) {
                exist_idx = Some(i);
                break;
            }
        }

        if let Some(i) = exist_idx {
            let exist = list[i].clone();
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
            list[i] = book.clone();
        } else {
            // 书架上限只对「新入架」计数，已有书的更新不受限
            let limit = self.user_book_limit as usize;
            if limit > 0 && list.len() >= limit {
                return Err(AppError::BadRequest(format!(
                    "书架书籍数量不能超过 {limit} 本"
                )));
            }
            // 本地书限额同样对「新入架」生效：saveBook 直构 local-* 书
            // 可绕过上传 handler 里的检查
            let local_limit = self.user_local_book_limit as usize;
            if local_limit > 0 && is_local_book_url(&book.book_url) {
                let local_count = list
                    .iter()
                    .filter(|b| is_local_book_url(&b.book_url) && b.book_url != book.book_url)
                    .count();
                if local_count >= local_limit {
                    return Err(AppError::BadRequest(format!(
                        "本地书籍数量不能超过 {local_limit} 本"
                    )));
                }
            }
            list.push(book.clone());
        }

        self.write_bookshelf(user_ns, &list).await?;
        Ok(book)
    }

    pub async fn save_books(&self, user_ns: &str, books: Vec<Book>) -> Result<Vec<Book>, AppError> {
        let mut normalized = Vec::with_capacity(books.len());
        for mut book in books {
            sanitize_book_urls(&mut book);
            if book.origin.trim().is_empty() {
                return Err(AppError::BadRequest("missing origin".to_string()));
            }
            if book.book_url.trim().is_empty() {
                return Err(AppError::BadRequest("bookUrl required".to_string()));
            }
            normalized.push(book);
        }
        // save_books 是整架替换（导入场景），按替换后的总量校验上限
        let limit = self.user_book_limit as usize;
        if limit > 0 && normalized.len() > limit {
            return Err(AppError::BadRequest(format!(
                "书架书籍数量不能超过 {limit} 本"
            )));
        }
        // 本地书限额在整架替换后同样成立
        let local_limit = self.user_local_book_limit as usize;
        if local_limit > 0 {
            let local_count = normalized
                .iter()
                .filter(|b| is_local_book_url(&b.book_url))
                .count();
            if local_count > local_limit {
                return Err(AppError::BadRequest(format!(
                    "本地书籍数量不能超过 {local_limit} 本"
                )));
            }
        }
        self.write_bookshelf(user_ns, &normalized).await?;
        Ok(normalized)
    }

    pub async fn delete_book(&self, user_ns: &str, book: &Book) -> Result<bool, AppError> {
        let mut list = self.read_bookshelf(user_ns).await?;
        let orig_len = list.len();
        let removed: Vec<Book> = list
            .iter()
            .filter(|b| books_match_for_delete(b, book))
            .cloned()
            .collect();
        list.retain(|b| !books_match_for_delete(b, book));
        let deleted = list.len() != orig_len;
        if deleted {
            self.write_bookshelf(user_ns, &list).await?;
            for removed_book in &removed {
                let _ = self.clear_book_related_cache(user_ns, removed_book).await;
            }
        }
        Ok(deleted)
    }

    pub async fn delete_books(&self, user_ns: &str, books: Vec<Book>) -> Result<usize, AppError> {
        let mut list = self.read_bookshelf(user_ns).await?;
        let mut deleted = 0usize;
        let mut removed_books: Vec<Book> = Vec::new();
        for book in books {
            let matched: Vec<Book> = list
                .iter()
                .filter(|b| books_match_for_delete(b, &book))
                .cloned()
                .collect();
            removed_books.extend(matched);
            let before = list.len();
            list.retain(|b| !books_match_for_delete(b, &book));
            if list.len() != before {
                deleted += 1;
            }
        }
        if deleted > 0 {
            self.write_bookshelf(user_ns, &list).await?;
            for removed_book in &removed_books {
                let _ = self.clear_book_related_cache(user_ns, removed_book).await;
            }
        }
        Ok(deleted)
    }

    /// 某本书已缓存的章节数：数缓存目录里的文件即可。
    ///
    /// 此前实现按章节 URL 列表逐个做同步 `exists()`，书架页对每本书执行，
    /// 千章书在书多的场景下是数万次 stat；一次 `read_dir` 计数就够了。
    pub async fn cached_chapter_count(
        &self,
        user_ns: &str,
        book_url: &str,
    ) -> Result<usize, AppError> {
        Ok(self
            .cache
            .book_file_count(user_ns, &md5_hex(book_url))
            .await as usize)
    }

    pub async fn cache_chapter(
        &self,
        user_ns: &str,
        book_url: &str,
        source: &BookSource,
        chapter_url: &str,
        refresh: bool,
    ) -> Result<(), AppError> {
        let book_key = md5_hex(book_url);
        if refresh {
            let _ = self.cache.remove(user_ns, &book_key, chapter_url).await;
        }
        let _ = self
            .get_content(user_ns, book_url, source, chapter_url)
            .await?;
        Ok(())
    }

    pub async fn get_cover(&self, user_ns: &str, url: &str) -> Result<(Vec<u8>, String), AppError> {
        // 出站守卫：封面 URL 来自查询参数，且 `/cover` 允许匿名访问
        crate::crawler::url_guard::ensure_outbound_url_str_allowed(url)
            .await
            .map_err(AppError::BadRequest)?;
        let ext = file_ext_from_url(url).unwrap_or_else(|| "png".to_string());
        let name = md5_hex(url);
        let path = self
            .storage_dir
            .join("cache")
            .join(user_ns)
            .join("cover")
            .join(format!("{}.{}", name, ext));
        if path.exists() {
            let data = fs::read(&path)
                .await
                .map_err(|e| AppError::Internal(e.into()))?;
            let content_type = safe_cover_content_type(None, &ext);
            return Ok((data, content_type));
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| AppError::Internal(e.into()))?;
        }

        // Extract referer from URL for anti-hotlinking bypass
        let referer = url::Url::parse(url).ok().and_then(|u| {
            let scheme = u.scheme();
            let host = u.host_str()?;
            Some(format!("{}://{}", scheme, host))
        });

        // 封面属于匿名资源，固定用 "public" 命名空间，绝不携带任何用户的
        // 书源会话 Cookie
        let http = self.http.client_for("public").map_err(AppError::Internal)?;
        let mut req = http.get(url);

        // Add necessary headers to bypass anti-hotlinking
        req = req
            .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
            .header("Accept", "image/avif,image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8");

        if let Some(ref referer) = referer {
            req = req.header("Referer", referer);
        }

        let res = req.send().await.map_err(|e| AppError::Internal(e.into()))?;
        if !res.status().is_success() {
            return Err(AppError::NotFound("cover not found".to_string()));
        }
        let upstream_content_type = res
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let bytes = crate::crawler::fetcher::read_body_limited(
            res,
            crate::crawler::fetcher::MAX_RESPONSE_BYTES,
        )
        .await
        .map_err(AppError::Internal)?
        .to_vec();
        let _ = fs::write(&path, &bytes).await;
        // 匿名可写的缓存目录必须有容量上限，否则换 URL 即可打满磁盘
        if let Some(parent) = path.parent() {
            crate::storage::cache::file_cache::enforce_flat_dir_capacity(
                parent,
                self.cover_cache_limit,
                bytes.len() as u64,
            )
            .await;
        }
        let content_type = safe_cover_content_type(upstream_content_type.as_deref(), &ext);
        Ok((bytes, content_type))
    }

    pub async fn load_book_sources_cache(
        &self,
        user_ns: &str,
        book_url: &str,
    ) -> Result<Option<Vec<SearchBook>>, AppError> {
        let path = self.book_source_cache_path(user_ns, book_url);
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
        book_url: &str,
        list: &Vec<SearchBook>,
    ) -> Result<(), AppError> {
        let path = self.book_source_cache_path(user_ns, book_url);
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
        book_url: &str,
    ) -> Result<(), AppError> {
        let path = self.book_source_cache_path(user_ns, book_url);
        if path.exists() {
            fs::remove_file(&path)
                .await
                .map_err(|e| AppError::Internal(e.into()))?;
        }
        Ok(())
    }

    fn book_source_cache_path(&self, user_ns: &str, book_url: &str) -> PathBuf {
        let name = md5_hex(book_url);
        self.storage_dir
            .join("data")
            .join(user_ns)
            .join("book_sources")
            .join(format!("{}.json", name))
    }

    fn bookshelf_path(&self, user_ns: &str) -> PathBuf {
        self.storage_dir
            .join("data")
            .join(user_ns)
            .join("bookshelf.json")
    }

    async fn read_bookshelf(&self, user_ns: &str) -> Result<Vec<Book>, AppError> {
        let path = self.bookshelf_path(user_ns);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let data = fs::read_to_string(&path)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
        let mut list: Vec<Book> = match serde_json::from_str(&data) {
            Ok(list) => list,
            Err(primary_err) => {
                let recovered = recover_bookshelf_entries(&data)
                    .ok_or_else(|| AppError::BadRequest(primary_err.to_string()))?;
                tracing::warn!(
                    "recovered malformed bookshelf for user_ns={}, path={}, entries={}",
                    user_ns,
                    path.display(),
                    recovered.len()
                );
                self.write_bookshelf(user_ns, &recovered).await?;
                recovered
            }
        };
        for book in &mut list {
            sanitize_book_urls(book);
        }
        Ok(list)
    }

    async fn write_bookshelf(&self, user_ns: &str, list: &Vec<Book>) -> Result<(), AppError> {
        let path = self.bookshelf_path(user_ns);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| AppError::Internal(e.into()))?;
        }
        let data = serde_json::to_string(list).map_err(|e| AppError::BadRequest(e.to_string()))?;
        // 临时文件 + 同盘 rename 保证原子性：进程中途死掉不会留下截断的 JSON，
        // 读书架的一方也见不到半个文件（并发写仍是 last-writer-wins，
        // 但至少不会损坏文件本身）
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, data)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
        if let Err(e) = fs::rename(&tmp, &path).await {
            let _ = fs::remove_file(&tmp).await;
            return Err(AppError::Internal(e.into()));
        }
        Ok(())
    }

    // Chapter list cache methods
    fn chapter_list_cache_path(&self, user_ns: &str, toc_url: &str) -> PathBuf {
        let name = md5_hex(toc_url);
        self.storage_dir
            .join("data")
            .join(user_ns)
            .join("chapters")
            .join(format!("{}.json", name))
    }

    pub async fn load_chapter_list_cache(
        &self,
        user_ns: &str,
        toc_url: &str,
    ) -> Result<Option<Vec<BookChapter>>, AppError> {
        let path = self.chapter_list_cache_path(user_ns, toc_url);
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

    pub async fn save_chapter_list_cache(
        &self,
        user_ns: &str,
        toc_url: &str,
        chapters: &Vec<BookChapter>,
    ) -> Result<(), AppError> {
        let path = self.chapter_list_cache_path(user_ns, toc_url);
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

    /// 标记某本书的目录后台补全开始；已在进行中时返回 `false`（调用方跳过 spawn）。
    pub fn try_begin_toc_fill(&self, user_ns: &str, toc_url: &str) -> bool {
        self.pending_toc_fills
            .lock()
            .unwrap()
            .insert((user_ns.to_string(), toc_url.to_string()))
    }

    /// 标记目录后台补全结束（无论成败都必须调用）。
    pub fn end_toc_fill(&self, user_ns: &str, toc_url: &str) {
        self.pending_toc_fills
            .lock()
            .unwrap()
            .remove(&(user_ns.to_string(), toc_url.to_string()));
    }

    pub async fn append_chapter_list_cache(
        &self,
        user_ns: &str,
        toc_url: &str,
        new_chapters: &Vec<BookChapter>,
    ) -> Result<Vec<BookChapter>, AppError> {
        let mut existing = self
            .load_chapter_list_cache(user_ns, toc_url)
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
            self.save_chapter_list_cache(user_ns, toc_url, &existing)
                .await?;
        }
        Ok(existing)
    }

    pub async fn delete_chapter_list_cache(
        &self,
        user_ns: &str,
        toc_url: &str,
    ) -> Result<(), AppError> {
        let path = self.chapter_list_cache_path(user_ns, toc_url);
        if path.exists() {
            fs::remove_file(&path)
                .await
                .map_err(|e| AppError::Internal(e.into()))?;
        }
        Ok(())
    }

    async fn clear_book_related_cache(&self, user_ns: &str, book: &Book) -> Result<(), AppError> {
        if !book.book_url.is_empty() {
            let _ = self.delete_book_cache(user_ns, &book.book_url).await;
            let _ = self
                .delete_book_sources_cache(user_ns, &book.book_url)
                .await;
            let _ = self
                .delete_chapter_list_cache(user_ns, &book.book_url)
                .await;
        }
        if let Some(toc_url) = &book.toc_url {
            if !toc_url.is_empty() {
                let _ = self.delete_chapter_list_cache(user_ns, toc_url).await;
            }
        }
        Ok(())
    }
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
                    FetchResponse::from(next)
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

fn is_local_book(book: &Book) -> bool {
    matches!(book.origin.trim(), "local-txt" | "local-epub")
        || book.book_url.trim().starts_with("local-txt:")
        || book.book_url.trim().starts_with("local-epub:")
}

fn books_match_for_save(existing: &Book, incoming: &Book) -> bool {
    if existing.book_url == incoming.book_url {
        return true;
    }
    if is_local_book(existing) || is_local_book(incoming) {
        return false;
    }
    !existing.name.is_empty()
        && existing.name == incoming.name
        && existing.author == incoming.author
}

fn books_match_for_delete(existing: &Book, target: &Book) -> bool {
    if !target.book_url.is_empty() && existing.book_url == target.book_url {
        return true;
    }
    if is_local_book(existing) || is_local_book(target) {
        return false;
    }
    !target.name.is_empty()
        && !target.author.is_empty()
        && existing.name == target.name
        && existing.author == target.author
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

fn recover_bookshelf_entries(data: &str) -> Option<Vec<Book>> {
    let mut recovered = Vec::new();
    let mut seen = HashSet::new();
    let stream = serde_json::Deserializer::from_str(data).into_iter::<serde_json::Value>();

    for item in stream {
        let value = match item {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!("bookshelf recovery stream stopped: {}", err);
                break;
            }
        };
        match value {
            serde_json::Value::Array(items) => {
                for entry in items {
                    if let Ok(book) = serde_json::from_value::<Book>(entry) {
                        push_recovered_book(&mut recovered, &mut seen, book);
                    }
                }
            }
            serde_json::Value::Object(_) => {
                if let Ok(book) = serde_json::from_value::<Book>(value) {
                    push_recovered_book(&mut recovered, &mut seen, book);
                }
            }
            _ => {}
        }
    }

    if recovered.is_empty() {
        None
    } else {
        Some(recovered)
    }
}

fn push_recovered_book(recovered: &mut Vec<Book>, seen: &mut HashSet<String>, mut book: Book) {
    sanitize_book_urls(&mut book);
    let key = format!("{}::{}", book.book_url, book.origin);
    if seen.insert(key) {
        recovered.push(book);
    }
}

fn file_ext_from_url(url: &str) -> Option<String> {
    let url = url.split('?').next().unwrap_or(url);
    let url = url.split('#').next().unwrap_or(url);
    let pos = url.rfind('.')?;
    let ext = &url[pos + 1..];
    if ext.len() > 0 && ext.len() <= 8 {
        Some(ext.to_ascii_lowercase())
    } else {
        None
    }
}

fn content_type_from_ext(ext: &str) -> String {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// 封面缓存目录的容量上限。
const DEFAULT_COVER_CACHE_BYTES: u64 = 256 * 1024 * 1024;

/// 封面响应只允许安全的位图类型。
///
/// 上游 Content-Type 原样透传时，`text/html` / `image/svg+xml` 会让直接打开
/// `/cover` 链接的读者在站点源下执行脚本（同源 XSS，可偷 localStorage 的 token）。
/// SVG 作为矢量图含脚本能力，同样不放行；上游类型不可信时按扩展名兜底，
/// 扩展名也不安全就强制 application/octet-stream（浏览器仅下载不渲染）。
fn safe_cover_content_type(upstream: Option<&str>, ext: &str) -> String {
    const ALLOWED: &[&str] = &[
        "image/jpeg",
        "image/png",
        "image/webp",
        "image/gif",
        "image/avif",
        "image/bmp",
        "image/x-icon",
        "image/vnd.microsoft.icon",
    ];
    if let Some(ct) = upstream
        .and_then(|v| v.split(';').next())
        .map(|v| v.trim().to_ascii_lowercase())
    {
        if ALLOWED.contains(&ct.as_str()) {
            return ct;
        }
    }
    let by_ext = content_type_from_ext(ext);
    if ALLOWED.contains(&by_ext.as_str()) {
        by_ext
    } else {
        "application/octet-stream".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cover_content_type_is_whitelisted() {
        assert_eq!(
            safe_cover_content_type(Some("image/jpeg"), "png"),
            "image/jpeg"
        );
        assert_eq!(
            safe_cover_content_type(Some("image/png; charset=binary"), "x"),
            "image/png"
        );
        // html/svg 一律不放行（同源 XSS 通道）
        assert_eq!(
            safe_cover_content_type(Some("text/html"), "html"),
            "application/octet-stream"
        );
        assert_eq!(
            safe_cover_content_type(Some("image/svg+xml"), "svg"),
            "application/octet-stream"
        );
        assert_eq!(
            safe_cover_content_type(None, "svg"),
            "application/octet-stream"
        );
        // 上游类型不可信时按扩展名兜底
        assert_eq!(
            safe_cover_content_type(Some("text/plain"), "jpg"),
            "image/jpeg"
        );
        assert_eq!(safe_cover_content_type(None, "webp"), "image/webp");
    }

    #[tokio::test]
    async fn append_chapter_list_cache_dedups_by_url_and_keeps_index_sequential() {
        let storage_dir =
            std::env::temp_dir().join(format!("reader-rust-toc-dedup-{}", std::process::id()));
        let service = BookService::new(
            HttpClient::new(5, None).unwrap(),
            RuleEngine::new().unwrap(),
            FileCache::new(storage_dir.join("cache"), 0),
            storage_dir.to_str().unwrap(),
        );
        let ns = "test-dedup";
        let toc = "https://example.com/toc";

        let mk = |index: i32, url: &str| BookChapter {
            url: url.to_string(),
            title: url.to_string(),
            index,
            ..Default::default()
        };
        service
            .save_chapter_list_cache(ns, toc, &vec![mk(0, "a"), mk(1, "b")])
            .await
            .unwrap();
        // 补全结果与缓存尾部重叠（竞态重放），另含一个空 URL 章节
        let all = service
            .append_chapter_list_cache(ns, toc, &vec![mk(9, "b"), mk(9, "c"), mk(9, "")])
            .await
            .unwrap();
        let urls: Vec<&str> = all.iter().map(|c| c.url.as_str()).collect();
        assert_eq!(urls, vec!["a", "b", "c", ""]);
        let indexes: Vec<i32> = all.iter().map(|c| c.index).collect();
        assert_eq!(indexes, vec![0, 1, 2, 3]);
        // 再次追加完全重叠的结果：无新增时不重写缓存
        let again = service
            .append_chapter_list_cache(ns, toc, &vec![mk(9, "c")])
            .await
            .unwrap();
        assert_eq!(again.len(), 4);

        // 进行中守卫
        assert!(service.try_begin_toc_fill(ns, toc));
        assert!(
            !service.try_begin_toc_fill(ns, toc),
            "同一本书不允许并发补全"
        );
        service.end_toc_fill(ns, toc);
        assert!(service.try_begin_toc_fill(ns, toc), "结束后可再次开始");
        service.end_toc_fill(ns, toc);
        let _ = std::fs::remove_dir_all(&storage_dir);
    }

    #[tokio::test]
    async fn window_rate_waits_when_existing_starts_reach_limit() {
        let storage_dir =
            std::env::temp_dir().join(format!("reader-rust-window-rate-{}", std::process::id()));
        let service = BookService::new(
            HttpClient::new(5, None).unwrap(),
            RuleEngine::new().unwrap(),
            FileCache::new(storage_dir.join("cache"), 0),
            storage_dir.to_str().unwrap(),
        );
        let now = Instant::now();
        service.rate_states.write().await.insert(
            "source".to_string(),
            RateState {
                window_starts: vec![now, now],
                ..Default::default()
            },
        );

        let result = tokio::time::timeout(
            Duration::from_millis(20),
            service.wait_for_window_rate("source", 2, 200),
        )
        .await;

        let _ = tokio::fs::remove_dir_all(&storage_dir).await;
        assert!(result.is_err());
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
    async fn user_book_limit_blocks_new_books_but_not_updates() {
        let storage_dir =
            std::env::temp_dir().join(format!("reader-rust-book-limit-{}", std::process::id()));
        let service = BookService::new(
            HttpClient::new(5, None).unwrap(),
            RuleEngine::new().unwrap(),
            FileCache::new(storage_dir.join("cache"), 0),
            storage_dir.to_str().unwrap(),
        )
        .with_user_book_limit(2);

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
            .save_books("u", vec![make_book("x"), make_book("y")])
            .await
            .unwrap();
        let err = service
            .save_books("u", vec![make_book("x"), make_book("y"), make_book("z")])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("书架书籍数量不能超过"));

        // 0 = 不限
        let unlimited = BookService::new(
            HttpClient::new(5, None).unwrap(),
            RuleEngine::new().unwrap(),
            FileCache::new(storage_dir.join("cache2"), 0),
            storage_dir.to_str().unwrap(),
        );
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
        let service = BookService::new(
            HttpClient::new(5, None).unwrap(),
            RuleEngine::new().unwrap(),
            FileCache::new(storage_dir.join("cache"), 0),
            storage_dir.to_str().unwrap(),
        )
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
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("本地书籍数量不能超过"));

        let _ = tokio::fs::remove_dir_all(&storage_dir).await;
    }
}
