mod kernel;

use kernel::{
    apply_legado_regex, capture_rule_value, eval_field_html, eval_field_html_doc_with_ctx,
    eval_field_html_with_ctx, eval_field_json, eval_field_json_with_ctx, eval_field_xpath,
    eval_field_xpath_with_ctx, eval_kind_html_doc, eval_kind_html_element, eval_kind_json,
    eval_kind_xpath, extract_js, global_as_i64, interpolate_common_templates,
    interpolate_json_templates, is_truthy, normalize_list_rule, pick_json_field,
    regex_list_captures, resolve_url, select_json_scope, select_xpath_scope, split_legado_regex,
    strip_js_rule, strip_mode_prefix, xpath_eval_strings, xpath_select_nodes, DocView,
    BODY_CHILDREN_SELECTOR, INLINE_JS_RE,
};

use crate::model::chapter_image::ChapterImage;
use crate::model::review::{ParaReviewCount, ReviewItem, ReviewPage, ReviewReply};
use crate::model::rule::{BookInfoRule, ContentImageRule, SearchRule, TocRule};
use crate::model::{
    book::Book, book_chapter::BookChapter, book_source::BookSource, search::SearchBook,
};
use crate::parser::{
    html,
    js::{begin_parse_budget, eval_js, eval_js_with_bindings_and_globals, with_book_source},
    jsonpath,
};
use serde_json::{json, Value};
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

    pub fn search_books(&self, source: &BookSource, body: &str, base_url: &str) -> Vec<SearchBook> {
        let _budget = begin_parse_budget();
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
            let mut results =
                self.dispatch_book_list(mode, source, body, base_url, &rule, list_rule);

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
        let _budget = begin_parse_budget();
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
            let mut results =
                self.dispatch_book_list(mode, source, body, base_url, &rule, list_rule);
            if reverse {
                results.reverse();
            }
            dedupe_books(results)
        })
    }

    /// 搜索/发现共用的按模式分发：两者列表与字段规则同构，
    /// 差别只在规则来源与详情页回退，分发本身不该写两份。
    fn dispatch_book_list(
        &self,
        mode: ParseMode,
        source: &BookSource,
        body: &str,
        base_url: &str,
        rule: &SearchRule,
        list_rule: &str,
    ) -> Vec<SearchBook> {
        match mode {
            ParseMode::JsonPath => self.search_books_json(source, body, base_url, rule, list_rule),
            ParseMode::XPath => self.search_books_xpath(source, body, base_url, rule, list_rule),
            ParseMode::Js => self.search_books_js(source, body, base_url, rule, list_rule),
            ParseMode::Regex => self.search_books_regex(source, body, base_url, rule, list_rule),
            ParseMode::Css => self.search_books_html(source, body, base_url, rule, list_rule),
        }
    }

    pub fn book_info(
        &self,
        source: &BookSource,
        body: &str,
        base_url: &str,
        book_url: &str,
    ) -> Book {
        let _budget = begin_parse_budget();
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
        let _budget = begin_parse_budget();
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
        let _budget = begin_parse_budget();
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
        let _budget = begin_parse_budget();
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
        let mut out = Vec::new();
        for el in doc.select(&BODY_CHILDREN_SELECTOR) {
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
                book_id: None,
                book_source_id: None,
                book_source_ids: None,
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
                book_id: None,
                book_source_id: None,
                book_source_ids: None,
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
        let mut out = Vec::new();
        let mut seen_urls = std::collections::HashSet::new();
        for el in doc.select(&BODY_CHILDREN_SELECTOR) {
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
            let volume = chapter_meta(
                rule.volume_name
                    .as_ref()
                    .and_then(|r| eval_field_html_with_ctx(r, &el, base_url, ctx)),
            );
            let word_count = chapter_meta(
                rule.word_count
                    .as_ref()
                    .and_then(|r| eval_field_html_with_ctx(r, &el, base_url, ctx)),
            );
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
                volume,
                word_count,
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
            let volume = chapter_meta(capture_rule_value(rule.volume_name.as_deref(), &groups));
            let word_count = chapter_meta(capture_rule_value(rule.word_count.as_deref(), &groups));
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
                volume,
                word_count,
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
                book_id: None,
                book_source_id: None,
                book_source_ids: None,
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
                book_id: None,
                book_source_id: None,
                book_source_ids: None,
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
                book_id: None,
                book_source_id: None,
                book_source_ids: None,
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

    /// 章评是否配了作者标记规则（「只看作者」的前提；缺规则时扫描只会得到空列表）。
    pub fn has_chapter_author_rules(&self, source: &BookSource) -> bool {
        let Some(rule) = source.rule_review.as_ref() else {
            return false;
        };
        has_rule(rule.author_rule.as_deref())
            || has_rule(rule.author_digg_rule.as_deref())
            || has_rule(rule.reply_author_rule.as_deref())
            || has_rule(rule.reply_author_digg_rule.as_deref())
    }

    /// 段评是否配了作者标记规则。
    pub fn has_para_author_rules(&self, source: &BookSource) -> bool {
        let Some(rule) = source.rule_para_review.as_ref() else {
            return false;
        };
        has_rule(rule.author_rule.as_deref())
            || has_rule(rule.author_digg_rule.as_deref())
            || has_rule(rule.reply_author_rule.as_deref())
            || has_rule(rule.reply_author_digg_rule.as_deref())
    }

    /// 求值章评列表地址。
    pub fn chapter_review_url(
        &self,
        source: &BookSource,
        body: &str,
        base_url: &str,
        ctx: &HashMap<String, String>,
    ) -> Option<String> {
        let _budget = begin_parse_budget();
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
        let _budget = begin_parse_budget();
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
        let _budget = begin_parse_budget();
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
        let _budget = begin_parse_budget();
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
        let _budget = begin_parse_budget();
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
            author: rule.author_rule.as_deref(),
            author_digg: rule.author_digg_rule.as_deref(),
            reply_author: rule.reply_author_rule.as_deref(),
            reply_author_digg: rule.reply_author_digg_rule.as_deref(),
            image: rule.image_rule.as_deref(),
            reply_image: rule
                .reply_image_rule
                .as_deref()
                .or(rule.image_rule.as_deref()),
        };
        with_book_source(source, || parse_review_page(&fields, body, base_url))
    }

    /// 解析某一段的段评列表。
    pub fn para_reviews(&self, source: &BookSource, body: &str, base_url: &str) -> ReviewPage {
        let _budget = begin_parse_budget();
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
            author: rule.author_rule.as_deref(),
            author_digg: rule.author_digg_rule.as_deref(),
            reply_author: rule.reply_author_rule.as_deref(),
            reply_author_digg: rule.reply_author_digg_rule.as_deref(),
            image: rule.image_rule.as_deref(),
            reply_image: rule
                .reply_image_rule
                .as_deref()
                .or(rule.image_rule.as_deref()),
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
        let _budget = begin_parse_budget();
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
            let author_rule = rule.index_author_commented_rule.as_deref().unwrap_or("");
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
                    let author_commented =
                        eval_field_json_with_ctx(author_rule, value, base_url, &mut ctx)
                            .map(is_truthy)
                            .unwrap_or(false);
                    paras.push(ParaReviewCount {
                        para_index,
                        count,
                        author_commented,
                        text: String::new(),
                    });
                }
            }
            paras.sort_by_key(|p| p.para_index);
            paras
        })
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
    author: Option<&'a str>,
    author_digg: Option<&'a str>,
    reply_author: Option<&'a str>,
    reply_author_digg: Option<&'a str>,
    image: Option<&'a str>,
    reply_image: Option<&'a str>,
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
                        author: eval_field_json_with_ctx(
                            fields.reply_author.unwrap_or(""),
                            reply,
                            base_url,
                            &mut ctx,
                        )
                        .map(is_truthy)
                        .unwrap_or(false),
                        author_digg: eval_field_json_with_ctx(
                            fields.reply_author_digg.unwrap_or(""),
                            reply,
                            base_url,
                            &mut ctx,
                        )
                        .map(is_truthy)
                        .unwrap_or(false),
                        images: fields
                            .reply_image
                            .map(|rule| eval_image_list_json(rule, reply, base_url))
                            .unwrap_or_default(),
                    })
                    .filter(|reply| !reply.content.trim().is_empty() || !reply.images.is_empty())
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
            author: eval_field_json_with_ctx(fields.author.unwrap_or(""), node, base_url, &mut ctx)
                .map(is_truthy)
                .unwrap_or(false),
            author_digg: eval_field_json_with_ctx(
                fields.author_digg.unwrap_or(""),
                node,
                base_url,
                &mut ctx,
            )
            .map(is_truthy)
            .unwrap_or(false),
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
                        author: eval_field_html_with_ctx(
                            fields.reply_author.unwrap_or(""),
                            reply,
                            base_url,
                            &mut ctx,
                        )
                        .map(is_truthy)
                        .unwrap_or(false),
                        author_digg: eval_field_html_with_ctx(
                            fields.reply_author_digg.unwrap_or(""),
                            reply,
                            base_url,
                            &mut ctx,
                        )
                        .map(is_truthy)
                        .unwrap_or(false),
                        images: fields
                            .reply_image
                            .map(|rule| eval_image_list_html(rule, reply, base_url))
                            .unwrap_or_default(),
                    })
                    .filter(|reply| !reply.content.trim().is_empty() || !reply.images.is_empty())
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
            author: eval_field_html_with_ctx(fields.author.unwrap_or(""), node, base_url, &mut ctx)
                .map(is_truthy)
                .unwrap_or(false),
            author_digg: eval_field_html_with_ctx(
                fields.author_digg.unwrap_or(""),
                node,
                base_url,
                &mut ctx,
            )
            .map(is_truthy)
            .unwrap_or(false),
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

/// 目录列表解析的公共骨架：三种解析模式（CSS/XPath/JSONPath）的
/// 「逐条目取八个字段 → URL 绝对化去重 → 组 BookChapter」循环完全同构，
/// 差别只在条目集合的产出与字段求值器。此前同一段循环逐字写了三份，
/// 修 bug 必然 drift。
fn parse_chapter_list_core<I>(
    items: Vec<I>,
    base_url: &str,
    rule: &TocRule,
    mut eval_field: impl FnMut(Option<&str>, &I) -> Option<String>,
) -> Vec<BookChapter> {
    let mut seen_urls = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let title = eval_field(rule.chapter_name.as_deref(), &item).unwrap_or_default();
        let url = eval_field(rule.chapter_url.as_deref(), &item).unwrap_or_default();
        let tag = eval_field(rule.update_time.as_deref(), &item);
        let volume = chapter_meta(eval_field(rule.volume_name.as_deref(), &item));
        let word_count = chapter_meta(eval_field(rule.word_count.as_deref(), &item));
        let is_volume = eval_field(rule.is_volume.as_deref(), &item)
            .map(is_truthy)
            .unwrap_or(false);
        let is_vip = eval_field(rule.is_vip.as_deref(), &item)
            .map(is_truthy)
            .unwrap_or(false);
        let is_pay = eval_field(rule.is_pay.as_deref(), &item)
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
            volume,
            word_count,
        });
    }
    out
}

/// 下一页目录地址的公共收尾：去空串 + 相对地址绝对化。
fn resolve_next_toc_urls(raw: Vec<String>, base_url: &str) -> Vec<String> {
    raw.into_iter()
        .filter(|u| !u.is_empty())
        .map(|u| resolve_url(base_url, &u))
        .collect()
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
    let out = parse_chapter_list_core(items, base_url, rule, |rule_text, el| {
        rule_text.and_then(|r| eval_field_html_with_ctx(r, el, base_url, ctx))
    });

    let raw_urls: Vec<String> =
        html::select_text_list(&doc, rule.next_toc_url.as_deref().unwrap_or(""));
    (out, resolve_next_toc_urls(raw_urls, base_url))
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

    let out = parse_chapter_list_core(items, base_url, rule, |rule_text, item| {
        eval_field_xpath_with_ctx(rule_text.unwrap_or(""), *item, base_url, ctx)
    });

    let raw_urls = rule
        .next_toc_url
        .as_deref()
        .map(|xpath| xpath_eval_strings(scope, xpath))
        .unwrap_or_default();
    (out, resolve_next_toc_urls(raw_urls, base_url))
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

    let out = parse_chapter_list_core(items, base_url, rule, |rule_text, item| {
        eval_field_json_with_ctx(rule_text.unwrap_or(""), item, base_url, ctx)
    });

    let raw_urls: Vec<String> = rule
        .next_toc_url
        .as_ref()
        .map(|r| jsonpath::jsonpath_query_combined(&scope, r))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect();
    (out, resolve_next_toc_urls(raw_urls, base_url))
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
        book_id: None,
        book_source_id: None,
        book_source_ids: None,
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
        book_id: None,
        book_source_id: None,
        book_source_ids: None,
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
    let volume = chapter_meta(eval_field_json_with_ctx(
        rule.volume_name.as_deref().unwrap_or(""),
        item,
        base_url,
        ctx,
    ));
    let word_count = chapter_meta(eval_field_json_with_ctx(
        rule.word_count.as_deref().unwrap_or(""),
        item,
        base_url,
        ctx,
    ));
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
        volume,
        word_count,
    })
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

/// 章节元信息（卷名/字数）归一化：空白结果视为「没有」，避免空串卷名
/// 在目录 UI 上渲染出空卷标题
fn chapter_meta(value: Option<String>) -> Option<String> {
    value.and_then(|s| {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
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
    fn test_json_field_rules_with_filter_and_multi_index_parse_as_jsonpath() {
        let engine = RuleEngine::new().unwrap();
        let source = BookSource {
            book_source_name: "Json".to_string(),
            book_source_url: "https://source.example".to_string(),
            rule_search: Some(SearchRule {
                book_list: Some("$.data[*]".to_string()),
                // 含 ? 和 == 的过滤表达式此前被字面量启发式吞成规则文本本身
                name: Some("$.items[?(@.x==1)].title".to_string()),
                // 多下标含逗号，同样被吞；现在解析出两个值（\n 连接）
                author: Some("$.a[0,1]".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let body = r#"{"data":[{
            "items":[{"x":1,"title":"命中"},{"x":2,"title":"跳过"}],
            "a":["作者甲","作者乙"]
        }]}"#;

        let results = engine.search_books(&source, body, "https://books.example");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "命中");
        assert_eq!(results[0].author, "作者甲\n作者乙");
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
    fn test_chapter_list_json_volume_word_count_and_tag() {
        let engine = RuleEngine::new().unwrap();
        let source = BookSource {
            book_source_name: "TOC".to_string(),
            book_source_url: "https://source.example".to_string(),
            rule_toc: Some(TocRule {
                chapter_list: Some("$.data.item_data_list[*]".to_string()),
                chapter_name: Some("$.title".to_string()),
                chapter_url: Some("$.item_id".to_string()),
                update_time: Some("$.first_pass_time".to_string()),
                volume_name: Some("$.volume_name".to_string()),
                word_count: Some("$.chapter_word_number".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let body = r#"{"data":{"item_data_list":[
            {"item_id":"1","title":"第一章","volume_name":"第一卷：起","chapter_word_number":2397,"first_pass_time":1724481989},
            {"item_id":"2","title":"第二章","volume_name":"第一卷：起","chapter_word_number":2104,"first_pass_time":1724500813},
            {"item_id":"3","title":"第三章","volume_name":" ","chapter_word_number":"","first_pass_time":0}
        ]}}"#;

        let (chapters, _) = engine.chapter_list(&source, body, "https://books.example");
        assert_eq!(chapters.len(), 3);
        assert_eq!(chapters[0].volume.as_deref(), Some("第一卷：起"));
        assert_eq!(chapters[0].word_count.as_deref(), Some("2397"));
        assert_eq!(chapters[0].tag.as_deref(), Some("1724481989"));
        assert_eq!(chapters[1].volume.as_deref(), Some("第一卷：起"));
        // 空白卷名/字数归一化为 None，避免目录渲染出空卷标题
        assert_eq!(chapters[2].volume, None);
        assert_eq!(chapters[2].word_count, None);
    }

    #[test]
    fn test_chapter_list_js_carries_volume_meta() {
        let engine = RuleEngine::new().unwrap();
        let source = BookSource {
            book_source_name: "TOC".to_string(),
            book_source_url: "https://source.example".to_string(),
            rule_toc: Some(TocRule {
                chapter_list: Some(
                    "js:JSON.stringify([{chapterName:'A',chapterUrl:'/1',volumeName:'卷一',wordCount:'100',updateTime:'1724481989'}])"
                        .to_string(),
                ),
                chapter_name: Some("chapterName".to_string()),
                chapter_url: Some("chapterUrl".to_string()),
                volume_name: Some("volumeName".to_string()),
                word_count: Some("wordCount".to_string()),
                update_time: Some("updateTime".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let (chapters, _) = engine.chapter_list(&source, "{}", "https://books.example");
        assert_eq!(chapters.len(), 1);
        assert_eq!(chapters[0].volume.as_deref(), Some("卷一"));
        assert_eq!(chapters[0].word_count.as_deref(), Some("100"));
        assert_eq!(chapters[0].tag.as_deref(), Some("1724481989"));
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

    #[test]
    fn test_book_info_html_supports_legado_regex_and_combinators() {
        // 文档级字段与元素级同构：`##` 替换与 `&&` 组合都生效
        let source = BookSource {
            book_source_name: "Info".to_string(),
            book_source_url: "https://source.example".to_string(),
            rule_book_info: Some(BookInfoRule {
                name: Some(".name@text##（.*）".to_string()),
                author: Some(".a1@text&&.a2@text".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let body = r#"<div class="name">雪中（修订版）</div><span class="a1">烽火</span><span class="a2">戏诸侯</span>"#;
        let mut ctx = HashMap::new();
        let book = parse_book_info_html(
            &source,
            body,
            "https://books.example/detail/1",
            &source.rule_book_info.clone().unwrap(),
            "https://books.example/detail/1",
            &mut ctx,
        );
        assert_eq!(book.name, "雪中");
        assert_eq!(book.author, "烽火\n戏诸侯");
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
                author_rule: Some("$.author".to_string()),
                author_digg_rule: Some("$.has_author_digg".to_string()),
                reply_author_rule: Some("$.author".to_string()),
                reply_author_digg_rule: Some("$.has_author_digg".to_string()),
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
                index_author_commented_rule: Some("$.author_commented".to_string()),
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
                author_rule: Some("$.author".to_string()),
                author_digg_rule: Some("$.has_author_digg".to_string()),
                reply_author_rule: Some("$.author".to_string()),
                reply_author_digg_rule: Some("$.has_author_digg".to_string()),
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
                            "author": 1,
                            "has_author_digg": true,
                            "reply_list": [
                                {"text": "同感", "user_info": {"user_name": "读者乙"}, "author": true},
                                {
                                    "text": "",
                                    "user_info": {"user_name": "读者丙"},
                                    "image_url": [
                                        "https://img.example/pic.heic",
                                        "https://img.example/pic.jpeg"
                                    ]
                                }
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
        assert_eq!(item.replies.len(), 2);
        assert_eq!(item.replies[0].name, "读者乙");
        assert_eq!(item.replies[0].content, "同感");
        // 作者标记：数字 1 与布尔 true 都算真（is_truthy 词表）
        assert!(item.author);
        assert!(item.author_digg);
        assert!(item.replies[0].author);
        assert!(!item.replies[0].author_digg);
        // 楼中楼配图：reply_image_rule 缺省回落到 image_rule；
        // 纯图回复（无文字）不因空 content 被丢弃
        assert!(item.replies[0].images.is_empty());
        let image_reply = &page.items[0].replies[1];
        assert_eq!(image_reply.name, "读者丙");
        assert_eq!(image_reply.images.len(), 2);
        assert_eq!(image_reply.images[0], "https://img.example/pic.heic");
    }

    #[test]
    fn author_flags_default_to_false_without_rules() {
        let engine = RuleEngine::new().unwrap();
        let mut source = fqweb_source();
        let rule = source.rule_review.as_mut().unwrap();
        rule.author_rule = None;
        rule.author_digg_rule = None;
        rule.reply_author_rule = None;
        rule.reply_author_digg_rule = None;
        let body = json!({
            "data": { "data": { "comment": [
                { "text": "x", "author": 1, "has_author_digg": true }
            ] } }
        })
        .to_string();

        let page = engine.chapter_reviews(&source, &body, "http://host/");
        assert_eq!(page.items.len(), 1);
        assert!(!page.items[0].author);
        assert!(!page.items[0].author_digg);
    }

    #[test]
    fn para_review_index_maps_paragraph_numbers_to_counts() {
        let engine = RuleEngine::new().unwrap();
        let source = fqweb_source();
        let body = json!({
            "data": {
                "data": {
                    "idea_data": {
                        "32": {"idea_count": 641, "author_commented": 1},
                        "5": {"idea_count": 192, "author_commented": 0},
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
                .map(|p| (p.para_index, p.count, p.author_commented))
                .collect::<Vec<_>>(),
            vec![(5, 192, false), (32, 641, true)]
        );
    }

    #[test]
    fn para_review_index_without_author_rule_defaults_to_false() {
        let engine = RuleEngine::new().unwrap();
        let mut source = fqweb_source();
        source
            .rule_para_review
            .as_mut()
            .unwrap()
            .index_author_commented_rule = None;
        let body = json!({
            "data": {"data": {"idea_data": {"3": {"idea_count": 9, "author_commented": 1}}}}
        })
        .to_string();

        let paras = engine.para_review_index(&source, &body, "http://host/");
        assert_eq!(paras.len(), 1);
        assert!(!paras[0].author_commented);
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
