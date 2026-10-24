use crate::api::AppState;
use crate::auth::{CurrentUser, MaybeUser};
use axum::{
    body::Bytes,
    extract::{ConnectInfo, Multipart, Query, State},
    Json,
};
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio::fs;

use crate::error::error::{ApiResponse, AppError, NEED_LOGIN};
use crate::util::safe_path;

/// 上传资源文件的单文件上限（multipart 字段不受 DefaultBodyLimit 约束，需自行限量）。
const MAX_UPLOAD_FILE_BYTES: usize = 32 * 1024 * 1024;

/// 上传文件扩展名白名单：/assets 与主站同源直出上传内容，放行 HTML/SVG
/// 等于给同源存储一个脚本落脚点，只收位图格式（大小写不敏感）。
fn is_allowed_upload_extension(name: &str) -> bool {
    let extension = Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default();
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "avif" | "bmp"
    )
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: Option<String>,
    pub password: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RefreshTokenRequest {
    #[serde(rename = "refreshToken")]
    pub refresh_token: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct FileTypeQuery {
    #[serde(rename = "type")]
    pub file_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ChangePasswordRequest {
    #[serde(rename = "oldPassword")]
    pub old_password: Option<String>,
    #[serde(rename = "newPassword")]
    pub new_password: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DeleteFileRequest {
    pub url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UserdataNameQuery {
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SaveUserdataRequest {
    pub name: Option<String>,
    pub value: Option<Value>,
}

/// userdata 文档名的白名单：映射到 json_documents 的 name 键，并兼容旧的
/// `data/<ns>/<name>` 文件路径回退，必须是单一路径分量且不能是 `.` / `..`。
/// 首字符限定字母数字即可同时排除上述两种情况。
fn validate_userdata_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    name.len() <= 128
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

/// 读取当前用户的 JSON 文档（如最近阅读、阅读统计）。不存在时返回 null。
pub async fn get_userdata(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(q): Query<UserdataNameQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let name = q.name.unwrap_or_default();
    if !validate_userdata_name(&name) {
        return Err(AppError::BadRequest("文档名不合法".to_string()));
    }
    let value = state
        .json_document_service
        .get_value(&user.0.ns, &name)
        .await?;
    Ok(Json(ApiResponse::ok(value.unwrap_or(Value::Null))))
}

/// 保存当前用户的 JSON 文档（整文档覆盖写）。
pub async fn save_userdata(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<SaveUserdataRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let name = req.name.unwrap_or_default();
    if !validate_userdata_name(&name) {
        return Err(AppError::BadRequest("文档名不合法".to_string()));
    }
    let Some(value) = req.value else {
        return Err(AppError::BadRequest("缺少文档内容".to_string()));
    };
    state
        .json_document_service
        .set_value(&user.0.ns, &name, &value)
        .await?;
    Ok(Json(ApiResponse::ok(Value::String(String::new()))))
}

/// 公开端点：登录。成功时返回含 JWT 的用户信息。
///
/// 不提供注册：唯一账号由启动时的 bootstrap 创建（`ADMIN_USERNAME` /
/// `ADMIN_PASSWORD`，见 `UserService::bootstrap_admin`）。
pub async fn login(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let username = req.username.unwrap_or_default();
    let password = req.password.unwrap_or_default();
    // 限速按对端 IP 记账（ConnectInfo 由 into_make_service_with_connect_info 注入）
    let client_ip = addr.ip().to_string();
    let data = state
        .user_service
        .login(&username, &password, Some(&client_ip))
        .await?;
    Ok(Json(ApiResponse::ok(data)))
}

/// 公开端点：用一次性 Refresh Token 换一对新令牌。
///
/// 成功返回新的 `accessToken` + `refreshToken`；任何失败（不存在、已过期、
/// 已吊销、重放）统一 401 `需要登录`，前端据此清理本地令牌并回到登录页。
pub async fn refresh_token(
    State(state): State<AppState>,
    Json(req): Json<RefreshTokenRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let token = req.refresh_token.unwrap_or_default();
    if token.trim().is_empty() {
        return Err(AppError::Unauthorized(NEED_LOGIN.to_string()));
    }
    let data = state.user_service.refresh_session(&token).await?;
    Ok(Json(ApiResponse::ok(data)))
}

/// 登出。
///
/// Refresh Token 需要在服务端吊销，因此从 body 读取；body 为空或已损坏时
/// 仍返回成功——令牌已过期的情况下，前端同样只需要清掉本地状态。
pub async fn logout(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    if !body.is_empty() {
        if let Ok(req) = serde_json::from_slice::<RefreshTokenRequest>(&body) {
            if let Some(token) = req.refresh_token.filter(|token| !token.trim().is_empty()) {
                if let Err(error) = state.user_service.revoke_refresh_token(&token).await {
                    tracing::warn!("登出时吊销 Refresh Token 失败: {error}");
                }
            }
        }
    }
    Ok(Json(ApiResponse::ok(Value::String(String::new()))))
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

/// 修改密码；响应携带换发的新令牌。
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
        return Err(AppError::BadRequest("文件类型不合法".to_string()));
    };
    // 与静态路由同源：静态服务把 ASSETS_DIR 挂在 /assets 下，
    // 写入端必须用同一个目录，否则自定义 ASSETS_DIR 后上传的文件取不到
    let mut file_list = Vec::new();
    let mut field_count = 0usize;
    while let Some(field) =
        crate::api::handlers::multipart::next_field_limited(&mut multipart, &mut field_count)
            .await?
    {
        let raw_name = field
            .file_name()
            .map(|s| s.to_string())
            .unwrap_or_else(|| "file".to_string());
        // 只取安全的纯文件名：拒绝 `..`、路径分隔符、绝对路径与 Windows 保留字符
        let Some(name) = safe_path::sanitize_file_name(&raw_name) else {
            return Err(AppError::BadRequest("文件名不合法".to_string()));
        };
        // 类型白名单在读取请求体之前判定：非位图文件不必再缓冲内容
        if !is_allowed_upload_extension(&name) {
            return Err(AppError::BadRequest(
                "只支持上传图片文件（jpg/jpeg/png/gif/webp/avif/bmp）".to_string(),
            ));
        }
        // multipart 字段不受 DefaultBodyLimit 约束，必须逐块计数限量
        let data = match crate::api::handlers::multipart::read_limited_multipart_field(
            field,
            MAX_UPLOAD_FILE_BYTES,
            "文件不能超过 32MB",
        )
        .await
        {
            Ok(data) => data,
            Err(e) => return Err(e),
        };
        // 写盘统一走 util::asset（消毒/越界检查/URL 形态与 AI 地图落盘共用一份）
        let url = match crate::util::asset::write_asset_file(
            &state.config.assets_dir,
            &user_ns,
            &file_type,
            &name,
            data,
        )
        .await
        {
            Ok(url) => url,
            Err(e) => return Err(e),
        };
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
        return Err(AppError::BadRequest("请输入文件链接".to_string()));
    }
    let prefix = format!("/assets/{}/", user_ns);
    if !url.starts_with(&prefix) {
        return Err(AppError::BadRequest("文件链接错误".to_string()));
    }
    // 去掉 `/assets/` 前缀后做词法解析：相对路径必须**保留 user_ns 分量**，
    // 且解析结果不能逃出 assets 根目录
    let assets_root = PathBuf::from(&state.config.assets_dir);
    let relative = Path::new(&user_ns).join(&url[prefix.len()..]);
    // 上传时生成的链接不含 `..`，客户端再传回来时出现回溯一律视为非法
    if safe_path::contains_parent_dir(&relative) {
        return Err(AppError::BadRequest("文件链接错误".to_string()));
    }
    let Some(full_path) = safe_path::resolve_within(&assets_root, &relative) else {
        return Err(AppError::BadRequest("文件链接错误".to_string()));
    };
    if let Err(err) = fs::remove_file(&full_path).await {
        if err.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!("deleteFile 删除失败 {}: {}", full_path.display(), err);
        }
    }
    Ok(Json(ApiResponse::ok(Value::String("".to_string()))))
}

#[cfg(test)]
mod tests {
    use super::is_allowed_upload_extension;

    #[test]
    fn upload_extension_whitelist_accepts_bitmaps_only() {
        for name in [
            "a.jpg", "b.JPEG", "c.png", "d.GIF", "e.webp", "f.avif", "g.bmp",
        ] {
            assert!(is_allowed_upload_extension(name), "{name} 应放行");
        }
        // 同源直出会变成脚本落脚点的类型一律拒绝；无扩展名也不放行
        for name in [
            "x.svg",
            "y.html",
            "z.txt",
            "a.js",
            "b.png.exe",
            "noext",
            "p.jpegx",
        ] {
            assert!(!is_allowed_upload_extension(name), "{name} 应拒绝");
        }
    }
}
