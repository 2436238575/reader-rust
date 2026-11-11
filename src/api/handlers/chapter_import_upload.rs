//! 整本 TXT 导入的自适应分片上传暂存。
//!
//! Cloudflare / OpenResty / WAF 可能对单次请求体设限（nginx 默认只有 1m），
//! 几 MB 的整本 TXT 一次性上传会被重置或截断。前端把文件拆成多个小请求
//! （失败就减半、成功后缓慢放大），这里按偏移落盘并校验完整性；预览与导入
//! 随后用 `uploadId` 取回拼好的文件，和直接上传 `file` 字段走同一条解析链路。

use crate::api::handlers::multipart::{next_field_limited, read_limited_multipart_field};
use crate::api::AppState;
use crate::auth::CurrentUser;
use crate::error::error::{ApiResponse, AppError};
use crate::service::local_txt_book::MAX_TXT_UPLOAD_BYTES;
use crate::util::keyed_mutex::KeyedMutex;
use axum::{
    extract::{Multipart, State},
    Json,
};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::fs::{self, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

/// 单个分片请求的正文上限。前端自适应区间是 128KB–8MB，这里放到 8MB。
pub const MAX_IMPORT_CHUNK_BYTES: usize = 8 * 1024 * 1024;
/// 暂存文件存活时间；过期条目在下次新上传时顺带清理。
const UPLOAD_TTL: Duration = Duration::from_secs(60 * 60);
/// 每个用户最多保留的暂存份数（每份最多 50MB）。
const MAX_UPLOADS_PER_USER: usize = 3;
/// `uploadId` 的长度（`random_string` 产出的大小写字母 + 数字）。
const UPLOAD_ID_LEN: usize = 32;

/// 同一 uploadId 的分片串行写入：并发请求各自判断偏移会互相覆盖。
static UPLOAD_LOCKS: Lazy<KeyedMutex> = Lazy::new(|| KeyedMutex::new("chapter-import-upload", 64));

#[derive(Debug, Serialize, Deserialize)]
struct UploadMeta {
    user_ns: String,
    total: u64,
    created_at: u64,
}

#[derive(Debug, Default)]
struct ChunkRequest {
    upload_id: Option<String>,
    offset: Option<u64>,
    file_size: Option<u64>,
    chunk: Option<Vec<u8>>,
}

fn upload_root(state: &AppState) -> PathBuf {
    Path::new(&state.config.storage_dir)
        .join("tmp")
        .join("chapter_import")
}

fn user_dir(state: &AppState, user_ns: &str) -> PathBuf {
    upload_root(state).join(user_ns)
}

fn chunk_path(dir: &Path, upload_id: &str) -> PathBuf {
    dir.join(format!("{upload_id}.bin"))
}

fn meta_path(dir: &Path, upload_id: &str) -> PathBuf {
    dir.join(format!("{upload_id}.json"))
}

fn valid_upload_id(value: &str) -> bool {
    value.len() == UPLOAD_ID_LEN && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

fn stale_upload_error() -> AppError {
    AppError::BadRequest("上传不存在或已过期，请重新上传".to_string())
}

async fn read_meta(dir: &Path, upload_id: &str) -> Result<UploadMeta, AppError> {
    let raw = fs::read(meta_path(dir, upload_id))
        .await
        .map_err(|_| stale_upload_error())?;
    serde_json::from_slice(&raw).map_err(|_| stale_upload_error())
}

async fn write_meta(dir: &Path, upload_id: &str, meta: &UploadMeta) -> Result<(), AppError> {
    let raw = serde_json::to_vec(meta)
        .map_err(|error| AppError::Internal(anyhow::anyhow!("上传状态序列化失败: {error}")))?;
    fs::write(meta_path(dir, upload_id), raw)
        .await
        .map_err(|error| AppError::Internal(anyhow::anyhow!("上传状态写入失败: {error}")))
}

/// 清掉过期暂存，并保证新上传落盘前最多只剩 `MAX_UPLOADS_PER_USER - 1` 份。
async fn sweep_user_uploads(dir: &Path) {
    let Ok(mut entries) = fs::read_dir(dir).await else {
        return;
    };
    let now = now_millis();
    let mut alive: Vec<(u64, String)> = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some((stem, extension)) = name.rsplit_once('.') else {
            continue;
        };
        if extension != "json" {
            continue;
        }
        let created_at = fs::read(&path)
            .await
            .ok()
            .and_then(|raw| serde_json::from_slice::<UploadMeta>(&raw).ok())
            .map(|meta| meta.created_at);
        let expired = created_at
            .map(|created_at| now.saturating_sub(created_at) > UPLOAD_TTL.as_millis() as u64)
            .unwrap_or(true);
        if expired {
            let _ = fs::remove_file(&path).await;
            let _ = fs::remove_file(chunk_path(dir, stem)).await;
        } else {
            alive.push((created_at.unwrap_or_default(), stem.to_string()));
        }
    }
    alive.sort_by_key(|(created_at, _)| *created_at);
    while alive.len() > MAX_UPLOADS_PER_USER.saturating_sub(1) {
        let (_, stem) = alive.remove(0);
        let _ = fs::remove_file(meta_path(dir, &stem)).await;
        let _ = fs::remove_file(chunk_path(dir, &stem)).await;
    }
}

async fn read_chunk_request(mut multipart: Multipart) -> Result<ChunkRequest, AppError> {
    let mut req = ChunkRequest::default();
    let mut count = 0usize;
    while let Some(field) = next_field_limited(&mut multipart, &mut count).await? {
        let name = field.name().unwrap_or_default().to_string();
        if name == "chunk" {
            req.chunk = Some(
                read_limited_multipart_field(field, MAX_IMPORT_CHUNK_BYTES, "单个分片不能超过 8MB")
                    .await?,
            );
            continue;
        }
        let limit = match name.as_str() {
            "fileSize" | "offset" => 32,
            "uploadId" => 64,
            _ => 128,
        };
        let bytes = read_limited_multipart_field(field, limit, "上传参数过长").await?;
        let value = String::from_utf8_lossy(&bytes).trim().to_string();
        match name.as_str() {
            "uploadId" => req.upload_id = Some(value),
            "offset" => {
                req.offset = Some(
                    value
                        .parse()
                        .map_err(|_| AppError::BadRequest("offset 无效".to_string()))?,
                )
            }
            "fileSize" => {
                req.file_size = Some(
                    value
                        .parse()
                        .map_err(|_| AppError::BadRequest("fileSize 无效".to_string()))?,
                )
            }
            _ => {}
        }
    }
    Ok(req)
}

/// 写入一个分片；返回（当前已收到的总字节数，文件应有的大小）。
///
/// 支持「响应丢失后客户端重发同一片」：偏移小于已写入长度时先比较重叠区，
/// 内容一致就当作成功，避免重试把文件写重。
async fn write_chunk(
    dir: &Path,
    user_ns: &str,
    upload_id: &str,
    offset: u64,
    chunk: &[u8],
    declared_total: Option<u64>,
) -> Result<(u64, u64), AppError> {
    let meta = read_meta(dir, upload_id).await?;
    if meta.user_ns != user_ns {
        // 不区分「不存在」和「不属于你」，避免探测别人的 uploadId
        return Err(stale_upload_error());
    }
    if let Some(total) = declared_total {
        if total != meta.total {
            return Err(AppError::BadRequest(
                "分片文件大小与首次上传不一致".to_string(),
            ));
        }
    }
    let end = offset
        .checked_add(chunk.len() as u64)
        .ok_or_else(|| AppError::BadRequest("offset 无效".to_string()))?;
    if end > meta.total {
        return Err(AppError::BadRequest("分片超出文件大小".to_string()));
    }

    let path = chunk_path(dir, upload_id);
    let current = fs::metadata(&path)
        .await
        .map(|meta| meta.len())
        .unwrap_or(0);
    if current > meta.total {
        return Err(AppError::BadRequest("上传状态损坏，请重新上传".to_string()));
    }
    if offset > current {
        return Err(AppError::Conflict(format!(
            "分片顺序错误：已收到 {current} 字节，当前偏移 {offset}"
        )));
    }
    if offset < current {
        let overlap_end = end.min(current);
        let overlap_len = (overlap_end - offset) as usize;
        if overlap_len > 0 {
            let mut file = OpenOptions::new()
                .read(true)
                .open(&path)
                .await
                .map_err(|error| AppError::Internal(anyhow::anyhow!("读取分片失败: {error}")))?;
            file.seek(SeekFrom::Start(offset))
                .await
                .map_err(|error| AppError::Internal(anyhow::anyhow!("定位分片失败: {error}")))?;
            let mut existing = vec![0u8; overlap_len];
            file.read_exact(&mut existing)
                .await
                .map_err(|error| AppError::Internal(anyhow::anyhow!("读取分片失败: {error}")))?;
            if existing != chunk[..overlap_len] {
                return Err(AppError::Conflict("分片内容冲突，请重新上传".to_string()));
            }
        }
        if current >= end {
            // 这一片之前已经完整写入（典型是响应丢失后的重试）
            return Ok((current, meta.total));
        }
    }

    let write_offset = current.max(offset);
    let write_from = (write_offset - offset) as usize;
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        // 按偏移写，不能截断已有分片
        .truncate(false)
        .open(&path)
        .await
        .map_err(|error| AppError::Internal(anyhow::anyhow!("写入分片失败: {error}")))?;
    file.seek(SeekFrom::Start(write_offset))
        .await
        .map_err(|error| AppError::Internal(anyhow::anyhow!("定位分片失败: {error}")))?;
    file.write_all(&chunk[write_from..])
        .await
        .map_err(|error| AppError::Internal(anyhow::anyhow!("写入分片失败: {error}")))?;
    file.flush()
        .await
        .map_err(|error| AppError::Internal(anyhow::anyhow!("写入分片失败: {error}")))?;
    Ok((end, meta.total))
}

/// `POST /reader3/uploadChapterImportChunk`
///
/// 首片不带 `uploadId`（带 `fileSize`、`offset=0`），响应里的 `uploadId`
/// 供后续分片使用；每片都返回当前已收到的字节数，`complete=true` 表示
/// 文件已拼完整。分片按顺序写，允许同一片重发（响应丢失后的幂等重试）。
pub async fn upload_chapter_import_chunk(
    State(state): State<AppState>,
    user: CurrentUser,
    multipart: Multipart,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let payload = run_import_chunk_upload(&state, &user.0.ns, multipart).await?;
    Ok(Json(ApiResponse::ok(
        serde_json::to_value(payload).unwrap_or_default(),
    )))
}

/// 分片上传结果（typed；线缆与 /reader3 逐字段一致）。
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChunkUploadPayload {
    pub upload_id: String,
    /// 服务端当前已收到的字节数（重发同一片时可能大于本次分片末尾）。
    pub received: u64,
    pub complete: bool,
}

/// 分片上传核心（v1 与 /reader3 共用）。
pub(crate) async fn run_import_chunk_upload(
    state: &AppState,
    user_ns: &str,
    multipart: Multipart,
) -> Result<ChunkUploadPayload, AppError> {
    let req = read_chunk_request(multipart).await?;
    let chunk = req
        .chunk
        .ok_or_else(|| AppError::BadRequest("chunk required".to_string()))?;
    if chunk.is_empty() {
        return Err(AppError::BadRequest("分片不能为空".to_string()));
    }
    let offset = req.offset.unwrap_or(0);
    let dir = user_dir(state, user_ns);
    fs::create_dir_all(&dir)
        .await
        .map_err(|error| AppError::Internal(anyhow::anyhow!("创建上传目录失败: {error}")))?;

    if offset == 0 && req.upload_id.is_none() {
        let total = req
            .file_size
            .ok_or_else(|| AppError::BadRequest("fileSize required".to_string()))?;
        if total == 0 || total > MAX_TXT_UPLOAD_BYTES as u64 {
            return Err(AppError::BadRequest(format!(
                "TXT 文件不能超过 {}MB",
                MAX_TXT_UPLOAD_BYTES / 1024 / 1024
            )));
        }
        if chunk.len() as u64 > total {
            return Err(AppError::BadRequest("分片超出文件大小".to_string()));
        }
        sweep_user_uploads(&dir).await;
        let upload_id = crate::util::crypto::random_string(UPLOAD_ID_LEN);
        write_meta(
            &dir,
            &upload_id,
            &UploadMeta {
                user_ns: user_ns.to_string(),
                total,
                created_at: now_millis(),
            },
        )
        .await?;
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(chunk_path(&dir, &upload_id))
            .await
            .map_err(|error| AppError::Internal(anyhow::anyhow!("创建分片文件失败: {error}")))?;
        file.write_all(&chunk)
            .await
            .map_err(|error| AppError::Internal(anyhow::anyhow!("写入分片失败: {error}")))?;
        file.flush()
            .await
            .map_err(|error| AppError::Internal(anyhow::anyhow!("写入分片失败: {error}")))?;
        let received = chunk.len() as u64;
        return Ok(ChunkUploadPayload {
            upload_id,
            received,
            complete: received == total,
        });
    }

    let upload_id = req
        .upload_id
        .ok_or_else(|| AppError::BadRequest("uploadId required".to_string()))?;
    if !valid_upload_id(&upload_id) {
        return Err(AppError::BadRequest("uploadId 无效".to_string()));
    }
    let lock = UPLOAD_LOCKS.acquire(&upload_id);
    let result = {
        let _guard = lock.lock().await;
        write_chunk(&dir, user_ns, &upload_id, offset, &chunk, req.file_size).await
    };
    UPLOAD_LOCKS.release(&upload_id, &lock);
    let (received, total) = result?;
    Ok(ChunkUploadPayload {
        upload_id,
        received,
        complete: received == total,
    })
}

/// 读取 `uploadId` 对应的完整文件；预览/导入共用。
///
/// 大小与首次声明不一致（少传/多传）都按「上传不完整」拒绝，避免把半本书
/// 静默解析成少几章。
pub async fn load_import_upload(
    state: &AppState,
    user_ns: &str,
    upload_id: &str,
) -> Result<Vec<u8>, AppError> {
    if !valid_upload_id(upload_id) {
        return Err(AppError::BadRequest("uploadId 无效".to_string()));
    }
    let dir = user_dir(state, user_ns);
    let meta = read_meta(&dir, upload_id).await?;
    if meta.user_ns != user_ns {
        return Err(stale_upload_error());
    }
    let bytes = fs::read(chunk_path(&dir, upload_id))
        .await
        .map_err(|_| stale_upload_error())?;
    if bytes.len() as u64 != meta.total {
        return Err(AppError::BadRequest(format!(
            "TXT 上传不完整：收到 {} 字节，文件应有 {} 字节，请重新上传",
            bytes.len(),
            meta.total
        )));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_id_validation_rejects_traversal_and_short_values() {
        assert!(valid_upload_id(&"a".repeat(UPLOAD_ID_LEN)));
        assert!(!valid_upload_id("abc"));
        assert!(!valid_upload_id(&format!("{}../", "a".repeat(29))));
        assert!(!valid_upload_id(&"a".repeat(UPLOAD_ID_LEN - 1)));
    }

    #[test]
    fn chunk_request_defaults_to_zero_offset() {
        let req = ChunkRequest::default();
        assert_eq!(req.offset.unwrap_or(0), 0);
        assert!(req.upload_id.is_none());
    }
}
