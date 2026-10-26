//! 章节列表（目录）：分页抓取、文件缓存、后台补全与目录快照备忘。
//!
//! 目录缓存是 JSON 文件（`<storage>/cache/<ns>/toc/<bookId>.json`）加旁路
//! meta（`<bookId>.meta.json` 的 complete 标志）：完整目录 1 小时 TTL，
//! 首屏分页的未完成目录只给 60 秒短 TTL。后台补全由 `TocFillGuard` 防并发、
//! `toc_append_lock` 串行化读-改-写；`toc_snapshots` 备忘给批量落库复用
//! 解析结果。分页抓取有 MAX_TOC_PAGES 上限与整页重复熔断，防失控书源
//! 把翻页变成无限请求。

use super::{book_id_path_segment, BookService};
use crate::error::error::AppError;
use crate::model::book_chapter::BookChapter;
use crate::model::book_source::BookSource;
use crate::util::atomic_write::write_atomic;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::fs;
use tokio::time::{Duration, Instant};

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

/// `toc_snapshots` 的条目：构建时间 + 不可变的目录映射。
pub(super) struct TocSnapshotMemoEntry {
    stored_at: Instant,
    map: Arc<HashMap<String, (i32, String)>>,
}

/// 目录快照备忘的兜底 TTL：目录写路径会主动失效条目，这里只防长尾驻留。
const TOC_SNAPSHOT_MEMO_TTL: Duration = Duration::from_secs(5);

/// 目录分页总页数上限：单链分支跟随书源给的 next 链接，恶意/失控书源
/// 每次回一个新 URL 就会变成无限请求。500 页按每页几十章估算覆盖数万章，
/// 大书足够；达到上限停止翻页并保留已抓章节（与正文 MAX_CONTENT_PAGES
/// 同一思路）。
const MAX_TOC_PAGES: usize = 500;

/// 章节列表缓存的存活时间（1 小时）。
///
/// 目录文件本体不带时间戳，过期判定用文件 mtime；只作用于「把缓存直接
/// 喂给响应」的读路径（过期按未命中处理，重抓后整份覆盖写回）。
/// 供反向查找、后台补全使用的 `load_chapter_list_cache` 不做 TTL——
/// 那些路径宁可读到旧目录，也不该顺手删掉还能用的文件。
const CHAPTER_LIST_CACHE_TTL: Duration = Duration::from_secs(3600);

/// 未完成目录缓存（首屏只落了第一页、后台补全尚未成功）的存活时间（60 秒）。
///
/// 首屏先落盘是为了让紧随其后的请求（翻目录、读者点开第一章）立即命中，
/// 但后台补全失败时这份缓存只有第一页——按完整 TTL（1 小时）复用会让
/// 「残缺目录」被当成全书目录服务一小时。未完成缓存只复用一小段时间，
/// 过期后调用方重抓首屏并重新拉起补全；补全成功由 append 把 meta 置为
/// 完成，之后按 `CHAPTER_LIST_CACHE_TTL` 走。
const INCOMPLETE_TOC_CACHE_TTL: Duration = Duration::from_secs(60);

/// 目录缓存的旁路元数据（`<bookId>.meta.json`，与目录文件同目录）。
///
/// `complete` 标记目录是否覆盖全部分页：`saved_at` 是落盘时的 Unix 秒，
/// 仅供排查参考（TTL 判定仍用目录文件 mtime，与升级前口径一致）。
#[derive(serde::Serialize, serde::Deserialize)]
struct ChapterListCacheMeta {
    complete: bool,
    saved_at: i64,
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

impl BookService {
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
        let (chapters, _, complete) = self
            .get_chapter_list_with_pagination(user_ns, source, toc_url)
            .await?;
        // 分页截断（MAX_TOC_PAGES 上限 / 整页重复熔断）与首屏分页一样按未完成
        // 落盘：读侧只给 60s 短 TTL，过期后重抓即可补全，截断目录不当完整目录
        // 缓存一小时。
        let _ = self
            .save_chapter_list_cache_with_state(user_ns, book_id, &chapters, complete)
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
    ///
    /// 总页数受 `MAX_TOC_PAGES` 限制：单链分支是「跟着书源给的下一页走」的
    /// 循环，环检测只能挡住重复 URL，恶意/失控书源每次回一个新 URL 就会把
    /// 抓取变成无限请求。达到上限记 warn 并停止翻页，已抓到的章节照常返回。
    ///
    /// 返回的 `complete` 表示目录是否抓全：达到 `MAX_TOC_PAGES` 或整页重复
    /// 熔断都算截断（false），单链自然结束、多分支全部处理完为 true。调用方
    /// 据此把缓存落成完成/未完成（未完成只给短 TTL，60 秒后可重试）。
    async fn fetch_paginated_chapters(
        &self,
        user_ns: &str,
        source: &BookSource,
        pending_urls: Vec<String>,
        visited_page_urls: &mut std::collections::HashSet<String>,
        seen_chapter_urls: &mut std::collections::HashSet<String>,
        chapter_index: &mut i32,
    ) -> Result<(Vec<BookChapter>, bool), AppError> {
        let mut all_chapters = Vec::new();
        let mut complete = true;
        let pending_urls: Vec<String> = pending_urls
            .into_iter()
            .filter(|u| !visited_page_urls.contains(u))
            .collect();
        let mut pages_fetched = 0usize;

        /// 页内公共动作：整页重复熔断 + 逐章去重追加。
        /// 整页重复即熔断（可能意味着书源把多个分页解析到了同一页）：单链
        /// 路径 `$on_dup` 是 break、多分支路径是 continue，两条路径都把
        /// `complete` 置 false——目录可能只抓到了一部分。
        macro_rules! absorb_page {
            ($chapters:expr, $page_url:expr, $on_dup:expr) => {{
                let all_seen = $chapters
                    .iter()
                    .all(|ch| seen_chapter_urls.contains(&ch.url));
                if all_seen && !$chapters.is_empty() {
                    tracing::debug!("Skipping duplicate page: {}", $page_url);
                    complete = false;
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
                if pages_fetched >= MAX_TOC_PAGES {
                    tracing::warn!(
                        "目录分页达到 {MAX_TOC_PAGES} 页上限，停止翻页（书源 {} 可能失控）",
                        source.book_source_url
                    );
                    complete = false;
                    break;
                }
                if visited_page_urls.contains(&url) {
                    continue;
                }
                visited_page_urls.insert(url.clone());
                pages_fetched += 1;

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
                if pages_fetched >= MAX_TOC_PAGES {
                    tracing::warn!(
                        "目录分页达到 {MAX_TOC_PAGES} 页上限，停止翻页（书源 {} 可能失控）",
                        source.book_source_url
                    );
                    complete = false;
                    break;
                }
                if visited_page_urls.contains(&current_url) {
                    break;
                }
                visited_page_urls.insert(current_url.clone());
                pages_fetched += 1;

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

        Ok((all_chapters, complete))
    }

    /// Continue fetching remaining chapters from pagination state
    ///
    /// 返回 `(章节, complete)`：`complete` 见 `fetch_paginated_chapters`，
    /// 后台补全据此决定把缓存落成完成还是未完成。
    pub async fn fetch_remaining_chapters(
        &self,
        pagination: ChapterPagination,
    ) -> Result<(Vec<BookChapter>, bool), AppError> {
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
    ) -> Result<(Vec<BookChapter>, Vec<String>, bool), AppError> {
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
        let (mut rest, complete) = self
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

        Ok((
            all_chapters,
            visited_page_urls.into_iter().collect(),
            complete,
        ))
    }

    /// 章节快照（章号/标题）尽力从目录缓存取；取不到就留空，不影响正文。
    pub(super) async fn chapter_snapshot(
        &self,
        user_ns: &str,
        book_id: &str,
        chapter_url: &str,
    ) -> (i32, String) {
        match self.toc_snapshot_map(user_ns, book_id).await {
            Some(map) => map.get(chapter_url).cloned().unwrap_or((0, String::new())),
            None => (0, String::new()),
        }
    }

    /// 取某本书的目录快照映射（`chapter_url -> (index, title)`）。
    ///
    /// 批量缓存（cacheCache SSE 循环起 N 个并发 `cache_chapter`）里每章
    /// 落库都要查一次目录元数据：逐章 parse 数 MB 的目录 JSON 是纯浪费。
    /// 这里把解析结果构建成 HashMap 后按 `(user_ns, book_id)` 短 TTL 备忘，
    /// 同一批次只在第一章 parse 一次；目录写路径（save/append/delete）主动
    /// 失效对应条目，TTL 只是兜底。
    /// 语义与逐次 `find` 一致：命中第一个同 URL 章节；目录缺失/为空时
    /// 不备忘，返回 None（回退 `(0, "")`）。
    async fn toc_snapshot_map(
        &self,
        user_ns: &str,
        book_id: &str,
    ) -> Option<Arc<HashMap<String, (i32, String)>>> {
        let key = format!("{user_ns}\u{0}{book_id}");
        {
            let memo = self.toc_snapshots.lock().unwrap();
            if let Some(entry) = memo.get(&key) {
                if entry.stored_at.elapsed() < TOC_SNAPSHOT_MEMO_TTL {
                    return Some(Arc::clone(&entry.map));
                }
            }
        }
        let list = self
            .load_chapter_list_cache(user_ns, book_id)
            .await
            .ok()??;
        if list.is_empty() {
            return None;
        }
        // 与逐次 find 相同的「首 URL 优先」：重复 URL 只保留第一个
        let mut map: HashMap<String, (i32, String)> = HashMap::with_capacity(list.len());
        for chapter in list {
            map.entry(chapter.url)
                .or_insert((chapter.index, chapter.title));
        }
        let snapshot = Arc::new(map);
        self.toc_snapshots.lock().unwrap().insert(
            key,
            TocSnapshotMemoEntry {
                stored_at: Instant::now(),
                map: Arc::clone(&snapshot),
            },
        );
        Some(snapshot)
    }

    /// 目录写路径调用：丢掉某本书的快照备忘，让下一次落库重新 parse。
    fn invalidate_toc_snapshot(&self, user_ns: &str, book_id: &str) {
        let key = format!("{user_ns}\u{0}{book_id}");
        self.toc_snapshots.lock().unwrap().remove(&key);
    }

    // Chapter list cache methods
    pub(super) fn chapter_list_cache_path(&self, user_ns: &str, book_id: &str) -> PathBuf {
        self.storage_dir
            .join("cache")
            .join(user_ns)
            .join("toc")
            .join(format!("{}.json", book_id_path_segment(book_id)))
    }

    /// 目录缓存的旁路元数据路径：与目录文件同目录，`<bookId>.meta.json`。
    fn chapter_list_cache_meta_path(&self, user_ns: &str, book_id: &str) -> PathBuf {
        self.storage_dir
            .join("cache")
            .join(user_ns)
            .join("toc")
            .join(format!("{}.meta.json", book_id_path_segment(book_id)))
    }

    /// 写目录缓存的完成标志旁路元数据（best-effort：目录文件才是权威数据，
    /// meta 写失败只打 WARN——读侧会按「存量缓存」回退 complete=true）。
    async fn write_chapter_list_cache_meta(&self, user_ns: &str, book_id: &str, complete: bool) {
        let meta = ChapterListCacheMeta {
            complete,
            saved_at: crate::util::time::now_ts(),
        };
        let meta_path = self.chapter_list_cache_meta_path(user_ns, book_id);
        match serde_json::to_string(&meta) {
            Ok(raw) => {
                if let Err(e) = write_atomic(&meta_path, raw.as_bytes()).await {
                    tracing::warn!("failed to write chapter list cache meta: {e}");
                }
            }
            Err(e) => tracing::warn!("failed to serialize chapter list cache meta: {e}"),
        }
    }

    /// 读目录缓存的完成标志。meta 缺失（升级前的存量缓存，或 meta 写失败）
    /// 按 complete=true 处理，保持升级前行为；存在但损坏同样回退 complete=true
    /// 并打 WARN（原子写下这几乎不可达）。
    async fn chapter_list_cache_complete(&self, user_ns: &str, book_id: &str) -> bool {
        let meta_path = self.chapter_list_cache_meta_path(user_ns, book_id);
        match fs::read_to_string(&meta_path).await {
            Ok(raw) => match serde_json::from_str::<ChapterListCacheMeta>(&raw) {
                Ok(meta) => meta.complete,
                Err(e) => {
                    tracing::warn!("chapter list cache meta corrupted, assume complete: {e}");
                    true
                }
            },
            Err(_) => true,
        }
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
        let data = match fs::read_to_string(&path).await {
            Ok(data) => data,
            // exists 与读取之间被并发删除：按未命中处理
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(AppError::Internal(e.into())),
        };
        // 损坏/半截写入的目录缓存按未命中降级：删掉坏文件让调用方重抓，
        // 而不是把 serde 错误冒成 400（目录本来就可再生）。
        match serde_json::from_str::<Vec<BookChapter>>(&data) {
            Ok(list) => Ok(Some(list)),
            Err(e) => {
                tracing::warn!("chapter list cache corrupted, dropping: {e}");
                let _ = fs::remove_file(&path).await;
                Ok(None)
            }
        }
    }

    /// 带 TTL 的目录缓存读取：超过 TTL 的文件按未命中处理（不删除，留给
    /// 其他裸读路径），由调用方重抓后覆盖写回。仅限「缓存直接喂响应」的
    /// 路径使用。TTL 按旁路 meta 的完成标志分档：未完成（首屏分页）只给
    /// `INCOMPLETE_TOC_CACHE_TTL`，完成（或存量缓存无 meta）按
    /// `CHAPTER_LIST_CACHE_TTL`。
    pub async fn load_fresh_chapter_list_cache(
        &self,
        user_ns: &str,
        book_id: &str,
    ) -> Result<Option<Vec<BookChapter>>, AppError> {
        let path = self.chapter_list_cache_path(user_ns, book_id);
        let ttl = if self.chapter_list_cache_complete(user_ns, book_id).await {
            CHAPTER_LIST_CACHE_TTL
        } else {
            INCOMPLETE_TOC_CACHE_TTL
        };
        let expired = match fs::metadata(&path).await {
            Ok(meta) => meta
                .modified()
                .ok()
                .and_then(|m| m.elapsed().ok())
                .map(|age| age > ttl)
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
        self.save_chapter_list_cache_with_state(user_ns, book_id, chapters, true)
            .await
    }

    /// 首屏分页落盘：目录尚未覆盖全部分页，meta 标记 `complete=false`，
    /// 后台补全成功（append）后置为完成。读侧对未完成缓存只给短 TTL。
    pub async fn save_chapter_list_cache_incomplete(
        &self,
        user_ns: &str,
        book_id: &str,
        chapters: &Vec<BookChapter>,
    ) -> Result<(), AppError> {
        self.save_chapter_list_cache_with_state(user_ns, book_id, chapters, false)
            .await
    }

    async fn save_chapter_list_cache_with_state(
        &self,
        user_ns: &str,
        book_id: &str,
        chapters: &Vec<BookChapter>,
        complete: bool,
    ) -> Result<(), AppError> {
        let path = self.chapter_list_cache_path(user_ns, book_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| AppError::Internal(e.into()))?;
        }
        let data =
            serde_json::to_string(chapters).map_err(|e| AppError::BadRequest(e.to_string()))?;
        // tmp + rename 原子覆盖：读侧（load_chapter_list_cache）要么看到旧目录、
        // 要么看到完整新目录，绝不会读到半截 JSON
        write_atomic(&path, data.as_bytes())
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
        // 先写目录本体再写 meta：读者最坏情况是把未完成目录当存量缓存
        // （complete=true）多复用一会儿，不会反过来把残缺目录标成已完成。
        self.write_chapter_list_cache_meta(user_ns, book_id, complete)
            .await;
        self.invalidate_toc_snapshot(user_ns, book_id);
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
        self.append_chapter_list_cache_with_complete(user_ns, book_id, new_chapters, true)
            .await
    }

    /// 后台补全落盘，`complete` 由分页抓取给出：完整补全把 meta 置完成（读侧
    /// 从短 TTL 回到正常 1 小时），截断补全（MAX_TOC_PAGES 上限 / 整页重复
    /// 熔断）保持未完成——已抓到的章节照常并入，60 秒后可重试。
    pub async fn append_chapter_list_cache_with_complete(
        &self,
        user_ns: &str,
        book_id: &str,
        new_chapters: &Vec<BookChapter>,
        complete: bool,
    ) -> Result<Vec<BookChapter>, AppError> {
        // 读-改-写整体持锁串行化：两个并发 append 无锁交错时后写者会基于旧快照
        // 落盘，先写者新增的章节被整体覆盖丢失。
        let _append_guard = self.toc_append_lock.lock().await;
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
        // 无论是否重写目录本体，都按调用方给的 complete 写 meta：新增章节全部
        // 与缓存重叠时目录内容同样完整与否取决于补全抓取本身，不由 added 决定。
        if added {
            self.save_chapter_list_cache_with_state(user_ns, book_id, &existing, complete)
                .await?;
        } else {
            self.write_chapter_list_cache_meta(user_ns, book_id, complete)
                .await;
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
        // 旁路 meta 一并清理，避免留下「无目录的孤儿 meta」
        let _ = fs::remove_file(self.chapter_list_cache_meta_path(user_ns, book_id)).await;
        self.invalidate_toc_snapshot(user_ns, book_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::book_service::tests::test_service;

    #[tokio::test]
    async fn append_chapter_list_cache_dedups_by_url_and_keeps_index_sequential() {
        let storage_dir =
            std::env::temp_dir().join(format!("reader-rust-toc-dedup-{}", std::process::id()));
        let (service, pool) = test_service(&storage_dir).await;
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
        pool.close().await;
        let _ = std::fs::remove_dir_all(&storage_dir);
    }

    #[tokio::test]
    async fn corrupted_chapter_list_cache_degrades_to_miss() {
        let storage_dir =
            std::env::temp_dir().join(format!("reader-rust-toc-corrupt-{}", std::process::id()));
        let (service, pool) = test_service(&storage_dir).await;
        let ns = "test-corrupt";
        let book_id = "book_1";

        // 半截 JSON（进程中途被杀 / 旧版直写读到一半）在缓存文件里
        let path = service.chapter_list_cache_path(ns, book_id);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"[{"url":"a","title":"A","index":0},"#).unwrap();

        // 读取按未命中降级（Ok(None)），不冒成 BadRequest；坏文件顺手删除
        let loaded = service.load_chapter_list_cache(ns, book_id).await.unwrap();
        assert!(loaded.is_none(), "损坏缓存应按未命中降级");
        assert!(!path.exists(), "坏文件应被删除");

        // 换源候选缓存同样降级
        let cand_path = service.book_source_cache_path(ns, book_id);
        std::fs::create_dir_all(cand_path.parent().unwrap()).unwrap();
        std::fs::write(&cand_path, "not-json").unwrap();
        let loaded = service.load_book_sources_cache(ns, book_id).await.unwrap();
        assert!(loaded.is_none());
        assert!(!cand_path.exists());

        // 覆盖写回后恢复正常读取
        service
            .save_chapter_list_cache(
                ns,
                book_id,
                &vec![BookChapter {
                    url: "a".into(),
                    title: "A".into(),
                    index: 0,
                    ..Default::default()
                }],
            )
            .await
            .unwrap();
        let loaded = service.load_chapter_list_cache(ns, book_id).await.unwrap();
        assert_eq!(loaded.unwrap().len(), 1);

        pool.close().await;
        let _ = std::fs::remove_dir_all(&storage_dir);
    }

    #[tokio::test]
    async fn concurrent_append_chapter_list_cache_keeps_all_chapters() {
        let storage_dir = std::env::temp_dir().join(format!(
            "reader-rust-toc-append-conc-{}",
            std::process::id()
        ));
        let (service, pool) = test_service(&storage_dir).await;
        let ns = "test-append-conc";
        let book_id = "book_1";

        let mk = |url: String| BookChapter {
            url: url.clone(),
            title: url,
            index: 0,
            ..Default::default()
        };
        // 两批各 30 章，URL 不相交：并发 append 后缓存必须 60 章全在
        let batch_a: Vec<BookChapter> = (0..30).map(|i| mk(format!("a{i}"))).collect();
        let batch_b: Vec<BookChapter> = (0..30).map(|i| mk(format!("b{i}"))).collect();
        let (ra, rb) = tokio::join!(
            service.append_chapter_list_cache(ns, book_id, &batch_a),
            service.append_chapter_list_cache(ns, book_id, &batch_b)
        );
        // 串行化语义：先拿到锁的 append 面对空缓存只返回自己那批（30），
        // 后完成者必须看到先完成者的写入（60）。无锁交错时两者都回 30
        // 且后写者覆盖先写者，最终缓存只剩一批。
        let len_a = ra.unwrap().len();
        let len_b = rb.unwrap().len();
        assert_eq!(
            len_a.max(len_b),
            60,
            "后完成的 append 必须包含先完成者的章节（实际 {len_a}/{len_b}）"
        );
        let final_list = service
            .load_chapter_list_cache(ns, book_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(final_list.len(), 60, "并发 append 不允许丢章");
        let indexes: HashSet<i32> = final_list.iter().map(|c| c.index).collect();
        assert_eq!(indexes.len(), 60, "index 必须连续不重复");

        pool.close().await;
        let _ = std::fs::remove_dir_all(&storage_dir);
    }

    /// 把文件 mtime 拨到指定时长前（模拟缓存过期）。
    fn set_file_age(path: &std::path::Path, age: std::time::Duration) {
        let file = std::fs::File::options().write(true).open(path).unwrap();
        let modified = std::time::SystemTime::now() - age;
        file.set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
    }

    #[tokio::test]
    async fn incomplete_toc_cache_meta_controls_ttl_and_cleanup() {
        let storage_dir =
            std::env::temp_dir().join(format!("reader-rust-toc-meta-{}", std::process::id()));
        let (service, pool) = test_service(&storage_dir).await;
        let ns = "test-toc-meta";
        let book_id = "book_1";

        let mk = |index: i32, url: &str| BookChapter {
            url: url.to_string(),
            title: url.to_string(),
            index,
            ..Default::default()
        };
        let toc_path = service.chapter_list_cache_path(ns, book_id);
        let meta_path = service.chapter_list_cache_meta_path(ns, book_id);
        let read_meta = || {
            serde_json::from_str::<ChapterListCacheMeta>(
                &std::fs::read_to_string(&meta_path).unwrap(),
            )
            .unwrap()
        };

        // 1) 首屏 incomplete 落盘：meta 存在且 complete=false；60s 内可命中
        service
            .save_chapter_list_cache_incomplete(ns, book_id, &vec![mk(0, "a"), mk(1, "b")])
            .await
            .unwrap();
        assert!(toc_path.exists());
        assert!(!read_meta().complete);
        assert_eq!(
            service
                .load_fresh_chapter_list_cache(ns, book_id)
                .await
                .unwrap()
                .unwrap()
                .len(),
            2,
            "未完成缓存 60s 内可命中"
        );

        // 超过 60s：按未命中（不删文件），裸读路径仍能拿到内容
        set_file_age(&toc_path, std::time::Duration::from_secs(61));
        assert!(
            service
                .load_fresh_chapter_list_cache(ns, book_id)
                .await
                .unwrap()
                .is_none(),
            "未完成缓存超过 60s 按未命中处理"
        );
        assert!(toc_path.exists(), "过期不删除目录文件");
        assert_eq!(
            service
                .load_chapter_list_cache(ns, book_id)
                .await
                .unwrap()
                .unwrap()
                .len(),
            2,
            "裸读路径不看 TTL"
        );

        // 2) 后台补全 append 后：meta 置为 complete，同样 61s 的年龄不再过期
        service
            .append_chapter_list_cache(ns, book_id, &vec![mk(9, "c")])
            .await
            .unwrap();
        assert!(read_meta().complete, "append 后 meta 置完成");
        set_file_age(&toc_path, std::time::Duration::from_secs(61));
        assert!(
            service
                .load_fresh_chapter_list_cache(ns, book_id)
                .await
                .unwrap()
                .is_some(),
            "完成缓存 61s 仍在 1h TTL 内"
        );
        set_file_age(&toc_path, std::time::Duration::from_secs(2 * 3600));
        assert!(
            service
                .load_fresh_chapter_list_cache(ns, book_id)
                .await
                .unwrap()
                .is_none(),
            "完成缓存超过 1h 按未命中处理"
        );

        // 3) meta 缺失（升级前的存量缓存）按 complete=true 处理
        std::fs::remove_file(&meta_path).unwrap();
        set_file_age(&toc_path, std::time::Duration::from_secs(61));
        assert!(
            service
                .load_fresh_chapter_list_cache(ns, book_id)
                .await
                .unwrap()
                .is_some(),
            "meta 缺失按存量缓存（complete）处理"
        );

        // 4) 完整落盘写 complete=true；delete 同时清理目录与 meta
        service
            .save_chapter_list_cache(ns, book_id, &vec![mk(0, "a")])
            .await
            .unwrap();
        assert!(read_meta().complete);
        service
            .delete_chapter_list_cache(ns, book_id)
            .await
            .unwrap();
        assert!(!toc_path.exists(), "delete 删除目录文件");
        assert!(!meta_path.exists(), "delete 连带删除 meta");

        pool.close().await;
        let _ = std::fs::remove_dir_all(&storage_dir);
    }

    #[tokio::test]
    async fn append_chapter_list_cache_with_complete_controls_meta_flag() {
        let storage_dir = std::env::temp_dir().join(format!(
            "reader-rust-toc-append-complete-{}",
            std::process::id()
        ));
        let (service, pool) = test_service(&storage_dir).await;
        let ns = "test-append-complete";
        let book_id = "book_1";

        let mk = |index: i32, url: &str| BookChapter {
            url: url.to_string(),
            title: url.to_string(),
            index,
            ..Default::default()
        };
        let meta_path = service.chapter_list_cache_meta_path(ns, book_id);
        let read_meta = || {
            serde_json::from_str::<ChapterListCacheMeta>(
                &std::fs::read_to_string(&meta_path).unwrap(),
            )
            .unwrap()
        };

        // 首屏 incomplete 落盘
        service
            .save_chapter_list_cache_incomplete(ns, book_id, &vec![mk(0, "a")])
            .await
            .unwrap();
        assert!(!read_meta().complete);

        // 截断补全（complete=false）：新章节照常并入，meta 保持未完成
        let all = service
            .append_chapter_list_cache_with_complete(ns, book_id, &vec![mk(1, "b")], false)
            .await
            .unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all.last().unwrap().url, "b");
        assert!(!read_meta().complete, "截断补全不把 meta 置完成");

        // 完整补全（旧入口 append_chapter_list_cache，complete=true）置完成
        service
            .append_chapter_list_cache(ns, book_id, &vec![mk(2, "c")])
            .await
            .unwrap();
        assert!(read_meta().complete, "完整 append 置完成");

        // 无新增时也按传入的 complete 写 meta：截断补全（即使全部重叠）标未完成
        service
            .append_chapter_list_cache_with_complete(ns, book_id, &vec![mk(9, "c")], false)
            .await
            .unwrap();
        assert!(!read_meta().complete, "无新增的截断补全同样标未完成");

        // 无新增的完整补全恢复完成标志
        service
            .append_chapter_list_cache(ns, book_id, &vec![mk(9, "c")])
            .await
            .unwrap();
        assert!(read_meta().complete, "无新增的完整补全恢复完成标志");

        pool.close().await;
        let _ = std::fs::remove_dir_all(&storage_dir);
    }

    #[tokio::test]
    async fn chapter_snapshot_memo_reuses_and_invalidates() {
        let storage_dir =
            std::env::temp_dir().join(format!("reader-rust-toc-snapshot-{}", std::process::id()));
        let (service, pool) = test_service(&storage_dir).await;
        let ns = "test-snapshot";
        let book_id = "book_1";

        let mk = |index: i32, url: &str, title: &str| BookChapter {
            url: url.to_string(),
            title: title.to_string(),
            index,
            ..Default::default()
        };
        service
            .save_chapter_list_cache(ns, book_id, &vec![mk(0, "a", "A"), mk(1, "b", "B")])
            .await
            .unwrap();

        // 命中：返回 (index, title)；缺失章节回退 (0, "")（语义与逐次 find 一致）
        assert_eq!(
            service.chapter_snapshot(ns, book_id, "b").await,
            (1, "B".into())
        );
        assert_eq!(
            service.chapter_snapshot(ns, book_id, "missing").await,
            (0, String::new())
        );
        // 第一次查询后条目已在备忘里：无目录的别的书不影响
        assert!(service.toc_snapshot_map(ns, book_id).await.is_some());
        assert!(service.toc_snapshot_map(ns, "no-such-book").await.is_none());

        // 覆盖写目录（index 变化）后备忘失效：立刻看到新值
        service
            .save_chapter_list_cache(ns, book_id, &vec![mk(5, "a", "A2"), mk(6, "b", "B2")])
            .await
            .unwrap();
        assert_eq!(
            service.chapter_snapshot(ns, book_id, "a").await,
            (5, "A2".into())
        );

        // append 走 save 路径，同样失效；覆盖写后目录只有 2 章，c 拿到 index=2
        service
            .append_chapter_list_cache(ns, book_id, &vec![mk(9, "c", "C")])
            .await
            .unwrap();
        assert_eq!(
            service.chapter_snapshot(ns, book_id, "c").await,
            (2, "C".into())
        );

        // 删除目录后回退 (0, "")
        service
            .delete_chapter_list_cache(ns, book_id)
            .await
            .unwrap();
        assert_eq!(
            service.chapter_snapshot(ns, book_id, "a").await,
            (0, String::new())
        );

        pool.close().await;
        let _ = std::fs::remove_dir_all(&storage_dir);
    }
}
