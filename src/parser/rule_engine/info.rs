//! 书籍详情：把详情页响应解析成 `Book`。
//!
//! 支持 CSS/XPath/JSONPath 三种模式；`init` 规则先行求值（可配合
//! `@put`/`@get` 在字段间传值），文档级字段求值复用 kernel 的
//! `DocView` 与 `eval_field_html_doc_with_ctx`。

use super::kernel::{
    eval_field_html_doc_with_ctx, eval_field_json_with_ctx, eval_field_xpath_with_ctx,
    eval_kind_html_doc, eval_kind_json, eval_kind_xpath, resolve_url, select_json_scope,
    select_xpath_scope, DocView,
};
use super::{ParseMode, RuleEngine};
use crate::model::rule::BookInfoRule;
use crate::model::{book::Book, book_source::BookSource};
use crate::parser::{
    html,
    js::{begin_parse_budget, with_book_source},
};
use serde_json::Value;
use std::collections::HashMap;

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

impl RuleEngine {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::book_source::BookSource;
    use crate::model::rule::BookInfoRule;

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
}
