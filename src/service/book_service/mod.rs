use crate::crawler::{
    fetcher::{fetch, FetchResponse, RequestSpec, StrResponse},
    http_client::HttpClient,
    url_analyzer::analyze_url,
};
use crate::error::error::AppError;
use crate::model::{book_source::BookSource, search::SearchBook};
use crate::parser::js::{eval_js_with_bindings, with_book_source, with_user_ns};
use crate::parser::rule_engine::RuleEngine;
use crate::service::chapter_content_service::{
    ChapterContentService, ChapterVersionView, UpstreamChapterMeta,
};
use crate::storage::cache::review_cache::ReviewCache;
use crate::storage::db::content_repo::EffectiveVersionRow;
use crate::storage::db::repo::{BookRepo, RegisterBook, ShelfBookRepo};
use crate::util::bounded_map::BoundedMap;
use crate::util::keyed_mutex::KeyedMutex;
use crate::util::text::normalize_source_url;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use toc::TocSnapshotMemoEntry;
use tokio::sync::RwLock;
use tokio::time::{sleep, Duration, Instant};

mod cache;
mod explore;
mod review;
mod shelf;
mod toc;

pub use cache::{CacheKind, CachePurgeResult};
pub use explore::BookSourceAvailability;
pub use review::REVIEW_PAGE_SIZE;
pub use toc::{ChapterPagination, TocFillGuard};

#[derive(Clone)]
pub struct BookService {
    http: HttpClient,
    parser: RuleEngine,
    content: Arc<ChapterContentService>,
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
    /// 正在后台回源的 `(book_id, chapter_url)` 集合：到期后的并发请求只起一个任务。
    pending_revalidations: Arc<Mutex<HashSet<(String, String)>>>,
    /// 串行化目录缓存的读-改-写（append）：无锁交错会互相覆盖丢章。
    /// 全局一把锁即可——append 只在目录后台补全时发生，频率低、临界区短。
    toc_append_lock: Arc<tokio::sync::Mutex<()>>,
    /// 目录快照备忘（见 `toc_snapshot_map`）：批量落库复用一份 parse 结果。
    toc_snapshots: Arc<Mutex<BoundedMap<TocSnapshotMemoEntry>>>,
    /// 正文回源 singleflight：`(user_ns, book_id, chapter_url)` -> 按键互斥锁。
    /// 并发未命中/并发到期回源的同一章只有一个真正打上游，其余等锁后
    /// 二次检查缓存直接复用。KeyedMutex 限条数：取消/panic 遗留的条目
    /// 在持有者 drop 后变为空闲可被逐出，在用锁不会被挤掉
    /// （详见 `util::keyed_mutex`）。
    refresh_locks: Arc<KeyedMutex>,
}

/// 内存里暂存的章节正文响应体。
struct RecentBody {
    body: String,
    url: String,
    stored_at: Instant,
}

/// 备忘条目上限：同时批量缓存的书籍数有限，16 本足以覆盖且控住内存。
const TOC_SNAPSHOT_MEMO_MAX_ENTRIES: usize = 16;
/// 回源 singleflight 锁表上限：正常路径用完即清，上限只为兜住取消/panic
/// 遗留的条目。
const REFRESH_LOCK_MAX_ENTRIES: usize = 256;

/// 一次抓取得到的章节正文（分页已拼接）。
struct FetchedChapter {
    content: String,
    fetch_url: String,
}

/// 阅读器读取正文的结果：当前版本、指针状态与是否需要后台回源。
pub struct ChapterContentRead {
    pub view: ChapterVersionView,
    pub latest_upstream_version_id: Option<i64>,
    pub latest_user_version_id: Option<i64>,
    pub selected_version_id: Option<i64>,
    pub last_attempt_at: Option<i64>,
    pub last_success_at: Option<i64>,
    pub upstream_changed_at: Option<i64>,
    pub edited_at: Option<i64>,
    pub needs_revalidate: bool,
    pub stale: bool,
}

/// 写进 chapter_latest.last_error 的短原因，不带上游 URL。
/// 回源 singleflight 的键：三段都用 `\0` 分隔（URL 里不会出现 NUL）。
fn refresh_lock_key(user_ns: &str, book_id: &str, chapter_url: &str) -> String {
    format!("{user_ns}\u{0}{book_id}\u{0}{chapter_url}")
}

fn error_summary(error: &AppError) -> String {
    match error {
        AppError::Upstream { message, .. } => message.clone(),
        AppError::Blocked(message) => message.clone(),
        AppError::NotFound(message) => message.clone(),
        AppError::BadRequest(message) => message.clone(),
        AppError::Conflict(message) => message.clone(),
        AppError::Unauthorized(_) => "需要登录".to_string(),
        _ => "回源失败".to_string(),
    }
}

/// 评论缓存的默认有效期（7 天）与单用户容量上限（64 MiB）。
const DEFAULT_REVIEW_CACHE_TTL_SECS: u64 = 7 * 24 * 60 * 60;
const DEFAULT_REVIEW_CACHE_BYTES: u64 = 64 * 1024 * 1024;
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

/// 后台回源集合的 RAII 释放：任务正常结束、取消或 panic 都不会把 key 留死。
struct PendingRevalidation {
    set: Arc<Mutex<HashSet<(String, String)>>>,
    key: (String, String),
}

impl Drop for PendingRevalidation {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.set.lock() {
            pending.remove(&self.key);
        }
    }
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
        content: Arc<ChapterContentService>,
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
            content,
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
            pending_revalidations: Arc::new(Mutex::new(HashSet::new())),
            toc_append_lock: Arc::new(tokio::sync::Mutex::new(())),
            toc_snapshots: Arc::new(Mutex::new(BoundedMap::new(TOC_SNAPSHOT_MEMO_MAX_ENTRIES))),
            refresh_locks: Arc::new(KeyedMutex::new(REFRESH_LOCK_MAX_ENTRIES)),
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

    #[cfg(test)]
    pub fn content(&self) -> &ChapterContentService {
        &self.content
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

    /// 抓一章正文（含分页拼接），只抓取和解析，不落库。
    async fn fetch_chapter_content(
        &self,
        user_ns: &str,
        source: &BookSource,
        chapter_url: &str,
    ) -> Result<FetchedChapter, AppError> {
        let mut all_content = String::new();
        let mut visited_urls = std::collections::HashSet::new();
        let mut current_url = chapter_url.to_string();
        let mut first_fetch_url = String::new();

        // 页数上限：环检测只能挡住重复 URL，恶意/病态书源持续产新 URL 时
        // 单次取正文会发出无限请求。正常分页章节（2-5 页）远够不着上限。
        const MAX_CONTENT_PAGES: usize = 50;
        // 累计字节上限：页数上限 × 单页体积（正文规则输出无界）的理论峰值可
        // 达 1.6GB；大分页正文必须在累计维度同样封顶。正常章节远够不着上限。
        const MAX_ACCUMULATED_CONTENT_BYTES: usize = 8 * 1024 * 1024;
        loop {
            if visited_urls.len() >= MAX_CONTENT_PAGES {
                tracing::warn!(
                    "fetch_chapter_content hit MAX_CONTENT_PAGES ({MAX_CONTENT_PAGES}), truncating: {chapter_url}"
                );
                break;
            }
            if visited_urls.contains(&current_url) {
                tracing::debug!("fetch_chapter_content detected loop, breaking");
                break;
            }
            visited_urls.insert(current_url.clone());

            tracing::debug!("fetch_chapter_content fetching: {}", current_url);
            let res = self
                .fetch_source_url(user_ns, source, &current_url, &source.book_source_url)
                .await?;
            if first_fetch_url.is_empty() {
                first_fetch_url = res.url.clone();
            }
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

            if !content.is_empty() {
                if !all_content.is_empty() {
                    all_content.push('\n');
                }
                all_content.push_str(&content);
                // 累计超上限：停止追加并告警（保留已累计的完整页面，
                // 不做无告警的半页静默截断），翻页到此为止。
                if all_content.len() >= MAX_ACCUMULATED_CONTENT_BYTES {
                    tracing::warn!(
                        "fetch_chapter_content hit MAX_ACCUMULATED_CONTENT_BYTES \
                         ({MAX_ACCUMULATED_CONTENT_BYTES}), truncating: {chapter_url}"
                    );
                    break;
                }
            }

            if let Some(next_url) = next_url {
                if should_follow_content_page(chapter_url, &current_url, &next_url) {
                    current_url = next_url;
                } else {
                    tracing::debug!("fetch_chapter_content next_url 已是下一章，停止");
                    break;
                }
            } else {
                break;
            }
        }

        Ok(FetchedChapter {
            content: all_content,
            fetch_url: first_fetch_url,
        })
    }

    async fn store_upstream_content(
        &self,
        user_ns: &str,
        book_id: &str,
        source: &BookSource,
        chapter_url: &str,
        fetched: &FetchedChapter,
    ) -> Result<(), AppError> {
        let book_url = self
            .book_repo
            .get("", book_id)
            .await?
            .map(|row| row.book_url)
            .unwrap_or_default();
        let (chapter_index, chapter_title) =
            self.chapter_snapshot(user_ns, book_id, chapter_url).await;
        self.content
            .save_upstream(
                UpstreamChapterMeta {
                    book_id,
                    chapter_url,
                    book_source_id: source.book_source_id,
                    book_source_name: &source.book_source_name,
                    chapter_index,
                    chapter_title: &chapter_title,
                    book_url: &book_url,
                    fetch_url: &fetched.fetch_url,
                },
                &fetched.content,
            )
            .await?;
        Ok(())
    }

    /// 回源一次并把结果落库（singleflight 包装，见 `refresh_upstream_flight`）。
    /// 非强刷：拿到键锁后先做二次检查，等锁期间别人已抓好的新鲜缓存直接复用。
    async fn refresh_upstream(
        &self,
        user_ns: &str,
        book_id: &str,
        source: &BookSource,
        chapter_url: &str,
    ) -> Result<String, AppError> {
        self.refresh_upstream_flight(user_ns, book_id, source, chapter_url, false)
            .await
    }

    /// 强刷路径的回源：与普通回源共用同一把键锁（与在途请求互斥），
    /// 但不做二次检查——显式「检查更新」必须真的打一次上游。
    async fn refresh_upstream_forced(
        &self,
        user_ns: &str,
        book_id: &str,
        source: &BookSource,
        chapter_url: &str,
    ) -> Result<String, AppError> {
        self.refresh_upstream_flight(user_ns, book_id, source, chapter_url, true)
            .await
    }

    /// 按 `(user_ns, book_id, chapter_url)` 的回源 singleflight：
    /// 同一章的并发回源只有一个真正抓上游，其余等键锁；等到锁后
    /// （非强刷时）二次检查缓存，别人刚落库的新鲜内容直接返回。
    async fn refresh_upstream_flight(
        &self,
        user_ns: &str,
        book_id: &str,
        source: &BookSource,
        chapter_url: &str,
        force: bool,
    ) -> Result<String, AppError> {
        let key = refresh_lock_key(user_ns, book_id, chapter_url);
        let lock = self.acquire_refresh_lock(&key);
        let _key_guard = lock.lock().await;
        if !force {
            if let Some(cached) = self
                .fresh_cached_upstream(user_ns, book_id, chapter_url)
                .await?
            {
                tracing::debug!(book_id, chapter_url, "并发回源命中二次检查，复用新鲜缓存");
                drop(_key_guard);
                self.release_refresh_lock(&key, &lock);
                return Ok(cached);
            }
        }
        let result = self
            .fetch_upstream_now(user_ns, book_id, source, chapter_url)
            .await;
        drop(_key_guard);
        self.release_refresh_lock(&key, &lock);
        result
    }

    /// 二次检查用的「新鲜上游缓存」：有上游版本、未到回源间隔
    /// （TTL=0 视为永远新鲜）且能解析出正文。
    async fn fresh_cached_upstream(
        &self,
        user_ns: &str,
        book_id: &str,
        chapter_url: &str,
    ) -> Result<Option<String>, AppError> {
        let Some(latest) = self.content.latest(book_id, chapter_url).await? else {
            return Ok(None);
        };
        if latest.latest_upstream_version_id.is_none() {
            return Ok(None);
        }
        if self.content.needs_revalidate(&latest) {
            return Ok(None);
        }
        Ok(self
            .content
            .resolve(user_ns, book_id, chapter_url, true)
            .await?
            .map(|view| view.content))
    }

    /// 登记并返回 key 的键锁：已存在则复用（后来的等待者跟在队尾），
    /// 否则新建。
    fn acquire_refresh_lock(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.refresh_locks.acquire(key)
    }

    /// 用完清理：表里还是这把锁、且没有别的等待者持有克隆时摘掉条目，
    /// 避免逐章回源把表撑满（表有上限，这里只是正常路径的精确回收）。
    fn release_refresh_lock(&self, key: &str, lock: &Arc<tokio::sync::Mutex<()>>) {
        self.refresh_locks.release(key, lock);
    }

    /// 真正抓一次上游并落库。空正文在已有上游内容时按失败处理，避免把
    /// 一次规则失效或风控页面写成空版本。
    async fn fetch_upstream_now(
        &self,
        user_ns: &str,
        book_id: &str,
        source: &BookSource,
        chapter_url: &str,
    ) -> Result<String, AppError> {
        let fetched = self
            .fetch_chapter_content(user_ns, source, chapter_url)
            .await?;
        if fetched.content.is_empty() {
            // 只有图片源（bookSourceType=2）允许空正文。文本源即使声明了
            // 章节配图，空正文也说明规则失效或上游返回了空页面，不能写成
            // 0 字节的上游版本。没有 bookSourceType 的旧书源仍按「声明了
            // 配图规则就可能是图片源」兼容。
            let image_source = source.book_source_type == Some(2)
                || (source.book_source_type.is_none() && source.rule_content_image.is_some());
            if !image_source {
                return Err(AppError::NotFound(
                    "正文为空：书源规则可能已失效，或上游页面结构有变化".to_string(),
                ));
            }
            if let Some(cached) = self
                .content
                .resolve(user_ns, book_id, chapter_url, true)
                .await?
            {
                if !cached.content.is_empty() {
                    return Err(AppError::NotFound(
                        "正文为空：上游返回了空内容，保留已有正文".to_string(),
                    ));
                }
            }
            // 图片源的空正文是常态：只记检查时间，不往时间线塞 0 字节版本。
            self.content
                .touch_empty_upstream(book_id, chapter_url)
                .await?;
            return Ok(fetched.content);
        }
        self.store_upstream_content(user_ns, book_id, source, chapter_url, &fetched)
            .await?;
        Ok(fetched.content)
    }

    /// 显式回源（强刷 / 历史面板的「检查更新」）。失败时如果已有缓存，
    /// 把错误记到指针上并返回缓存正文；没有缓存才把错误抛给调用方。
    pub async fn refresh_chapter_content(
        &self,
        user_ns: &str,
        book_id: &str,
        source: &BookSource,
        chapter_url: &str,
    ) -> Result<String, AppError> {
        match self
            .refresh_upstream_forced(user_ns, book_id, source, chapter_url)
            .await
        {
            Ok(content) => Ok(content),
            Err(error) => {
                let has_cached = self
                    .content
                    .latest(book_id, chapter_url)
                    .await?
                    .and_then(|row| row.latest_upstream_version_id)
                    .is_some();
                if !has_cached {
                    return Err(error);
                }
                tracing::warn!(%error, book_id, chapter_url, "强刷失败，回退缓存正文");
                let summary = error_summary(&error);
                let _ = self
                    .content
                    .mark_failure(book_id, chapter_url, &summary)
                    .await;
                self.content
                    .resolve(user_ns, book_id, chapter_url, true)
                    .await?
                    .map(|view| view.content)
                    .ok_or(error)
            }
        }
    }

    /// 后台回源：失败只记录原因，不影响已经返回给用户的缓存正文。
    pub async fn revalidate_chapter(
        &self,
        user_ns: &str,
        book_id: &str,
        source: &BookSource,
        chapter_url: &str,
    ) {
        // 到期后的并发请求只起一个后台任务；任务结束（成功或失败）后释放。
        let key = (book_id.to_string(), chapter_url.to_string());
        {
            let mut pending = self.pending_revalidations.lock().unwrap();
            if !pending.insert(key.clone()) {
                return;
            }
        }
        let _guard = PendingRevalidation {
            set: self.pending_revalidations.clone(),
            key,
        };
        if let Err(error) = self
            .refresh_upstream(user_ns, book_id, source, chapter_url)
            .await
        {
            tracing::warn!(%error, book_id, chapter_url, "后台回源失败，继续使用缓存正文");
            let summary = error_summary(&error);
            let _ = self
                .content
                .mark_failure(book_id, chapter_url, &summary)
                .await;
        }
    }

    /// 阅读器读取正文：有缓存立即返回，并把「是否该后台回源」告诉调用方。
    pub async fn read_content(
        &self,
        user_ns: &str,
        book_id: &str,
        source: &BookSource,
        chapter_url: &str,
        force_upstream: bool,
    ) -> Result<ChapterContentRead, AppError> {
        let latest = self.content.latest(book_id, chapter_url).await?;
        if let Some(latest) = &latest {
            if let Some(view) = self
                .content
                .resolve(user_ns, book_id, chapter_url, force_upstream)
                .await?
            {
                return Ok(ChapterContentRead {
                    view,
                    latest_upstream_version_id: latest.latest_upstream_version_id,
                    latest_user_version_id: latest.latest_user_version_id,
                    selected_version_id: self
                        .content
                        .selected_version_id(user_ns, book_id, chapter_url)
                        .await?,
                    last_attempt_at: latest.last_attempt_at,
                    last_success_at: latest.last_success_at,
                    upstream_changed_at: latest.upstream_changed_at,
                    edited_at: latest.edited_at,
                    needs_revalidate: self.content.needs_revalidate(latest),
                    stale: self.content.is_stale(latest),
                });
            }
            // 图片源的空正文只有检查时间，没有版本指针；在回源间隔内直接
            // 返回空正文，避免每次翻章都重新抓上游。
            if latest.latest_upstream_version_id.is_none()
                && latest.latest_user_version_id.is_none()
                && latest.last_success_at.is_some()
                && !self.content.needs_revalidate(latest)
            {
                return Ok(ChapterContentRead {
                    view: ChapterVersionView {
                        version_id: 0,
                        kind: "upstream".to_string(),
                        base_version_id: None,
                        content: String::new(),
                        created_at: latest
                            .last_success_at
                            .unwrap_or_else(crate::util::time::now_millis),
                        book_source_name: source.book_source_name.clone(),
                        chapter_index: 0,
                        chapter_title: String::new(),
                        note: String::new(),
                        raw_size: 0,
                    },
                    latest_upstream_version_id: None,
                    latest_user_version_id: None,
                    selected_version_id: None,
                    last_attempt_at: latest.last_attempt_at,
                    last_success_at: latest.last_success_at,
                    upstream_changed_at: latest.upstream_changed_at,
                    edited_at: latest.edited_at,
                    needs_revalidate: false,
                    stale: false,
                });
            }
        }

        let content = self
            .refresh_upstream(user_ns, book_id, source, chapter_url)
            .await?;
        let fresh = self.content.latest(book_id, chapter_url).await?;
        let view = self
            .content
            .resolve(user_ns, book_id, chapter_url, force_upstream)
            .await?
            .unwrap_or(ChapterVersionView {
                version_id: 0,
                kind: "upstream".to_string(),
                base_version_id: None,
                content,
                created_at: crate::util::time::now_millis(),
                book_source_name: source.book_source_name.clone(),
                chapter_index: 0,
                chapter_title: String::new(),
                note: String::new(),
                raw_size: 0,
            });
        Ok(ChapterContentRead {
            view,
            latest_upstream_version_id: fresh
                .as_ref()
                .and_then(|row| row.latest_upstream_version_id),
            latest_user_version_id: fresh.as_ref().and_then(|row| row.latest_user_version_id),
            selected_version_id: self
                .content
                .selected_version_id(user_ns, book_id, chapter_url)
                .await?,
            last_attempt_at: fresh.as_ref().and_then(|row| row.last_attempt_at),
            last_success_at: fresh.as_ref().and_then(|row| row.last_success_at),
            upstream_changed_at: fresh.as_ref().and_then(|row| row.upstream_changed_at),
            edited_at: fresh.as_ref().and_then(|row| row.edited_at),
            needs_revalidate: false,
            stale: false,
        })
    }

    /// 上游正文（阅读器不显示编辑版时的内部路径、批量缓存、AI 资料）。
    ///
    /// 缓存到期时不再同步重抓：先返回过期缓存并 spawn 后台回源（与
    /// read_content 的 handlers 模式一致）。批量缓存一本全过期的书
    /// 不会因此全量重下。只有真未命中才同步回源（singleflight 去重）。
    pub async fn get_content(
        &self,
        user_ns: &str,
        book_id: &str,
        source: &BookSource,
        chapter_url: &str,
    ) -> Result<String, AppError> {
        let latest = self.content.latest(book_id, chapter_url).await?;
        let has_cached = latest
            .as_ref()
            .and_then(|row| row.latest_upstream_version_id)
            .is_some();
        if let Some(latest) = &latest {
            if has_cached {
                if let Some(view) = self
                    .content
                    .resolve(user_ns, book_id, chapter_url, true)
                    .await?
                {
                    if self.content.needs_revalidate(latest) {
                        // 到期：先返回缓存，后台回源；pending_revalidations +
                        // refresh 键锁保证并发只抓一次。
                        let service = self.clone();
                        let user_ns = user_ns.to_string();
                        let book_id = book_id.to_string();
                        let source = source.clone();
                        let chapter_url = chapter_url.to_string();
                        tokio::spawn(async move {
                            service
                                .revalidate_chapter(&user_ns, &book_id, &source, &chapter_url)
                                .await;
                        });
                    }
                    return Ok(view.content);
                }
            }
        }
        match self
            .refresh_upstream(user_ns, book_id, source, chapter_url)
            .await
        {
            Ok(content) => Ok(content),
            Err(error) => {
                if has_cached {
                    let summary = error_summary(&error);
                    let _ = self
                        .content
                        .mark_failure(book_id, chapter_url, &summary)
                        .await;
                    if let Some(view) = self
                        .content
                        .resolve(user_ns, book_id, chapter_url, true)
                        .await?
                    {
                        return Ok(view.content);
                    }
                }
                Err(error)
            }
        }
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

    /// 缓存内全文检索：只扫已缓存章节的正文文件（可再生数据），不触发任何
    /// 上游请求。本地书不走这套缓存，自然不在检索范围。`query` 是纯文本
    /// 子串匹配（大小写不敏感），不做正则语法——用户输错正则不该炸掉搜索。
    ///
    /// 资源预算：最多扫 5000 章 / 50MB / 10 秒，超限 truncated 截断并如实
    /// 上报覆盖度。章节文件按剩余预算限量读入（单章几十 MB 不会先把内存
    /// 翻倍再判超限）。已编辑章节读编辑版（与阅读器显示一致），命中带
    /// `edited` 标记。同时最多 2 个检索在跑（每个的读入上限都是 50MB）。
    pub async fn search_cached_content(
        &self,
        user_ns: &str,
        book_id: &str,
        query: &str,
        max_results: usize,
    ) -> Result<CachedSearchOutcome, AppError> {
        const MAX_SCAN_CHAPTERS: usize = 5000;
        const MAX_SCAN_BYTES: u64 = 50 * 1024 * 1024;
        const MAX_SCAN_DURATION: std::time::Duration = std::time::Duration::from_secs(10);
        /// 单批送进 blocking 线程的章节数：剥标签 + 正则是纯 CPU，
        /// 逐章在 async worker 上跑会饿死同 worker 上的其他任务。
        const SCAN_BLOCKING_BATCH: usize = 100;

        let _permit = CACHED_SEARCH_PERMITS
            .acquire()
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("检索并发闸门已关闭: {e}")))?;

        // 目录用来报「全书可检索章节总数」；正文全部来自正文池里当前有效的版本。
        let chapters = self
            .load_chapter_list_cache(user_ns, book_id)
            .await?
            .unwrap_or_default();
        let total_in_book = chapters
            .iter()
            .filter(|chapter| !chapter.is_volume && !chapter.url.is_empty())
            .count();
        let versions = self
            .content
            .list_effective_versions(user_ns, book_id)
            .await?;
        let total_cached = versions.len();
        let total_for_coverage = if total_in_book > 0 {
            total_in_book
        } else {
            total_cached
        };
        // 章节数预算：超出部分按截断处理（coverage 的 total 仍报真实总数）
        let scan_list: Vec<EffectiveVersionRow> =
            versions.into_iter().take(MAX_SCAN_CHAPTERS).collect();
        let re = Arc::new(
            regex::Regex::new(&format!("(?i){}", regex::escape(query)))
                .map_err(|e| AppError::BadRequest(format!("搜索词不可用: {e}")))?,
        );
        // 目录元数据改 owned：blocking 任务闭包要求 'static
        let chapter_meta: HashMap<String, (i32, String)> = chapters
            .iter()
            .map(|chapter| (chapter.url.clone(), (chapter.index, chapter.title.clone())))
            .collect();
        let chapter_meta = Arc::new(chapter_meta);

        let mut outcome = CachedSearchOutcome {
            results: Vec::new(),
            cached_chapters: 0,
            total_chapters: total_for_coverage,
            truncated: total_for_coverage > MAX_SCAN_CHAPTERS || total_cached > MAX_SCAN_CHAPTERS,
        };
        let started = std::time::Instant::now();
        let mut scanned_bytes = 0u64;
        // 已送进加载循环的章节数（含加载失败的）：给批内章节标全局位置
        let mut scanned_offset = 0usize;
        // 逐批推进：async 侧做预算检查与正文加载，CPU 段（剥标签 + 正则 +
        // 摘要）整批移交 blocking 线程，结果按章顺序原样接回。
        for batch in scan_list.chunks(SCAN_BLOCKING_BATCH) {
            // 耗时预算：超时按已扫部分截断返回（批间检查，批粒度足够——
            // 预算本身是 10s 量级的软上限，且字节预算把单批 CPU 量封了顶）
            if started.elapsed() > MAX_SCAN_DURATION {
                outcome.truncated = true;
                break;
            }
            // 预算淘汰与正文加载留在 async 侧（DB 读是异步的）
            let mut loaded: Vec<(usize, EffectiveVersionRow, String)> =
                Vec::with_capacity(batch.len());
            let mut budget_exhausted = false;
            for (position, version) in batch.iter().enumerate() {
                // 字节预算：循环起点上后面一定还有未扫章节，剩余为 0 即截断
                let remaining = MAX_SCAN_BYTES.saturating_sub(scanned_bytes);
                if remaining == 0 {
                    outcome.truncated = true;
                    budget_exhausted = true;
                    break;
                }
                // 半截内容不搜：截断点附近的命中会丢，如实报截断
                if version.raw_size as u64 > remaining {
                    outcome.truncated = true;
                    budget_exhausted = true;
                    break;
                }
                let Ok(body) = self.content.load_content(version.content_id).await else {
                    continue;
                };
                outcome.cached_chapters += 1;
                scanned_bytes += body.len() as u64;
                // 记录章在扫描清单里的全局位置：命中限额的截断判定要用
                let global_position = scanned_offset + position;
                loaded.push((global_position, version.clone(), body));
            }
            scanned_offset += batch.len();
            if budget_exhausted {
                break;
            }
            if loaded.is_empty() {
                continue;
            }
            let hit_budget = max_results.saturating_sub(outcome.results.len());
            let loaded_count = loaded.len();
            let re = Arc::clone(&re);
            let meta = Arc::clone(&chapter_meta);
            let batch_outcome = tokio::task::spawn_blocking(move || {
                scan_content_batch(loaded, re, meta, hit_budget)
            })
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("缓存检索任务失败: {e}")))?;
            outcome.results.extend(batch_outcome.hits);
            if let Some(position) = batch_outcome.limit_position {
                // 命中上限中断：分母用全书可检索章节总数，不是截断后的扫描
                // 清单长度——5000 章上限之外还可能有缓存章节
                outcome.truncated = hit_limit_leaves_more(position, total_for_coverage);
                // 批内加载了但没来得及检索的章节不计入 cached_chapters，
                // 保持逐章实现「实际参与检索」的口径
                outcome.cached_chapters -= loaded_count - batch_outcome.examined;
                break;
            }
        }
        Ok(outcome)
    }
}

/// 单章的命中摘要数上限：一章命中太多时前三条足够判断要不要跳过去。
const MAX_SNIPPETS_PER_CHAPTER: usize = 3;

/// bookId 拼进缓存文件路径前的归一：别名（`book_007`/`book_+7`）归一为
/// canonical `book_<n>`（同一本书不该有两个缓存文件），非法 id 落隔离段——
/// 与缓存路径的 book_id 归一策略一致：读返回空、写进隔离区、删除是 no-op。
/// handler 层的 resolve_book 是主防线，这层兜底防未来新调用点绕开它。
fn book_id_path_segment(book_id: &str) -> String {
    crate::storage::db::repo::canonical_book_id(book_id)
        .unwrap_or_else(|| "__invalid_book__".to_string())
}

/// 命中数达到上限而中断时，是否还有未扫的章节。
/// 分母必须是全书可检索章节总数（`total_in_book`），而不是按 5000 章上限
/// 裁剪后的扫描清单长度：全书超过上限时，最后一次命中可能恰好落在扫描
/// 清单的末尾，但上限之外还有章节没看过。
fn hit_limit_leaves_more(position: usize, total_in_book: usize) -> bool {
    position + 1 < total_in_book
}

/// 一批已加载正文的 CPU 检索段（剥标签 + 正则命中 + 摘要构建），
/// 由 `search_cached_content` 整批移交 blocking 线程执行。
///
/// 命中按章顺序追加；`limit_position` 是命中限额停下的章在扫描清单里的
/// 全局位置（未达限额为 None），`examined` 是本批实际过了一遍的章节数
/// （供调用方把「加载了但没来得及检索」的章节从 cached_chapters 里扣掉）。
struct BatchScanOutcome {
    hits: Vec<CachedSearchHit>,
    limit_position: Option<usize>,
    examined: usize,
}

fn scan_content_batch(
    batch: Vec<(usize, EffectiveVersionRow, String)>,
    re: Arc<regex::Regex>,
    chapter_meta: Arc<HashMap<String, (i32, String)>>,
    hit_budget: usize,
) -> BatchScanOutcome {
    let mut hits = Vec::new();
    let total = batch.len();
    for (i, (position, version, body)) in batch.into_iter().enumerate() {
        // 正文可能是 HTML：剥掉标签再匹配，摘要才不带尖括号
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
        // 目录是章节标题/序号的最新快照；版本里的快照可能停在抓取那一刻。
        let (chapter_index, chapter_title) = chapter_meta
            .get(version.chapter_url.as_str())
            .cloned()
            .unwrap_or_else(|| (version.chapter_index, version.chapter_title.clone()));
        hits.push(CachedSearchHit {
            chapter_index,
            chapter_title,
            chapter_url: version.chapter_url.clone(),
            snippets,
            edited: version.kind != "upstream",
        });
        if hits.len() >= hit_budget {
            return BatchScanOutcome {
                hits,
                limit_position: Some(position),
                examined: i + 1,
            };
        }
    }
    BatchScanOutcome {
        hits,
        limit_position: None,
        examined: total,
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedSearchHit {
    pub chapter_index: i32,
    pub chapter_title: String,
    pub chapter_url: String,
    pub snippets: Vec<String>,
    /// 命中内容来自编辑版（而非上游缓存）：前端在条目上标「已编辑」
    pub edited: bool,
}

/// 全书缓存检索的并发闸门：一次检索读入上限 50MB，无闸门时并发请求
/// 线性放大内存占用。单用户场景 2 个并发足够（阅读器搜索 + 重试）。
static CACHED_SEARCH_PERMITS: once_cell::sync::Lazy<tokio::sync::Semaphore> =
    once_cell::sync::Lazy::new(|| tokio::sync::Semaphore::new(2));

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

/// 请求规格的脱敏摘要：只保留 method、URL 的 host、header 键名与 body 长度。
///
/// header 值（Cookie、Authorization 等）与 URL 的 query/path（可能带 token、
/// 签名参数）绝不进日志——与 fetcher 里「绝不要打印完整 headers」的约定一致。
fn request_spec_summary(spec: &RequestSpec) -> String {
    let host = url::Url::parse(&spec.url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_string))
        .unwrap_or_else(|| "<unparsable-url>".to_string());
    let header_keys: Vec<&str> = spec.headers.iter().map(|(k, _)| k.as_str()).collect();
    format!(
        "method={:?} host={} header_keys=[{}] body_len={}",
        spec.method,
        host,
        header_keys.join(","),
        spec.body.as_ref().map(String::len).unwrap_or(0)
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::book::Book;
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
    fn request_spec_summary_never_leaks_header_values_or_query() {
        let spec = RequestSpec {
            url: "https://example.com/search?token=sekret&q=kw".into(),
            method: crate::crawler::fetcher::HttpMethod::POST,
            headers: vec![
                ("Cookie".into(), "session=super-secret".into()),
                ("User-Agent".into(), "Mozilla/5.0".into()),
            ],
            body: Some("username=a&password=b".into()),
            ..Default::default()
        };
        let summary = request_spec_summary(&spec);
        // 只允许出现 host、method、header 键名与 body 长度
        assert!(summary.contains("host=example.com"), "{summary}");
        assert!(summary.contains("method=POST"), "{summary}");
        assert!(summary.contains("Cookie"), "{summary}");
        assert!(summary.contains("body_len=21"), "{summary}");
        // 值与 query 绝不进日志
        assert!(!summary.contains("sekret"), "{summary}");
        assert!(!summary.contains("super-secret"), "{summary}");
        assert!(!summary.contains("username=a"), "{summary}");
        assert!(!summary.contains("token"), "{summary}");
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

    /// 返回池句柄：收尾必须先 `pool.close().await` 再删目录（Windows 文件锁，
    /// 见 repo.rs 测试约定）。
    pub(super) async fn test_service(
        storage_dir: &std::path::Path,
    ) -> (BookService, sqlx::SqlitePool) {
        crate::util::test_temp::clean_stale_test_dirs();
        // init_pool 不会建父目录，先落目录再开库
        std::fs::create_dir_all(storage_dir).unwrap();
        let pool = db::init_pool(&format!(
            "sqlite:{}?mode=rwc",
            storage_dir.join("reader.db").display()
        ))
        .await
        .unwrap();
        let content = Arc::new(ChapterContentService::new(
            crate::storage::db::content_repo::ChapterContentRepo::new(pool.clone()),
            0,
        ));
        let service = BookService::new(
            HttpClient::new(5, None).unwrap(),
            RuleEngine::new().unwrap(),
            content,
            storage_dir.to_str().unwrap(),
            BookRepo::new(pool.clone()),
            ShelfBookRepo::new(pool.clone()),
        );
        (service, pool)
    }

    #[tokio::test]
    async fn window_rate_waits_when_existing_starts_reach_limit() {
        let storage_dir =
            std::env::temp_dir().join(format!("reader-rust-window-rate-{}", std::process::id()));
        let (service, pool) = test_service(&storage_dir).await;
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

        pool.close().await;
        let _ = tokio::fs::remove_dir_all(&storage_dir).await;
        assert!(result.is_ok(), "窗口滑出后应放行而非卡住");
        assert!(
            began.elapsed() >= Duration::from_millis(180),
            "应等到窗口滑出（200ms）才放行，实际 {:?}",
            began.elapsed()
        );
    }

    pub(super) fn make_book(url: &str) -> Book {
        Book {
            book_url: url.to_string(),
            origin: "local".to_string(),
            name: format!("书 {url}"),
            ..Default::default()
        }
    }

    /// 命中上限中断的截断判定：分母是全书章节总数，不是 5000 章扫描上限
    /// 裁剪后的长度。全书 5001 章、最后一章命中时仍要报截断（旧实现会
    /// 把初始的 truncated=true 覆盖成 false）。
    #[test]
    fn hit_limit_boundary_uses_the_real_book_total() {
        assert!(super::hit_limit_leaves_more(4999, 5001), "上限之外还有章节");
        assert!(
            !super::hit_limit_leaves_more(4999, 5000),
            "恰好全书最后一章不应误报"
        );
        assert!(!super::hit_limit_leaves_more(2, 3));
        assert!(super::hit_limit_leaves_more(0, 2));
    }
}
