use crate::api::AppState;
use crate::app::config::AppConfig;
use crate::auth::cookie::{
    build_cookie, clear_cookie, cookie_secure, cookie_value, ACCESS_COOKIE, REFRESH_COOKIE,
};
use crate::auth::{CurrentUser, MaybeUser};
use axum::{
    extract::{ConnectInfo, Multipart, Query, State},
    http::{header, HeaderMap},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio::fs;

use crate::error::error::{ApiResponse, AppError, NEED_LOGIN};
use crate::util::safe_path;

/// 上传资源文件的单文件上限。axum 0.7.9 的 `Multipart` 提取器经
/// `Request::with_limited_body` 让**请求体总量**受 `DefaultBodyLimit` 约束
/// （本路由在 big_body_routes 组，上限 100MB）；但单个字段的大小不受其单独
/// 约束（`Field::bytes()` 会把整个字段无界读入内存），仍须逐字段逐块计数限量。
const MAX_UPLOAD_FILE_BYTES: usize = 32 * 1024 * 1024;

/// 用户 JSON 文档（userdata / userConfig）的大小上限：两者都整文档写进
/// json_documents，超限时明确 400，而不是任由全局请求体上限当缓冲配额。
const MAX_USER_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;

/// 序列化后的文档超过上限时给出用户可读错误。
fn ensure_document_within_limit(serialized: &[u8]) -> Result<(), AppError> {
    if serialized.len() > MAX_USER_DOCUMENT_BYTES {
        return Err(AppError::BadRequest(
            "文档内容不能超过 4MB，请精简后重试".to_string(),
        ));
    }
    Ok(())
}

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

/// userdata 文档名的白名单：映射到 json_documents 的 name 键。
/// 文档名历史上曾直接拼进 `data/<ns>/<name>` 文件路径（legacy 读穿已随
/// 破坏性升级移除，见 `JsonDocumentService` 注释），约束保留下来：首字符
/// 限定字母数字，天然排除 `.` / `..` 与路径分隔符，让文档名保持
/// URL/日志安全且与存量键形态一致。
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

/// 批量 userdata 接口的文档数上限：一次首屏所需的全部 userdata 文档
/// （最近阅读、阅读统计、已读标记等）远低于 20；上限用来防拼接失控的
/// 客户端把批量接口当无限通道。
const MAX_USERDATA_BATCH: usize = 20;

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
    let serialized = serde_json::to_vec(&value)
        .map_err(|e| AppError::BadRequest(format!("文档序列化失败: {e}")))?;
    ensure_document_within_limit(&serialized)?;
    state
        .json_document_service
        .set_value(&user.0.ns, &name, &value)
        .await?;
    Ok(Json(ApiResponse::ok(Value::String(String::new()))))
}

#[derive(Debug, Deserialize)]
pub struct UserdataBatchQuery {
    pub names: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UserdataBatchDoc {
    pub name: Option<String>,
    pub value: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct SaveUserdataBatchRequest {
    pub docs: Option<Vec<UserdataBatchDoc>>,
}

/// 把逗号分隔的 names 参数拆成去重后的文档名列表；空参数 400，条目数
/// 超上限 400（按原始条目计，与 saveUserdataBatch 口径一致），任一条目
/// 不通过文档名白名单也 400。
fn parse_userdata_batch_names(raw: &str) -> Result<Vec<String>, AppError> {
    if raw.trim().is_empty() {
        return Err(AppError::BadRequest("文档名列表不能为空".to_string()));
    }
    let items = raw.split(',').collect::<Vec<_>>();
    if items.len() > MAX_USERDATA_BATCH {
        return Err(AppError::BadRequest(format!(
            "一次最多读取 {MAX_USERDATA_BATCH} 个文档"
        )));
    }
    let mut names: Vec<String> = Vec::new();
    for item in items {
        let name = item.trim();
        if !validate_userdata_name(name) {
            return Err(AppError::BadRequest("文档名不合法".to_string()));
        }
        if !names.iter().any(|n| n == name) {
            names.push(name.to_string());
        }
    }
    Ok(names)
}

/// 批量读取当前用户的 JSON 文档：`?names=a,b,c`，缺失的文档在 `docs`
/// 映射里为 `null`。把首屏的 N 次单文档 GET 合成一次往返。
pub async fn get_userdata_batch(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(q): Query<UserdataBatchQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let names = parse_userdata_batch_names(q.names.as_deref().unwrap_or_default())?;
    let docs = state
        .json_document_service
        .get_many(&user.0.ns, &names)
        .await?;
    let mut mapping = serde_json::Map::with_capacity(docs.len());
    for (name, value) in docs {
        mapping.insert(name, value.unwrap_or(Value::Null));
    }
    Ok(Json(ApiResponse::ok(
        serde_json::json!({ "docs": Value::Object(mapping) }),
    )))
}

/// 批量保存当前用户的 JSON 文档（各自整文档覆盖写，单文档沿用 4MB 上限）。
/// 全部条目先校验后写入：任一文档名非法或缺内容/超限时整批拒绝、一个都
/// 不落库（服务层再套一层事务，见 `JsonDocumentService::set_many`）；
/// 同名文档按最后一次为准。响应 `data.saved` 为实际写入的文档数。
pub async fn save_userdata_batch(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<SaveUserdataBatchRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let incoming = req.docs.unwrap_or_default();
    if incoming.is_empty() {
        return Err(AppError::BadRequest("文档列表不能为空".to_string()));
    }
    if incoming.len() > MAX_USERDATA_BATCH {
        return Err(AppError::BadRequest(format!(
            "一次最多保存 {MAX_USERDATA_BATCH} 个文档"
        )));
    }
    let mut docs: Vec<(String, Value)> = Vec::with_capacity(incoming.len());
    for doc in incoming {
        let name = doc.name.unwrap_or_default();
        if !validate_userdata_name(&name) {
            return Err(AppError::BadRequest("文档名不合法".to_string()));
        }
        let Some(value) = doc.value else {
            return Err(AppError::BadRequest("缺少文档内容".to_string()));
        };
        let serialized = serde_json::to_vec(&value)
            .map_err(|e| AppError::BadRequest(format!("文档序列化失败: {e}")))?;
        ensure_document_within_limit(&serialized)?;
        match docs.iter_mut().find(|(n, _)| *n == name) {
            // 同名文档按最后一次为准：后面的覆盖前面的
            Some(slot) => slot.1 = value,
            None => docs.push((name, value)),
        }
    }
    let saved = state
        .json_document_service
        .set_many(&user.0.ns, &docs)
        .await?;
    Ok(Json(ApiResponse::ok(serde_json::json!({ "saved": saved }))))
}

/// 从请求 `Host` 头取值（Cookie 的 `Secure` 判定用）。
fn host_header_value(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
}

/// 当前请求下会话 Cookie 是否加 `Secure`（COOKIE_SECURE 三态，见 auth::cookie）。
fn session_cookie_secure(config: &AppConfig, headers: &HeaderMap) -> bool {
    cookie_secure(&config.cookie_secure, host_header_value(headers))
}

/// 登录/改密/刷新成功的响应：两枚 HttpOnly Cookie + JSON。
///
/// Refresh Token 只经 `reader_refresh` Set-Cookie 下发，不进 JSON
///（service 层通过 `IssuedSession` 把它与 JSON 数据分开带出）；JSON 保留
/// `accessToken` 供 API 客户端（`Authorization: Bearer`）继续使用。
fn session_response(
    config: &AppConfig,
    headers: &HeaderMap,
    data: Value,
    refresh_token: &str,
) -> Response {
    let access = data["accessToken"].as_str().unwrap_or_default().to_string();
    let secure = session_cookie_secure(config, headers);
    let mut cookies = HeaderMap::new();
    cookies.append(
        header::SET_COOKIE,
        build_cookie(ACCESS_COOKIE, &access, config.access_token_ttl_secs, secure),
    );
    cookies.append(
        header::SET_COOKIE,
        build_cookie(
            REFRESH_COOKIE,
            refresh_token,
            config.refresh_token_ttl_secs,
            secure,
        ),
    );
    (cookies, Json(ApiResponse::ok(data))).into_response()
}

/// 登出/会话失效的响应：`Max-Age=0` 清掉两枚 Cookie，其余属性与下发时
/// 一致（同名同路径覆盖删除），JSON 仍按统一包装返回。
fn session_cleared_response(config: &AppConfig, headers: &HeaderMap, data: Value) -> Response {
    let secure = session_cookie_secure(config, headers);
    let mut cookies = HeaderMap::new();
    cookies.append(header::SET_COOKIE, clear_cookie(ACCESS_COOKIE, secure));
    cookies.append(header::SET_COOKIE, clear_cookie(REFRESH_COOKIE, secure));
    (cookies, Json(ApiResponse::ok(data))).into_response()
}

/// 刷新失败的 401：错误响应也要带清除 Cookie 的头。
///
/// 走 `Ok(...)` 而不是 `Err(AppError::Unauthorized)`：`AppError` 的
/// `IntoResponse` 不知道会话 Cookie，`Err` 分支挂不上 `Set-Cookie`。
fn unauthorized_with_cleared_cookies(config: &AppConfig, headers: &HeaderMap) -> Response {
    let secure = session_cookie_secure(config, headers);
    let mut response = AppError::Unauthorized(NEED_LOGIN.to_string()).into_response();
    response
        .headers_mut()
        .append(header::SET_COOKIE, clear_cookie(ACCESS_COOKIE, secure));
    response
        .headers_mut()
        .append(header::SET_COOKIE, clear_cookie(REFRESH_COOKIE, secure));
    response
}

/// 公开端点：登录。成功时下发两枚 HttpOnly Cookie，JSON 只保留用户信息
/// 与 `accessToken` / `accessTokenExpiresIn`（`refreshToken` 不交给 JS）。
///
/// 不提供注册：唯一账号由启动时的 bootstrap 创建（`ADMIN_USERNAME` /
/// `ADMIN_PASSWORD`，见 `UserService::bootstrap_admin`）。
pub async fn login(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<LoginRequest>,
) -> Result<Response, AppError> {
    let username = req.username.unwrap_or_default();
    let password = req.password.unwrap_or_default();
    // 限速按客户端 IP 记账：对端命中 TRUSTED_PROXIES 时从 X-Forwarded-For
    // 还原真实地址（挂 nginx/Docker NAT 后全体请求同 socket IP，会互相连坐），
    // 否则一律用 socket 地址（ConnectInfo 由 into_make_service_with_connect_info 注入）
    let trusted = crate::util::client_ip::parse_or_default(&state.config.trusted_proxies);
    let client_ip = crate::util::client_ip::resolve_client_ip(addr.ip(), &headers, &trusted);
    let session = state
        .user_service
        .login(&username, &password, Some(&client_ip))
        .await?;
    Ok(session_response(
        &state.config,
        &headers,
        session.data,
        &session.refresh_token,
    ))
}

/// 公开端点：用一次性 Refresh Token 换一对新令牌。
///
/// 令牌只认 `reader_refresh` HttpOnly Cookie。成功换发两枚新 Cookie
/// （Refresh 重新计时）；任何失败（不存在、已过期、已吊销、重放）统一 401
/// `需要登录`，并清除两枚 Cookie，让浏览器无感知地退出会话。
pub async fn refresh_token(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let Some(token) = cookie_value(&headers, REFRESH_COOKIE) else {
        return Ok(unauthorized_with_cleared_cookies(&state.config, &headers));
    };
    match state.user_service.refresh_session(&token).await {
        Ok(session) => Ok(session_response(
            &state.config,
            &headers,
            session.data,
            &session.refresh_token,
        )),
        Err(_) => Ok(unauthorized_with_cleared_cookies(&state.config, &headers)),
    }
}

/// 登出。
///
/// Refresh Token 需要在服务端吊销：读 `reader_refresh` Cookie。Cookie 缺失
/// 或令牌已损坏时仍返回成功并清除 Cookie——令牌已过期的情况下，前端同样
/// 只需要清掉本地状态（幂等）。
pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if let Some(token) = cookie_value(&headers, REFRESH_COOKIE) {
        if let Err(error) = state.user_service.revoke_refresh_token(&token).await {
            tracing::warn!("登出时吊销 Refresh Token 失败: {error}");
        }
    }
    Ok(session_cleared_response(
        &state.config,
        &headers,
        Value::String(String::new()),
    ))
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
    let serialized = serde_json::to_vec(&body)
        .map_err(|e| AppError::BadRequest(format!("配置序列化失败: {e}")))?;
    ensure_document_within_limit(&serialized)?;
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

/// 修改密码；响应换发两枚新的会话 Cookie。
///
/// 改密码会自增撤销版本号，令其他设备上的令牌立即失效；当前设备用
/// 换发的新令牌继续使用，避免用户改完密码就被登出。
pub async fn change_password(
    State(state): State<AppState>,
    user: CurrentUser,
    headers: HeaderMap,
    Json(req): Json<ChangePasswordRequest>,
) -> Result<Response, AppError> {
    let old_password = req.old_password.unwrap_or_default();
    let new_password = req.new_password.unwrap_or_default();
    if old_password.is_empty() || new_password.is_empty() {
        return Err(AppError::BadRequest("请填写当前密码和新密码".to_string()));
    }
    let session = state
        .user_service
        .change_password(&user.0.username, &old_password, &new_password)
        .await?;
    Ok(session_response(
        &state.config,
        &headers,
        session.data,
        &session.refresh_token,
    ))
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
        // DefaultBodyLimit 只约束请求体总量，单字段大小须逐块计数自行限量
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
