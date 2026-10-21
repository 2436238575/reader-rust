use serde::{Deserialize, Serialize};

/// 旧客户端不带 kind 的请求一律按章书签落库（serde default 只给空串，
/// 判等语义要求显式值）。
fn default_kind() -> String {
    "bookmark".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Bookmark {
    /// 所属书籍的 bookId（`book_<n>`）。书签身份 = (bookId, chapterIndex, chapterPos, kind)，
    /// 一本书可有多条；bookName/bookAuthor 只是展示快照，不参与判等。
    pub book_id: String,
    pub time: i64,
    pub book_name: String,
    pub book_author: String,
    pub chapter_index: i32,
    /// 万分比章内进度（0–10000）：章书签恒 0，划线取收藏那一刻的章内进度。
    pub chapter_pos: i32,
    pub chapter_name: String,
    pub book_text: String,
    pub content: String,
    /// 类型：`bookmark` 章书签 / `underline` 划线（选文字收藏）。
    #[serde(default = "default_kind")]
    pub kind: String,
}
