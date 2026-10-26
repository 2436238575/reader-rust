//! 字段求值内核：所有用途簇共用的枢纽。
//!
//! 模板插值（`{{...}}` / `@get:{}`）、`@put`/`@get` 上下文、`##` 替换、
//! `<js>` 链式、CSS/XPath/JSONPath 三态字段求值、列表正则抓取与若干小工具。

use crate::parser::{
    html,
    js::{eval_js, eval_js_with_bindings},
    jsonpath,
};
use crate::util::text::normalize_source_url;
use once_cell::sync::Lazy;
use serde_json::Value;
use std::collections::HashMap;
use sxd_xpath::{Context as XPathContext, Value as XPathValue};

/// 规则模板/占位符的固定正则：热路径上对每个元素每个字段都会用到，
/// 每次调用重新编译是纯浪费（2000 章目录页此前要编译数万次）。
static TEMPLATE_JS_RE: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"\{\{(.*?)\}\}").unwrap());

pub(super) static INLINE_JS_RE: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"\{\{([^}]+)\}\}").unwrap());

static TEMPLATE_GET_RE: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"@get:\{([^}]+)\}").unwrap());

static REGEX_PLACEHOLDER_RE: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"\$(\d{1,2})").unwrap());

/// 「body > *」固定选择器（正则退化到全文档列表时用），只编译一次。
pub(super) static BODY_CHILDREN_SELECTOR: Lazy<scraper::Selector> =
    Lazy::new(|| scraper::Selector::parse("body > *").unwrap());

/// 文档级字段求值的共享视图：把整份 DOM 序列化成 HTML 字符串是
/// O(文档大小) 的工作，book_info 一次要对同一份文档求值十余个字段，
/// 用 `OnceCell` 让多个字段复用同一次序列化结果。
pub(super) struct DocView<'a> {
    doc: &'a scraper::Html,
    serialized: std::cell::OnceCell<String>,
}

impl<'a> DocView<'a> {
    pub(super) fn new(doc: &'a scraper::Html) -> Self {
        Self {
            doc,
            serialized: std::cell::OnceCell::new(),
        }
    }

    pub(super) fn html(&self) -> &str {
        self.serialized.get_or_init(|| self.doc.html())
    }

    pub(super) fn doc(&self) -> &'a scraper::Html {
        self.doc
    }
}

pub(super) fn select_json_scope(
    v: &Value,
    init_rule: Option<&str>,
    base_url: &str,
    ctx: &mut HashMap<String, String>,
) -> Value {
    let Some(init_rule) = init_rule.map(str::trim).filter(|s| !s.is_empty()) else {
        return v.clone();
    };

    if let Some(res) = try_put_get(init_rule, ctx, |r, ctx| {
        eval_field_json_with_ctx(r, v, base_url, ctx)
    }) {
        let _ = res;
        return v.clone();
    }

    let interpolated = interpolate_json_templates(init_rule, v, base_url, ctx);
    let (pure_rule, _) = split_legado_regex(&interpolated);
    let (pure, _, _) = extract_js(&pure_rule);
    if pure.is_empty() {
        return v.clone();
    }

    jsonpath::jsonpath_query(v, pure)
        .into_iter()
        .next()
        .unwrap_or_else(|| v.clone())
}

pub(super) fn pick_json_field(v: &Value, rule: Option<&str>) -> Option<String> {
    let rule = rule?;
    if rule.trim_start().starts_with('$') {
        return jsonpath::jsonpath_first_string(v, rule);
    }
    if let Some(obj) = v.as_object() {
        if let Some(val) = obj.get(rule) {
            return jsonpath::value_to_string(val);
        }
    }
    None
}

pub(super) fn resolve_url(base: &str, url: &str) -> String {
    let base = normalize_source_url(base);
    let url = normalize_source_url(strip_url_config(url));

    if url.is_empty() {
        return base.to_string();
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        return url.to_string();
    }
    if url.starts_with("//") {
        return format!("https:{}", url);
    }

    let mut base_url = match url::Url::parse(&base) {
        Ok(u) => u,
        Err(_) => return url.to_string(),
    };
    base_url.set_fragment(None);

    match base_url.join(&url) {
        Ok(u) => u.to_string(),
        Err(_) => {
            let base = base.trim_end_matches('/');
            format!("{}/{}", base, url.trim_start_matches('/'))
        }
    }
}

pub(super) fn interpolate_json_templates(
    rule: &str,
    v: &Value,
    base_url: &str,
    ctx: &HashMap<String, String>,
) -> String {
    let re = &*TEMPLATE_JS_RE;
    re.replace_all(rule, |caps: &regex::Captures| {
        let expr = caps.get(1).map(|m| m.as_str().trim()).unwrap_or_default();
        if expr.is_empty() {
            return String::new();
        }

        if let Some(key) = expr
            .strip_prefix("@get:{")
            .and_then(|s| s.strip_suffix('}'))
        {
            return ctx.get(key.trim()).cloned().unwrap_or_default();
        }

        if expr.starts_with('$') {
            return pick_json_field(v, Some(expr)).unwrap_or_default();
        }

        if let Some(val) = ctx.get(expr) {
            return val.clone();
        }

        if let Some(val) = pick_json_field(v, Some(expr)) {
            return val;
        }

        // 上下文里的键（`page` / `sort` 等）也作为 JS 变量暴露，
        // 这样模板里可以做映射，例如 `{{sort === 'hot' ? 'Hot' : 'TimeDesc'}}`。
        let bindings: HashMap<String, Value> = ctx
            .iter()
            .map(|(key, value)| (key.clone(), Value::String(value.clone())))
            .collect();
        eval_js_with_bindings(
            expr,
            &serde_json::to_string(v).unwrap_or_default(),
            base_url,
            &bindings,
        )
        .unwrap_or_default()
    })
    .into_owned()
}

pub(super) fn interpolate_common_templates(
    rule: &str,
    input: &str,
    base_url: &str,
    ctx: &HashMap<String, String>,
) -> String {
    let get_re = &*TEMPLATE_GET_RE;
    let with_get = get_re.replace_all(rule, |caps: &regex::Captures| {
        let key = caps.get(1).map(|m| m.as_str().trim()).unwrap_or_default();
        ctx.get(key).cloned().unwrap_or_default()
    });

    let js_re = &*TEMPLATE_JS_RE;
    js_re
        .replace_all(&with_get, |caps: &regex::Captures| {
            let expr = caps.get(1).map(|m| m.as_str().trim()).unwrap_or_default();
            if expr.is_empty() {
                return String::new();
            }
            if let Some(val) = ctx.get(expr) {
                return val.clone();
            }
            eval_js(expr, input, base_url).unwrap_or_default()
        })
        .into_owned()
}

fn strip_url_config(url: &str) -> &str {
    if let Some(idx) = url.find("##$##") {
        &url[..idx]
    } else if let Some(idx) = url.find(",{'webView'") {
        &url[..idx]
    } else if let Some(idx) = url.find(",{\"webView\"") {
        &url[..idx]
    } else {
        url
    }
}

/// 拆出规则里的 JS 片段：`(前缀, JS, 后缀)`。
///
/// `@js:` 会吞掉后续整段规则（后缀为空）；`<js>...</js>` 允许前后拼接普通片段，
/// 后缀继续对 JS 结果求值——这是规格 §20 说的「链式规则必须用 `<js>...</js>`」。
pub(super) fn extract_js(rule: &str) -> (&str, Option<&str>, Option<&str>) {
    if let Some(idx) = rule.find("<js>") {
        if let Some(end_idx) = rule.rfind("</js>") {
            if end_idx > idx {
                let pure = rule[..idx].trim();
                let js = &rule[idx + 4..end_idx];
                let tail = rule[end_idx + 5..].trim();
                let tail = if tail.is_empty() { None } else { Some(tail) };
                return (pure, Some(js), tail);
            }
        }
    }
    if let Some(idx) = rule.find("@js:") {
        let pure = rule[..idx].trim();
        let js = &rule[idx + 4..];
        return (pure, Some(js), None);
    }
    (rule, None, None)
}

/// 把 `<js>` 之后的片段继续作用在 JS 结果上：结果是 JSON 就按 JSON 规则求值，
/// 否则按 HTML 文档规则求值（与列表/字段的自动识别一致）。
fn eval_rule_on_text(
    rule: &str,
    text: &str,
    base_url: &str,
    ctx: &mut HashMap<String, String>,
) -> Option<String> {
    let rule = rule.trim();
    if rule.is_empty() || text.trim().is_empty() {
        return None;
    }
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        return eval_field_json_with_ctx(rule, &value, base_url, ctx);
    }
    let doc = html::parse_document(text);
    let doc_view = DocView::new(&doc);
    eval_field_html_doc_with_ctx(rule, &doc_view, base_url, ctx)
}

/// `@regex:` 模式：不执行匹配，直接返回规则文本（规格 §6.8 的 Regex 分支）。
///
/// 只做模板插值、内联 JS 与 `##` 替换；`$n` 分组引用在没有上游捕获组时保持原文。
fn eval_literal_field(
    rule: &str,
    input: &str,
    base_url: &str,
    ctx: &mut HashMap<String, String>,
) -> Option<String> {
    let interpolated = interpolate_common_templates(rule, input, base_url, ctx);
    let (pure_rule, regex_part) = split_legado_regex(&interpolated);
    let (pure, js, tail) = extract_js(&pure_rule);

    let mut text = pure.to_string();
    if let Some(script) = js {
        if let Ok(res) = eval_js(script, &text, base_url) {
            text = res;
        }
    }
    if let Some(tail) = tail {
        if let Some(res) = eval_rule_on_text(tail, &text, base_url, ctx) {
            text = res;
        }
    }
    if let Some(reg) = regex_part {
        text = apply_legado_regex(&text, reg);
    }

    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

/// 字段级组合符求值：`&&` 拼接 / `||` 首个非空 / `%%` 交错。
///
/// 返回 `None` 表示规则里没有顶层组合符，调用方按单条规则继续处理。
/// 用 `FnMut` 是因为各条分支共享同一份 `@put`/`@get` 上下文。
fn combine_strings<F>(rule: &str, mut eval: F) -> Option<Vec<String>>
where
    F: FnMut(&str) -> Option<String>,
{
    let combo = crate::parser::rule_analyzer::split_top_level(rule, &["&&", "||", "%%"]);
    let operator = combo.delimiter.as_deref()?;

    let mut result: Vec<String> = eval(combo.parts.first().map(String::as_str).unwrap_or(""))
        .into_iter()
        .collect();
    for part in combo.parts.iter().skip(1) {
        let next: Vec<String> = eval(part).into_iter().collect();
        match operator {
            "&&" => result.extend(next),
            "||" => {
                if result.is_empty() {
                    result = next;
                }
            }
            "%%" => {
                let mut zipped = Vec::with_capacity(result.len() + next.len());
                let max_len = result.len().max(next.len());
                for idx in 0..max_len {
                    if idx < result.len() {
                        zipped.push(result[idx].clone());
                    }
                    if idx < next.len() {
                        zipped.push(next[idx].clone());
                    }
                }
                result = zipped;
            }
            _ => {}
        }
    }
    Some(result)
}

/// 多值字段（`kind`）的命中收集是否适用：规则是纯选择器/路径。
///
/// 含 JS、模板或 `@put`/`@get` 的规则无法用列表求值表达，交给单值求值兜底，
/// 由书源自行拼接多值。
fn is_plain_selector_rule(rule: &str) -> bool {
    let rule = rule.trim();
    !rule.is_empty()
        && !rule.contains("{{")
        && !rule.contains("<js>")
        && !rule.contains("@js:")
        && !rule.starts_with("js:")
        && !rule.starts_with("@put:")
        && !rule.starts_with("@get:")
}

/// 多值字段（`kind`）求值：全部命中去空后用 `,` 连接（规格 §13.4 / §14）。
fn join_multi_values(
    values: Vec<String>,
    fallback: impl FnOnce() -> Option<String>,
) -> Option<String> {
    let values: Vec<String> = values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    if values.is_empty() {
        fallback()
    } else {
        Some(values.join(","))
    }
}

/// JSON 规则的 `kind` 命中：`$.a` 走 JsonPath，裸字段名直接取值（数组展开）。
fn json_kind_values(v: &Value, rule: &str) -> Vec<String> {
    let rule = rule.trim();
    if rule.starts_with('$') {
        return jsonpath::jsonpath_query_combined(v, rule)
            .iter()
            .filter_map(jsonpath::value_to_string)
            .collect();
    }
    v.as_object()
        .and_then(|obj| obj.get(rule))
        .map(|value| match value {
            Value::Array(items) => items.iter().filter_map(jsonpath::value_to_string).collect(),
            other => jsonpath::value_to_string(other).into_iter().collect(),
        })
        .unwrap_or_default()
}

pub(super) fn eval_kind_json(rule: Option<&str>, v: &Value, base_url: &str) -> Option<String> {
    let rule = rule.unwrap_or("").trim();
    let values = if is_plain_selector_rule(rule) {
        json_kind_values(v, strip_mode_prefix(rule))
    } else {
        Vec::new()
    };
    join_multi_values(values, || eval_field_json(rule, v, base_url))
}

pub(super) fn eval_kind_xpath(
    rule: Option<&str>,
    node: sxd_xpath::nodeset::Node<'_>,
    base_url: &str,
) -> Option<String> {
    let rule = rule.unwrap_or("").trim();
    let values = if is_plain_selector_rule(rule) {
        xpath_eval_strings(node, strip_mode_prefix(rule))
    } else {
        Vec::new()
    };
    join_multi_values(values, || eval_field_xpath(rule, node, base_url))
}

pub(super) fn eval_kind_html_doc(
    rule: Option<&str>,
    doc: &scraper::Html,
    base_url: &str,
) -> Option<String> {
    let rule = rule.unwrap_or("").trim();
    let values = if is_plain_selector_rule(rule) {
        html::select_text_list(doc, strip_mode_prefix(rule))
    } else {
        Vec::new()
    };
    let doc_view = DocView::new(doc);
    join_multi_values(values, || {
        eval_field_html_doc_with_ctx(rule, &doc_view, base_url, &mut HashMap::new())
    })
}

pub(super) fn eval_kind_html_element(
    rule: Option<&str>,
    el: &scraper::ElementRef,
    base_url: &str,
) -> Option<String> {
    let rule = rule.unwrap_or("").trim();
    let values = if is_plain_selector_rule(rule) {
        html::select_text_list_from_element(el, strip_mode_prefix(rule))
    } else {
        Vec::new()
    };
    join_multi_values(values, || eval_field_html(rule, el, base_url))
}

pub(super) fn eval_field_html(
    rule: &str,
    el: &scraper::ElementRef,
    base_url: &str,
) -> Option<String> {
    eval_field_html_with_ctx(rule, el, base_url, &mut HashMap::new())
}

/// 字段规则求值的共享尾段：`##` 拆分 → JS 提取 → 取值 → JS/`<js>` 尾段链式 → `##` 替换
/// → 空则 None。html 元素级 / xpath / json 三个入口共用；差异由调用方注入：
///
/// - 插值在调用方完成（JSON 上下文走 `interpolate_json_templates`，其余走 common），
///   传入插值后的规则与 `had_templates`；
/// - `default_text`：`pure` 为空时的起始文本（xpath 取节点自身文本，其余为空串）；
/// - `select`：用 `pure` 选择器取起始文本（json 在内部附带「字面量启发式」）；
/// - `template_fallback`：插值后选择器落空时把 `pure` 当字面量返回
///   （json 有自己的字面量启发式，不走这条）。
///
/// 注意：文档级入口 `eval_field_html_doc_with_ctx` 是更精简的变体（无 `##` 拆分、
/// 无组合符），刻意不走这里——改动它会改变书籍详情字段的既有解析行为。
fn eval_field_tail(
    interpolated_rule: &str,
    had_templates: bool,
    base_url: &str,
    ctx: &mut HashMap<String, String>,
    template_fallback: bool,
    default_text: impl FnOnce() -> String,
    select: impl FnOnce(&str) -> String,
) -> Option<String> {
    let (pure_rule, regex_part) = split_legado_regex(interpolated_rule);
    let (pure, js, tail) = extract_js(&pure_rule);

    let mut text = if pure.trim().is_empty() {
        default_text()
    } else {
        select(pure)
    };
    if template_fallback && text.is_empty() && had_templates && !pure.trim().is_empty() {
        text = pure.to_string();
    }

    if let Some(script) = js {
        if let Ok(res) = eval_js(script, &text, base_url) {
            text = res;
        }
    }
    if let Some(tail) = tail {
        if let Some(res) = eval_rule_on_text(tail, &text, base_url, ctx) {
            text = res;
        }
    }

    if let Some(reg) = regex_part {
        text = apply_legado_regex(&text, reg);
    }

    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

pub(super) fn eval_field_html_with_ctx(
    rule: &str,
    el: &scraper::ElementRef,
    base_url: &str,
    ctx: &mut HashMap<String, String>,
) -> Option<String> {
    // Handle mode forcing prefixes
    let rule = rule.trim();
    if let Some(pure) = rule.strip_prefix("@css:") {
        return eval_field_html_with_ctx(pure, el, base_url, ctx);
    }
    if let Some(pure) = rule.strip_prefix("@xpath:") {
        // 元素级 XPath：把当前元素序列化后求值，取首个结果。
        return html::select_xpath(&el.html(), pure.trim())
            .into_iter()
            .next();
    }
    if let Some(pure) = rule.strip_prefix("@json:") {
        // 元素级 JSON：规则作用在元素自身的文本上（通常是上一步 JS 产出的 JSON）。
        let value: Value = serde_json::from_str(&el.text().collect::<Vec<_>>().join("")).ok()?;
        return eval_field_json_with_ctx(pure, &value, base_url, ctx);
    }
    if let Some(pure) = rule.strip_prefix("@regex:") {
        return eval_literal_field(pure, &el.text().collect::<Vec<_>>().join(""), base_url, ctx);
    }

    // 字段级组合符：逐条求值后合并（规格 §8.3）
    if let Some(texts) = combine_strings(rule, |part| {
        eval_field_html_with_ctx(part, el, base_url, ctx)
    }) {
        if texts.is_empty() {
            return None;
        }
        return Some(texts.join("\n"));
    }

    // Handle @put/@get
    if let Some(res) = try_put_get(rule, ctx, |r, ctx| {
        eval_field_html_with_ctx(r, el, base_url, ctx)
    }) {
        return Some(res);
    }

    let input = html::descendant_text(el);
    let interpolated_rule = interpolate_common_templates(rule, &input, base_url, ctx);
    let had_templates = interpolated_rule != rule;
    eval_field_tail(
        &interpolated_rule,
        had_templates,
        base_url,
        ctx,
        true,
        String::new,
        |pure| html::select_text_from_element(el, pure).unwrap_or_default(),
    )
}

/// 文档级字段求值（书籍详情等），与元素级入口同构：模式前缀、字段级组合符、
/// `@put`/`@get` 与共享尾段（`##` 拆分/替换、JS 链式）全部一致，只差取值原语
/// 作用在整份文档上。此前缺 `##`/组合符是漂移——含这些记号的文档级规则原本
/// 就是坏的（选择器非法恒落空），补齐只会修不会回归。
pub(super) fn eval_field_html_doc_with_ctx(
    rule: &str,
    doc_view: &DocView<'_>,
    base_url: &str,
    ctx: &mut HashMap<String, String>,
) -> Option<String> {
    // Handle mode forcing prefixes
    let rule = rule.trim();
    if let Some(pure) = rule.strip_prefix("@css:") {
        return eval_field_html_doc_with_ctx(pure, doc_view, base_url, ctx);
    }
    if let Some(pure) = rule.strip_prefix("@xpath:") {
        return html::select_xpath(doc_view.html(), pure.trim())
            .into_iter()
            .next();
    }
    if let Some(pure) = rule.strip_prefix("@json:") {
        // 文档级 JSON：规则作用在整份文档的文本上（通常是上一步 JS 产出的 JSON）
        let text = doc_view
            .doc()
            .root_element()
            .text()
            .collect::<Vec<_>>()
            .join("");
        let value: Value = serde_json::from_str(&text).ok()?;
        return eval_field_json_with_ctx(pure, &value, base_url, ctx);
    }
    if let Some(pure) = rule.strip_prefix("@regex:") {
        let text = doc_view
            .doc()
            .root_element()
            .text()
            .collect::<Vec<_>>()
            .join("");
        return eval_literal_field(pure, &text, base_url, ctx);
    }

    // 字段级组合符：逐条求值后合并（规格 §8.3）
    if let Some(texts) = combine_strings(rule, |part| {
        eval_field_html_doc_with_ctx(part, doc_view, base_url, ctx)
    }) {
        if texts.is_empty() {
            return None;
        }
        return Some(texts.join("\n"));
    }

    if let Some(res) = try_put_get(rule, ctx, |r, ctx| {
        eval_field_html_doc_with_ctx(r, doc_view, base_url, ctx)
    }) {
        return Some(res);
    }

    let interpolated_rule = interpolate_common_templates(rule, doc_view.html(), base_url, ctx);
    let had_templates = interpolated_rule != rule;
    eval_field_tail(
        &interpolated_rule,
        had_templates,
        base_url,
        ctx,
        true,
        String::new,
        |pure| html::select_text(doc_view.doc(), pure).unwrap_or_default(),
    )
}

pub(super) fn eval_field_json(rule: &str, v: &Value, base_url: &str) -> Option<String> {
    eval_field_json_with_ctx(rule, v, base_url, &mut HashMap::new())
}

pub(super) fn eval_field_xpath(
    rule: &str,
    node: sxd_xpath::nodeset::Node<'_>,
    base_url: &str,
) -> Option<String> {
    eval_field_xpath_with_ctx(rule, node, base_url, &mut HashMap::new())
}

pub(super) fn eval_field_xpath_with_ctx(
    rule: &str,
    node: sxd_xpath::nodeset::Node<'_>,
    base_url: &str,
    ctx: &mut HashMap<String, String>,
) -> Option<String> {
    let rule = rule.trim();
    if rule.is_empty() {
        return None;
    }
    if let Some(pure) = rule.strip_prefix("@xpath:") {
        return eval_field_xpath_with_ctx(pure, node, base_url, ctx);
    }
    if let Some(pure) = rule.strip_prefix("@json:") {
        let value: Value = serde_json::from_str(&node.string_value()).ok()?;
        return eval_field_json_with_ctx(pure, &value, base_url, ctx);
    }
    if rule.starts_with("@css:") {
        // XPath 上下文里没有 HTML 文档可查，显式拒绝而不是误当 XPath 求值
        return None;
    }
    if let Some(res) = try_put_get(rule, ctx, |r, ctx| {
        eval_field_xpath_with_ctx(r, node, base_url, ctx)
    }) {
        return Some(res);
    }

    let interpolated_rule = interpolate_common_templates(rule, &node.string_value(), base_url, ctx);
    let had_templates = interpolated_rule != rule;
    eval_field_tail(
        &interpolated_rule,
        had_templates,
        base_url,
        ctx,
        true,
        || node.string_value(),
        |pure| {
            xpath_eval_strings(node, pure)
                .into_iter()
                .next()
                .unwrap_or_default()
        },
    )
}

pub(super) fn select_xpath_scope<'a>(
    node: sxd_xpath::nodeset::Node<'a>,
    init_rule: Option<&str>,
) -> sxd_xpath::nodeset::Node<'a> {
    let Some(init_rule) = init_rule.map(str::trim).filter(|s| !s.is_empty()) else {
        return node;
    };
    xpath_select_nodes(node, init_rule)
        .into_iter()
        .next()
        .unwrap_or(node)
}

pub(super) fn xpath_select_nodes<'a>(
    node: sxd_xpath::nodeset::Node<'a>,
    xpath: &str,
) -> Vec<sxd_xpath::nodeset::Node<'a>> {
    let xpath = xpath.trim();
    if xpath.is_empty() || !html::xpath_within_limits(xpath) {
        return vec![];
    }
    // 组合规则（顶层切分）：&& 拼接 / || 首个非空 / %% 交错
    let combo = crate::parser::rule_analyzer::split_top_level(xpath, &["&&", "||", "%%"]);
    if let Some(op) = combo.delimiter.as_deref() {
        let mut result =
            xpath_select_nodes(node, combo.parts.first().map(String::as_str).unwrap_or(""));
        for part in combo.parts.iter().skip(1) {
            let next = xpath_select_nodes(node, part);
            match op {
                "&&" => result.extend(next),
                "||" => {
                    if result.is_empty() {
                        result = next;
                    }
                }
                "%%" => {
                    let mut zipped = Vec::new();
                    let max_len = result.len().max(next.len());
                    for idx in 0..max_len {
                        if idx < result.len() {
                            zipped.push(result[idx]);
                        }
                        if idx < next.len() {
                            zipped.push(next[idx]);
                        }
                    }
                    result = zipped;
                }
                _ => {}
            }
        }
        return result;
    }
    let context = XPathContext::new();
    match html::compile_xpath(xpath) {
        Some(expr) => match expr.evaluate(&context, node) {
            Ok(XPathValue::Nodeset(ns)) => ns.document_order(),
            _ => vec![],
        },
        _ => vec![],
    }
}

pub(super) fn xpath_eval_strings(node: sxd_xpath::nodeset::Node<'_>, xpath: &str) -> Vec<String> {
    let xpath = xpath.trim();
    if xpath.is_empty() || !html::xpath_within_limits(xpath) {
        return vec![];
    }
    // 组合规则（顶层切分）：&& 拼接 / || 首个非空 / %% 交错
    let combo = crate::parser::rule_analyzer::split_top_level(xpath, &["&&", "||", "%%"]);
    if let Some(op) = combo.delimiter.as_deref() {
        return html::combine_string_parts(op, &combo.parts, |part| xpath_eval_strings(node, part));
    }
    let context = XPathContext::new();
    match html::compile_xpath(xpath) {
        Some(expr) => match expr.evaluate(&context, node) {
            Ok(XPathValue::Nodeset(ns)) => ns
                .document_order()
                .into_iter()
                .map(|n| n.string_value())
                .collect(),
            Ok(XPathValue::String(s)) => vec![s],
            Ok(XPathValue::Number(n)) => vec![n.to_string()],
            Ok(XPathValue::Boolean(b)) => vec![b.to_string()],
            Err(_) => vec![],
        },
        _ => vec![],
    }
}

pub(super) fn eval_field_json_with_ctx(
    rule: &str,
    v: &Value,
    base_url: &str,
    ctx: &mut HashMap<String, String>,
) -> Option<String> {
    let rule = rule.trim();
    if let Some(pure) = rule.strip_prefix("@json:") {
        return eval_field_json_with_ctx(pure, v, base_url, ctx);
    }
    if rule.starts_with("@xpath:") || rule.starts_with("@css:") {
        // JSON 上下文里没有 HTML 文档或 XML 节点可查，显式拒绝而不是误当 JsonPath 求值
        return None;
    }

    // 字段级组合符：JSON 规则里的 `&`/`,` 会命中下面的「字面量」启发式，必须先切分
    if let Some(texts) = combine_strings(rule, |part| {
        eval_field_json_with_ctx(part, v, base_url, ctx)
    }) {
        if texts.is_empty() {
            return None;
        }
        return Some(texts.join("\n"));
    }

    if let Some(res) = try_put_get(rule, ctx, |r, ctx| {
        eval_field_json_with_ctx(r, v, base_url, ctx)
    }) {
        return Some(res);
    }

    let interpolated_rule = interpolate_json_templates(rule, v, base_url, ctx);
    eval_field_tail(
        &interpolated_rule,
        false,
        base_url,
        ctx,
        false,
        String::new,
        |pure| {
            // `$` 开头且是合法 JsonPath 的规则优先按 JsonPath 求值：
            // 过滤表达式（$.data[?(@.x==1)].title）与多下标（$.a[0,1]）
            // 含 `?`/`=`/`,` 等字符，此前被下面的字面量启发式吞成规则文本本身。
            // 多命中与字段级组合符同口径：按 value_to_string 逐个转换后用 \n 连接
            let trimmed = pure.trim();
            if trimmed.starts_with('$') && jsonpath::jsonpath_parses(trimmed) {
                return jsonpath::jsonpath_query(v, trimmed)
                    .iter()
                    .filter_map(jsonpath::value_to_string)
                    .collect::<Vec<_>>()
                    .join("\n");
            }
            // 「字面量启发式」：插值后仍含模板痕迹或带 URL/列表特征的规则按字面量返回
            if (pure.contains("{{") && pure.contains("}}"))
                || pure.contains('/')
                || pure.contains('?')
                || pure.contains('&')
                || pure.contains('=')
                || pure.contains(',')
            {
                pure.to_string()
            } else {
                pick_json_field(v, Some(pure)).unwrap_or_default()
            }
        },
    )
}

/// 去掉值规则两端成对的引号（只去一层，不误伤值内部或末尾的引号）。
fn unquote_pair(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

/// 拆分 `@put:{key:rule, ...}` 的内容。
///
/// 顶层的 `,` 与 `:` 才作分隔符，引号/括号内的逗号冒号保持原样，
/// 因此值规则里可以出现逗号（如 `@put:{k:$.a[0,1]}`）与带引号的 URL。
fn split_put_map(inner: &str) -> Vec<(String, String)> {
    let mut entries = Vec::new();
    for part in crate::parser::rule_analyzer::split_top_level(inner, &[","]).parts {
        let split = crate::parser::rule_analyzer::split_top_level(&part, &[":"]);
        let Some(key) = split.parts.first().map(|key| key.trim()) else {
            continue;
        };
        if key.is_empty() || split.parts.len() < 2 {
            continue;
        }
        let joined = split.parts[1..].join(":");
        let value = unquote_pair(joined.trim()).to_string();
        entries.push((key.to_string(), value));
    }
    entries
}

/// `@put:{key: rule, ...}` 把各值规则的结果写入上下文字段表，`@get:{key}` 读回。
/// 四种上下文（html 元素/文档、xpath、json）共用；`eval` 是所属上下文的字段求值入口。
fn try_put_get(
    rule: &str,
    ctx: &mut HashMap<String, String>,
    mut eval: impl FnMut(&str, &mut HashMap<String, String>) -> Option<String>,
) -> Option<String> {
    if let Some(content) = rule.strip_prefix("@put:") {
        let content = content.trim();
        if content.starts_with('{') && content.ends_with('}') {
            for (key, val_rule) in split_put_map(&content[1..content.len() - 1]) {
                let val = eval(&val_rule, ctx).unwrap_or_default();
                ctx.insert(key, val);
            }
        }
        return Some(String::new());
    }
    if let Some(content) = rule.strip_prefix("@get:") {
        let content = content.trim();
        if content.starts_with('{') && content.ends_with('}') {
            let key = content[1..content.len() - 1].trim();
            return ctx.get(key).cloned();
        }
    }
    None
}

pub(super) fn split_legado_regex(rule: &str) -> (String, Option<&str>) {
    if let Some(idx) = rule.find("##") {
        let (pure, reg) = rule.split_at(idx);
        return (pure.trim().to_string(), Some(reg));
    }
    (rule.to_string(), None)
}

/// `##` 替换（规格 §6.7）：形如 `##regex[##replacement[##]]`。
///
/// 第四段存在即 replaceFirst：取**首个匹配片段**、在该片段上替换一次并返回该片段
/// （不是替换整串里的第一处；无匹配返回空串，正则不可编译时返回 replacement）。
/// 缺少 replacement 时按空串处理，也就是删除匹配内容——`##regex` 这种写法很常见。
pub(super) fn apply_legado_regex(text: &str, regex_part: &str) -> String {
    let trimmed = regex_part.trim();
    let Some(rest) = trimmed.strip_prefix("##") else {
        return text.to_string();
    };

    let parts: Vec<&str> = rest.split("##").collect();
    let pattern = parts.first().copied().unwrap_or("").trim();
    if pattern.is_empty() {
        return text.to_string();
    }
    let replacement = parts.get(1).copied().unwrap_or("");
    let replace_first = parts.len() > 2;

    let Some(re) = crate::util::text::compiled_regex(pattern) else {
        // 正则不可编译：第四段时按规格返回 replacement，否则退化为普通字符串替换
        if replace_first {
            return replacement.to_string();
        }
        return text.replace(pattern, replacement);
    };

    if !replace_first {
        return re.replace_all(text, replacement).to_string();
    }

    match re.find(text) {
        Some(matched) => re.replace(matched.as_str(), replacement).to_string(),
        None => String::new(),
    }
}

pub(super) fn normalize_list_rule(rule: &str) -> (&str, bool) {
    let rule = rule.trim();
    if let Some(rest) = rule.strip_prefix('-') {
        return (rest.trim(), true);
    }
    if let Some(rest) = rule.strip_prefix('+') {
        return (rest.trim(), false);
    }
    (rule, false)
}

pub(super) fn strip_mode_prefix(rule: &str) -> &str {
    let rule = rule.trim();
    if let Some(rest) = rule
        .strip_prefix("<js>")
        .and_then(|s| s.strip_suffix("</js>"))
    {
        return rest;
    }
    for prefix in [
        "@css:", "@CSS:", "@xpath:", "@XPath:", "@XPATH:", "@json:", "@Json:", "@JSON:", "@regex:",
        "@Regex:", "@js:", "js:",
    ] {
        if let Some(rest) = rule.strip_prefix(prefix) {
            return rest;
        }
    }
    rule
}

pub(super) fn strip_js_rule(rule: &str) -> &str {
    let rule = rule.trim();
    if let Some(rest) = rule
        .strip_prefix("<js>")
        .and_then(|s| s.strip_suffix("</js>"))
    {
        return rest;
    }
    if let Some(rest) = rule.strip_prefix("@js:") {
        return rest;
    }
    if let Some(rest) = rule.strip_prefix("js:") {
        return rest;
    }
    rule
}

/// 列表正则单段允许收集的最大完整匹配数（中间段与末段各自适用）。
///
/// 列表正则逐级下钻时每段把**全部**完整匹配串收集为下一段输入；32MB 响应体
/// 上 `:\d` 这类规则可产出数千万匹配，末段再给每个匹配分配一个捕获组 Vec，
/// 时间与内存都无界。10 万匹配已远超任何真实目录的规模，超限截断即可。
const MAX_REGEX_MATCHES_PER_STAGE: usize = 100_000;

/// 末段捕获组总产出上限（组数 × 匹配数），防止「少量匹配 × 海量捕获组」。
const MAX_REGEX_CAPTURES_TOTAL: usize = 200_000;

/// 列表正则求值（规格 §9.3）：`&&` 分段逐级下钻，前一段的所有完整匹配串拼成文本喂给下一段；
/// 最后一段的每个匹配产出一组捕获组（含 group 0），供字段规则的 `$n` 引用——
/// 但 `$0` 按 Legado 兼容保留字面量，只有 `$1` 起才替换（见 `capture_rule_value`）。
pub(super) fn regex_list_captures(pattern: &str, body: &str) -> Vec<Vec<String>> {
    regex_list_captures_with_limits(
        pattern,
        body,
        MAX_REGEX_MATCHES_PER_STAGE,
        MAX_REGEX_CAPTURES_TOTAL,
    )
}

/// 带显式上限的列表正则求值（公开包装用常量；小上限便于测试截断行为）。
fn regex_list_captures_with_limits(
    pattern: &str,
    body: &str,
    stage_limit: usize,
    total_limit: usize,
) -> Vec<Vec<String>> {
    let split = crate::parser::rule_analyzer::split_top_level(pattern, &["&&"]);
    let stages: Vec<&str> = split.parts.iter().map(|part| part.trim()).collect();
    if stages.is_empty() || stages.iter().all(|stage| stage.is_empty()) {
        return vec![];
    }

    let mut texts = vec![body.to_string()];
    let mut total_captures = 0usize;
    for (index, stage) in stages.iter().enumerate() {
        let Some(re) = crate::util::text::compiled_regex(stage) else {
            return vec![];
        };
        let last = index + 1 == stages.len();
        let mut groups_out = Vec::new();
        let mut next_texts = Vec::new();
        let mut stage_truncated = false;
        'outer: for text in &texts {
            for captures in re.captures_iter(text) {
                if last {
                    if groups_out.len() >= stage_limit || total_captures >= total_limit {
                        stage_truncated = true;
                        break 'outer;
                    }
                    let group_count = captures.len();
                    groups_out.push(
                        (0..group_count)
                            .map(|i| {
                                captures
                                    .get(i)
                                    .map(|m| m.as_str().to_string())
                                    .unwrap_or_default()
                            })
                            .collect(),
                    );
                    total_captures += group_count;
                } else {
                    if next_texts.len() >= stage_limit {
                        stage_truncated = true;
                        break 'outer;
                    }
                    next_texts.push(
                        captures
                            .get(0)
                            .map(|m| m.as_str().to_string())
                            .unwrap_or_default(),
                    );
                }
            }
        }
        if stage_truncated {
            let preview: String = pattern.chars().take(120).collect();
            tracing::warn!(pattern = %preview, stage = index, "列表正则命中数超过上限，已截断");
        }
        if last {
            return groups_out;
        }
        if next_texts.is_empty() {
            return vec![];
        }
        texts = next_texts;
    }
    vec![]
}

pub(super) fn capture_rule_value(rule: Option<&str>, groups: &[String]) -> Option<String> {
    let rule = rule?.trim();
    if rule.is_empty() {
        return None;
    }
    let placeholder = &*REGEX_PLACEHOLDER_RE;
    let replaced = placeholder.replace_all(rule, |cap: &regex::Captures| {
        let index = cap
            .get(1)
            .and_then(|m| m.as_str().parse::<usize>().ok())
            .unwrap_or(0);
        if index == 0 {
            return cap
                .get(0)
                .map(|m| m.as_str())
                .unwrap_or_default()
                .to_string();
        }
        groups.get(index).cloned().unwrap_or_else(|| {
            cap.get(0)
                .map(|m| m.as_str())
                .unwrap_or_default()
                .to_string()
        })
    });
    let (pure, regex_part) = split_legado_regex(&replaced);
    let mut output = pure;
    if let Some(regex_part) = regex_part {
        output = apply_legado_regex(&output, regex_part);
    }
    if output.is_empty() {
        None
    } else {
        Some(output)
    }
}

/// JS 全局变量的数值读取：QuickJS 的自增/浮点结果表示为浮点，整数结果也要接受。
pub(super) fn global_as_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_f64().map(|number| number as i64))
}

pub(super) fn is_truthy(value: String) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return false;
    }
    // 规格 §15 的词表是 `false|no|not|0`（外加 `"null"`）；`none`/`off` 是本项目的超集
    !matches!(
        value.to_ascii_lowercase().as_str(),
        "0" | "false" | "null" | "none" | "no" | "not" | "off"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apply_legado_regex() {
        let text = "Hello World 123 456";

        // 全局替换
        let result = apply_legado_regex(text, "##\\d+##NUM");
        assert_eq!(result, "Hello World NUM NUM");

        // 缺少 replacement 时删除匹配内容
        let result = apply_legado_regex(text, "##\\d+");
        assert_eq!(result, "Hello World  ");

        // 第四段 = replaceFirst：只取首个匹配片段并在该片段上替换
        let result = apply_legado_regex(text, "##\\d+##NUM##");
        assert_eq!(result, "NUM");

        // 正则可编译但无匹配时返回空串
        let result = apply_legado_regex(text, "##zzz##NUM##");
        assert_eq!(result, "");
    }

    #[test]
    fn test_json_field_literal_heuristic_still_returns_urls_verbatim() {
        // 非 JsonPath 的字面量（URL、带查询串）必须原样返回，不受新分支影响
        let v: Value = serde_json::from_str(r#"{"data":{"list":[]}}"#).unwrap();
        assert_eq!(
            eval_field_json("http://example.com?x=1&y=2", &v, ""),
            Some("http://example.com?x=1&y=2".to_string())
        );
        assert_eq!(
            eval_field_json("a/b/c,d", &v, ""),
            Some("a/b/c,d".to_string())
        );
        // `$` 开头但不是合法 JsonPath（且带 URL 特征字符）时仍走字面量启发式
        assert_eq!(
            eval_field_json("$.a[?invalid", &v, ""),
            Some("$.a[?invalid".to_string())
        );
    }

    #[test]
    fn test_regex_list_captures_truncated_at_limits() {
        // 30 个匹配、单匹配单捕获组（group 0）：末段截断到 stage_limit=10
        // （调用方已剥掉 `:` 前缀，这里直接给纯正则）
        let body: String = "x".repeat(30);
        let capped = regex_list_captures_with_limits("x", &body, 10, 200);
        assert_eq!(capped.len(), 10, "末段命中数应截断到 stage_limit");

        // 中间段截断：第一段 8 个完整匹配喂给第二段，stage_limit=3 只保留 3 个输入
        let list_body = "<l>a</l><l>b</l><l>c</l><l>d</l><l>e</l><l>f</l><l>g</l><l>h</l>";
        let drilled = regex_list_captures_with_limits("<l>(.*?)</l>&&(\\w)", list_body, 3, 200);
        assert_eq!(drilled.len(), 3, "中间段命中数应截断到 stage_limit");

        // 捕获组总量上限：每匹配产出 group 0 + 2 个捕获组 = 3 个字符串，
        // total_limit=5 → 2 个匹配（共 6 个字符串）后截断
        let two_group_body = "ab".repeat(10);
        let total_capped = regex_list_captures_with_limits("(\\w)(\\w)", &two_group_body, 100, 5);
        assert_eq!(total_capped.len(), 2);
        assert!(total_capped.iter().all(|g| g.len() == 3));

        // 正常规则结果不变：上限远大于命中数时原样返回
        let body2 = "<ul><li><a href=\"/1\">One</a></li><li><a href=\"/2\">Two</a></li></ul>";
        let full = regex_list_captures_with_limits(
            r#"(?is)<li>(.*?)</li>&&<a href="([^"]+)">([^<]+)</a>"#,
            body2,
            MAX_REGEX_MATCHES_PER_STAGE,
            MAX_REGEX_CAPTURES_TOTAL,
        );
        assert_eq!(full.len(), 2);
        assert_eq!(full[0][2], "One");
        assert_eq!(full[1][2], "Two");
    }

    #[test]
    fn test_split_put_map_keeps_commas_and_colons() {
        let entries = split_put_map(r#"a:$.x[0,1], b:"http://example.com/p", c:js:return 'x,y'"#);

        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0], ("a".to_string(), "$.x[0,1]".to_string()));
        assert_eq!(
            entries[1],
            ("b".to_string(), "http://example.com/p".to_string())
        );
        assert_eq!(entries[2], ("c".to_string(), "js:return 'x,y'".to_string()));
    }

    #[test]
    fn test_is_truthy_follows_spec_word_list() {
        for falsy in [
            "", "   ", "0", "false", "FALSE", "no", "not", "null", "none", "off",
        ] {
            assert!(!is_truthy(falsy.to_string()), "{falsy:?} 应为假");
        }
        for truthy in ["1", "true", "yes", "是"] {
            assert!(is_truthy(truthy.to_string()), "{truthy:?} 应为真");
        }
    }
}
