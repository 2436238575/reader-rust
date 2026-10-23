//! 把用户提供的 TXT 拆成章节，并和本书目录匹配。
//!
//! 这里只做解析与匹配，不读写数据库。单章文本原样使用；整本文本按章节
//! 标题拆分，再依次尝试「标题完全相同 → 章节序号相同 → 按位置对齐」。
//! 匹配结果交给 handler 逐章写入正文池，预览和实际导入走同一套规则。

use crate::model::book_chapter::BookChapter;
use crate::service::local_txt_book::{is_chapter_heading_line, parse_txt_chapters};
use std::collections::HashMap;

/// 一次导入最多接受的章节数。标题行异常的 TXT 可能被切成上万段，
/// 超过这个量基本可以判定不是正常小说文本。
pub const MAX_IMPORT_CHAPTERS: usize = 20_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImportMode {
    /// 根据文本里能拆出几章自动判断。
    #[default]
    Auto,
    /// 整段文本作为当前章节。
    Single,
    /// 按章节标题拆成多章。
    Book,
}

impl ImportMode {
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "single" => Self::Single,
            "book" => Self::Book,
            _ => Self::Auto,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Single => "single",
            Self::Book => "book",
        }
    }
}

/// 从 TXT 里拆出来的一章（正文已去掉标题行）。
#[derive(Debug, Clone)]
pub struct SourceChapter {
    /// 文本内的原始顺序号，预览里用它标识用户勾选的章节。
    pub source_index: i32,
    pub title: String,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    Title,
    Number,
    Order,
    None,
}

impl MatchKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Number => "number",
            Self::Order => "order",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone)]
pub struct MatchedChapter {
    pub source: SourceChapter,
    /// 命中目录里的第几章（调用方传入的 `toc` 下标）。
    pub toc_pos: Option<usize>,
    pub match_kind: MatchKind,
}

/// 按模式拆分文本。`fallback_title` 用于单章没有标题行时的显示名。
pub fn split_text(text: &str, mode: ImportMode, fallback_title: &str) -> Vec<SourceChapter> {
    match mode {
        ImportMode::Single => vec![single_chapter(text, fallback_title)],
        ImportMode::Book => book_chapters(text),
        ImportMode::Auto => {
            let chapters = book_chapters(text);
            if chapters.len() > 1 {
                chapters
            } else {
                vec![single_chapter(text, fallback_title)]
            }
        }
    }
}

fn single_chapter(text: &str, fallback_title: &str) -> SourceChapter {
    let trimmed = text.trim();
    let mut lines = trimmed.splitn(2, '\n');
    let first = lines.next().unwrap_or("").trim_end_matches('\r').trim();
    let rest = lines.next().unwrap_or("").trim();
    if !rest.is_empty() && is_chapter_heading_line(first) {
        SourceChapter {
            source_index: 0,
            title: first.to_string(),
            content: rest.to_string(),
        }
    } else {
        SourceChapter {
            source_index: 0,
            title: fallback_title.trim().to_string(),
            content: trimmed.to_string(),
        }
    }
}

fn book_chapters(text: &str) -> Vec<SourceChapter> {
    let parsed = parse_txt_chapters("import", text);
    parsed
        .into_iter()
        .map(|chapter| {
            let content = text
                .get(chapter.start..chapter.end)
                .unwrap_or_default()
                .trim()
                .to_string();
            SourceChapter {
                source_index: chapter.index,
                title: chapter.title.trim().to_string(),
                content,
            }
        })
        .collect()
}

/// 把拆出来的章节和目录匹配。
///
/// 目录里的卷名条目（`is_volume`）和没有章节地址的条目不参与匹配。
pub fn match_chapters(source: &[SourceChapter], toc: &[BookChapter]) -> Vec<MatchedChapter> {
    let chapters: Vec<(usize, &BookChapter)> = toc
        .iter()
        .enumerate()
        .filter(|(_, chapter)| !chapter.is_volume && !chapter.url.trim().is_empty())
        .collect();
    let mut result: Vec<MatchedChapter> = source
        .iter()
        .map(|source| MatchedChapter {
            source: source.clone(),
            toc_pos: None,
            match_kind: MatchKind::None,
        })
        .collect();
    if chapters.is_empty() || result.is_empty() {
        return result;
    }

    let mut used = vec![false; chapters.len()];

    // 第一轮：标题完全相同。先做这一轮，避免内容里引用的「第 N 章」
    // 抢先占掉真正同名的那一条。
    let mut by_title: HashMap<String, Vec<usize>> = HashMap::new();
    for (pos, (_, chapter)) in chapters.iter().enumerate() {
        let key = normalize_title(&chapter.title);
        if !key.is_empty() {
            by_title.entry(key).or_default().push(pos);
        }
    }
    for item in result.iter_mut() {
        if item.source.content.trim().is_empty() {
            continue;
        }
        let key = normalize_title(&item.source.title);
        if key.is_empty() {
            continue;
        }
        let Some(candidates) = by_title.get(&key) else {
            continue;
        };
        let Some(pos) = candidates.iter().copied().find(|&pos| !used[pos]) else {
            continue;
        };
        used[pos] = true;
        item.toc_pos = Some(pos);
        item.match_kind = MatchKind::Title;
    }

    // 第二轮：章节序号相同。
    let mut by_number: HashMap<u32, Vec<usize>> = HashMap::new();
    for (pos, (_, chapter)) in chapters.iter().enumerate() {
        if let Some(number) = extract_chapter_number(&chapter.title) {
            by_number.entry(number).or_default().push(pos);
        }
    }
    for item in result.iter_mut() {
        if item.toc_pos.is_some() || item.source.content.trim().is_empty() {
            continue;
        }
        let Some(number) = extract_chapter_number(&item.source.title) else {
            continue;
        };
        let Some(candidates) = by_number.get(&number) else {
            continue;
        };
        let Some(pos) = candidates.iter().copied().find(|&pos| !used[pos]) else {
            continue;
        };
        used[pos] = true;
        item.toc_pos = Some(pos);
        item.match_kind = MatchKind::Number;
    }

    // 第三轮：按位置补齐。目录和文本都很少完全没有章节序号，剩余部分
    // 用最近的两个已知位置夹出区间，区间内按顺序一一对应。
    //
    // 目录本身带序号时，正文里带序号却匹配不上的章节不参与按顺序补齐：
    // 「第 37 章」配不上目录，说明这段文本不属于本书或目录缺章，硬按
    // 位置塞进某一章只会写错内容。
    let toc_has_numbers = chapters
        .iter()
        .any(|(_, chapter)| extract_chapter_number(&chapter.title).is_some());
    let order_eligible = |item: &MatchedChapter| {
        item.toc_pos.is_none()
            && !item.source.content.trim().is_empty()
            && !(toc_has_numbers && extract_chapter_number(&item.source.title).is_some())
    };
    let anchors: Vec<(usize, usize)> = result
        .iter()
        .enumerate()
        .filter_map(|(index, item)| item.toc_pos.map(|pos| (index, pos)))
        .collect();
    if anchors.is_empty() {
        // 完全没有任何标题/序号线索时，只能假设文本从目录第一章开始。
        for (item, pos) in result.iter_mut().zip(0..chapters.len()) {
            if !order_eligible(item) {
                continue;
            }
            item.toc_pos = Some(pos);
            item.match_kind = MatchKind::Order;
        }
        map_back_to_toc(&mut result, &chapters);
        return result;
    }

    let fill = |from: usize,
                to: usize,
                toc_start: usize,
                toc_end: usize,
                result: &mut Vec<MatchedChapter>,
                used: &mut Vec<bool>| {
        let source_positions: Vec<usize> = (from..to)
            .filter(|&index| order_eligible(&result[index]))
            .collect();
        let toc_positions: Vec<usize> = (toc_start..toc_end).filter(|&pos| !used[pos]).collect();
        for (index, pos) in source_positions.into_iter().zip(toc_positions) {
            used[pos] = true;
            result[index].toc_pos = Some(pos);
            result[index].match_kind = MatchKind::Order;
        }
    };

    // 首段：从目录开头对齐到第一个已知位置；文本比目录短时靠后对齐，
    // 让最后一个未匹配章紧挨着锚点。
    let (first_source, first_toc) = anchors[0];
    let head_sources = result[..first_source]
        .iter()
        .filter(|item| order_eligible(item))
        .count();
    let head_start = first_toc.saturating_sub(head_sources);
    fill(
        0,
        first_source,
        head_start,
        first_toc,
        &mut result,
        &mut used,
    );

    // 锚点之间与尾段：从上一个锚点往后对齐。
    for (index, &(source_pos, toc_pos)) in anchors.iter().enumerate() {
        let next = anchors.get(index + 1).copied();
        let (source_end, toc_end) = next.unwrap_or((result.len(), chapters.len()));
        fill(
            source_pos + 1,
            source_end,
            toc_pos + 1,
            toc_end,
            &mut result,
            &mut used,
        );
    }

    map_back_to_toc(&mut result, &chapters);
    result
}

fn map_back_to_toc(result: &mut [MatchedChapter], chapters: &[(usize, &BookChapter)]) {
    for item in result {
        if let Some(pos) = item.toc_pos {
            item.toc_pos = Some(chapters[pos].0);
        }
    }
}

/// 去掉空白和常见中英文标点，便于比较标题。
pub fn normalize_title(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    for ch in title.chars() {
        if ch.is_whitespace() {
            continue;
        }
        if let Some(ascii) = fullwidth_ascii(ch) {
            out.push(ascii.to_ascii_lowercase());
            continue;
        }
        if is_title_punctuation(ch) {
            continue;
        }
        out.push(ch.to_ascii_lowercase());
    }
    out
}

fn fullwidth_ascii(ch: char) -> Option<char> {
    let code = ch as u32;
    match code {
        0xFF10..=0xFF19 => Some(char::from(b'0' + (code - 0xFF10) as u8)),
        0xFF21..=0xFF3A => Some(char::from(b'A' + (code - 0xFF21) as u8)),
        0xFF41..=0xFF5A => Some(char::from(b'a' + (code - 0xFF41) as u8)),
        _ => None,
    }
}

fn is_title_punctuation(ch: char) -> bool {
    matches!(
        ch,
        ':' | '：'
            | '、'
            | '.'
            | '．'
            | ','
            | '，'
            | '。'
            | ';'
            | '；'
            | '!'
            | '！'
            | '?'
            | '？'
            | '"'
            | '\''
            | '“'
            | '”'
            | '‘'
            | '’'
            | '('
            | ')'
            | '（'
            | '）'
            | '['
            | ']'
            | '【'
            | '】'
            | '{'
            | '}'
            | '「'
            | '」'
            | '『'
            | '』'
            | '<'
            | '>'
            | '《'
            | '》'
            | '-'
            | '_'
            | '—'
            | '·'
            | '•'
            | '～'
            | '~'
            | '@'
            | '#'
            | '￥'
            | '$'
            | '%'
            | '^'
            | '&'
            | '*'
            | '+'
            | '='
            | '|'
            | '\\'
            | '/'
    )
}

/// 从「第 123 章 / 第一百二十三回」里取出序号。
///
/// 只认「章节回」三种单位：目录里的「第一卷」「第 1 部」是卷名或分部，
/// 拿来和正文标题比较会错配。
pub fn extract_chapter_number(title: &str) -> Option<u32> {
    let trimmed = title.trim_start();
    let rest = trimmed.strip_prefix('第')?;
    let mut raw = String::new();
    for ch in rest.chars() {
        if ch.is_whitespace() {
            continue;
        }
        if is_number_char(ch) {
            raw.push(ch);
            continue;
        }
        if matches!(ch, '章' | '节' | '回') {
            break;
        }
        return None;
    }
    if raw.is_empty() {
        return None;
    }
    parse_number(&raw)
}

fn is_number_char(ch: char) -> bool {
    ch.is_ascii_digit()
        || fullwidth_ascii(ch).is_some_and(|c| c.is_ascii_digit())
        || matches!(
            ch,
            '零' | '〇'
                | '一'
                | '二'
                | '两'
                | '三'
                | '四'
                | '五'
                | '六'
                | '七'
                | '八'
                | '九'
                | '十'
                | '百'
                | '千'
                | '万'
                | '壹'
                | '贰'
                | '叁'
                | '肆'
                | '伍'
                | '陆'
                | '柒'
                | '捌'
                | '玖'
                | '拾'
                | '佰'
                | '仟'
        )
}

fn parse_number(raw: &str) -> Option<u32> {
    let normalized: String = raw
        .chars()
        .map(|ch| fullwidth_ascii(ch).unwrap_or(ch))
        .collect();
    if normalized.chars().all(|ch| ch.is_ascii_digit()) {
        return normalized.parse().ok();
    }

    let has_unit = normalized
        .chars()
        .any(|ch| matches!(ch, '十' | '百' | '千' | '万' | '拾' | '佰' | '仟'));
    if !has_unit {
        // 「一〇一」这种逐位写法
        let mut digits = String::new();
        for ch in normalized.chars() {
            digits.push(char::from(b'0' + digit_value(ch)? as u8));
        }
        return digits.parse().ok();
    }

    let mut total: u64 = 0;
    let mut section: u64 = 0;
    let mut current: u64 = 0;
    for ch in normalized.chars() {
        match ch {
            '零' | '〇' => {}
            '一' | '壹' => current = 1,
            '二' | '两' | '贰' => current = 2,
            '三' | '叁' => current = 3,
            '四' | '肆' => current = 4,
            '五' | '伍' => current = 5,
            '六' | '陆' => current = 6,
            '七' | '柒' => current = 7,
            '八' | '捌' => current = 8,
            '九' | '玖' => current = 9,
            '十' | '拾' => {
                section = section.saturating_add(if current == 0 { 1 } else { current } * 10);
                current = 0;
            }
            '百' | '佰' => {
                section = section.saturating_add(if current == 0 { 1 } else { current } * 100);
                current = 0;
            }
            '千' | '仟' => {
                section = section.saturating_add(if current == 0 { 1 } else { current } * 1000);
                current = 0;
            }
            '万' => {
                total = total.saturating_add((section + current) * 10_000);
                section = 0;
                current = 0;
            }
            _ => return None,
        }
    }
    let value = total + section + current;
    u32::try_from(value).ok()
}

fn digit_value(ch: char) -> Option<u32> {
    match ch {
        '零' | '〇' => Some(0),
        '一' | '壹' => Some(1),
        '二' | '两' | '贰' => Some(2),
        '三' | '叁' => Some(3),
        '四' | '肆' => Some(4),
        '五' | '伍' => Some(5),
        '六' | '陆' => Some(6),
        '七' | '柒' => Some(7),
        '八' | '捌' => Some(8),
        '九' | '玖' => Some(9),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toc(items: &[(i32, &str)]) -> Vec<BookChapter> {
        items
            .iter()
            .map(|(index, title)| BookChapter {
                title: title.to_string(),
                url: format!("chapter-{index}"),
                index: *index,
                ..BookChapter::default()
            })
            .collect()
    }

    #[test]
    fn parses_arabic_and_chinese_numbers() {
        assert_eq!(extract_chapter_number("第12章 开始"), Some(12));
        assert_eq!(extract_chapter_number("第 １２ 章"), Some(12));
        assert_eq!(extract_chapter_number("第一百零二回"), Some(102));
        assert_eq!(extract_chapter_number("第一千零一章"), Some(1001));
        assert_eq!(extract_chapter_number("第两节"), Some(2));
        assert_eq!(extract_chapter_number("第一卷"), None);
        assert_eq!(extract_chapter_number("第十万零一章"), Some(100001));
        assert_eq!(extract_chapter_number("序章"), None);
    }

    #[test]
    fn splits_book_text_and_drops_headings_from_body() {
        let text = "第一章 开始\n正文一。\n\n第二章 继续\r\n正文二。\r\n";
        let chapters = split_text(text, ImportMode::Book, "");
        assert_eq!(chapters.len(), 2);
        assert_eq!(chapters[0].title, "第一章 开始");
        assert_eq!(chapters[0].content, "正文一。");
        assert_eq!(chapters[1].content, "正文二。");
    }

    #[test]
    fn auto_mode_treats_text_without_headings_as_one_chapter() {
        let chapters = split_text("只有一段正文。", ImportMode::Auto, "第一章");
        assert_eq!(chapters.len(), 1);
        assert_eq!(chapters[0].title, "第一章");
        assert_eq!(chapters[0].content, "只有一段正文。");
    }

    #[test]
    fn single_mode_strips_a_leading_heading() {
        let chapters = split_text("第三章 标题\n真正的正文。", ImportMode::Single, "当前章节");
        assert_eq!(chapters[0].title, "第三章 标题");
        assert_eq!(chapters[0].content, "真正的正文。");
    }

    #[test]
    fn matches_titles_before_numbers() {
        let source = split_text("第1章 同名\nA\n第2章 别的标题\nB\n", ImportMode::Book, "");
        let toc = toc(&[(0, "第1章 同名"), (1, "第2章 目录标题")]);
        let matched = match_chapters(&source, &toc);
        assert_eq!(matched[0].toc_pos, Some(0));
        assert_eq!(matched[0].match_kind, MatchKind::Title);
        assert_eq!(matched[1].toc_pos, Some(1));
        assert_eq!(matched[1].match_kind, MatchKind::Number);
    }

    #[test]
    fn order_fallback_keeps_extra_source_chapters_unmatched() {
        // 没有可用的标题或序号：按顺序对齐，多出来的章节不硬塞。
        let source = vec![
            SourceChapter {
                source_index: 0,
                title: "甲".to_string(),
                content: "A".to_string(),
            },
            SourceChapter {
                source_index: 1,
                title: "乙".to_string(),
                content: "B".to_string(),
            },
            SourceChapter {
                source_index: 2,
                title: "丙".to_string(),
                content: "C".to_string(),
            },
        ];
        let toc = toc(&[(0, "第一章"), (1, "第二章")]);
        let matched = match_chapters(&source, &toc);
        assert_eq!(matched[0].toc_pos, Some(0));
        assert_eq!(matched[1].toc_pos, Some(1));
        assert_eq!(matched[2].toc_pos, None, "目录不够长时多出的章节不硬塞");
    }

    #[test]
    fn volume_entries_are_not_matched() {
        let mut toc = toc(&[(0, "第一卷"), (1, "第一章")]);
        toc[0].is_volume = true;
        let source = split_text("第一章\n正文\n", ImportMode::Book, "");
        let matched = match_chapters(&source, &toc);
        assert_eq!(matched[0].toc_pos, Some(1), "卷名不参与匹配");
        assert_ne!(matched[0].match_kind, MatchKind::None);
    }

    #[test]
    fn head_segment_aligns_against_the_end_of_the_range() {
        // 前两章标题没有序号，第三章按序号命中目录第 10 章：
        // 前两章应贴着锚点对齐到 8、9，而不是从 0 开始。
        let source = vec![
            SourceChapter {
                source_index: 0,
                title: "序".to_string(),
                content: "A".to_string(),
            },
            SourceChapter {
                source_index: 1,
                title: "引".to_string(),
                content: "B".to_string(),
            },
            SourceChapter {
                source_index: 2,
                title: "第十章 了".to_string(),
                content: "C".to_string(),
            },
        ];
        let toc_items: Vec<(i32, String)> = (0..12)
            .map(|index| (index, format!("第{}章", index + 1)))
            .collect();
        let toc: Vec<BookChapter> = toc_items
            .iter()
            .map(|(index, title)| BookChapter {
                title: title.clone(),
                url: format!("chapter-{index}"),
                index: *index,
                ..BookChapter::default()
            })
            .collect();
        let matched = match_chapters(&source, &toc);
        assert_eq!(matched[2].toc_pos, Some(9));
        assert_eq!(matched[0].toc_pos, Some(7));
        assert_eq!(matched[1].toc_pos, Some(8));
        assert_eq!(matched[0].match_kind, MatchKind::Order);
    }
}
