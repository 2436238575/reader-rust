use crate::api::handlers::multipart::read_limited_multipart_field;
use crate::api::AppState;
use crate::auth::CurrentUser;
use crate::error::error::{ApiResponse, AppError};
use crate::util::time::now_ts;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{
    extract::{ConnectInfo, Multipart, Path, Query, Request, State},
    Json,
};
use base64::Engine;
use dav_server::{localfs::LocalFs, memls::MemLs, DavHandler};
use http_body_util::{BodyExt, Limited};
use serde::Deserialize;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::OnceLock;
use tokio::fs;

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
    Ok(Response::new(axum::body::Body::from_stream(
        tokio_util::io::ReaderStream::new(file),
    )))
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
    let user_ns = user.0.ns.clone();
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
    // dav-server 自己会拦路径逃逸（`..` 越级、段内 %2f），但不认识 Windows 的
    // 盘符/设备名/ADS/结尾点空格，分发前按原有规则再拦一遍（Path 通配值已
    // percent-decode）。裸根路径 /reader3/webdav 无通配参数，path 为 None。
    let rel = format!("/{}", path.map(|p| p.0).unwrap_or_default());
    if normalize_rel_path(&rel).is_err() {
        return StatusCode::BAD_REQUEST.into_response();
    }

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
