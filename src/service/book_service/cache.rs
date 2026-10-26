//! 可再生缓存的清理、占用统计与缓存文件路径。
//!
//! 覆盖 `purgeCache` / `cacheStats` 两个端点背后的全部逻辑：封面（含图片
//! 管道目录）、目录、书籍详情、换源候选、评论与失效书源清单的按用户/按类型/
//! 按书清理。正文历史（正文池与版本时间线）不在清理范围内——它严格保留、
//! 不提供清理入口。

use super::{book_id_path_segment, BookService, BOOK_INFO_CACHE_DIR};
use crate::error::error::AppError;
use crate::model::search::SearchBook;
use crate::storage::cache::file_cache::{
    remove_dir_counting_files, remove_file_counting, CacheUsage,
};
use crate::util::atomic_write::write_atomic;
use serde_json::Value;
use std::path::PathBuf;
use tokio::fs;
use tokio::time::Duration;

/// 换源候选缓存的存活时间（1 小时），与章节列表同口径：文件 mtime 判定，
/// 过期按未命中处理（不删除，留给换源改绑等裸读路径），由调用方重搜后
/// 整份覆盖写回。只作用于「把缓存直接喂给响应」的读路径。
const SOURCE_CANDIDATES_CACHE_TTL: Duration = Duration::from_secs(3600);

/// 缓存清理的粒度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CacheKind {
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

impl BookService {
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

    /// 清理某个用户的可再生缓存。
    ///
    /// 正文历史不在这里：正文池和时间线严格保留，不提供清理入口。
    pub async fn purge_user_cache(&self, user_ns: &str) -> Result<CachePurgeResult, AppError> {
        let result = CachePurgeResult {
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
                .map_err(internal_error)?
                + remove_dir_counting_files(
                    &self
                        .storage_dir
                        .join("cache")
                        .join(BOOK_INFO_CACHE_DIR)
                        .join(user_ns),
                )
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
        Ok(result)
    }

    /// 清理全部用户的可再生缓存。正文历史不受影响。
    pub async fn purge_all_cache(&self) -> Result<CachePurgeResult, AppError> {
        let mut total = CachePurgeResult::default();
        for user_ns in self.cache_user_namespaces().await {
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

    /// `cache/` 下仍是用户命名空间的目录清单（toc/cover/source_candidates 所在层）。
    async fn cache_user_namespaces(&self) -> Vec<String> {
        const NON_USER_DIRS: [&str; 4] = ["reviews", "bookinfo", "image", "invalid_book_sources"];
        let mut names = Vec::new();
        let Ok(mut entries) = fs::read_dir(self.storage_dir.join("cache")).await else {
            return names;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let is_dir = entry
                .file_type()
                .await
                .map(|kind| kind.is_dir())
                .unwrap_or(false);
            if !is_dir {
                continue;
            }
            if let Some(name) = entry.file_name().to_str() {
                if !NON_USER_DIRS.contains(&name) {
                    names.push(name.to_string());
                }
            }
        }
        names.sort();
        names
    }

    /// 按缓存类型清理某个用户的一层缓存。
    pub async fn purge_user_cache_kind(
        &self,
        user_ns: &str,
        kind: CacheKind,
    ) -> Result<CachePurgeResult, AppError> {
        let mut result = CachePurgeResult::default();
        match kind {
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

    /// 换源时清理源相关缓存（目录/详情/评论），但保留换源候选清单
    ///——它就是为下一次换源准备的。正文历史不删，旧源版本仍留在时间线里。
    pub async fn purge_book_reading_cache(
        &self,
        user_ns: &str,
        book_id: &str,
    ) -> Result<(), AppError> {
        let _ = self.delete_chapter_list_cache(user_ns, book_id).await;
        let _ = fs::remove_file(self.book_info_cache_path(user_ns, book_id)).await;
        let _ = self.review_cache.remove_book(user_ns, book_id).await;
        Ok(())
    }

    /// 清理单本书的可再生缓存（目录/详情/换源候选/评论）。正文历史不删。
    pub async fn purge_book_cache(
        &self,
        user_ns: &str,
        book_id: &str,
    ) -> Result<CachePurgeResult, AppError> {
        let mut result = CachePurgeResult::default();
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

    /// 各层缓存的当前占用。正文历史整部署共享，不随 `user_ns` 变化。
    pub async fn cache_stats(&self, user_ns: Option<&str>) -> Result<Value, AppError> {
        use crate::storage::cache::file_cache::dir_usage;
        let (contents, versions, chapters, bytes) = self.content.stats().await?;
        let content = serde_json::json!({
            "files": contents,
            "bytes": bytes,
            "chapters": chapters,
            "versions": versions,
        });
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

    /// 本章是否有任何可读版本（上游或用户版本）。
    pub async fn is_chapter_cached(
        &self,
        _user_ns: &str,
        book_id: &str,
        chapter_url: &str,
    ) -> bool {
        self.content
            .has_content(book_id, chapter_url)
            .await
            .unwrap_or(false)
    }

    pub async fn chapter_list_cache_exists(&self, user_ns: &str, book_id: &str) -> bool {
        let path = self.chapter_list_cache_path(user_ns, book_id);
        path.exists()
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
        let data = match fs::read_to_string(&path).await {
            Ok(data) => data,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(AppError::Internal(e.into())),
        };
        // 换源候选同样可再生存级，坏文件按未命中降级并删除
        match serde_json::from_str::<Vec<SearchBook>>(&data) {
            Ok(list) => Ok(Some(list)),
            Err(e) => {
                tracing::warn!("source candidates cache corrupted, dropping: {e}");
                let _ = fs::remove_file(&path).await;
                Ok(None)
            }
        }
    }

    /// 带 TTL 的换源候选缓存读取：超过 `SOURCE_CANDIDATES_CACHE_TTL` 的文件
    /// 按未命中处理（不删除，留给其他裸读路径），由调用方重搜后覆盖写回。
    /// 仅限「缓存直接喂响应」的路径使用。
    pub async fn load_fresh_book_sources_cache(
        &self,
        user_ns: &str,
        book_id: &str,
    ) -> Result<Option<Vec<SearchBook>>, AppError> {
        let path = self.book_source_cache_path(user_ns, book_id);
        let expired = match fs::metadata(&path).await {
            Ok(meta) => meta
                .modified()
                .ok()
                .and_then(|m| m.elapsed().ok())
                .map(|age| age > SOURCE_CANDIDATES_CACHE_TTL)
                .unwrap_or(true),
            Err(_) => false,
        };
        if expired {
            return Ok(None);
        }
        self.load_book_sources_cache(user_ns, book_id).await
    }

    pub async fn save_book_sources_cache(
        &self,
        user_ns: &str,
        book_id: &str,
        list: &Vec<SearchBook>,
    ) -> Result<(), AppError> {
        let path = self.book_source_cache_path(user_ns, book_id);
        let data = serde_json::to_string(list).map_err(|e| AppError::BadRequest(e.to_string()))?;
        write_atomic(&path, data.as_bytes())
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

    pub(super) fn book_source_cache_path(&self, user_ns: &str, book_id: &str) -> PathBuf {
        self.storage_dir
            .join("cache")
            .join(user_ns)
            .join("source_candidates")
            .join(format!("{}.json", book_id_path_segment(book_id)))
    }

    /// 删除一本书的全部源相关缓存（正文/目录/详情/换源候选/评论）。
    pub(super) async fn clear_book_related_cache(
        &self,
        user_ns: &str,
        book_id: &str,
    ) -> Result<(), AppError> {
        let _ = self.delete_book_sources_cache(user_ns, book_id).await;
        let _ = self.delete_chapter_list_cache(user_ns, book_id).await;
        let _ = fs::remove_file(self.book_info_cache_path(user_ns, book_id)).await;
        let _ = self.review_cache.remove_book(user_ns, book_id).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::book_chapter::BookChapter;
    use crate::service::book_service::tests::{make_book, test_service};
    use crate::storage::db::repo::RegisterBook;

    #[tokio::test]
    async fn fresh_book_sources_cache_expires_after_ttl() {
        let storage_dir =
            std::env::temp_dir().join(format!("reader-rust-candidates-ttl-{}", std::process::id()));
        let (service, pool) = test_service(&storage_dir).await;
        let ns = "test-candidates-ttl";
        let book_id = "book_1";

        let list = vec![SearchBook {
            name: "书".into(),
            author: "作者".into(),
            book_url: "https://example.com/book".into(),
            origin: "https://example.com".into(),
            ..Default::default()
        }];
        service
            .save_book_sources_cache(ns, book_id, &list)
            .await
            .unwrap();

        // 新鲜缓存：fresh 与裸读都命中
        assert!(service
            .load_fresh_book_sources_cache(ns, book_id)
            .await
            .unwrap()
            .is_some());

        // 把 mtime 拨到 2 小时前：fresh 按未命中（不删文件），裸读仍能拿到内容
        let path = service.book_source_cache_path(ns, book_id);
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        let two_hours_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 3600);
        file.set_times(std::fs::FileTimes::new().set_modified(two_hours_ago))
            .unwrap();
        drop(file);

        assert!(
            service
                .load_fresh_book_sources_cache(ns, book_id)
                .await
                .unwrap()
                .is_none(),
            "过期候选应按未命中处理"
        );
        let stale = service.load_book_sources_cache(ns, book_id).await.unwrap();
        assert_eq!(stale.unwrap().len(), 1, "裸读路径不看 TTL，仍能读到旧候选");
        assert!(path.exists(), "过期不删除文件");

        pool.close().await;
        let _ = std::fs::remove_dir_all(&storage_dir);
    }

    #[tokio::test]
    async fn cache_paths_normalize_book_id_aliases_and_isolate_invalid() {
        let storage_dir =
            std::env::temp_dir().join(format!("reader-rust-pathkey-{}", std::process::id()));
        let (service, pool) = test_service(&storage_dir).await;

        // 别名（前导零/显式正号）与 canonical 收敛到同一路径：同一本书一份缓存
        let canonical = service.chapter_list_cache_path("u", "book_7");
        assert_eq!(canonical, service.chapter_list_cache_path("u", "book_007"));
        assert_eq!(canonical, service.chapter_list_cache_path("u", "book_+7"));
        assert!(service
            .book_source_cache_path("u", "book_07")
            .ends_with("book_7.json"));

        // 非法 id 落隔离段：读为空、写进隔离区、删除是 no-op，绝不越出缓存目录
        for invalid in ["../../etc/passwd", "book_1/../../x", "C:/temp/x"] {
            let path = service.chapter_list_cache_path("u", invalid);
            assert!(
                path.to_string_lossy().contains("__invalid_book__"),
                "{invalid:?} 应落隔离段，实际 {path:?}"
            );
            assert!(path.starts_with(&storage_dir));
        }
        assert!(service
            .book_info_cache_path("u", "../x")
            .to_string_lossy()
            .contains("__invalid_book__"));

        pool.close().await;
        let _ = std::fs::remove_dir_all(&storage_dir);
    }

    #[tokio::test]
    async fn purge_user_cache_keeps_registrations_and_content_history() {
        let storage_dir =
            std::env::temp_dir().join(format!("reader-rust-purge-{}", std::process::id()));
        let (service, pool) = test_service(&storage_dir).await;

        // 在架一本 + 非在架登记一本
        service
            .save_book("u", make_book("https://example.com/onshelf"))
            .await
            .unwrap();
        let orphan_id = service
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
        // 给孤儿登记行写一个用户版本和一份目录缓存
        service
            .content()
            .save_user_version(
                "u",
                &orphan_id,
                "http://up/ch/1",
                "edited",
                "编辑内容",
                None,
                0,
                "第一章",
                "https://example.com/orphan",
                "",
                true,
                None,
            )
            .await
            .unwrap();
        service
            .save_chapter_list_cache(
                "u",
                &orphan_id,
                &vec![BookChapter {
                    url: "http://up/ch/1".to_string(),
                    title: "第一章".to_string(),
                    index: 0,
                    ..Default::default()
                }],
            )
            .await
            .unwrap();

        service.purge_user_cache("u").await.unwrap();

        let repo = service.book_repo();
        // 登记行保留：正文历史依赖 book_id，清缓存不能把身份一起删掉
        assert!(repo
            .get_by_url("u", "https://example.com/onshelf")
            .await
            .unwrap()
            .is_some());
        assert!(repo
            .get_by_url("u", "https://example.com/orphan")
            .await
            .unwrap()
            .is_some());
        // 正文历史保留
        let latest = service
            .content()
            .latest(&orphan_id, "http://up/ch/1")
            .await
            .unwrap();
        assert!(latest.and_then(|row| row.latest_user_version_id).is_some());
        // 目录缓存被清掉
        assert!(!service.chapter_list_cache_exists("u", &orphan_id).await);

        pool.close().await;
        let _ = tokio::fs::remove_dir_all(&storage_dir).await;
    }
}
