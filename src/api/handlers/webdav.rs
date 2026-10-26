use crate::api::handlers::multipart::read_limited_multipart_field;
use crate::api::AppState;
use crate::auth::CurrentUser;
use crate::error::error::{ApiResponse, AppError};
use crate::util::time::now_ts;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{
    extract::{ConnectInfo, Multipart, Path, Query, Request, State},
    Json,
};
use base64::Engine;
use dav_server::{localfs::LocalFs, memls::MemLs, DavHandler};
use futures::StreamExt;
use http_body_util::{BodyExt, Limited};
use serde::Deserialize;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::fs;
use tokio::io::AsyncWriteExt;

#[derive(Debug, Deserialize)]
pub struct WebdavPathRequest {
    pub path: Option<String>,
}

/// WebDAV 上传/PUT 单文件上限。multipart 字段不受 DefaultBodyLimit 约束，
/// 直通 dav-server 的 PUT 体也不走 axum 提取器，两处都必须自行限量。
const MAX_WEBDAV_UPLOAD_BYTES: usize = 100 * 1024 * 1024;
/// 目录路径字段的上限，超出即异常请求。
const MAX_WEBDAV_PATH_FIELD_BYTES: usize = 4 * 1024;

#[derive(Debug, Deserialize)]
pub struct WebdavDeleteListRequest {
    pub path: Option<Vec<String>>,
}

pub async fn get_webdav_file_list(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(req): Query<WebdavPathRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let home = webdav_home(&state, &user_ns).await?;
    let path = req.path.unwrap_or_else(|| "/".to_string());
    let parts = normalize_rel_path(&path)?;
    let full = join_parts(&home, &parts);
    if !full.exists() {
        return Err(AppError::BadRequest("路径不存在".to_string()));
    }
    if !full.is_dir() {
        return Err(AppError::BadRequest("路径不是目录".to_string()));
    }
    // 顺手清扫本次访问目录的上传残留（只扫本层，避免列目录变成全树扫描）
    sweep_webdav_upload_artifacts_in_dir(&full).await;
    let mut list = Vec::new();
    let mut dir = fs::read_dir(full)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    while let Some(entry) = dir
        .next_entry()
        .await
        .map_err(|e| AppError::Internal(e.into()))?
    {
        let meta = entry
            .metadata()
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let child_path = build_relative_path(&parts, &name);
        list.push(serde_json::json!({
            "name": name,
            "size": meta.len(),
            "path": child_path,
            "lastModified": meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64).unwrap_or(now_ts()),
            "isDirectory": meta.is_dir()
        }));
    }
    Ok(Json(ApiResponse::ok(Value::from(list))))
}

pub async fn get_webdav_file(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(req): Query<WebdavPathRequest>,
) -> Result<Response, AppError> {
    let user_ns = user.0.ns.clone();
    let home = webdav_home(&state, &user_ns).await?;
    let path = req.path.unwrap_or_default();
    if path.is_empty() {
        return Ok(StatusCode::BAD_REQUEST.into_response());
    }
    let parts = normalize_rel_path(&path)?;
    let full = join_parts(&home, &parts);
    if !full.exists() || full.is_dir() {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    // 流式返回，避免大文件整读入内存
    let file = fs::File::open(&full)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    // 显式 Content-Type：裸流响应会被浏览器嗅探后同源渲染（备份 JSON 被
    // 当页面执行的隐患）。已知扩展名给准确类型，未知一律 octet-stream
    // 触发下载；协议端点由 dav-server 的 mime_guess 做同样的事。
    let content_type = download_content_type(&full);
    let mut res = Response::new(axum::body::Body::from_stream(
        tokio_util::io::ReaderStream::new(file),
    ));
    res.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static(content_type),
    );
    Ok(res)
}

/// 按扩展名给下载文件一个显式 Content-Type（小表覆盖常见格式，
/// 与 dav-server 的 mime_guess 口径一致；未知扩展名一律 octet-stream）。
fn download_content_type(path: &std::path::Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("txt") | Some("text") | Some("log") | Some("md") => "text/plain; charset=utf-8",
        Some("json") | Some("jsonl") | Some("ndjson") => "application/json",
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("xml") => "application/xml",
        Some("pdf") => "application/pdf",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("epub") => "application/epub+zip",
        Some("zip") => "application/zip",
        Some("gz") => "application/gzip",
        Some("csv") => "text/csv",
        _ => "application/octet-stream",
    }
}

pub async fn upload_file_to_webdav(
    State(state): State<AppState>,
    user: CurrentUser,
    mut multipart: Multipart,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let home = webdav_home(&state, &user_ns).await?;
    let mut file_list = Vec::new();
    let mut path = "/".to_string();

    let mut field_count = 0usize;
    while let Some(field) =
        crate::api::handlers::multipart::next_field_limited(&mut multipart, &mut field_count)
            .await?
    {
        let name = field.name().unwrap_or_default().to_string();
        if name == "path" {
            let val = read_limited_multipart_field(field, MAX_WEBDAV_PATH_FIELD_BYTES, "路径过长")
                .await?;
            let val = String::from_utf8_lossy(&val).to_string();
            if !val.is_empty() {
                path = val;
            }
            continue;
        }
        let filename = field
            .file_name()
            .map(|s| s.to_string())
            .unwrap_or_else(|| "file".to_string());
        // multipart 文件名是客户端可控输入：绝对路径会整体替换 join 的 base，
        // `..` 可穿越出 webdav 根目录，必须收敛为纯文件名
        let Some(filename) = crate::util::safe_path::sanitize_file_name(&filename) else {
            return Err(AppError::BadRequest("非法文件名".to_string()));
        };
        // 先收敛路径并建目录，再流式落盘：100MB 级文件不整读进内存
        let rel = normalize_rel_path(&path)?;
        let dir = join_parts(&home, &rel);
        fs::create_dir_all(&dir)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
        // 顺手清扫目标目录里的上传残留（只扫本层）：崩溃/取消留下的临时
        // 文件不必等到 6 小时周期任务才消失
        sweep_webdav_upload_artifacts_in_dir(&dir).await;
        let target = dir.join(&filename);
        stream_multipart_field_to_file(field, &target, MAX_WEBDAV_UPLOAD_BYTES).await?;
        let meta = fs::metadata(&target)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
        file_list.push(serde_json::json!({
            "name": filename,
            "size": meta.len(),
            "path": target.to_string_lossy().replace(home.to_string_lossy().as_ref(), ""),
            "lastModified": meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64).unwrap_or(now_ts()),
            "isDirectory": meta.is_dir()
        }));
    }
    Ok(Json(ApiResponse::ok(Value::from(file_list))))
}

/// multipart 文件字段流式落盘到 `target`：逐块读字段、边读边写目标同目录的
/// 临时文件，全部写完并 flush 后 rename 到目标路径（Windows 上覆盖失败时
/// 先备份目标再替换，见 [`rename_overwrite_with_backup`]），返回写入的字节数。
///
/// 临时文件 `<文件名>.upload-<uuid>.tmp` 放在目标同目录，保证 rename 不跨卷。
/// 内存占用只有当前 chunk（对比 [`read_limited_multipart_field`] 把整个字段
/// 读进 Vec）。清理统一交给 [`TempUploadGuard`]：显式错误分支、**取消路径**
/// （handler future 被丢弃——客户端断连、上传超时在收流中途触发、panic）都会
/// 经守卫的 Drop 删除临时文件，不存在 async Drop 可靠性这回事。
async fn stream_multipart_field_to_file(
    mut field: axum::extract::multipart::Field<'_>,
    target: &std::path::Path,
    limit: usize,
) -> Result<u64, AppError> {
    let dir = target.parent().ok_or_else(|| {
        AppError::Internal(anyhow::anyhow!("WebDAV 上传目标缺少父目录: {target:?}"))
    })?;
    let stem = target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file");
    let temp = dir.join(format!("{stem}.upload-{}.tmp", uuid::Uuid::new_v4()));
    // 守卫必须声明在 File **之前**：局部按逆声明序析构，句柄先关（空闲时
    // tokio File 的 Arc<StdFile> 同步关掉）、守卫后删——顺序反了的话 Windows
    // 上删一个仍被句柄占用的文件会静默失败。File::create 失败时磁盘上没有
    // 文件，守卫在武装成功前保持无害。
    let mut guard = TempUploadGuard {
        path: temp.clone(),
        armed: false,
    };
    let mut file = match fs::File::create(&temp).await {
        Ok(f) => f,
        Err(e) => return Err(AppError::Internal(e.into())),
    };
    guard.armed = true;
    let mut written: u64 = 0;
    while let Some(chunk) = field.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            // 字段读取失败按 400 透传，与 read_limited_multipart_field 同口径
            Err(e) => return Err(AppError::BadRequest(e.to_string())),
        };
        written = written.saturating_add(chunk.len() as u64);
        if written > limit as u64 {
            return Err(AppError::BadRequest("文件不能超过 100MB".to_string()));
        }
        if let Err(e) = file.write_all(&chunk).await {
            return Err(AppError::Internal(e.into()));
        }
    }
    if let Err(e) = file.flush().await {
        return Err(AppError::Internal(e.into()));
    }
    // 先关句柄再 rename：Windows 上被占用的文件无法改名
    drop(file);
    // POSIX 的 rename 原子覆盖已存在目标。Windows 的 MoveFileEx(
    // MOVEFILE_REPLACE_EXISTING) 通常也能覆盖，但目标被其他进程以不兼容的
    // 共享模式打开、或 temp 被杀毒/索引服务短时锁住时会失败（表现为
    // AlreadyExists/PermissionDenied）。此时走「先备份目标再替换」：
    // 绝不先删旧文件——旧文件若先删、重试又失败，旧备份与新内容就一起丢
    // 了（Linux 不走这条分支，见 [`rename_overwrite_with_backup`]）。
    overwrite_via_rename(&temp, target)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    // rename 成功：临时文件已不存在，解除守卫避免误删同名新文件
    guard.disarm();
    Ok(written)
}

/// 生产环境的覆盖 rename 实现：[`tokio::fs::rename`] + 备份替换语义。
async fn overwrite_via_rename(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    rename_overwrite_with_backup(
        from,
        to,
        |f, t| async move { tokio::fs::rename(&f, &t).await },
    )
    .await
}

/// 覆盖写语义：把 `from` rename 到 `to`，保证任意失败路径下旧文件要么已被
/// 新内容替换、要么原样保留，绝不出现「先删旧文件、重试又失败」的两头空。
///
/// **目录目标先行拒绝**：文件不能覆盖同名目录。Linux 的 rename 本就报
/// IsADirectory（不进备份分支）；Windows 上是 PermissionDenied，不拦的话会
/// 误入备份分支——整个目录被搬进点前缀隐藏备份后重试成功、目录名被上传
/// 文件顶替，而 best-effort 删备份对目录必然失败又被吞掉，目录就从 JSON
/// 文件列表（点前缀过滤）里静默消失了。判定用 `symlink_metadata` 不跟随
/// 末段符号链接，保持 POSIX 直接替换符号链接的原语义。
///
/// 流程：
/// 1. 直接 rename（POSIX 原子覆盖，绝大多数调用到此为止）；
/// 2. 失败且错误类型为 AlreadyExists/PermissionDenied（Windows 上目标被
///    其他进程以不兼容共享模式打开，或 `from` 被杀毒/索引服务短时锁住）
///    时进入备份替换：把旧目标 rename 成同目录点前缀备份
///    `.<目标名>.reader-upload-backup-<uuid>.tmp`（带目标名，清扫任务才能
///    判断它是否还是唯一副本）；
///    - 备份 rename 返回 NotFound：目标已不存在（首次失败另有原因，如瞬时
///      共享冲突），直接重试一次覆盖，仍失败返回错误；
///    - 备份 rename 其他失败：返回错误，旧文件原样保留（temp 交由
///      [`TempUploadGuard`] 清理），**不删目标**；
///    - 备份成功后重试覆盖：成功则删除备份，删除失败只记 warn 并把路径
///      留给定期清扫；失败则把备份 rename 回目标恢复旧文件——恢复成功返回
///      原始错误；恢复失败时错误信息带上备份路径，备份绝不删除，留给人工
///      恢复与清扫任务兜底。
///
/// `rename_op` 注入 rename 动作（生产为 [`tokio::fs::rename`]），失败分支
/// 因此可以确定性测试；不进备份分支的其他错误类型（NotFound 等）直接透传。
async fn rename_overwrite_with_backup<F, Fut>(
    from: &std::path::Path,
    to: &std::path::Path,
    mut rename_op: F,
) -> std::io::Result<()>
where
    F: FnMut(PathBuf, PathBuf) -> Fut,
    Fut: std::future::Future<Output = std::io::Result<()>>,
{
    // 目标是目录时拒绝（见函数文档）；metadata 取不到时按非目录处理，
    // 交由后续 rename 决定成败
    if matches!(
        tokio::fs::symlink_metadata(to).await,
        Ok(m) if m.is_dir()
    ) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::IsADirectory,
            "上传目标与现有目录同名，文件不能覆盖目录",
        ));
    }
    match rename_op(from.to_path_buf(), to.to_path_buf()).await {
        Ok(()) => return Ok(()),
        Err(e)
            if !matches!(
                e.kind(),
                std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            return Err(e);
        }
        Err(_) => {}
    }
    let target_name = to.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let backup = to.with_file_name(format!(
        ".{target_name}.reader-upload-backup-{}.tmp",
        uuid::Uuid::new_v4()
    ));
    if let Err(backup_err) = rename_op(to.to_path_buf(), backup.clone()).await {
        if backup_err.kind() == std::io::ErrorKind::NotFound {
            // 目标已不存在：首次失败另有原因，重试一次覆盖，仍失败返回错误
            return rename_op(from.to_path_buf(), to.to_path_buf()).await;
        }
        // 目标被占用等：返回错误，旧文件原样保留
        return Err(backup_err);
    }
    match rename_op(from.to_path_buf(), to.to_path_buf()).await {
        Ok(()) => {
            // 覆盖成功：备份只是旧版本副本，best-effort 清理。删除失败不能
            // 静默：残留的点前缀文件会出现在 WebDAV PROPFIND 里，交给清扫
            // 任务（目标已存在 → 判为冗余备份删除）
            if let Err(e) = tokio::fs::remove_file(&backup).await {
                tracing::warn!(
                    path = %backup.display(),
                    error = %e,
                    "覆盖成功后删除旧文件备份失败，留待定期清扫"
                );
            }
            Ok(())
        }
        Err(e) => match rename_op(backup.clone(), to.to_path_buf()).await {
            // 恢复成功：旧文件回到原位，返回原始错误
            Ok(()) => Err(e),
            Err(restore_err) => Err(std::io::Error::other(format!(
                "覆盖写入失败（{e}），恢复原文件也失败（{restore_err}）；原文件已保留在备份 {}",
                backup.display()
            ))),
        },
    }
}

/// 上传临时文件的兜底清理守卫：armed 状态下 Drop 时删除临时文件。
///
/// tokio 没有 async Drop，handler future 被取消时任何显式错误分支都不会
/// 执行，唯一可靠的清理点是 future 状态机析构时的逐局部 Drop。同步删除
/// 在句柄已被关闭后（见 [`stream_multipart_field_to_file`] 的声明序说明）
/// 对常见取消点（等下一块网络数据）直接生效；若句柄仍被在途写持有
/// （Windows 上表现为共享冲突），退化为在运行时上延迟重试一次。
struct TempUploadGuard {
    path: PathBuf,
    armed: bool,
}

impl TempUploadGuard {
    /// 成功落盘后解除清理义务（消费 self，Drop 变为无操作）。
    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for TempUploadGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if std::fs::remove_file(&self.path).is_ok() {
            return;
        }
        // 典型残留原因：句柄仍被在途写持有（tokio File 的 Arc<StdFile> 在
        // 阻塞池任务手里），Windows 上删除共享冲突。延迟重试兜底；
        // 不在运行时上（如进程退出路径）则放弃。
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let path = self.path.clone();
            // 直接丢弃 JoinHandle（分离任务）：清理尽力而为，无人等待它
            handle.spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                let _ = fs::remove_file(&path).await;
            });
        }
    }
}

/// 上传临时文件的最长保留时长：上传超时是 900 秒，1 小时足够避免误删在途上传。
const WEBDAV_UPLOAD_TEMP_MAX_AGE: Duration = Duration::from_secs(60 * 60);
/// 无法解析原目标名的旧格式备份（`.reader-upload-backup-<uuid>.tmp`）保留 7 天，
/// 给人工恢复留时间。
const WEBDAV_LEGACY_BACKUP_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// 上传临时文件名里的标记：`<目标名>.upload-<uuid>.tmp`。
const UPLOAD_TEMP_MARKER: &str = ".upload-";
/// 备份文件名里的标记：`.<目标名>.reader-upload-backup-<uuid>.tmp`。
const BACKUP_MARKER: &str = ".reader-upload-backup-";

/// WebDAV 目录里由本模块产生的临时产物。
#[derive(Debug, PartialEq, Eq)]
enum WebdavArtifact<'a> {
    /// `<目标名>.upload-<uuid>.tmp`：上传在途/中断留下的临时文件。
    UploadTemp,
    /// `.<目标名>.reader-upload-backup-<uuid>.tmp`：覆盖写时搬走的旧文件。
    /// `target = None` 是无法解析原目标名的旧格式。
    Backup { target: Option<&'a str> },
}

/// 从文件名识别上传临时文件与覆盖备份（不命中返回 None）。
///
/// 只认带合法 UUID 后缀的名字：用户自己的 `x.upload-report.tmp` 之类不会被
/// 误判成清扫对象。
fn classify_webdav_artifact(name: &str) -> Option<WebdavArtifact<'_>> {
    let stem = name.strip_suffix(".tmp")?;
    if let Some(idx) = stem.rfind(BACKUP_MARKER) {
        let uuid_part = &stem[idx + BACKUP_MARKER.len()..];
        if uuid::Uuid::parse_str(uuid_part).is_ok() {
            if idx == 0 {
                // 旧格式：`reader-upload-backup` 前的点就是标记自己的首字符
                return Some(WebdavArtifact::Backup { target: None });
            }
            if stem.starts_with('.') {
                // 从最后一个标记往前切，目标名本身含该标记也能正确还原
                let target = &stem[1..idx];
                if !target.is_empty() {
                    return Some(WebdavArtifact::Backup {
                        target: Some(target),
                    });
                }
            }
        }
    }
    if let Some(idx) = stem.rfind(UPLOAD_TEMP_MARKER) {
        let uuid_part = &stem[idx + UPLOAD_TEMP_MARKER.len()..];
        if !stem[..idx].is_empty() && uuid::Uuid::parse_str(uuid_part).is_ok() {
            return Some(WebdavArtifact::UploadTemp);
        }
    }
    None
}

/// 递归清扫一个 WebDAV 根目录：启动时与每 6 小时的周期任务使用。
///
/// 目录不存在时直接返回；单个文件失败只记日志，不影响其余条目。
pub(crate) async fn sweep_webdav_upload_artifacts(root: &std::path::Path) {
    sweep_webdav_artifacts(
        root,
        true,
        WEBDAV_UPLOAD_TEMP_MAX_AGE,
        WEBDAV_LEGACY_BACKUP_MAX_AGE,
    )
    .await;
}

/// 清扫单个目录（不递归）：上传/列表/协议端点访问当前目录时顺手调用，
/// 让崩溃或取消留下的残留不必等到周期任务。
pub(crate) async fn sweep_webdav_upload_artifacts_in_dir(dir: &std::path::Path) {
    sweep_webdav_artifacts(
        dir,
        false,
        WEBDAV_UPLOAD_TEMP_MAX_AGE,
        WEBDAV_LEGACY_BACKUP_MAX_AGE,
    )
    .await;
}

async fn sweep_webdav_artifacts(
    root: &std::path::Path,
    recursive: bool,
    upload_artifact_max_age: Duration,
    legacy_backup_max_age: Duration,
) {
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let mut entries = match fs::read_dir(&dir).await {
            Ok(entries) => entries,
            // 目录不存在（还没人用过 WebDAV）不是错误
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                tracing::warn!(dir = %dir.display(), error = %e, "WebDAV 清扫：读取目录失败，跳过");
                continue;
            }
        };
        loop {
            let entry = match entries.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!(dir = %dir.display(), error = %e, "WebDAV 清扫：读取目录条目失败，跳过剩余条目");
                    break;
                }
            };
            // file_type 不跟随符号链接：符号链接既不算目录（不递归进入）
            // 也不算普通文件（不清扫），避免顺链删到 WebDAV 根之外
            let file_type = match entry.file_type().await {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(path = %entry.path().display(), error = %e, "WebDAV 清扫：读取条目类型失败，跳过");
                    continue;
                }
            };
            if file_type.is_dir() {
                if recursive {
                    pending.push(entry.path());
                }
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            let os_name = entry.file_name();
            let Some(name) = os_name.to_str() else {
                continue;
            };
            let Some(artifact) = classify_webdav_artifact(name) else {
                continue;
            };
            let Some(age) = entry_age(&entry).await else {
                continue;
            };
            let path = entry.path();
            match artifact {
                WebdavArtifact::UploadTemp => {
                    if age > upload_artifact_max_age {
                        remove_artifact(&path, "清理超时的 WebDAV 上传临时文件").await;
                    }
                }
                WebdavArtifact::Backup {
                    target: Some(target),
                } => {
                    if age <= upload_artifact_max_age {
                        continue;
                    }
                    let target_path = path.with_file_name(target);
                    match fs::symlink_metadata(&target_path).await {
                        // 目标还在：备份只是旧版本副本，删除
                        Ok(meta) if !meta.is_dir() => {
                            remove_artifact(&path, "目标已存在，删除冗余的 WebDAV 上传备份").await;
                        }
                        // 同名目录占用了目标路径：不能把它 rename 掉，也不删
                        // 备份（可能是旧文件的唯一副本），留待人工处理
                        Ok(_) => tracing::warn!(
                            path = %path.display(),
                            target = %target_path.display(),
                            "WebDAV 上传备份的目标路径被目录占用，保留备份待人工处理"
                        ),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                            // 目标缺失：备份可能是旧文件的唯一副本，恢复而不是删除
                            match fs::rename(&path, &target_path).await {
                                Ok(()) => tracing::info!(
                                    backup = %path.display(),
                                    target = %target_path.display(),
                                    "WebDAV 上传备份已恢复到缺失的目标"
                                ),
                                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                                Err(e) => tracing::warn!(
                                    backup = %path.display(),
                                    target = %target_path.display(),
                                    error = %e,
                                    "恢复 WebDAV 上传备份失败，备份保留"
                                ),
                            }
                        }
                        Err(e) => tracing::warn!(
                            path = %path.display(),
                            target = %target_path.display(),
                            error = %e,
                            "检查 WebDAV 上传备份的目标失败，备份保留"
                        ),
                    }
                }
                WebdavArtifact::Backup { target: None } => {
                    if age > legacy_backup_max_age {
                        // 旧格式无法判断原目标，删前 warn 留下路径供人工恢复
                        tracing::warn!(
                            path = %path.display(),
                            "删除超过 7 天的旧格式 WebDAV 上传备份（无法解析原目标名）"
                        );
                        remove_artifact(&path, "删除旧格式 WebDAV 上传备份").await;
                    }
                }
            }
        }
    }
}

/// 条目的 mtime 距今时长；取不到或 mtime 在未来时返回 None（按新鲜处理，不删）。
async fn entry_age(entry: &tokio::fs::DirEntry) -> Option<Duration> {
    let modified = entry.metadata().await.ok()?.modified().ok()?;
    std::time::SystemTime::now().duration_since(modified).ok()
}

/// 删除单个残留文件；失败只记日志，不影响清扫其余文件。
async fn remove_artifact(path: &std::path::Path, action: &str) {
    match fs::remove_file(path).await {
        Ok(()) => tracing::info!(path = %path.display(), action, "WebDAV 清扫删除文件"),
        // 并发清扫（周期任务撞上 handler 顺手清理）时对方已删掉，不算失败
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            tracing::warn!(path = %path.display(), action, error = %e, "WebDAV 清扫删除文件失败")
        }
    }
}

pub async fn delete_webdav_file(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<WebdavPathRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let home = webdav_home(&state, &user_ns).await?;
    let path = req.path.unwrap_or_default();
    if path.is_empty() {
        return Err(AppError::BadRequest("参数错误".to_string()));
    }
    let rel = normalize_rel_path(&path)?;
    let target = join_parts(&home, &rel);
    if !target.exists() {
        return Err(AppError::BadRequest("路径不存在".to_string()));
    }
    if target.is_dir() {
        fs::remove_dir_all(target)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
    } else {
        fs::remove_file(target)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
    }
    Ok(Json(ApiResponse::ok(Value::String("".to_string()))))
}

pub async fn delete_webdav_file_list(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<WebdavDeleteListRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let home = webdav_home(&state, &user_ns).await?;
    let paths = req.path.unwrap_or_default();
    for p in paths {
        if p.is_empty() {
            continue;
        }
        let rel = normalize_rel_path(&p)?;
        let target = join_parts(&home, &rel);
        if target.exists() {
            if target.is_dir() {
                let _ = fs::remove_dir_all(target).await;
            } else {
                let _ = fs::remove_file(target).await;
            }
        }
    }
    Ok(Json(ApiResponse::ok(Value::String("".to_string()))))
}

/// WebDAV 协议入口：协议语义由 dav-server 实现（litmus 验证过的
/// PROPFIND/PUT/MOVE/LOCK 等行为），本层只负责 Basic 认证、按用户隔离目录、
/// Windows 路径加固与上传体积上限。
pub async fn webdav_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    path: Option<Path<String>>,
    headers: HeaderMap,
    req: Request,
) -> Response {
    // Basic 认证与登录接口共享限速，按客户端 IP 记账；对端命中
    // TRUSTED_PROXIES 时从 X-Forwarded-For 还原真实地址（挂 nginx/Docker
    // NAT 后全体请求同 socket IP，会互相连坐），否则一律用 socket 地址
    let trusted = crate::util::client_ip::parse_or_default(&state.config.trusted_proxies);
    let client_ip = crate::util::client_ip::resolve_client_ip(addr.ip(), &headers, &trusted);
    let user_ns = match resolve_webdav_user(&state, &headers, Some(&client_ip)).await {
        Ok(ns) => ns,
        Err(res) => return *res,
    };
    let home = match webdav_home(&state, &user_ns).await {
        Ok(h) => h,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    // dav-server 自己会拦路径逃逸（`..` 越级、段内 %2f），但不认识 Windows 的
    // 盘符/设备名/ADS/结尾点空格，分发前按原有规则再拦一遍（Path 通配值已
    // percent-decode）。裸根路径 /reader3/webdav 无通配参数，path 为 None。
    let rel = format!("/{}", path.map(|p| p.0).unwrap_or_default());
    let Ok(parts) = normalize_rel_path(&rel) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    // 顺手清扫本次请求路径所在目录的上传残留（只扫本层）：PUT 覆盖崩溃
    // 留下的备份能从下一次任何 WebDAV 请求里自愈
    let parent_parts = &parts[..parts.len().saturating_sub(1)];
    sweep_webdav_upload_artifacts_in_dir(&join_parts(&home, parent_parts)).await;

    let dav = DavHandler::builder()
        .filesystem(LocalFs::new(&home, false, false, false))
        .locksystem(memls())
        .strip_prefix("/reader3/webdav")
        .build_handler();
    let (parts, body) = req.into_parts();
    // DefaultBodyLimit 只约束 axum 提取器，直通 dav-server 的 PUT 体自行限量；
    // dav-server 要求 body 错误类型是具体 StdError，axum 0.7 的
    // BoxError（Box<dyn Error> 不实现 Error）不满足，统一映射为 io::Error。
    let body = Limited::new(body, MAX_WEBDAV_UPLOAD_BYTES).map_err(std::io::Error::other);
    let req = Request::from_parts(parts, body);
    dav.handle(req).await.into_response()
}

/// 全进程共享一把真实内存锁。MemLs 是 Arc 句柄，必须全局唯一——
/// 每请求新建等于锁状态恒为空。
fn memls() -> Box<MemLs> {
    static LS: OnceLock<MemLs> = OnceLock::new();
    Box::new(LS.get_or_init(|| *MemLs::new()).clone())
}

/// 认证失败统一返回同一个匿名 401 质询（无错误详情可区分），Err 装箱
/// 避免 `Response`（≥128 字节）触发 `result_large_err`。
async fn resolve_webdav_user(
    state: &AppState,
    headers: &HeaderMap,
    client_ip: Option<&str>,
) -> Result<String, Box<Response>> {
    let auth = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !auth.to_ascii_lowercase().starts_with("basic ") {
        return Err(Box::new(unauthorized()));
    }
    let b64 = auth[6..].trim();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|_| Box::new(unauthorized()))?;
    let decoded = String::from_utf8_lossy(&decoded);
    let parts: Vec<&str> = decoded.splitn(2, ':').collect();
    if parts.len() != 2 {
        return Err(Box::new(unauthorized()));
    }
    let username = parts[0];
    let password = parts[1];
    match state
        .user_service
        .verify_basic_webdav(username, password, client_ip)
        .await
    {
        // 存储路径按 user_<uid> 隔离：username 只用于认证
        Ok(Some(user)) => Ok(crate::auth::middleware::user_ns_of(user.uid)),
        _ => Err(Box::new(unauthorized())),
    }
}

/// 401 质询应答：缺头、解码失败、用户名/密码错误统一走这里。
///
/// 必须带 `WWW-Authenticate: Basic realm="reader"`（RFC 7617）：Windows
/// 资源管理器等客户端收不到质询头就不会弹凭据框，表现为静默失败。
fn unauthorized() -> Response {
    let mut res = StatusCode::UNAUTHORIZED.into_response();
    res.headers_mut().insert(
        "WWW-Authenticate",
        HeaderValue::from_static("Basic realm=\"reader\""),
    );
    res
}

async fn webdav_home(state: &AppState, user_ns: &str) -> Result<PathBuf, AppError> {
    let dir = PathBuf::from(&state.config.storage_dir)
        .join("webdav")
        .join(user_ns);
    fs::create_dir_all(&dir)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    Ok(dir)
}

/// 把客户端给的相对路径拆成逐段的安全分量。
///
/// 必须同时按 `/` 和 `\` 切分：Windows 上 `\` 是路径分隔符，只按 `/` 切会让
/// `..\..\x` 成为"普通段"逃过 `..` 检查，join 后直接穿越出 webdav 根目录。
/// `:` 一并拒绝（Windows 盘符前缀与 NTFS ADS）。
fn normalize_rel_path(path: &str) -> Result<Vec<String>, AppError> {
    let mut parts = Vec::new();
    for p in path.split(['/', '\\']) {
        if p.is_empty() || p == "." {
            continue;
        }
        if p == ".."
            || p.contains([':', '\0'])
            || p.ends_with(['.', ' '])
            || crate::util::safe_path::is_windows_device_name(p)
        {
            return Err(AppError::BadRequest("非法路径".to_string()));
        }
        parts.push(p.to_string());
    }
    Ok(parts)
}

fn join_parts(home: &std::path::Path, parts: &[String]) -> PathBuf {
    let mut p = home.to_path_buf();
    for part in parts {
        p = p.join(part);
    }
    p
}

fn build_relative_path(parts: &[String], name: &str) -> String {
    if parts.is_empty() {
        format!("/{}", name)
    } else {
        format!("/{}/{}", parts.join("/"), name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_rel_path_rejects_traversal_on_both_separators() {
        assert!(normalize_rel_path("/../../etc/passwd").is_err());
        // Windows 上反斜杠就是分隔符，必须等价拒绝
        assert!(normalize_rel_path(r"..\..\storage\data\users.json").is_err());
        assert!(normalize_rel_path("/a/../..").is_err());
        assert!(normalize_rel_path(r"a\..\..\b").is_err());
        // 盘符与 ADS
        assert!(normalize_rel_path("C:/Windows/x").is_err());
        assert!(normalize_rel_path("a.txt:evil").is_err());
        // Windows 设备名与结尾点/空格
        assert!(normalize_rel_path("/NUL").is_err());
        assert!(normalize_rel_path("/con.txt").is_err());
        assert!(normalize_rel_path("/a.txt.").is_err());
        assert!(normalize_rel_path("/a.txt ").is_err());
    }

    #[test]
    fn normalize_rel_path_accepts_normal_paths() {
        assert_eq!(
            normalize_rel_path("/books/中文 书/1.txt").unwrap(),
            vec!["books", "中文 书", "1.txt"]
        );
        assert!(normalize_rel_path("/").unwrap().is_empty());
        assert_eq!(normalize_rel_path("//a//./b//").unwrap(), vec!["a", "b"]);
    }

    #[test]
    fn download_content_type_maps_known_extensions_and_defaults_to_octet_stream() {
        use std::path::Path;
        assert_eq!(
            download_content_type(Path::new("/backup/a.json")),
            "application/json"
        );
        assert_eq!(
            download_content_type(Path::new("/backup/a.txt")),
            "text/plain; charset=utf-8"
        );
        assert_eq!(
            download_content_type(Path::new("/backup/book.epub")),
            "application/epub+zip"
        );
        // 大写扩展名与未知/无扩展名一律回退 octet-stream（触发下载而非渲染）
        assert_eq!(
            download_content_type(Path::new("/backup/A.JSON")),
            "application/json"
        );
        assert_eq!(
            download_content_type(Path::new("/backup/backup.dat")),
            "application/octet-stream"
        );
        assert_eq!(
            download_content_type(Path::new("/backup/noext")),
            "application/octet-stream"
        );
    }

    // ---- rename_overwrite_with_backup 的备份替换语义 ----

    type BoxRenameFuture =
        std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<()>>>>;

    /// 测试沙盒目录：target/ 下按 uuid 隔离，测试结束自行清理。
    fn sandbox(tag: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("agent-test")
            .join(format!("webdav-replace-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 目录里残留的备份文件名（应为空时断言用）。新命名带目标名
    /// （`.book.epub.reader-upload-backup-<uuid>.tmp`），按标记匹配。
    fn backup_residues(dir: &std::path::Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| name.contains(".reader-upload-backup-"))
            .collect()
    }

    /// 构造按脚本注入失败的 rename 闭包：第 n 次调用（0 起）命中 `fail_at`
    /// 时返回 PermissionDenied（不动磁盘），其余调用执行真实
    /// `std::fs::rename`。返回（闭包, 实际调用次数单元格）供断言调用序列。
    /// Windows「目标被杀毒/索引服务锁住」等真实占用无法在测试里稳定复现
    /// （进程内无法模拟跨进程共享冲突），用脚本化失败确定性覆盖各分支。
    fn scripted_rename(
        fail_at: Vec<usize>,
    ) -> (
        impl FnMut(PathBuf, PathBuf) -> BoxRenameFuture,
        std::rc::Rc<std::cell::RefCell<usize>>,
    ) {
        use std::cell::RefCell;
        use std::rc::Rc;
        let fails = Rc::new(RefCell::new(fail_at));
        let calls = Rc::new(RefCell::new(0usize));
        let fails_c = Rc::clone(&fails);
        let calls_c = Rc::clone(&calls);
        let op = move |f: PathBuf, t: PathBuf| {
            let n = {
                let mut c = calls_c.borrow_mut();
                let n = *c;
                *c += 1;
                n
            };
            let fail = fails_c.borrow().contains(&n);
            let fut: BoxRenameFuture = Box::pin(async move {
                if fail {
                    return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
                }
                std::fs::rename(&f, &t)
            });
            fut
        };
        (op, calls)
    }

    /// 真实 rename 的注入实现（不注入失败的成功路径用）。
    fn real_rename() -> (
        impl FnMut(PathBuf, PathBuf) -> BoxRenameFuture,
        std::rc::Rc<std::cell::RefCell<usize>>,
    ) {
        use std::cell::RefCell;
        use std::rc::Rc;
        let calls = Rc::new(RefCell::new(0usize));
        let calls_c = Rc::clone(&calls);
        let op = move |f: PathBuf, t: PathBuf| {
            *calls_c.borrow_mut() += 1;
            let fut: BoxRenameFuture = Box::pin(async move { std::fs::rename(&f, &t) });
            fut
        };
        (op, calls)
    }

    #[tokio::test]
    async fn overwrite_replaces_target_and_leaves_no_backup() {
        let dir = sandbox("success");
        let target = dir.join("book.epub");
        std::fs::write(&target, b"old").unwrap();
        let temp = dir.join("book.epub.upload-x.tmp");
        std::fs::write(&temp, b"new").unwrap();

        let (op, calls) = scripted_rename(vec![]);
        rename_overwrite_with_backup(&temp, &target, op)
            .await
            .unwrap();
        assert_eq!(*calls.borrow(), 1, "成功路径只应调用一次 rename");

        assert_eq!(std::fs::read(&target).unwrap(), b"new", "目标应是新内容");
        assert!(!temp.exists(), "temp 应已改名到目标");
        assert!(
            backup_residues(&dir).is_empty(),
            "不应残留备份: {:?}",
            backup_residues(&dir)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn overwrite_creates_target_when_absent() {
        let dir = sandbox("absent");
        let target = dir.join("book.epub");
        let temp = dir.join("book.epub.upload-x.tmp");
        std::fs::write(&temp, b"new").unwrap();

        let (op, calls) = real_rename();
        rename_overwrite_with_backup(&temp, &target, op)
            .await
            .unwrap();
        assert_eq!(*calls.borrow(), 1);

        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert!(backup_residues(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn overwrite_rejects_directory_target_and_preserves_it() {
        // 目标是同名目录：文件不能覆盖目录。Windows 上 rename(file→目录)
        // 返回 PermissionDenied，若不拦会误入备份分支——整个目录被搬进点前缀
        // 隐藏备份后重试成功、目录名被上传文件顶替，旧目录就从 JSON 文件
        // 列表（点前缀过滤）里静默消失且接口报成功（Linux 上 rename 返回
        // IsADirectory 直接透传，无此问题）。
        let dir = sandbox("dir-target");
        let target = dir.join("notes");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("old.txt"), b"inside").unwrap();
        let temp = dir.join("notes.upload-x.tmp");
        std::fs::write(&temp, b"new").unwrap();

        let (op, calls) = real_rename();
        let err = rename_overwrite_with_backup(&temp, &target, op)
            .await
            .unwrap_err();
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::IsADirectory,
            "应返回 IsADirectory 而非其他错误: {err}"
        );
        assert_eq!(*calls.borrow(), 0, "目录目标应在任何 rename 之前被拒绝");
        assert!(target.is_dir(), "同名目录必须原样保留");
        assert_eq!(
            std::fs::read(target.join("old.txt")).unwrap(),
            b"inside",
            "目录内容必须完好"
        );
        assert!(
            backup_residues(&dir).is_empty(),
            "不得把目录搬进隐藏备份: {:?}",
            backup_residues(&dir)
        );
        assert_eq!(std::fs::read(&temp).unwrap(), b"new", "temp 应原样保留");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn overwrite_passes_through_non_retryable_errors() {
        // NotFound 等其他错误类型不进备份分支，直接透传
        let dir = sandbox("passthrough");
        let target = dir.join("book.epub");
        let missing_temp = dir.join("no-such.upload-x.tmp");

        let (op, calls) = real_rename();
        let err = rename_overwrite_with_backup(&missing_temp, &target, op)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        assert_eq!(*calls.borrow(), 1, "不应进入备份分支");
        assert!(!target.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn overwrite_retries_once_when_target_vanishes() {
        // 首次 rename 注入失败（PermissionDenied）但目标其实不存在：
        // 备份 rename 返回 NotFound，核心直接重试一次覆盖
        let dir = sandbox("vanish");
        let target = dir.join("book.epub");
        let temp = dir.join("book.epub.upload-x.tmp");
        std::fs::write(&temp, b"new").unwrap();

        // 调用序：0 首次(注入失败) → 1 备份(真实, target 缺失 → NotFound)
        //        → 2 重试(真实, 成功)
        let (op, calls) = scripted_rename(vec![0]);
        rename_overwrite_with_backup(&temp, &target, op)
            .await
            .unwrap();
        assert_eq!(
            *calls.borrow(),
            3,
            "应经历 失败→备份(NotFound)→重试 三次调用"
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert!(backup_residues(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn overwrite_returns_error_when_retry_after_vanish_fails() {
        // NotFound 重试仍失败就返回错误，不引入备份也不删 temp
        let dir = sandbox("vanish-fail");
        let target = dir.join("book.epub");
        let temp = dir.join("book.epub.upload-x.tmp");
        std::fs::write(&temp, b"new").unwrap();

        // 调用序：0 首次(注入失败) → 1 备份(真实, NotFound) → 2 重试(注入失败)
        let (op, calls) = scripted_rename(vec![0, 2]);
        let err = rename_overwrite_with_backup(&temp, &target, op)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(*calls.borrow(), 3);
        assert!(!target.exists());
        assert_eq!(std::fs::read(&temp).unwrap(), b"new", "temp 应原样保留");
        assert!(backup_residues(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn overwrite_restores_backup_when_second_rename_fails() {
        // 第二次覆盖 rename 失败（如 temp 被杀毒/索引服务锁住）：
        // 旧文件必须从备份恢复，绝不两头空
        let dir = sandbox("restore");
        let target = dir.join("book.epub");
        std::fs::write(&target, b"old").unwrap();
        let temp = dir.join("book.epub.upload-x.tmp");
        std::fs::write(&temp, b"new").unwrap();

        // 调用序：0 首次(注入失败) → 1 备份(真实, target→backup)
        //        → 2 重试(注入失败) → 3 恢复(真实, backup→target)
        let (op, calls) = scripted_rename(vec![0, 2]);
        let err = rename_overwrite_with_backup(&temp, &target, op)
            .await
            .unwrap_err();
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::PermissionDenied,
            "应返回重试失败的原始错误"
        );
        assert_eq!(
            *calls.borrow(),
            4,
            "应经历 失败→备份→重试失败→恢复 四次调用"
        );
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"old",
            "旧文件应从备份恢复"
        );
        assert!(
            backup_residues(&dir).is_empty(),
            "恢复后不应残留备份: {:?}",
            backup_residues(&dir)
        );
        assert_eq!(
            std::fs::read(&temp).unwrap(),
            b"new",
            "temp 由 TempUploadGuard 清理，核心不动它"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn overwrite_keeps_backup_when_restore_also_fails() {
        // 恢复也失败：错误信息带备份路径，备份保留待人工恢复，绝不删除
        let dir = sandbox("restore-fail");
        let target = dir.join("book.epub");
        std::fs::write(&target, b"old").unwrap();
        let temp = dir.join("book.epub.upload-x.tmp");
        std::fs::write(&temp, b"new").unwrap();

        // 调用序：0 首次(注入失败) → 1 备份(真实) → 2 重试(注入失败) → 3 恢复(注入失败)
        let (op, calls) = scripted_rename(vec![0, 2, 3]);
        let err = rename_overwrite_with_backup(&temp, &target, op)
            .await
            .unwrap_err();
        assert_eq!(*calls.borrow(), 4);
        let residues = backup_residues(&dir);
        assert_eq!(residues.len(), 1, "恢复失败时备份必须保留: {residues:?}");
        let backup_name = residues.into_iter().next().unwrap();
        assert!(
            backup_name.contains("book.epub"),
            "备份名应带目标文件名，清扫任务才能判断是否唯一副本: {backup_name}"
        );
        assert!(
            err.to_string().contains(&backup_name),
            "错误信息必须带备份路径: {}",
            err
        );
        assert_eq!(
            std::fs::read(dir.join(&backup_name)).unwrap(),
            b"old",
            "备份内容应是旧文件"
        );
        assert!(!target.exists(), "恢复失败时目标不应出现半成品");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn overwrite_keeps_target_when_backup_rename_fails() {
        // 备份 rename 失败（目标被占用）：直接返回错误，旧文件原样保留
        let dir = sandbox("backup-fail");
        let target = dir.join("book.epub");
        std::fs::write(&target, b"old").unwrap();
        let temp = dir.join("book.epub.upload-x.tmp");
        std::fs::write(&temp, b"new").unwrap();

        // 调用序：0 首次(注入失败) → 1 备份(注入失败)
        let (op, calls) = scripted_rename(vec![0, 1]);
        let err = rename_overwrite_with_backup(&temp, &target, op)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(*calls.borrow(), 2);
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"old",
            "备份失败时旧文件必须原样保留"
        );
        assert!(backup_residues(&dir).is_empty());
        assert_eq!(std::fs::read(&temp).unwrap(), b"new");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- 上传临时文件与遗留备份的清扫 ----

    const HOUR: Duration = Duration::from_secs(60 * 60);
    const SEVEN_DAYS: Duration = Duration::from_secs(7 * 24 * 60 * 60);

    /// 把文件 mtime 拨到指定时长前（模拟进程崩溃/取消留下的遗留产物）。
    fn set_file_age(path: &std::path::Path, age: Duration) {
        let file = std::fs::File::options().write(true).open(path).unwrap();
        let modified = std::time::SystemTime::now() - age;
        file.set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
    }

    fn upload_temp_name(target: &str) -> String {
        format!("{target}.upload-{}.tmp", uuid::Uuid::new_v4())
    }

    fn backup_name(target: &str) -> String {
        format!(
            ".{target}.reader-upload-backup-{}.tmp",
            uuid::Uuid::new_v4()
        )
    }

    fn legacy_backup_name() -> String {
        format!(".reader-upload-backup-{}.tmp", uuid::Uuid::new_v4())
    }

    #[tokio::test]
    async fn sweep_removes_only_stale_upload_temps() {
        let dir = sandbox("sweep-temp");
        let fresh = dir.join(upload_temp_name("fresh.txt"));
        let stale = dir.join(upload_temp_name("stale.txt"));
        let nested_dir = dir.join("sub");
        std::fs::create_dir_all(&nested_dir).unwrap();
        let nested = nested_dir.join(upload_temp_name("nested.txt"));
        for path in [&fresh, &stale, &nested] {
            std::fs::write(path, b"partial").unwrap();
        }
        set_file_age(&stale, 2 * HOUR);
        set_file_age(&nested, 2 * HOUR);

        sweep_webdav_artifacts(&dir, true, HOUR, SEVEN_DAYS).await;

        assert!(fresh.exists(), "1 小时内的临时文件不得删除");
        assert!(!stale.exists(), "超过 1 小时的临时文件应删除");
        assert!(!nested.exists(), "递归扫描应清掉子目录里的超时临时文件");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn sweep_in_single_dir_leaves_nested_artifacts() {
        let dir = sandbox("sweep-flat");
        let top = dir.join(upload_temp_name("top.txt"));
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let nested = sub.join(upload_temp_name("nested.txt"));
        for path in [&top, &nested] {
            std::fs::write(path, b"partial").unwrap();
            set_file_age(path, 2 * HOUR);
        }

        sweep_webdav_artifacts(&dir, false, HOUR, SEVEN_DAYS).await;

        assert!(!top.exists(), "当前目录的超时临时文件应删除");
        assert!(nested.exists(), "非递归清扫不得进入子目录");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn sweep_deletes_redundant_backup_when_target_exists() {
        let dir = sandbox("sweep-backup");
        let target = dir.join("book.epub");
        std::fs::write(&target, b"new").unwrap();
        let stale = dir.join(backup_name("book.epub"));
        std::fs::write(&stale, b"old").unwrap();
        set_file_age(&stale, 2 * HOUR);
        // 1 小时内的备份可能仍属于在途覆盖流程，保留
        let fresh = dir.join(backup_name("book.epub"));
        std::fs::write(&fresh, b"old").unwrap();

        sweep_webdav_artifacts(&dir, true, HOUR, SEVEN_DAYS).await;

        assert!(!stale.exists(), "目标已存在的超时备份应删除");
        assert!(fresh.exists(), "1 小时内的备份不得删除");
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"new",
            "删除冗余备份不得动目标文件"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn sweep_restores_backup_when_target_missing() {
        let dir = sandbox("sweep-restore");
        let target = dir.join("book.epub");
        let backup = dir.join(backup_name("book.epub"));
        std::fs::write(&backup, b"old").unwrap();
        set_file_age(&backup, 2 * HOUR);

        sweep_webdav_artifacts(&dir, true, HOUR, SEVEN_DAYS).await;

        assert!(!backup.exists(), "恢复后备份应改名到目标路径");
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"old",
            "目标缺失时应把备份内容恢复成目标文件，而不是删除唯一副本"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn sweep_deletes_legacy_backup_only_after_seven_days() {
        let dir = sandbox("sweep-legacy");
        let recent = dir.join(legacy_backup_name());
        let old = dir.join(legacy_backup_name());
        for path in [&recent, &old] {
            std::fs::write(path, b"old").unwrap();
        }
        set_file_age(&recent, 2 * HOUR);
        set_file_age(&old, 8 * 24 * HOUR);

        sweep_webdav_artifacts(&dir, true, HOUR, SEVEN_DAYS).await;

        assert!(recent.exists(), "旧格式备份 7 天内不得删除（留给人工恢复）");
        assert!(!old.exists(), "超过 7 天的旧格式备份应删除");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn sweep_ignores_user_files_and_directories() {
        let dir = sandbox("sweep-user");
        let file = dir.join("notes.txt");
        std::fs::write(&file, b"keep").unwrap();
        set_file_age(&file, 30 * 24 * HOUR);
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let nested = sub.join("keep.txt");
        std::fs::write(&nested, b"keep").unwrap();
        set_file_age(&nested, 30 * 24 * HOUR);
        // 名字像临时文件但 UUID 段非法：不是本模块产生的，不能误删
        let lookalike = dir.join("report.upload-2024.tmp");
        std::fs::write(&lookalike, b"keep").unwrap();
        set_file_age(&lookalike, 30 * 24 * HOUR);

        sweep_webdav_artifacts(&dir, true, HOUR, SEVEN_DAYS).await;

        assert!(file.exists(), "普通用户文件不得清理");
        assert!(nested.exists(), "普通子目录内容不得清理");
        assert!(lookalike.exists(), "UUID 非法的相似文件名不得误删");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn sweep_does_not_follow_symlinked_directories() {
        let dir = sandbox("sweep-symlink");
        let outside = sandbox("sweep-symlink-outside");
        let victim = outside.join(upload_temp_name("victim.txt"));
        std::fs::write(&victim, b"partial").unwrap();
        set_file_age(&victim, 2 * HOUR);
        let link = dir.join("linked");
        #[cfg(unix)]
        let created = std::os::unix::fs::symlink(&outside, &link);
        #[cfg(windows)]
        let created = std::os::windows::fs::symlink_dir(&outside, &link);
        if created.is_err() {
            eprintln!("无法创建目录符号链接（Windows 需要开发者模式/权限），跳过");
            let _ = std::fs::remove_dir_all(&dir);
            let _ = std::fs::remove_dir_all(&outside);
            return;
        }

        sweep_webdav_artifacts(&dir, true, HOUR, SEVEN_DAYS).await;

        assert!(
            victim.exists(),
            "不得跟随符号链接递归到 WebDAV 根之外删文件"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
