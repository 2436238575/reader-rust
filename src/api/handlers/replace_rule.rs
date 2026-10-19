use crate::api::AppState;
use crate::auth::CurrentUser;
use axum::{extract::State, Json};
use serde_json::Value;

use crate::error::error::{ApiResponse, AppError};
use crate::model::replace_rule::ReplaceRule;

pub async fn get_replace_rules(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let list = state.replace_rule_repo.list(&user.0.ns).await?;
    Ok(Json(ApiResponse::ok(
        serde_json::to_value(list).unwrap_or_default(),
    )))
}

pub async fn save_replace_rule(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(rule): Json<ReplaceRule>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    validate_rule(&rule)?;
    state.replace_rule_repo.upsert(&user.0.ns, &rule).await?;
    Ok(Json(ApiResponse::ok(Value::String(String::new()))))
}

pub async fn save_replace_rules(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(rules): Json<Vec<ReplaceRule>>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    for r in &rules {
        if !r.name.is_empty() && !r.pattern.is_empty() {
            state.replace_rule_repo.upsert(&user.0.ns, r).await?;
        }
    }
    Ok(Json(ApiResponse::ok(Value::String(String::new()))))
}

pub async fn delete_replace_rule(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(rule): Json<ReplaceRule>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    state
        .replace_rule_repo
        .delete_by_name(&user.0.ns, &rule.name)
        .await?;
    Ok(Json(ApiResponse::ok(Value::String(String::new()))))
}

pub async fn delete_replace_rules(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(rules): Json<Vec<ReplaceRule>>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    for r in &rules {
        state
            .replace_rule_repo
            .delete_by_name(&user.0.ns, &r.name)
            .await?;
    }
    Ok(Json(ApiResponse::ok(Value::String(String::new()))))
}

fn validate_rule(rule: &ReplaceRule) -> Result<(), AppError> {
    if rule.name.is_empty() {
        return Err(AppError::BadRequest("名称不能为空".to_string()));
    }
    if rule.pattern.is_empty() {
        return Err(AppError::BadRequest("替换规则不能为空".to_string()));
    }
    Ok(())
}
