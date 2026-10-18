use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct SearchBook {
    pub name: String,
    pub author: String,
    pub book_url: String,
    pub origin: String,
    /// 登记分配的 `book_<n>`：搜索结果产出即登记，后续详情/目录/正文请求只用它
    #[serde(skip_serializing_if = "Option::is_none")]
    pub book_id: Option<String>,
    /// 该书当前命中的书源 id
    #[serde(skip_serializing_if = "Option::is_none")]
    pub book_source_id: Option<i64>,
    pub cover_url: Option<String>,
    pub intro: Option<String>,
    pub kind: Option<String>,
    pub last_chapter: Option<String>,
    pub update_time: Option<String>,
    pub word_count: Option<String>,
    /// 同一本书在各书源的命中（聚合结果用），值是 bookSourceId
    #[serde(skip_serializing_if = "Option::is_none")]
    pub book_source_ids: Option<Vec<i64>>,
}

impl SearchBook {
    /// Generate a key for merging books with same name and author
    pub fn merge_key(&self) -> String {
        let name = self.name.trim().to_lowercase();
        let author = self.author.trim().to_lowercase();
        format!("{}|{}", name, author)
    }
}
