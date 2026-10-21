use crate::util::bounded_map::BoundedMap;
use regex::Regex;
use std::sync::Mutex;

/// 书源正则的编译缓存条目上限。
///
/// 键来自书源（第三方内容），没有淘汰机制就会一直涨；超出上限按 FIFO
/// 逐出最旧条目（此前是整表清空，高峰期会让全部正则同时重新编译）。
const REGEX_CACHE_MAX_ENTRIES: usize = 512;

/// 已编译正则的缓存（含编译失败的结果）。
///
/// 书源规则会在每次请求里重复编译同一批正则——搜索列表、每本书的目录与正文
/// 字段都会各自编译一次，而正则编译的成本远高于匹配本身。
static REGEX_CACHE: once_cell::sync::Lazy<Mutex<BoundedMap<Option<Regex>>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(BoundedMap::new(REGEX_CACHE_MAX_ENTRIES)));

/// 取（并缓存）编译好的正则；编译失败返回 `None`，失败结果同样缓存。
pub fn compiled_regex(pattern: &str) -> Option<Regex> {
    if let Some(hit) = REGEX_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(pattern)
    {
        return hit.clone();
    }

    let compiled = Regex::new(pattern).ok();
    REGEX_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(pattern.to_string(), compiled.clone());
    compiled
}

pub fn strip_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 剥掉 HTML 标签并把连续空白并成单个空格（错误摘要/全文检索的摘要用）。
pub fn strip_html_tags(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut in_tag = false;
    for ch in value.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                output.push(' ');
            }
            _ if !in_tag => output.push(ch),
            _ => {}
        }
    }
    output.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn apply_regex_replace(input: &str, pattern: &str, replace: &str) -> String {
    // 走编译缓存：该函数对同一书源规则的每次调用模式都相同
    match compiled_regex(pattern) {
        Some(re) => re.replace_all(input, replace).to_string(),
        None => input.to_string(),
    }
}

pub fn normalize_source_url(input: &str) -> String {
    input
        .chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\r' | '\t'))
        .collect::<String>()
        .trim()
        .to_string()
}

pub fn repair_encoded_url(input: &str) -> String {
    let normalized = normalize_source_url(input);
    // 前置检查列表必须与下面的替换列表一致：漏了 %23 会导致只含 %23 的
    // URL 提前返回、永远修不到；%26/%23 是全数字编码，没有大小写变体
    if !(normalized.contains("%3F")
        || normalized.contains("%3f")
        || normalized.contains("%26")
        || normalized.contains("%3D")
        || normalized.contains("%3d")
        || normalized.contains("%23"))
    {
        return normalized;
    }

    normalized
        .replace("%3F", "?")
        .replace("%3f", "?")
        .replace("%26", "&")
        .replace("%3D", "=")
        .replace("%3d", "=")
        .replace("%23", "#")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repair_encoded_url_fixes_fragment_only_urls() {
        // 只含 %23（无 %3F/%26/%3D）的 URL 以前会因前置检查漏项直接原样返回
        assert_eq!(
            repair_encoded_url("https://a.com/b%23frag"),
            "https://a.com/b#frag"
        );
    }
}
