mod ai_book;
mod ai_model;
mod ai_proxy;
mod book;
mod book_group;
mod book_source;
mod bookmark;
mod cache;
mod chapter_image;
mod image;
mod multipart;
mod replace_rule;
mod review;
mod user;
mod webdav;

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
pub use chapter_image::get_chapter_images;
pub use image::{get_book_cover, get_image};
pub use replace_rule::{
    delete_replace_rule, delete_replace_rules, get_replace_rules, save_replace_rule,
    save_replace_rules,
};
pub use review::{get_chapter_comments, get_para_comment_index, get_para_comments};
pub use user::{
    change_password, delete_file, get_user_config, get_user_info, login, logout, save_user_config,
    upload_file,
};
pub use webdav::{
    delete_webdav_file, delete_webdav_file_list, get_webdav_file, get_webdav_file_list,
    upload_file_to_webdav, webdav_handler,
};

use crate::error::error::{ApiResponse, AppError};
use axum::response::IntoResponse;
use axum::Json;

pub async fn health() -> impl IntoResponse {
    Json(ApiResponse::ok("ok"))
}

/// `/reader3` 命名空间下未匹配的路径。
///
/// 挂在 API 路由组末尾，避免未知接口落到静态文件服务上返回一个
/// 没有响应体的 404——接口调用方需要的是能解析的 JSON。
pub async fn api_not_found() -> AppError {
    AppError::NotFound("接口不存在".to_string())
}
