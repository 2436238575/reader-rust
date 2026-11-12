//! 本地书上传（v1）：`POST /v1/local-books`，TXT/EPUB 按扩展名分派。
//!
//! 与 `/reader3/uploadTxtBook|uploadEpubBook` 共用同一套导入服务与限额；
//! 差异只在契约：单一路径按扩展名分派（`.epub` 走 EPUB，其余按 TXT），
//! 响应是不带包装的 Book。这些路由**不进 120s 常规组**（大文件 + 慢速
//! 链路），由 `v1::router` 在超时层之外 merge，子路由自带 900s 上传超时、
//! 100MB 体限、鉴权与 CSRF。

use crate::api::problem::{ErrorCode, Problem};
use crate::api::v1::V1User;
use crate::api::AppState;
use crate::model::book::Book;
use axum::extract::{Multipart, Query, State};
use axum::routing::post;
use axum::Json;
use axum::Router;
use serde::Deserialize;

pub fn router(state: AppState, upload_timeout: std::time::Duration) -> Router<AppState> {
    Router::new()
        .route("/v1/local-books", post(upload_local_book))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::api::v1::require_auth,
        ))
        .layer(axum::extract::DefaultBodyLimit::max(
            crate::api::router::MAX_BODY_LIMIT_BYTES,
        ))
        .layer(axum::middleware::from_fn(crate::api::v1::require_csrf))
        .layer(axum::middleware::from_fn_with_state(
            (
                upload_timeout,
                crate::api::inbound_guard::TimeoutShape::Problem,
            ),
            crate::api::inbound_guard::timeout_middleware,
        ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalBookQuery {
    /// 显式声明格式（`txt` / `epub`）；缺省按扩展名分派
    pub kind: Option<String>,
}

/// 上传本地书（multipart `file` 字段）：按扩展名分派 TXT/EPUB 导入服务，
/// 响应是不带包装的 Book（`bookId` 已分配，且已在书架）。
#[utoipa::path(
    post,
    path = "/v1/local-books",
    tag = "books",
    operation_id = "uploadLocalBook",
    params(
        ("kind" = Option<String>, Query, description = "显式声明格式（txt / epub）；缺省按扩展名分派"),
    ),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "已创建并入架", body = Book),
        (status = 400, description = "file 缺失 / 解析失败 / 超出本地书数量上限", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 413, description = "TXT 上限 50MB、EPUB 上限 80MB", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn upload_local_book(
    State(state): State<AppState>,
    V1User(user): V1User,
    Query(query): Query<LocalBookQuery>,
    mut multipart: Multipart,
) -> Result<Json<Book>, Problem> {
    let mut file_name = String::new();
    let mut bytes: Option<Vec<u8>> = None;
    // EPUB 上限 80MB / TXT 上限 50MB：在读完字段内容之前先按文件名与
    // `?kind=` 判定格式，选对读体上限——50–80MB 的 EPUB 才不会被 TXT
    // 的 50MB 上限误杀。
    // `?kind=` 显式覆盖扩展名分派；非法取值显式 400（静默回落扩展名会让
    // `kind=garbage` + `.epub` 文件名得到「仅支持上传 .txt 文件」这种看不懂的错误）
    let kind_override = match query.kind.as_deref().map(str::trim) {
        None | Some("") => None,
        Some("epub") => Some(true),
        Some("txt") => Some(false),
        Some(other) => {
            return Err(Problem::new(ErrorCode::MalformedRequest)
                .detail(format!("kind 只接受 txt 或 epub，收到 {other}")))
        }
    };
    let mut is_epub = kind_override.unwrap_or(false);
    let mut field_count = 0usize;
    while let Some(field) =
        crate::api::handlers::multipart::next_field_limited(&mut multipart, &mut field_count)
            .await
            .map_err(Problem::from)?
    {
        if field.name().unwrap_or_default() != "file" {
            continue;
        }
        file_name = field.file_name().unwrap_or("book.txt").to_string();
        if kind_override.is_none() {
            is_epub = file_name.to_lowercase().ends_with(".epub");
        }
        let (limit, message) = if is_epub {
            (
                crate::service::local_epub_book::MAX_EPUB_UPLOAD_BYTES,
                "EPUB 文件不能超过 80MB",
            )
        } else {
            (
                crate::service::local_txt_book::MAX_TXT_UPLOAD_BYTES,
                "TXT 文件不能超过 50MB",
            )
        };
        bytes = Some(
            crate::api::handlers::multipart::read_limited_multipart_field(field, limit, message)
                .await
                .map_err(Problem::from)?,
        );
        break;
    }
    let bytes =
        bytes.ok_or_else(|| Problem::new(ErrorCode::MalformedRequest).detail("file required"))?;

    let book = if is_epub {
        state
            .local_epub_book_service
            .import_epub_book(&user.ns, &file_name, bytes)
            .await
            .map_err(Problem::from)?
    } else {
        state
            .local_txt_book_service
            .import_txt_book(&user.ns, &file_name, &bytes)
            .await
            .map_err(Problem::from)?
    };

    // 本地书数量上限：超限即清理已落盘文件并拒绝（与 /reader3 同口径）
    if let Err(err) = crate::api::handlers::book::shelf::ensure_user_local_book_limit(
        &state,
        &user.ns,
        &book.book_url,
    )
    .await
    .map_err(Problem::from)
    {
        let cleanup = if is_epub {
            state
                .local_epub_book_service
                .delete_book_files(&user.ns, &book.book_url)
                .await
        } else {
            state
                .local_txt_book_service
                .delete_book_files(&user.ns, &book.book_url)
                .await
        };
        if let Err(cleanup_err) = cleanup {
            tracing::warn!(
                "failed to clean up rejected local book {}: {:?}",
                book.book_url,
                cleanup_err
            );
        }
        return Err(err);
    }

    let saved = state
        .book_service
        .save_book(&user.ns, book)
        .await
        .map_err(Problem::from)?;
    Ok(Json(saved))
}
