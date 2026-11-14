//! `PUT /v1/me/userdata:batch`：用户 JSON 文档批量整文档覆盖写。
//!
//! 与 legacy `/reader3/saveUserdataBatch` 同一套语义：最多 20 个文档、单文档
//! 序列化后 ≤4MB、文档名白名单（首字符字母数字，长度 ≤128，只含
//! `alnum . _ -`）、同名按最后一次为准、全部条目校验通过后单事务写入。
//! 任一文档非法整批拒绝，一个都不落库。
//!
//! 文档 `value` 是任意 JSON（阅读统计对象、最近阅读数组等），schema 里用
//! 空对象表示「任意结构」——这是 v1 red line「响应不用裸 Value」的明确例外：
//! userdata 本身就是通用文档存储，无法给出固定字段。

use crate::api::handlers::user::{
    validate_userdata_name, MAX_USERDATA_BATCH, MAX_USER_DOCUMENT_BYTES,
};
use crate::api::problem::{ErrorCode, FieldIssue, Problem};
use crate::api::v1::{V1Json, V1User};
use crate::api::AppState;
use axum::extract::State;
use axum::routing::put;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

/// 批量请求体上限：单文档 4MB × 最多 2 个已到 8MB；更大批次由客户端拆分。
/// 与 legacy `/reader3` 受保护组的默认 8MB 对齐。
const BATCH_BODY_LIMIT_BYTES: usize = 8 * 1024 * 1024;

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/v1/me/userdata:batch",
        put(save_userdata_batch)
            .layer(axum::extract::DefaultBodyLimit::max(BATCH_BODY_LIMIT_BYTES)),
    )
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UserdataBatchDoc {
    pub name: String,
    /// 整文档新值；缺失 422（不能把「忘传内容」当成删文档）
    #[schema(required = true, nullable = false)]
    pub value: Option<Value>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UserdataBatchSaveRequest {
    pub docs: Vec<UserdataBatchDoc>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UserdataBatchSaveResult {
    pub saved: u32,
}

fn field_error(pointer: String, code: &str, detail: &str) -> Problem {
    Problem::new(ErrorCode::ValidationFailed)
        .detail(detail)
        .errors(vec![FieldIssue {
            pointer,
            code: code.to_string(),
            detail: Some(detail.to_string()),
        }])
}

/// 批量保存当前用户的 JSON 文档（各自整文档覆盖写）。
#[utoipa::path(
    put,
    path = "/v1/me/userdata:batch",
    tag = "me",
    operation_id = "saveUserdataBatch",
    request_body = UserdataBatchSaveRequest,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "实际写入的文档数", body = UserdataBatchSaveResult),
        (status = 400, description = "请求体不是合法 JSON 或字段类型不符", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 413, description = "请求体超过 8MB 或单个文档超过 4MB", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "文档列表为空/超上限、文档名非法、缺少 value 或含未知字段", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn save_userdata_batch(
    State(state): State<AppState>,
    V1User(user): V1User,
    V1Json(request): V1Json<UserdataBatchSaveRequest>,
) -> Result<Json<UserdataBatchSaveResult>, Problem> {
    if request.docs.is_empty() {
        return Err(field_error(
            "/docs".to_string(),
            "REQUIRED",
            "文档列表不能为空",
        ));
    }
    if request.docs.len() > MAX_USERDATA_BATCH {
        return Err(field_error(
            "/docs".to_string(),
            "TOO_MANY",
            &format!("一次最多保存 {MAX_USERDATA_BATCH} 个文档"),
        ));
    }
    let mut docs: Vec<(String, Value)> = Vec::with_capacity(request.docs.len());
    for (index, doc) in request.docs.into_iter().enumerate() {
        if !validate_userdata_name(&doc.name) {
            return Err(field_error(
                format!("/docs/{index}/name"),
                "INVALID_FORMAT",
                "文档名不合法",
            ));
        }
        let Some(value) = doc.value else {
            return Err(field_error(
                format!("/docs/{index}/value"),
                "REQUIRED",
                "缺少文档内容",
            ));
        };
        let serialized = serde_json::to_vec(&value).map_err(|error| {
            field_error(
                format!("/docs/{index}/value"),
                "INVALID_VALUE",
                &format!("文档序列化失败: {error}"),
            )
        })?;
        if serialized.len() > MAX_USER_DOCUMENT_BYTES {
            return Err(Problem::new(ErrorCode::PayloadTooLarge)
                .detail("单个文档不能超过 4MB，请精简后重试"));
        }
        // 同名文档按最后一次为准：后面的覆盖前面的
        match docs.iter_mut().find(|(name, _)| *name == doc.name) {
            Some(slot) => slot.1 = value,
            None => docs.push((doc.name, value)),
        }
    }
    let saved = state
        .json_document_service
        .set_many(&user.ns, &docs)
        .await
        .map_err(Problem::from)?;
    Ok(Json(UserdataBatchSaveResult {
        saved: saved as u32,
    }))
}
