//! ASSETS_DIR 写盘的统一出口。
//!
//! `uploadFile`（multipart 上传）与 AI 资料的地图落盘共用这一条路径，
//! 保证安全原语（目录段/文件名消毒、越界检查）与 URL 形态只有一份实现。

use std::path::Path;

use tokio::fs;

use crate::error::error::AppError;
use crate::util::safe_path::{resolve_within, sanitize_dir_segment, sanitize_file_name};

/// 把字节写入 `<ASSETS_DIR>/<user_ns>/<file_type>/<file_name>`，
/// 返回可直接访问的 `/assets/{ns}/{type}/{name}` URL。
///
/// 与静态路由同源：静态服务把 ASSETS_DIR 挂在 `/assets` 下，写入端必须用
/// 同一个目录，否则自定义 ASSETS_DIR 后文件取不到。
pub async fn write_asset_file(
    assets_dir: &str,
    user_ns: &str,
    file_type: &str,
    file_name: &str,
    bytes: Vec<u8>,
) -> Result<String, AppError> {
    let Some(file_type) = sanitize_dir_segment(file_type) else {
        return Err(AppError::BadRequest("文件类型不合法".to_string()));
    };
    let Some(name) = sanitize_file_name(file_name) else {
        return Err(AppError::BadRequest("文件名不合法".to_string()));
    };
    let Some(ns) = sanitize_dir_segment(user_ns) else {
        return Err(AppError::BadRequest("用户命名空间不合法".to_string()));
    };

    let assets_root = Path::new(assets_dir);
    let relative = Path::new(&ns).join(&file_type).join(&name);
    let Some(path) = resolve_within(assets_root, &relative) else {
        return Err(AppError::BadRequest("文件名不合法".to_string()));
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
    }
    fs::write(&path, bytes)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    Ok(format!("/assets/{}/{}/{}", ns, file_type, name))
}
