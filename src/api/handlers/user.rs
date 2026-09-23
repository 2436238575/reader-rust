use crate::api::auth::AuthContext;
use crate::api::AppState;
use axum::{
    extract::{Multipart, Query, State},
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

pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let username = req.username.unwrap_or_default();
    let password = req.password.unwrap_or_default();
    let is_login = req.is_login.unwrap_or(false);
    let is_new_user = !is_login && !username.is_empty(); // registration attempt
    let data = state
        .user_service
        .login(&username, &password, is_login, req.code.as_deref())
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

pub async fn logout(
    State(state): State<AppState>,
    auth: AuthContext,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    if !state.user_service.secure_enabled() {
        return Ok(Json(ApiResponse::err("不支持的操作")));
    }
    if let Some(token) = auth.access_token() {
        let _ = state.user_service.logout(token).await;
    }
    Ok(Json(ApiResponse::err_with_data(
        "请重新登录",
        Value::String("NEED_LOGIN".to_string()),
    )))
}

pub async fn get_user_info(
    State(state): State<AppState>,
    auth: AuthContext,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let (user_info, secure, secure_key_required, admin_authorized) = state
        .user_service
        .get_user_info(auth.access_token(), auth.secure_key())
        .await?;
    let data = serde_json::json!({
        "userInfo": user_info,
        "secure": secure,
        "secureKeyRequired": secure_key_required,
        "adminAuthorized": admin_authorized,
    });
    Ok(Json(ApiResponse::ok(data)))
}

pub async fn save_user_config(
    State(state): State<AppState>,
    auth: AuthContext,
    Json(body): Json<Value>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = match state
        .user_service
        .resolve_user_ns_with_override(auth.access_token(), auth.secure_key(), auth.user_ns())
        .await
    {
        Ok(ns) => ns,
        Err(_) => {
            return Ok(Json(ApiResponse::err_with_data(
                "请登录后使用",
                Value::String("NEED_LOGIN".to_string()),
            )))
        }
    };
    state.user_service.save_user_config(&user_ns, body).await?;
    Ok(Json(ApiResponse::ok(Value::String("".to_string()))))
}

pub async fn get_user_config(
    State(state): State<AppState>,
    auth: AuthContext,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = match state
        .user_service
        .resolve_user_ns_with_override(auth.access_token(), auth.secure_key(), auth.user_ns())
        .await
    {
        Ok(ns) => ns,
        Err(_) => {
            return Ok(Json(ApiResponse::err_with_data(
                "请登录后使用",
                Value::String("NEED_LOGIN".to_string()),
            )))
        }
    };
    let cfg = state.user_service.get_user_config(&user_ns).await?;
    Ok(Json(ApiResponse::ok(cfg)))
}

pub async fn get_user_list(
    State(state): State<AppState>,
    auth: AuthContext,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    if !state.user_service.secure_enabled() {
        return Ok(Json(ApiResponse::err("不支持的操作")));
    }
    // Check if admin (either by is_admin flag or secure key)
    let is_admin = state
        .user_service
        .is_admin(auth.access_token(), auth.secure_key())
        .await?;
    if !is_admin {
        return Ok(Json(ApiResponse::err_with_data(
            "请输入管理密码",
            Value::String("NEED_SECURE_KEY".to_string()),
        )));
    }
    let list = state.user_service.get_user_list().await?;
    Ok(Json(ApiResponse::ok(Value::from(list))))
}

pub async fn add_user(
    State(state): State<AppState>,
    auth: AuthContext,
    Json(req): Json<AddUserRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    if !state.user_service.secure_enabled() {
        return Ok(Json(ApiResponse::err("不支持的操作")));
    }
    // Check if admin (either by is_admin flag or secure key)
    let is_admin = state
        .user_service
        .is_admin(auth.access_token(), auth.secure_key())
        .await?;
    if !is_admin {
        return Ok(Json(ApiResponse::err_with_data(
            "请输入管理密码",
            Value::String("NEED_SECURE_KEY".to_string()),
        )));
    }
    let username = req.username.unwrap_or_default();
    let password = req.password.unwrap_or_default();
    let list = state.user_service.add_user(&username, &password).await?;
    Ok(Json(ApiResponse::ok(Value::from(list))))
}

pub async fn reset_password(
    State(state): State<AppState>,
    auth: AuthContext,
    Json(req): Json<ResetPasswordRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    if !state.user_service.secure_enabled() {
        return Ok(Json(ApiResponse::err("不支持的操作")));
    }
    // Check if admin (either by is_admin flag or secure key)
    let is_admin = state
        .user_service
        .is_admin(auth.access_token(), auth.secure_key())
        .await?;
    if !is_admin {
        return Ok(Json(ApiResponse::err_with_data(
            "请输入管理密码",
            Value::String("NEED_SECURE_KEY".to_string()),
        )));
    }
    let username = req.username.unwrap_or_default();
    let password = req.password.unwrap_or_default();
    state
        .user_service
        .reset_password(&username, &password)
        .await?;
    Ok(Json(ApiResponse::ok(Value::String("".to_string()))))
}

pub async fn change_password(
    State(state): State<AppState>,
    auth: AuthContext,
    Json(req): Json<ChangePasswordRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    if !state.user_service.secure_enabled() {
        return Ok(Json(ApiResponse::err("不支持的操作")));
    }
    let token = auth
        .access_token()
        .ok_or_else(|| AppError::BadRequest("NEED_LOGIN".to_string()))?;
    let old_password = req.old_password.unwrap_or_default();
    let new_password = req.new_password.unwrap_or_default();
    if old_password.is_empty() || new_password.is_empty() {
        return Err(AppError::BadRequest("请填写当前密码和新密码".to_string()));
    }
    state
        .user_service
        .change_password(token, &old_password, &new_password)
        .await?;
    Ok(Json(ApiResponse::ok(Value::String("".to_string()))))
}

pub async fn delete_users(
    State(state): State<AppState>,
    auth: AuthContext,
    Json(list): Json<Vec<String>>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    if !state.user_service.secure_enabled() {
        return Ok(Json(ApiResponse::err("不支持的操作")));
    }
    // Check if admin (either by is_admin flag or secure key)
    let is_admin = state
        .user_service
        .is_admin(auth.access_token(), auth.secure_key())
        .await?;
    if !is_admin {
        return Ok(Json(ApiResponse::err_with_data(
            "请输入管理密码",
            Value::String("NEED_SECURE_KEY".to_string()),
        )));
    }
    let users = state.user_service.delete_users(&list).await?;
    Ok(Json(ApiResponse::ok(Value::from(users))))
}

pub async fn update_user(
    State(state): State<AppState>,
    auth: AuthContext,
    Json(req): Json<UpdateUserRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    if !state.user_service.secure_enabled() {
        return Ok(Json(ApiResponse::err("不支持的操作")));
    }
    // Check if admin (either by is_admin flag or secure key)
    let is_admin = state
        .user_service
        .is_admin(auth.access_token(), auth.secure_key())
        .await?;
    if !is_admin {
        return Ok(Json(ApiResponse::err_with_data(
            "请输入管理密码",
            Value::String("NEED_SECURE_KEY".to_string()),
        )));
    }
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
    auth: AuthContext,
    Query(q): Query<FileTypeQuery>,
    mut multipart: Multipart,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = match state
        .user_service
        .resolve_user_ns_with_override(auth.access_token(), auth.secure_key(), auth.user_ns())
        .await
    {
        Ok(ns) => ns,
        Err(_) => {
            return Ok(Json(ApiResponse::err_with_data(
                "请登录后使用",
                Value::String("NEED_LOGIN".to_string()),
            )))
        }
    };
    let raw_file_type = match q.file_type.as_deref() {
        Some(t) if !t.trim().is_empty() => t,
        _ => "images",
    };
    // 目录名只允许 [A-Za-z0-9_-]，避免 `..` 或分隔符进入路径
    let Some(file_type) = safe_path::sanitize_dir_segment(raw_file_type) else {
        return Ok(Json(ApiResponse::err("文件类型不合法")));
    };
    // 所有写入都必须落在 storage/assets 之内（user_ns 在 secure_key 路径下也可能是用户可控的）
    let assets_root = PathBuf::from(&state.config.storage_dir).join("assets");

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
        let Some(path) =
            safe_path::resolve_within(&assets_root, &Path::new(&user_ns).join(&file_type).join(&name))
        else {
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
    auth: AuthContext,
    Json(req): Json<DeleteFileRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = match state
        .user_service
        .resolve_user_ns_with_override(auth.access_token(), auth.secure_key(), auth.user_ns())
        .await
    {
        Ok(ns) => ns,
        Err(_) => {
            return Ok(Json(ApiResponse::err_with_data(
                "请登录后使用",
                Value::String("NEED_LOGIN".to_string()),
            )))
        }
    };
    let url = req.url.unwrap_or_default();
    if url.is_empty() {
        return Ok(Json(ApiResponse::err("请输入文件链接")));
    }
    let prefix = format!("/assets/{}/", user_ns);
    if !url.starts_with(&prefix) {
        return Ok(Json(ApiResponse::err("文件链接错误")));
    }
    // 去掉 `/assets/` 前缀后做词法解析：相对路径必须**保留 user_ns 分量**，
    // 且解析结果不能逃出 storage/assets/
    let assets_root = PathBuf::from(&state.config.storage_dir).join("assets");
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
