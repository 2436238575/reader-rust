mod kernel;

mod content;
mod info;
mod review;
mod search;
mod toc;
use kernel::{
    eval_field_html_with_ctx, eval_field_json_with_ctx, extract_js, normalize_list_rule,
    resolve_url, split_legado_regex, strip_mode_prefix,
};
use review::has_rule;

use crate::model::book_source::BookSource;
use crate::model::chapter_image::ChapterImage;
use crate::model::rule::ContentImageRule;
use crate::parser::{
    html,
    js::{begin_parse_budget, eval_js, with_book_source},
    jsonpath,
};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Clone, Default)]
pub struct RuleEngine;

#[derive(Debug, Clone, PartialEq)]
enum ParseMode {
    Css,      // CSS selector
    XPath,    // XPath expression
    JsonPath, // JSONPath expression
    Regex,    // Regex pattern
    Js,       // JavaScript
}

impl RuleEngine {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self)
    }

    /// Detect the parsing mode from the rule string
    fn detect_mode(&self, rule: &str, content: &str) -> ParseMode {
        let rule = rule.trim();

        // Explicit mode forcing
        if rule.starts_with("@css:") || rule.starts_with("@CSS:") {
            return ParseMode::Css;
        }
        if rule.starts_with("@xpath:") || rule.starts_with("@XPath:") || rule.starts_with("@XPATH:")
        {
            return ParseMode::XPath;
        }
        if rule.starts_with("@json:") || rule.starts_with("@Json:") || rule.starts_with("@JSON:") {
            return ParseMode::JsonPath;
        }
        if rule.starts_with("@regex:") || rule.starts_with("@Regex:") {
            return ParseMode::Regex;
        }
        if rule.starts_with("js:") || rule.starts_with("@js:") || rule.starts_with("<js>") {
            return ParseMode::Js;
        }

        // Auto-detect from rule prefix
        if rule.starts_with('/') || rule.starts_with("./") {
            return ParseMode::XPath;
        }
        if rule.starts_with("$.") || rule.starts_with("$[") {
            return ParseMode::JsonPath;
        }
        if rule.starts_with(':') {
            return ParseMode::Regex;
        }

        // Auto-detect from content（规则的 `$.`/`$[` 前缀已在上方提前返回，这里只剩内容判定）
        let content_trimmed = content.trim();
        if content_trimmed.starts_with('{') || content_trimmed.starts_with('[') {
            // Try to parse as JSON
            if serde_json::from_str::<Value>(content_trimmed).is_ok() {
                return ParseMode::JsonPath;
            }
        }

        // Default to CSS
        ParseMode::Css
    }

    /// Strip mode prefix from rule
    fn strip_mode_prefix<'a>(&self, rule: &'a str) -> &'a str {
        strip_mode_prefix(rule)
    }

    /// 书源是否声明了章节配图规则。
    pub fn has_content_image_rule(&self, source: &BookSource) -> bool {
        let Some(rule) = source.rule_content_image.as_ref() else {
            return false;
        };
        has_rule(rule.image_url.as_deref()) && has_rule(rule.list_rule.as_deref())
    }

    /// 求值配图接口地址。
    ///
    /// 与评论 URL 同理：配图接口的地址通常要用正文响应里的书籍/章节 ID 拼，
    /// 所以规则对**章节正文响应**求值，模板里可以写
    /// `content/image?item_id={{$.data.data.novel_data.item_id}}`。
    pub fn content_image_url(
        &self,
        source: &BookSource,
        body: &str,
        base_url: &str,
    ) -> Option<String> {
        let _budget = begin_parse_budget();
        let rule = source.rule_content_image.as_ref()?.image_url.clone()?;
        with_book_source(source, || {
            self.review_url(&rule, body, base_url, &HashMap::new())
        })
    }

    /// 解析章节配图列表。
    ///
    /// 只管「配图另走一个接口」的站点；正文 HTML 里直接内嵌 `<img>` 的书源
    /// 不需要规则，由渲染端显示。
    pub fn content_images(
        &self,
        source: &BookSource,
        body: &str,
        base_url: &str,
    ) -> Vec<ChapterImage> {
        let _budget = begin_parse_budget();
        let Some(rule) = source.rule_content_image.as_ref() else {
            return Vec::new();
        };
        if !has_rule(rule.list_rule.as_deref()) {
            return Vec::new();
        }
        with_book_source(source, || parse_chapter_images(rule, body, base_url))
    }
}

/// 在元素内部按 CSS 选择器取子元素。
///
/// 文档级的列表规则支持 `&&` 等组合符，元素级这里只认单条 CSS 选择器——
/// 内联回复的规则实际都很简单。
fn select_child_elements<'a>(
    el: &scraper::ElementRef<'a>,
    selector: &str,
) -> Vec<scraper::ElementRef<'a>> {
    let selector = selector.split("@@").next().unwrap_or(selector).trim();
    let Ok(parsed) = scraper::Selector::parse(selector) else {
        return Vec::new();
    };
    el.select(&parsed).collect()
}

/// 求值「图片列表」规则。
///
/// 与普通字段规则不同，图片天然是多个值：站点常给同一张图的多个变体
/// （番茄同时给 HEIC 和 JPEG），所以这里返回列表而不是单个字符串。
/// 支持三种写法：
///
/// - JSONPath 列表：`$.image_url[*]`
/// - JS：返回多行文本，逐行当作一个地址
/// - 普通字段名：该字段是数组就展开，是字符串就取一个
fn eval_image_list_json(rule: &str, node: &Value, base_url: &str) -> Vec<String> {
    let rule = rule.trim();
    if rule.is_empty() {
        return Vec::new();
    }
    let (pure_rule, _) = split_legado_regex(rule);
    let (pure, js, _) = extract_js(&pure_rule);
    // 与 `content` 等入口保持一致：裸 `js:` 前缀同样算 JS 规则
    let (pure, js) = match js {
        Some(script) => (pure, Some(script)),
        None if pure.trim_start().starts_with("js:") => ("", Some(strip_mode_prefix(pure))),
        None => (pure, None),
    };

    let values: Vec<Value> = if let Some(script) = js {
        eval_js(
            script,
            &serde_json::to_string(node).unwrap_or_default(),
            base_url,
        )
        .map(|out| {
            out.lines()
                .map(|line| Value::String(line.trim().to_string()))
                .collect()
        })
        .unwrap_or_default()
    } else if pure.trim_start().starts_with('$') {
        jsonpath::jsonpath_query_combined(node, pure.trim())
    } else {
        match node.get(pure.trim()) {
            Some(Value::Array(items)) => items.clone(),
            Some(other) => vec![other.clone()],
            None => Vec::new(),
        }
    };

    values
        .iter()
        .filter_map(jsonpath::value_to_string)
        .map(|url| url.trim().to_string())
        .filter(|url| !url.is_empty())
        .map(|url| resolve_url(base_url, &url))
        .collect()
}

/// 求值「图片列表」规则（HTML 分支）。
///
/// 规则写成 `选择器@属性`（如 `img@src`、`.cover img@data-src`），
/// 省略 `@属性` 时默认取 `src`。
fn eval_image_list_html(rule: &str, el: &scraper::ElementRef, base_url: &str) -> Vec<String> {
    let rule = rule.trim();
    if rule.is_empty() {
        return Vec::new();
    }
    let (selector, attr) = match rule.split_once('@') {
        Some((selector, attr)) if !attr.trim().is_empty() => (selector.trim(), attr.trim()),
        _ => (rule, "src"),
    };
    let attr = attr.strip_prefix("img@").unwrap_or(attr).trim();
    select_child_elements(el, selector)
        .iter()
        .filter_map(|node| {
            node.value()
                .attr(attr)
                .map(str::to_string)
                // 选中的是容器时，退一步找它内部的 img
                .or_else(|| {
                    select_child_elements(node, "img")
                        .first()
                        .and_then(|img| img.value().attr(attr))
                        .map(str::to_string)
                })
        })
        .map(|url| url.trim().to_string())
        .filter(|url| !url.is_empty())
        .map(|url| resolve_url(base_url, &url))
        .collect()
}

/// 取字段规则，空规则用默认值兜底。
///
/// 配图接口的字段名（`url` / `caption` / `para_index`）是本站与 FQWeb 之间的
/// 约定，多数书源不用逐条写；写了自己那套就按自己的来。
fn image_field_rule<'a>(rule: Option<&'a str>, default: &'a str) -> &'a str {
    rule.map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(default)
}

/// 解析章节配图列表：JSON 站点走 JSONPath，HTML 站点走 CSS 选择器。
fn parse_chapter_images(rule: &ContentImageRule, body: &str, base_url: &str) -> Vec<ChapterImage> {
    let mut images = match serde_json::from_str::<Value>(body) {
        Ok(v) => parse_chapter_images_json(rule, &v, base_url),
        Err(_) => parse_chapter_images_html(rule, body, base_url),
    };
    // 位置是站点给的，顺序未必可靠；同一位置的图保持站点原序（稳定排序）。
    // 没给位置的排在最后——它们本来就只可能落在章末。
    images.sort_by_key(|image| image.para_index.unwrap_or(i32::MAX));
    images
}

fn parse_chapter_images_json(
    rule: &ContentImageRule,
    v: &Value,
    base_url: &str,
) -> Vec<ChapterImage> {
    let mut ctx = HashMap::new();
    let (list_rule, reverse) = normalize_list_rule(rule.list_rule.as_deref().unwrap_or(""));
    let nodes = jsonpath::jsonpath_query_combined(v, strip_mode_prefix(list_rule));
    let mut images = Vec::with_capacity(nodes.len());
    for node in nodes.iter() {
        let url_rule = image_field_rule(rule.url_rule.as_deref(), "url");
        let url = eval_field_json_with_ctx(url_rule, node, base_url, &mut ctx).unwrap_or_default();
        let url = url.trim();
        if url.is_empty() {
            continue;
        }
        images.push(ChapterImage {
            url: resolve_url(base_url, url),
            caption: eval_field_json_with_ctx(
                image_field_rule(rule.caption_rule.as_deref(), "caption"),
                node,
                base_url,
                &mut ctx,
            )
            .unwrap_or_default(),
            para_index: eval_field_json_with_ctx(
                image_field_rule(rule.para_index_rule.as_deref(), "para_index"),
                node,
                base_url,
                &mut ctx,
            )
            .and_then(|text| text.trim().parse::<i32>().ok()),
            width: image_number(
                rule.width_rule.as_deref(),
                "width",
                node,
                base_url,
                &mut ctx,
            ),
            height: image_number(
                rule.height_rule.as_deref(),
                "height",
                node,
                base_url,
                &mut ctx,
            ),
        });
    }
    if reverse {
        images.reverse();
    }
    images
}

fn parse_chapter_images_html(
    rule: &ContentImageRule,
    body: &str,
    base_url: &str,
) -> Vec<ChapterImage> {
    let doc = html::parse_document(body);
    let (list_rule, reverse) = normalize_list_rule(rule.list_rule.as_deref().unwrap_or(""));
    let nodes = html::select_list(&doc, strip_mode_prefix(list_rule));
    let mut ctx = HashMap::new();
    let mut images = Vec::with_capacity(nodes.len());
    for node in nodes.iter() {
        // 列表项可能本身就是 `<img>`（规则直接写 `img`），否则取它内部第一张图
        let element = if node.value().name() == "img" {
            *node
        } else {
            match select_child_elements(node, "img").into_iter().next() {
                Some(image) => image,
                None => *node,
            }
        };
        let url_rule = rule
            .url_rule
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let url = match url_rule {
            Some(url_rule) => eval_image_list_html(url_rule, node, base_url)
                .into_iter()
                .next(),
            None => element
                .value()
                .attr("src")
                .map(|src| resolve_url(base_url, src.trim())),
        };
        let Some(url) = url.filter(|url| !url.is_empty()) else {
            continue;
        };
        images.push(ChapterImage {
            url,
            caption: eval_field_html_with_ctx(
                image_field_rule(rule.caption_rule.as_deref(), "figcaption@text"),
                node,
                base_url,
                &mut ctx,
            )
            .unwrap_or_default(),
            para_index: eval_field_html_with_ctx(
                image_field_rule(rule.para_index_rule.as_deref(), ""),
                node,
                base_url,
                &mut ctx,
            )
            .and_then(|text| text.trim().parse::<i32>().ok()),
            // 番茄的富文本把原始尺寸写在 `img-width`/`img-height` 上，普通站点用 `width`/`height`
            width: attr_number(&element, &["img-width", "width"]),
            height: attr_number(&element, &["img-height", "height"]),
        });
    }
    if reverse {
        images.reverse();
    }
    images
}

/// 从 JSON 节点取一个数字字段（宽高用，取不到就是 0）。
fn image_number(
    rule: Option<&str>,
    default: &str,
    node: &Value,
    base_url: &str,
    ctx: &mut HashMap<String, String>,
) -> i32 {
    eval_field_json_with_ctx(image_field_rule(rule, default), node, base_url, ctx)
        .and_then(|text| text.trim().parse::<f64>().ok())
        .map(|value| value.round() as i32)
        .unwrap_or(0)
}

/// 从元素属性取一个数字（宽高用，取不到就是 0）。
fn attr_number(element: &scraper::ElementRef, attrs: &[&str]) -> i32 {
    attrs
        .iter()
        .find_map(|attr| element.value().attr(attr))
        .and_then(|text| text.trim().parse::<f64>().ok())
        .map(|value| value.round() as i32)
        .unwrap_or(0)
}

/// Extract chapter number from title
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::book_source::BookSource;
    use serde_json::json;

    #[test]
    fn test_detect_mode() {
        let engine = RuleEngine::new().unwrap();

        assert_eq!(engine.detect_mode("@css:.test", ""), ParseMode::Css);
        assert_eq!(engine.detect_mode("@xpath://div", ""), ParseMode::XPath);
        assert_eq!(engine.detect_mode("$.data.list", ""), ParseMode::JsonPath);
        assert_eq!(engine.detect_mode("/html/body/div", ""), ParseMode::XPath);
        assert_eq!(engine.detect_mode(".class", ""), ParseMode::Css);
        assert_eq!(engine.detect_mode("js:return 1", ""), ParseMode::Js);
        assert_eq!(engine.detect_mode("<js>return 1</js>", ""), ParseMode::Js);
    }

    /// 配图规则：只有 URL 模板 + 列表规则，字段名走默认值。
    fn image_source() -> BookSource {
        BookSource {
            book_source_name: "配图站点".to_string(),
            book_source_url: "http://192.168.100.99:9999/".to_string(),
            rule_content_image: Some(ContentImageRule {
                image_url: Some(
                    "content/image?item_id={{$.data.data.novel_data.item_id}}".to_string(),
                ),
                list_rule: Some("$.data.images[*]".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn image_body() -> String {
        json!({
            "data": {
                "item_id": "7173615518858150414",
                "has_image": true,
                "images": [{
                    "url": "http://img.example/a.jpeg?sign=1",
                    "width": 1400,
                    "height": 933,
                    "para_index": 100,
                    "caption": "配图（画师：奈月Oo）"
                }]
            }
        })
        .to_string()
    }

    // 与 review.rs 测试同款的正文响应样本：配图 URL 模板同样要从
    // 正文响应里取 item_id，拆分后各自保留一份副本。
    fn content_body() -> String {
        json!({
            "data": {
                "data": {
                    "content": "第一段\n第二段",
                    "novel_data": {
                        "book_id": "7143038691944959011",
                        "item_id": "7173615518858150414",
                        "version": "52754e01aa26a8dbd8c15cbcf7f4f64b_1_v5"
                    }
                }
            }
        })
        .to_string()
    }

    #[test]
    fn content_image_url_reads_ids_from_the_content_response() {
        let engine = RuleEngine::new().unwrap();
        let url = engine
            .content_image_url(
                &image_source(),
                &content_body(),
                "http://192.168.100.99:9999/content?item_id=1",
            )
            .unwrap();
        assert_eq!(
            url,
            "http://192.168.100.99:9999/content/image?item_id=7173615518858150414"
        );
    }

    #[test]
    fn content_images_parse_default_fields() {
        let engine = RuleEngine::new().unwrap();
        let images = engine.content_images(
            &image_source(),
            &image_body(),
            "http://192.168.100.99:9999/content/image?item_id=1",
        );
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].url, "http://img.example/a.jpeg?sign=1");
        assert_eq!(images[0].caption, "配图（画师：奈月Oo）");
        assert_eq!(images[0].para_index, Some(100));
        assert_eq!(images[0].width, 1400);
        assert_eq!(images[0].height, 933);
    }

    #[test]
    fn content_images_allow_custom_field_rules() {
        let engine = RuleEngine::new().unwrap();
        let mut source = image_source();
        source.rule_content_image = Some(ContentImageRule {
            image_url: Some("content/image".to_string()),
            list_rule: Some("$.data.pics[*]".to_string()),
            url_rule: Some("$.src".to_string()),
            caption_rule: Some("$.desc".to_string()),
            para_index_rule: Some("$.idx".to_string()),
            width_rule: Some("$.w".to_string()),
            height_rule: Some("$.h".to_string()),
        });
        let body = json!({
            "data": { "pics": [{
                "src": "http://img.example/b.png",
                "desc": "插图",
                "idx": "7",
                "w": "800",
                "h": 600
            }]}
        })
        .to_string();
        let images = engine.content_images(&source, &body, "http://img.example/api");
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].url, "http://img.example/b.png");
        assert_eq!(images[0].caption, "插图");
        assert_eq!(images[0].para_index, Some(7));
        assert_eq!(images[0].width, 800);
        assert_eq!(images[0].height, 600);
    }

    #[test]
    fn content_images_without_position_go_last() {
        let engine = RuleEngine::new().unwrap();
        let mut source = image_source();
        source.rule_content_image = Some(ContentImageRule {
            image_url: Some("content/image".to_string()),
            list_rule: Some("$.data.images[*]".to_string()),
            ..Default::default()
        });
        // 第二张没给 para_index，且位置乱序：没位置的排到最后，其余按位置升序
        let body = json!({
            "data": { "images": [
                { "url": "http://img.example/2.png", "para_index": 20 },
                { "url": "http://img.example/1.png", "para_index": 5 },
                { "url": "http://img.example/3.png" }
            ]}
        })
        .to_string();
        let images = engine.content_images(&source, &body, "http://img.example/api");
        let urls: Vec<&str> = images.iter().map(|image| image.url.as_str()).collect();
        assert_eq!(
            urls,
            vec![
                "http://img.example/1.png",
                "http://img.example/2.png",
                "http://img.example/3.png"
            ]
        );
        assert_eq!(images[2].para_index, None);
    }

    #[test]
    fn content_images_skip_items_without_url() {
        let engine = RuleEngine::new().unwrap();
        let body = json!({
            "data": { "images": [
                { "url": "", "para_index": 1 },
                { "url": "http://img.example/ok.png", "para_index": 2 }
            ]}
        })
        .to_string();
        let images = engine.content_images(&image_source(), &body, "http://img.example/api");
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].url, "http://img.example/ok.png");
    }

    #[test]
    fn content_images_parse_html_lists() {
        let engine = RuleEngine::new().unwrap();
        let mut source = image_source();
        source.rule_content_image = Some(ContentImageRule {
            image_url: Some("content/image".to_string()),
            list_rule: Some("figure.pic".to_string()),
            ..Default::default()
        });
        let body = "<html><body>\
            <figure class=\"pic\"><img src=\"/a.png\" img-width=\"1400\" img-height=\"933\">\
            <figcaption>配图（画师：奈月Oo）</figcaption></figure>\
            <figure class=\"pic\"><img data-src=\"/b.png\"></figure>\
            </body></html>";
        let images = engine.content_images(&source, body, "http://host/book/chapter");
        assert_eq!(images.len(), 1, "没有 src 的图应当跳过");
        assert_eq!(images[0].url, "http://host/a.png");
        assert_eq!(images[0].caption, "配图（画师：奈月Oo）");
        assert_eq!(images[0].width, 1400);
        assert_eq!(images[0].height, 933);
        assert_eq!(images[0].para_index, None);
    }

    #[test]
    fn content_images_html_accepts_attribute_rules() {
        let engine = RuleEngine::new().unwrap();
        let mut source = image_source();
        source.rule_content_image = Some(ContentImageRule {
            image_url: Some("content/image".to_string()),
            list_rule: Some("div.pic".to_string()),
            url_rule: Some("img@data-src".to_string()),
            ..Default::default()
        });
        let body = "<html><body><div class=\"pic\"><img data-src=\"//cdn.example/c.png\"></div></body></html>";
        let images = engine.content_images(&source, body, "https://host/chapter");
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].url, "https://cdn.example/c.png");
    }

    #[test]
    fn content_image_rule_needs_url_and_list() {
        let engine = RuleEngine::new().unwrap();
        let mut source = image_source();
        assert!(engine.has_content_image_rule(&source));

        source.rule_content_image = Some(ContentImageRule {
            image_url: Some("content/image".to_string()),
            list_rule: None,
            ..Default::default()
        });
        assert!(!engine.has_content_image_rule(&source));
        assert!(engine
            .content_images(&source, &image_body(), "http://host/api")
            .is_empty());

        source.rule_content_image = None;
        assert!(!engine.has_content_image_rule(&source));
        assert!(engine
            .content_images(&source, &image_body(), "http://host/api")
            .is_empty());
    }
}
