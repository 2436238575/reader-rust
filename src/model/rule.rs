use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(default, rename_all = "camelCase")]
#[ts(
    export,
    export_to = "../frontend/src/types/generated/",
    rename_all = "camelCase"
)]
pub struct SearchRule {
    #[ts(optional)]
    pub check_key_word: Option<String>,
    #[ts(optional)]
    pub book_list: Option<String>,
    #[ts(optional)]
    pub name: Option<String>,
    #[ts(optional)]
    pub author: Option<String>,
    #[ts(optional)]
    pub intro: Option<String>,
    #[ts(optional)]
    pub kind: Option<String>,
    #[ts(optional)]
    pub last_chapter: Option<String>,
    #[ts(optional)]
    pub update_time: Option<String>,
    #[ts(optional)]
    pub book_url: Option<String>,
    #[ts(optional)]
    pub cover_url: Option<String>,
    #[ts(optional)]
    pub word_count: Option<String>,
}

pub type ExploreRule = SearchRule;

#[derive(Debug, Clone, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(default, rename_all = "camelCase")]
#[ts(
    export,
    export_to = "../frontend/src/types/generated/",
    rename_all = "camelCase"
)]
pub struct BookInfoRule {
    #[ts(optional)]
    pub init: Option<String>,
    #[ts(optional)]
    pub name: Option<String>,
    #[ts(optional)]
    pub author: Option<String>,
    #[ts(optional)]
    pub intro: Option<String>,
    #[ts(optional)]
    pub kind: Option<String>,
    #[ts(optional)]
    pub last_chapter: Option<String>,
    #[ts(optional)]
    pub update_time: Option<String>,
    #[ts(optional)]
    pub cover_url: Option<String>,
    #[ts(optional)]
    pub word_count: Option<String>,
    #[ts(optional)]
    pub toc_url: Option<String>,
    #[ts(optional)]
    pub can_re_name: Option<String>,
    #[ts(optional)]
    pub download_urls: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(default, rename_all = "camelCase")]
#[ts(
    export,
    export_to = "../frontend/src/types/generated/",
    rename_all = "camelCase"
)]
pub struct TocRule {
    #[ts(optional)]
    pub pre_update_js: Option<String>,
    #[ts(optional)]
    pub init: Option<String>,
    #[ts(optional)]
    pub chapter_list: Option<String>,
    #[ts(optional)]
    pub chapter_name: Option<String>,
    #[ts(optional)]
    pub chapter_url: Option<String>,
    #[ts(optional)]
    pub format_js: Option<String>,
    #[ts(optional)]
    pub is_volume: Option<String>,
    #[ts(optional)]
    pub is_vip: Option<String>,
    #[ts(optional)]
    pub is_pay: Option<String>,
    #[ts(optional)]
    pub update_time: Option<String>,
    /// 卷名（本项目扩展）：规则挂在每个章节条目上，
    /// 目录 UI 把卷名相同的连续章节归到同一个卷标题下
    #[ts(optional)]
    pub volume_name: Option<String>,
    /// 字数（本项目扩展）：目录里随章节条目展示
    #[ts(optional)]
    pub word_count: Option<String>,
    #[ts(optional)]
    pub next_toc_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(default, rename_all = "camelCase")]
#[ts(
    export,
    export_to = "../frontend/src/types/generated/",
    rename_all = "camelCase"
)]
pub struct ContentRule {
    #[ts(optional)]
    pub content: Option<String>,
    #[ts(optional)]
    pub title: Option<String>,
    #[ts(optional)]
    pub next_content_url: Option<String>,
    #[ts(optional)]
    pub web_js: Option<String>,
    #[ts(optional)]
    pub source_regex: Option<String>,
    #[ts(optional)]
    pub replace_regex: Option<String>,
    #[ts(optional)]
    pub image_style: Option<String>,
    #[ts(optional)]
    pub image_decode: Option<String>,
    #[ts(optional)]
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
#[derive(Debug, Clone, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(default, rename_all = "camelCase")]
#[ts(
    export,
    export_to = "../frontend/src/types/generated/",
    rename_all = "camelCase"
)]
pub struct ReviewRule {
    /// 章评列表 URL 模板，对章节正文响应求值。
    #[ts(optional)]
    pub review_url: Option<String>,
    #[ts(optional)]
    pub avatar_rule: Option<String>,
    #[ts(optional)]
    pub content_rule: Option<String>,
    #[ts(optional)]
    pub post_time_rule: Option<String>,
    #[ts(optional)]
    pub review_quote_url: Option<String>,
    #[ts(optional)]
    pub vote_up_url: Option<String>,
    #[ts(optional)]
    pub vote_down_url: Option<String>,
    #[ts(optional)]
    pub post_review_url: Option<String>,
    #[ts(optional)]
    pub post_quote_url: Option<String>,
    #[ts(optional)]
    pub delete_url: Option<String>,
    /// 评论列表项的选择规则（JSONPath / CSS / XPath / 正则）。
    #[ts(optional)]
    pub list_rule: Option<String>,
    /// 评论 ID，用于前端去重与「加载更多」拼接。
    #[ts(optional)]
    pub id_rule: Option<String>,
    /// 用户名。
    #[ts(optional)]
    pub name_rule: Option<String>,
    /// 点赞数。
    #[ts(optional)]
    pub digg_rule: Option<String>,
    /// 回复数。
    #[ts(optional)]
    pub reply_count_rule: Option<String>,
    /// 评论总数，用于「本章评论 · N」。
    #[ts(optional)]
    pub total_rule: Option<String>,
    /// 是否还有下一页。
    #[ts(optional)]
    pub has_more_rule: Option<String>,
    /// 内联回复列表（`reply_list` 之类）的选择规则。
    #[ts(optional)]
    pub reply_list_rule: Option<String>,
    #[ts(optional)]
    pub reply_name_rule: Option<String>,
    #[ts(optional)]
    pub reply_content_rule: Option<String>,
    #[ts(optional)]
    pub reply_post_time_rule: Option<String>,
    /// 回复的目标用户名（「回复 @某人」）。
    #[ts(optional)]
    pub reply_to_rule: Option<String>,
    /// 评论者是作者本人（真值词表见规格 §15）。
    #[ts(optional)]
    pub author_rule: Option<String>,
    /// 作者赞过这条评论。
    #[ts(optional)]
    pub author_digg_rule: Option<String>,
    /// 回复者是作者本人。
    #[ts(optional)]
    pub reply_author_rule: Option<String>,
    /// 作者赞过这条回复。
    #[ts(optional)]
    pub reply_author_digg_rule: Option<String>,
    /// 评论配图。写成列表规则（如 `$.image_url[*]`）时取多个；JS 规则按行切分。
    #[ts(optional)]
    pub image_rule: Option<String>,
    /// 回复配图（楼中楼里的图片）。缺省时回落到 [`ReviewRule::image_rule`]。
    #[ts(optional)]
    pub reply_image_rule: Option<String>,
}

/// 段评（段落评论）规则。
///
/// 阅读3.0 没有段评概念，这是本项目新增的扩展规则；字段命名与
/// [`ReviewRule`] 保持一致，便于书源作者套用。
///
/// 段评分两步：先用 `indexUrl` 取「哪些段落有评论、各有多少条」的概览，
/// 再用 `reviewUrl` 取某一段的评论列表。两个 URL 都对章节正文响应求值，
/// 模板里可额外使用 `{{paraIndex}}`。
#[derive(Debug, Clone, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(default, rename_all = "camelCase")]
#[ts(
    export,
    export_to = "../frontend/src/types/generated/",
    rename_all = "camelCase"
)]
pub struct ParaReviewRule {
    /// 段评概览 URL 模板，对章节正文响应求值。
    #[ts(optional)]
    pub index_url: Option<String>,
    /// 概览里的「段号 → 该段数据」映射，通常是一个对象。
    #[ts(optional)]
    pub index_list_rule: Option<String>,
    /// 从单段数据里取评论条数。
    #[ts(optional)]
    pub index_count_rule: Option<String>,
    /// 从单段数据里取「作者评论/点赞过」标记；规则缺省时该标记恒为 false。
    #[ts(optional)]
    pub index_author_commented_rule: Option<String>,
    /// 段评列表 URL 模板；模板里可用 `{{paraIndex}}`。
    #[ts(optional)]
    pub review_url: Option<String>,
    #[ts(optional)]
    pub list_rule: Option<String>,
    #[ts(optional)]
    pub id_rule: Option<String>,
    #[ts(optional)]
    pub name_rule: Option<String>,
    #[ts(optional)]
    pub avatar_rule: Option<String>,
    #[ts(optional)]
    pub content_rule: Option<String>,
    #[ts(optional)]
    pub post_time_rule: Option<String>,
    #[ts(optional)]
    pub digg_rule: Option<String>,
    #[ts(optional)]
    pub total_rule: Option<String>,
    #[ts(optional)]
    pub has_more_rule: Option<String>,
    /// 回复数。段评列表里通常没有内联回复，只有条数，用于「共 N 条回复」。
    #[ts(optional)]
    pub reply_count_rule: Option<String>,
    /// 内联回复列表（有的站点段评也带 `reply_list`）。
    #[ts(optional)]
    pub reply_list_rule: Option<String>,
    #[ts(optional)]
    pub reply_name_rule: Option<String>,
    #[ts(optional)]
    pub reply_content_rule: Option<String>,
    #[ts(optional)]
    pub reply_post_time_rule: Option<String>,
    #[ts(optional)]
    pub reply_to_rule: Option<String>,
    /// 评论者/回复者是作者本人、作者赞过（同 [`ReviewRule`] 的四个字段）。
    #[ts(optional)]
    pub author_rule: Option<String>,
    #[ts(optional)]
    pub author_digg_rule: Option<String>,
    #[ts(optional)]
    pub reply_author_rule: Option<String>,
    #[ts(optional)]
    pub reply_author_digg_rule: Option<String>,
    /// 评论配图，同 [`ReviewRule::image_rule`]。
    #[ts(optional)]
    pub image_rule: Option<String>,
    /// 回复配图，同 [`ReviewRule::reply_image_rule`]。
    #[ts(optional)]
    pub reply_image_rule: Option<String>,
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
#[derive(Debug, Clone, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(default, rename_all = "camelCase")]
#[ts(
    export,
    export_to = "../frontend/src/types/generated/",
    rename_all = "camelCase"
)]
pub struct ContentImageRule {
    /// 配图接口 URL 模板，对章节正文响应求值。
    #[ts(optional)]
    pub image_url: Option<String>,
    /// 配图列表规则，如 `$.data.images[*]`（HTML 站点写 CSS 选择器）。
    #[ts(optional)]
    pub list_rule: Option<String>,
    /// 单张图的地址规则，默认 `url`（HTML 站点默认取 `img` 的 `src`）。
    #[ts(optional)]
    pub url_rule: Option<String>,
    /// 图片说明文字，默认 `caption`。
    #[ts(optional)]
    pub caption_rule: Option<String>,
    /// 插入位置：正文按 `\n` 切分后的行号（从 0 开始，插在该行之前），
    /// 默认 `para_index`。取不到位置时图片排在章末。
    #[ts(optional)]
    pub para_index_rule: Option<String>,
    /// 原始宽高，用于给图片占位（避免加载时页面跳动），默认 `width` / `height`。
    #[ts(optional)]
    pub width_rule: Option<String>,
    #[ts(optional)]
    pub height_rule: Option<String>,
}
