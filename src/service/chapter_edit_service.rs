//! 章节正文编辑版：用户数据目录下的叠加层。
//!
//! 布局：`data/<ns>/edited_chapters/<bookId>/<md5(chapterUrl)>.txt` 存整章编辑后正文，
//! 同目录 `_index.json` 是清单（chapterUrl → 章号/标题/时间），供目录「已编辑」徽章
//! 与前端预加载绕过用。
//!
//! 落在 `data/` 而不是 `cache/`：编辑版是用户资产，purgeCache 只清 cache 子树，
//! 刷新上游（refresh=1）也只动缓存基准，两边互不干扰。

use crate::error::error::AppError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::fs;

/// 单章编辑内容上限：正常章节 << 100KB，2MB 足够盖住富文本长章又挡住滥写。
const MAX_EDITED_CHAPTER_BYTES: usize = 2 * 1024 * 1024;

const EDITED_DIR: &str = "edited_chapters";
const INDEX_FILE: &str = "_index.json";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct ChapterEditMeta {
    pub chapter_index: i32,
    pub chapter_title: String,
    pub edited_at: i64,
}

type EditIndex = HashMap<String, ChapterEditMeta>;

#[derive(Clone)]
pub struct ChapterEditService {
    storage_dir: PathBuf,
}

impl ChapterEditService {
    pub fn new(storage_dir: &str) -> Self {
        Self {
            storage_dir: PathBuf::from(storage_dir),
        }
    }

    fn book_dir(&self, user_ns: &str, book_id: &str) -> PathBuf {
        self.storage_dir
            .join("data")
            .join(user_ns)
            .join(EDITED_DIR)
            .join(book_id)
    }

    fn chapter_path(&self, user_ns: &str, book_id: &str, chapter_url: &str) -> PathBuf {
        self.book_dir(user_ns, book_id)
            .join(format!("{}.txt", crate::util::hash::md5_hex(chapter_url)))
    }

    fn index_path(&self, user_ns: &str, book_id: &str) -> PathBuf {
        self.book_dir(user_ns, book_id).join(INDEX_FILE)
    }

    async fn read_index(&self, user_ns: &str, book_id: &str) -> EditIndex {
        let Ok(data) = fs::read_to_string(self.index_path(user_ns, book_id)).await else {
            return HashMap::new();
        };
        serde_json::from_str(&data).unwrap_or_default()
    }

    async fn write_index(
        &self,
        user_ns: &str,
        book_id: &str,
        index: &EditIndex,
    ) -> Result<(), AppError> {
        let path = self.index_path(user_ns, book_id);
        let data = serde_json::to_string_pretty(index).map_err(|e| AppError::Internal(e.into()))?;
        write_atomic(&path, data.as_bytes()).await
    }

    /// 保存整章编辑版。空内容合法（用户把整章删空也是编辑意图）。
    pub async fn save_chapter_edit(
        &self,
        user_ns: &str,
        book_id: &str,
        chapter_url: &str,
        chapter_index: i32,
        chapter_title: &str,
        content: &str,
    ) -> Result<(), AppError> {
        if content.len() > MAX_EDITED_CHAPTER_BYTES {
            return Err(AppError::BadRequest(format!(
                "章节内容超出上限（{}MB）",
                MAX_EDITED_CHAPTER_BYTES / 1024 / 1024
            )));
        }
        let dir = self.book_dir(user_ns, book_id);
        fs::create_dir_all(&dir)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
        write_atomic(
            &self.chapter_path(user_ns, book_id, chapter_url),
            content.as_bytes(),
        )
        .await?;
        let mut index = self.read_index(user_ns, book_id).await;
        index.insert(
            chapter_url.to_string(),
            ChapterEditMeta {
                chapter_index,
                chapter_title: chapter_title.to_string(),
                edited_at: crate::util::time::now_millis(),
            },
        );
        self.write_index(user_ns, book_id, &index).await
    }

    /// 读编辑版；没有编辑过返回 None。
    pub async fn get_chapter_edit(
        &self,
        user_ns: &str,
        book_id: &str,
        chapter_url: &str,
    ) -> Result<Option<String>, AppError> {
        match fs::read_to_string(self.chapter_path(user_ns, book_id, chapter_url)).await {
            Ok(content) => Ok(Some(content)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(AppError::Internal(e.into())),
        }
    }

    /// 按书列出已编辑章节的 chapterUrl 集（目录徽章用）。
    pub async fn list_chapter_edits(&self, user_ns: &str, book_id: &str) -> Vec<String> {
        self.read_index(user_ns, book_id)
            .await
            .keys()
            .cloned()
            .collect()
    }

    /// 删除单章编辑版（恢复上游版本）。
    pub async fn delete_chapter_edit(
        &self,
        user_ns: &str,
        book_id: &str,
        chapter_url: &str,
    ) -> Result<(), AppError> {
        let _ = fs::remove_file(self.chapter_path(user_ns, book_id, chapter_url)).await;
        let mut index = self.read_index(user_ns, book_id).await;
        index.remove(chapter_url);
        self.write_index(user_ns, book_id, &index).await
    }

    /// 删书级联：整书编辑版目录连带删除。
    pub async fn delete_book_edits(&self, user_ns: &str, book_id: &str) {
        let _ = fs::remove_dir_all(self.book_dir(user_ns, book_id)).await;
    }
}

/// tmp + rename 写入：防半截文件（与 FileCache 同一手法）。
async fn write_atomic(path: &PathBuf, bytes: &[u8]) -> Result<(), AppError> {
    let tmp = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        crate::util::crypto::random_string(6)
    ));
    fs::write(&tmp, bytes)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    fs::rename(&tmp, path)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_service() -> (ChapterEditService, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "reader-rust-edit-test-{}-{}",
            std::process::id(),
            crate::util::crypto::random_string(6)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let service = ChapterEditService::new(dir.to_str().unwrap());
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
        assert!(service.list_chapter_edits(ns, "book_1").await.is_empty());

        service
            .save_chapter_edit(ns, "book_1", "http://a/1", 0, "第一章", "改后的正文")
            .await
            .unwrap();
        service
            .save_chapter_edit(ns, "book_1", "http://a/2", 1, "第二章", "")
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
        let mut urls = service.list_chapter_edits(ns, "book_1").await;
        urls.sort();
        assert_eq!(urls, vec!["http://a/1", "http://a/2"]);
        // 别的书互不可见
        assert!(service.list_chapter_edits(ns, "book_2").await.is_empty());

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
            service.list_chapter_edits(ns, "book_1").await,
            vec!["http://a/2"]
        );

        // 覆盖写：同章再编辑是替换不是追加
        service
            .save_chapter_edit(ns, "book_1", "http://a/2", 1, "第二章", "第二版")
            .await
            .unwrap();
        assert_eq!(
            service
                .get_chapter_edit(ns, "book_1", "http://a/2")
                .await
                .unwrap(),
            Some("第二版".to_string())
        );

        // 删书级联
        service.delete_book_edits(ns, "book_1").await;
        assert!(service.list_chapter_edits(ns, "book_1").await.is_empty());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn oversized_content_is_rejected() {
        let (service, dir) = test_service().await;
        let big = "x".repeat(MAX_EDITED_CHAPTER_BYTES + 1);
        let result = service
            .save_chapter_edit("user_1", "book_1", "http://a/1", 0, "章", &big)
            .await;
        assert!(matches!(result, Err(AppError::BadRequest(_))));
        let _ = std::fs::remove_dir_all(dir);
    }
}
