use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct SearchRule {
    pub check_key_word: Option<String>,
    pub book_list: Option<String>,
    pub name: Option<String>,
    pub author: Option<String>,
    pub intro: Option<String>,
    pub kind: Option<String>,
    pub last_chapter: Option<String>,
    pub update_time: Option<String>,
    pub book_url: Option<String>,
    pub cover_url: Option<String>,
    pub word_count: Option<String>,
}

pub type ExploreRule = SearchRule;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct BookInfoRule {
    pub init: Option<String>,
    pub name: Option<String>,
    pub author: Option<String>,
    pub intro: Option<String>,
    pub kind: Option<String>,
    pub last_chapter: Option<String>,
    pub update_time: Option<String>,
    pub cover_url: Option<String>,
    pub word_count: Option<String>,
    pub toc_url: Option<String>,
    pub can_re_name: Option<String>,
    pub download_urls: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct TocRule {
    pub pre_update_js: Option<String>,
    pub init: Option<String>,
    pub chapter_list: Option<String>,
    pub chapter_name: Option<String>,
    pub chapter_url: Option<String>,
    pub format_js: Option<String>,
    pub is_volume: Option<String>,
    pub is_vip: Option<String>,
    pub is_pay: Option<String>,
    pub update_time: Option<String>,
    pub next_toc_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct ContentRule {
    pub content: Option<String>,
    pub title: Option<String>,
    pub next_content_url: Option<String>,
    pub web_js: Option<String>,
    pub source_regex: Option<String>,
    pub replace_regex: Option<String>,
    pub image_style: Option<String>,
    pub image_decode: Option<String>,
    pub pay_action: Option<String>,
}

/// 章评（章节评论）规则。
///
/// 阅读3.0 里 `ruleReview` 是「预留字段」，官方实现从不执行（见
/// `docs/reference/book-source-rules.md` 第 17 节），因此这里把它接上：
/// `reviewUrl` 按**章节正文响应**求值，得到该章的评论列表地址，
/// 其余字段描述如何从响应里取出一条评论。
///
/// 前四个字段沿用阅读3.0 的既有命名，后面的是本项目为「能真正渲染评论」
/// 补的解析字段。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct ReviewRule {
    /// 章评列表 URL 模板，对章节正文响应求值。
    pub review_url: Option<String>,
    pub avatar_rule: Option<String>,
    pub content_rule: Option<String>,
    pub post_time_rule: Option<String>,
    pub review_quote_url: Option<String>,
    pub vote_up_url: Option<String>,
    pub vote_down_url: Option<String>,
    pub post_review_url: Option<String>,
    pub post_quote_url: Option<String>,
    pub delete_url: Option<String>,
    /// 评论列表项的选择规则（JSONPath / CSS / XPath / 正则）。
    pub list_rule: Option<String>,
    /// 评论 ID，用于前端去重与「加载更多」拼接。
    pub id_rule: Option<String>,
    /// 用户名。
    pub name_rule: Option<String>,
    /// 点赞数。
    pub digg_rule: Option<String>,
    /// 回复数。
    pub reply_count_rule: Option<String>,
    /// 评论总数，用于「本章评论 · N」。
    pub total_rule: Option<String>,
    /// 是否还有下一页。
    pub has_more_rule: Option<String>,
    /// 内联回复列表（`reply_list` 之类）的选择规则。
    pub reply_list_rule: Option<String>,
    pub reply_name_rule: Option<String>,
    pub reply_content_rule: Option<String>,
    pub reply_post_time_rule: Option<String>,
    /// 回复的目标用户名（「回复 @某人」）。
    pub reply_to_rule: Option<String>,
    /// 评论配图。写成列表规则（如 `$.image_url[*]`）时取多个；JS 规则按行切分。
    pub image_rule: Option<String>,
}

/// 段评（段落评论）规则。
///
/// 阅读3.0 没有段评概念，这是本项目新增的扩展规则；字段命名与
/// [`ReviewRule`] 保持一致，便于书源作者套用。
///
/// 段评分两步：先用 `indexUrl` 取「哪些段落有评论、各有多少条」的概览，
/// 再用 `reviewUrl` 取某一段的评论列表。两个 URL 都对章节正文响应求值，
/// 模板里可额外使用 `{{paraIndex}}`。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct ParaReviewRule {
    /// 段评概览 URL 模板，对章节正文响应求值。
    pub index_url: Option<String>,
    /// 概览里的「段号 → 该段数据」映射，通常是一个对象。
    pub index_list_rule: Option<String>,
    /// 从单段数据里取评论条数。
    pub index_count_rule: Option<String>,
    /// 段评列表 URL 模板；模板里可用 `{{paraIndex}}`。
    pub review_url: Option<String>,
    pub list_rule: Option<String>,
    pub id_rule: Option<String>,
    pub name_rule: Option<String>,
    pub avatar_rule: Option<String>,
    pub content_rule: Option<String>,
    pub post_time_rule: Option<String>,
    pub digg_rule: Option<String>,
    pub total_rule: Option<String>,
    pub has_more_rule: Option<String>,
    /// 回复数。段评列表里通常没有内联回复，只有条数，用于「共 N 条回复」。
    pub reply_count_rule: Option<String>,
    /// 内联回复列表（有的站点段评也带 `reply_list`）。
    pub reply_list_rule: Option<String>,
    pub reply_name_rule: Option<String>,
    pub reply_content_rule: Option<String>,
    pub reply_post_time_rule: Option<String>,
    pub reply_to_rule: Option<String>,
    /// 评论配图，同 [`ReviewRule::image_rule`]。
    pub image_rule: Option<String>,
}

/// 章节配图规则（本项目扩展，阅读3.0 无此字段）。
///
/// 多数书源的配图就写在正文 HTML 里（`<img>`），渲染端直接显示，不需要规则；
/// 这一组规则是给「配图另走一个接口」的站点用的：先用 `imageUrl` 拿配图接口
/// 的地址，再用 `listRule` 取列表、用各字段规则取单张图的信息。
///
/// `imageUrl` 对**章节正文响应**求值（与 [`ReviewRule::review_url`] 同理），
/// 所以模板里可以直接引用正文响应里的字段，例如
/// `content/image?item_id={{$.data.data.novel_data.item_id}}`。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct ContentImageRule {
    /// 配图接口 URL 模板，对章节正文响应求值。
    pub image_url: Option<String>,
    /// 配图列表规则，如 `$.data.images[*]`（HTML 站点写 CSS 选择器）。
    pub list_rule: Option<String>,
    /// 单张图的地址规则，默认 `url`（HTML 站点默认取 `img` 的 `src`）。
    pub url_rule: Option<String>,
    /// 图片说明文字，默认 `caption`。
    pub caption_rule: Option<String>,
    /// 插入位置：正文按 `\n` 切分后的行号（从 0 开始，插在该行之前），
    /// 默认 `para_index`。取不到位置时图片排在章末。
    pub para_index_rule: Option<String>,
    /// 原始宽高，用于给图片占位（避免加载时页面跳动），默认 `width` / `height`。
    pub width_rule: Option<String>,
    pub height_rule: Option<String>,
}
