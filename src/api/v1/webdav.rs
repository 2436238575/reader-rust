//! `/v1` 文件域（WebDAV 存储）：文件浏览（列表/下载/上传/删除）+ 服务端备份与恢复。
//!
//! 协议挂载在 `/v1/webdav`（见 `api/handlers/webdav.rs` 与 `api/router.rs`），
//! 走 HTTP Basic，不在本模块的 JWT/CSRF 层内。本模块只服务浏览器/API 客户端
//! 的会话语义，文件浏览放在 `/v1/me/files`，备份动作放在 `/v1/me/backups`：
//!
//! - `GET /v1/me/files?path=/`：目录列表（强 ETag + 304）；
//! - `POST /v1/me/files?path=/dir`：multipart 上传（多文件、单文件
//!   100MB、字段数 20，与 `/reader3/uploadFileToWebdav` 同一套流式落盘）；
//! - `GET /v1/me/files/{path}`：原样下载（显式 Content-Type、
//!   `Content-Security-Policy: sandbox` 由路由层补）；
//! - `DELETE /v1/me/files/{path}`：递归删除；
//! - `POST /v1/me/files:batchDelete`：批量删除（预校验全部路径）；
//! - `POST /v1/me/backups`：服务端快照到 `/backups/reader-backup-*.json`；
//! - `POST /v1/me/backups:restore`：以备份为准恢复，返回需要前端落地的
//!   localStorage 快照。
//!
//! 备份端点独立于 `/v1/me/files`：文件浏览的 `{path}` 是通配路径，把备份动作
//! 挂在同一前缀下会让 `/v1/me/files/backups` 与一个名为 `backups` 的目录
//! 抢同一个路径（静态路由优先，目录本身再也无法下载/删除）。
//!
//! 这些路由有大文件（下载/上传），在 `v1::router` 的常规超时层之外 merge，
//! 自带 900s 上传超时、100MB 体限、鉴权与 CSRF（与 local-books 同模式）。

use crate::api::handlers::multipart::{next_field_limited, read_limited_multipart_field};
use crate::api::handlers::webdav::{
    build_relative_path, download_content_type, join_parts, normalize_rel_path,
    stream_multipart_field_to_file, sweep_webdav_upload_artifacts_in_dir, webdav_home,
    MAX_WEBDAV_PATH_FIELD_BYTES, MAX_WEBDAV_UPLOAD_BYTES,
};
use crate::api::problem::{ErrorCode, Problem};
use crate::api::v1::{V1Json, V1Multipart, V1Query, V1User};
use crate::api::AppState;
use crate::service::webdav_backup_service::{WebdavBackupDocument, WebdavRestoreResult};
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path as StdPath;
use std::time::Duration;
use tokio::fs;
use utoipa::ToSchema;

/// 批量删除单次上限：路径是短文本，1000 条仍在 1MB 体限内；更大的集合由
/// 客户端分批（前端按 500 分片）。
const MAX_BATCH_DELETE_PATHS: usize = 1000;

pub fn router(state: AppState, upload_timeout: Duration) -> Router<AppState> {
    Router::new()
        .route("/v1/me/files", get(list_files).post(upload_files))
        .route(
            "/v1/me/files/{*path}",
            get(download_file).delete(delete_file),
        )
        .route("/v1/me/files:batchDelete", post(batch_delete_files))
        .route("/v1/me/backups", post(create_backup))
        .route("/v1/me/backups:inspect", post(inspect_backup))
        .route("/v1/me/backups:restore", post(restore_backup))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::api::v1::require_auth,
        ))
        .layer(axum::extract::DefaultBodyLimit::max(
            crate::api::router::MAX_BODY_LIMIT_BYTES,
        ))
        .layer(axum::middleware::from_fn(crate::api::v1::require_csrf))
        // 下载的 .html/.xml/.svg 会以可渲染类型同源直出：这里补 sandbox CSP
        // （与 /reader3/getWebdavFile 同一条中间件；非活性类型无副作用）
        .layer(axum::middleware::from_fn(
            crate::api::router::sandbox_active_webdav_content,
        ))
        .layer(axum::middleware::from_fn_with_state(
            (
                upload_timeout,
                crate::api::inbound_guard::TimeoutShape::Problem,
            ),
            crate::api::inbound_guard::timeout_middleware,
        ))
}

/// 目录/文件条目。`modifiedAt` 与 v1 其他时间字段一致用 RFC 3339 UTC。
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WebdavEntry {
    pub name: String,
    /// 相对用户 WebDAV 根目录的路径（`/backups/a.json`）
    pub path: String,
    pub size: u64,
    pub is_directory: bool,
    #[schema(format = DateTime)]
    pub modified_at: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebdavPathQuery {
    /// 目录路径；缺省 `/`
    pub path: Option<String>,
}

fn internal(error: impl std::fmt::Display) -> Problem {
    tracing::warn!(error = %error, "WebDAV v1 文件操作失败");
    Problem::new(ErrorCode::Internal)
}

fn path_not_found() -> Problem {
    Problem::new(ErrorCode::NotFound).detail("路径不存在")
}

/// 家目录的真实路径（`webdav_home` 保证已创建）。后续目标都必须
/// canonicalize 后落在它下面，防止符号链接把读写带出用户目录。
async fn checked_home(state: &AppState, user_ns: &str) -> Result<std::path::PathBuf, Problem> {
    let home = webdav_home(state, user_ns).await.map_err(Problem::from)?;
    fs::canonicalize(&home).await.map_err(internal)
}

/// canonicalize 目标并确认仍在家目录内；返回真实路径（Windows 下带
/// `\\?\` 前缀，同一个 home 下比较一致）。
async fn ensure_within(home: &StdPath, target: &StdPath) -> Result<std::path::PathBuf, Problem> {
    let canonical = fs::canonicalize(target).await.map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            path_not_found()
        } else {
            internal(error)
        }
    })?;
    if canonical.starts_with(home) {
        Ok(canonical)
    } else {
        Err(path_not_found())
    }
}

/// 删除/写目标：只 canonicalize 父目录（目标本身可能不存在），返回
/// `(真实父目录, 文件/目录名)`。
async fn checked_parent(
    home: &StdPath,
    parts: &[String],
) -> Result<(std::path::PathBuf, String), Problem> {
    let Some((name, parent_parts)) = parts.split_last() else {
        return Err(Problem::new(ErrorCode::MalformedRequest).detail("path required"));
    };
    let parent = ensure_within(home, &join_parts(home, parent_parts)).await?;
    Ok((parent, name.clone()))
}

fn modified_at(meta: &std::fs::Metadata) -> String {
    let secs = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    crate::util::time::rfc3339_secs(secs)
}

fn entry_of(name: &str, path: String, meta: &std::fs::Metadata) -> WebdavEntry {
    WebdavEntry {
        name: name.to_string(),
        path,
        size: meta.len(),
        is_directory: meta.is_dir(),
        modified_at: modified_at(meta),
    }
}

/// 目录列表（`?path=`，缺省根目录）。强 ETag 对响应字节取 md5，命中
/// `If-None-Match` 回 304。
#[utoipa::path(
    get,
    path = "/v1/me/files",
    tag = "files",
    operation_id = "listFiles",
    params(("path" = Option<String>, Query, description = "目录路径，缺省 /")),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "目录条目（隐藏文件不列出）", body = [WebdavEntry]),
        (status = 304, description = "If-None-Match 命中，无 body"),
        (status = 400, description = "路径非法", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "目录不存在", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn list_files(
    State(state): State<AppState>,
    V1User(user): V1User,
    V1Query(query): V1Query<WebdavPathQuery>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    let home = checked_home(&state, &user.ns).await?;
    let raw = query.path.unwrap_or_else(|| "/".to_string());
    let parts = normalize_rel_path(&raw).map_err(Problem::from)?;
    let dir = ensure_within(&home, &join_parts(&home, &parts)).await?;
    let meta = fs::metadata(&dir).await.map_err(internal)?;
    if !meta.is_dir() {
        return Err(Problem::new(ErrorCode::MalformedRequest).detail("路径不是目录"));
    }
    // 顺手清扫本层上传残留（与 /reader3 列表同口径），不让崩溃留下的 tmp
    // 一直占着目录
    sweep_webdav_upload_artifacts_in_dir(&dir).await;

    let mut entries = Vec::new();
    let mut reader = fs::read_dir(&dir).await.map_err(internal)?;
    while let Some(item) = reader.next_entry().await.map_err(internal)? {
        let name = item.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        // 符号链接不列出也不跟随：读路径都在家目录内，链接可能指向外面
        let file_type = match item.file_type().await {
            Ok(file_type) => file_type,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(internal(error)),
        };
        if file_type.is_symlink() {
            continue;
        }
        let meta = match item.metadata().await {
            Ok(meta) => meta,
            // 列目录期间被删：跳过而不是整页失败
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(internal(error)),
        };
        entries.push(entry_of(&name, build_relative_path(&parts, &name), &meta));
    }
    entries.sort_by(|a, b| {
        b.is_directory
            .cmp(&a.is_directory)
            .then_with(|| a.name.cmp(&b.name))
    });

    let body = serde_json::to_vec(&entries).expect("webdav entries serialize");
    let etag = crate::api::etag::strong_etag(&body);
    let mut response =
        if crate::api::etag::if_none_match_matches(headers.get(header::IF_NONE_MATCH), &etag) {
            StatusCode::NOT_MODIFIED.into_response()
        } else {
            ([(header::CONTENT_TYPE, "application/json")], body).into_response()
        };
    response.headers_mut().insert(header::ETAG, etag);
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-cache"),
    );
    Ok(response)
}

/// 下载文件（流式，不整读进内存）。显式 Content-Type 与 `sandbox` CSP
/// （路由层补）避免备份 JSON/HTML 被浏览器当页面执行。
#[utoipa::path(
    get,
    path = "/v1/me/files/{path}",
    tag = "files",
    operation_id = "downloadFile",
    params(("path" = String, Path, description = "相对用户根目录的文件路径（可含 /）")),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "文件内容（流式）"),
        (status = 304, description = "If-None-Match 命中，无 body"),
        (status = 400, description = "路径非法", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "文件不存在或是目录", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn download_file(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(raw): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    let home = checked_home(&state, &user.ns).await?;
    if raw.trim_matches('/').is_empty() {
        return Err(path_not_found());
    }
    let parts = normalize_rel_path(&raw).map_err(Problem::from)?;
    let full = ensure_within(&home, &join_parts(&home, &parts)).await?;
    let meta = fs::metadata(&full).await.map_err(internal)?;
    if meta.is_dir() {
        return Err(path_not_found());
    }
    // 元数据 ETag：mtime + size，足够区分内容版本（文件要么整体替换）。
    let etag = HeaderValue::from_str(&format!(
        "\"{:x}-{:x}\"",
        meta.modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos())
            .unwrap_or(0),
        meta.len()
    ))
    .map_err(internal)?;
    if crate::api::etag::if_none_match_matches(headers.get(header::IF_NONE_MATCH), &etag) {
        let mut response = StatusCode::NOT_MODIFIED.into_response();
        response.headers_mut().insert(header::ETAG, etag);
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-cache"),
        );
        return Ok(response);
    }

    let file = fs::File::open(&full).await.map_err(internal)?;
    let mut response = Response::new(Body::from_stream(tokio_util::io::ReaderStream::new(file)));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(download_content_type(&full)),
    );
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(meta.len()));
    headers.insert(header::ETAG, etag);
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-cache"),
    );
    Ok(response)
}

/// multipart 上传的表单形态（只服务 OpenAPI 文档；运行时由 `V1Multipart`
/// 逐字段解析）。`file` 对应可重复的文件字段。
#[derive(Debug, ToSchema)]
#[allow(dead_code)] // 仅用于 OpenAPI schema，运行时不构造
pub struct WebdavUploadForm {
    #[schema(value_type = Vec<String>, format = Binary)]
    pub file: Vec<String>,
}

/// multipart 上传：`?path=` 是目标目录，`file` 字段可重复（最多 20 个字段）。
/// 目标目录不存在时自动创建（与 `/reader3/uploadFileToWebdav` 一致）。
#[utoipa::path(
    post,
    path = "/v1/me/files",
    tag = "files",
    operation_id = "uploadFiles",
    params(("path" = Option<String>, Query, description = "目标目录，缺省 /")),
    request_body(content = WebdavUploadForm, content_type = "multipart/form-data"),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 201, description = "已写入条目", body = [WebdavEntry]),
        (status = 400, description = "缺少文件字段、文件名非法或超过单文件 100MB", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn upload_files(
    State(state): State<AppState>,
    V1User(user): V1User,
    V1Query(query): V1Query<WebdavPathQuery>,
    V1Multipart(mut multipart): V1Multipart,
) -> Result<(StatusCode, Json<Vec<WebdavEntry>>), Problem> {
    let home = checked_home(&state, &user.ns).await?;
    let has_query_path = query.path.is_some();
    let mut path = query.path.unwrap_or_else(|| "/".to_string());
    let mut parts = normalize_rel_path(&path).map_err(Problem::from)?;
    let raw_dir = join_parts(&home, &parts);
    fs::create_dir_all(&raw_dir).await.map_err(internal)?;
    let mut dir = ensure_within(&home, &raw_dir).await?;
    sweep_webdav_upload_artifacts_in_dir(&dir).await;

    let mut entries = Vec::new();
    let mut field_count = 0usize;
    while let Some(field) = next_field_limited(&mut multipart, &mut field_count)
        .await
        .map_err(Problem::from)?
    {
        if field.name().unwrap_or_default() == "path" {
            if !has_query_path {
                let bytes =
                    read_limited_multipart_field(field, MAX_WEBDAV_PATH_FIELD_BYTES, "路径过长")
                        .await?;
                let value = String::from_utf8_lossy(&bytes).to_string();
                if !value.is_empty() {
                    path = value;
                    parts = normalize_rel_path(&path).map_err(Problem::from)?;
                    let raw_dir = join_parts(&home, &parts);
                    fs::create_dir_all(&raw_dir).await.map_err(internal)?;
                    dir = ensure_within(&home, &raw_dir).await?;
                }
            }
            continue;
        }
        // 只接受文件字段：普通文本字段没有 filename，按「file 缺失」处理，
        // 不再悄悄落一个名为 file 的文件
        let Some(filename) = field.file_name().map(str::to_string) else {
            return Err(Problem::new(ErrorCode::MalformedRequest).detail("只接受文件字段"));
        };
        let Some(filename) = crate::util::safe_path::sanitize_file_name(&filename) else {
            return Err(Problem::new(ErrorCode::MalformedRequest).detail("非法文件名"));
        };
        let target = dir.join(&filename);
        stream_multipart_field_to_file(field, &target, MAX_WEBDAV_UPLOAD_BYTES)
            .await
            .map_err(Problem::from)?;
        let meta = fs::metadata(&target).await.map_err(internal)?;
        entries.push(entry_of(
            &filename,
            build_relative_path(&parts, &filename),
            &meta,
        ));
    }
    if entries.is_empty() {
        return Err(Problem::new(ErrorCode::MalformedRequest).detail("至少需要一个文件字段"));
    }
    Ok((StatusCode::CREATED, Json(entries)))
}

/// 递归删除文件或目录；不存在的路径按 404（幂等重试由客户端决定）。
#[utoipa::path(
    delete,
    path = "/v1/me/files/{path}",
    tag = "files",
    operation_id = "deleteFile",
    params(("path" = String, Path, description = "相对用户根目录的路径（可含 /）")),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 204, description = "已删除"),
        (status = 400, description = "路径为空或非法", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "路径不存在", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn delete_file(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(raw): Path<String>,
) -> Result<StatusCode, Problem> {
    let home = checked_home(&state, &user.ns).await?;
    if raw.trim_matches('/').is_empty() {
        return Err(Problem::new(ErrorCode::MalformedRequest).detail("path required"));
    }
    let parts = normalize_rel_path(&raw).map_err(Problem::from)?;
    let (parent, name) = checked_parent(&home, &parts).await?;
    let target = parent.join(name);
    let meta = fs::symlink_metadata(&target).await.map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            path_not_found()
        } else {
            internal(error)
        }
    })?;
    let result = if meta.is_dir() {
        fs::remove_dir_all(&target).await
    } else {
        fs::remove_file(&target).await
    };
    result.map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebdavBatchDeleteRequest {
    pub paths: Vec<String>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WebdavBatchDeleteResult {
    pub deleted: u32,
    pub missing: u32,
}

/// 批量删除：先校验全部路径（任一非法整批 400，避免删一半才发现），
/// 不存在的计入 `missing`。前端按 500 条分片调用。
#[utoipa::path(
    post,
    path = "/v1/me/files:batchDelete",
    tag = "files",
    operation_id = "batchDeleteFiles",
    request_body = WebdavBatchDeleteRequest,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "删除/缺失条数", body = WebdavBatchDeleteResult),
        (status = 400, description = "路径为空、超上限或含非法路径", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn batch_delete_files(
    State(state): State<AppState>,
    V1User(user): V1User,
    V1Json(request): V1Json<WebdavBatchDeleteRequest>,
) -> Result<Json<WebdavBatchDeleteResult>, Problem> {
    if request.paths.is_empty() || request.paths.len() > MAX_BATCH_DELETE_PATHS {
        return Err(Problem::new(ErrorCode::MalformedRequest)
            .detail(format!("paths 需要提供 1 到 {MAX_BATCH_DELETE_PATHS} 个")));
    }
    let home = checked_home(&state, &user.ns).await?;
    let mut parts_list = Vec::with_capacity(request.paths.len());
    for raw in &request.paths {
        if raw.trim_matches('/').is_empty() {
            return Err(Problem::new(ErrorCode::MalformedRequest).detail("path 不能为空"));
        }
        parts_list.push(normalize_rel_path(raw).map_err(Problem::from)?);
    }

    // 先解析全部父目录（符号链接逃逸整批 404，不删一半）；父目录不存在
    // 记为 missing，留给下一阶段计数。
    let mut targets: Vec<Option<std::path::PathBuf>> = Vec::with_capacity(parts_list.len());
    let mut missing = 0u32;
    for parts in parts_list {
        let Some((name, parent_parts)) = parts.split_last() else {
            return Err(Problem::new(ErrorCode::MalformedRequest).detail("path 不能为空"));
        };
        let raw_parent = join_parts(&home, parent_parts);
        match fs::canonicalize(&raw_parent).await {
            Ok(parent) if parent.starts_with(&home) => {
                targets.push(Some(parent.join(name)));
            }
            Ok(_) => return Err(path_not_found()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                targets.push(None);
                missing += 1;
            }
            Err(error) => return Err(internal(error)),
        }
    }

    let mut deleted = 0u32;
    for target in targets.into_iter().flatten() {
        let meta = match fs::symlink_metadata(&target).await {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing += 1;
                continue;
            }
            Err(error) => return Err(internal(error)),
        };
        let result = if meta.is_dir() {
            fs::remove_dir_all(&target).await
        } else {
            fs::remove_file(&target).await
        };
        match result {
            Ok(()) => deleted += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => missing += 1,
            Err(error) => return Err(internal(error)),
        }
    }
    Ok(Json(WebdavBatchDeleteResult { deleted, missing }))
}

/// 创建备份的可选本地配置快照。服务端不知道浏览器 localStorage，必须由
/// 前端把白名单键放进来；空对象表示「这些键当前都没有值」。
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct BackupCreateRequest {
    /// 缺省 = 这份备份不含浏览器本地配置（恢复时不动 localStorage）
    pub local_state: Option<BTreeMap<String, String>>,
}

/// 生成服务端备份：快照当前账号的书架/分组/书源/书签/替换规则，原子写入
/// `/backups/reader-backup-<UTC 时间>-<随机后缀>.json`。
#[utoipa::path(
    post,
    path = "/v1/me/backups",
    tag = "files",
    operation_id = "createBackup",
    request_body = BackupCreateRequest,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 201, description = "已写入的备份文件", body = WebdavEntry),
        (status = 400, description = "请求体含未知字段", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn create_backup(
    State(state): State<AppState>,
    V1User(user): V1User,
    V1Json(request): V1Json<BackupCreateRequest>,
) -> Result<(StatusCode, Json<WebdavEntry>), Problem> {
    let document = state
        .webdav_backup_service
        .snapshot(&user.ns, request.local_state)
        .await
        .map_err(Problem::from)?;
    let body = serde_json::to_vec_pretty(&document).map_err(internal)?;
    let home = checked_home(&state, &user.ns).await?;
    let raw_dir = home.join("backups");
    fs::create_dir_all(&raw_dir).await.map_err(internal)?;
    let dir = ensure_within(&home, &raw_dir).await?;

    let stem = format!(
        "reader-backup-{}",
        chrono::Utc::now().format("%Y%m%d-%H%M%S")
    );
    // 同一秒并发创建时带随机后缀，不依赖 check-then-write 的竞态窗口
    let mut name = format!(
        "{stem}-{}.json",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let mut suffix = 2u32;
    while fs::symlink_metadata(dir.join(&name)).await.is_ok() {
        name = format!(
            "{stem}-{}-{suffix}.json",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        );
        suffix += 1;
    }
    let target = dir.join(&name);
    crate::util::atomic_write::write_atomic(&target, &body)
        .await
        .map_err(internal)?;
    let meta = fs::metadata(&target).await.map_err(internal)?;
    Ok((
        StatusCode::CREATED,
        Json(entry_of(&name, format!("/backups/{name}"), &meta)),
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RestoreBackupRequest {
    /// 备份文件路径（相对用户 WebDAV 根目录）
    #[schema(required = true)]
    pub path: String,
}

/// 备份文件摘要：恢复前的确认框用它在点下去之前展示条目数。
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BackupInspectResult {
    pub version: u32,
    #[schema(format = DateTime)]
    pub created_at: String,
    pub books: u32,
    pub groups: u32,
    pub book_sources: u32,
    pub bookmarks: u32,
    pub replace_rules: u32,
    /// 备份是否包含浏览器本地配置（null/缺失 = 不含）
    pub has_local_state: bool,
}

/// 读取并严格解析备份文件。恢复与 inspect 共用同一套路径/大小/JSON 校验，
/// 避免「inspect 通过、restore 才发现非法」。
async fn read_backup_document(
    state: &AppState,
    user_ns: &str,
    path: &str,
) -> Result<WebdavBackupDocument, Problem> {
    if path.trim_matches('/').is_empty() {
        return Err(Problem::new(ErrorCode::MalformedRequest).detail("path required"));
    }
    let home = checked_home(state, user_ns).await?;
    let parts = normalize_rel_path(path).map_err(Problem::from)?;
    let target = ensure_within(&home, &join_parts(&home, &parts)).await?;
    let meta = fs::metadata(&target).await.map_err(internal)?;
    if meta.is_dir() {
        return Err(Problem::new(ErrorCode::MalformedRequest).detail("路径不是文件"));
    }
    if meta.len() > MAX_WEBDAV_UPLOAD_BYTES as u64 {
        return Err(Problem::new(ErrorCode::PayloadTooLarge).detail("备份文件不能超过 100MB"));
    }
    let bytes = fs::read(&target).await.map_err(internal)?;
    serde_json::from_slice(&bytes).map_err(|error| {
        Problem::new(ErrorCode::ValidationFailed)
            .detail(format!("备份文件不是合法 JSON 或字段不完整：{error}"))
    })
}

/// 恢复前预览备份内容（不写库）：确认框展示各域条数，空书源等异常在
/// 用户点「确定」之前可见。
#[utoipa::path(
    post,
    path = "/v1/me/backups:inspect",
    tag = "files",
    operation_id = "inspectBackup",
    request_body = RestoreBackupRequest,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "备份摘要", body = BackupInspectResult),
        (status = 400, description = "路径为空/非法", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "备份文件不存在", body = Problem, content_type = "application/problem+json"),
        (status = 413, description = "备份文件超过 100MB", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "备份 JSON 非法、必填字段缺失或类型不符", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn inspect_backup(
    State(state): State<AppState>,
    V1User(user): V1User,
    V1Json(request): V1Json<RestoreBackupRequest>,
) -> Result<Json<BackupInspectResult>, Problem> {
    let document = read_backup_document(&state, &user.ns, &request.path).await?;
    Ok(Json(BackupInspectResult {
        version: document.version,
        created_at: document.created_at,
        books: document.bookshelf.books.len() as u32,
        groups: document.bookshelf.groups.len() as u32,
        book_sources: document.book_sources.len() as u32,
        bookmarks: document.bookmarks.len() as u32,
        replace_rules: document.replace_rules.len() as u32,
        has_local_state: document.local_state.is_some(),
    }))
}

/// 以备份为准恢复：删除当前书架/分组/书源/书签/替换规则后按文档重建。
/// 返回各域条数与备份里的 localStorage 快照（前端只应用已知键）。
#[utoipa::path(
    post,
    path = "/v1/me/backups:restore",
    tag = "files",
    operation_id = "restoreBackup",
    request_body = RestoreBackupRequest,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "恢复结果", body = WebdavRestoreResult),
        (status = 400, description = "路径为空/非法、备份版本不支持或书籍缺少 bookUrl", body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "已有恢复任务正在执行", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "备份文件不存在", body = Problem, content_type = "application/problem+json"),
        (status = 413, description = "备份文件超过 100MB", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "备份 JSON 非法、必填字段缺失或类型不符", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn restore_backup(
    State(state): State<AppState>,
    V1User(user): V1User,
    V1Json(request): V1Json<RestoreBackupRequest>,
) -> Result<Json<WebdavRestoreResult>, Problem> {
    let document = read_backup_document(&state, &user.ns, &request.path).await?;
    let result = state
        .webdav_backup_service
        .restore(&user.ns, document)
        .await
        .map_err(Problem::from)?;
    Ok(Json(result))
}
