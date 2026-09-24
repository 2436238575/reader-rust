use regex::Regex;
use std::collections::HashMap;
use std::sync::Mutex;

/// 书源正则的编译缓存条目上限。
///
/// 键来自书源（第三方内容），没有淘汰机制就会一直涨；超出上限时整表清空，
/// 代价只是重新编译。
const REGEX_CACHE_MAX_ENTRIES: usize = 512;

/// 已编译正则的缓存（含编译失败的结果）。
///
/// 书源规则会在每次请求里重复编译同一批正则——搜索列表、每本书的目录与正文
/// 字段都会各自编译一次，而正则编译的成本远高于匹配本身。
static REGEX_CACHE: once_cell::sync::Lazy<Mutex<HashMap<String, Option<Regex>>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(HashMap::new()));

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
    let mut cache = REGEX_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() >= REGEX_CACHE_MAX_ENTRIES && !cache.contains_key(pattern) {
        cache.clear();
    }
    cache.insert(pattern.to_string(), compiled.clone());
    compiled
}

pub fn strip_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn apply_regex_replace(input: &str, pattern: &str, replace: &str) -> String {
    if let Ok(re) = Regex::new(pattern) {
        re.replace_all(input, replace).to_string()
    } else {
        input.to_string()
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
    if !(normalized.contains("%3F")
        || normalized.contains("%3f")
        || normalized.contains("%26")
        || normalized.contains("%26")
        || normalized.contains("%3D")
        || normalized.contains("%3d"))
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
        .replace("%23", "#")
}
