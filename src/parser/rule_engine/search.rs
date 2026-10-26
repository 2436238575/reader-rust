//! 搜索与发现：把列表页/详情页响应解析成书籍条目。
//!
//! 覆盖五种解析模式（CSS/XPath/JSONPath/正则/JS）的列表分发、
//! `bookUrlPattern` 命中时的详情页回退，以及搜索结果去重。

use super::kernel::{
    capture_rule_value, eval_field_html, eval_field_json, eval_field_json_with_ctx,
    eval_field_xpath, eval_kind_html_element, eval_kind_json, eval_kind_xpath, normalize_list_rule,
    regex_list_captures, resolve_url, xpath_select_nodes, BODY_CHILDREN_SELECTOR,
};
use super::{parse_js_output_items, ParseMode, RuleEngine};
use crate::model::rule::SearchRule;
use crate::model::{book::Book, book_source::BookSource, search::SearchBook};
use crate::parser::{
    html,
    js::{begin_parse_budget, eval_js, with_book_source},
    jsonpath,
};
use serde_json::Value;
use std::collections::HashMap;

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

impl RuleEngine {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::book_source::BookSource;
    use crate::model::rule::{BookInfoRule, SearchRule};

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
}
