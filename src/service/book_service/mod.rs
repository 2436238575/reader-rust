use crate::crawler::{
    fetcher::{fetch, FetchResponse, RequestSpec, StrResponse},
    http_client::HttpClient,
    url_analyzer::analyze_url,
};
use crate::error::error::AppError;
use crate::model::{book_source::BookSource, search::SearchBook};
use crate::parser::js::{eval_js_with_bindings, with_book_source, with_user_ns};
use crate::parser::rule_engine::RuleEngine;
use crate::service::chapter_content_service::ChapterContentService;
use crate::storage::cache::review_cache::ReviewCache;
use crate::storage::db::repo::{BookRepo, RegisterBook, ShelfBookRepo};
use crate::util::bounded_map::BoundedMap;
use crate::util::keyed_mutex::KeyedMutex;
use crate::util::text::normalize_source_url;
use content::RecentBody;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use toc::TocSnapshotMemoEntry;
use tokio::sync::RwLock;
use tokio::time::{sleep, Duration, Instant};

mod cache;
mod cached_search;
mod content;
mod explore;
mod review;
mod shelf;
mod toc;

pub use cache::{CacheKind, CachePurgeResult};
pub use cached_search::{CachedSearchHit, CachedSearchOutcome};
pub use content::ChapterContentRead;
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

/// 备忘条目上限：同时批量缓存的书籍数有限，16 本足以覆盖且控住内存。
const TOC_SNAPSHOT_MEMO_MAX_ENTRIES: usize = 16;
/// 回源 singleflight 锁表上限：正常路径用完即清，上限只为兜住取消/panic
/// 遗留的条目。
const REFRESH_LOCK_MAX_ENTRIES: usize = 256;

/// 评论缓存的默认有效期（7 天）与单用户容量上限（64 MiB）。
const DEFAULT_REVIEW_CACHE_TTL_SECS: u64 = 7 * 24 * 60 * 60;
const DEFAULT_REVIEW_CACHE_BYTES: u64 = 64 * 1024 * 1024;

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
}

/// bookId 拼进缓存文件路径前的归一：别名（`book_007`/`book_+7`）归一为
/// canonical `book_<n>`（同一本书不该有两个缓存文件），非法 id 落隔离段——
/// 与缓存路径的 book_id 归一策略一致：读返回空、写进隔离区、删除是 no-op。
/// handler 层的 resolve_book 是主防线，这层兜底防未来新调用点绕开它。
fn book_id_path_segment(book_id: &str) -> String {
    crate::storage::db::repo::canonical_book_id(book_id)
        .unwrap_or_else(|| "__invalid_book__".to_string())
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
}
