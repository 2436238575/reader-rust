use jsonpath_rfc9535::JsonPath;
use serde_json::Value;
use std::sync::LazyLock;

use crate::parser::rule_analyzer::split_top_level;

static EMBEDDED_PATH_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\{\s*(\$[^}]+)\}").unwrap());

/// JsonPath 表达式的长度上限。
///
/// jsonpath-rfc9535 的过滤表达式解析器是相互递归下降、无深度计数，
/// 书源可控的深嵌套表达式（如数千层 `$[?((((...))))]`）会在解析阶段
/// 撑爆线程栈——栈溢出在 Rust 里是 abort，整个进程直接死，必须在
/// parse 之前拦掉。长度上限同时兜住解析耗时。
pub const MAX_JSONPATH_EXPR_LEN: usize = 4 * 1024;
/// JsonPath 表达式的括号/方括号嵌套上限（引号字符串内的字符不计）。
pub const MAX_JSONPATH_EXPR_NESTING: usize = 64;

/// 表达式是否在安全界限内（长度 + 括号/方括号嵌套深度）。
///
/// 单双引号字符串字面量内的字符不参与统计（引号内的 `((((` 是普通字符），
/// 引号内反斜杠转义 consume 下一个字符。
pub fn jsonpath_within_limits(rule: &str) -> bool {
    if rule.len() > MAX_JSONPATH_EXPR_LEN {
        return false;
    }
    let mut depth: i32 = 0;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for ch in rule.chars() {
        if let Some(active_quote) = quote {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if ch == active_quote {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' => quote = Some(ch),
            '(' | '[' => {
                depth += 1;
                if depth > MAX_JSONPATH_EXPR_NESTING as i32 {
                    return false;
                }
            }
            ')' | ']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    true
}

/// 单条 JsonPath 求值，不处理 `&&`/`||`/`%%` 组合符。
pub fn jsonpath_query(value: &Value, rule: &str) -> Vec<Value> {
    if let Some(rendered) = render_embedded_paths(value, rule) {
        return vec![Value::String(rendered)];
    }
    // 资源预检：`{$...}` 模板分支已先行处理，这里对真正的 JsonPath 表达式
    // 做长度/嵌套上限（防递归下降解析器栈溢出 abort），超限按解析失败处理
    if !jsonpath_within_limits(rule) {
        return vec![];
    }
    // RFC 9535 方言：解析失败（非法表达式）与查询失败一样返回空，
    // 与旧 jsonpath_lib 的 Err 分支行为一致
    let Ok(query) = JsonPath::parse(rule) else {
        return vec![];
    };
    let mut out = Vec::new();
    for item in query.query_values(value) {
        match item {
            Value::Array(items) => {
                out.extend(items.iter().cloned());
            }
            other => out.push(other.clone()),
        }
    }
    out
}

pub fn jsonpath_first_string(value: &Value, rule: &str) -> Option<String> {
    if let Some(rendered) = render_embedded_paths(value, rule) {
        return Some(rendered);
    }
    let res = jsonpath_query(value, rule);
    res.first().and_then(value_to_string)
}

pub fn value_to_string(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Array(items) => Some(
            items
                .iter()
                .filter_map(value_to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        Value::Object(_) => Some(v.to_string()),
    }
}

/// 支持组合符的列表求值：`&&` 拼接 / `||` 首个非空 / `%%` 交错（规格 §9.1 `getStringList`）。
pub fn jsonpath_query_combined(value: &Value, rule: &str) -> Vec<Value> {
    let split = split_top_level(rule, &["&&", "||", "%%"]);
    let Some(operator) = split.delimiter.as_deref() else {
        return jsonpath_query(value, rule);
    };

    let mut result = jsonpath_query(value, split.parts.first().map(String::as_str).unwrap_or(""));
    for part in split.parts.iter().skip(1) {
        let next = jsonpath_query(value, part);
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
    result
}

/// 规则内嵌 `{$.path}` 占位符时整体按字符串模板渲染；含 `{$` 但占位符
/// 未闭合/不匹配时返回 None，交回普通 JSONPath 求值，而不是吞成空串假命中。
fn render_embedded_paths(value: &Value, rule: &str) -> Option<String> {
    if !rule.contains("{$") {
        return None;
    }
    let mut replaced_any = false;
    let rendered = EMBEDDED_PATH_RE
        .replace_all(rule, |captures: &regex::Captures| {
            replaced_any = true;
            let path = captures.get(1).map(|m| m.as_str()).unwrap_or_default();
            jsonpath_first_string(value, path).unwrap_or_default()
        })
        .into_owned();
    replaced_any.then_some(rendered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn embedded_paths_render_against_document() {
        let doc = json!({"data": {"name": "abc", "id": 7}});
        assert_eq!(
            jsonpath_first_string(&doc, "prefix-{$.data.name}-{$.data.id}"),
            Some("prefix-abc-7".to_string())
        );
    }

    #[test]
    fn unclosed_embedded_path_falls_back_to_normal_query() {
        let doc = json!({"a": 1});
        // 未闭合的占位符不是合法 JSONPath：结果为空，但不能吞成空串假命中
        assert_eq!(jsonpath_first_string(&doc, "{$.a"), None);
        assert!(jsonpath_query(&doc, "{$.a").is_empty());
        // 正常 JSONPath 不受影响
        assert_eq!(jsonpath_first_string(&doc, "$.a"), Some("1".to_string()));
    }

    // ─── 资源上限（防深嵌套解析器栈溢出 abort） ───

    #[test]
    fn deep_nesting_is_rejected_without_panic() {
        // 100 层方括号嵌套远超 64 层上限：预检直接拒绝，不进入递归下降解析器
        let deep = format!("${}{}", "[".repeat(100), "]".repeat(100));
        assert!(!jsonpath_within_limits(&deep));
        let doc = json!({"a": 1});
        assert!(jsonpath_query(&doc, &deep).is_empty());

        // 过滤表达式里的深层括号同样拒绝（这是解析器相互递归的入口）
        let deep_filter = format!("$.a[?({}1{})]", "(".repeat(100), ")".repeat(100));
        assert!(!jsonpath_within_limits(&deep_filter));
        assert!(jsonpath_query(&doc, &deep_filter).is_empty());

        // 64 层以内仍交给解析器（这条规则语法上取不到值，但不因预检被吞）
        let within = format!("${}{}", "[".repeat(64), "]".repeat(64));
        assert!(jsonpath_within_limits(&within));
    }

    #[test]
    fn oversized_rule_is_rejected() {
        let doc = json!({"a": 1});
        let oversized = format!("$.{}{}", "b".repeat(MAX_JSONPATH_EXPR_LEN + 1), ".c");
        assert!(oversized.len() > MAX_JSONPATH_EXPR_LEN);
        assert!(!jsonpath_within_limits(&oversized));
        assert!(jsonpath_query(&doc, &oversized).is_empty());
        // 4KB 以内的长规则不受影响
        let ok = format!("$.{}b", "a".repeat(100));
        assert!(jsonpath_within_limits(&ok));
    }

    #[test]
    fn brackets_inside_string_literals_do_not_count() {
        let doc = json!({"a": [{"x": "[".repeat(100)}, {"x": "plain"}]});
        // 引号内的括号是普通字符，不计入嵌套深度；语法上仍是合法 JSONPath，
        // 能正常求值命中
        let quoted = format!("$.a[?(@.x == \"{}\")]", "[".repeat(100));
        assert!(jsonpath_within_limits(&quoted));
        assert_eq!(jsonpath_query(&doc, &quoted), vec![doc["a"][0].clone()]);
        // 对照样：同样 100 层括号在引号外则被预检拒绝
        assert!(!jsonpath_within_limits(&format!(
            "$.a[{}]",
            "[".repeat(100)
        )));
        // 单引号方言同理
        let single_quoted = format!("$.a[?(@.x == '{}')]", "(".repeat(100));
        assert!(jsonpath_within_limits(&single_quoted));
        // 引号内的转义字符不提前闭合字符串
        let escaped_quote = "$.a[?(@.x == '\\\"((((')]";
        assert!(jsonpath_within_limits(escaped_quote));
    }

    #[test]
    fn normal_rules_still_query_within_limits() {
        let doc = json!({"data": {"list": [{"name": "书甲"}, {"name": "书乙"}]}});
        assert!(jsonpath_within_limits("$.data.list[*].name"));
        assert_eq!(
            jsonpath_query(&doc, "$.data.list[*].name"),
            vec![json!("书甲"), json!("书乙")]
        );
    }

    // ─── RFC 9535 方言覆盖（jsonpath-rfc9535 换库的行为锚点） ───

    #[test]
    fn book_source_shapes_basics() {
        let doc = json!({
            "data": {
                "list": [
                    {"name": "书甲", "author": "甲", "id": 1},
                    {"name": "书乙", "author": "乙", "id": 2},
                ],
                "total": 2
            }
        });
        // 列表通配：书源列表规则的最常见形态
        let names = jsonpath_query(&doc, "$.data.list[*].name");
        assert_eq!(names, vec![json!("书甲"), json!("书乙")]);
        // 数组节点整体命中时拍平一层（旧 jsonpath_lib 行为，适配层保留）
        let list = jsonpath_query(&doc, "$.data.list");
        assert_eq!(list.len(), 2);
        // 标量
        assert_eq!(
            jsonpath_first_string(&doc, "$.data.total"),
            Some("2".to_string())
        );
        // 下标与倒数下标
        assert_eq!(
            jsonpath_first_string(&doc, "$.data.list[0].author"),
            Some("甲".to_string())
        );
        assert_eq!(
            jsonpath_first_string(&doc, "$.data.list[-1].name"),
            Some("书乙".to_string())
        );
    }

    #[test]
    fn book_source_shapes_filters_and_descendants() {
        let doc = json!({
            "store": {
                "book": [
                    {"title": "A", "price": 5},
                    {"title": "B", "price": 12, "isbn": "x"}
                ]
            }
        });
        // 带括号的过滤（旧书源写法 ?(@.x)）与存在性过滤
        assert_eq!(
            jsonpath_first_string(&doc, "$.store.book[?(@.price < 10)].title"),
            Some("A".to_string())
        );
        assert_eq!(
            jsonpath_first_string(&doc, "$.store.book[?(@.isbn)].title"),
            Some("B".to_string())
        );
        // 深层扫描
        let titles = jsonpath_query(&doc, "$..title");
        assert_eq!(titles, vec![json!("A"), json!("B")]);
    }

    #[test]
    fn regex_dialect_migration_is_explicit() {
        let doc = json!({"items": [{"name": "斗破苍穹"}, {"name": "其他"}]});
        // 旧方言的 =~ 在 RFC 9535 里不是合法语法：解析失败返回空（不是 panic、
        // 不是假命中）——书源作者能看到的症状是「规则取不到值」
        assert!(jsonpath_query(&doc, "$.items[?(@.name =~ /斗破/)].name").is_empty());
        // 标准替代：match()/search() 函数
        assert_eq!(
            jsonpath_first_string(&doc, "$.items[?search(@.name, \"斗破\")].name"),
            Some("斗破苍穹".to_string())
        );
    }

    #[test]
    fn combinators_still_work_on_rfc_dialect() {
        let doc = json!({"a": {"x": "1"}, "b": {"y": "2"}});
        // && 拼接
        assert_eq!(
            jsonpath_query_combined(&doc, "$.a.x&&$.b.y"),
            vec![json!("1"), json!("2")]
        );
        // || 首个非空（组合符只在 query_combined 层拆分，first_string 不管组合）
        assert_eq!(
            jsonpath_query_combined(&doc, "$.missing||$.a.x"),
            vec![json!("1")]
        );
        // %% 交错
        assert_eq!(
            jsonpath_query_combined(&doc, "$.a.x%%$.b.y"),
            vec![json!("1"), json!("2")]
        );
    }
}
