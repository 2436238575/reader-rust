use once_cell::sync::Lazy;
use scraper::{ElementRef, Html, Selector};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use crate::parser::rule_analyzer::split_top_level;

/// CSS 选择器编译缓存：同一条规则对每个元素每个字段都会命中，
/// `Selector::parse` 不便宜（千章目录页此前要重复编译数万次）。
/// 键为选择器文本，解析失败也缓存（`None`）；满 1024 条整表清空，
/// 与 `util::text` 的 REGEX_CACHE 同策略。
static CSS_SELECTOR_CACHE: Lazy<Mutex<HashMap<String, Option<Selector>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

const CSS_SELECTOR_CACHE_MAX: usize = 1024;

fn cached_selector(css: &str) -> Option<Selector> {
    let mut cache = CSS_SELECTOR_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(parsed) = cache.get(css) {
        return parsed.clone();
    }
    let parsed = Selector::parse(css).ok();
    if cache.len() >= CSS_SELECTOR_CACHE_MAX {
        cache.clear();
    }
    cache.insert(css.to_string(), parsed.clone());
    parsed
}

/// `*` 全选器：按文本筛选的兜底路径用，固定编译一次。
static ALL_ELEMENTS: Lazy<Selector> = Lazy::new(|| Selector::parse("*").unwrap());

#[derive(Clone, Debug, PartialEq)]
enum SelectorBase {
    Css(String),
    Children,
    Text(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IndexMode {
    Select,
    Exclude,
}

#[derive(Clone, Debug, PartialEq)]
enum IndexItem {
    Single(i32),
    Range {
        start: Option<i32>,
        end: Option<i32>,
        step: i32,
    },
}

#[derive(Clone, Debug, PartialEq)]
struct ParsedSelector {
    base: SelectorBase,
    explicit_index: bool,
    index_mode: IndexMode,
    index_items: Vec<IndexItem>,
}

pub fn parse_document(html: &str) -> Html {
    Html::parse_document(html)
}

/// Convert Legado selector format to CSS selector
/// Legado formats:
/// - class.xxx yyy zzz → .xxx.yyy.zzz (multiple classes on one element)
/// - class.xxx or .xxx → .xxx
/// - tag.xxx → xxx (tag name)
/// - id.xxx or #xxx → #xxx
/// - tag.xxx@tag.yyy → nested selectors (split by @)
fn legado_to_css(selector: &str) -> String {
    let selector = selector.trim();

    // Handle special "class." prefix - multiple classes separated by space
    if selector.starts_with("class.") {
        let rest = &selector[6..];
        let classes: Vec<&str> = rest.split_whitespace().collect();
        if classes.len() > 1 {
            return format!(".{}", classes.join("."));
        } else {
            return format!(".{}", rest.trim());
        }
    }

    // Handle id. prefix
    if selector.starts_with("id.") {
        return format!("#{}", &selector[3..]);
    }

    // Handle tag. prefix
    if selector.starts_with("tag.") {
        return selector[4..].to_string();
    }

    // Already CSS selector format (index will be stripped in parse_selector_with_index)
    if selector.starts_with('.') {
        return selector.to_string();
    }

    if selector.starts_with('#') || selector.starts_with('[') {
        return selector.to_string();
    }

    // Default: treat as class if it doesn't look like a tag
    if selector
        .chars()
        .next()
        .map(|c| c.is_alphabetic())
        .unwrap_or(false)
    {
        let parts: Vec<&str> = selector.split_whitespace().collect();
        if parts.len() > 1 {
            return format!(".{}", parts.join("."));
        }
        return selector.to_string();
    }

    selector.to_string()
}

fn parse_selector_with_index(selector: &str) -> ParsedSelector {
    let selector = selector.trim();

    if let Some((base, index_mode, index_items)) = parse_bracket_index_spec(selector) {
        return ParsedSelector {
            base: parse_selector_base(base),
            explicit_index: true,
            index_mode,
            index_items,
        };
    }

    if let Some((base, index_mode, index_items)) = parse_legacy_index_spec(selector) {
        return ParsedSelector {
            base: parse_selector_base(base),
            explicit_index: true,
            index_mode,
            index_items,
        };
    }

    ParsedSelector {
        base: parse_selector_base(selector),
        explicit_index: false,
        index_mode: IndexMode::Select,
        index_items: Vec::new(),
    }
}

fn parse_selector_base(selector: &str) -> SelectorBase {
    let selector = selector.trim();
    if selector.is_empty() || selector == "children" {
        return SelectorBase::Children;
    }
    if let Some(text) = selector.strip_prefix("text.") {
        return SelectorBase::Text(text.trim().to_string());
    }
    SelectorBase::Css(legado_to_css(selector))
}

fn parse_bracket_index_spec(selector: &str) -> Option<(&str, IndexMode, Vec<IndexItem>)> {
    let selector = selector.trim();
    if !selector.ends_with(']') {
        return None;
    }
    let start = selector.rfind('[')?;
    let base = selector[..start].trim();
    let mut inner = selector[start + 1..selector.len() - 1].trim();
    let index_mode = if let Some(rest) = inner.strip_prefix('!') {
        inner = rest.trim();
        IndexMode::Exclude
    } else {
        IndexMode::Select
    };

    let mut items = Vec::new();
    if !inner.is_empty() {
        for part in inner.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            if let Some(item) = parse_bracket_index_item(part) {
                items.push(item);
            } else {
                return None;
            }
        }
    }

    Some((base, index_mode, items))
}

fn parse_bracket_index_item(part: &str) -> Option<IndexItem> {
    if part.contains(':') {
        let segments: Vec<&str> = part.split(':').collect();
        if segments.len() < 2 || segments.len() > 3 {
            return None;
        }
        let start = parse_optional_i32(segments[0])?;
        let end = parse_optional_i32(segments[1])?;
        let step = match segments.get(2) {
            Some(value) => parse_optional_i32(value)?.unwrap_or(1),
            None => 1,
        };
        return Some(IndexItem::Range { start, end, step });
    }

    Some(IndexItem::Single(part.parse().ok()?))
}

fn parse_optional_i32(part: &str) -> Option<Option<i32>> {
    let part = part.trim();
    if part.is_empty() {
        return Some(None);
    }
    Some(Some(part.parse().ok()?))
}

fn parse_legacy_index_spec(selector: &str) -> Option<(&str, IndexMode, Vec<IndexItem>)> {
    for (delimiter, index_mode) in [('!', IndexMode::Exclude), ('.', IndexMode::Select)] {
        let Some(pos) = selector.rfind(delimiter) else {
            continue;
        };
        let base = selector[..pos].trim();
        let tail = selector[pos + 1..].trim();
        if tail.is_empty() {
            continue;
        }

        // 旧写法用 `:` 分隔，且 `:` 支持区间与步长（规格 §8.5）：`!0:2`、`.1:5:2`
        let mut items = Vec::new();
        for part in tail.split(',') {
            let part = part.trim();
            if part.is_empty() {
                return None;
            }
            items.push(parse_legacy_index_item(part)?);
        }
        return Some((base, index_mode, items));
    }
    None
}

/// 旧式索引项：单个下标，或 `start:end[:step]` 区间（省略端点表示首/末项）。
fn parse_legacy_index_item(part: &str) -> Option<IndexItem> {
    if !part.contains(':') {
        return Some(IndexItem::Single(part.parse().ok()?));
    }

    let segments: Vec<&str> = part.split(':').collect();
    if segments.len() < 2 || segments.len() > 3 {
        return None;
    }
    let start = parse_optional_i32(segments[0])?;
    let end = parse_optional_i32(segments[1])?;
    let step = match segments.get(2) {
        Some(value) => parse_optional_i32(value)?.unwrap_or(1),
        None => 1,
    };
    Some(IndexItem::Range { start, end, step })
}

fn collect_matches<'a>(doc: &'a Html, selector: &ParsedSelector) -> Vec<ElementRef<'a>> {
    let matches = match &selector.base {
        SelectorBase::Css(css_selector) => select_css(doc, css_selector),
        SelectorBase::Children => Vec::new(),
        SelectorBase::Text(text) => select_by_text_doc(doc, text),
    };
    apply_indices(matches, selector)
}

fn collect_matches_from_element<'a>(
    el: ElementRef<'a>,
    selector: &ParsedSelector,
) -> Vec<ElementRef<'a>> {
    let matches = match &selector.base {
        SelectorBase::Css(css_selector) => select_css_from_element(el, css_selector),
        SelectorBase::Children => child_elements(el),
        SelectorBase::Text(text) => select_by_text_from_element(el, text),
    };
    apply_indices(matches, selector)
}

fn select_css<'a>(doc: &'a Html, css_selector: &str) -> Vec<ElementRef<'a>> {
    let sel = match cached_selector(css_selector) {
        Some(s) => s,
        None => return vec![],
    };
    doc.select(&sel).collect()
}

fn select_css_from_element<'a>(el: ElementRef<'a>, css_selector: &str) -> Vec<ElementRef<'a>> {
    let sel = match cached_selector(css_selector) {
        Some(s) => s,
        None => return vec![],
    };
    el.select(&sel).collect()
}

fn child_elements<'a>(el: ElementRef<'a>) -> Vec<ElementRef<'a>> {
    el.children().filter_map(ElementRef::wrap).collect()
}

fn select_by_text_doc<'a>(doc: &'a Html, needle: &str) -> Vec<ElementRef<'a>> {
    doc.select(&ALL_ELEMENTS)
        .filter(|el| own_text(el).contains(needle))
        .collect()
}

fn select_by_text_from_element<'a>(el: ElementRef<'a>, needle: &str) -> Vec<ElementRef<'a>> {
    let mut matches = Vec::new();
    if own_text(&el).contains(needle) {
        matches.push(el);
    }
    matches.extend(
        el.select(&ALL_ELEMENTS)
            .filter(|candidate| own_text(candidate).contains(needle)),
    );
    matches
}

fn own_text(el: &ElementRef) -> String {
    let mut text = String::new();
    for node in el.children() {
        if let Some(text_node) = node.value().as_text() {
            text.push_str(text_node.text.trim());
        }
    }
    text
}

fn apply_indices<'a>(
    matches: Vec<ElementRef<'a>>,
    selector: &ParsedSelector,
) -> Vec<ElementRef<'a>> {
    if !selector.explicit_index {
        return matches;
    }

    let resolved = resolve_indices(matches.len(), &selector.index_items);
    if selector.index_mode == IndexMode::Exclude {
        let exclude_set: HashSet<usize> = resolved.into_iter().collect();
        return matches
            .into_iter()
            .enumerate()
            .filter(|(idx, _)| !exclude_set.contains(idx))
            .map(|(_, el)| el)
            .collect();
    }

    resolved
        .into_iter()
        .filter_map(|idx| matches.get(idx).copied())
        .collect()
}

fn resolve_indices(len: usize, items: &[IndexItem]) -> Vec<usize> {
    if len == 0 {
        return Vec::new();
    }

    let mut seen = HashSet::new();
    let mut resolved = Vec::new();
    for item in items {
        for idx in expand_index_item(len, item) {
            if seen.insert(idx) {
                resolved.push(idx);
            }
        }
    }
    resolved
}

fn expand_index_item(len: usize, item: &IndexItem) -> Vec<usize> {
    match item {
        IndexItem::Single(idx) => normalize_index(*idx, len).into_iter().collect(),
        IndexItem::Range { start, end, step } => expand_range(len, *start, *end, *step),
    }
}

fn normalize_index(index: i32, len: usize) -> Option<usize> {
    let len_i32 = len as i32;
    let resolved = if index < 0 { len_i32 + index } else { index };
    if resolved < 0 || resolved >= len_i32 {
        return None;
    }
    Some(resolved as usize)
}

fn expand_range(len: usize, start: Option<i32>, end: Option<i32>, step: i32) -> Vec<usize> {
    if len == 0 {
        return Vec::new();
    }

    let len_i32 = len as i32;
    let mut start = start.unwrap_or(0);
    let mut end = end.unwrap_or(len_i32 - 1);

    if start < 0 {
        start += len_i32;
    }
    if end < 0 {
        end += len_i32;
    }

    if (start < 0 && end < 0) || (start >= len_i32 && end >= len_i32) {
        return Vec::new();
    }

    start = start.clamp(0, len_i32 - 1);
    end = end.clamp(0, len_i32 - 1);

    let step = if step > 0 {
        step as usize
    } else if -step < len_i32 {
        (step + len_i32).max(1) as usize
    } else {
        1
    };

    let mut indices = Vec::new();
    if start <= end {
        let mut current = start as usize;
        let end = end as usize;
        while current <= end {
            indices.push(current);
            current = match current.checked_add(step) {
                Some(next) => next,
                None => break,
            };
        }
    } else {
        let mut current = start as usize;
        let end = end as usize;
        loop {
            indices.push(current);
            if current <= end || current < step {
                break;
            }
            current -= step;
        }
    }

    indices
}

/// Select elements with Legado rule syntax
pub fn select_list<'a>(doc: &'a Html, selector: &str) -> Vec<ElementRef<'a>> {
    let selector = selector.trim();

    // Split by @ first to get the base selector (handle @@ separately in text extraction)
    let sel_text = selector.split("@@").next().unwrap_or(selector).trim();
    let split = split_top_level(sel_text, &["@"]);
    let sel_text = split
        .parts
        .first()
        .map(String::as_str)
        .unwrap_or(sel_text)
        .trim();

    // Handle list combination operators at the top level
    if sel_text.contains("&&") || sel_text.contains("||") || sel_text.contains("%%") {
        return select_with_combination(doc, sel_text);
    }

    collect_matches(doc, &parse_selector_with_index(sel_text))
}

/// Handle list combination operators
fn select_with_combination<'a>(doc: &'a Html, rule: &str) -> Vec<ElementRef<'a>> {
    let split = split_top_level(rule, &["&&", "||", "%%"]);
    let rules = split.parts;

    if rules.is_empty() {
        return vec![];
    }

    let mut result = select_list_simple(doc, &rules[0]);
    let operator = split.delimiter.as_deref().unwrap_or("");

    for next_rule in rules.iter().skip(1) {
        let next_results = select_list_simple(doc, next_rule);

        match operator {
            "&&" => {
                result.extend(next_results);
            }
            "||" => {
                if result.is_empty() {
                    result = next_results;
                }
            }
            "%%" => {
                let mut zipped = Vec::new();
                let max_len = result.len().max(next_results.len());
                for j in 0..max_len {
                    if j < result.len() {
                        zipped.push(result[j]);
                    }
                    if j < next_results.len() {
                        zipped.push(next_results[j]);
                    }
                }
                result = zipped;
            }
            _ => {}
        }
    }

    result
}

/// Simple select without combination operators
fn select_list_simple<'a>(doc: &'a Html, selector: &str) -> Vec<ElementRef<'a>> {
    collect_matches(doc, &parse_selector_with_index(selector))
}

/// Extract text from element with various Legado extractors
pub fn extract_text(el: &ElementRef, extractor: &str) -> Option<String> {
    let extractor = extractor.trim();

    match extractor {
        "text" | "@text" => {
            let text = el.text().collect::<Vec<_>>().join(" ");
            let text = text.trim().to_string();
            if text.is_empty() {
                None
            } else {
                Some(text)
            }
        }
        "textNodes" | "@textNodes" => {
            // 规格 §8.4：只取**直接**文本节点，trim 后按 `\n` 连接
            let text = direct_text_nodes(el);
            if text.is_empty() {
                None
            } else {
                Some(text)
            }
        }
        "ownText" | "@ownText" => {
            let mut own_text = String::new();
            for node in el.children() {
                if let Some(text_node) = node.value().as_text() {
                    own_text.push_str(text_node.text.trim());
                    own_text.push(' ');
                }
            }
            let text = own_text.trim().to_string();
            if text.is_empty() {
                None
            } else {
                Some(text)
            }
        }
        // 规格 §8.4：`html`/`all` 先移除结果元素里的 script/style，再返回 outerHtml
        "html" | "@html" | "all" | "@all" => Some(strip_script_style(&el.html())),
        _ => {
            if let Some(attr_name) = parse_attr_extractor(extractor) {
                return el.value().attr(attr_name).map(|v| v.to_string());
            }

            if extractor.starts_with('@') {
                el.value().attr(&extractor[1..]).map(|v| v.to_string())
            } else {
                el.value().attr(extractor).map(|v| v.to_string())
            }
        }
    }
}

fn parse_attr_extractor(extractor: &str) -> Option<&str> {
    let extractor = extractor.trim();
    let extractor = extractor.strip_prefix('@').unwrap_or(extractor);
    extractor
        .strip_prefix("attr[")
        .and_then(|s| s.strip_suffix(']'))
        .filter(|s| !s.is_empty())
}

/// 递归后代文本（保留元素内部结构，按 `\n` 连接）。
///
/// 供规则引擎作为内联 JS 的输入、以及无取值后缀时的默认取值使用；
/// 与规格里只取直接文本节点的 `textNodes` 取值后缀不是一回事。
pub(crate) fn descendant_text(el: &ElementRef) -> String {
    let mut texts = Vec::new();
    collect_text_nodes(*el, &mut texts);
    texts.join("\n")
}

/// 直接文本节点（规格 §8.4 的 `textNodes`）：只取元素的直接子文本节点。
fn direct_text_nodes(el: &ElementRef) -> String {
    let mut texts = Vec::new();
    for node in el.children() {
        if let Some(text_node) = node.value().as_text() {
            let text = text_node.text.trim();
            if !text.is_empty() {
                texts.push(text.to_string());
            }
        }
    }
    texts.join("\n")
}

/// 移除 HTML 片段里的 `script`/`style` 元素（规格 §8.4 的 `html`/`all` 取值）。
fn strip_script_style(html: &str) -> String {
    static SCRIPT_STYLE: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r"(?is)<script\b[^>]*>.*?</script>|<style\b[^>]*>.*?</style>").unwrap()
    });
    SCRIPT_STYLE.replace_all(html, "").into_owned()
}

fn collect_text_nodes(el: ElementRef, texts: &mut Vec<String>) {
    for node in el.children() {
        if let Some(text_node) = node.value().as_text() {
            let text = text_node.text.trim().to_string();
            if !text.is_empty() {
                texts.push(text);
            }
        }
        if let Some(child_el) = ElementRef::wrap(node) {
            collect_text_nodes(child_el, texts);
        }
    }
}

pub fn select_text_from_element(el: &ElementRef, rule: &str) -> Option<String> {
    let parts = split_top_level(rule, &["@"]).parts;
    let mut current_matches = vec![*el];

    for i in 0..parts.len() {
        let part = parts[i].trim();
        if part.is_empty() {
            continue;
        }

        if i == parts.len() - 1 {
            return current_matches
                .into_iter()
                .find_map(|current| extract_text(&current, part));
        }

        let parsed = parse_selector_with_index(part);
        let current_level = current_matches;
        let mut next_matches = Vec::new();
        for current in current_level {
            next_matches.extend(collect_matches_from_element(current, &parsed));
        }
        if next_matches.is_empty() {
            return None;
        }
        current_matches = next_matches;
    }

    current_matches
        .into_iter()
        .find_map(|current| extract_text(&current, "text"))
}

/// 元素级列表取值（规格 §8.3 `getStringList`）：返回规则在当前元素下命中的全部文本。
///
/// `@` 链与单值版同语义：前面的段逐级下钻选择器，最后一段是取值器
/// （旧实现把 `parts[1..]` 拼回单个取值器，`a@b@c` 会静默退化成查属性 `"b@c"`）。
pub fn select_text_list_from_element(el: &ElementRef, rule: &str) -> Vec<String> {
    let combo = split_top_level(rule, &["&&", "||", "%%"]);
    if let Some(operator) = combo.delimiter.as_deref() {
        return combine_string_parts(operator, &combo.parts, |part| {
            select_text_list_from_element(el, part)
        });
    }

    let split = split_top_level(rule, &["@"]);
    let parts: Vec<&str> = split
        .parts
        .iter()
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .collect();
    if parts.is_empty() {
        return vec![];
    }

    let (selectors, extractor) = if parts.len() > 1 {
        (&parts[..parts.len() - 1], parts[parts.len() - 1])
    } else {
        (&parts[..], "text")
    };

    let mut current_matches = vec![*el];
    for selector in selectors {
        let parsed = parse_selector_with_index(selector);
        let mut next_matches = Vec::new();
        for current in current_matches {
            next_matches.extend(collect_matches_from_element(current, &parsed));
        }
        if next_matches.is_empty() {
            return vec![];
        }
        current_matches = next_matches;
    }

    current_matches
        .into_iter()
        .filter_map(|matched| extract_text(&matched, extractor))
        .collect()
}

/// Select all matching elements and collect their text, joined by newlines
pub fn select_all_text(doc: &Html, rule: &str) -> Option<String> {
    let parts = split_top_level(rule, &["@"]).parts;
    if parts.is_empty() {
        return None;
    }

    let first_part = parts[0].trim();
    let roots = collect_matches(doc, &parse_selector_with_index(first_part));
    if roots.is_empty() {
        return None;
    }

    if parts.len() > 1 {
        let mut all_texts = Vec::new();
        let sub_rule = parts[1..].join("@");

        for root in roots {
            let last_part = sub_rule.trim();
            if let Some(text) = extract_text(&root, last_part) {
                if !text.is_empty() {
                    all_texts.push(text);
                }
                continue;
            }

            let parsed = parse_selector_with_index(sub_rule.trim());
            for el in collect_matches_from_element(root, &parsed) {
                if let Some(text) = extract_text(&el, "text") {
                    if !text.is_empty() {
                        all_texts.push(text);
                    }
                }
            }
        }

        if all_texts.is_empty() {
            return None;
        }
        return Some(all_texts.join("\n"));
    }

    let mut texts = Vec::new();
    for root in roots {
        let text = descendant_text(&root);
        if !text.is_empty() {
            texts.push(text);
        }
    }
    if texts.is_empty() {
        return None;
    }
    Some(texts.join("\n"))
}

pub fn select_text(doc: &Html, rule: &str) -> Option<String> {
    select_text_list(doc, rule).into_iter().next()
}

pub fn select_text_list(doc: &Html, rule: &str) -> Vec<String> {
    let combo = split_top_level(rule, &["&&", "||", "%%"]);
    if let Some(operator) = combo.delimiter.as_deref() {
        let mut result =
            select_text_list(doc, combo.parts.first().map(String::as_str).unwrap_or(""));
        for part in combo.parts.iter().skip(1) {
            let next = select_text_list(doc, part);
            match operator {
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
        return result;
    }

    // Handle rule chaining with @@
    if rule.contains("@@") {
        let rules: Vec<&str> = rule.split("@@").collect();
        if rules.is_empty() {
            return vec![];
        }

        // Start with first rule - get all matching texts
        let mut current_texts = select_text_list(doc, rules[0]);

        // Apply subsequent rules
        for r in rules.iter().skip(1) {
            if current_texts.is_empty() {
                break;
            }
            let mut new_texts = Vec::new();
            for text in &current_texts {
                let sub_doc = Html::parse_document(text);
                new_texts.extend(select_text_list(&sub_doc, r));
            }
            current_texts = new_texts;
        }

        return current_texts;
    }

    let parts = split_top_level(rule, &["@"]).parts;
    if parts.is_empty() {
        return vec![];
    }

    let first_part = parts[0].trim();

    let matches = collect_matches(doc, &parse_selector_with_index(first_part));
    if matches.is_empty() {
        return vec![];
    }

    let mut results = Vec::new();
    for el in matches {
        if parts.len() > 1 {
            let rest = parts[1..].join("@");
            if let Some(v) = select_text_from_element(&el, &rest) {
                results.push(v);
            }
        } else {
            results.push(extract_text(&el, "text").unwrap_or_default());
        }
    }
    results
}

/// XPath 表达式的长度上限。
pub const MAX_XPATH_EXPR_LEN: usize = 8 * 1024;
/// XPath 表达式的括号/谓词嵌套上限。
///
/// sxd-xpath 的解析器按嵌套层级递归，求值也按表达式树递归；书源可控的深嵌套
/// 表达式（如数千层 `((((...))))`) 会撑爆线程栈、直接终止整个进程（栈溢出在
/// Rust 里不可捕获），所以必须在 build 之前拦掉。
pub const MAX_XPATH_EXPR_NESTING: usize = 64;

/// 表达式是否在安全界限内（长度 + 嵌套深度）。字符串字面量内的括号不计。
pub fn xpath_within_limits(xpath: &str) -> bool {
    if xpath.len() > MAX_XPATH_EXPR_LEN {
        return false;
    }
    let mut depth = 0usize;
    let mut in_string = false;
    let mut quote = '\0';
    for ch in xpath.chars() {
        match ch {
            '"' | '\'' if in_string && ch == quote => in_string = false,
            '"' | '\'' if !in_string => {
                in_string = true;
                quote = ch;
            }
            '(' | '[' if !in_string => {
                depth += 1;
                if depth > MAX_XPATH_EXPR_NESTING {
                    return false;
                }
            }
            ')' | ']' if !in_string => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    true
}

/// 把输入解析为 sxd 文档包。先试严格 XML；失败则经 html5ever 容错解析后重新
/// 序列化为良构文档再解析——sxd_document 是严格 XML 解析器，普通 HTML（不自
/// 闭合的 void 元素、HTML 实体、裸 `&`）会直接失败，没有这层回退时 XPath 规则
/// 对绝大多数书源页面只会静默返回空。
pub fn parse_xml_or_html(input: &str) -> Option<sxd_document::Package> {
    if let Ok(package) = sxd_document::parser::parse(input) {
        return Some(package);
    }
    let mut xhtml = Html::parse_document(input).html();
    // html5ever 的序列化仍不是良构 XML，需两处修补：
    // `&nbsp;` 是 HTML 专有实体（换成字面字符），void 元素不自闭合（补上 `/>`）
    if xhtml.contains("&nbsp;") {
        xhtml = xhtml.replace("&nbsp;", "\u{a0}");
    }
    static VOID_ELEMENTS: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(
            r"<(area|base|br|col|embed|hr|img|input|link|meta|param|source|track|wbr)(\s[^>]*)?>",
        )
        .unwrap()
    });
    xhtml = VOID_ELEMENTS.replace_all(&xhtml, "<$1$2/>").into_owned();
    sxd_document::parser::parse(&xhtml).ok()
}

/// `&&` 结果拼接 / `||` 首个非空 / `%%` 交错合并，与 CSS 组合符语义一致。
pub(crate) fn combine_string_parts<F>(op: &str, parts: &[String], eval: F) -> Vec<String>
where
    F: Fn(&str) -> Vec<String>,
{
    let mut result = eval(parts.first().map(String::as_str).unwrap_or(""));
    for part in parts.iter().skip(1) {
        let next = eval(part);
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
    result
}

/// XPath support using sxd-xpath
pub fn select_xpath(html: &str, xpath: &str) -> Vec<String> {
    if !xpath_within_limits(xpath) {
        return vec![];
    }
    // 组合规则：&& 拼接 / || 首个非空 / %% 交错（顶层切分，引号括号内不切）
    let combo = crate::parser::rule_analyzer::split_top_level(xpath, &["&&", "||", "%%"]);
    if let Some(op) = combo.delimiter.as_deref() {
        return combine_string_parts(op, &combo.parts, |part| select_xpath(html, part));
    }
    let package = match parse_xml_or_html(html) {
        Some(p) => p,
        None => return vec![],
    };

    let document = package.as_document();
    let context = sxd_xpath::Context::new();

    match sxd_xpath::Factory::new().build(xpath) {
        Ok(Some(xpath_expr)) => match xpath_expr.evaluate(&context, document.root()) {
            Ok(value) => match value {
                sxd_xpath::Value::Nodeset(ns) => ns
                    .document_order()
                    .into_iter()
                    .map(|n| n.string_value())
                    .collect(),
                sxd_xpath::Value::String(s) => vec![s],
                sxd_xpath::Value::Number(n) => vec![n.to_string()],
                sxd_xpath::Value::Boolean(b) => vec![b.to_string()],
            },
            Err(_) => vec![],
        },
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xpath_limits_reject_deep_nesting_and_overlong() {
        let deep = format!("{}{}", "(".repeat(5000), ")".repeat(5000));
        assert!(!xpath_within_limits(&deep));
        // 被拦在 build 之前，不会递归爆栈
        assert!(select_xpath("<p>x</p>", &deep).is_empty());
        assert!(!xpath_within_limits(&"a".repeat(MAX_XPATH_EXPR_LEN + 1)));
    }

    #[test]
    fn xpath_limits_accept_normal_expressions() {
        assert!(xpath_within_limits("//div[@class='a']/span[1]/text()"));
        // 字符串字面量里的括号不计入嵌套
        assert!(xpath_within_limits("//ul/li[contains(@class,'(((((')]"));
    }

    #[test]
    fn xpath_parses_messy_html_via_html5ever_fallback() {
        // 普通 HTML5：未闭合 void 元素、&nbsp; 实体、裸 & —— 严格 XML 解析必失败，
        // 必须经 html5ever 容错回退后才能用 XPath
        let messy = "<html><body><div class=\"list\"><p>第一章&nbsp;<br><img src=x.png></p>\
                     <p>第二章 & 后续</p></div></body></html>";
        assert!(
            sxd_document::parser::parse(messy).is_err(),
            "前提：严格解析必须失败"
        );
        let items = select_xpath(messy, "//div[@class=\"list\"]/p");
        assert_eq!(items.len(), 2);
        assert!(items[0].contains("第一章"), "got: {:?}", items[0]);
        assert!(items[1].contains("第二章"), "got: {:?}", items[1]);
        // 属性取值
        let srcs = select_xpath(messy, "//img/@src");
        assert_eq!(srcs, vec!["x.png".to_string()]);
    }

    #[test]
    fn xpath_combinators_concat_fallback_and_interleave() {
        let doc = r#"<html><body><div class="a"><p>A1</p><p>A2</p></div>
                     <div class="b"><p>B1</p></div></body></html>"#;
        // && 结果拼接
        assert_eq!(
            select_xpath(doc, "//div[@class='a']/p && //div[@class='b']/p"),
            vec!["A1", "A2", "B1"]
        );
        // || 首个非空
        assert_eq!(
            select_xpath(doc, "//div[@class='none']/p || //div[@class='b']/p"),
            vec!["B1"]
        );
        // %% 交错
        assert_eq!(
            select_xpath(doc, "//div[@class='a']/p %% //div[@class='b']/p"),
            vec!["A1", "B1", "A2"]
        );
        // 引号内的分隔符不切
        assert_eq!(
            select_xpath(doc, "//p[text()='A1 && x']").len(),
            0,
            "文本不命中但不应把 && 当组合符切开"
        );
        // XPath 原生 | 并集不受影响
        let union = select_xpath(doc, "//div[@class='a']/p | //div[@class='b']/p");
        assert_eq!(union.len(), 3);
    }

    #[test]
    fn test_legado_to_css() {
        assert_eq!(legado_to_css("class.mod block"), ".mod.block");
        assert_eq!(legado_to_css("class.test"), ".test");
        assert_eq!(legado_to_css("id.main"), "#main");
        assert_eq!(legado_to_css("tag.div"), "div");
    }

    #[test]
    fn test_parse_selector_with_index() {
        let parsed = parse_selector_with_index(".test.0");
        assert_eq!(parsed.base, SelectorBase::Css(".test".to_string()));
        assert!(parsed.explicit_index);
        assert_eq!(parsed.index_mode, IndexMode::Select);
        assert_eq!(parsed.index_items, vec![IndexItem::Single(0)]);

        let parsed = parse_selector_with_index(".test.-1");
        assert_eq!(parsed.base, SelectorBase::Css(".test".to_string()));
        assert_eq!(parsed.index_items, vec![IndexItem::Single(-1)]);

        let parsed = parse_selector_with_index(".test!0");
        assert_eq!(parsed.base, SelectorBase::Css(".test".to_string()));
        assert_eq!(parsed.index_mode, IndexMode::Exclude);
        assert_eq!(parsed.index_items, vec![IndexItem::Single(0)]);

        let parsed = parse_selector_with_index("div[-1, 1:3]");
        assert_eq!(parsed.base, SelectorBase::Css("div".to_string()));
        assert_eq!(
            parsed.index_items,
            vec![
                IndexItem::Single(-1),
                IndexItem::Range {
                    start: Some(1),
                    end: Some(3),
                    step: 1,
                },
            ]
        );
    }

    #[test]
    fn test_legacy_index_range_syntax() {
        let doc = parse_document(
            r#"<div><a href="/1">A</a><a href="/2">B</a><a href="/3">C</a><a href="/4">D</a></div>"#,
        );

        // 旧写法 `!0:2` 是区间排除（规格 §8.5）
        assert_eq!(select_text_list(&doc, "a!0:2@text"), vec!["D".to_string()]);
        // `.1:2` 是区间选择
        assert_eq!(
            select_text_list(&doc, "a.1:2@text"),
            vec!["B".to_string(), "C".to_string()]
        );
        // `.0:3:2` 带步长
        assert_eq!(
            select_text_list(&doc, "a.0:3:2@text"),
            vec!["A".to_string(), "C".to_string()]
        );
    }

    #[test]
    fn test_text_nodes_takes_only_direct_children() {
        let doc = parse_document(r#"<div>直接<span>嵌套</span></div>"#);

        assert_eq!(select_text(&doc, "div@textNodes"), Some("直接".to_string()));
        // 无取值后缀时的默认取值仍是递归后代文本
        assert_eq!(select_text(&doc, "div"), Some("直接 嵌套".to_string()));
    }

    #[test]
    fn test_html_extractor_strips_script_and_style() {
        let doc = parse_document(
            r#"<div class="intro">正文<script>var a = 1;</script><style>.x{}</style><b>粗</b></div>"#,
        );

        let html = select_text(&doc, "div@html").unwrap();
        assert!(html.contains("<b>粗</b>"), "应保留普通标签：{html}");
        assert!(!html.contains("<script"), "应移除 script：{html}");
        assert!(!html.contains("<style"), "应移除 style：{html}");
        assert!(html.starts_with("<div"), "应返回 outer HTML：{html}");
    }

    #[test]
    fn test_select_text_list_from_element_collects_every_match() {
        let doc = parse_document(
            r#"<div class="book"><span class="tag">玄幻</span><span class="tag">仙侠</span></div>"#,
        );
        let element = doc
            .select(&Selector::parse("div.book").unwrap())
            .next()
            .unwrap();

        assert_eq!(
            select_text_list_from_element(&element, "span.tag@text"),
            vec!["玄幻".to_string(), "仙侠".to_string()]
        );
    }

    #[test]
    fn test_select_text_list_from_element_walks_at_chain() {
        // `a@b@c` 的前几段是逐级下钻的选择器，最后一段才是取值器
        let doc = parse_document(
            r#"<div class="book"><ul><li><a href="/t1">玄幻</a></li><li><a href="/t2">仙侠</a></li></ul></div>"#,
        );
        let element = doc
            .select(&Selector::parse("div.book").unwrap())
            .next()
            .unwrap();

        assert_eq!(
            select_text_list_from_element(&element, "ul@li@a@text"),
            vec!["玄幻".to_string(), "仙侠".to_string()]
        );
        assert_eq!(
            select_text_list_from_element(&element, "ul@li@a@href"),
            vec!["/t1".to_string(), "/t2".to_string()]
        );
    }

    #[test]
    fn test_select_text_list_with_bracket_indices() {
        let doc = parse_document(
            r#"<div><a href="/1">A</a><a href="/2">B</a><a href="/3">C</a><a href="/4">D</a></div>"#,
        );

        assert_eq!(
            select_text_list(&doc, "a[1:2]@text"),
            vec!["B".to_string(), "C".to_string()]
        );
        assert_eq!(
            select_text_list(&doc, "a[!1,2]@text"),
            vec!["A".to_string(), "D".to_string()]
        );
        assert_eq!(
            select_text_list(&doc, "a[-1:0]@text"),
            vec![
                "D".to_string(),
                "C".to_string(),
                "B".to_string(),
                "A".to_string()
            ]
        );
    }

    #[test]
    fn test_extract_attr_bracket_syntax() {
        let doc = parse_document(r#"<div><a href="/book/1" data-id="abc">Book</a></div>"#);

        assert_eq!(
            select_text(&doc, "a@attr[href]"),
            Some("/book/1".to_string())
        );
        assert_eq!(
            select_text(&doc, "a@attr[data-id]"),
            Some("abc".to_string())
        );
        assert_eq!(select_text(&doc, "a@href"), Some("/book/1".to_string()));
    }
}
