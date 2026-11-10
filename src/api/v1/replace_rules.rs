//! `/v1/me/replace-rules`：替换规则 CRUD。
//!
//! 存储的键是 `(user_ns, name)`（0002 迁移的主键）——**规则名就是资源身份**，
//! 模型里的 `id` 是 Legado 导入携带的旧字段、恒 0，不参与身份。因此 v1 的
//! 路径参数是 `{name}`（契约 §7.1 写的 `{id}` 是示意，Draft-2 §16.2 记偏差）。
//! `scope` 用 `book:<bookId>` / `source:<bookSourceId>`，仅存储不校验目标存在。

use crate::api::problem::{ErrorCode, Problem};
use crate::api::v1::{V1Json, V1User};
use crate::api::AppState;
use crate::model::replace_rule::ReplaceRule;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, patch, post};
use axum::Router;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/me/replace-rules",
            get(list_replace_rules).post(create_replace_rule),
        )
        .route(
            "/v1/me/replace-rules:batch",
            post(batch_upsert_replace_rules),
        )
        .route(
            "/v1/me/replace-rules:batchDelete",
            post(batch_delete_replace_rules),
        )
        .route(
            "/v1/me/replace-rules/{name}",
            patch(patch_replace_rule).delete(delete_replace_rule),
        )
}

/// 替换规则条目。`name` 是身份（书名内唯一）。
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReplaceRuleItem {
    pub name: String,
    pub group: Option<String>,
    /// 匹配串；`isRegex` 为 true 时按正则解释
    pub pattern: String,
    pub replacement: String,
    /// `book:<bookId>` / `source:<bookSourceId>`；null 表示全局
    pub scope: Option<String>,
    pub is_enabled: bool,
    pub is_regex: bool,
    pub order: i32,
}

impl From<ReplaceRule> for ReplaceRuleItem {
    fn from(rule: ReplaceRule) -> Self {
        Self {
            name: rule.name,
            group: rule.group,
            pattern: rule.pattern,
            replacement: rule.replacement,
            scope: rule.scope,
            is_enabled: rule.is_enabled,
            is_regex: rule.is_regex,
            order: rule.order,
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplaceRuleWrite {
    pub name: String,
    pub group: Option<String>,
    pub pattern: String,
    pub replacement: Option<String>,
    pub scope: Option<String>,
    pub is_enabled: Option<bool>,
    pub is_regex: Option<bool>,
    pub order: Option<i32>,
}

impl ReplaceRuleWrite {
    fn validate(&self) -> Result<(), Problem> {
        if self.name.trim().is_empty() || self.pattern.is_empty() {
            return Err(Problem::new(ErrorCode::ValidationFailed)
                .detail("规则名与匹配串不能为空")
                .errors(vec![crate::api::problem::FieldIssue {
                    pointer: "/name".to_string(),
                    code: "REQUIRED".to_string(),
                    detail: None,
                }]));
        }
        Ok(())
    }

    fn into_model(self) -> ReplaceRule {
        ReplaceRule {
            id: 0,
            name: self.name,
            group: self.group,
            pattern: self.pattern,
            replacement: self.replacement.unwrap_or_default(),
            scope: self.scope,
            is_enabled: self.is_enabled.unwrap_or(true),
            is_regex: self.is_regex.unwrap_or(false),
            order: self.order.unwrap_or(0),
        }
    }
}

/// merge-patch 请求：只写出现的字段。
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplaceRulePatch {
    pub group: Option<String>,
    pub pattern: Option<String>,
    pub replacement: Option<String>,
    pub scope: Option<String>,
    pub is_enabled: Option<bool>,
    pub is_regex: Option<bool>,
    pub order: Option<i32>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct BatchUpsertRequest {
    pub rules: Vec<ReplaceRuleWrite>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct BatchDeleteRequest {
    pub names: Vec<String>,
}

/// 替换规则列表（按 `order` 升序）。
#[utoipa::path(
    get,
    path = "/v1/me/replace-rules",
    tag = "me",
    operation_id = "listReplaceRules",
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses((status = 200, description = "替换规则列表", body = [ReplaceRuleItem]))
)]
pub async fn list_replace_rules(
    State(state): State<AppState>,
    V1User(user): V1User,
) -> Result<axum::Json<Vec<ReplaceRuleItem>>, Problem> {
    let list = state
        .replace_rule_repo
        .list(&user.ns)
        .await
        .map_err(Problem::from)?;
    Ok(axum::Json(
        list.into_iter().map(ReplaceRuleItem::from).collect(),
    ))
}

/// 新建或整体替换（按 name upsert）：201 新建 / 200 替换。
#[utoipa::path(
    post,
    path = "/v1/me/replace-rules",
    tag = "me",
    operation_id = "createReplaceRule",
    request_body = ReplaceRuleWrite,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 201, description = "已新建", body = ReplaceRuleItem),
        (status = 200, description = "同名规则已存在，整体替换", body = ReplaceRuleItem),
        (status = 422, description = "规则名或匹配串为空", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn create_replace_rule(
    State(state): State<AppState>,
    V1User(user): V1User,
    V1Json(write): V1Json<ReplaceRuleWrite>,
) -> Result<(StatusCode, axum::Json<ReplaceRuleItem>), Problem> {
    write.validate()?;
    let existed = state
        .replace_rule_repo
        .list(&user.ns)
        .await
        .map_err(Problem::from)?
        .iter()
        .any(|rule| rule.name == write.name);
    let rule = write.into_model();
    state
        .replace_rule_repo
        .upsert(&user.ns, &rule)
        .await
        .map_err(Problem::from)?;
    let status = if existed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((status, axum::Json(ReplaceRuleItem::from(rule))))
}

/// 部分更新（merge-patch）：404 规则不存在。
#[utoipa::path(
    patch,
    path = "/v1/me/replace-rules/{name}",
    tag = "me",
    operation_id = "patchReplaceRule",
    params(("name" = String, Path, description = "规则名（身份）")),
    request_body = ReplaceRulePatch,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "更新后的规则", body = ReplaceRuleItem),
        (status = 404, description = "规则不存在", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn patch_replace_rule(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(name): Path<String>,
    V1Json(patch): V1Json<ReplaceRulePatch>,
) -> Result<axum::Json<ReplaceRuleItem>, Problem> {
    let mut rule = state
        .replace_rule_repo
        .list(&user.ns)
        .await
        .map_err(Problem::from)?
        .into_iter()
        .find(|rule| rule.name == name)
        .ok_or_else(|| Problem::new(ErrorCode::NotFound).detail("替换规则不存在"))?;
    if let Some(group) = patch.group {
        rule.group = Some(group);
    }
    if let Some(pattern) = patch.pattern {
        if pattern.is_empty() {
            return Err(Problem::new(ErrorCode::ValidationFailed)
                .detail("匹配串不能为空")
                .errors(vec![crate::api::problem::FieldIssue {
                    pointer: "/pattern".to_string(),
                    code: "REQUIRED".to_string(),
                    detail: None,
                }]));
        }
        rule.pattern = pattern;
    }
    if let Some(replacement) = patch.replacement {
        rule.replacement = replacement;
    }
    if let Some(scope) = patch.scope {
        rule.scope = Some(scope);
    }
    if let Some(enabled) = patch.is_enabled {
        rule.is_enabled = enabled;
    }
    if let Some(is_regex) = patch.is_regex {
        rule.is_regex = is_regex;
    }
    if let Some(order) = patch.order {
        rule.order = order;
    }
    state
        .replace_rule_repo
        .upsert(&user.ns, &rule)
        .await
        .map_err(Problem::from)?;
    Ok(axum::Json(ReplaceRuleItem::from(rule)))
}

/// 删除（幂等）：不存在也 204。
#[utoipa::path(
    delete,
    path = "/v1/me/replace-rules/{name}",
    tag = "me",
    operation_id = "deleteReplaceRule",
    params(("name" = String, Path, description = "规则名（身份）")),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses((status = 204, description = "已删除（幂等）"))
)]
pub async fn delete_replace_rule(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(name): Path<String>,
) -> Result<StatusCode, Problem> {
    state
        .replace_rule_repo
        .delete_by_name(&user.ns, &name)
        .await
        .map_err(Problem::from)?;
    Ok(StatusCode::NO_CONTENT)
}

/// 批量 upsert（导入/跨端同步）：返回写入条数。
#[utoipa::path(
    post,
    path = "/v1/me/replace-rules:batch",
    tag = "me",
    operation_id = "batchUpsertReplaceRules",
    request_body = BatchUpsertRequest,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses((status = 200, description = "写入条数"))
)]
pub async fn batch_upsert_replace_rules(
    State(state): State<AppState>,
    V1User(user): V1User,
    V1Json(request): V1Json<BatchUpsertRequest>,
) -> Result<axum::Json<serde_json::Value>, Problem> {
    for write in &request.rules {
        write.validate()?;
    }
    let mut count = 0usize;
    for write in request.rules {
        state
            .replace_rule_repo
            .upsert(&user.ns, &write.into_model())
            .await
            .map_err(Problem::from)?;
        count += 1;
    }
    Ok(axum::Json(serde_json::json!({ "count": count })))
}

/// 批量删除：返回删除条数（不存在的名字忽略）。
#[utoipa::path(
    post,
    path = "/v1/me/replace-rules:batchDelete",
    tag = "me",
    operation_id = "batchDeleteReplaceRules",
    request_body = BatchDeleteRequest,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses((status = 200, description = "删除条数"))
)]
pub async fn batch_delete_replace_rules(
    State(state): State<AppState>,
    V1User(user): V1User,
    V1Json(request): V1Json<BatchDeleteRequest>,
) -> Result<axum::Json<serde_json::Value>, Problem> {
    for name in &request.names {
        state
            .replace_rule_repo
            .delete_by_name(&user.ns, name)
            .await
            .map_err(Problem::from)?;
    }
    Ok(axum::Json(
        serde_json::json!({ "deleted": request.names.len() }),
    ))
}
