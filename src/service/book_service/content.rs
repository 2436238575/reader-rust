//! 章节正文：抓取（含分页拼接）、正文池落库、到期回源与 singleflight。
//!
//! 读取路径是「缓存优先 + 到期后台回源」：`read_content` / `get_content`
//! 命中即返，`refresh_chapter_content` 供显式强刷；同一章的并发回源由
//! `refresh_locks` 键锁收口（等锁后二次检查新鲜缓存直接复用），后台回源
//! 由 `pending_revalidations` 去重。正文响应体的内存暂存（`recent_bodies`）
//! 供评论 URL 求值复用，省掉打开评论时的第二次抓取。分页拼接有页数与累计
//! 字节双上限，`should_follow_content_page` 挡住「下一页已是下一章」。

use super::BookService;
use crate::error::error::AppError;
use crate::model::book_source::BookSource;
use crate::service::chapter_content_service::{ChapterVersionView, UpstreamChapterMeta};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use tokio::time::{Duration, Instant};

/// 内存里暂存的章节正文响应体。
pub(super) struct RecentBody {
    body: String,
    url: String,
    stored_at: Instant,
}

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

/// 正文响应体内存暂存的条数、单条上限与存活时间。
const RECENT_BODY_LIMIT: usize = 32;
const RECENT_BODY_MAX_BYTES: usize = 1024 * 1024;
const RECENT_BODY_TTL: Duration = Duration::from_secs(600);

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

impl BookService {
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
    pub(super) async fn chapter_source_body(
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
