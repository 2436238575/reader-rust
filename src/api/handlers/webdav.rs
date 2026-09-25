use crate::api::handlers::multipart::read_limited_multipart_field;
use crate::api::AppState;
use crate::auth::CurrentUser;
use crate::error::error::{ApiResponse, AppError};
use crate::util::time::now_ts;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{
    body::Bytes,
    extract::{ConnectInfo, Multipart, Path, Query, State},
    Json,
};
use base64::Engine;
use serde::Deserialize;
use serde_json::Value;
use std::path::PathBuf;
use tokio::fs;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct WebdavPathRequest {
    pub path: Option<String>,
}

/// WebDAV 上传单文件上限（multipart 字段不受 DefaultBodyLimit 约束，必须自行限量）。
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
    let user_ns = user.0.require_webdav_ns()?.to_string();
    let home = webdav_home(&state, &user_ns).await?;
    let path = req.path.unwrap_or_else(|| "/".to_string());
    let parts = normalize_rel_path(&path)?;
    let full = join_parts(&home, &parts);
    if !full.exists() {
        return Ok(Json(ApiResponse::err("路径不存在")));
    }
    if !full.is_dir() {
        return Ok(Json(ApiResponse::err("路径不是目录")));
    }
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
    let user_ns = user.0.require_webdav_ns()?.to_string();
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
    Ok(Response::new(axum::body::Body::from_stream(
        tokio_util::io::ReaderStream::new(file),
    )))
}

pub async fn upload_file_to_webdav(
    State(state): State<AppState>,
    user: CurrentUser,
    mut multipart: Multipart,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.require_webdav_ns()?.to_string();
    let home = webdav_home(&state, &user_ns).await?;
    let mut file_list = Vec::new();
    let mut path = "/".to_string();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?
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
        let data =
            read_limited_multipart_field(field, MAX_WEBDAV_UPLOAD_BYTES, "文件不能超过 100MB")
                .await?;
        let rel = normalize_rel_path(&path)?;
        let dir = join_parts(&home, &rel);
        fs::create_dir_all(&dir)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
        let target = dir.join(&filename);
        fs::write(&target, data)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
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

pub async fn delete_webdav_file(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<WebdavPathRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.require_webdav_ns()?.to_string();
    let home = webdav_home(&state, &user_ns).await?;
    let path = req.path.unwrap_or_default();
    if path.is_empty() {
        return Ok(Json(ApiResponse::err("参数错误")));
    }
    let rel = normalize_rel_path(&path)?;
    let target = join_parts(&home, &rel);
    if !target.exists() {
        return Ok(Json(ApiResponse::err("路径不存在")));
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
    let user_ns = user.0.require_webdav_ns()?.to_string();
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

pub async fn webdav_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    method: Method,
    Path(path): Path<String>,
    body: Bytes,
) -> Response {
    // Basic 认证与登录接口共享限速，按对端 IP 记账
    let client_ip = addr.ip().to_string();
    let user_ns = match resolve_webdav_user(&state, &headers, Some(&client_ip)).await {
        Ok(ns) => ns,
        Err(status) => return status.into_response(),
    };
    let home = match webdav_home(&state, &user_ns).await {
        Ok(h) => h,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let rel_path = format!("/{}", path);
    let rel = match normalize_rel_path(&rel_path) {
        Ok(p) => p,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let full = join_parts(&home, &rel);

    match method.as_str() {
        "OPTIONS" => StatusCode::OK.into_response(),
        "PROPFIND" => webdav_propfind(&full, &rel_path).await,
        "MKCOL" => webdav_mkcol(&full).await,
        "PUT" => webdav_put(&full, body).await,
        "GET" => webdav_get(&full).await,
        "DELETE" => webdav_delete(&full).await,
        "MOVE" => webdav_move(&home, &full, &headers).await,
        "COPY" => webdav_copy(&home, &full, &headers).await,
        "LOCK" => webdav_lock(&rel_path),
        "UNLOCK" => webdav_unlock(&headers),
        _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
    }
}

async fn resolve_webdav_user(
    state: &AppState,
    headers: &HeaderMap,
    client_ip: Option<&str>,
) -> Result<String, StatusCode> {
    let auth = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !auth.to_ascii_lowercase().starts_with("basic ") {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let b64 = auth[6..].trim();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    let decoded = String::from_utf8_lossy(&decoded);
    let parts: Vec<&str> = decoded.splitn(2, ':').collect();
    if parts.len() != 2 {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let username = parts[0];
    let password = parts[1];
    match state
        .user_service
        .verify_basic_webdav(username, password, client_ip)
        .await
    {
        Ok(Some(_)) => Ok(username.to_string()),
        _ => Err(StatusCode::UNAUTHORIZED),
    }
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

fn join_parts(home: &PathBuf, parts: &Vec<String>) -> PathBuf {
    let mut p = home.clone();
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

async fn webdav_propfind(full: &PathBuf, rel: &str) -> Response {
    if !full.exists() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let mut response =
        String::from(r#"<?xml version="1.0" encoding="utf-8"?><D:multistatus xmlns:D="DAV:">"#);
    let add_entry = |resp: &mut String, href: String, is_dir: bool, size: u64, modified: String| {
        if is_dir {
            resp.push_str(&format!(r#"<D:response><D:href>{}</D:href><D:propstat><D:status>HTTP/1.1 200 OK</D:status><D:prop><D:getlastmodified>{}</D:getlastmodified><D:creationdate>{}</D:creationdate><D:resourcetype><D:collection /></D:resourcetype><D:displayname></D:displayname></D:prop></D:propstat></D:response>"#, href, modified, modified));
        } else {
            resp.push_str(&format!(r#"<D:response><D:href>{}</D:href><D:propstat><D:status>HTTP/1.1 200 OK</D:status><D:prop><D:getlastmodified>{}</D:getlastmodified><D:creationdate>{}</D:creationdate><D:resourcetype /><D:displayname></D:displayname><D:getcontentlength>{}</D:getcontentlength></D:prop></D:propstat></D:response>"#, href, modified, modified, size));
        }
    };
    let href_base = if rel.ends_with('/') {
        rel.to_string()
    } else {
        format!("{}/", rel)
    };
    let meta = std::fs::metadata(full).ok();
    let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    let modified = meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    add_entry(
        &mut response,
        href_base.clone(),
        full.is_dir(),
        size,
        modified.to_string(),
    );
    if full.is_dir() {
        if let Ok(entries) = std::fs::read_dir(full) {
            for entry in entries.flatten() {
                let file = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    continue;
                }
                let href = format!("{}{}", href_base, name);
                let meta = entry.metadata().ok();
                let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                let modified = meta
                    .as_ref()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                add_entry(
                    &mut response,
                    href,
                    file.is_dir(),
                    size,
                    modified.to_string(),
                );
            }
        }
    }
    response.push_str("</D:multistatus>");
    let mut resp = Response::new(axum::body::Body::from(response));
    *resp.status_mut() = StatusCode::MULTI_STATUS;
    resp
}

async fn webdav_mkcol(full: &PathBuf) -> Response {
    if full.exists() {
        return StatusCode::CREATED.into_response();
    }
    match fs::create_dir_all(full).await {
        Ok(_) => StatusCode::CREATED.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn webdav_put(full: &PathBuf, body: Bytes) -> Response {
    if let Some(parent) = full.parent() {
        if !parent.exists() {
            return StatusCode::CONFLICT.into_response();
        }
    }
    if full.exists() && full.is_dir() {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    if let Err(_) = fs::write(full, body).await {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    StatusCode::CREATED.into_response()
}

async fn webdav_get(full: &PathBuf) -> Response {
    if !full.exists() {
        return StatusCode::NOT_FOUND.into_response();
    }
    if full.is_dir() {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    // 流式返回，避免大文件整读入内存
    match fs::File::open(full).await {
        Ok(file) => Response::new(axum::body::Body::from_stream(
            tokio_util::io::ReaderStream::new(file),
        )),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn webdav_delete(full: &PathBuf) -> Response {
    if !full.exists() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let res = if full.is_dir() {
        fs::remove_dir_all(full).await
    } else {
        fs::remove_file(full).await
    };
    match res {
        Ok(_) => StatusCode::OK.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// 解析 MOVE/COPY 的 `Destination` 头为 home 下的目标路径。
///
/// Destination 必须带 `/reader3/webdav/` 前缀：缺前缀时若按空相对路径解析，
/// 目标会塌缩成家目录本身，配合 `Overwrite` 头等于把整个家目录清空。
fn resolve_destination(home: &PathBuf, headers: &HeaderMap) -> Result<PathBuf, StatusCode> {
    let destination = headers
        .get("Destination")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let Some(dest_path) = destination.split("/reader3/webdav/").nth(1) else {
        return Err(StatusCode::BAD_REQUEST);
    };
    let rel =
        normalize_rel_path(&format!("/{}", dest_path)).map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(join_parts(home, &rel))
}

async fn webdav_move(home: &PathBuf, full: &PathBuf, headers: &HeaderMap) -> Response {
    let dest = match resolve_destination(home, headers) {
        Ok(p) => p,
        Err(status) => return status.into_response(),
    };
    if dest.exists() {
        let overwrite = headers
            .get("Overwrite")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if overwrite.is_empty() {
            return StatusCode::PRECONDITION_FAILED.into_response();
        }
        let _ = if dest.is_dir() {
            std::fs::remove_dir_all(&dest)
        } else {
            std::fs::remove_file(&dest)
        };
    }
    match std::fs::rename(full, dest) {
        Ok(_) => StatusCode::CREATED.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn webdav_copy(home: &PathBuf, full: &PathBuf, headers: &HeaderMap) -> Response {
    let dest = match resolve_destination(home, headers) {
        Ok(p) => p,
        Err(status) => return status.into_response(),
    };
    if dest.exists() {
        let overwrite = headers
            .get("Overwrite")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if overwrite.is_empty() {
            return StatusCode::PRECONDITION_FAILED.into_response();
        }
        let _ = if dest.is_dir() {
            std::fs::remove_dir_all(&dest)
        } else {
            std::fs::remove_file(&dest)
        };
    }
    let res = if full.is_dir() {
        copy_dir(full, &dest)
    } else {
        std::fs::copy(full, &dest).map(|_| ())
    };
    match res {
        Ok(_) => StatusCode::CREATED.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

fn copy_dir(src: &PathBuf, dst: &PathBuf) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let dest = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &dest)?;
        } else {
            std::fs::copy(&path, &dest)?;
        }
    }
    Ok(())
}

fn webdav_lock(path: &str) -> Response {
    let lock_token = format!("urn:uuid:{}", Uuid::new_v4());
    let response = format!(
        r#"<?xml version="1.0" encoding="utf-8"?><D:prop xmlns:D="DAV:"><D:lockdiscovery><D:activelock><D:locktype><write /></D:locktype><D:lockscope><exclusive /></D:lockscope><D:locktoken><D:href>{}</D:href></D:locktoken><D:lockroot><D:href>{}</D:href></D:lockroot><D:depth>infinity</D:depth><D:timeout>Second-3600</D:timeout></D:activelock></D:lockdiscovery></D:prop>"#,
        lock_token, path
    );
    let mut resp = Response::new(axum::body::Body::from(response));
    if let Ok(v) = lock_token.parse() {
        resp.headers_mut().insert("Lock-Token", v);
    }
    *resp.status_mut() = StatusCode::OK;
    resp
}

fn webdav_unlock(headers: &HeaderMap) -> Response {
    let lock_token = headers
        .get("Lock-Token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let mut resp = StatusCode::NO_CONTENT.into_response();
    if let Ok(v) = lock_token.parse() {
        resp.headers_mut().insert("Lock-Token", v);
    }
    resp
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
}
