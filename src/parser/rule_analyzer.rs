#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitResult {
    pub parts: Vec<String>,
    pub delimiter: Option<String>,
}

pub fn split_top_level(rule: &str, delimiters: &[&str]) -> SplitResult {
    let Some((_, delimiter)) = find_next_delimiter(rule, delimiters, 0) else {
        return SplitResult {
            parts: vec![rule.trim().to_string()],
            delimiter: None,
        };
    };

    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut search_from = 0usize;
    while let Some((idx, _)) = find_next_delimiter(rule, &[delimiter], search_from) {
        parts.push(rule[start..idx].trim().to_string());
        start = idx + delimiter.len();
        search_from = start;
    }
    parts.push(rule[start..].trim().to_string());

    SplitResult {
        parts,
        delimiter: Some(delimiter.to_string()),
    }
}

fn find_next_delimiter<'a>(
    rule: &'a str,
    delimiters: &[&'a str],
    from: usize,
) -> Option<(usize, &'a str)> {
    let mut square_depth = 0i32;
    let mut paren_depth = 0i32;
    let mut brace_depth = 0i32;
    let mut quote: Option<char> = None;
    let mut escaped = false;

    for (idx, ch) in rule.char_indices().filter(|(idx, _)| *idx >= from) {
        // 被反斜杠转义的字符（引号内外同样规则）不参与任何判定：
        // 引号外此前 escaped 置位后从不复位，`\&&` 仍会被顶层切开
        if escaped {
            escaped = false;
            continue;
        }
        if let Some(active_quote) = quote {
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
            '"' | '\'' => {
                quote = Some(ch);
                continue;
            }
            '[' => square_depth += 1,
            ']' => square_depth -= 1,
            '(' => paren_depth += 1,
            ')' => paren_depth -= 1,
            '{' => brace_depth += 1,
            '}' => brace_depth -= 1,
            '\\' => {
                escaped = true;
                continue;
            }
            _ => {}
        }

        if square_depth == 0 && paren_depth == 0 && brace_depth == 0 {
            if let Some(delimiter) = delimiters.iter().find(|delimiter| {
                rule[idx..].starts_with(**delimiter)
                    && !(**delimiter == "@" && rule[idx..].starts_with("@@"))
            }) {
                return Some((idx, *delimiter));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_ignores_delimiters_inside_attribute_selector() {
        let result = split_top_level(r#"div[a="x&&y"]&&span"#, &["&&", "||", "%%"]);

        assert_eq!(result.delimiter.as_deref(), Some("&&"));
        assert_eq!(result.parts, vec![r#"div[a="x&&y"]"#, "span"]);
    }

    #[test]
    fn escaped_delimiter_outside_quotes_is_not_split() {
        // 引号外的 `\&&`：转义符消费其后字符，分隔符不生效
        let result = split_top_level(r"a\&&b", &["&&", "||", "%%"]);
        assert_eq!(result.delimiter, None);
        assert_eq!(result.parts, vec![r"a\&&b".to_string()]);

        // `\||` 与 `\%%` 同理
        assert_eq!(split_top_level(r"a\||b", &["||"]).delimiter, None);
        assert_eq!(split_top_level(r"a\%%b", &["%%"]).delimiter, None);

        // 转义的括号也不参与深度计数：`\[` 不开层，其后 `&&` 在顶层生效
        let result = split_top_level(r"a\[x&&y", &["&&"]);
        assert_eq!(result.delimiter.as_deref(), Some("&&"));
        assert_eq!(result.parts, vec![r"a\[x".to_string(), "y".to_string()]);
    }

    #[test]
    fn unescaped_delimiter_outside_quotes_still_splits() {
        let result = split_top_level("a&&b", &["&&", "||", "%%"]);
        assert_eq!(result.delimiter.as_deref(), Some("&&"));
        assert_eq!(result.parts, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn escaped_state_resets_after_consuming_one_char() {
        // 转义只消费紧随的一个字符：`\x` 不吞掉后面的 `&&`，正常切分；
        // 第二段里的 `&&` 在引号内，不切分
        let result = split_top_level(r#"a\x&&b"c&&d""#, &["&&"]);
        assert_eq!(result.delimiter.as_deref(), Some("&&"));
        assert_eq!(
            result.parts,
            vec![r"a\x".to_string(), r#"b"c&&d""#.to_string()]
        );
    }
}
