//! 本地书上传（v1）：`POST /v1/local-books`，TXT/EPUB 按扩展名分派。
//!
//! 与 `/reader3/uploadTxtBook|uploadEpubBook` 共用同一套导入服务与限额；
//! 差异只在契约：单一路径按扩展名分派（`.epub` 走 EPUB，其余按 TXT），
//! 响应是不带包装的 Book。这些路由**不进 120s 常规组**（大文件 + 慢速
//! 链路），由 `v1::router` 在超时层之外 merge，子路由自带 900s 上传超时、
//! 100MB 体限、鉴权与 CSRF。

use crate::api::problem::{ErrorCode, Problem};
use crate::api::v1::{V1Query, V1User};
use crate::api::AppState;
use crate::model::book::Book;
use crate::service::local_epub_book;
use axum::body::Body;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::header;
use axum::response::Response;
use axum::routing::{get, post};
use axum::Json;
use axum::Router;
use serde::Deserialize;

/// 本地书资源读取：挂在 v1 常规鉴权/并发/超时组里（GET，无上传体）。
pub fn asset_router() -> Router<AppState> {
    Router::new().route(
        "/v1/local-books/assets/{bookRef}",
        get(get_local_book_asset).layer(axum::middleware::from_fn(
            crate::api::router::sandbox_epub_svg_assets,
        )),
    )
}

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

#[derive(Debug, Deserialize)]
pub struct LocalBookAssetQuery {
    /// EPUB 内部资源路径（zip 内相对路径）
    pub path: String,
}

/// 读取本地 EPUB 的内部资源（封面、插图等）。
///
/// `bookRef` 是本地书 URL `local-epub:<hash>` 里的 32 位 hash——导入时还没
/// 分配 bookId，章节 HTML 与封面 URL 只能带这个稳定引用。服务端确认登记行
/// 存在后，用调用者的 `user_ns` 拼资源目录读取；跨用户隔离由目录前缀完成
/// （`books` 登记表本身是整部署共享的）。旧数据里的 legacy 资源 URL 由
/// 服务层在读正文/封面时改写成本路径。
#[utoipa::path(
    get,
    path = "/v1/local-books/assets/{bookRef}",
    tag = "books",
    operation_id = "getLocalBookAsset",
    params(
        ("bookRef" = String, Path, description = "本地 EPUB 的 32 位 hash（local-epub:<hash> 的 hash 部分）"),
        ("path" = String, Query, description = "EPUB 内部资源路径"),
    ),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "资源字节；Content-Type 按 EPUB manifest 给出"),
        (status = 400, description = "资源路径非法或资源不存在", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "本地书不存在或未登记", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_local_book_asset(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(book_ref): Path<String>,
    V1Query(query): V1Query<LocalBookAssetQuery>,
) -> Result<Response, Problem> {
    let book_url = local_epub_book::local_url_from_asset_ref(&book_ref)
        .ok_or_else(|| Problem::new(ErrorCode::BookNotFound).detail("本地书资源不存在"))?;
    // 本地 EPUB 的 hash 由 user_ns + 文件名 + 内容共同决定，路径本身也按
    // user_ns 隔离；这里再确认登记行存在，避免对任意 hash 做无谓的文件探测。
    let registered = state
        .book_service
        .book_repo()
        .get_by_url(&user.ns, &book_url)
        .await
        .map_err(Problem::from)?
        .is_some();
    if !registered {
        return Err(Problem::new(ErrorCode::BookNotFound).detail("本地书资源不存在"));
    }
    let asset = state
        .local_epub_book_service
        .get_asset(&user.ns, &book_url, &query.path)
        .await
        .map_err(Problem::from)?;
    let mut response = Response::new(Body::from(asset.bytes));
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("private, max-age=3600"),
    );
    if let Ok(value) = header::HeaderValue::from_str(&asset.content_type) {
        headers.insert(header::CONTENT_TYPE, value);
    }
    Ok(response)
}
