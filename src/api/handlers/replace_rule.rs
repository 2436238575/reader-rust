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
    let user_ns = user.0.ns.clone();
    let list = read_list::<ReplaceRule>(&state, &user_ns, "replaceRule.json").await?;
    Ok(Json(ApiResponse::ok(
        serde_json::to_value(list).unwrap_or_default(),
    )))
}

pub async fn save_replace_rule(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(rule): Json<ReplaceRule>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    if rule.name.is_empty() {
        return Err(AppError::BadRequest("名称不能为空".to_string()));
    }
    if rule.pattern.is_empty() {
        return Err(AppError::BadRequest("替换规则不能为空".to_string()));
    }
    let mut list = read_list::<ReplaceRule>(&state, &user_ns, "replaceRule.json").await?;
    upsert_by_key(&mut list, rule, |r| r.name.clone());
    write_list(&state, &user_ns, "replaceRule.json", &list).await?;
    Ok(Json(ApiResponse::ok(Value::String("".to_string()))))
}

pub async fn save_replace_rules(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(mut rules): Json<Vec<ReplaceRule>>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let mut list = read_list::<ReplaceRule>(&state, &user_ns, "replaceRule.json").await?;
    rules.retain(|r| !r.name.is_empty() && !r.pattern.is_empty());
    for r in rules {
        upsert_by_key(&mut list, r, |v| v.name.clone());
    }
    write_list(&state, &user_ns, "replaceRule.json", &list).await?;
    Ok(Json(ApiResponse::ok(Value::String("".to_string()))))
}

pub async fn delete_replace_rule(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(rule): Json<ReplaceRule>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let mut list = read_list::<ReplaceRule>(&state, &user_ns, "replaceRule.json").await?;
    list.retain(|r| r.name != rule.name);
    write_list(&state, &user_ns, "replaceRule.json", &list).await?;
    Ok(Json(ApiResponse::ok(Value::String("".to_string()))))
}

pub async fn delete_replace_rules(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(rules): Json<Vec<ReplaceRule>>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let mut list = read_list::<ReplaceRule>(&state, &user_ns, "replaceRule.json").await?;
    for r in rules {
        list.retain(|v| v.name != r.name);
    }
    write_list(&state, &user_ns, "replaceRule.json", &list).await?;
    Ok(Json(ApiResponse::ok(Value::String("".to_string()))))
}

async fn read_list<T: for<'de> serde::Deserialize<'de>>(
    state: &AppState,
    user_ns: &str,
    name: &str,
) -> Result<Vec<T>, AppError> {
    state.json_document_service.read_list(user_ns, name).await
}

async fn write_list<T: serde::Serialize>(
    state: &AppState,
    user_ns: &str,
    name: &str,
    list: &Vec<T>,
) -> Result<(), AppError> {
    state
        .json_document_service
        .write_list(user_ns, name, list)
        .await
}

fn upsert_by_key<T, F>(list: &mut Vec<T>, item: T, key_fn: F)
where
    F: Fn(&T) -> String,
{
    let key = key_fn(&item);
    if let Some(pos) = list.iter().position(|v| key_fn(v) == key) {
        list[pos] = item;
    } else {
        list.push(item);
    }
}
