//! 搜索结果的服务端归一化与去重键（P2/D13）。
//!
//! 去重发生在服务端：不同书源对「同一本书」的书名写法差异很大——全角与
//! 半角、夹空格、繁体、站点加的书名后缀（`（全本）` / `最新章节` / `txt下载`）。
//! `dedupeKey = JSON(["归一化书名","归一化作者"])` 写进响应，客户端按它定位
//! 「同一本」；缺书名时退化为按 `bookUrl` 唯一，避免无名条目互相合并。
//!
//! 规则保持克制：只折叠宽度/空白/大小写/繁简与**已知**后缀。带语义的括号
//! （`三体（纪念版）`）不剥，避免把不同版本误并成一本书。

use crate::model::search::SearchBook;

/// 书名末尾的站点装饰词（归一化后再匹配，全部为简化字/小写）。
/// 只收**纯装饰**词；`精校版`/`未删节` 这类可能代表不同版本，不剥（宁可不并）。
const TITLE_SUFFIXES: &[&str] = &[
    "完结",
    "全本",
    "全集",
    "完本",
    "无弹窗",
    "全文阅读",
    "免费阅读",
    "最新章节",
    "txt下载",
    "epub",
    "txt",
];

/// 作者末尾的装饰词。
const AUTHOR_SUFFIXES: &[&str] = &["著", "作者"];

const OPEN_BRACKETS: [(char, char); 8] = [
    ('(', ')'),
    ('[', ']'),
    ('{', '}'),
    ('<', '>'),
    ('「', '」'),
    ('『', '』'),
    ('《', '》'),
    ('【', '】'),
];

/// 归一化书名：全角→半角、去空白、小写、繁→简、剥已知后缀。
/// 剥完为空时回退到未剥后缀的归一化文本（`"-"`、`"（全本）"` 这类不能变成空键）。
pub fn normalize_title(title: &str) -> String {
    let base = normalize_common(title);
    let stripped = strip_title_suffixes(&base);
    if stripped.is_empty() {
        base
    } else {
        stripped
    }
}

/// 归一化作者：与书名同一套宽度/空白/大小写/繁简折叠，再剥 `著` 之类的尾巴。
pub fn normalize_author(author: &str) -> String {
    let base = normalize_common(author);
    strip_suffixes(&base, AUTHOR_SUFFIXES)
}

/// 去重键：`["归一化书名","归一化作者"]` 的 JSON 编码。
/// 用 JSON 而不是 `name|author` 拼接：书名/作者本身含分隔符时不会碰撞。
pub fn dedupe_key(name: &str, author: &str) -> String {
    serde_json::to_string(&[normalize_title(name), normalize_author(author)])
        .unwrap_or_else(|_| format!("{}|{}", name.trim(), author.trim()))
}

/// 任务结果用的去重键：书名归一化后为空（缺书名）时退化为按 bookUrl 唯一，
/// 避免所有无名条目合并成一条。
pub fn dedupe_key_for(book: &SearchBook) -> String {
    let title = normalize_title(&book.name);
    if title.is_empty() {
        return format!("url:{}", book.book_url.trim());
    }
    serde_json::to_string(&[title, normalize_author(&book.author)])
        .unwrap_or_else(|_| format!("{}|{}", book.name.trim(), book.author.trim()))
}

/// 把 `incoming` 并入 `existing`（同 dedupeKey）：补齐缺失字段、合并书源列表。
/// 先到的条目保留书名/地址（搜索结果是候选，不在这里裁决哪个源更好）。
/// 返回是否真的有字段变化（调用方据此决定是否补发 `item` 事件）。
pub fn merge_into(existing: &mut SearchBook, incoming: SearchBook) -> bool {
    let mut changed = false;
    let had_source_ids = existing.book_source_ids.is_some();
    let mut source_ids = existing.book_source_ids.take().unwrap_or_default();
    if let Some(id) = existing.book_source_id {
        if !source_ids.contains(&id) {
            source_ids.push(id);
            changed = true;
        }
    }
    if let Some(id) = incoming.book_source_id {
        if !source_ids.contains(&id) {
            source_ids.push(id);
            changed = true;
        }
    }
    if source_ids.len() > 1 || had_source_ids {
        existing.book_source_ids = Some(source_ids);
    } else if let Some(id) = source_ids.first().copied() {
        if existing.book_source_id.is_none() {
            existing.book_source_id = Some(id);
            changed = true;
        }
    }

    changed |= fill(&mut existing.cover_url, incoming.cover_url);
    changed |= fill(&mut existing.intro, incoming.intro);
    changed |= fill(&mut existing.kind, incoming.kind);
    changed |= fill(&mut existing.last_chapter, incoming.last_chapter);
    changed |= fill(&mut existing.update_time, incoming.update_time);
    changed |= fill(&mut existing.word_count, incoming.word_count);
    changed
}

fn fill<T>(slot: &mut Option<T>, incoming: Option<T>) -> bool {
    if slot.is_none() {
        if let Some(value) = incoming {
            *slot = Some(value);
            return true;
        }
    }
    false
}

fn normalize_common(text: &str) -> String {
    let folded = fold_width(text);
    let stripped: String = folded.chars().filter(|c| !c.is_whitespace()).collect();
    let lowered = stripped.to_lowercase();
    to_simplified(&lowered)
}

/// 全角 ASCII（U+FF01..=U+FF5E）与全角空格（U+3000）折成半角。
fn fold_width(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\u{3000}' => ' ',
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            _ => c,
        })
        .collect()
}

/// 繁→简折叠。`Converter` 内部有 `RefCell` 缓冲、不是 `Sync`，
/// 每次构造又要解析内嵌词典，这里按线程各建一份（归一化调用之间没有 await）。
fn to_simplified(text: &str) -> String {
    use std::cell::RefCell;
    thread_local! {
        static T2S: RefCell<zhhz::Converter> =
            RefCell::new(zhhz::Converter::new(zhhz::Config::T2s));
    }
    T2S.with(|converter| converter.borrow().convert(text))
}

fn strip_title_suffixes(text: &str) -> String {
    let mut current = text.to_string();
    loop {
        let before = current.clone();
        current = strip_suffixes(&current, TITLE_SUFFIXES);
        current = strip_trailing_bracket_group(&current);
        current = current
            .trim_end_matches(['-', '_', '·', ':', '：', ',', '，', '.', '。', '、', '|'])
            .to_string();
        if current == before {
            return current;
        }
    }
}

/// 末尾的括号组：只在括号内是「已知装饰词」或空时剥掉。
/// `(纪念版)` 这类带语义的版本标识保留——宁可不合并，也不误判同一本书。
fn strip_trailing_bracket_group(text: &str) -> String {
    let Some(last) = text.chars().last() else {
        return text.to_string();
    };
    let Some((open, _)) = OPEN_BRACKETS.iter().find(|(_, close)| *close == last) else {
        return text.to_string();
    };
    let Some(index) = text.rfind(*open) else {
        return text.to_string();
    };
    let inner = &text[index + open.len_utf8()..text.len() - last.len_utf8()];
    let inner_trimmed = inner.trim();
    if inner_trimmed.is_empty() || TITLE_SUFFIXES.contains(&inner_trimmed) {
        text[..index].to_string()
    } else {
        text.to_string()
    }
}

fn strip_suffixes(text: &str, suffixes: &[&str]) -> String {
    let mut current = text.to_string();
    loop {
        let Some(hit) = suffixes.iter().find(|suffix| {
            current.ends_with(**suffix)
                && current.len() > suffix.len()
                && (!suffix.is_ascii() || ascii_suffix_has_boundary(&current, suffix))
        }) else {
            return current;
        };
        current.truncate(current.len() - hit.len());
    }
}

/// ASCII 后缀（`txt`/`epub`）要求前面是非字母数字（`书名.txt` 可以剥，
/// `我的TXT` 不能剥成 `我的`）。
fn ascii_suffix_has_boundary(text: &str, suffix: &str) -> bool {
    text[..text.len() - suffix.len()]
        .chars()
        .next_back()
        .is_some_and(|prev| !prev.is_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_width_whitespace_and_case() {
        assert_eq!(normalize_title("Ｓａｎ Ｔｉ"), "santi");
        assert_eq!(normalize_title("三 体"), "三体");
        assert_eq!(normalize_title("三体　（全本）"), "三体");
    }

    #[test]
    fn folds_traditional_to_simplified() {
        assert_eq!(normalize_title("三體"), "三体");
        assert_eq!(normalize_author("劉慈欣"), "刘慈欣");
        assert_eq!(
            dedupe_key("三體（全本）", "劉 慈欣"),
            dedupe_key("三体", "刘慈欣")
        );
    }

    #[test]
    fn strips_known_title_suffixes_only() {
        assert_eq!(normalize_title("斗破苍穹 最新章节"), "斗破苍穹");
        assert_eq!(normalize_title("斗破苍穹(无弹窗)"), "斗破苍穹");
        assert_eq!(normalize_title("斗破苍穹【完结】"), "斗破苍穹");
        assert_eq!(normalize_title("斗破苍穹txt下载"), "斗破苍穹");
        assert_eq!(normalize_title("斗破苍穹.txt"), "斗破苍穹");
        // 带语义的版本括号不剥：宁可不合并不误并
        assert_eq!(normalize_title("三体（纪念版）"), "三体(纪念版)");
        assert_ne!(normalize_title("三体（纪念版）"), normalize_title("三体"));
        // ASCII 后缀要求词边界："我的TXT" 不能剥成 "我的"
        assert_eq!(normalize_title("我的TXT"), "我的txt");
        // 剥完为空时回退，避免所有装饰性书名合并成空键
        assert_eq!(normalize_title("（全本）"), "(全本)");
        assert_eq!(normalize_title("-"), "-");
    }

    #[test]
    fn dedupe_key_is_json_and_does_not_collide_on_pipe() {
        assert_ne!(dedupe_key("a|b", "c"), dedupe_key("a", "b|c"));
        assert_eq!(dedupe_key("三体", "刘慈欣"), r#"["三体","刘慈欣"]"#);
    }

    #[test]
    fn dedupe_key_falls_back_to_url_for_missing_names() {
        let book = SearchBook {
            book_url: "https://a.example/1".into(),
            ..SearchBook::default()
        };
        assert_eq!(dedupe_key_for(&book), "url:https://a.example/1");
    }

    #[test]
    fn strips_author_tail() {
        assert_eq!(normalize_author("刘慈欣 著"), "刘慈欣");
    }

    #[test]
    fn merge_keeps_first_and_collects_sources() {
        let mut existing = SearchBook {
            name: "三体".into(),
            author: "刘慈欣".into(),
            book_url: "https://a.example/1".into(),
            origin: "https://a.example".into(),
            book_source_id: Some(1),
            dedupe_key: Some(dedupe_key("三体", "刘慈欣")),
            ..SearchBook::default()
        };
        let incoming = SearchBook {
            name: "三體（全本）".into(),
            author: "劉慈欣".into(),
            book_url: "https://b.example/2".into(),
            origin: "https://b.example".into(),
            book_source_id: Some(2),
            intro: Some("简介".into()),
            ..SearchBook::default()
        };

        assert!(merge_into(&mut existing, incoming));

        assert_eq!(existing.name, "三体", "先到的书名保留");
        assert_eq!(existing.intro.as_deref(), Some("简介"), "缺失字段补齐");
        assert_eq!(existing.book_source_ids, Some(vec![1, 2]));
    }

    #[test]
    fn merge_preserves_single_element_source_ids() {
        let mut existing = SearchBook {
            name: "三体".into(),
            author: "刘慈欣".into(),
            book_url: "https://a.example/1".into(),
            origin: "https://a.example".into(),
            book_source_id: None,
            book_source_ids: Some(vec![7]),
            ..SearchBook::default()
        };
        let incoming = SearchBook {
            name: "三体".into(),
            author: "刘慈欣".into(),
            book_url: "https://b.example/2".into(),
            origin: "https://b.example".into(),
            ..SearchBook::default()
        };
        merge_into(&mut existing, incoming);
        assert_eq!(
            existing.book_source_ids,
            Some(vec![7]),
            "单元素列表不应被清空"
        );
    }
}
