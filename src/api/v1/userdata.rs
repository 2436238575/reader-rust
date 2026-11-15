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
use crate::api::v1::{V1Json, V1Query, V1User};
use crate::api::AppState;
use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use utoipa::{IntoParams, ToSchema};

/// 批量请求体上限：单文档 4MB × 最多 2 个已到 8MB；更大批次由客户端拆分。
/// 与 legacy `/reader3` 受保护组的默认 8MB 对齐。
const BATCH_BODY_LIMIT_BYTES: usize = 8 * 1024 * 1024;
/// 批量读取的响应总大小上限：写接口保证单文档 ≤4MB，20 个文档最坏 80MB；
/// 读接口一次拉这么多没有意义，超过 16MB 直接拒绝。
const MAX_USERDATA_READ_BYTES: usize = 16 * 1024 * 1024;

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/v1/me/userdata:batch",
        get(read_userdata_batch)
            .put(save_userdata_batch)
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

/// 批量读取查询：`names` 逗号分隔，最多 20 个。
#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct UserdataBatchReadQuery {
    /// 逗号分隔的文档名，最多 20 个；不存在于返回映射的文档为 null
    pub names: Option<String>,
}

/// 批量读取结果：文档名 → 文档值；缺失文档为 null。
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UserdataBatchReadResult {
    /// 文档名 → 任意 JSON 值；缺失的文档为 null
    #[schema(additional_properties = true)]
    pub docs: BTreeMap<String, Value>,
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

/// 批量读取当前用户的 JSON 文档：`?names=a,b,c`，最多 20 个，缺失文档为 null。
#[utoipa::path(
    get,
    path = "/v1/me/userdata:batch",
    tag = "me",
    operation_id = "readUserdataBatch",
    params(UserdataBatchReadQuery),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "文档名到文档值的映射；缺失文档为 null", body = UserdataBatchReadResult),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 413, description = "读取结果超过 16MB", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "names 缺失/为空/超上限或文档名非法", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn read_userdata_batch(
    State(state): State<AppState>,
    V1User(user): V1User,
    V1Query(query): V1Query<UserdataBatchReadQuery>,
) -> Result<Json<UserdataBatchReadResult>, Problem> {
    let raw = query.names.unwrap_or_default();
    let raw_names: Vec<&str> = raw
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect();
    // 上限按原始条目计（与写入侧一致），去重只减少实际查询量，不能绕过上限。
    if raw_names.len() > MAX_USERDATA_BATCH {
        return Err(field_error(
            "/names".to_string(),
            "TOO_MANY",
            &format!("一次最多读取 {MAX_USERDATA_BATCH} 个文档"),
        ));
    }
    let mut names: Vec<String> = Vec::new();
    for name in raw_names {
        if !validate_userdata_name(name) {
            return Err(field_error(
                "/names".to_string(),
                "INVALID_FORMAT",
                "文档名不合法",
            ));
        }
        if !names.iter().any(|existing| existing == name) {
            names.push(name.to_string());
        }
    }
    if names.is_empty() {
        return Err(field_error(
            "/names".to_string(),
            "REQUIRED",
            "names 不能为空",
        ));
    }

    let rows = state
        .json_document_service
        .get_many(&user.ns, &names)
        .await
        .map_err(Problem::from)?;
    let mut docs = BTreeMap::new();
    for (name, value) in rows {
        docs.insert(name, value.unwrap_or(Value::Null));
    }
    let size = serde_json::to_vec(&docs)
        .expect("userdata docs serialize")
        .len();
    if size > MAX_USERDATA_READ_BYTES {
        return Err(Problem::new(ErrorCode::PayloadTooLarge)
            .detail("批量读取结果超过 16MB，请减少文档数量"));
    }
    Ok(Json(UserdataBatchReadResult { docs }))
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
