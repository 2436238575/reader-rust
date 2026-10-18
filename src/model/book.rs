use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Book {
    pub name: String,
    pub author: String,
    /// 后端分配的稳定身份（`book_<n>`），不随换源变化；请求身份参数一律用它。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub book_id: Option<String>,
    /// 当前绑定的书源 id；与 `origin`（书源 URL，数据字段）并行保存。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub book_source_id: Option<i64>,
    pub book_url: String,
    pub origin: String,
    pub origin_name: Option<String>,
    pub cover_url: Option<String>,
    pub toc_url: Option<String>,
    pub charset: Option<String>,
    pub custom_cover_url: Option<String>,
    pub can_update: Option<bool>,
    pub dur_chapter_index: Option<i32>,
    pub dur_chapter_pos: Option<i32>,
    pub dur_chapter_time: Option<i64>,
    pub dur_chapter_title: Option<String>,
    pub intro: Option<String>,
    pub latest_chapter_title: Option<String>,
    pub last_check_time: Option<i64>,
    pub total_chapter_num: Option<i32>,
    pub r#type: Option<i32>,
    pub group: Option<i64>,
    pub word_count: Option<String>,
    pub info_html: Option<String>,
    pub toc_html: Option<String>,
    pub kind: Option<String>,
    pub update_time: Option<String>,
    pub can_re_name: Option<String>,
    pub download_urls: Option<String>,
}
