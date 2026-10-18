use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Bookmark {
    /// 所属书籍的 bookId（`book_<n>`）。书签身份 = (bookId, chapterIndex, chapterPos)，
    /// 一本书可有多条；bookName/bookAuthor 只是展示快照，不参与判等。
    pub book_id: String,
    pub time: i64,
    pub book_name: String,
    pub book_author: String,
    pub chapter_index: i32,
    pub chapter_pos: i32,
    pub chapter_name: String,
    pub book_text: String,
    pub content: String,
}

impl Bookmark {
    /// 同一本书同一位置视为同一条书签：重复保存覆盖，不产生重复条目。
    pub fn identity(&self) -> (String, i32, i32) {
        (self.book_id.clone(), self.chapter_index, self.chapter_pos)
    }
}
