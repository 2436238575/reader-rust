//! 章节正文编辑版：用户数据目录下的叠加层。
//!
//! 布局：正文存 `data/<ns>/edited_chapters/<bookId>/<md5(chapterUrl)>.txt`，
//! 元数据（章号/标题/时间）进 `chapter_edits` 表——清单查询与删书级联走 SQL，
//! 不再有 `_index.json` 整文档读-改-写（并发互相覆盖、删未编辑过的书 500）。
//!
//! 落在 `data/` 而不是 `cache/`：编辑版是用户资产，purgeCache 只清 cache 子树，
//! 刷新上游（refresh=1）也只动缓存基准，两边互不干扰。

use crate::error::error::AppError;
use crate::storage::db::repo::{ChapterEditMeta, ChapterEditRepo};
use std::path::PathBuf;
use tokio::fs;

/// 单章编辑内容上限：正常章节 << 100KB，2MB 足够盖住富文本长章又挡住滥写。
const MAX_EDITED_CHAPTER_BYTES: usize = 2 * 1024 * 1024;

const EDITED_DIR: &str = "edited_chapters";

#[derive(Clone)]
pub struct ChapterEditService {
    storage_dir: PathBuf,
    repo: ChapterEditRepo,
}

impl ChapterEditService {
    pub fn new(storage_dir: &str, repo: ChapterEditRepo) -> Self {
        Self {
            storage_dir: PathBuf::from(storage_dir),
            repo,
        }
    }

    /// bookId 在这里再过一道格式校验（纵深防御）：必须是 `book_<n>` 的数字 id，
    /// 再由 resolve_within 词法限定在 data/<ns>/edited_chapters/ 之内——
    /// handler 层的 resolve_book 是第一道防线，这层兜底防未来新调用点。
    fn book_dir(&self, user_ns: &str, book_id: &str) -> Result<PathBuf, AppError> {
        let id = crate::storage::db::repo::parse_book_id(book_id)
            .ok_or_else(|| AppError::BadRequest(format!("bookId 格式非法: {book_id:?}")))?;
        let base = self.storage_dir.join("data").join(user_ns).join(EDITED_DIR);
        crate::util::safe_path::resolve_within(&base, std::path::Path::new(&format!("book_{id}")))
            .ok_or_else(|| AppError::BadRequest(format!("bookId 越界: {book_id:?}")))
    }

    fn chapter_path(
        &self,
        user_ns: &str,
        book_id: &str,
        chapter_url: &str,
    ) -> Result<PathBuf, AppError> {
        Ok(self
            .book_dir(user_ns, book_id)?
            .join(format!("{}.txt", crate::util::hash::md5_hex(chapter_url))))
    }

    /// 保存整章编辑版。空内容合法（用户把整章删空也是编辑意图）。
    /// 顺序是「写临时文件 → 落索引 → 改名就位」：索引写失败时旧正文原样
    /// 保留（旧版先覆盖正文再 upsert，upsert 一失败就把刚覆盖的文件删掉，
    /// 二次编辑的旧版就此永久丢失）。
    pub async fn save_chapter_edit(
        &self,
        user_ns: &str,
        book_id: &str,
        chapter_url: &str,
        chapter_index: i32,
        chapter_title: &str,
        content: &str,
        book_url: &str,
    ) -> Result<(), AppError> {
        if content.len() > MAX_EDITED_CHAPTER_BYTES {
            return Err(AppError::BadRequest(format!(
                "章节内容超出上限（{}MB）",
                MAX_EDITED_CHAPTER_BYTES / 1024 / 1024
            )));
        }
        let dir = self.book_dir(user_ns, book_id)?;
        fs::create_dir_all(&dir)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
        let path = self.chapter_path(user_ns, book_id, chapter_url)?;
        let tmp = path.with_extension(format!(
            "tmp-{}-{}",
            std::process::id(),
            crate::util::crypto::random_string(6)
        ));
        fs::write(&tmp, content.as_bytes())
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
        if let Err(err) = self
            .repo
            .upsert(
                user_ns,
                book_id,
                chapter_url,
                &ChapterEditMeta {
                    chapter_index,
                    chapter_title: chapter_title.to_string(),
                    edited_at: crate::util::time::now_millis(),
                },
                book_url,
            )
            .await
        {
            let _ = fs::remove_file(&tmp).await;
            return Err(err);
        }
        if let Err(e) = fs::rename(&tmp, &path).await {
            // 索引已落、正文没换上：索引指向的旧正文仍是合法编辑版，
            // 下次保存自愈；清掉 tmp 不留孤儿
            let _ = fs::remove_file(&tmp).await;
            return Err(AppError::Internal(e.into()));
        }
        Ok(())
    }

    /// 读编辑版正文；没有编辑过返回 None。
    pub async fn get_chapter_edit(
        &self,
        user_ns: &str,
        book_id: &str,
        chapter_url: &str,
    ) -> Result<Option<String>, AppError> {
        match fs::read_to_string(self.chapter_path(user_ns, book_id, chapter_url)?).await {
            Ok(content) => Ok(Some(content)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(AppError::Internal(e.into())),
        }
    }

    /// 限量读编辑版正文（全文检索的字节预算用）：最多 max_bytes，
    /// 返回 (内容, 文件是否还有剩余)；没有编辑过返回 None。
    pub async fn get_chapter_edit_head(
        &self,
        user_ns: &str,
        book_id: &str,
        chapter_url: &str,
        max_bytes: u64,
    ) -> Result<Option<(String, bool)>, AppError> {
        let path = self.chapter_path(user_ns, book_id, chapter_url)?;
        crate::storage::cache::file_cache::read_head_limited(&path, max_bytes)
            .await
            .map_err(|e| AppError::Internal(e.into()))
    }

    /// 按书列出已编辑章节的 chapterUrl 集（目录徽章用）。
    pub async fn list_chapter_edits(
        &self,
        user_ns: &str,
        book_id: &str,
    ) -> Result<Vec<String>, AppError> {
        self.repo.list_urls(user_ns, book_id).await
    }

    /// 该用户全部编辑索引的 (bookId, bookUrl) 去重清单（WebDAV 恢复校验用）。
    pub async fn list_book_refs(&self, user_ns: &str) -> Result<Vec<(String, String)>, AppError> {
        self.repo.list_book_refs(user_ns).await
    }

    /// 删除单章编辑版（恢复上游版本）：先删正文文件（不存在不算错），再删索引行。
    pub async fn delete_chapter_edit(
        &self,
        user_ns: &str,
        book_id: &str,
        chapter_url: &str,
    ) -> Result<(), AppError> {
        let path = self.chapter_path(user_ns, book_id, chapter_url)?;
        match fs::remove_file(&path).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(AppError::Internal(e.into())),
        }
        self.repo.delete(user_ns, book_id, chapter_url).await
    }

    /// 删书级联：索引一句 DELETE，目录整个清掉。非法 bookId 直接 no-op。
    pub async fn delete_book_edits(&self, user_ns: &str, book_id: &str) {
        let Ok(dir) = self.book_dir(user_ns, book_id) else {
            return;
        };
        let _ = self.repo.delete_for_book(user_ns, book_id).await;
        let _ = fs::remove_dir_all(dir).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db::repo::ChapterEditRepo;

    async fn test_service() -> (ChapterEditService, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "reader-rust-edit-test-{}-{}",
            std::process::id(),
            crate::util::crypto::random_string(6)
        ));
        // init_pool 不建父目录，先落目录再开库
        std::fs::create_dir_all(&dir).unwrap();
        let url = format!("sqlite:{}?mode=rwc", dir.join("reader.db").display());
        let pool = crate::storage::db::init_pool(&url).await.unwrap();
        let service = ChapterEditService::new(dir.to_str().unwrap(), ChapterEditRepo::new(pool));
        (service, dir)
    }

    #[tokio::test]
    async fn save_get_list_delete_roundtrip() {
        let (service, dir) = test_service().await;
        let ns = "user_1";

        assert_eq!(
            service
                .get_chapter_edit(ns, "book_1", "http://a/1")
                .await
                .unwrap(),
            None
        );
        assert!(service
            .list_chapter_edits(ns, "book_1")
            .await
            .unwrap()
            .is_empty());

        service
            .save_chapter_edit(ns, "book_1", "http://a/1", 0, "第一章", "改后的正文", "https://example.com/b")
            .await
            .unwrap();
        service
            .save_chapter_edit(ns, "book_1", "http://a/2", 1, "第二章", "", "https://example.com/b")
            .await
            .unwrap();

        assert_eq!(
            service
                .get_chapter_edit(ns, "book_1", "http://a/1")
                .await
                .unwrap(),
            Some("改后的正文".to_string())
        );
        // 空内容也是合法编辑版
        assert_eq!(
            service
                .get_chapter_edit(ns, "book_1", "http://a/2")
                .await
                .unwrap(),
            Some(String::new())
        );
        let mut urls = service.list_chapter_edits(ns, "book_1").await.unwrap();
        urls.sort();
        assert_eq!(urls, vec!["http://a/1", "http://a/2"]);
        // 别的书互不可见
        assert!(service
            .list_chapter_edits(ns, "book_2")
            .await
            .unwrap()
            .is_empty());

        service
            .delete_chapter_edit(ns, "book_1", "http://a/1")
            .await
            .unwrap();
        assert_eq!(
            service
                .get_chapter_edit(ns, "book_1", "http://a/1")
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            service.list_chapter_edits(ns, "book_1").await.unwrap(),
            vec!["http://a/2"]
        );

        // 覆盖写：同章再编辑是替换不是追加
        service
            .save_chapter_edit(ns, "book_1", "http://a/2", 1, "第二章", "第二版", "https://example.com/b")
            .await
            .unwrap();
        assert_eq!(
            service
                .get_chapter_edit(ns, "book_1", "http://a/2")
                .await
                .unwrap(),
            Some("第二版".to_string())
        );

        // 删除从未编辑过的书：no-op 而不是 500
        service.delete_book_edits(ns, "book_99").await;
        service
            .delete_chapter_edit(ns, "book_99", "http://a/none")
            .await
            .unwrap();

        // 删书级联
        service.delete_book_edits(ns, "book_1").await;
        assert!(service
            .list_chapter_edits(ns, "book_1")
            .await
            .unwrap()
            .is_empty());

        let _ = std::fs::remove_dir_all(dir);
    }

    /// M1 回归：两章并发保存，索引（表）不丢条目。
    #[tokio::test]
    async fn concurrent_saves_keep_all_index_entries() {
        let (service, dir) = test_service().await;
        let ns = "user_1";
        let mut handles = Vec::new();
        for i in 0..8 {
            let service = service.clone();
            handles.push(tokio::spawn(async move {
                service
                    .save_chapter_edit(
                        ns,
                        "book_1",
                        &format!("http://a/{i}"),
                        i,
                        &format!("第{i}章"),
                        "正文",
                        "https://example.com/b",
                    )
                    .await
                    .unwrap();
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }
        assert_eq!(
            service
                .list_chapter_edits(ns, "book_1")
                .await
                .unwrap()
                .len(),
            8
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn oversized_content_is_rejected() {
        let (service, dir) = test_service().await;
        let big = "x".repeat(MAX_EDITED_CHAPTER_BYTES + 1);
        let result = service
            .save_chapter_edit("user_1", "book_1", "http://a/1", 0, "章", &big, "https://example.com/b")
            .await;
        assert!(matches!(result, Err(AppError::BadRequest(_))));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 索引写失败时旧编辑版必须原样保住（旧版先覆盖正文再 upsert，
    /// 失败回滚把新文件删掉，二次编辑的旧版就此永久丢失）。
    #[tokio::test]
    async fn failed_index_upsert_keeps_the_previous_edition() {
        let (service, dir) = test_service().await;
        let ns = "user_1";
        service
            .save_chapter_edit(ns, "book_1", "http://a/1", 0, "章", "旧版正文", "https://example.com/b")
            .await
            .unwrap();

        // 模拟索引写失败：把表 drop 掉，upsert 必炸
        let pool2 = sqlx::SqlitePool::connect(&format!(
            "sqlite:{}",
            dir.join("reader.db").display()
        ))
        .await
        .unwrap();
        sqlx::query("DROP TABLE chapter_edits")
            .execute(&pool2)
            .await
            .unwrap();

        let result = service
            .save_chapter_edit(ns, "book_1", "http://a/1", 0, "章", "新版正文", "https://example.com/b")
            .await;
        assert!(result.is_err(), "索引失败应报错");
        assert_eq!(
            service
                .get_chapter_edit(ns, "book_1", "http://a/1")
                .await
                .unwrap(),
            Some("旧版正文".to_string()),
            "索引失败时旧编辑版不能被覆盖或删除"
        );
        // tmp 文件也清干净，不留孤儿
        let edit_dir = dir.join("data").join(ns).join(EDITED_DIR).join("book_1");
        let leftovers = std::fs::read_dir(&edit_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("tmp"))
            .count();
        assert_eq!(leftovers, 0, "失败的保存不该留下 tmp 文件");

        let _ = std::fs::remove_dir_all(dir);
    }
}
