use serde::{Deserialize, Serialize};

/// 一条评论（章评或段评）。
///
/// 字段全部由书源的评论规则解析而来，缺规则就是缺字段，前端按缺省渲染。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ReviewItem {
    pub id: String,
    pub name: String,
    pub avatar: String,
    pub content: String,
    /// 原始发布时间字符串。来源站点的格式五花八门，这里不做归一化，
    /// 前端只做展示；需要排序时用站点自己的顺序。
    pub time: String,
    pub digg: i64,
    pub reply_count: i64,
    pub replies: Vec<ReviewReply>,
    /// 评论配图。同一个图站点常给出多个变体（番茄会同时给 HEIC 与 JPEG），
    /// 顺序原样保留，由客户端挑自己能渲染的那一个。
    pub images: Vec<String>,
}

/// 内联回复（评论下方直接展开的那几条）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ReviewReply {
    pub name: String,
    pub content: String,
    pub time: String,
    /// 被回复者；为空表示直接回复评论本身。
    pub reply_to: String,
}

/// 一页评论。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ReviewPage {
    /// 总条数；规则没给或取不到时为 `0`。
    pub total: i64,
    pub has_more: bool,
    pub page: i32,
    pub items: Vec<ReviewItem>,
}

impl ReviewPage {
    pub fn empty(page: i32) -> Self {
        Self {
            page,
            ..Default::default()
        }
    }
}

/// 段评概览里的一段：哪一段、有多少条、段落原文是什么。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ParaReviewCount {
    /// 段号，等于正文按 `\n` 切分后的下标（从 0 开始）。
    pub para_index: i32,
    pub count: i64,
    /// 段落原文（截断）。段号会因用户的书源替换规则、繁简转换而漂移，
    /// 前端拿它做兜底定位。
    pub text: String,
}

/// 段评概览。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ParaReviewIndex {
    pub paras: Vec<ParaReviewCount>,
}

/// 评论接口的响应体：把「书源支不支持」也告诉前端，
/// 前端据此决定要不要渲染气泡与评论入口。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewResponse<T> {
    /// 当前书源是否声明了对应的评论规则。
    pub enabled: bool,
    pub data: T,
}

impl<T> ReviewResponse<T> {
    pub fn new(enabled: bool, data: T) -> Self {
        Self { enabled, data }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_page_keeps_page_number() {
        let page = ReviewPage::empty(3);
        assert_eq!(page.page, 3);
        assert_eq!(page.total, 0);
        assert!(!page.has_more);
        assert!(page.items.is_empty());
    }
}
