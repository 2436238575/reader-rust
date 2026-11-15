use crate::api::AppState;
use crate::auth::MaybeUser;
use axum::{extract::State, Json};
use serde_json::Value;

use crate::error::error::{ApiResponse, AppError};

/// 用户 JSON 文档的大小上限：整文档写进 json_documents，超限时明确 413，
/// 而不是任由全局请求体上限当缓冲配额。
pub(crate) const MAX_USER_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;

/// userdata 文档名的白名单：映射到 json_documents 的 name 键。
/// 文档名历史上曾直接拼进 `data/<ns>/<name>` 文件路径（legacy 读穿已随
/// 破坏性升级移除，见 `JsonDocumentService` 注释），约束保留下来：首字符
/// 限定字母数字，天然排除 `.` / `..` 与路径分隔符，让文档名保持
/// URL/日志安全且与存量键形态一致。
pub(crate) fn validate_userdata_name(name: &str) -> bool {
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
pub(crate) const MAX_USERDATA_BATCH: usize = 20;

/// 可选鉴权：未登录时返回 `userInfo: null`，不报错。
pub async fn get_user_info(
    State(state): State<AppState>,
    MaybeUser(user): MaybeUser,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let data = state.user_service.get_user_info(user.as_ref()).await?;
    Ok(Json(ApiResponse::ok(data)))
}
