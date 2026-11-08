//! 章节列表（目录）：分页抓取、SQLite 章节登记表、后台补全与章节快照。
//!
//! 目录持久化在章节登记表（`chapters` + `book_toc_state`，见 0007 迁移）：
//! 完整目录 1 小时 TTL，首屏分页的未完成目录只给 60 秒短 TTL。后台补全由
//! `TocFillGuard` 防并发、`toc_append_lock` 串行化读-改-写。分页抓取有
//! MAX_TOC_PAGES 上限与整页重复熔断，防失控书源把翻页变成无限请求。
//! index 在登记表里有稳定归属，`resolve_chapter_by_index` 是
//! 「(bookId, index) 定位章节」的解析入口，v1 边界直接调用。

use super::BookService;
use crate::error::error::AppError;
use crate::model::book_chapter::BookChapter;
use crate::model::book_source::BookSource;
use crate::storage::db::repo::parse_book_id;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use tokio::time::Duration;

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

/// 目录分页总页数上限：单链分支跟随书源给的 next 链接，恶意/失控书源
/// 每次回一个新 URL 就会变成无限请求。500 页按每页几十章估算覆盖数万章，
/// 大书足够；达到上限停止翻页并保留已抓章节（与正文 MAX_CONTENT_PAGES
/// 同一思路）。
const MAX_TOC_PAGES: usize = 500;

/// 章节列表缓存的存活时间（1 小时）。
///
/// 只作用于「把目录直接喂给响应」的读路径（过期按未命中处理，重抓后
/// 整表覆盖写回）。供反向查找、后台补全使用的 `load_chapter_list_cache`
/// 不做 TTL——那些路径宁可读到旧目录，也不该顺手删掉还能用的行。
const CHAPTER_LIST_TTL: Duration = Duration::from_secs(3600);

/// 未完成目录（首屏只落了第一页、后台补全尚未成功）的存活时间（60 秒）。
///
/// 首屏先落库是为了让紧随其后的请求（翻目录、读者点开第一章）立即命中，
/// 但后台补全失败时这份目录只有第一页——按完整 TTL（1 小时）复用会让
/// 「残缺目录」被当成全书目录服务一小时。未完成目录只复用一小段时间，
/// 过期后调用方重抓首屏并重新拉起补全；补全成功由 append 把状态置为
/// 完成，之后按 `CHAPTER_LIST_TTL` 走。
const INCOMPLETE_TOC_TTL: Duration = Duration::from_secs(60);

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
        // 落库：读侧只给 60s 短 TTL，过期后重抓即可补全，截断目录不当完整目录
        // 缓存一小时。
        let _ = self
            .store_chapter_list(user_ns, book_id, &chapters, complete)
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

    /// 章节快照（章号/标题）：登记表按 URL 反查；取不到就留空，不影响正文。
    /// 登记表是索引化查询，逐章查成本可忽略，无需备忘。
    pub(super) async fn chapter_snapshot(
        &self,
        _user_ns: &str,
        book_id: &str,
        chapter_url: &str,
    ) -> (i32, String) {
        let Some(id) = Self::chapter_row_id(book_id) else {
            return (0, String::new());
        };
        self.chapters
            .find_chapter_by_url(id, chapter_url)
            .await
            .ok()
            .flatten()
            .unwrap_or((0, String::new()))
    }

    // ── 目录的登记表读写 ────────────────────────────────────────────────

    /// bookId → 登记表行 id。非法 id 不读库不落库（与缓存路径时代的隔离段
    /// 语义一致：读返回空、写是 no-op），handler 层的 resolve_book 是主防线。
    fn chapter_row_id(book_id: &str) -> Option<i64> {
        match parse_book_id(book_id) {
            Some(id) => Some(id),
            None => {
                tracing::warn!("invalid bookId for chapter registry: {book_id:?}");
                None
            }
        }
    }

    pub async fn load_chapter_list_cache(
        &self,
        _user_ns: &str,
        book_id: &str,
    ) -> Result<Option<Vec<BookChapter>>, AppError> {
        let Some(id) = Self::chapter_row_id(book_id) else {
            return Ok(None);
        };
        let rows = self
            .chapters
            .get_chapters(id)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
        Ok((!rows.is_empty()).then_some(rows))
    }

    /// 带 TTL 的目录读取：完整目录 1 小时、未完成 60 秒，过期按未命中处理
    ///（不删除，留给反向查找等裸读路径），由调用方重抓后整表覆盖。仅限
    /// 「目录直接喂响应」的路径使用，TTL 按 `book_toc_state.complete` 分档。
    pub async fn load_fresh_chapter_list_cache(
        &self,
        user_ns: &str,
        book_id: &str,
    ) -> Result<Option<Vec<BookChapter>>, AppError> {
        let Some(id) = Self::chapter_row_id(book_id) else {
            return Ok(None);
        };
        let Some(state) = self
            .chapters
            .get_toc_state(id)
            .await
            .map_err(|e| AppError::Internal(e.into()))?
        else {
            return Ok(None);
        };
        let ttl = if state.complete {
            CHAPTER_LIST_TTL
        } else {
            INCOMPLETE_TOC_TTL
        };
        if crate::util::time::now_ts() - state.fetched_at >= ttl.as_secs() as i64 {
            return Ok(None);
        }
        self.load_chapter_list_cache(user_ns, book_id).await
    }

    pub async fn save_chapter_list_cache(
        &self,
        user_ns: &str,
        book_id: &str,
        chapters: &[BookChapter],
    ) -> Result<(), AppError> {
        self.store_chapter_list(user_ns, book_id, chapters, true)
            .await
    }

    /// 首屏分页落库：目录尚未覆盖全部分页，状态标 `complete=false`，
    /// 后台补全成功（append）后置为完成。读侧对未完成目录只给短 TTL。
    pub async fn save_chapter_list_cache_incomplete(
        &self,
        user_ns: &str,
        book_id: &str,
        chapters: &[BookChapter],
    ) -> Result<(), AppError> {
        self.store_chapter_list(user_ns, book_id, chapters, false)
            .await
    }

    async fn store_chapter_list(
        &self,
        _user_ns: &str,
        book_id: &str,
        chapters: &[BookChapter],
        complete: bool,
    ) -> Result<(), AppError> {
        let Some(id) = Self::chapter_row_id(book_id) else {
            return Ok(());
        };
        self.chapters
            .replace_chapters(id, chapters, complete, crate::util::time::now_ts())
            .await
            .map_err(|e| AppError::Internal(e.into()))
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
        new_chapters: &[BookChapter],
    ) -> Result<Vec<BookChapter>, AppError> {
        self.append_chapter_list_cache_with_complete(user_ns, book_id, new_chapters, true)
            .await
    }

    /// 后台补全落库，`complete` 由分页抓取给出：完整补全把状态置完成（读侧
    /// 从短 TTL 回到正常 1 小时），截断补全（MAX_TOC_PAGES 上限 / 整页重复
    /// 熔断）保持未完成——已抓到的章节照常并入，60 秒后可重试。
    pub async fn append_chapter_list_cache_with_complete(
        &self,
        _user_ns: &str,
        book_id: &str,
        new_chapters: &[BookChapter],
        complete: bool,
    ) -> Result<Vec<BookChapter>, AppError> {
        // 读-改-写整体持锁串行化：两个并发 append 无锁交错时后写者会基于旧
        // 快照落库，先写者新增的章节被整体覆盖丢失（事务 IMMEDIATE 是兜底）。
        let _append_guard = self.toc_append_lock.lock().await;
        let Some(id) = Self::chapter_row_id(book_id) else {
            return Ok(Vec::new());
        };
        self.chapters
            .append_chapters(id, new_chapters, complete, crate::util::time::now_ts())
            .await
            .map_err(|e| AppError::Internal(e.into()))
    }

    /// 删除某本书的目录登记行与抓取状态（refresh=1、换源、清缓存）。
    pub async fn delete_chapter_list_cache(
        &self,
        _user_ns: &str,
        book_id: &str,
    ) -> Result<(), AppError> {
        let Some(id) = Self::chapter_row_id(book_id) else {
            return Ok(());
        };
        self.chapters
            .delete_chapters(id)
            .await
            .map_err(|e| AppError::Internal(e.into()))
    }

    /// index → 章节记录：目录登记表是 index 的唯一权威，对外 API 只凭
    /// (bookId, index) 定位章节时的解析入口（v1 边界直接调用）。
    pub async fn resolve_chapter_by_index(
        &self,
        book_id: &str,
        index: i32,
    ) -> Result<Option<BookChapter>, AppError> {
        let Some(id) = Self::chapter_row_id(book_id) else {
            return Ok(None);
        };
        self.chapters
            .find_chapter_by_index(id, index)
            .await
            .map_err(|e| AppError::Internal(e.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::book_service::tests::test_service;

    fn mk(index: i32, url: &str) -> BookChapter {
        BookChapter {
            url: url.to_string(),
            title: url.to_string(),
            index,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn append_chapter_list_cache_dedups_by_url_and_keeps_index_sequential() {
        let storage_dir =
            std::env::temp_dir().join(format!("reader-rust-toc-dedup-{}", std::process::id()));
        let (service, pool) = test_service(&storage_dir).await;
        let ns = "test-dedup";
        let book_id = "book_1";

        service
            .save_chapter_list_cache(ns, book_id, &[mk(0, "a"), mk(1, "b")])
            .await
            .unwrap();
        // 补全结果与登记表尾部重叠（竞态重放），另含一个空 URL 章节
        let all = service
            .append_chapter_list_cache(ns, book_id, &[mk(9, "b"), mk(9, "c"), mk(9, "")])
            .await
            .unwrap();
        let urls: Vec<&str> = all.iter().map(|c| c.url.as_str()).collect();
        assert_eq!(urls, vec!["a", "b", "c", ""]);
        let indexes: Vec<i32> = all.iter().map(|c| c.index).collect();
        assert_eq!(indexes, vec![0, 1, 2, 3]);
        // 再次追加完全重叠的结果：无新增时不改登记行
        let again = service
            .append_chapter_list_cache(ns, book_id, &[mk(9, "c")])
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
    async fn corrupted_book_source_cache_degrades_to_miss() {
        let storage_dir =
            std::env::temp_dir().join(format!("reader-rust-toc-corrupt-{}", std::process::id()));
        let (service, pool) = test_service(&storage_dir).await;
        let ns = "test-corrupt";
        let book_id = "book_1";

        // 换源候选缓存损坏按未命中降级：不冒成错误，坏文件顺手删除
        let cand_path = service.book_source_cache_path(ns, book_id);
        std::fs::create_dir_all(cand_path.parent().unwrap()).unwrap();
        std::fs::write(&cand_path, "not-json").unwrap();
        let loaded = service.load_book_sources_cache(ns, book_id).await.unwrap();
        assert!(loaded.is_none());
        assert!(!cand_path.exists());

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

        let mk_url = |url: String| BookChapter {
            url: url.clone(),
            title: url,
            index: 0,
            ..Default::default()
        };
        // 两批各 30 章，URL 不相交：并发 append 后登记表必须 60 章全在
        let batch_a: Vec<BookChapter> = (0..30).map(|i| mk_url(format!("a{i}"))).collect();
        let batch_b: Vec<BookChapter> = (0..30).map(|i| mk_url(format!("b{i}"))).collect();
        let (ra, rb) = tokio::join!(
            service.append_chapter_list_cache(ns, book_id, &batch_a),
            service.append_chapter_list_cache(ns, book_id, &batch_b)
        );
        // 串行化语义：先拿到锁的 append 面对空登记表只返回自己那批（30），
        // 后完成者必须看到先完成者的写入（60）。无锁交错时两者都回 30
        // 且后写者覆盖先写者，最终登记表只剩一批。
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

    #[tokio::test]
    async fn incomplete_toc_state_controls_freshness_and_cleanup() {
        let storage_dir =
            std::env::temp_dir().join(format!("reader-rust-toc-meta-{}", std::process::id()));
        let (service, pool) = test_service(&storage_dir).await;
        let ns = "test-toc-meta";
        let book_id = "book_1";
        let row_id = parse_book_id(book_id).unwrap();
        let backdate = |secs: i64| {
            service
                .chapters
                .set_toc_fetched_at(row_id, crate::util::time::now_ts() - secs)
        };

        // 1) 首屏 incomplete 落库：状态 complete=false；60s 内可命中
        service
            .save_chapter_list_cache_incomplete(ns, book_id, &[mk(0, "a"), mk(1, "b")])
            .await
            .unwrap();
        assert_eq!(
            service
                .load_fresh_chapter_list_cache(ns, book_id)
                .await
                .unwrap()
                .unwrap()
                .len(),
            2,
            "未完成目录 60s 内可命中"
        );

        // 超过 60s：按未命中（不删行），裸读路径仍能拿到内容
        backdate(61).await;
        assert!(
            service
                .load_fresh_chapter_list_cache(ns, book_id)
                .await
                .unwrap()
                .is_none(),
            "未完成目录超过 60s 按未命中处理"
        );
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

        // 2) 后台补全 append 后：状态置为 complete，同样 61s 的年龄不再过期
        service
            .append_chapter_list_cache(ns, book_id, &[mk(9, "c")])
            .await
            .unwrap();
        backdate(61).await;
        assert!(
            service
                .load_fresh_chapter_list_cache(ns, book_id)
                .await
                .unwrap()
                .is_some(),
            "完成目录 61s 仍在 1h TTL 内"
        );
        backdate(2 * 3600).await;
        assert!(
            service
                .load_fresh_chapter_list_cache(ns, book_id)
                .await
                .unwrap()
                .is_none(),
            "完成目录超过 1h 按未命中处理"
        );

        // 3) 完整落库写 complete=true；delete 同时清理目录行与状态
        service
            .save_chapter_list_cache(ns, book_id, &[mk(0, "a")])
            .await
            .unwrap();
        let state = service
            .chapters
            .get_toc_state(row_id)
            .await
            .unwrap()
            .unwrap();
        assert!(state.complete);
        service
            .delete_chapter_list_cache(ns, book_id)
            .await
            .unwrap();
        assert!(
            service
                .load_chapter_list_cache(ns, book_id)
                .await
                .unwrap()
                .is_none(),
            "delete 删除目录行"
        );
        assert!(
            service
                .chapters
                .get_toc_state(row_id)
                .await
                .unwrap()
                .is_none(),
            "delete 连抓取状态一起清"
        );

        pool.close().await;
        let _ = std::fs::remove_dir_all(&storage_dir);
    }

    #[tokio::test]
    async fn append_chapter_list_cache_with_complete_controls_state_flag() {
        let storage_dir = std::env::temp_dir().join(format!(
            "reader-rust-toc-append-complete-{}",
            std::process::id()
        ));
        let (service, pool) = test_service(&storage_dir).await;
        let ns = "test-append-complete";
        let book_id = "book_1";
        let row_id = parse_book_id(book_id).unwrap();
        let state_complete = || async {
            service
                .chapters
                .get_toc_state(row_id)
                .await
                .unwrap()
                .unwrap()
                .complete
        };

        // 首屏 incomplete 落库
        service
            .save_chapter_list_cache_incomplete(ns, book_id, &[mk(0, "a")])
            .await
            .unwrap();
        assert!(!state_complete().await);

        // 截断补全（complete=false）：新章节照常并入，状态保持未完成
        let all = service
            .append_chapter_list_cache_with_complete(ns, book_id, &[mk(1, "b")], false)
            .await
            .unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all.last().unwrap().url, "b");
        assert!(!state_complete().await, "截断补全不把状态置完成");

        // 完整补全（旧入口 append_chapter_list_cache，complete=true）置完成
        service
            .append_chapter_list_cache(ns, book_id, &[mk(2, "c")])
            .await
            .unwrap();
        assert!(state_complete().await, "完整 append 置完成");

        // 无新增时也按传入的 complete 更新状态：截断补全（即使全部重叠）标未完成
        service
            .append_chapter_list_cache_with_complete(ns, book_id, &[mk(9, "c")], false)
            .await
            .unwrap();
        assert!(!state_complete().await, "无新增的截断补全同样标未完成");

        // 无新增的完整补全恢复完成标志
        service
            .append_chapter_list_cache(ns, book_id, &[mk(9, "c")])
            .await
            .unwrap();
        assert!(state_complete().await, "无新增的完整补全恢复完成标志");

        pool.close().await;
        let _ = std::fs::remove_dir_all(&storage_dir);
    }

    #[tokio::test]
    async fn chapter_snapshot_reads_registry_and_falls_back() {
        let storage_dir =
            std::env::temp_dir().join(format!("reader-rust-toc-snapshot-{}", std::process::id()));
        let (service, pool) = test_service(&storage_dir).await;
        let ns = "test-snapshot";
        let book_id = "book_1";

        let mk_full = |index: i32, url: &str, title: &str| BookChapter {
            url: url.to_string(),
            title: title.to_string(),
            index,
            ..Default::default()
        };
        service
            .save_chapter_list_cache(ns, book_id, &[mk_full(0, "a", "A"), mk_full(1, "b", "B")])
            .await
            .unwrap();

        // 命中：返回 (index, title)；缺失章节回退 (0, "")
        assert_eq!(
            service.chapter_snapshot(ns, book_id, "b").await,
            (1, "B".into())
        );
        assert_eq!(
            service.chapter_snapshot(ns, book_id, "missing").await,
            (0, String::new())
        );

        // 覆盖写目录（index 变化）后立刻看到新值
        service
            .save_chapter_list_cache(ns, book_id, &[mk_full(5, "a", "A2"), mk_full(6, "b", "B2")])
            .await
            .unwrap();
        assert_eq!(
            service.chapter_snapshot(ns, book_id, "a").await,
            (5, "A2".into())
        );

        // append 后 c 拿到 index=7：追加的 index 从现有最大值 +1 顺延
        //（人工覆盖写造出了 5、6 的空洞，生产路径整表覆盖总是从 0 连续编号）
        service
            .append_chapter_list_cache(ns, book_id, &[mk_full(9, "c", "C")])
            .await
            .unwrap();
        assert_eq!(
            service.chapter_snapshot(ns, book_id, "c").await,
            (7, "C".into())
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

    /// v1 边界的章节身份解析：登记表是 index 的唯一权威，
    /// 不依赖目录文件缓存与上游可达性。
    #[tokio::test]
    async fn resolve_chapter_by_index_maps_index_to_chapter() {
        let storage_dir =
            std::env::temp_dir().join(format!("reader-rust-toc-resolve-{}", std::process::id()));
        let (service, pool) = test_service(&storage_dir).await;
        let ns = "test-resolve";
        let book_id = "book_1";

        service
            .save_chapter_list_cache(ns, book_id, &[mk(0, "https://a/0"), mk(1, "https://a/1")])
            .await
            .unwrap();

        assert_eq!(
            service
                .resolve_chapter_by_index(book_id, 0)
                .await
                .unwrap()
                .map(|c| c.url),
            Some("https://a/0".into())
        );
        assert_eq!(
            service
                .resolve_chapter_by_index(book_id, 1)
                .await
                .unwrap()
                .map(|c| c.url),
            Some("https://a/1".into())
        );
        assert!(
            service
                .resolve_chapter_by_index(book_id, 9)
                .await
                .unwrap()
                .is_none(),
            "越界 index 返回 None"
        );
        assert!(
            service
                .resolve_chapter_by_index("book_999", 0)
                .await
                .unwrap()
                .is_none(),
            "未登记的书返回 None"
        );

        // 删除目录后解析随之失效（与目录生命周期一致）
        service
            .delete_chapter_list_cache(ns, book_id)
            .await
            .unwrap();
        assert!(service
            .resolve_chapter_by_index(book_id, 0)
            .await
            .unwrap()
            .is_none());

        pool.close().await;
        let _ = std::fs::remove_dir_all(&storage_dir);
    }
}
