mod agent;
mod ai_book;
mod ai_model;
mod ai_proxy;
mod book;
mod book_group;
mod book_source;
mod bookmark;
mod cache;
mod chapter_edit;
mod chapter_image;
mod image;
mod multipart;
mod replace_rule;
mod review;
mod search_cache;
mod user;
mod webdav;

pub use agent::*;
pub use ai_book::*;
pub use ai_model::*;
pub use ai_proxy::*;
pub use book::*;
pub use book_group::*;
pub use book_source::*;
pub use bookmark::{
    delete_bookmark, delete_bookmarks, get_bookmarks, save_bookmark, save_bookmarks,
};
pub use cache::{cache_stats, purge_cache};
pub use chapter_edit::{
    delete_chapter_edit, get_chapter_edit, get_chapter_import_diff, get_chapter_timeline,
    get_chapter_version, get_chapter_version_diff, import_chapter_text, import_chapter_version,
    list_chapter_edits, preview_chapter_import, reset_book_to_upstream, save_chapter_edit,
    select_chapter_version,
};
pub use chapter_image::get_chapter_images;
pub use image::{get_book_cover, get_image};
pub use replace_rule::{
    delete_replace_rule, delete_replace_rules, get_replace_rules, save_replace_rule,
    save_replace_rules,
};
pub use review::{get_chapter_comments, get_para_comment_index, get_para_comments};
pub use search_cache::search_cached_content;
pub use user::{
    change_password, delete_file, get_user_config, get_user_info, get_userdata, get_userdata_batch,
    login, logout, refresh_token, save_user_config, save_userdata, save_userdata_batch,
    upload_file,
};
pub(crate) use webdav::sweep_webdav_upload_artifacts;
pub use webdav::{
    delete_webdav_file, delete_webdav_file_list, get_webdav_file, get_webdav_file_list,
    upload_file_to_webdav, webdav_handler,
};

use crate::error::error::{ApiResponse, AppError};
use axum::response::IntoResponse;
use axum::Json;

/// `/health` 的响应体（`ApiResponse` 的 `data` 字段）。
///
/// `version` 在编译期由 `CARGO_PKG_VERSION` 写入，运行实例由此自报版本；
/// 容器 HEALTHCHECK 与外部探活只依赖 HTTP 200 与 `status` 字段。
fn health_payload() -> serde_json::Value {
    serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    })
}

pub async fn health() -> impl IntoResponse {
    Json(ApiResponse::ok(health_payload()))
}

/// `/reader3` 命名空间下未匹配的路径。
///
/// 挂在 API 路由组末尾，避免未知接口落到静态文件服务上返回一个
/// 没有响应体的 404——接口调用方需要的是能解析的 JSON。
pub async fn api_not_found() -> AppError {
    AppError::NotFound("接口不存在".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// /health 的 `data` 是带版本号的对象（不是旧的裸 "ok" 字符串），
    /// 锁死响应形状，防止后续改动悄悄破坏探活方与文档示例
    #[test]
    fn health_payload_reports_status_and_version() {
        let body = serde_json::to_value(ApiResponse::ok(health_payload())).unwrap();
        assert_eq!(body["isSuccess"], true);
        assert_eq!(body["errorMsg"], "");
        assert_eq!(body["data"]["status"], "ok");
        assert_eq!(body["data"]["version"], env!("CARGO_PKG_VERSION"));
    }
}
