use crate::model::review::{ParaReviewCount, ReviewItem, ReviewPage, ReviewReply};
use crate::model::rule::{BookInfoRule, SearchRule, TocRule};
use crate::model::{
    book::Book, book_chapter::BookChapter, book_source::BookSource, search::SearchBook,
};
use crate::parser::{
    html,
    js::{eval_js, eval_js_with_bindings, eval_js_with_bindings_and_globals, with_book_source},
    jsonpath,
};
use crate::util::text::normalize_source_url;
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use std::collections::HashMap;
use sxd_xpath::{Context as XPathContext, Factory as XPathFactory, Value as XPathValue};

/// 规则模板/占位符的固定正则：热路径上对每个元素每个字段都会用到，
/// 每次调用重新编译是纯浪费（2000 章目录页此前要编译数万次）。
static TEMPLATE_JS_RE: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"\{\{(.*?)\}\}").unwrap());
static INLINE_JS_RE: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"\{\{([^}]+)\}\}").unwrap());
static TEMPLATE_GET_RE: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"@get:\{([^}]+)\}").unwrap());
static REGEX_PLACEHOLDER_RE: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"\$(\d{1,2})").unwrap());

/// 文档级字段求值的共享视图：把整份 DOM 序列化成 HTML 字符串是
/// O(文档大小) 的工作，book_info 一次要对同一份文档求值十余个字段，
/// 用 `OnceCell` 让多个字段复用同一次序列化结果。
struct DocView<'a> {
    doc: &'a scraper::Html,
    serialized: std::cell::OnceCell<String>,
}

impl<'a> DocView<'a> {
    fn new(doc: &'a scraper::Html) -> Self {
        Self {
            doc,
            serialized: std::cell::OnceCell::new(),
        }
    }

    fn html(&self) -> &str {
        self.serialized.get_or_init(|| self.doc.html())
    }

    fn doc(&self) -> &'a scraper::Html {
        self.doc
    }
}

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

        // Auto-detect from content
        let content_trimmed = content.trim();
        if content_trimmed.starts_with('{') || content_trimmed.starts_with('[') {
            // Likely JSON content
            if rule.starts_with("$.") || rule.starts_with("$[") {
                return ParseMode::JsonPath;
            }
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

    pub fn search_books(&self, source: &BookSource, body: &str, base_url: &str) -> Vec<SearchBook> {
        with_book_source(source, || {
            let rule = source.rule_search.clone().unwrap_or_default();
            let (list_rule, reverse) = normalize_list_rule(rule.book_list.as_deref().unwrap_or(""));

            // 规格 §13.4 第 4 步：`bookUrlPattern` 命中当前响应 URL 时，这一页本身就是详情页
            if book_url_pattern_matches(source, base_url) {
                if let Some(detail_book) = self.search_detail_fallback(source, body, base_url) {
                    return vec![detail_book];
                }
            }

            let mode = self.detect_mode(list_rule, body);
            let mut results = match mode {
                ParseMode::JsonPath => {
                    self.search_books_json(source, body, base_url, &rule, list_rule)
                }
                ParseMode::XPath => {
                    self.search_books_xpath(source, body, base_url, &rule, list_rule)
                }
                ParseMode::Js => self.search_books_js(source, body, base_url, &rule, list_rule),
                ParseMode::Regex => {
                    self.search_books_regex(source, body, base_url, &rule, list_rule)
                }
                ParseMode::Css => self.search_books_html(source, body, base_url, &rule, list_rule),
            };

            if results.is_empty()
                && source
                    .book_url_pattern
                    .as_deref()
                    .map(|s| s.trim().is_empty())
                    .unwrap_or(true)
            {
                if let Some(detail_book) = self.search_detail_fallback(source, body, base_url) {
                    results.push(detail_book);
                }
            }
            if reverse {
                results.reverse();
            }
            dedupe_books(results)
        })
    }

    pub fn explore_books(
        &self,
        source: &BookSource,
        body: &str,
        base_url: &str,
    ) -> Vec<SearchBook> {
        with_book_source(source, || {
            let rule = source
                .rule_explore
                .clone()
                .filter(|rule| {
                    rule.book_list
                        .as_deref()
                        .is_some_and(|value| !value.trim().is_empty())
                })
                .unwrap_or_else(|| source.rule_search.clone().unwrap_or_default());
            let (list_rule, reverse) = normalize_list_rule(rule.book_list.as_deref().unwrap_or(""));
            let mode = self.detect_mode(list_rule, body);
            let mut results = match mode {
                ParseMode::JsonPath => {
                    self.search_books_json(source, body, base_url, &rule, list_rule)
                }
                ParseMode::XPath => {
                    self.search_books_xpath(source, body, base_url, &rule, list_rule)
                }
                ParseMode::Js => self.search_books_js(source, body, base_url, &rule, list_rule),
                ParseMode::Regex => {
                    self.search_books_regex(source, body, base_url, &rule, list_rule)
                }
                ParseMode::Css => self.search_books_html(source, body, base_url, &rule, list_rule),
            };
            if reverse {
                results.reverse();
            }
            dedupe_books(results)
        })
    }

    pub fn book_info(
        &self,
        source: &BookSource,
        body: &str,
        base_url: &str,
        book_url: &str,
    ) -> Book {
        with_book_source(source, || {
            let rule = source.rule_book_info.clone().unwrap_or_default();
            let mut context = HashMap::new();

            let mode = self.detect_mode(rule.name.as_deref().unwrap_or(""), body);
            match mode {
                ParseMode::JsonPath => {
                    if let Ok(v) = serde_json::from_str::<Value>(body) {
                        return parse_book_info_json(
                            source,
                            &v,
                            base_url,
                            &rule,
                            book_url,
                            &mut context,
                        );
                    }
                }
                ParseMode::XPath => {
                    return parse_book_info_xpath(
                        source,
                        body,
                        base_url,
                        &rule,
                        book_url,
                        &mut context,
                    );
                }
                _ => {}
            }
            parse_book_info_html(source, body, base_url, &rule, book_url, &mut context)
        })
    }

    pub fn chapter_list(
        &self,
        source: &BookSource,
        body: &str,
        base_url: &str,
    ) -> (Vec<BookChapter>, Vec<String>) {
        with_book_source(source, || {
            let rule = source.rule_toc.clone().unwrap_or_default();
            let mut context = HashMap::new();
            let (list_rule, reverse) =
                normalize_list_rule(rule.chapter_list.as_deref().unwrap_or(""));
            let prepared_body = prepare_toc_body(body, base_url, &rule);
            let mode = self.detect_mode(list_rule, &prepared_body);
            let (mut chapters, next_urls) = match mode {
                ParseMode::JsonPath => parse_chapter_list_json(
                    &prepared_body,
                    base_url,
                    &rule,
                    list_rule,
                    &mut context,
                ),
                ParseMode::XPath => parse_chapter_list_xpath(
                    &prepared_body,
                    base_url,
                    &rule,
                    list_rule,
                    &mut context,
                ),
                ParseMode::Js => self.parse_chapter_list_js(
                    &prepared_body,
                    base_url,
                    &rule,
                    list_rule,
                    &mut context,
                ),
                ParseMode::Regex => {
                    self.parse_chapter_list_regex(&prepared_body, base_url, &rule, list_rule)
                }
                ParseMode::Css => parse_chapter_list_html(
                    &prepared_body,
                    base_url,
                    &rule,
                    list_rule,
                    &mut context,
                ),
            };
            apply_toc_format_js(&mut chapters, rule.format_js.as_deref(), base_url);
            if reverse {
                chapters.reverse();
            }
            for (index, chapter) in chapters.iter_mut().enumerate() {
                chapter.index = index as i32;
            }
            (chapters, next_urls)
        })
    }

    pub fn content(&self, source: &BookSource, body: &str, base_url: &str) -> String {
        with_book_source(source, || {
            let rule = source.rule_content.clone().unwrap_or_default();
            let mut content_body = body.to_string();

            if let Some(source_regex) = rule
                .source_regex
                .as_deref()
                .filter(|s| !s.trim().is_empty())
            {
                content_body = apply_legado_regex(&content_body, source_regex);
            }
            if let Some(web_js) = rule.web_js.as_deref().filter(|s| !s.trim().is_empty()) {
                if let Ok(processed) =
                    eval_js(self.strip_mode_prefix(web_js), &content_body, base_url)
                {
                    if !processed.trim().is_empty() {
                        content_body = processed;
                    }
                }
            }

            if let Some(content_rule) = rule.content.clone() {
                if matches!(
                    self.detect_mode(&content_rule, &content_body),
                    ParseMode::Js
                ) {
                    let script = self.strip_mode_prefix(&content_rule);
                    if let Ok(res) = eval_js(script, &content_body, base_url) {
                        return res;
                    }
                }

                let content_rule = self.process_inline_js(&content_rule, &content_body, base_url);

                let mode = self.detect_mode(&content_rule, &content_body);
                let mut content = match mode {
                    ParseMode::JsonPath => {
                        if let Ok(v) = serde_json::from_str::<Value>(&content_body) {
                            jsonpath::jsonpath_first_string(
                                &v,
                                self.strip_mode_prefix(&content_rule),
                            )
                            .unwrap_or_default()
                        } else {
                            String::new()
                        }
                    }
                    ParseMode::XPath => {
                        html::select_xpath(&content_body, self.strip_mode_prefix(&content_rule))
                            .first()
                            .cloned()
                            .unwrap_or_default()
                    }
                    _ => {
                        let doc = html::parse_document(&content_body);
                        let result =
                            html::select_all_text(&doc, self.strip_mode_prefix(&content_rule));
                        result.unwrap_or_default()
                    }
                };

                if let Some(replace) = rule.replace_regex.as_deref() {
                    content = apply_legado_regex(&content, replace);
                }

                return content;
            }

            String::new()
        })
    }

    /// Process inline JavaScript {{...}} in rules
    fn process_inline_js(&self, rule: &str, body: &str, base_url: &str) -> String {
        let mut result = rule.to_string();

        // Find all {{...}} blocks and evaluate them
        let re = &*INLINE_JS_RE;
        for cap in re.captures_iter(rule) {
            if let Some(js_code) = cap.get(1) {
                if let Ok(js_result) = eval_js(js_code.as_str(), body, base_url) {
                    result = result.replace(cap.get(0).unwrap().as_str(), &js_result);
                }
            }
        }

        result
    }

    /// Get the next content page URL if pagination exists
    pub fn next_content_url(
        &self,
        source: &BookSource,
        body: &str,
        base_url: &str,
    ) -> Option<String> {
        let rule = source.rule_content.clone().unwrap_or_default();
        let next_rule = rule.next_content_url.as_deref()?;
        if next_rule.is_empty() {
            return None;
        }

        let mode = self.detect_mode(next_rule, body);
        let next_url = match mode {
            ParseMode::JsonPath => {
                if let Ok(v) = serde_json::from_str::<Value>(body) {
                    jsonpath::jsonpath_first_string(&v, self.strip_mode_prefix(next_rule))
                } else {
                    None
                }
            }
            ParseMode::XPath => html::select_xpath(body, self.strip_mode_prefix(next_rule))
                .first()
                .cloned(),
            _ => {
                let doc = html::parse_document(body);
                html::select_text(&doc, self.strip_mode_prefix(next_rule))
            }
        };

        if next_url.as_ref().map(|s| s.is_empty()).unwrap_or(true) {
            return None;
        }

        let next_url = next_url?;
        Some(resolve_url(base_url, &next_url))
    }

    fn search_detail_fallback(
        &self,
        source: &BookSource,
        body: &str,
        base_url: &str,
    ) -> Option<SearchBook> {
        let book = self.book_info(source, body, base_url, base_url);
        search_book_from_book(book)
    }

    fn search_books_js(
        &self,
        source: &BookSource,
        body: &str,
        base_url: &str,
        rule: &SearchRule,
        list_rule: &str,
    ) -> Vec<SearchBook> {
        let output = match eval_js(self.strip_mode_prefix(list_rule), body, base_url) {
            Ok(result) => result,
            Err(_) => return vec![],
        };

        if let Some(items) = parse_js_output_items(&output) {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                if let Some(book) = build_search_book_from_json(source, &item, base_url, rule) {
                    out.push(book);
                }
            }
            return out;
        }

        let doc = html::parse_document(&output);
        let sel = match scraper::Selector::parse("body > *") {
            Ok(sel) => sel,
            Err(_) => return vec![],
        };
        let mut out = Vec::new();
        for el in doc.select(&sel) {
            let name = rule
                .name
                .as_ref()
                .and_then(|r| eval_field_html(r, &el, base_url))
                .unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            let author = rule
                .author
                .as_ref()
                .and_then(|r| eval_field_html(r, &el, base_url))
                .unwrap_or_default();
            let book_url = rule
                .book_url
                .as_ref()
                .and_then(|r| eval_field_html(r, &el, base_url))
                .unwrap_or_default();
            let cover_url = rule
                .cover_url
                .as_ref()
                .and_then(|r| eval_field_html(r, &el, base_url))
                .map(|u| resolve_url(base_url, &u));
            let intro = rule
                .intro
                .as_ref()
                .and_then(|r| eval_field_html(r, &el, base_url));
            let kind = eval_kind_html_element(rule.kind.as_deref(), &el, base_url);
            let last_chapter = rule
                .last_chapter
                .as_ref()
                .and_then(|r| eval_field_html(r, &el, base_url));
            let update_time = rule
                .update_time
                .as_ref()
                .and_then(|r| eval_field_html(r, &el, base_url));
            let word_count = rule
                .word_count
                .as_ref()
                .and_then(|r| eval_field_html(r, &el, base_url));
            out.push(SearchBook {
                name,
                author,
                book_url: resolve_url(base_url, &book_url),
                origin: source.book_source_url.clone(),
                cover_url,
                intro,
                kind,
                last_chapter,
                update_time,
                word_count,
                book_source_urls: None,
            });
        }
        out
    }

    fn search_books_regex(
        &self,
        source: &BookSource,
        body: &str,
        base_url: &str,
        rule: &SearchRule,
        list_rule: &str,
    ) -> Vec<SearchBook> {
        let pattern = self
            .strip_mode_prefix(list_rule)
            .trim_start_matches(':')
            .trim();

        let mut out = Vec::new();
        for groups in regex_list_captures(pattern, body) {
            let name = capture_rule_value(rule.name.as_deref(), &groups).unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            let author = capture_rule_value(rule.author.as_deref(), &groups).unwrap_or_default();
            let book_url =
                capture_rule_value(rule.book_url.as_deref(), &groups).unwrap_or_default();
            let cover_url = capture_rule_value(rule.cover_url.as_deref(), &groups)
                .map(|u| resolve_url(base_url, &u));
            let intro = capture_rule_value(rule.intro.as_deref(), &groups);
            let kind = capture_rule_value(rule.kind.as_deref(), &groups);
            let last_chapter = capture_rule_value(rule.last_chapter.as_deref(), &groups);
            let update_time = capture_rule_value(rule.update_time.as_deref(), &groups);
            let word_count = capture_rule_value(rule.word_count.as_deref(), &groups);
            out.push(SearchBook {
                name,
                author,
                book_url: resolve_url(base_url, &book_url),
                origin: source.book_source_url.clone(),
                cover_url,
                intro,
                kind,
                last_chapter,
                update_time,
                word_count,
                book_source_urls: None,
            });
        }
        out
    }

    fn parse_chapter_list_js(
        &self,
        body: &str,
        base_url: &str,
        rule: &TocRule,
        list_rule: &str,
        ctx: &mut HashMap<String, String>,
    ) -> (Vec<BookChapter>, Vec<String>) {
        let output = match eval_js(self.strip_mode_prefix(list_rule), body, base_url) {
            Ok(result) => result,
            Err(_) => return (vec![], vec![]),
        };

        if let Some(items) = parse_js_output_items(&output) {
            let mut out = Vec::with_capacity(items.len());
            let mut seen_urls = std::collections::HashSet::new();
            for item in items {
                if let Some(chapter) =
                    build_chapter_from_json(&item, base_url, rule, ctx, out.len())
                {
                    if seen_urls.insert(chapter.url.clone()) {
                        out.push(chapter);
                    }
                }
            }
            return (out, vec![]);
        }

        let doc = html::parse_document(&output);
        let sel = match scraper::Selector::parse("body > *") {
            Ok(sel) => sel,
            Err(_) => return (vec![], vec![]),
        };
        let mut out = Vec::new();
        let mut seen_urls = std::collections::HashSet::new();
        for el in doc.select(&sel) {
            let title = rule
                .chapter_name
                .as_ref()
                .and_then(|r| eval_field_html_with_ctx(r, &el, base_url, ctx))
                .unwrap_or_default();
            if title.is_empty() {
                continue;
            }
            let raw_url = rule
                .chapter_url
                .as_ref()
                .and_then(|r| eval_field_html_with_ctx(r, &el, base_url, ctx))
                .unwrap_or_default();
            let tag = rule
                .update_time
                .as_ref()
                .and_then(|r| eval_field_html_with_ctx(r, &el, base_url, ctx));
            let is_volume = rule
                .is_volume
                .as_ref()
                .and_then(|r| eval_field_html_with_ctx(r, &el, base_url, ctx))
                .map(is_truthy)
                .unwrap_or(false);
            let is_vip = rule
                .is_vip
                .as_ref()
                .and_then(|r| eval_field_html_with_ctx(r, &el, base_url, ctx))
                .map(is_truthy)
                .unwrap_or(false);
            let is_pay = rule
                .is_pay
                .as_ref()
                .and_then(|r| eval_field_html_with_ctx(r, &el, base_url, ctx))
                .map(is_truthy)
                .unwrap_or(false);
            let url = finalize_chapter_url(base_url, &raw_url, &title, is_volume, out.len());
            if !seen_urls.insert(url.clone()) {
                continue;
            }
            out.push(BookChapter {
                title,
                url,
                index: out.len() as i32,
                tag,
                is_vip,
                is_pay,
                is_volume,
                ..Default::default()
            });
        }
        (out, vec![])
    }

    fn parse_chapter_list_regex(
        &self,
        body: &str,
        base_url: &str,
        rule: &TocRule,
        list_rule: &str,
    ) -> (Vec<BookChapter>, Vec<String>) {
        let pattern = self
            .strip_mode_prefix(list_rule)
            .trim_start_matches(':')
            .trim();

        let mut out = Vec::new();
        let mut seen_urls = std::collections::HashSet::new();
        for groups in regex_list_captures(pattern, body) {
            let title =
                capture_rule_value(rule.chapter_name.as_deref(), &groups).unwrap_or_default();
            if title.is_empty() {
                continue;
            }
            let raw_url =
                capture_rule_value(rule.chapter_url.as_deref(), &groups).unwrap_or_default();
            let tag = capture_rule_value(rule.update_time.as_deref(), &groups);
            let is_volume = capture_rule_value(rule.is_volume.as_deref(), &groups)
                .map(is_truthy)
                .unwrap_or(false);
            let is_vip = capture_rule_value(rule.is_vip.as_deref(), &groups)
                .map(is_truthy)
                .unwrap_or(false);
            let is_pay = capture_rule_value(rule.is_pay.as_deref(), &groups)
                .map(is_truthy)
                .unwrap_or(false);
            let url = finalize_chapter_url(base_url, &raw_url, &title, is_volume, out.len());
            if !seen_urls.insert(url.clone()) {
                continue;
            }
            out.push(BookChapter {
                title,
                url,
                index: out.len() as i32,
                tag,
                is_vip,
                is_pay,
                is_volume,
                ..Default::default()
            });
        }
        (out, vec![])
    }

    fn search_books_html(
        &self,
        source: &BookSource,
        body: &str,
        base_url: &str,
        rule: &SearchRule,
        list_sel: &str,
    ) -> Vec<SearchBook> {
        if list_sel.trim().is_empty() {
            return vec![];
        }
        let doc = html::parse_document(body);
        let items = html::select_list(&doc, self.strip_mode_prefix(list_sel));
        let mut out = Vec::with_capacity(items.len());

        for el in items {
            let name = rule
                .name
                .as_ref()
                .and_then(|r| eval_field_html(r, &el, base_url))
                .unwrap_or_default();
            let author = rule
                .author
                .as_ref()
                .and_then(|r| eval_field_html(r, &el, base_url))
                .unwrap_or_default();
            let book_url = rule
                .book_url
                .as_ref()
                .and_then(|r| eval_field_html(r, &el, base_url))
                .unwrap_or_default();
            let cover_url = rule
                .cover_url
                .as_ref()
                .and_then(|r| eval_field_html(r, &el, base_url));
            let intro = rule
                .intro
                .as_ref()
                .and_then(|r| eval_field_html(r, &el, base_url));
            let kind = eval_kind_html_element(rule.kind.as_deref(), &el, base_url);
            let last_chapter = rule
                .last_chapter
                .as_ref()
                .and_then(|r| eval_field_html(r, &el, base_url));
            let update_time = rule
                .update_time
                .as_ref()
                .and_then(|r| eval_field_html(r, &el, base_url));
            let word_count = rule
                .word_count
                .as_ref()
                .and_then(|r| eval_field_html(r, &el, base_url));
            let book_url_abs = resolve_url(base_url, &book_url);
            let cover_url_abs = cover_url.map(|u| resolve_url(base_url, &u));
            out.push(SearchBook {
                name,
                author,
                book_url: book_url_abs,
                origin: source.book_source_url.clone(),
                cover_url: cover_url_abs,
                intro,
                kind,
                last_chapter,
                update_time,
                word_count,
                book_source_urls: None,
            });
        }
        out
    }

    fn search_books_xpath(
        &self,
        source: &BookSource,
        body: &str,
        base_url: &str,
        rule: &SearchRule,
        list_rule: &str,
    ) -> Vec<SearchBook> {
        let package = match html::parse_xml_or_html(body) {
            Some(p) => p,
            None => return vec![],
        };
        let document = package.as_document();
        let items = xpath_select_nodes(
            sxd_xpath::nodeset::Node::Root(document.root()),
            self.strip_mode_prefix(list_rule),
        );
        let mut out = Vec::with_capacity(items.len());

        for item in items {
            let name = eval_field_xpath(rule.name.as_deref().unwrap_or(""), item, base_url);
            let author = eval_field_xpath(rule.author.as_deref().unwrap_or(""), item, base_url);
            let book_url = eval_field_xpath(rule.book_url.as_deref().unwrap_or(""), item, base_url);
            let cover_url =
                eval_field_xpath(rule.cover_url.as_deref().unwrap_or(""), item, base_url);
            let intro = eval_field_xpath(rule.intro.as_deref().unwrap_or(""), item, base_url);
            let kind = eval_kind_xpath(rule.kind.as_deref(), item, base_url);
            let last_chapter =
                eval_field_xpath(rule.last_chapter.as_deref().unwrap_or(""), item, base_url);
            let update_time =
                eval_field_xpath(rule.update_time.as_deref().unwrap_or(""), item, base_url);
            let word_count =
                eval_field_xpath(rule.word_count.as_deref().unwrap_or(""), item, base_url);
            out.push(SearchBook {
                name: name.unwrap_or_default(),
                author: author.unwrap_or_default(),
                book_url: resolve_url(base_url, &book_url.unwrap_or_default()),
                origin: source.book_source_url.clone(),
                cover_url: cover_url.map(|u| resolve_url(base_url, &u)),
                intro,
                kind,
                last_chapter,
                update_time,
                word_count,
                book_source_urls: None,
            });
        }

        out
    }

    fn search_books_json(
        &self,
        source: &BookSource,
        body: &str,
        base_url: &str,
        rule: &SearchRule,
        list_rule: &str,
    ) -> Vec<SearchBook> {
        let v: Value = match serde_json::from_str(body) {
            Ok(v) => v,
            Err(_) => return vec![],
        };
        let items = jsonpath::jsonpath_query_combined(&v, self.strip_mode_prefix(list_rule));
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            let name = eval_field_json(rule.name.as_deref().unwrap_or(""), &item, base_url);
            let author = eval_field_json(rule.author.as_deref().unwrap_or(""), &item, base_url);
            let book_url = eval_field_json(rule.book_url.as_deref().unwrap_or(""), &item, base_url);
            let cover_url =
                eval_field_json(rule.cover_url.as_deref().unwrap_or(""), &item, base_url);
            let intro = eval_field_json(rule.intro.as_deref().unwrap_or(""), &item, base_url);
            let kind = eval_kind_json(rule.kind.as_deref(), &item, base_url);
            let last_chapter =
                eval_field_json(rule.last_chapter.as_deref().unwrap_or(""), &item, base_url);
            let update_time =
                eval_field_json(rule.update_time.as_deref().unwrap_or(""), &item, base_url);
            let word_count =
                eval_field_json(rule.word_count.as_deref().unwrap_or(""), &item, base_url);
            let book_url = resolve_url(base_url, &book_url.unwrap_or_default());
            let cover_url = cover_url.map(|u| resolve_url(base_url, &u));
            out.push(SearchBook {
                name: name.unwrap_or_default(),
                author: author.unwrap_or_default(),
                book_url,
                origin: source.book_source_url.clone(),
                cover_url,
                intro,
                kind,
                last_chapter,
                update_time,
                word_count,
                book_source_urls: None,
            });
        }
        out
    }

    // ── 评论（章评 / 段评） ─────────────────────────────────────────────
    //
    // 与 `nextContentUrl` 同理：评论地址要先拿到**章节正文响应**才知道
    // （番茄接口的书籍 ID / 章节 ID / 版本号都在正文响应里），所以评论的
    // URL 规则一律对正文响应求值。

    /// 书源是否声明了章评规则。
    pub fn has_chapter_review_rule(&self, source: &BookSource) -> bool {
        let Some(rule) = source.rule_review.as_ref() else {
            return false;
        };
        has_rule(rule.review_url.as_deref()) && has_rule(rule.list_rule.as_deref())
    }

    /// 书源是否声明了段评规则。
    pub fn has_para_review_rule(&self, source: &BookSource) -> bool {
        let Some(rule) = source.rule_para_review.as_ref() else {
            return false;
        };
        has_rule(rule.index_url.as_deref()) && has_rule(rule.index_list_rule.as_deref())
    }

    /// 求值章评列表地址。
    pub fn chapter_review_url(
        &self,
        source: &BookSource,
        body: &str,
        base_url: &str,
        ctx: &HashMap<String, String>,
    ) -> Option<String> {
        let rule = source.rule_review.as_ref()?.review_url.clone()?;
        with_book_source(source, || self.review_url(&rule, body, base_url, ctx))
    }

    /// 求值段评概览地址。
    pub fn para_review_index_url(
        &self,
        source: &BookSource,
        body: &str,
        base_url: &str,
        ctx: &HashMap<String, String>,
    ) -> Option<String> {
        let rule = source.rule_para_review.as_ref()?.index_url.clone()?;
        with_book_source(source, || self.review_url(&rule, body, base_url, ctx))
    }

    /// 求值某一段的段评列表地址（模板里可用 `{{paraIndex}}`）。
    pub fn para_review_url(
        &self,
        source: &BookSource,
        body: &str,
        base_url: &str,
        ctx: &HashMap<String, String>,
    ) -> Option<String> {
        let rule = source.rule_para_review.as_ref()?.review_url.clone()?;
        with_book_source(source, || self.review_url(&rule, body, base_url, ctx))
    }

    /// 求值一条评论 URL 规则。
    ///
    /// 规则按模板处理：`{{$.a.b}}` 取 JSONPath、`{{表达式}}` 走 JS，
    /// `ctx` 里的键（`page` / `count` / `paraIndex` 等）可直接用
    /// `{{page}}` 引用。规则本身也可以写成纯 JSONPath。
    pub fn review_url(
        &self,
        rule: &str,
        body: &str,
        base_url: &str,
        ctx: &HashMap<String, String>,
    ) -> Option<String> {
        let rule = rule.trim();
        if rule.is_empty() {
            return None;
        }
        let text = if let Ok(v) = serde_json::from_str::<Value>(body) {
            let interpolated = interpolate_json_templates(rule, &v, base_url, ctx);
            let pure = interpolated.trim();
            if pure.starts_with('$') {
                pick_json_field(&v, Some(pure)).unwrap_or_default()
            } else {
                interpolated
            }
        } else {
            interpolate_common_templates(rule, body, base_url, ctx)
        };
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        Some(resolve_url(base_url, &drop_empty_sort_param(text)))
    }

    /// 解析章评列表。
    pub fn chapter_reviews(&self, source: &BookSource, body: &str, base_url: &str) -> ReviewPage {
        let Some(rule) = source.rule_review.as_ref() else {
            return ReviewPage::default();
        };
        let fields = ReviewFields {
            list: rule.list_rule.as_deref().unwrap_or(""),
            id: rule.id_rule.as_deref(),
            name: rule.name_rule.as_deref(),
            avatar: rule.avatar_rule.as_deref(),
            content: rule.content_rule.as_deref(),
            time: rule.post_time_rule.as_deref(),
            digg: rule.digg_rule.as_deref(),
            reply_count: rule.reply_count_rule.as_deref(),
            total: rule.total_rule.as_deref(),
            has_more: rule.has_more_rule.as_deref(),
            reply_list: rule.reply_list_rule.as_deref(),
            reply_name: rule.reply_name_rule.as_deref(),
            reply_content: rule.reply_content_rule.as_deref(),
            reply_time: rule.reply_post_time_rule.as_deref(),
            reply_to: rule.reply_to_rule.as_deref(),
            image: rule.image_rule.as_deref(),
        };
        with_book_source(source, || parse_review_page(&fields, body, base_url))
    }

    /// 解析某一段的段评列表。
    pub fn para_reviews(&self, source: &BookSource, body: &str, base_url: &str) -> ReviewPage {
        let Some(rule) = source.rule_para_review.as_ref() else {
            return ReviewPage::default();
        };
        let fields = ReviewFields {
            list: rule.list_rule.as_deref().unwrap_or(""),
            id: rule.id_rule.as_deref(),
            name: rule.name_rule.as_deref(),
            avatar: rule.avatar_rule.as_deref(),
            content: rule.content_rule.as_deref(),
            time: rule.post_time_rule.as_deref(),
            digg: rule.digg_rule.as_deref(),
            reply_count: rule.reply_count_rule.as_deref(),
            total: rule.total_rule.as_deref(),
            has_more: rule.has_more_rule.as_deref(),
            reply_list: rule.reply_list_rule.as_deref(),
            reply_name: rule.reply_name_rule.as_deref(),
            reply_content: rule.reply_content_rule.as_deref(),
            reply_time: rule.reply_post_time_rule.as_deref(),
            reply_to: rule.reply_to_rule.as_deref(),
            image: rule.image_rule.as_deref(),
        };
        with_book_source(source, || parse_review_page(&fields, body, base_url))
    }

    /// 解析段评概览：哪些段落有评论、各有多少条。
    ///
    /// 概览在多数实现里是「段号 → 该段数据」的对象（番茄的 `idea_data`
    /// 就是），因此按对象键取段号；规则若直接选成数组则按顺序编号。
    pub fn para_review_index(
        &self,
        source: &BookSource,
        body: &str,
        base_url: &str,
    ) -> Vec<ParaReviewCount> {
        let Some(rule) = source.rule_para_review.as_ref() else {
            return Vec::new();
        };
        let list_rule = rule.index_list_rule.as_deref().unwrap_or("").trim();
        if list_rule.is_empty() {
            return Vec::new();
        }
        with_book_source(source, || {
            let Ok(v) = serde_json::from_str::<Value>(body) else {
                return Vec::new();
            };
            let mut ctx = HashMap::new();
            let count_rule = rule.index_count_rule.as_deref().unwrap_or("");
            let mut paras: Vec<ParaReviewCount> = Vec::new();
            for node in jsonpath::jsonpath_query_combined(&v, strip_mode_prefix(list_rule)) {
                let entries: Vec<(i32, &Value)> = match &node {
                    // 段号按定义是「正文行号，从 0 开始」；番茄的 idea_data 里
                    // 另有 -1 这种整章聚合桶，不属于任何段落，直接丢掉。
                    Value::Object(map) => map
                        .iter()
                        .filter_map(|(key, value)| {
                            let index = key.trim().parse::<i32>().ok()?;
                            (index >= 0).then_some((index, value))
                        })
                        .collect(),
                    Value::Array(items) => items
                        .iter()
                        .enumerate()
                        .map(|(index, value)| (index as i32, value))
                        .collect(),
                    _ => Vec::new(),
                };
                for (para_index, value) in entries {
                    let count = eval_field_json_with_ctx(count_rule, value, base_url, &mut ctx)
                        .map(|text| parse_count_text(&text))
                        .unwrap_or(0);
                    if count <= 0 {
                        continue;
                    }
                    paras.push(ParaReviewCount {
                        para_index,
                        count,
                        text: String::new(),
                    });
                }
            }
            paras.sort_by_key(|p| p.para_index);
            paras
        })
    }
}

/// 删掉取值为空的 `sort=` 查询参数。
///
/// 站点对空值的处理不可预期：FQWeb 实测 `&sort=`（空串）会直接返回
/// 参数错误，而「完全不传」才走默认的最热排序。空值从来不是有意义的
/// 输入，删掉等于让站点用缺省值。模板里的 `{{sort}}` 由后端填充、
/// 一般不会为空，这里防的是书源作者手写的映射 JS 求值失败的情况。
fn drop_empty_sort_param(url: &str) -> String {
    let Some((base, query)) = url.split_once('?') else {
        return url.to_string();
    };
    let kept: Vec<&str> = query
        .split('&')
        .filter(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            !(key == "sort" && value.is_empty())
        })
        .collect();
    if kept.is_empty() {
        return base.to_string();
    }
    format!("{}?{}", base, kept.join("&"))
}

/// 评论解析所需的一组字段规则。
struct ReviewFields<'a> {
    list: &'a str,
    id: Option<&'a str>,
    name: Option<&'a str>,
    avatar: Option<&'a str>,
    content: Option<&'a str>,
    time: Option<&'a str>,
    digg: Option<&'a str>,
    reply_count: Option<&'a str>,
    total: Option<&'a str>,
    has_more: Option<&'a str>,
    reply_list: Option<&'a str>,
    reply_name: Option<&'a str>,
    reply_content: Option<&'a str>,
    reply_time: Option<&'a str>,
    reply_to: Option<&'a str>,
    image: Option<&'a str>,
}

fn has_rule(rule: Option<&str>) -> bool {
    rule.map(|s| !s.trim().is_empty()).unwrap_or(false)
}

/// 把内联回复按时间升序排（早在上、晚在下）。
///
/// 回复串的阅读顺序就是对话顺序，站点给的顺序不一定对。时间字段是站点原样
/// 返回的字符串，这里只认 Unix 秒/毫秒时间戳；只要有一条认不出来就整体保持
/// 站点顺序——宁可不排，也不要把不可比的时间混着排。
fn sort_replies_chronologically(replies: &mut Vec<ReviewReply>) {
    if replies.len() < 2 {
        return;
    }
    let keys: Vec<Option<i64>> = replies
        .iter()
        .map(|reply| reply_time_key(&reply.time))
        .collect();
    if keys.iter().any(Option::is_none) {
        return;
    }
    let mut keyed: Vec<(i64, ReviewReply)> = std::mem::take(replies)
        .into_iter()
        .zip(keys.into_iter().flatten())
        .map(|(reply, key)| (key, reply))
        .collect();
    keyed.sort_by_key(|(key, _)| *key);
    *replies = keyed.into_iter().map(|(_, reply)| reply).collect();
}

fn reply_time_key(text: &str) -> Option<i64> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.len() > 13 || !trimmed.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    trimmed.parse::<i64>().ok()
}

/// 从「216」「216 赞」「1.2万」这类文本里取出数字。
fn parse_count_text(text: &str) -> i64 {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return 0;
    }
    let digits: String = trimmed
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let Ok(base) = digits.parse::<f64>() else {
        return 0;
    };
    let value = if trimmed.contains('万') {
        base * 10_000.0
    } else {
        base
    };
    value as i64
}

fn parse_review_page(fields: &ReviewFields<'_>, body: &str, base_url: &str) -> ReviewPage {
    if fields.list.trim().is_empty() {
        return ReviewPage::default();
    }
    match serde_json::from_str::<Value>(body) {
        Ok(v) => parse_review_page_json(fields, &v, base_url),
        Err(_) => parse_review_page_html(fields, body, base_url),
    }
}

fn parse_review_page_json(fields: &ReviewFields<'_>, v: &Value, base_url: &str) -> ReviewPage {
    let mut ctx = HashMap::new();
    let (list_rule, reverse) = normalize_list_rule(fields.list);
    let nodes = jsonpath::jsonpath_query_combined(v, strip_mode_prefix(list_rule));
    let mut items = Vec::with_capacity(nodes.len());
    for node in nodes.iter() {
        let content =
            eval_field_json_with_ctx(fields.content.unwrap_or(""), node, base_url, &mut ctx)
                .unwrap_or_default();
        if content.trim().is_empty() {
            continue;
        }
        let replies = fields
            .reply_list
            .filter(|rule| has_rule(Some(rule)))
            .map(|rule| {
                jsonpath::jsonpath_query_combined(node, strip_mode_prefix(rule.trim()))
                    .iter()
                    .map(|reply| ReviewReply {
                        name: eval_field_json_with_ctx(
                            fields.reply_name.unwrap_or(""),
                            reply,
                            base_url,
                            &mut ctx,
                        )
                        .unwrap_or_default(),
                        content: eval_field_json_with_ctx(
                            fields.reply_content.unwrap_or(""),
                            reply,
                            base_url,
                            &mut ctx,
                        )
                        .unwrap_or_default(),
                        time: eval_field_json_with_ctx(
                            fields.reply_time.unwrap_or(""),
                            reply,
                            base_url,
                            &mut ctx,
                        )
                        .unwrap_or_default(),
                        reply_to: eval_field_json_with_ctx(
                            fields.reply_to.unwrap_or(""),
                            reply,
                            base_url,
                            &mut ctx,
                        )
                        .unwrap_or_default(),
                    })
                    .filter(|reply| !reply.content.trim().is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut replies = replies;
        sort_replies_chronologically(&mut replies);
        items.push(ReviewItem {
            id: eval_field_json_with_ctx(fields.id.unwrap_or(""), node, base_url, &mut ctx)
                .unwrap_or_default(),
            name: eval_field_json_with_ctx(fields.name.unwrap_or(""), node, base_url, &mut ctx)
                .unwrap_or_default(),
            avatar: eval_field_json_with_ctx(fields.avatar.unwrap_or(""), node, base_url, &mut ctx)
                .map(|url| resolve_url(base_url, &url))
                .unwrap_or_default(),
            content,
            time: eval_field_json_with_ctx(fields.time.unwrap_or(""), node, base_url, &mut ctx)
                .unwrap_or_default(),
            digg: eval_field_json_with_ctx(fields.digg.unwrap_or(""), node, base_url, &mut ctx)
                .map(|text| parse_count_text(&text))
                .unwrap_or(0),
            reply_count: eval_field_json_with_ctx(
                fields.reply_count.unwrap_or(""),
                node,
                base_url,
                &mut ctx,
            )
            .map(|text| parse_count_text(&text))
            .unwrap_or(0),
            replies,
            images: eval_image_list_json(fields.image.unwrap_or(""), node, base_url),
        });
    }
    if reverse {
        items.reverse();
    }
    // 总数与「还有下一页」取自整个响应，不是单条评论。
    let total = eval_field_json_with_ctx(fields.total.unwrap_or(""), v, base_url, &mut ctx)
        .map(|text| parse_count_text(&text))
        .unwrap_or(items.len() as i64);
    let has_more = eval_field_json_with_ctx(fields.has_more.unwrap_or(""), v, base_url, &mut ctx)
        .map(is_truthy)
        .unwrap_or(false);
    ReviewPage {
        total,
        has_more,
        page: 0,
        items,
    }
}

fn parse_review_page_html(fields: &ReviewFields<'_>, body: &str, base_url: &str) -> ReviewPage {
    let doc = html::parse_document(body);
    let doc_view = DocView::new(&doc);
    let (list_rule, reverse) = normalize_list_rule(fields.list);
    let nodes = html::select_list(&doc, strip_mode_prefix(list_rule));
    let mut ctx = HashMap::new();
    let mut items = Vec::with_capacity(nodes.len());
    for node in nodes.iter() {
        let content =
            eval_field_html_with_ctx(fields.content.unwrap_or(""), node, base_url, &mut ctx)
                .unwrap_or_default();
        if content.trim().is_empty() {
            continue;
        }
        let replies = fields
            .reply_list
            .filter(|rule| has_rule(Some(rule)))
            .map(|rule| {
                select_child_elements(node, strip_mode_prefix(rule.trim()))
                    .iter()
                    .map(|reply| ReviewReply {
                        name: eval_field_html_with_ctx(
                            fields.reply_name.unwrap_or(""),
                            reply,
                            base_url,
                            &mut ctx,
                        )
                        .unwrap_or_default(),
                        content: eval_field_html_with_ctx(
                            fields.reply_content.unwrap_or(""),
                            reply,
                            base_url,
                            &mut ctx,
                        )
                        .unwrap_or_default(),
                        time: eval_field_html_with_ctx(
                            fields.reply_time.unwrap_or(""),
                            reply,
                            base_url,
                            &mut ctx,
                        )
                        .unwrap_or_default(),
                        reply_to: eval_field_html_with_ctx(
                            fields.reply_to.unwrap_or(""),
                            reply,
                            base_url,
                            &mut ctx,
                        )
                        .unwrap_or_default(),
                    })
                    .filter(|reply| !reply.content.trim().is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut replies = replies;
        sort_replies_chronologically(&mut replies);
        items.push(ReviewItem {
            id: eval_field_html_with_ctx(fields.id.unwrap_or(""), node, base_url, &mut ctx)
                .unwrap_or_default(),
            name: eval_field_html_with_ctx(fields.name.unwrap_or(""), node, base_url, &mut ctx)
                .unwrap_or_default(),
            avatar: eval_field_html_with_ctx(fields.avatar.unwrap_or(""), node, base_url, &mut ctx)
                .map(|url| resolve_url(base_url, &url))
                .unwrap_or_default(),
            content,
            time: eval_field_html_with_ctx(fields.time.unwrap_or(""), node, base_url, &mut ctx)
                .unwrap_or_default(),
            digg: eval_field_html_with_ctx(fields.digg.unwrap_or(""), node, base_url, &mut ctx)
                .map(|text| parse_count_text(&text))
                .unwrap_or(0),
            reply_count: eval_field_html_with_ctx(
                fields.reply_count.unwrap_or(""),
                node,
                base_url,
                &mut ctx,
            )
            .map(|text| parse_count_text(&text))
            .unwrap_or(0),
            replies,
            images: eval_image_list_html(fields.image.unwrap_or(""), node, base_url),
        });
    }
    if reverse {
        items.reverse();
    }
    let total =
        eval_field_html_doc_with_ctx(fields.total.unwrap_or(""), &doc_view, base_url, &mut ctx)
            .map(|text| parse_count_text(&text))
            .unwrap_or(items.len() as i64);
    let has_more =
        eval_field_html_doc_with_ctx(fields.has_more.unwrap_or(""), &doc_view, base_url, &mut ctx)
            .map(is_truthy)
            .unwrap_or(false);
    ReviewPage {
        total,
        has_more,
        page: 0,
        items,
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

fn parse_book_info_html(
    source: &BookSource,
    body: &str,
    base_url: &str,
    rule: &BookInfoRule,
    book_url: &str,
    ctx: &mut HashMap<String, String>,
) -> Book {
    let doc = html::parse_document(body);
    let doc_view = DocView::new(&doc);

    // Execute init rule if present
    if let Some(init) = &rule.init {
        let _ = eval_field_html_doc_with_ctx(init, &doc_view, base_url, ctx);
    }

    let name = rule
        .name
        .as_ref()
        .and_then(|r| eval_field_html_doc_with_ctx(r, &doc_view, base_url, ctx))
        .unwrap_or_default();
    let author = rule
        .author
        .as_ref()
        .and_then(|r| eval_field_html_doc_with_ctx(r, &doc_view, base_url, ctx))
        .unwrap_or_default();
    let intro = rule
        .intro
        .as_ref()
        .and_then(|r| eval_field_html_doc_with_ctx(r, &doc_view, base_url, ctx));
    let kind = eval_kind_html_doc(rule.kind.as_deref(), &doc, base_url);
    let last_chapter = rule
        .last_chapter
        .as_ref()
        .and_then(|r| eval_field_html_doc_with_ctx(r, &doc_view, base_url, ctx));
    let update_time = rule
        .update_time
        .as_ref()
        .and_then(|r| eval_field_html_doc_with_ctx(r, &doc_view, base_url, ctx));
    let cover_url = rule
        .cover_url
        .as_ref()
        .and_then(|r| eval_field_html_doc_with_ctx(r, &doc_view, base_url, ctx))
        .map(|u| resolve_url(base_url, &u));
    let word_count = rule
        .word_count
        .as_ref()
        .and_then(|r| eval_field_html_doc_with_ctx(r, &doc_view, base_url, ctx));
    let toc_url = rule
        .toc_url
        .as_ref()
        .and_then(|r| eval_field_html_doc_with_ctx(r, &doc_view, base_url, ctx))
        .map(|u| resolve_url(base_url, &u));
    let can_re_name = rule
        .can_re_name
        .as_ref()
        .and_then(|r| eval_field_html_doc_with_ctx(r, &doc_view, base_url, ctx));
    let download_urls = rule
        .download_urls
        .as_ref()
        .and_then(|r| eval_field_html_doc_with_ctx(r, &doc_view, base_url, ctx));

    let final_toc_url = toc_url.or_else(|| Some(book_url.to_string()));

    Book {
        name,
        author,
        book_url: book_url.to_string(),
        origin: source.book_source_url.clone(),
        origin_name: Some(source.book_source_name.clone()),
        cover_url,
        toc_url: final_toc_url,
        intro,
        latest_chapter_title: last_chapter,
        word_count,
        info_html: None,
        toc_html: None,
        kind,
        update_time,
        can_re_name,
        download_urls,
        ..Default::default()
    }
}

fn parse_book_info_xpath(
    source: &BookSource,
    body: &str,
    base_url: &str,
    rule: &BookInfoRule,
    book_url: &str,
    ctx: &mut HashMap<String, String>,
) -> Book {
    let package = match html::parse_xml_or_html(body) {
        Some(p) => p,
        None => return parse_book_info_html(source, body, base_url, rule, book_url, ctx),
    };
    let document = package.as_document();
    let scope = select_xpath_scope(
        sxd_xpath::nodeset::Node::Root(document.root()),
        rule.init.as_deref(),
    );

    let name = eval_field_xpath_with_ctx(rule.name.as_deref().unwrap_or(""), scope, base_url, ctx)
        .unwrap_or_default();
    let author =
        eval_field_xpath_with_ctx(rule.author.as_deref().unwrap_or(""), scope, base_url, ctx)
            .unwrap_or_default();
    let intro =
        eval_field_xpath_with_ctx(rule.intro.as_deref().unwrap_or(""), scope, base_url, ctx);
    let kind = eval_kind_xpath(rule.kind.as_deref(), scope, base_url);
    let last_chapter = eval_field_xpath_with_ctx(
        rule.last_chapter.as_deref().unwrap_or(""),
        scope,
        base_url,
        ctx,
    );
    let update_time = eval_field_xpath_with_ctx(
        rule.update_time.as_deref().unwrap_or(""),
        scope,
        base_url,
        ctx,
    );
    let cover_url = eval_field_xpath_with_ctx(
        rule.cover_url.as_deref().unwrap_or(""),
        scope,
        base_url,
        ctx,
    )
    .map(|u| resolve_url(base_url, &u));
    let word_count = eval_field_xpath_with_ctx(
        rule.word_count.as_deref().unwrap_or(""),
        scope,
        base_url,
        ctx,
    );
    let toc_url =
        eval_field_xpath_with_ctx(rule.toc_url.as_deref().unwrap_or(""), scope, base_url, ctx)
            .map(|u| resolve_url(base_url, &u));
    let can_re_name = eval_field_xpath_with_ctx(
        rule.can_re_name.as_deref().unwrap_or(""),
        scope,
        base_url,
        ctx,
    );
    let download_urls = eval_field_xpath_with_ctx(
        rule.download_urls.as_deref().unwrap_or(""),
        scope,
        base_url,
        ctx,
    );

    Book {
        name,
        author,
        book_url: book_url.to_string(),
        origin: source.book_source_url.clone(),
        origin_name: Some(source.book_source_name.clone()),
        cover_url,
        toc_url: toc_url.or_else(|| Some(book_url.to_string())),
        intro,
        latest_chapter_title: last_chapter,
        word_count,
        info_html: None,
        toc_html: None,
        kind,
        update_time,
        can_re_name,
        download_urls,
        ..Default::default()
    }
}

fn parse_book_info_json(
    source: &BookSource,
    v: &Value,
    base_url: &str,
    rule: &BookInfoRule,
    book_url: &str,
    ctx: &mut HashMap<String, String>,
) -> Book {
    let scope = select_json_scope(v, rule.init.as_deref(), base_url, ctx);
    let name = eval_field_json_with_ctx(rule.name.as_deref().unwrap_or(""), &scope, base_url, ctx)
        .unwrap_or_default();
    let author =
        eval_field_json_with_ctx(rule.author.as_deref().unwrap_or(""), &scope, base_url, ctx)
            .unwrap_or_default();
    let intro =
        eval_field_json_with_ctx(rule.intro.as_deref().unwrap_or(""), &scope, base_url, ctx);
    let kind = eval_kind_json(rule.kind.as_deref(), &scope, base_url);
    let last_chapter = eval_field_json_with_ctx(
        rule.last_chapter.as_deref().unwrap_or(""),
        &scope,
        base_url,
        ctx,
    );
    let update_time = eval_field_json_with_ctx(
        rule.update_time.as_deref().unwrap_or(""),
        &scope,
        base_url,
        ctx,
    );
    let cover_url = eval_field_json_with_ctx(
        rule.cover_url.as_deref().unwrap_or(""),
        &scope,
        base_url,
        ctx,
    )
    .map(|u| resolve_url(base_url, &u));
    let word_count = eval_field_json_with_ctx(
        rule.word_count.as_deref().unwrap_or(""),
        &scope,
        base_url,
        ctx,
    );
    let toc_url =
        eval_field_json_with_ctx(rule.toc_url.as_deref().unwrap_or(""), &scope, base_url, ctx)
            .map(|u| resolve_url(base_url, &u));
    let can_re_name = eval_field_json_with_ctx(
        rule.can_re_name.as_deref().unwrap_or(""),
        &scope,
        base_url,
        ctx,
    );
    let download_urls = eval_field_json_with_ctx(
        rule.download_urls.as_deref().unwrap_or(""),
        &scope,
        base_url,
        ctx,
    );
    Book {
        name,
        author,
        book_url: book_url.to_string(),
        origin: source.book_source_url.clone(),
        origin_name: Some(source.book_source_name.clone()),
        cover_url,
        toc_url: toc_url.or_else(|| Some(book_url.to_string())),
        intro,
        latest_chapter_title: last_chapter,
        word_count,
        info_html: None,
        toc_html: None,
        kind,
        update_time,
        can_re_name,
        download_urls,
        ..Default::default()
    }
}

fn parse_chapter_list_html(
    body: &str,
    base_url: &str,
    rule: &TocRule,
    list_sel: &str,
    ctx: &mut HashMap<String, String>,
) -> (Vec<BookChapter>, Vec<String>) {
    if list_sel.trim().is_empty() {
        return (vec![], vec![]);
    }
    let doc = html::parse_document(body);
    let doc_view = DocView::new(&doc);

    // Execute init rule if present
    if let Some(init) = &rule.init {
        let _ = eval_field_html_doc_with_ctx(init, &doc_view, base_url, ctx);
    }

    let items = html::select_list(&doc, strip_mode_prefix(list_sel));

    // Use a set to deduplicate chapters by URL
    let mut seen_urls = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(items.len());

    for el in items {
        let title = rule
            .chapter_name
            .as_ref()
            .and_then(|r| eval_field_html_with_ctx(r, &el, base_url, ctx))
            .unwrap_or_default();
        let url = rule
            .chapter_url
            .as_ref()
            .and_then(|r| eval_field_html_with_ctx(r, &el, base_url, ctx))
            .unwrap_or_default();
        let tag = rule
            .update_time
            .as_ref()
            .and_then(|r| eval_field_html_with_ctx(r, &el, base_url, ctx));
        let is_volume = rule
            .is_volume
            .as_ref()
            .and_then(|r| eval_field_html_with_ctx(r, &el, base_url, ctx))
            .map(is_truthy)
            .unwrap_or(false);
        let is_vip = rule
            .is_vip
            .as_ref()
            .and_then(|r| eval_field_html_with_ctx(r, &el, base_url, ctx))
            .map(is_truthy)
            .unwrap_or(false);
        let is_pay = rule
            .is_pay
            .as_ref()
            .and_then(|r| eval_field_html_with_ctx(r, &el, base_url, ctx))
            .map(is_truthy)
            .unwrap_or(false);
        let url_abs = finalize_chapter_url(base_url, &url, &title, is_volume, out.len());

        // Skip duplicate chapters (same URL)
        if seen_urls.contains(&url_abs) {
            continue;
        }

        seen_urls.insert(url_abs.clone());

        out.push(BookChapter {
            title,
            url: url_abs,
            index: out.len() as i32,
            tag,
            is_vip,
            is_pay,
            is_volume,
            ..Default::default()
        });
    }

    // Extract next_toc_url(s)
    let rule_str = rule.next_toc_url.as_deref().unwrap_or("");
    let raw_urls: Vec<String> = html::select_text_list(&doc, rule_str);
    let next_urls: Vec<String> = raw_urls
        .into_iter()
        .filter(|u| !u.is_empty())
        .map(|u| resolve_url(base_url, &u))
        .collect();

    (out, next_urls)
}

fn parse_chapter_list_xpath(
    body: &str,
    base_url: &str,
    rule: &TocRule,
    list_rule: &str,
    ctx: &mut HashMap<String, String>,
) -> (Vec<BookChapter>, Vec<String>) {
    let package = match html::parse_xml_or_html(body) {
        Some(p) => p,
        None => return parse_chapter_list_html(body, base_url, rule, list_rule, ctx),
    };
    let document = package.as_document();
    let scope = select_xpath_scope(
        sxd_xpath::nodeset::Node::Root(document.root()),
        rule.init.as_deref(),
    );
    let items = xpath_select_nodes(scope, strip_mode_prefix(list_rule));

    let mut seen_urls = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let title = eval_field_xpath_with_ctx(
            rule.chapter_name.as_deref().unwrap_or(""),
            item,
            base_url,
            ctx,
        )
        .unwrap_or_default();
        let url = eval_field_xpath_with_ctx(
            rule.chapter_url.as_deref().unwrap_or(""),
            item,
            base_url,
            ctx,
        )
        .unwrap_or_default();
        let tag = eval_field_xpath_with_ctx(
            rule.update_time.as_deref().unwrap_or(""),
            item,
            base_url,
            ctx,
        );
        let is_volume =
            eval_field_xpath_with_ctx(rule.is_volume.as_deref().unwrap_or(""), item, base_url, ctx)
                .map(is_truthy)
                .unwrap_or(false);
        let is_vip =
            eval_field_xpath_with_ctx(rule.is_vip.as_deref().unwrap_or(""), item, base_url, ctx)
                .map(is_truthy)
                .unwrap_or(false);
        let is_pay =
            eval_field_xpath_with_ctx(rule.is_pay.as_deref().unwrap_or(""), item, base_url, ctx)
                .map(is_truthy)
                .unwrap_or(false);
        let url_abs = finalize_chapter_url(base_url, &url, &title, is_volume, out.len());
        if seen_urls.contains(&url_abs) {
            continue;
        }
        seen_urls.insert(url_abs.clone());
        out.push(BookChapter {
            title,
            url: url_abs,
            index: out.len() as i32,
            tag,
            is_vip,
            is_pay,
            is_volume,
            ..Default::default()
        });
    }

    let next_urls = rule
        .next_toc_url
        .as_deref()
        .map(|xpath| xpath_eval_strings(scope, xpath))
        .unwrap_or_default()
        .into_iter()
        .filter(|u| !u.is_empty())
        .map(|u| resolve_url(base_url, &u))
        .collect();

    (out, next_urls)
}

fn parse_chapter_list_json(
    body: &str,
    base_url: &str,
    rule: &TocRule,
    list_rule: &str,
    ctx: &mut HashMap<String, String>,
) -> (Vec<BookChapter>, Vec<String>) {
    let v: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return (vec![], vec![]),
    };
    let scope = select_json_scope(&v, rule.init.as_deref(), base_url, ctx);
    let items = jsonpath::jsonpath_query_combined(&scope, strip_mode_prefix(list_rule));

    let mut seen_urls = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let title = eval_field_json_with_ctx(
            rule.chapter_name.as_deref().unwrap_or(""),
            &item,
            base_url,
            ctx,
        )
        .unwrap_or_default();
        let url = eval_field_json_with_ctx(
            rule.chapter_url.as_deref().unwrap_or(""),
            &item,
            base_url,
            ctx,
        )
        .unwrap_or_default();
        let tag = eval_field_json_with_ctx(
            rule.update_time.as_deref().unwrap_or(""),
            &item,
            base_url,
            ctx,
        );
        let is_volume = eval_field_json_with_ctx(
            rule.is_volume.as_deref().unwrap_or(""),
            &item,
            base_url,
            ctx,
        )
        .map(is_truthy)
        .unwrap_or(false);
        let is_vip =
            eval_field_json_with_ctx(rule.is_vip.as_deref().unwrap_or(""), &item, base_url, ctx)
                .map(is_truthy)
                .unwrap_or(false);
        let is_pay =
            eval_field_json_with_ctx(rule.is_pay.as_deref().unwrap_or(""), &item, base_url, ctx)
                .map(is_truthy)
                .unwrap_or(false);
        let url_abs = finalize_chapter_url(base_url, &url, &title, is_volume, out.len());

        if seen_urls.contains(&url_abs) {
            continue;
        }
        seen_urls.insert(url_abs.clone());

        out.push(BookChapter {
            title,
            url: url_abs,
            index: out.len() as i32,
            tag,
            is_vip,
            is_pay,
            is_volume,
            ..Default::default()
        });
    }

    let next_urls: Vec<String> = rule
        .next_toc_url
        .as_ref()
        .map(|r| jsonpath::jsonpath_query_combined(&scope, r))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .filter(|u| !u.is_empty())
        .map(|u| resolve_url(base_url, &u))
        .collect();

    (out, next_urls)
}

fn select_json_scope(
    v: &Value,
    init_rule: Option<&str>,
    base_url: &str,
    ctx: &mut HashMap<String, String>,
) -> Value {
    let Some(init_rule) = init_rule.map(str::trim).filter(|s| !s.is_empty()) else {
        return v.clone();
    };

    if let Some(res) = try_put_get_json(init_rule, v, base_url, ctx) {
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

fn pick_json_field(v: &Value, rule: Option<&str>) -> Option<String> {
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

fn resolve_url(base: &str, url: &str) -> String {
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

fn interpolate_json_templates(
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
        match eval_js_with_bindings(
            expr,
            &serde_json::to_string(v).unwrap_or_default(),
            base_url,
            &bindings,
        ) {
            Ok(res) => res,
            Err(_) => String::new(),
        }
    })
    .into_owned()
}

fn interpolate_common_templates(
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
fn extract_js(rule: &str) -> (&str, Option<&str>, Option<&str>) {
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

fn eval_kind_json(rule: Option<&str>, v: &Value, base_url: &str) -> Option<String> {
    let rule = rule.unwrap_or("").trim();
    let values = if is_plain_selector_rule(rule) {
        json_kind_values(v, strip_mode_prefix(rule))
    } else {
        Vec::new()
    };
    join_multi_values(values, || eval_field_json(rule, v, base_url))
}

fn eval_kind_xpath(
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

fn eval_kind_html_doc(rule: Option<&str>, doc: &scraper::Html, base_url: &str) -> Option<String> {
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

fn eval_kind_html_element(
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

fn eval_field_html(rule: &str, el: &scraper::ElementRef, base_url: &str) -> Option<String> {
    eval_field_html_with_ctx(rule, el, base_url, &mut HashMap::new())
}

fn eval_field_html_with_ctx(
    rule: &str,
    el: &scraper::ElementRef,
    base_url: &str,
    ctx: &mut HashMap<String, String>,
) -> Option<String> {
    // Handle mode forcing prefixes
    let rule = rule.trim();
    if rule.starts_with("@css:") {
        let pure = &rule[5..];
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
    if let Some(res) = try_put_get_html(rule, el, base_url, ctx) {
        return Some(res);
    }

    let input = html::descendant_text(el);
    let interpolated_rule = interpolate_common_templates(rule, &input, base_url, ctx);
    let had_templates = interpolated_rule != rule;
    let (pure_rule, regex_part) = split_legado_regex(&interpolated_rule);
    let (pure, js, tail) = extract_js(&pure_rule);

    let mut text = if pure.is_empty() {
        "".to_string()
    } else {
        html::select_text_from_element(el, pure).unwrap_or_default()
    };
    if text.is_empty() && had_templates && !pure.is_empty() {
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

fn eval_field_html_doc_with_ctx(
    rule: &str,
    doc_view: &DocView<'_>,
    base_url: &str,
    ctx: &mut HashMap<String, String>,
) -> Option<String> {
    // Handle mode forcing prefixes
    let rule = rule.trim();
    if rule.starts_with("@css:") {
        let pure = &rule[5..];
        return eval_field_html_doc_with_ctx(pure, doc_view, base_url, ctx);
    }
    if rule.starts_with("@xpath:") {
        let pure = &rule[7..];
        return html::select_xpath(doc_view.html(), pure).first().cloned();
    }

    if let Some(res) = try_put_get_html_doc(rule, doc_view, base_url, ctx) {
        return Some(res);
    }

    let interpolated_rule = interpolate_common_templates(rule, doc_view.html(), base_url, ctx);
    let had_templates = interpolated_rule != rule;
    let (pure, js, tail) = extract_js(&interpolated_rule);
    let mut text = if pure.is_empty() {
        "".to_string()
    } else {
        html::select_text(doc_view.doc(), pure).unwrap_or_default()
    };
    if text.is_empty() && had_templates && !pure.is_empty() {
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

    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn eval_field_json(rule: &str, v: &Value, base_url: &str) -> Option<String> {
    eval_field_json_with_ctx(rule, v, base_url, &mut HashMap::new())
}

fn eval_field_xpath(
    rule: &str,
    node: sxd_xpath::nodeset::Node<'_>,
    base_url: &str,
) -> Option<String> {
    eval_field_xpath_with_ctx(rule, node, base_url, &mut HashMap::new())
}

fn eval_field_xpath_with_ctx(
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
    if let Some(res) = try_put_get_xpath(rule, node, base_url, ctx) {
        return Some(res);
    }

    let interpolated_rule = interpolate_common_templates(rule, &node.string_value(), base_url, ctx);
    let had_templates = interpolated_rule != rule;
    let (pure_rule, regex_part) = split_legado_regex(&interpolated_rule);
    let (pure, js, tail) = extract_js(&pure_rule);
    let mut text = if pure.trim().is_empty() {
        node.string_value()
    } else {
        xpath_eval_strings(node, pure)
            .into_iter()
            .next()
            .unwrap_or_default()
    };
    if text.is_empty() && had_templates && !pure.is_empty() {
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

fn select_xpath_scope<'a>(
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

fn xpath_select_nodes<'a>(
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
    match XPathFactory::new().build(xpath) {
        Ok(Some(expr)) => match expr.evaluate(&context, node) {
            Ok(XPathValue::Nodeset(ns)) => ns.document_order(),
            _ => vec![],
        },
        _ => vec![],
    }
}

fn xpath_eval_strings(node: sxd_xpath::nodeset::Node<'_>, xpath: &str) -> Vec<String> {
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
    match XPathFactory::new().build(xpath) {
        Ok(Some(expr)) => match expr.evaluate(&context, node) {
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

fn eval_field_json_with_ctx(
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

    if let Some(res) = try_put_get_json(rule, v, base_url, ctx) {
        return Some(res);
    }

    let interpolated_rule = interpolate_json_templates(rule, v, base_url, ctx);
    let (pure_rule, regex_part) = split_legado_regex(&interpolated_rule);
    let (pure, js, tail) = extract_js(&pure_rule);

    let mut text = if pure.is_empty() {
        "".to_string()
    } else if pure.contains("{{") && pure.contains("}}") {
        pure.to_string()
    } else if pure.contains('/')
        || pure.contains('?')
        || pure.contains('&')
        || pure.contains('=')
        || pure.contains(',')
    {
        pure.to_string()
    } else {
        pick_json_field(v, Some(pure)).unwrap_or_default()
    };

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

fn try_put_get_html(
    rule: &str,
    el: &scraper::ElementRef,
    base_url: &str,
    ctx: &mut HashMap<String, String>,
) -> Option<String> {
    if let Some(content) = rule.strip_prefix("@put:") {
        let content = content.trim();
        if content.starts_with('{') && content.ends_with('}') {
            for (key, val_rule) in split_put_map(&content[1..content.len() - 1]) {
                let val =
                    eval_field_html_with_ctx(&val_rule, el, base_url, ctx).unwrap_or_default();
                ctx.insert(key, val);
            }
        }
        return Some("".to_string());
    }
    if rule.starts_with("@get:") {
        let content = &rule[5..];
        if content.starts_with('{') && content.ends_with('}') {
            let key = &content[1..content.len() - 1].trim();
            return ctx.get(*key).cloned();
        }
    }
    None
}

fn try_put_get_html_doc(
    rule: &str,
    doc_view: &DocView<'_>,
    base_url: &str,
    ctx: &mut HashMap<String, String>,
) -> Option<String> {
    if let Some(content) = rule.strip_prefix("@put:") {
        let content = content.trim();
        if content.starts_with('{') && content.ends_with('}') {
            for (key, val_rule) in split_put_map(&content[1..content.len() - 1]) {
                let val = eval_field_html_doc_with_ctx(&val_rule, doc_view, base_url, ctx)
                    .unwrap_or_default();
                ctx.insert(key, val);
            }
        }
        return Some("".to_string());
    }
    if rule.starts_with("@get:") {
        let content = &rule[5..];
        if content.starts_with('{') && content.ends_with('}') {
            let key = &content[1..content.len() - 1].trim();
            return ctx.get(*key).cloned();
        }
    }
    None
}

fn try_put_get_json(
    rule: &str,
    v: &Value,
    base_url: &str,
    ctx: &mut HashMap<String, String>,
) -> Option<String> {
    if let Some(content) = rule.strip_prefix("@put:") {
        let content = content.trim();
        if content.starts_with('{') && content.ends_with('}') {
            for (key, val_rule) in split_put_map(&content[1..content.len() - 1]) {
                let val = eval_field_json_with_ctx(&val_rule, v, base_url, ctx).unwrap_or_default();
                ctx.insert(key, val);
            }
        }
        return Some("".to_string());
    }
    if rule.starts_with("@get:") {
        let content = &rule[5..];
        if content.starts_with('{') && content.ends_with('}') {
            let key = &content[1..content.len() - 1].trim();
            return ctx.get(*key).cloned();
        }
    }
    None
}

fn try_put_get_xpath(
    rule: &str,
    node: sxd_xpath::nodeset::Node<'_>,
    base_url: &str,
    ctx: &mut HashMap<String, String>,
) -> Option<String> {
    if let Some(content) = rule.strip_prefix("@put:") {
        let content = content.trim();
        if content.starts_with('{') && content.ends_with('}') {
            for (key, val_rule) in split_put_map(&content[1..content.len() - 1]) {
                let val =
                    eval_field_xpath_with_ctx(&val_rule, node, base_url, ctx).unwrap_or_default();
                ctx.insert(key, val);
            }
        }
        return Some(String::new());
    }
    if rule.starts_with("@get:") {
        let content = &rule[5..];
        if content.starts_with('{') && content.ends_with('}') {
            let key = &content[1..content.len() - 1].trim();
            return ctx.get(*key).cloned();
        }
    }
    None
}

fn split_legado_regex(rule: &str) -> (String, Option<&str>) {
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
fn apply_legado_regex(text: &str, regex_part: &str) -> String {
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

fn normalize_list_rule(rule: &str) -> (&str, bool) {
    let rule = rule.trim();
    if let Some(rest) = rule.strip_prefix('-') {
        return (rest.trim(), true);
    }
    if let Some(rest) = rule.strip_prefix('+') {
        return (rest.trim(), false);
    }
    (rule, false)
}

fn strip_mode_prefix(rule: &str) -> &str {
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

fn strip_js_rule(rule: &str) -> &str {
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

fn prepare_toc_body(body: &str, base_url: &str, rule: &TocRule) -> String {
    let Some(script) = rule
        .pre_update_js
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    else {
        return body.to_string();
    };
    match eval_js(strip_js_rule(script), body, base_url) {
        Ok(result) if !result.trim().is_empty() => result,
        _ => body.to_string(),
    }
}

fn apply_toc_format_js(chapters: &mut [BookChapter], format_js: Option<&str>, base_url: &str) {
    let Some(script) = format_js.filter(|s| !s.trim().is_empty()) else {
        return;
    };
    let script = strip_js_rule(script);
    // 规格 §15：`gInt` 初始 0，同一轮 formatJs 的多章之间复用
    let mut g_int = 0i64;
    for (index, chapter) in chapters.iter_mut().enumerate() {
        let mut bindings = HashMap::new();
        bindings.insert("index".to_string(), json!(index + 1));
        bindings.insert("gInt".to_string(), json!(g_int));
        bindings.insert("title".to_string(), json!(chapter.title.clone()));
        bindings.insert(
            "chapter".to_string(),
            serde_json::to_value(&*chapter).unwrap_or_else(|_| json!({})),
        );
        if let Ok((result, globals)) = eval_js_with_bindings_and_globals(
            script,
            &chapter.title,
            base_url,
            &bindings,
            &["gInt"],
        ) {
            if let Some(value) = globals.get("gInt").and_then(global_as_i64) {
                g_int = value;
            }
            if !result.trim().is_empty() {
                chapter.title = result;
            }
        }
    }
}

fn parse_js_output_items(output: &str) -> Option<Vec<Value>> {
    let value = serde_json::from_str::<Value>(output.trim()).ok()?;
    match value {
        Value::Array(items) => Some(items),
        Value::Object(_) => Some(vec![value]),
        _ => None,
    }
}

fn search_book_from_book(book: Book) -> Option<SearchBook> {
    if book.name.trim().is_empty() {
        return None;
    }
    Some(SearchBook {
        name: book.name,
        author: book.author,
        book_url: book.book_url,
        origin: book.origin,
        cover_url: book.cover_url,
        intro: book.intro,
        kind: book.kind,
        last_chapter: book.latest_chapter_title,
        update_time: book.update_time,
        word_count: book.word_count,
        book_source_urls: None,
    })
}

fn build_search_book_from_json(
    source: &BookSource,
    item: &Value,
    base_url: &str,
    rule: &SearchRule,
) -> Option<SearchBook> {
    let mut ctx = HashMap::new();
    let name =
        eval_field_json_with_ctx(rule.name.as_deref().unwrap_or(""), item, base_url, &mut ctx)
            .unwrap_or_default();
    if name.is_empty() {
        return None;
    }
    let author = eval_field_json_with_ctx(
        rule.author.as_deref().unwrap_or(""),
        item,
        base_url,
        &mut ctx,
    )
    .unwrap_or_default();
    let book_url = eval_field_json_with_ctx(
        rule.book_url.as_deref().unwrap_or(""),
        item,
        base_url,
        &mut ctx,
    )
    .unwrap_or_default();
    let cover_url = eval_field_json_with_ctx(
        rule.cover_url.as_deref().unwrap_or(""),
        item,
        base_url,
        &mut ctx,
    )
    .map(|u| resolve_url(base_url, &u));
    let intro = eval_field_json_with_ctx(
        rule.intro.as_deref().unwrap_or(""),
        item,
        base_url,
        &mut ctx,
    );
    let kind = eval_kind_json(rule.kind.as_deref(), item, base_url);
    let last_chapter = eval_field_json_with_ctx(
        rule.last_chapter.as_deref().unwrap_or(""),
        item,
        base_url,
        &mut ctx,
    );
    let update_time = eval_field_json_with_ctx(
        rule.update_time.as_deref().unwrap_or(""),
        item,
        base_url,
        &mut ctx,
    );
    let word_count = eval_field_json_with_ctx(
        rule.word_count.as_deref().unwrap_or(""),
        item,
        base_url,
        &mut ctx,
    );
    Some(SearchBook {
        name,
        author,
        book_url: resolve_url(base_url, &book_url),
        origin: source.book_source_url.clone(),
        cover_url,
        intro,
        kind,
        last_chapter,
        update_time,
        word_count,
        book_source_urls: None,
    })
}

fn build_chapter_from_json(
    item: &Value,
    base_url: &str,
    rule: &TocRule,
    ctx: &mut HashMap<String, String>,
    index: usize,
) -> Option<BookChapter> {
    let title = eval_field_json_with_ctx(
        rule.chapter_name.as_deref().unwrap_or(""),
        item,
        base_url,
        ctx,
    )
    .unwrap_or_default();
    if title.is_empty() {
        return None;
    }
    let raw_url = eval_field_json_with_ctx(
        rule.chapter_url.as_deref().unwrap_or(""),
        item,
        base_url,
        ctx,
    )
    .unwrap_or_default();
    let tag = eval_field_json_with_ctx(
        rule.update_time.as_deref().unwrap_or(""),
        item,
        base_url,
        ctx,
    );
    let is_volume =
        eval_field_json_with_ctx(rule.is_volume.as_deref().unwrap_or(""), item, base_url, ctx)
            .map(is_truthy)
            .unwrap_or(false);
    let is_vip =
        eval_field_json_with_ctx(rule.is_vip.as_deref().unwrap_or(""), item, base_url, ctx)
            .map(is_truthy)
            .unwrap_or(false);
    let is_pay =
        eval_field_json_with_ctx(rule.is_pay.as_deref().unwrap_or(""), item, base_url, ctx)
            .map(is_truthy)
            .unwrap_or(false);
    Some(BookChapter {
        title: title.clone(),
        url: finalize_chapter_url(base_url, &raw_url, &title, is_volume, index),
        index: index as i32,
        tag,
        is_vip,
        is_pay,
        is_volume,
    })
}

/// 列表正则求值（规格 §9.3）：`&&` 分段逐级下钻，前一段的所有完整匹配串拼成文本喂给下一段；
/// 最后一段的每个匹配产出一组捕获组（含 group 0），供字段规则的 `$n` 引用。
fn regex_list_captures(pattern: &str, body: &str) -> Vec<Vec<String>> {
    let split = crate::parser::rule_analyzer::split_top_level(pattern, &["&&"]);
    let stages: Vec<&str> = split.parts.iter().map(|part| part.trim()).collect();
    if stages.is_empty() || stages.iter().all(|stage| stage.is_empty()) {
        return vec![];
    }

    let mut texts = vec![body.to_string()];
    for (index, stage) in stages.iter().enumerate() {
        let Some(re) = crate::util::text::compiled_regex(stage) else {
            return vec![];
        };
        let last = index + 1 == stages.len();
        let mut groups_out = Vec::new();
        let mut next_texts = Vec::new();
        for text in &texts {
            for captures in re.captures_iter(text) {
                if last {
                    groups_out.push(
                        (0..captures.len())
                            .map(|i| {
                                captures
                                    .get(i)
                                    .map(|m| m.as_str().to_string())
                                    .unwrap_or_default()
                            })
                            .collect(),
                    );
                } else {
                    next_texts.push(
                        captures
                            .get(0)
                            .map(|m| m.as_str().to_string())
                            .unwrap_or_default(),
                    );
                }
            }
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

fn capture_rule_value(rule: Option<&str>, groups: &[String]) -> Option<String> {
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

fn finalize_chapter_url(
    base_url: &str,
    raw_url: &str,
    title: &str,
    is_volume: bool,
    index: usize,
) -> String {
    if !raw_url.trim().is_empty() {
        return resolve_url(base_url, raw_url);
    }
    if is_volume {
        return format!("{}{}", title, index);
    }
    base_url.to_string()
}

/// JS 全局变量的数值读取：QuickJS 的自增/浮点结果表示为浮点，整数结果也要接受。
fn global_as_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_f64().map(|number| number as i64))
}

/// 搜索结果去重保序（规格 §13.4 第 10 步），键与多书源合并一致（书名|作者）。
fn dedupe_books(books: Vec<SearchBook>) -> Vec<SearchBook> {
    let mut seen = std::collections::HashSet::new();
    books
        .into_iter()
        .filter(|book| seen.insert(book.merge_key()))
        .collect()
}

/// 书源 `bookUrlPattern` 是否命中该 URL（规格 §13.4：命中说明当前页就是详情页）。
///
/// 空值与 `NONE` 都表示不参与 URL 匹配。
fn book_url_pattern_matches(source: &BookSource, url: &str) -> bool {
    let Some(pattern) = source
        .book_url_pattern
        .as_deref()
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty() && !pattern.eq_ignore_ascii_case("none"))
    else {
        return false;
    };
    crate::util::text::compiled_regex(pattern)
        .map(|re| re.is_match(url))
        .unwrap_or(false)
}

fn is_truthy(value: String) -> bool {
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

/// Extract chapter number from title
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::book_source::BookSource;
    use crate::model::rule::{BookInfoRule, ParaReviewRule, ReviewRule, SearchRule, TocRule};

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
    fn test_search_detail_fallback_uses_book_info_rules() {
        let engine = RuleEngine::new().unwrap();
        let source = BookSource {
            book_source_name: "Test".to_string(),
            book_source_url: "https://source.example".to_string(),
            rule_search: Some(SearchRule {
                book_list: Some(String::new()),
                ..Default::default()
            }),
            rule_book_info: Some(BookInfoRule {
                name: Some(".name@text".to_string()),
                author: Some(".author@text".to_string()),
                intro: Some(".intro@text".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let body = r#"
            <div class="name">Fallback Book</div>
            <div class="author">Fallback Author</div>
            <div class="intro">Fallback Intro</div>
        "#;

        let results = engine.search_books(&source, body, "https://books.example/detail/1");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "Fallback Book");
        assert_eq!(results[0].author, "Fallback Author");
        assert_eq!(results[0].intro.as_deref(), Some("Fallback Intro"));
    }

    #[test]
    fn test_search_books_regex_list() {
        let engine = RuleEngine::new().unwrap();
        let source = BookSource {
            book_source_name: "Regex".to_string(),
            book_source_url: "https://source.example".to_string(),
            rule_search: Some(SearchRule {
                book_list: Some(r#":<a href="([^"]+)">([^<]+)</a>"#.to_string()),
                name: Some("$2".to_string()),
                book_url: Some("$1".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let body = r#"<a href="/book/1">One</a><a href="/book/2">Two</a>"#;

        let results = engine.search_books(&source, body, "https://books.example");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].name, "One");
        assert_eq!(results[0].book_url, "https://books.example/book/1");
        assert_eq!(results[1].name, "Two");
    }

    #[test]
    fn test_jsonpath_combinators_element_fields_and_kind_multi_value() {
        let engine = RuleEngine::new().unwrap();
        let source = BookSource {
            book_source_name: "Json".to_string(),
            book_source_url: "https://source.example".to_string(),
            rule_search: Some(SearchRule {
                // 列表规则用 `||`：前一条命中即止
                book_list: Some("$.missing[*] || $.data[*]".to_string()),
                // 元素级字段组合符 `&&`：两条结果拼接
                name: Some("$.name && $.suffix".to_string()),
                author: Some("@json:$.author".to_string()),
                // 多值字段按 `,` 连接（规格 §13.4）
                kind: Some("$.tags[*]".to_string()),
                book_url: Some("$.url".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let body = r#"{"data":[{"name":"Alpha","suffix":"-X","author":"Tester","tags":["玄幻","仙侠"],"url":"/a"}]}"#;

        let results = engine.search_books(&source, body, "https://books.example");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "Alpha\n-X");
        assert_eq!(results[0].author, "Tester");
        assert_eq!(results[0].kind.as_deref(), Some("玄幻,仙侠"));
        assert_eq!(results[0].book_url, "https://books.example/a");
    }

    #[test]
    fn test_search_dedupes_and_falls_back_to_base_url() {
        let engine = RuleEngine::new().unwrap();
        let source = BookSource {
            book_source_name: "Json".to_string(),
            book_source_url: "https://source.example".to_string(),
            rule_search: Some(SearchRule {
                book_list: Some("$.data[*]".to_string()),
                name: Some("$.name".to_string()),
                author: Some("$.author".to_string()),
                // 命不中时应回退到 baseUrl，而不是留空
                book_url: Some("$.missing".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let body = r#"{"data":[
            {"name":"Same","author":"A"},
            {"name":"Same","author":"A"},
            {"name":"Other","author":"B"}
        ]}"#;

        let results = engine.search_books(&source, body, "https://books.example/list");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].name, "Same");
        assert_eq!(results[0].book_url, "https://books.example/list");
        assert_eq!(results[1].name, "Other");
    }

    #[test]
    fn test_book_url_pattern_switches_page_to_detail_parsing() {
        let engine = RuleEngine::new().unwrap();
        let source = BookSource {
            book_source_name: "Detail".to_string(),
            book_source_url: "https://source.example".to_string(),
            book_url_pattern: Some(r"https?://[^/]+/info\?book_id=\d+".to_string()),
            rule_search: Some(SearchRule {
                book_list: Some("$.items[*]".to_string()),
                name: Some("$.name".to_string()),
                ..Default::default()
            }),
            rule_book_info: Some(BookInfoRule {
                name: Some("$.title".to_string()),
                author: Some("$.author".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let body = r#"{"title":"Detail Book","author":"Tester"}"#;

        // 命中 bookUrlPattern：整页按详情页解析成单本书
        let results = engine.search_books(&source, body, "https://site.example/info?book_id=42");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "Detail Book");
        assert_eq!(results[0].author, "Tester");

        // 未命中且列表为空：`bookUrlPattern` 非空时不做详情页回落
        let results = engine.search_books(&source, body, "https://site.example/search?key=x");
        assert!(results.is_empty());
    }

    #[test]
    fn test_regex_list_drills_down_through_ampersand_stages() {
        let engine = RuleEngine::new().unwrap();
        let source = BookSource {
            book_source_name: "Regex".to_string(),
            book_source_url: "https://source.example".to_string(),
            rule_search: Some(SearchRule {
                book_list: Some(
                    r#":(?is)<li>(.*?)</li>&&<a href="([^"]+)">([^<]+)</a>"#.to_string(),
                ),
                name: Some("$2".to_string()),
                book_url: Some("$1".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let body = r#"<ul><li><a href="/1">One</a></li><li><a href="/2">Two</a></li></ul>"#;

        let results = engine.search_books(&source, body, "https://books.example");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].name, "One");
        assert_eq!(results[0].book_url, "https://books.example/1");
        assert_eq!(results[1].name, "Two");
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

    #[test]
    fn test_format_js_gint_is_shared_across_chapters() {
        let engine = RuleEngine::new().unwrap();
        let source = BookSource {
            book_source_name: "TOC".to_string(),
            book_source_url: "https://source.example".to_string(),
            rule_toc: Some(TocRule {
                chapter_list: Some(
                    "js:JSON.stringify([{chapterName:'A',chapterUrl:'/1'},{chapterName:'B',chapterUrl:'/2'},{chapterName:'C',chapterUrl:'/3'}])"
                        .to_string(),
                ),
                chapter_name: Some("chapterName".to_string()),
                chapter_url: Some("chapterUrl".to_string()),
                // gInt 从 0 开始，并在同一轮 formatJs 的章节之间累加
                format_js: Some("`${gInt++}-${title}`".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let (chapters, _) = engine.chapter_list(&source, "{}", "https://books.example");
        let titles: Vec<&str> = chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["0-A", "1-B", "2-C"]);
    }

    #[test]
    fn test_java_log_and_toast_do_not_break_rules() {
        let engine = RuleEngine::new().unwrap();
        let source = BookSource {
            book_source_name: "JS".to_string(),
            book_source_url: "https://source.example".to_string(),
            rule_search: Some(SearchRule {
                book_list: Some("js:JSON.stringify([{name:'Alpha'}])".to_string()),
                name: Some(
                    "@js:(java.log('debug'), java.toast('hi'), java.openUrl('x'), 'Alpha')"
                        .to_string(),
                ),
                ..Default::default()
            }),
            ..Default::default()
        };

        let results = engine.search_books(&source, "{}", "https://books.example");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "Alpha");
    }

    #[test]
    fn js_rules_may_use_top_level_return() {
        let engine = RuleEngine::new().unwrap();
        let source = BookSource {
            book_source_name: "JS".to_string(),
            book_source_url: "https://source.example".to_string(),
            rule_search: Some(SearchRule {
                book_list: Some("js:JSON.stringify([{name:'Alpha'}])".to_string()),
                name: Some("@js:return 'Renamed' + input.length".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let results = engine.search_books(&source, "{}", "https://books.example");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "Renamed0");
    }

    #[test]
    fn js_tag_chains_into_the_remaining_rule() {
        let engine = RuleEngine::new().unwrap();
        let source = BookSource {
            book_source_name: "Chain".to_string(),
            book_source_url: "https://source.example".to_string(),
            rule_book_info: Some(BookInfoRule {
                // <js> 之后继续解析 JS 结果（规格 §20：链式规则必须用 <js>...</js>）
                name: Some(r#"<js>'{"data":{"title":"Chained"}}'</js>$.data.title"#.to_string()),
                // `@js:` 会吞掉后续整段规则，因此这里取不到值
                author: Some(r#"@js:'{"data":{"author":"X"}}'$.data.author"#.to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let book = engine.book_info(
            &source,
            r#"{"x":1}"#,
            "https://site.example/info",
            "https://site.example/info",
        );

        assert_eq!(book.name, "Chained");
        assert!(book.author.is_empty(), "@js: 应吞掉后续规则");
    }

    #[test]
    fn js_tag_chains_in_html_mode() {
        let engine = RuleEngine::new().unwrap();
        let source = BookSource {
            book_source_name: "Chain".to_string(),
            book_source_url: "https://source.example".to_string(),
            rule_book_info: Some(BookInfoRule {
                name: Some(r#"<js>'{"a":"Html"}'</js>$.a"#.to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let book = engine.book_info(
            &source,
            "<html><body><p>x</p></body></html>",
            "https://site.example/info",
            "https://site.example/info",
        );

        assert_eq!(book.name, "Html");
    }

    #[test]
    fn source_templates_resolve_to_the_book_source() {
        let engine = RuleEngine::new().unwrap();
        let source = BookSource {
            book_source_name: "Detail".to_string(),
            book_source_url: "https://source.example".to_string(),
            rule_book_info: Some(BookInfoRule {
                name: Some("{{source.bookSourceUrl}}/book".to_string()),
                author: Some("@js:source.getKey()".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let book = engine.book_info(
            &source,
            r#"{"x":1}"#,
            "https://site.example/info",
            "https://site.example/info",
        );

        assert_eq!(book.name, "https://source.example/book");
        assert_eq!(book.author, "https://source.example");
    }

    #[test]
    fn test_search_books_js_json_list() {
        let engine = RuleEngine::new().unwrap();
        let source = BookSource {
            book_source_name: "JS".to_string(),
            book_source_url: "https://source.example".to_string(),
            rule_search: Some(SearchRule {
                book_list: Some(
                    "js:JSON.stringify([{name:'Alpha',author:'Tester',bookUrl:'/alpha'}])"
                        .to_string(),
                ),
                name: Some("name".to_string()),
                author: Some("author".to_string()),
                book_url: Some("bookUrl".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let results = engine.search_books(&source, "<html></html>", "https://books.example");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "Alpha");
        assert_eq!(results[0].author, "Tester");
        assert_eq!(results[0].book_url, "https://books.example/alpha");
    }

    #[test]
    fn test_chapter_list_js_and_format_js() {
        let engine = RuleEngine::new().unwrap();
        let source = BookSource {
            book_source_name: "JS TOC".to_string(),
            book_source_url: "https://source.example".to_string(),
            rule_toc: Some(TocRule {
                chapter_list: Some("js:JSON.stringify([{chapterName:'One',chapterUrl:'/1',isVip:'1'},{chapterName:'Two',chapterUrl:'/2',isPay:'true'}])".to_string()),
                chapter_name: Some("chapterName".to_string()),
                chapter_url: Some("chapterUrl".to_string()),
                is_vip: Some("isVip".to_string()),
                is_pay: Some("isPay".to_string()),
                format_js: Some("`${index}.${title}`".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let (chapters, next_urls) =
            engine.chapter_list(&source, "<html></html>", "https://books.example");
        assert!(next_urls.is_empty());
        assert_eq!(chapters.len(), 2);
        assert_eq!(chapters[0].title, "1.One");
        assert_eq!(chapters[0].url, "https://books.example/1");
        assert!(chapters[0].is_vip);
        assert_eq!(chapters[1].title, "2.Two");
        assert!(chapters[1].is_pay);
    }

    #[test]
    fn test_chapter_list_keeps_real_chapterlist_container() {
        let engine = RuleEngine::new().unwrap();
        let source = BookSource {
            book_source_name: "HTML TOC".to_string(),
            book_source_url: "https://source.example".to_string(),
            rule_toc: Some(TocRule {
                chapter_list: Some("#chapterlist a".to_string()),
                chapter_name: Some("@text".to_string()),
                chapter_url: Some("@href".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let body = r#"
            <div id="chapterlist">
                <a href="/1">第一章</a>
                <a href="/2">第二章</a>
            </div>
        "#;

        let (chapters, next_urls) = engine.chapter_list(&source, body, "https://books.example");
        assert!(next_urls.is_empty());
        assert_eq!(chapters.len(), 2);
        assert_eq!(chapters[0].url, "https://books.example/1");
        assert_eq!(chapters[1].url, "https://books.example/2");
    }

    #[test]
    fn test_search_books_js_uses_js_lib() {
        let engine = RuleEngine::new().unwrap();
        let source = BookSource {
            book_source_name: "JS Lib".to_string(),
            book_source_url: "https://source.example".to_string(),
            js_lib: Some("function buildName(v){ return v + '-lib'; }".to_string()),
            rule_search: Some(SearchRule {
                book_list: Some("js:JSON.stringify([{name:buildName('Alpha'),author:'Tester',bookUrl:'/alpha'}])".to_string()),
                name: Some("name".to_string()),
                author: Some("author".to_string()),
                book_url: Some("bookUrl".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let results = engine.search_books(&source, "<html></html>", "https://books.example");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "Alpha-lib");
    }

    #[test]
    fn test_book_info_html_interpolates_get_template() {
        let source = BookSource {
            book_source_name: "Info".to_string(),
            book_source_url: "https://source.example".to_string(),
            rule_book_info: Some(BookInfoRule {
                init: Some("@put:{alias:.name@text}".to_string()),
                name: Some("Book-@get:{alias}".to_string()),
                author: Some(".author@text".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let body = r#"<div class="name">Alias</div><div class="author">Tester</div>"#;
        let mut ctx = HashMap::new();
        let book = parse_book_info_html(
            &source,
            body,
            "https://books.example/detail/1",
            &source.rule_book_info.clone().unwrap(),
            "https://books.example/detail/1",
            &mut ctx,
        );
        assert_eq!(book.name, "Book-Alias");
        assert_eq!(book.author, "Tester");
    }

    /// 一份按番茄接口（FQWeb）写的评论规则，用作解析回归的样本。
    fn fqweb_source() -> BookSource {
        BookSource {
            book_source_name: "番茄Web".to_string(),
            book_source_url: "http://192.168.100.99:9999/".to_string(),
            rule_review: Some(ReviewRule {
                review_url: Some(
                    "comment/item?item_id={{$.data.data.novel_data.item_id}}\
                     &book_id={{$.data.data.novel_data.book_id}}&page={{page}}&count={{count}}"
                        .to_string(),
                ),
                list_rule: Some("$.data.data.comment[*]".to_string()),
                total_rule: Some("$.data.data.comment_cnt".to_string()),
                has_more_rule: Some("$.data.data.has_more".to_string()),
                id_rule: Some("$.comment_id".to_string()),
                name_rule: Some("$.user_info.user_name".to_string()),
                avatar_rule: Some("$.user_info.user_avatar".to_string()),
                content_rule: Some("$.text".to_string()),
                post_time_rule: Some("$.create_timestamp".to_string()),
                digg_rule: Some("$.digg_count".to_string()),
                reply_count_rule: Some("$.reply_count".to_string()),
                reply_list_rule: Some("$.reply_list[*]".to_string()),
                reply_name_rule: Some("$.user_info.user_name".to_string()),
                reply_content_rule: Some("$.text".to_string()),
                reply_post_time_rule: Some("$.create_timestamp".to_string()),
                image_rule: Some("$.image_url[*]".to_string()),
                ..Default::default()
            }),
            rule_para_review: Some(ParaReviewRule {
                index_url: Some(
                    "comment/para/list?book_id={{$.data.data.novel_data.book_id}}\
                     &item_id={{$.data.data.novel_data.item_id}}\
                     &item_version={{$.data.data.novel_data.version}}"
                        .to_string(),
                ),
                index_list_rule: Some("$.data.data.idea_data".to_string()),
                index_count_rule: Some("$.idea_count".to_string()),
                review_url: Some(
                    "comment/para?book_id={{$.data.data.novel_data.book_id}}\
                     &item_id={{$.data.data.novel_data.item_id}}\
                     &para_index={{paraIndex}}\
                     &item_version={{$.data.data.novel_data.version}}\
                     &page={{page}}&count={{count}}"
                        .to_string(),
                ),
                list_rule: Some("$.data.data.comments[*]".to_string()),
                total_rule: Some("$.data.data.count".to_string()),
                has_more_rule: Some("$.data.data.has_more".to_string()),
                name_rule: Some("$.user_info.user_name".to_string()),
                content_rule: Some("$.text".to_string()),
                digg_rule: Some("$.digg_count".to_string()),
                reply_count_rule: Some("$.reply_count".to_string()),
                reply_list_rule: Some("$.reply_list[*]".to_string()),
                reply_name_rule: Some("$.user_info.user_name".to_string()),
                reply_content_rule: Some("$.text".to_string()),
                image_rule: Some("$.image_url[*]".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

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
    fn review_url_reads_ids_from_the_content_response() {
        let engine = RuleEngine::new().unwrap();
        let source = fqweb_source();
        let body = content_body();
        let mut ctx = HashMap::new();
        ctx.insert("page".to_string(), "2".to_string());
        ctx.insert("count".to_string(), "20".to_string());

        let url = engine
            .chapter_review_url(
                &source,
                &body,
                "http://192.168.100.99:9999/content?item_id=1",
                &ctx,
            )
            .unwrap();
        assert_eq!(
            url,
            "http://192.168.100.99:9999/comment/item?item_id=7173615518858150414\
             &book_id=7143038691944959011&page=2&count=20"
        );

        ctx.insert("paraIndex".to_string(), "7".to_string());
        let url = engine
            .para_review_url(
                &source,
                &body,
                "http://192.168.100.99:9999/content?item_id=1",
                &ctx,
            )
            .unwrap();
        assert!(url.contains("para_index=7"), "段评 URL 应带上段号: {url}");
        assert!(url.contains("item_version=52754e01aa26a8dbd8c15cbcf7f4f64b_1_v5"));

        let url = engine
            .para_review_index_url(
                &source,
                &body,
                "http://192.168.100.99:9999/content?item_id=1",
                &ctx,
            )
            .unwrap();
        assert!(url.starts_with("http://192.168.100.99:9999/comment/para/list?"));
    }

    #[test]
    fn chapter_reviews_parse_a_page_with_inline_replies() {
        let engine = RuleEngine::new().unwrap();
        let source = fqweb_source();
        let body = json!({
            "data": {
                "data": {
                    "comment_cnt": 67,
                    "has_more": true,
                    "comment": [
                        {
                            "comment_id": "7468555045690393369",
                            "text": "不是说谎者也可以说谎",
                            "create_timestamp": 1717597455,
                            "digg_count": 68,
                            "reply_count": 13,
                            "user_info": {
                                "user_name": "读者甲",
                                "user_avatar": "https://img.example/a.jpg"
                            },
                            "reply_list": [
                                {"text": "同感", "user_info": {"user_name": "读者乙"}}
                            ]
                        },
                        {
                            "comment_id": "2",
                            "text": "   ",
                            "user_info": {"user_name": "空评论"}
                        }
                    ]
                }
            }
        })
        .to_string();

        let page = engine.chapter_reviews(&source, &body, "http://host/");
        assert_eq!(page.total, 67);
        assert!(page.has_more);
        // 内容为空的条目直接丢弃
        assert_eq!(page.items.len(), 1);
        let item = &page.items[0];
        assert_eq!(item.id, "7468555045690393369");
        assert_eq!(item.name, "读者甲");
        assert_eq!(item.content, "不是说谎者也可以说谎");
        assert_eq!(item.time, "1717597455");
        assert_eq!(item.digg, 68);
        assert_eq!(item.reply_count, 13);
        assert_eq!(item.avatar, "https://img.example/a.jpg");
        assert_eq!(item.replies.len(), 1);
        assert_eq!(item.replies[0].name, "读者乙");
        assert_eq!(item.replies[0].content, "同感");
    }

    #[test]
    fn para_review_index_maps_paragraph_numbers_to_counts() {
        let engine = RuleEngine::new().unwrap();
        let source = fqweb_source();
        let body = json!({
            "data": {
                "data": {
                    "idea_data": {
                        "32": {"idea_count": 641},
                        "5": {"idea_count": 192},
                        "7": {"idea_count": 0},
                        "-1": {"idea_count": 31},
                        "not-a-number": {"idea_count": 9}
                    }
                }
            }
        })
        .to_string();

        let paras = engine.para_review_index(&source, &body, "http://host/");
        // 段号非数字、条数为 0、以及负段号（整章聚合桶）都跳过，其余按段号升序
        assert_eq!(
            paras
                .iter()
                .map(|p| (p.para_index, p.count))
                .collect::<Vec<_>>(),
            vec![(5, 192), (32, 641)]
        );
    }

    #[test]
    fn missing_rules_disable_the_feature_instead_of_erroring() {
        let engine = RuleEngine::new().unwrap();
        let bare = BookSource {
            book_source_name: "无评论规则".to_string(),
            book_source_url: "https://source.example".to_string(),
            ..Default::default()
        };
        assert!(!engine.has_chapter_review_rule(&bare));
        assert!(!engine.has_para_review_rule(&bare));
        assert!(engine
            .chapter_review_url(&bare, "{}", "https://source.example", &HashMap::new())
            .is_none());
        assert!(engine
            .chapter_reviews(&bare, "{}", "https://source.example")
            .items
            .is_empty());
        assert!(engine
            .para_review_index(&bare, "{}", "https://source.example")
            .is_empty());

        // 只填 URL 不填列表规则等于没接：没有列表就渲染不出任何东西
        let half = BookSource {
            rule_review: Some(ReviewRule {
                review_url: Some("comment?page={{page}}".to_string()),
                ..Default::default()
            }),
            ..bare
        };
        assert!(!engine.has_chapter_review_rule(&half));
    }

    #[test]
    fn para_reviews_keep_replies_and_images() {
        let engine = RuleEngine::new().unwrap();
        let source = fqweb_source();
        let body = json!({
            "data": {
                "data": {
                    "count": 192,
                    "has_more": false,
                    "comments": [{
                        "comment_id": "p1",
                        "text": "这一段写得真好",
                        "digg_count": 216,
                        "reply_count": 2,
                        // 番茄同一张图给两个格式：HEIC 在前、JPEG 在后。
                        // 后端只负责按原序返回，挑哪个格式渲染是客户端的事。
                        "image_url": [
                            "https://img.example/a.heic?sign=1",
                            "https://img.example/a.jpeg?sign=2"
                        ],
                        "user_info": { "user_name": "读者丙" },
                        "reply_list": [
                            { "text": "同感", "user_info": { "user_name": "读者丁" } }
                        ]
                    }]
                }
            }
        })
        .to_string();

        let page = engine.para_reviews(&source, &body, "http://host/");
        assert_eq!(page.total, 192);
        assert_eq!(page.items.len(), 1);
        let item = &page.items[0];
        assert_eq!(item.reply_count, 2);
        assert_eq!(item.replies.len(), 1);
        assert_eq!(item.replies[0].name, "读者丁");
        assert_eq!(item.replies[0].content, "同感");
        assert_eq!(
            item.images,
            vec![
                "https://img.example/a.heic?sign=1".to_string(),
                "https://img.example/a.jpeg?sign=2".to_string()
            ]
        );
    }

    #[test]
    fn replies_are_sorted_by_time_ascending() {
        let engine = RuleEngine::new().unwrap();
        let source = fqweb_source();
        let body = json!({
            "data": { "data": {
                "comment_cnt": 1,
                "has_more": false,
                "comment": [{
                    "comment_id": "c1",
                    "text": "主评论",
                    "user_info": { "user_name": "楼主" },
                    "reply_list": [
                        { "text": "晚", "create_timestamp": 1700000300,
                          "user_info": { "user_name": "丙" } },
                        { "text": "早", "create_timestamp": 1700000100,
                          "user_info": { "user_name": "甲" } },
                        { "text": "中", "create_timestamp": 1700000200,
                          "user_info": { "user_name": "乙" } }
                    ]
                }]
            }}
        })
        .to_string();

        let page = engine.chapter_reviews(&source, &body, "http://host/");
        let names: Vec<&str> = page.items[0]
            .replies
            .iter()
            .map(|reply| reply.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["甲", "乙", "丙"],
            "回复要按时间升序：早在上、晚在下"
        );
    }

    #[test]
    fn replies_keep_the_source_order_when_times_are_not_comparable() {
        let engine = RuleEngine::new().unwrap();
        let source = fqweb_source();
        let body = json!({
            "data": { "data": {
                "comment_cnt": 1,
                "has_more": false,
                "comment": [{
                    "comment_id": "c1",
                    "text": "主评论",
                    "user_info": { "user_name": "楼主" },
                    // 时间不是时间戳（站点常见的 `2024-06-05 12:00` 形态）：
                    // 排不了就保持站点顺序，不做半吊子排序
                    "reply_list": [
                        { "text": "一", "create_timestamp": "2024-06-05 12:00",
                          "user_info": { "user_name": "甲" } },
                        { "text": "二", "create_timestamp": "2024-06-04 09:00",
                          "user_info": { "user_name": "乙" } }
                    ]
                }]
            }}
        })
        .to_string();

        let page = engine.chapter_reviews(&source, &body, "http://host/");
        let names: Vec<&str> = page.items[0]
            .replies
            .iter()
            .map(|reply| reply.name.as_str())
            .collect();
        assert_eq!(names, vec!["甲", "乙"]);
    }

    #[test]
    fn image_rule_accepts_field_js_and_html_forms() {
        let node = json!({
            "cover": "https://img.example/c.png",
            "shots": ["https://img.example/1.jpg", "", "https://img.example/2.webp"]
        });
        let base = "https://source.example/dir/";
        assert_eq!(
            eval_image_list_json("$.shots[*]", &node, base),
            vec![
                "https://img.example/1.jpg".to_string(),
                "https://img.example/2.webp".to_string()
            ]
        );
        // 字段名写法：数组展开，空值丢掉
        assert_eq!(
            eval_image_list_json("shots", &node, base),
            vec![
                "https://img.example/1.jpg".to_string(),
                "https://img.example/2.webp".to_string()
            ]
        );
        // 单值字段
        assert_eq!(
            eval_image_list_json("cover", &node, base),
            vec!["https://img.example/c.png".to_string()]
        );
        // JS 规则按行切分；相对地址按 base 解析
        assert_eq!(
            eval_image_list_json(
                "js:result = '1.jpg\\nhttps://img.example/2.jpg'",
                &node,
                base
            ),
            vec![
                "https://source.example/dir/1.jpg".to_string(),
                "https://img.example/2.jpg".to_string()
            ]
        );
        // 没写规则就是没图，不该报错
        assert!(eval_image_list_json("", &node, base).is_empty());
    }

    #[test]
    fn review_url_drops_empty_sort_param() {
        let engine = RuleEngine::new().unwrap();
        let base = "https://host/content?item_id=1";
        // 空值从来不是有意义的输入：站点要么报错、要么当成缺省，
        // 删掉让站点走默认的「最热」最稳
        let empty_sort: HashMap<String, String> = HashMap::new();
        assert_eq!(
            engine
                .review_url(
                    "comment/list?page={{page}}&sort=&count=20",
                    "{}",
                    base,
                    &empty_sort
                )
                .unwrap(),
            "https://host/comment/list?page=&count=20"
        );
        assert_eq!(
            engine
                .review_url("comment/list?sort=&page=1", "{}", base, &empty_sort)
                .unwrap(),
            "https://host/comment/list?page=1"
        );
        // 有值的 sort 原样保留
        let mut with_sort = HashMap::new();
        with_sort.insert("sort".to_string(), "time".to_string());
        assert_eq!(
            engine
                .review_url("comment/list?sort={{sort}}&page=1", "{}", base, &with_sort)
                .unwrap(),
            "https://host/comment/list?sort=time&page=1"
        );
    }

    #[test]
    fn review_count_text_handles_chinese_units() {
        assert_eq!(parse_count_text("216"), 216);
        assert_eq!(parse_count_text("216 赞"), 216);
        assert_eq!(parse_count_text("1.2万"), 12000);
        assert_eq!(parse_count_text(""), 0);
        assert_eq!(parse_count_text("暂无"), 0);
    }
}
