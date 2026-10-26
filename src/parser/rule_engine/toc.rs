//! 章节列表：把目录页响应解析成章节序列与下一页目录地址。
//!
//! 覆盖 CSS/XPath/JSONPath/正则/JS 五种模式；`preUpdateJs` 先改写响应体，
//! `formatJs` 逐章后处理标题（`gInt` 在同一轮内跨章累加）；CSS/XPath/JSONPath
//! 三种模式共用 `parse_chapter_list_core` 骨架，JS/JSON 模式的单章组装在
//! `build_chapter_from_json`。

use super::kernel::{
    capture_rule_value, eval_field_html_doc_with_ctx, eval_field_html_with_ctx,
    eval_field_json_with_ctx, eval_field_xpath_with_ctx, global_as_i64, is_truthy,
    normalize_list_rule, regex_list_captures, resolve_url, select_json_scope, select_xpath_scope,
    strip_js_rule, strip_mode_prefix, xpath_eval_strings, xpath_select_nodes, DocView,
    BODY_CHILDREN_SELECTOR,
};
use super::{ParseMode, RuleEngine};
use crate::model::rule::TocRule;
use crate::model::{book_chapter::BookChapter, book_source::BookSource};
use crate::parser::{
    html,
    js::{begin_parse_budget, eval_js, eval_js_with_bindings_and_globals, with_book_source},
    jsonpath,
};
use serde_json::{json, Value};
use std::collections::HashMap;

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

pub(super) fn parse_js_output_items(output: &str) -> Option<Vec<Value>> {
    let value = serde_json::from_str::<Value>(output.trim()).ok()?;
    match value {
        Value::Array(items) => Some(items),
        Value::Object(_) => Some(vec![value]),
        _ => None,
    }
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

impl RuleEngine {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::book_source::BookSource;
    use crate::model::rule::TocRule;

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
}
