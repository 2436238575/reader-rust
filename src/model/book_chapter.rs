use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct BookChapter {
    pub title: String,
    pub url: String,
    pub index: i32,
    pub tag: Option<String>,
    pub is_vip: bool,
    pub is_pay: bool,
    pub is_volume: bool,
    /// 卷名：书源目录规则的 `volumeName` 求值结果，
    /// 相同卷名的连续章节在目录 UI 上归为一组
    pub volume: Option<String>,
    /// 字数：书源目录规则的 `wordCount` 求值结果，目录里随章节展示
    pub word_count: Option<String>,
}
