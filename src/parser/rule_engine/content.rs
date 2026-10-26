//! 章节正文与下一页地址。
//!
//! `content` 依次应用 `sourceRegex` 改写、`webJs` 预处理与正文规则
//! （JS/JSONPath/XPath/CSS 自动识别），再做 `replaceRegex` 收尾；
//! `next_content_url` 对正文响应求值分页规则并绝对化。

use super::kernel::{apply_legado_regex, resolve_url, INLINE_JS_RE};
use super::{ParseMode, RuleEngine};
use crate::model::book_source::BookSource;
use crate::parser::{
    html,
    js::{begin_parse_budget, eval_js, with_book_source},
    jsonpath,
};
use serde_json::Value;

impl RuleEngine {
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
}
