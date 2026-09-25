use crate::api::AppState;
use crate::auth::{CurrentUser, MaybeUser};
use axum::{
    extract::{ConnectInfo, Multipart, Query, State},
    Json,
};
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio::fs;

use crate::error::error::{ApiResponse, AppError};
use crate::util::safe_path;

/// 上传资源文件的单文件上限（multipart 字段不受 DefaultBodyLimit 约束，需自行限量）。
const MAX_UPLOAD_FILE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: Option<String>,
    pub password: Option<String>,
    #[serde(rename = "isLogin")]
    pub is_login: Option<bool>,
    pub code: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct FileTypeQuery {
    #[serde(rename = "type")]
    pub file_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AddUserRequest {
    pub username: Option<String>,
    pub password: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ResetPasswordRequest {
    pub username: Option<String>,
    pub password: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ChangePasswordRequest {
    #[serde(rename = "oldPassword")]
    pub old_password: Option<String>,
    #[serde(rename = "newPassword")]
    pub new_password: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateUserRequest {
    pub username: Option<String>,
    #[serde(rename = "enableWebdav")]
    pub enable_webdav: Option<bool>,
    #[serde(rename = "enableLocalStore")]
    pub enable_local_store: Option<bool>,
    #[serde(rename = "enableAiModel")]
    pub enable_ai_model: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct DeleteFileRequest {
    pub url: Option<String>,
}

/// 公开端点：登录与注册。成功时返回含 JWT 的用户信息。
pub async fn login(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let username = req.username.unwrap_or_default();
    let password = req.password.unwrap_or_default();
    let is_login = req.is_login.unwrap_or(false);
    let is_new_user = !is_login && !username.is_empty(); // registration attempt
                                                         // 限速按对端 IP 记账（ConnectInfo 由 into_make_service_with_connect_info 注入）
    let client_ip = addr.ip().to_string();
    let data = state
        .user_service
        .login(
            &username,
            &password,
            is_login,
            req.code.as_deref(),
            Some(&client_ip),
        )
        .await?;
    // If this was a new user registration, copy default book sources
    if is_new_user {
        let _ = state
            .book_source_service
            .copy_default_to_user(&username)
            .await;
    }
    Ok(Json(ApiResponse::ok(data)))
}

/// 登出。
///
/// 令牌是无状态的，服务端没有可撤销的会话记录——这里只作为客户端
/// 「清除本地令牌」的确认端点，因此挂在可选鉴权路由上：即使令牌已过期
/// 也返回成功，前端不必为此走一次失败分支。
pub async fn logout() -> Result<Json<ApiResponse<Value>>, AppError> {
    Ok(Json(ApiResponse::ok(Value::String("".to_string()))))
}

/// 可选鉴权：未登录时返回 `userInfo: null`，不报错。
pub async fn get_user_info(
    State(state): State<AppState>,
    MaybeUser(user): MaybeUser,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let data = state.user_service.get_user_info(user.as_ref()).await?;
    Ok(Json(ApiResponse::ok(data)))
}

pub async fn save_user_config(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<Value>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    state
        .user_service
        .save_user_config(&user.0.ns, body)
        .await?;
    Ok(Json(ApiResponse::ok(Value::String("".to_string()))))
}

pub async fn get_user_config(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let cfg = state.user_service.get_user_config(&user.0.ns).await?;
    Ok(Json(ApiResponse::ok(cfg)))
}

/// 管理员专用：路由层已挂 `require_admin`。
pub async fn get_user_list(
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let list = state.user_service.get_user_list().await?;
    Ok(Json(ApiResponse::ok(Value::from(list))))
}

/// 管理员专用：路由层已挂 `require_admin`。
pub async fn add_user(
    State(state): State<AppState>,
    Json(req): Json<AddUserRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let username = req.username.unwrap_or_default();
    let password = req.password.unwrap_or_default();
    let list = state.user_service.add_user(&username, &password).await?;
    Ok(Json(ApiResponse::ok(Value::from(list))))
}

/// 管理员专用：路由层已挂 `require_admin`。
pub async fn reset_password(
    State(state): State<AppState>,
    Json(req): Json<ResetPasswordRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let username = req.username.unwrap_or_default();
    let password = req.password.unwrap_or_default();
    state
        .user_service
        .reset_password(&username, &password)
        .await?;
    Ok(Json(ApiResponse::ok(Value::String("".to_string()))))
}

/// 修改自己的密码；响应携带换发的新令牌。
///
/// 改密码会自增撤销版本号，令其他设备上的令牌立即失效；当前设备用
/// 返回的新令牌继续使用，避免用户改完密码就被登出。
pub async fn change_password(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<ChangePasswordRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let old_password = req.old_password.unwrap_or_default();
    let new_password = req.new_password.unwrap_or_default();
    if old_password.is_empty() || new_password.is_empty() {
        return Err(AppError::BadRequest("请填写当前密码和新密码".to_string()));
    }
    let data = state
        .user_service
        .change_password(&user.0.username, &old_password, &new_password)
        .await?;
    Ok(Json(ApiResponse::ok(data)))
}

/// 管理员专用：路由层已挂 `require_admin`。
pub async fn delete_users(
    State(state): State<AppState>,
    Json(list): Json<Vec<String>>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let users = state.user_service.delete_users(&list).await?;
    Ok(Json(ApiResponse::ok(Value::from(users))))
}

/// 管理员专用：路由层已挂 `require_admin`。
pub async fn update_user(
    State(state): State<AppState>,
    Json(req): Json<UpdateUserRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let username = req.username.unwrap_or_default();
    let list = state
        .user_service
        .update_user(
            &username,
            req.enable_webdav,
            req.enable_local_store,
            req.enable_ai_model,
        )
        .await?;
    Ok(Json(ApiResponse::ok(Value::from(list))))
}

pub async fn upload_file(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(q): Query<FileTypeQuery>,
    mut multipart: Multipart,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let raw_file_type = match q.file_type.as_deref() {
        Some(t) if !t.trim().is_empty() => t,
        _ => "images",
    };
    // 目录名只允许 [A-Za-z0-9_-]，避免 `..` 或分隔符进入路径
    let Some(file_type) = safe_path::sanitize_dir_segment(raw_file_type) else {
        return Ok(Json(ApiResponse::err("文件类型不合法")));
    };
    // 与静态路由同源：静态服务把 ASSETS_DIR 挂在 /assets 下，
    // 写入端必须用同一个目录，否则自定义 ASSETS_DIR 后上传的文件取不到
    let assets_root = PathBuf::from(&state.config.assets_dir);

    let mut file_list = Vec::new();
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?
    {
        let raw_name = field
            .file_name()
            .map(|s| s.to_string())
            .unwrap_or_else(|| "file".to_string());
        // 只取安全的纯文件名：拒绝 `..`、路径分隔符、绝对路径与 Windows 保留字符
        let Some(name) = safe_path::sanitize_file_name(&raw_name) else {
            return Ok(Json(ApiResponse::err("文件名不合法")));
        };
        // multipart 字段不受 DefaultBodyLimit 约束，必须逐块计数限量
        let data = match crate::api::handlers::multipart::read_limited_multipart_field(
            field,
            MAX_UPLOAD_FILE_BYTES,
            "文件不能超过 32MB",
        )
        .await
        {
            Ok(data) => data,
            Err(e) => return Ok(Json(ApiResponse::err(e.to_string()))),
        };
        let dir = assets_root.join(&user_ns).join(&file_type);
        fs::create_dir_all(&dir)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
        // 词法解析 + 越界检查：解析后必须仍在 assets/ 之内
        let Some(path) = safe_path::resolve_within(
            &assets_root,
            &Path::new(&user_ns).join(&file_type).join(&name),
        ) else {
            return Ok(Json(ApiResponse::err("文件名不合法")));
        };
        fs::write(&path, data)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
        let url = format!("/assets/{}/{}/{}", user_ns, file_type, name);
        file_list.push(Value::String(url));
    }
    Ok(Json(ApiResponse::ok(Value::from(file_list))))
}

pub async fn delete_file(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<DeleteFileRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let url = req.url.unwrap_or_default();
    if url.is_empty() {
        return Ok(Json(ApiResponse::err("请输入文件链接")));
    }
    let prefix = format!("/assets/{}/", user_ns);
    if !url.starts_with(&prefix) {
        return Ok(Json(ApiResponse::err("文件链接错误")));
    }
    // 去掉 `/assets/` 前缀后做词法解析：相对路径必须**保留 user_ns 分量**，
    // 且解析结果不能逃出 assets 根目录
    let assets_root = PathBuf::from(&state.config.assets_dir);
    let relative = Path::new(&user_ns).join(&url[prefix.len()..]);
    // 上传时生成的链接不含 `..`，客户端再传回来时出现回溯一律视为非法
    if safe_path::contains_parent_dir(&relative) {
        return Ok(Json(ApiResponse::err("文件链接错误")));
    }
    let Some(full_path) = safe_path::resolve_within(&assets_root, &relative) else {
        return Ok(Json(ApiResponse::err("文件链接错误")));
    };
    if let Err(err) = fs::remove_file(&full_path).await {
        if err.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!("deleteFile 删除失败 {}: {}", full_path.display(), err);
        }
    }
    Ok(Json(ApiResponse::ok(Value::String("".to_string()))))
}
