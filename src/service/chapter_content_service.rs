//! 正文池与版本时间线的业务层。
//!
//! 只负责存储、去重、压缩、版本追加和指针切换；抓取上游由 `BookService`
//! 负责，避免这一层依赖解析器和 HTTP。

use crate::error::error::AppError;
use crate::storage::db::content_repo::{
    ChapterContentRepo, ChapterLatestRow, ChapterVersionRow, EffectiveContentBlob,
    EffectiveVersionRow, NewChapterVersion, RecordOutcome,
};
use crate::util::time::now_millis;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

/// 单章正文上限。异常书源返回超大正文时拒绝写入，避免把数据库写坏。
pub const MAX_CHAPTER_CONTENT_BYTES: usize = 16 * 1024 * 1024;

const CODEC_PLAIN: i64 = 0;
const CODEC_ZSTD: i64 = 1;

/// 一个可以返回给上层的版本及其正文。
#[derive(Debug, Clone)]
pub struct ChapterVersionView {
    pub version_id: i64,
    pub kind: String,
    pub base_version_id: Option<i64>,
    pub content: String,
    pub created_at: i64,
    pub book_source_name: String,
    pub chapter_index: i32,
    pub chapter_title: String,
    pub note: String,
    pub raw_size: i64,
}

/// 写入上游正文时需要的章节快照。`book_url` / `book_source_name` 只作数据。
pub struct UpstreamChapterMeta<'a> {
    pub book_id: &'a str,
    pub chapter_url: &'a str,
    pub book_source_id: Option<i64>,
    pub book_source_name: &'a str,
    pub chapter_index: i32,
    pub chapter_title: &'a str,
    pub book_url: &'a str,
    pub fetch_url: &'a str,
}

/// 批量导入时一章的输入；公共参数（kind / note / make_current 等）在方法参数上。
pub struct UserVersionImport<'a> {
    pub chapter_url: &'a str,
    pub content: &'a str,
    pub chapter_index: i32,
    pub chapter_title: &'a str,
}

#[derive(Clone)]
pub struct ChapterContentService {
    repo: ChapterContentRepo,
    revalidate_ttl_secs: u64,
}

impl ChapterContentService {
    pub fn new(repo: ChapterContentRepo, revalidate_ttl_secs: u64) -> Self {
        Self {
            repo,
            revalidate_ttl_secs,
        }
    }

    /// 集成测试与工具代码用：直接从一个连接池装配。
    pub fn from_pool(pool: sqlx::SqlitePool, revalidate_ttl_secs: u64) -> Self {
        Self::new(ChapterContentRepo::new(pool), revalidate_ttl_secs)
    }

    pub async fn latest(
        &self,
        book_id: &str,
        chapter_url: &str,
    ) -> Result<Option<ChapterLatestRow>, AppError> {
        self.repo.get_latest(book_id, chapter_url).await
    }

    /// 解析当前用户应显示的版本。`force_upstream` 只预览最新上游。
    pub async fn resolve(
        &self,
        user_ns: &str,
        book_id: &str,
        chapter_url: &str,
        force_upstream: bool,
    ) -> Result<Option<ChapterVersionView>, AppError> {
        let Some(row) = self
            .repo
            .resolve_version(user_ns, book_id, chapter_url, force_upstream)
            .await?
        else {
            return Ok(None);
        };
        let (codec, bytes) = self
            .repo
            .get_content_blob(row.content_id)
            .await?
            .ok_or_else(|| AppError::Internal(anyhow::anyhow!("正文池缺少 content_id")))?;
        Ok(Some(to_view(row, decode_content(codec, bytes)?)))
    }

    /// 最近一次保存的用户版本（编辑版或设为当前的导入版本）。
    pub async fn latest_user_version(
        &self,
        book_id: &str,
        chapter_url: &str,
    ) -> Result<Option<ChapterVersionView>, AppError> {
        let latest = self.repo.get_latest(book_id, chapter_url).await?;
        let Some(version_id) = latest.and_then(|row| row.latest_user_version_id) else {
            return Ok(None);
        };
        self.version(book_id, chapter_url, version_id).await
    }

    pub async fn version(
        &self,
        book_id: &str,
        chapter_url: &str,
        version_id: i64,
    ) -> Result<Option<ChapterVersionView>, AppError> {
        let Some(row) = self
            .repo
            .get_version(book_id, chapter_url, version_id)
            .await?
        else {
            return Ok(None);
        };
        let (codec, bytes) = self
            .repo
            .get_content_blob(row.content_id)
            .await?
            .ok_or_else(|| AppError::Internal(anyhow::anyhow!("正文池缺少 content_id")))?;
        Ok(Some(to_view(row, decode_content(codec, bytes)?)))
    }

    pub async fn load_content(&self, content_id: i64) -> Result<String, AppError> {
        let (codec, bytes) = self
            .repo
            .get_content_blob(content_id)
            .await?
            .ok_or_else(|| AppError::NotFound("正文内容不存在".to_string()))?;
        decode_content(codec, bytes)
    }

    /// 追加一份上游正文。内容与最近上游版本相同则只更新检查时间。
    pub async fn save_upstream(
        &self,
        meta: UpstreamChapterMeta<'_>,
        content: &str,
    ) -> Result<RecordOutcome, AppError> {
        let encoded = encode_content(content)?;
        self.repo
            .record_version(NewChapterVersion {
                user_ns: "",
                book_id: meta.book_id,
                chapter_url: meta.chapter_url,
                kind: "upstream",
                content_hash: &encoded.hash,
                codec: encoded.codec,
                raw_size: encoded.raw_size,
                stored_size: encoded.stored_size,
                content: &encoded.bytes,
                book_source_id: meta.book_source_id,
                book_source_name: meta.book_source_name,
                base_version_id: None,
                chapter_index: meta.chapter_index,
                chapter_title: meta.chapter_title,
                book_url: meta.book_url,
                fetch_url: meta.fetch_url,
                note: "",
                make_current: true,
                created_at: None,
            })
            .await
    }

    /// 追加一份用户版本（编辑版或手动导入）。
    #[allow(clippy::too_many_arguments)]
    pub async fn save_user_version(
        &self,
        user_ns: &str,
        book_id: &str,
        chapter_url: &str,
        kind: &str,
        content: &str,
        base_version_id: Option<i64>,
        chapter_index: i32,
        chapter_title: &str,
        book_url: &str,
        note: &str,
        make_current: bool,
        created_at: Option<i64>,
    ) -> Result<RecordOutcome, AppError> {
        if kind != "edited" && kind != "imported" {
            return Err(AppError::BadRequest(format!("未知的用户版本类型: {kind}")));
        }
        if note.len() > 2000 {
            return Err(AppError::BadRequest(
                "版本备注过长（最多 2000 字节）".to_string(),
            ));
        }
        let encoded = encode_content(content)?;
        self.repo
            .record_version(NewChapterVersion {
                user_ns,
                book_id,
                chapter_url,
                kind,
                content_hash: &encoded.hash,
                codec: encoded.codec,
                raw_size: encoded.raw_size,
                stored_size: encoded.stored_size,
                content: &encoded.bytes,
                book_source_id: None,
                book_source_name: "",
                base_version_id,
                chapter_index,
                chapter_title,
                book_url,
                fetch_url: "",
                note,
                make_current,
                created_at,
            })
            .await
    }

    /// 批量追加用户版本（整本 TXT 导入）：repo 侧分块事务 + 章级保存点，
    /// 单章失败（如超出单章上限）记入失败，不影响同块其他章节。
    ///
    /// 对外语义与逐章调 [`Self::save_user_version`] 一致；返回与 `chapters`
    /// 等长、按顺序对齐的逐章结果。
    #[allow(clippy::too_many_arguments)]
    pub async fn save_user_versions_batch(
        &self,
        user_ns: &str,
        book_id: &str,
        kind: &str,
        chapters: &[UserVersionImport<'_>],
        book_url: &str,
        note: &str,
        make_current: bool,
        created_at: Option<i64>,
    ) -> Vec<Result<RecordOutcome, AppError>> {
        if kind != "edited" && kind != "imported" {
            return chapters
                .iter()
                .map(|_| Err(AppError::BadRequest(format!("未知的用户版本类型: {kind}"))))
                .collect();
        }
        if note.len() > 2000 {
            return chapters
                .iter()
                .map(|_| {
                    Err(AppError::BadRequest(
                        "版本备注过长（最多 2000 字节）".to_string(),
                    ))
                })
                .collect();
        }
        // 先逐章编码：单章上限检查在这一步，编码失败的章节直接记失败、不进事务。
        let mut results: Vec<Option<Result<RecordOutcome, AppError>>> = Vec::new();
        results.resize_with(chapters.len(), || None);
        let mut encoded: Vec<Option<EncodedContent>> = Vec::with_capacity(chapters.len());
        for (index, chapter) in chapters.iter().enumerate() {
            match encode_content(chapter.content) {
                Ok(enc) => encoded.push(Some(enc)),
                Err(error) => {
                    encoded.push(None);
                    results[index] = Some(Err(error));
                }
            }
        }
        let mut indexes: Vec<usize> = Vec::new();
        let mut inputs: Vec<NewChapterVersion<'_>> = Vec::new();
        for (index, (chapter, enc)) in chapters.iter().zip(encoded.iter()).enumerate() {
            let Some(enc) = enc else { continue };
            indexes.push(index);
            inputs.push(NewChapterVersion {
                user_ns,
                book_id,
                chapter_url: chapter.chapter_url,
                kind,
                content_hash: &enc.hash,
                codec: enc.codec,
                raw_size: enc.raw_size,
                stored_size: enc.stored_size,
                content: &enc.bytes,
                book_source_id: None,
                book_source_name: "",
                base_version_id: None,
                chapter_index: chapter.chapter_index,
                chapter_title: chapter.chapter_title,
                book_url,
                fetch_url: "",
                note,
                make_current,
                created_at,
            });
        }
        let outcomes = self.repo.record_versions_batch(&inputs).await;
        for (pos, outcome) in outcomes.into_iter().enumerate() {
            results[indexes[pos]] = Some(outcome);
        }
        results
            .into_iter()
            .map(|slot| {
                slot.unwrap_or_else(|| {
                    Err(AppError::Internal(anyhow::anyhow!(
                        "批量写入未返回该章结果"
                    )))
                })
            })
            .collect()
    }

    pub async fn clear_current_user_version(
        &self,
        user_ns: &str,
        book_id: &str,
        chapter_url: &str,
    ) -> Result<(), AppError> {
        self.repo
            .clear_current_user_version(user_ns, book_id, chapter_url)
            .await
    }

    pub async fn select_version(
        &self,
        user_ns: &str,
        book_id: &str,
        chapter_url: &str,
        version_id: Option<i64>,
    ) -> Result<(), AppError> {
        self.repo
            .set_view_state(user_ns, book_id, chapter_url, version_id)
            .await
    }

    pub async fn selected_version_id(
        &self,
        user_ns: &str,
        book_id: &str,
        chapter_url: &str,
    ) -> Result<Option<i64>, AppError> {
        self.repo
            .selected_version_id(user_ns, book_id, chapter_url)
            .await
    }

    /// 整书换回最新上游：清掉本书全部版本选择与用户版本指针，每章显示
    /// 回落到最新上游；历史版本严格保留。返回摘掉指针的章节数。
    pub async fn reset_book_to_upstream(
        &self,
        user_ns: &str,
        book_id: &str,
    ) -> Result<u64, AppError> {
        self.repo.reset_book_to_upstream(user_ns, book_id).await
    }

    /// 时间线元数据（不读正文），历史面板列表用。
    pub async fn timeline_meta(
        &self,
        book_id: &str,
        chapter_url: &str,
        limit: i64,
        before_version_id: Option<i64>,
    ) -> Result<Vec<ChapterVersionRow>, AppError> {
        self.repo
            .list_timeline(book_id, chapter_url, limit, before_version_id)
            .await
    }

    pub async fn mark_failure(
        &self,
        book_id: &str,
        chapter_url: &str,
        error: &str,
    ) -> Result<(), AppError> {
        self.repo
            .touch_upstream_failure(book_id, chapter_url, error)
            .await
    }

    /// 是否该在后台回源。`0` 表示关闭自动回源。
    pub fn needs_revalidate(&self, latest: &ChapterLatestRow) -> bool {
        if self.revalidate_ttl_secs == 0 {
            return false;
        }
        let Some(last_attempt) = latest.last_attempt_at else {
            return true;
        };
        let age = now_millis().saturating_sub(last_attempt);
        age >= (self.revalidate_ttl_secs as i64).saturating_mul(1000)
    }

    /// 返回给前端的旧数据标记：到期未回源，或最近一次回源失败。
    pub fn is_stale(&self, latest: &ChapterLatestRow) -> bool {
        self.needs_revalidate(latest) || !latest.last_error.is_empty()
    }

    pub async fn list_user_version_urls(
        &self,
        user_ns: &str,
        book_id: &str,
    ) -> Result<Vec<String>, AppError> {
        self.repo.list_user_version_urls(user_ns, book_id).await
    }

    pub async fn list_effective_versions(
        &self,
        user_ns: &str,
        book_id: &str,
    ) -> Result<Vec<EffectiveVersionRow>, AppError> {
        self.repo.list_effective_versions(user_ns, book_id).await
    }

    /// 一本书当前显示版本的归一化指纹，按章节地址索引；导入预览用来判断差异。
    ///
    /// 归一化口径与 diff 面板一致：剥 HTML 标签、逐行 trim、去掉空行。
    /// 这样只差 HTML 包裹或空行的「同一份正文」不会被算成内容变化。
    pub async fn effective_normalized_hashes(
        &self,
        user_ns: &str,
        book_id: &str,
    ) -> Result<HashMap<String, String>, AppError> {
        let rows: Vec<EffectiveContentBlob> = self
            .repo
            .list_effective_content_blobs(user_ns, book_id)
            .await?;
        let mut hashes = HashMap::with_capacity(rows.len());
        for row in rows {
            let content = decode_content(row.codec, row.content)?;
            hashes.insert(row.chapter_url, normalized_content_hash(&content));
        }
        Ok(hashes)
    }

    /// 图片源的空正文只更新检查时间，不追加空版本。
    pub async fn touch_empty_upstream(
        &self,
        book_id: &str,
        chapter_url: &str,
    ) -> Result<(), AppError> {
        self.repo.touch_empty_upstream(book_id, chapter_url).await
    }

    pub async fn has_content(&self, book_id: &str, chapter_url: &str) -> Result<bool, AppError> {
        self.repo.has_content(book_id, chapter_url).await
    }

    pub async fn count_chapters(&self, book_id: &str) -> Result<u64, AppError> {
        self.repo.count_chapters(book_id).await
    }

    pub async fn stats(&self) -> Result<(u64, u64, u64, u64), AppError> {
        self.repo.stats().await
    }
}

fn to_view(row: ChapterVersionRow, content: String) -> ChapterVersionView {
    ChapterVersionView {
        version_id: row.version_id,
        kind: row.kind,
        base_version_id: row.base_version_id,
        content,
        created_at: row.created_at,
        book_source_name: row.book_source_name,
        chapter_index: row.chapter_index,
        chapter_title: row.chapter_title,
        note: row.note,
        raw_size: row.raw_size,
    }
}

struct EncodedContent {
    codec: i64,
    raw_size: i64,
    stored_size: i64,
    bytes: Vec<u8>,
    hash: String,
}

/// 正文池的内容指纹：sha256(原始 UTF-8 字节)。
pub fn content_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
}

/// 导入预览用的归一化指纹：剥 HTML 标签、去掉空行、逐行 trim 后再 hash。
///
/// 与 diff 面板的切段口径一致，避免「正文一样、只是 HTML 或空行不同」
/// 被误判成有差异。
pub fn normalized_content_hash(content: &str) -> String {
    let paragraphs = crate::service::chapter_diff_service::split_to_paragraphs(content);
    content_hash(&paragraphs.join("\n"))
}

fn encode_content(content: &str) -> Result<EncodedContent, AppError> {
    if content.len() > MAX_CHAPTER_CONTENT_BYTES {
        return Err(AppError::BadRequest(format!(
            "章节内容超出上限（{}MB）",
            MAX_CHAPTER_CONTENT_BYTES / 1024 / 1024
        )));
    }
    let raw = content.as_bytes();
    let compressed = zstd::encode_all(raw, 3)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("正文压缩失败: {e}")))?;
    // 小正文压缩后反而更大，直接存明文。
    let (codec, bytes) = if compressed.len() < raw.len() {
        (CODEC_ZSTD, compressed)
    } else {
        (CODEC_PLAIN, raw.to_vec())
    };
    Ok(EncodedContent {
        codec,
        raw_size: raw.len() as i64,
        stored_size: bytes.len() as i64,
        bytes,
        hash: content_hash(content),
    })
}

fn decode_content(codec: i64, bytes: Vec<u8>) -> Result<String, AppError> {
    match codec {
        CODEC_PLAIN => String::from_utf8(bytes)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("正文不是合法 UTF-8: {e}"))),
        CODEC_ZSTD => {
            let raw = zstd::decode_all(bytes.as_slice())
                .map_err(|e| AppError::Internal(anyhow::anyhow!("正文解压失败: {e}")))?;
            String::from_utf8(raw)
                .map_err(|e| AppError::Internal(anyhow::anyhow!("正文不是合法 UTF-8: {e}")))
        }
        other => Err(AppError::Internal(anyhow::anyhow!(
            "未知的正文编码: {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compression_roundtrip_keeps_text() {
        let text = "第一章 很长很长的一段正文。".repeat(100);
        let encoded = encode_content(&text).unwrap();
        let decoded = decode_content(encoded.codec, encoded.bytes).unwrap();
        assert_eq!(decoded, text);
        assert_eq!(encoded.raw_size as usize, text.len());
    }

    #[test]
    fn small_content_stays_plain() {
        let encoded = encode_content("短").unwrap();
        assert_eq!(encoded.codec, CODEC_PLAIN);
        assert_eq!(encoded.stored_size, encoded.raw_size);
    }

    #[test]
    fn oversized_content_is_rejected() {
        let text = "x".repeat(MAX_CHAPTER_CONTENT_BYTES + 1);
        assert!(matches!(
            encode_content(&text),
            Err(AppError::BadRequest(_))
        ));
    }

    #[test]
    fn hash_is_stable() {
        let a = encode_content("同样的正文").unwrap();
        let b = encode_content("同样的正文").unwrap();
        assert_eq!(a.hash, b.hash);
    }

    async fn test_service() -> (ChapterContentService, sqlx::SqlitePool, std::path::PathBuf) {
        crate::util::test_temp::clean_stale_test_dirs();
        let dir =
            std::env::temp_dir().join(format!("reader-rust-content-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let pool = crate::storage::db::init_pool(&format!(
            "sqlite:{}?mode=rwc",
            dir.join("reader.db").display()
        ))
        .await
        .unwrap();
        let service = ChapterContentService::from_pool(pool.clone(), 24 * 3600);
        (service, pool, dir)
    }

    fn meta<'a>(book_id: &'a str, chapter_url: &'a str) -> UpstreamChapterMeta<'a> {
        UpstreamChapterMeta {
            book_id,
            chapter_url,
            book_source_id: None,
            book_source_name: "测试源",
            chapter_index: 0,
            chapter_title: "第一章",
            book_url: "https://example.test/book",
            fetch_url: chapter_url,
        }
    }

    fn version_id(outcome: &RecordOutcome) -> i64 {
        match outcome {
            RecordOutcome::Unchanged { version_id } => *version_id,
            RecordOutcome::Appended { version_id, .. } => *version_id,
        }
    }

    #[tokio::test]
    async fn content_pool_dedupes_and_timeline_keeps_every_version() {
        let (service, pool, dir) = test_service().await;
        let url = "https://up/ch/1";
        let first = service
            .save_upstream(meta("book_1", url), "第一版")
            .await
            .unwrap();
        assert!(matches!(
            first,
            RecordOutcome::Appended {
                became_current: true,
                ..
            }
        ));
        let first_id = version_id(&first);

        // 同样的正文再抓一次：不追加时间线
        let same = service
            .save_upstream(meta("book_1", url), "第一版")
            .await
            .unwrap();
        assert!(matches!(
            same,
            RecordOutcome::Unchanged { version_id } if version_id == first_id
        ));
        let (contents, versions, chapters, _) = service.stats().await.unwrap();
        assert_eq!((contents, versions, chapters), (1, 1, 1));

        // 上游变化：追加版本，旧版本仍在
        service
            .save_upstream(meta("book_1", url), "第二版")
            .await
            .unwrap();
        assert_eq!(service.stats().await.unwrap().1, 2);

        // 内容回退到第一版：时间线继续追加，正文池复用旧 blob
        let reverted = service
            .save_upstream(meta("book_1", url), "第一版")
            .await
            .unwrap();
        assert!(matches!(reverted, RecordOutcome::Appended { .. }));
        let (contents, versions, _, _) = service.stats().await.unwrap();
        assert_eq!(contents, 2, "相同正文只存一份");
        assert_eq!(versions, 3, "每次内容变化都追加时间线");

        let latest = service.latest("book_1", url).await.unwrap().unwrap();
        assert_eq!(
            latest.latest_upstream_version_id,
            Some(version_id(&reverted))
        );

        pool.close().await;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn user_version_is_default_and_selection_can_override_it() {
        let (service, pool, dir) = test_service().await;
        let url = "https://up/ch/1";
        let upstream = service
            .save_upstream(meta("book_1", url), "上游正文")
            .await
            .unwrap();
        let upstream_id = version_id(&upstream);
        let edit = service
            .save_user_version(
                "user_1",
                "book_1",
                url,
                "edited",
                "我的版本",
                Some(upstream_id),
                0,
                "第一章",
                "https://example.test/book",
                "",
                true,
                None,
            )
            .await
            .unwrap();
        let edit_id = version_id(&edit);

        let resolved = service
            .resolve("user_1", "book_1", url, false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resolved.version_id, edit_id, "没选过时用户版本优先");
        assert_eq!(resolved.content, "我的版本");

        // 手动选择上游版本；另一个用户不受影响
        service
            .select_version("user_1", "book_1", url, Some(upstream_id))
            .await
            .unwrap();
        let selected = service
            .resolve("user_1", "book_1", url, false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(selected.version_id, upstream_id);
        let other = service
            .resolve("user_2", "book_1", url, false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(other.version_id, edit_id, "版本选择按用户隔离");

        // 跟随最新：回到用户版本
        service
            .select_version("user_1", "book_1", url, None)
            .await
            .unwrap();
        assert_eq!(
            service
                .resolve("user_1", "book_1", url, false)
                .await
                .unwrap()
                .unwrap()
                .version_id,
            edit_id
        );

        // 删除当前用户版本：回落上游，历史仍可读
        service
            .clear_current_user_version("user_1", "book_1", url)
            .await
            .unwrap();
        assert_eq!(
            service
                .resolve("user_1", "book_1", url, false)
                .await
                .unwrap()
                .unwrap()
                .version_id,
            upstream_id
        );
        assert_eq!(
            service
                .version("book_1", url, edit_id)
                .await
                .unwrap()
                .unwrap()
                .content,
            "我的版本"
        );
        assert_eq!(service.stats().await.unwrap().1, 2, "历史不删");

        pool.close().await;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn history_only_import_does_not_become_current() {
        let (service, pool, dir) = test_service().await;
        let url = "https://up/ch/1";
        service
            .save_upstream(meta("book_1", url), "上游正文")
            .await
            .unwrap();
        let imported = service
            .save_user_version(
                "user_1",
                "book_1",
                url,
                "imported",
                "旧版存档",
                None,
                0,
                "",
                "https://example.test/book",
                "从旧站导入",
                false,
                Some(1_600_000_000_000),
            )
            .await
            .unwrap();
        assert!(matches!(
            imported,
            RecordOutcome::Appended {
                became_current: false,
                ..
            }
        ));
        let latest = service.latest("book_1", url).await.unwrap().unwrap();
        assert!(latest.latest_user_version_id.is_none());
        assert_eq!(
            service
                .resolve("user_1", "book_1", url, false)
                .await
                .unwrap()
                .unwrap()
                .content,
            "上游正文"
        );
        let timeline = service
            .timeline_meta("book_1", url, 10, None)
            .await
            .unwrap();
        assert_eq!(timeline.len(), 2);
        let imported_item = timeline
            .iter()
            .find(|row| row.kind == "imported")
            .expect("导入版本应在时间线里");
        assert_eq!(imported_item.created_at, 1_600_000_000_000);

        pool.close().await;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn revalidate_ttl_uses_last_attempt() {
        let (service, pool, dir) = test_service().await;
        let mut row = ChapterLatestRow::default();
        assert!(service.needs_revalidate(&row), "从未回源过应回源");

        row.last_attempt_at = Some(now_millis());
        assert!(!service.needs_revalidate(&row));

        row.last_attempt_at = Some(now_millis() - 25 * 3600 * 1000);
        assert!(service.needs_revalidate(&row));

        let disabled = ChapterContentService::from_pool(pool.clone(), 0);
        assert!(!disabled.needs_revalidate(&row), "0 表示关闭自动回源");

        pool.close().await;
        let _ = std::fs::remove_dir_all(dir);
    }

    fn import_chapter<'a>(
        chapter_url: &'a str,
        content: &'a str,
        chapter_index: i32,
    ) -> UserVersionImport<'a> {
        UserVersionImport {
            chapter_url,
            content,
            chapter_index,
            chapter_title: "",
        }
    }

    /// 批量写入：一章超限记失败，同块其他章节照常落库；同内容再导计 Unchanged。
    #[tokio::test]
    async fn batch_save_isolates_failures_and_detects_unchanged() {
        let (service, pool, dir) = test_service().await;
        service
            .save_upstream(meta("book_1", "https://up/ch/1"), "上游一")
            .await
            .unwrap();
        service
            .save_upstream(meta("book_1", "https://up/ch/2"), "上游二")
            .await
            .unwrap();

        let oversized = "x".repeat(MAX_CHAPTER_CONTENT_BYTES + 1);
        let chapters = [
            import_chapter("https://up/ch/1", "批量正文一", 0),
            import_chapter("https://up/ch/2", &oversized, 1),
            import_chapter("https://up/ch/3", "批量正文三", 2),
        ];
        let outcomes = service
            .save_user_versions_batch(
                "user_1",
                "book_1",
                "edited",
                &chapters,
                "https://example.test/book",
                "",
                true,
                None,
            )
            .await;
        assert_eq!(outcomes.len(), 3);
        assert!(matches!(
            outcomes[0],
            Ok(RecordOutcome::Appended {
                became_current: true,
                ..
            })
        ));
        assert!(
            matches!(&outcomes[1], Err(AppError::BadRequest(_))),
            "超限章节记失败: {:?}",
            outcomes[1]
        );
        assert!(matches!(outcomes[2], Ok(RecordOutcome::Appended { .. })));

        // 同块其他章节真实落库并成为当前版本；失败章节仍显示上游
        assert_eq!(
            service
                .resolve("user_1", "book_1", "https://up/ch/1", false)
                .await
                .unwrap()
                .unwrap()
                .content,
            "批量正文一"
        );
        assert_eq!(
            service
                .resolve("user_1", "book_1", "https://up/ch/2", false)
                .await
                .unwrap()
                .unwrap()
                .content,
            "上游二"
        );
        assert_eq!(
            service
                .resolve("user_1", "book_1", "https://up/ch/3", false)
                .await
                .unwrap()
                .unwrap()
                .content,
            "批量正文三"
        );

        // 同一份内容再导一次：Unchanged，不追加时间线
        let chapters = [import_chapter("https://up/ch/1", "批量正文一", 0)];
        let outcomes = service
            .save_user_versions_batch(
                "user_1",
                "book_1",
                "edited",
                &chapters,
                "https://example.test/book",
                "",
                true,
                None,
            )
            .await;
        assert!(matches!(outcomes[0], Ok(RecordOutcome::Unchanged { .. })));
        let (_, versions, _, _) = service.stats().await.unwrap();
        assert_eq!(versions, 4, "两条上游 + 两条用户版本，unchanged 不追加");

        pool.close().await;
        let _ = std::fs::remove_dir_all(dir);
    }

    /// repo 层：一章在事务内保存点之后失败（bookId 非法），ROLLBACK TO SAVEPOINT
    /// 撤销它，同块其他章节正常提交。
    #[tokio::test]
    async fn batch_rolls_back_failed_chapter_and_commits_the_rest() {
        let (service, pool, dir) = test_service().await;
        let enc_a = encode_content("批量甲").unwrap();
        let enc_b = encode_content("批量乙").unwrap();
        fn repo_input<'a>(
            book_id: &'a str,
            chapter_url: &'a str,
            enc: &'a EncodedContent,
        ) -> NewChapterVersion<'a> {
            NewChapterVersion {
                user_ns: "user_1",
                book_id,
                chapter_url,
                kind: "edited",
                content_hash: &enc.hash,
                codec: enc.codec,
                raw_size: enc.raw_size,
                stored_size: enc.stored_size,
                content: &enc.bytes,
                book_source_id: None,
                book_source_name: "",
                base_version_id: None,
                chapter_index: 0,
                chapter_title: "",
                book_url: "https://example.test/book",
                fetch_url: "",
                note: "",
                make_current: true,
                created_at: None,
            }
        }
        let inputs = [
            repo_input("book_1", "https://up/ch/1", &enc_a),
            // 非法 bookId：在保存点之后、写入之前失败，走 ROLLBACK TO SAVEPOINT
            repo_input("不是书籍", "https://up/ch/2", &enc_b),
            repo_input("book_1", "https://up/ch/3", &enc_b),
        ];
        let outcomes = service.repo.record_versions_batch(&inputs).await;
        assert_eq!(outcomes.len(), 3);
        assert!(matches!(outcomes[0], Ok(RecordOutcome::Appended { .. })));
        assert!(matches!(&outcomes[1], Err(AppError::BadRequest(_))));
        assert!(matches!(outcomes[2], Ok(RecordOutcome::Appended { .. })));
        assert!(service
            .has_content("book_1", "https://up/ch/1")
            .await
            .unwrap());
        assert!(service
            .has_content("book_1", "https://up/ch/3")
            .await
            .unwrap());
        assert!(!service
            .has_content("book_1", "https://up/ch/2")
            .await
            .unwrap());

        pool.close().await;
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 章节数跨块（> RECORD_VERSION_BATCH_SIZE）：分块提交，全部追加成功。
    #[tokio::test]
    async fn batch_spans_multiple_transaction_chunks() {
        let (service, pool, dir) = test_service().await;
        let total = crate::storage::db::content_repo::RECORD_VERSION_BATCH_SIZE + 5;
        let urls: Vec<String> = (0..total).map(|i| format!("https://up/ch/{i}")).collect();
        let contents: Vec<String> = (0..total).map(|i| format!("批量正文 {i}")).collect();
        let chapters: Vec<UserVersionImport> = (0..total)
            .map(|i| import_chapter(&urls[i], &contents[i], i as i32))
            .collect();
        let outcomes = service
            .save_user_versions_batch(
                "user_1",
                "book_1",
                "edited",
                &chapters,
                "https://example.test/book",
                "",
                true,
                None,
            )
            .await;
        assert_eq!(outcomes.len(), total);
        assert!(
            outcomes
                .iter()
                .all(|o| matches!(o, Ok(RecordOutcome::Appended { .. }))),
            "跨块后每章都应追加成功"
        );
        let (_, versions, chapter_count, _) = service.stats().await.unwrap();
        assert_eq!(versions as usize, total);
        assert_eq!(chapter_count as usize, total);

        pool.close().await;
        let _ = std::fs::remove_dir_all(dir);
    }
}
