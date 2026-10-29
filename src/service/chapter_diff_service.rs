//! 章节段落 diff：进程内计算，无外部进程依赖。
//!
//! 用 imara-diff 的 Histogram 算法（git 默认 diff 算法的 Rust 移植，带 git
//! 同款 indent-heuristic 后处理）算行级差异，再重建段落级操作序列。
//! 输入是两段正文（编辑版与上游基准），结果按两份文本的哈希做进程内缓存，
//! 同一章反复阅读不会重算。

use imara_diff::{Algorithm, Diff, InternedInput};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// 每侧段落数与单段长度上限：防超大输入拖垮 diff。
const MAX_DIFF_PARAGRAPHS: usize = 20_000;
const MAX_PARAGRAPH_BYTES: usize = 8192;
/// 双侧合计字节上限：段数×单段字节两个维度在极端值下仍能凑出超大输入
///（2 万段 × 8KB ≈ 160MB），总量再压一道。
const MAX_TOTAL_BYTES: usize = 32 * 1024 * 1024;
/// diff 结果缓存条数：编辑过的章节不多，超过就整表清空（与其它进程内小缓存同策略）。
const MAX_CACHE_ENTRIES: usize = 64;
/// 一段删除和一段新增至少要有这么多相似度才算「同一段被改」，否则保留删+增。
const MIN_PAIR_SIMILARITY: f32 = 0.45;
/// 删除/新增块做相似度配对的 DP 规模上限；超过就按顺序配对。
const PAIRING_DP_MAX_CELLS: usize = 4096;
/// 短段落要求完全相同才算同一段（「……」这类段落不能靠碎片重合乱配）。
const MIN_SIMILARITY_CHARS: usize = 4;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DiffOp {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edited_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edited_text: Option<String>,
}

impl DiffOp {
    fn equal(base_index: usize, edited_index: usize, text: &str) -> Self {
        Self {
            kind: "equal".into(),
            base_index: Some(base_index),
            edited_index: Some(edited_index),
            base_text: Some(text.into()),
            edited_text: Some(text.into()),
        }
    }
    fn del(base_index: usize, text: &str) -> Self {
        Self {
            kind: "del".into(),
            base_index: Some(base_index),
            edited_index: None,
            base_text: Some(text.into()),
            edited_text: None,
        }
    }
    fn add(edited_index: usize, text: &str) -> Self {
        Self {
            kind: "add".into(),
            base_index: None,
            edited_index: Some(edited_index),
            base_text: None,
            edited_text: Some(text.into()),
        }
    }
    fn modify(
        base_index: Option<usize>,
        edited_index: Option<usize>,
        base_text: Option<String>,
        edited_text: Option<String>,
    ) -> Self {
        Self {
            kind: "modify".into(),
            base_index,
            edited_index,
            base_text,
            edited_text,
        }
    }
}

/// diff 算不出来的原因。正文不受影响，前端把它展示在 diff 面板里。
///
/// 进程内计算没有外部依赖，过了大小闸门就不会失败。
#[derive(Debug, Clone, PartialEq)]
pub enum DiffUnavailable {
    TooLarge,
}

impl std::fmt::Display for DiffUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge => write!(f, "章节过长，未计算差异"),
        }
    }
}

#[derive(Clone, Default)]
pub struct ChapterDiffService {
    /// md5(base)+md5(edited) → 操作序列。编辑过的章节反复阅读直接命中。
    cache: Arc<Mutex<HashMap<String, Arc<Vec<DiffOp>>>>>,
}

impl ChapterDiffService {
    pub fn new() -> Self {
        Self::default()
    }

    /// 对两段正文算段落 diff。返回 Err 表示算不出来（原因给前端展示），
    /// 调用方不应把它当成正文请求失败。
    ///
    /// 同步函数：典型一章几百段，Histogram 算出结果是毫秒级，不值得挪到
    /// 阻塞线程池；大小闸门已经把极端输入挡在外面。
    pub fn diff_texts(&self, base: &str, edited: &str) -> Result<Vec<DiffOp>, DiffUnavailable> {
        let base = split_to_paragraphs(base);
        let edited = split_to_paragraphs(edited);
        self.diff_paragraphs(&base, &edited)
    }

    /// 与 diff_texts 相同，但 equal 段回填编辑版文本。
    ///
    /// 供响应里不带完整正文的端点（导入对比）使用；正文接口的 diff 仍由
    /// 前端拿响应里的正文自行补齐，省流量。
    pub fn diff_texts_with_equal_texts(
        &self,
        base: &str,
        edited: &str,
    ) -> Result<Vec<DiffOp>, DiffUnavailable> {
        let mut ops = self.diff_texts(base, edited)?;
        let edited_paragraphs = split_to_paragraphs(edited);
        for op in &mut ops {
            if op.kind == "equal" {
                if let Some(index) = op.edited_index {
                    op.edited_text = edited_paragraphs.get(index).cloned();
                }
            }
        }
        Ok(ops)
    }

    fn diff_paragraphs(
        &self,
        base: &[String],
        edited: &[String],
    ) -> Result<Vec<DiffOp>, DiffUnavailable> {
        let total_bytes: usize = base.iter().map(|p| p.len()).sum::<usize>()
            + edited.iter().map(|p| p.len()).sum::<usize>();
        if base.len() > MAX_DIFF_PARAGRAPHS
            || edited.len() > MAX_DIFF_PARAGRAPHS
            || total_bytes > MAX_TOTAL_BYTES
            || base.iter().any(|p| p.len() > MAX_PARAGRAPH_BYTES)
            || edited.iter().any(|p| p.len() > MAX_PARAGRAPH_BYTES)
        {
            return Err(DiffUnavailable::TooLarge);
        }

        let key = cache_key(base, edited);
        if let Some(hit) = self
            .cache
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(&key)
        {
            return Ok((**hit).clone());
        }

        let mut ops = if base == edited {
            // 完全一致：不用进 diff 引擎，直接全 equal
            base.iter()
                .enumerate()
                .map(|(i, text)| DiffOp::equal(i, i, text))
                .collect()
        } else {
            compute_ops(base, edited)
        };
        slim_ops(&mut ops);

        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if cache.len() >= MAX_CACHE_ENTRIES {
            cache.clear();
        }
        cache.insert(key, Arc::new(ops.clone()));
        Ok(ops)
    }
}

/// 正文切段：剥 HTML 标签、按 `\n` 切、trim、去空行。
///
/// 与前端 `frontend/src/utils/chapterDiff.ts` 的 `splitToParagraphs` 同口径：
/// 两边必须一致，diff 的段落下标才能和编辑版正文对齐。
pub(crate) fn split_to_paragraphs(text: &str) -> Vec<String> {
    static TAG: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let tag = TAG.get_or_init(|| regex::Regex::new("<[^>]+>").expect("HTML 标签正则应可编译"));
    tag.replace_all(text, "")
        .split('\n')
        .map(|line| line.trim().trim_matches('\u{feff}').trim())
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// 喂给 diff 引擎的文本：段落之间换行，非空时补末尾换行。
fn diff_input_text(paragraphs: &[String]) -> String {
    if paragraphs.is_empty() {
        String::new()
    } else {
        format!("{}\n", paragraphs.join("\n"))
    }
}

fn cache_key(base: &[String], edited: &[String]) -> String {
    format!(
        "{}:{}",
        crate::util::hash::md5_hex(&base.join("\n")),
        crate::util::hash::md5_hex(&edited.join("\n"))
    )
}

/// 行级 diff → 段落级操作序列。
///
/// hunk 之间未覆盖的行全是 equal；hunk 内先出全部删除再出全部新增（与
/// `git diff --unified=0` 的输出顺序一致），最后交给 `merge_modify`
/// 把同一块里相似的删/增配对成 modify。
fn compute_ops(base: &[String], edited: &[String]) -> Vec<DiffOp> {
    let base_text = diff_input_text(base);
    let edited_text = diff_input_text(edited);
    let input = InternedInput::new(base_text.as_str(), edited_text.as_str());
    let mut diff = Diff::compute(Algorithm::Histogram, &input);
    // git 默认开启的 indent-heuristic 滑动，让变更块边界落在更自然的位置
    diff.postprocess_lines(&input);

    let mut ops: Vec<DiffOp> = Vec::new();
    let mut base_pos = 0usize;
    let mut edited_pos = 0usize;
    for hunk in diff.hunks() {
        let base_start = hunk.before.start as usize;
        let base_end = hunk.before.end as usize;
        let edited_start = hunk.after.start as usize;
        let edited_end = hunk.after.end as usize;
        // hunk 前的 equal 区
        while base_pos < base_start && edited_pos < edited_start {
            ops.push(DiffOp::equal(base_pos, edited_pos, &base[base_pos]));
            base_pos += 1;
            edited_pos += 1;
        }
        for (index, text) in (base_start..base_end).zip(&base[base_start..base_end]) {
            ops.push(DiffOp::del(index, text));
        }
        for (index, text) in (edited_start..edited_end).zip(&edited[edited_start..edited_end]) {
            ops.push(DiffOp::add(index, text));
        }
        base_pos = base_end;
        edited_pos = edited_end;
    }
    // 尾部剩余全 equal
    while base_pos < base.len() && edited_pos < edited.len() {
        ops.push(DiffOp::equal(base_pos, edited_pos, &base[base_pos]));
        base_pos += 1;
        edited_pos += 1;
    }

    merge_modify(ops)
}

/// 响应里省掉可以从正文还原的文本：
/// equal 两边的文本都不发；add 只发新增文本；del 只发被删文本；
/// modify 两段都发（段内对照要用）。equal 行的文本由前端用编辑版正文补齐。
fn slim_ops(ops: &mut [DiffOp]) {
    for op in ops {
        match op.kind.as_str() {
            "equal" => {
                op.base_text = None;
                op.edited_text = None;
            }
            "add" => op.base_text = None,
            "del" => op.edited_text = None,
            _ => {}
        }
    }
}

/// 把连续的 del/add 块配对成 modify。
///
/// 同一替换块输出的是「先连续 `-`，再连续 `+`」。只合并紧挨着的最后一对
/// 的话，三行改词会变成 `del, del, modify, add, add`；这里把整块的删除
/// 序列和新增序列做相似度对齐：相似度够的配成 modify，配不上的保留
/// del/add（真·重写）。
fn merge_modify(ops: Vec<DiffOp>) -> Vec<DiffOp> {
    let mut merged: Vec<DiffOp> = Vec::with_capacity(ops.len());
    let mut index = 0;
    while index < ops.len() {
        if ops[index].kind == "equal" {
            merged.push(ops[index].clone());
            index += 1;
            continue;
        }
        let start = index;
        while index < ops.len() && (ops[index].kind == "del" || ops[index].kind == "add") {
            index += 1;
        }
        merged.extend(pair_block(&ops[start..index]));
    }
    merged
}

fn pair_block(block: &[DiffOp]) -> Vec<DiffOp> {
    let dels: Vec<&DiffOp> = block.iter().filter(|op| op.kind == "del").collect();
    let adds: Vec<&DiffOp> = block.iter().filter(|op| op.kind == "add").collect();
    if dels.is_empty() || adds.is_empty() {
        return block.to_vec();
    }
    let mut paired: Vec<DiffOp> = Vec::new();
    for (del_index, add_index) in align_pairs(&dels, &adds) {
        match (del_index, add_index) {
            (Some(del), Some(add)) => paired.push(DiffOp::modify(
                dels[del].base_index,
                adds[add].edited_index,
                dels[del].base_text.clone(),
                adds[add].edited_text.clone(),
            )),
            (Some(del), None) => paired.push(dels[del].clone()),
            (None, Some(add)) => paired.push(adds[add].clone()),
            (None, None) => {}
        }
    }
    paired
}

/// 有序对齐：返回 `(删除下标, 新增下标)` 序列，未配对的一侧为 None。
/// 只有相似度达到门槛的允许配对；规模过大时退化为按顺序配对。
fn align_pairs(dels: &[&DiffOp], adds: &[&DiffOp]) -> Vec<(Option<usize>, Option<usize>)> {
    let d = dels.len();
    let a = adds.len();

    if d.saturating_mul(a) > PAIRING_DP_MAX_CELLS {
        let mut out = Vec::with_capacity(d.max(a));
        for index in 0..d.max(a) {
            match (index < d, index < a) {
                (true, true) => {
                    let similarity =
                        paragraph_similarity(op_text(dels[index]), op_text(adds[index]));
                    if similarity >= MIN_PAIR_SIMILARITY {
                        out.push((Some(index), Some(index)));
                    } else {
                        out.push((Some(index), None));
                        out.push((None, Some(index)));
                    }
                }
                (true, false) => out.push((Some(index), None)),
                (false, true) => out.push((None, Some(index))),
                (false, false) => {}
            }
        }
        return out;
    }

    // dp[i][j] = 从 (i,j) 出发能拿到的最大相似度总和
    let mut dp = vec![vec![0f32; a + 1]; d + 1];
    for i in (0..d).rev() {
        for j in (0..a).rev() {
            let mut best = dp[i + 1][j].max(dp[i][j + 1]);
            let similarity = paragraph_similarity(op_text(dels[i]), op_text(adds[j]));
            if similarity >= MIN_PAIR_SIMILARITY {
                best = best.max(dp[i + 1][j + 1] + similarity);
            }
            dp[i][j] = best;
        }
    }

    let mut out = Vec::with_capacity(d + a);
    let (mut i, mut j) = (0usize, 0usize);
    while i < d && j < a {
        let similarity = paragraph_similarity(op_text(dels[i]), op_text(adds[j]));
        if similarity >= MIN_PAIR_SIMILARITY
            && (dp[i][j] - (dp[i + 1][j + 1] + similarity)).abs() < 1e-6
        {
            out.push((Some(i), Some(j)));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            out.push((Some(i), None));
            i += 1;
        } else {
            out.push((None, Some(j)));
            j += 1;
        }
    }
    while i < d {
        out.push((Some(i), None));
        i += 1;
    }
    while j < a {
        out.push((None, Some(j)));
        j += 1;
    }
    out
}

fn op_text(op: &DiffOp) -> &str {
    op.base_text
        .as_deref()
        .or(op.edited_text.as_deref())
        .unwrap_or("")
}

/// 段落相似度：去掉空白后按字符二元组算 Dice 系数。
/// 短段落（< 4 字）没有足够的二元组，改用公共前缀占短串的比例——
/// 「二」→「二改」算同一段，「……」和别的短句不会靠碎片重合乱配。
fn paragraph_similarity(left: &str, right: &str) -> f32 {
    let left = normalize_for_similarity(left);
    let right = normalize_for_similarity(right);
    if left == right {
        return 1.0;
    }
    let left_chars: Vec<char> = left.chars().collect();
    let right_chars: Vec<char> = right.chars().collect();
    if left_chars.len() < MIN_SIMILARITY_CHARS || right_chars.len() < MIN_SIMILARITY_CHARS {
        let shorter = left_chars.len().min(right_chars.len());
        if shorter == 0 {
            return 0.0;
        }
        let common_prefix = left_chars
            .iter()
            .zip(right_chars.iter())
            .take_while(|(left, right)| left == right)
            .count();
        return common_prefix as f32 / shorter as f32;
    }
    let mut left_bigrams: HashMap<(char, char), usize> = HashMap::new();
    for pair in left_chars.windows(2) {
        *left_bigrams.entry((pair[0], pair[1])).or_insert(0) += 1;
    }
    let mut shared = 0usize;
    let mut right_total = 0usize;
    let mut right_bigrams: HashMap<(char, char), usize> = HashMap::new();
    for pair in right_chars.windows(2) {
        *right_bigrams.entry((pair[0], pair[1])).or_insert(0) += 1;
        right_total += 1;
    }
    for (pair, right_count) in right_bigrams {
        if let Some(left_count) = left_bigrams.get(&pair) {
            shared += (*left_count).min(right_count);
        }
    }
    let left_total = left_chars.len() - 1;
    if left_total + right_total == 0 {
        return 0.0;
    }
    (2 * shared) as f32 / (left_total + right_total) as f32
}

fn normalize_for_similarity(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paragraphs(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|line| line.to_string()).collect()
    }

    fn kinds(ops: &[DiffOp]) -> Vec<&str> {
        ops.iter().map(|op| op.kind.as_str()).collect()
    }

    #[test]
    fn identical_paragraphs_are_all_equal() {
        let base = paragraphs(&["一", "二", "三"]);
        let ops = ChapterDiffService::new()
            .diff_paragraphs(&base, &base.clone())
            .unwrap();
        assert_eq!(kinds(&ops), ["equal", "equal", "equal"]);
        // 响应形态：equal 行不带文本
        assert!(ops[0].base_text.is_none() && ops[0].edited_text.is_none());
    }

    #[test]
    fn changed_line_pairs_into_modify() {
        let ops = ChapterDiffService::new()
            .diff_paragraphs(
                &paragraphs(&["一", "二", "三"]),
                &paragraphs(&["一", "二改", "三"]),
            )
            .unwrap();
        assert_eq!(kinds(&ops), ["equal", "modify", "equal"]);
        assert_eq!(ops[1].base_index, Some(1));
        assert_eq!(ops[1].edited_index, Some(1));
        assert_eq!(ops[1].base_text.as_deref(), Some("二"));
        assert_eq!(ops[1].edited_text.as_deref(), Some("二改"));
    }

    #[test]
    fn insert_at_start_comes_before_equal() {
        let ops = ChapterDiffService::new()
            .diff_paragraphs(&paragraphs(&["甲"]), &paragraphs(&["新", "甲"]))
            .unwrap();
        assert_eq!(kinds(&ops), ["add", "equal"]);
        assert_eq!(ops[1].base_index, Some(0));
        assert_eq!(ops[1].edited_index, Some(1));
    }

    #[test]
    fn deleting_everything_is_all_del() {
        let ops = ChapterDiffService::new()
            .diff_paragraphs(&paragraphs(&["甲", "乙"]), &[])
            .unwrap();
        assert_eq!(kinds(&ops), ["del", "del"]);
    }

    #[test]
    fn adding_to_empty_base_is_all_add() {
        let ops = ChapterDiffService::new()
            .diff_paragraphs(&[], &paragraphs(&["甲"]))
            .unwrap();
        assert_eq!(kinds(&ops), ["add"]);
    }

    /// 同一替换块的多行删除 + 多行新增：按相似度逐行配对，不能只合并最后一对。
    #[test]
    fn multi_line_replacement_pairs_every_line() {
        let base = paragraphs(&[
            "也是在这时，脑海中的反派系统再度出声了。",
            "【所以宿主不要犹豫啊，横竖都是一死，不如我们先看看眼前这个女主的滋味如何？】",
            "“按照剧情，男主的小弟很快就要过来了吧，你还有心思让我去理女主？”",
            "【我们是反派啊，就该做反派的事情才能获得情绪值！】",
            "池小橙没听这废物反派系统的瞎比比，保住小命才是最重要的。",
        ]);
        let edited = paragraphs(&[
            "也是在这时，脑海中的反派系统再度出声了。",
            "【所以宿主不要犹豫啊，横竖都是一死，不如我们先尝尝眼前这个女主的滋味如何？】",
            "“按照剧情，男主的小弟很快就要过来了吧，你还有心思让我去调教女主？”",
            "【我们是反派啊，就该做反派的事情才能获得技能点！】",
            "池小橙没听这废物反派系统的瞎比比，保住小命才是最重要的。",
        ]);
        let ops = ChapterDiffService::new()
            .diff_paragraphs(&base, &edited)
            .unwrap();
        assert_eq!(
            kinds(&ops),
            ["equal", "modify", "modify", "modify", "equal"]
        );
        assert_eq!(ops[1].base_index, Some(1));
        assert_eq!(ops[1].edited_index, Some(1));
        assert!(ops[2].base_text.as_deref().unwrap().contains("理女主"));
        assert!(ops[2].edited_text.as_deref().unwrap().contains("调教女主"));
    }

    #[test]
    fn dissimilar_paragraphs_stay_del_and_add() {
        let ops = ChapterDiffService::new()
            .diff_paragraphs(
                &paragraphs(&["今天天气很好，我们一起去公园散步吧。"]),
                &paragraphs(&["系统提示：任务完成，奖励已发放。"]),
            )
            .unwrap();
        assert_eq!(kinds(&ops), ["del", "add"]);
    }

    /// 删除比新增多的替换块：相似的那对配成 modify，配不上的保留 del。
    /// 真实引擎遇到「整块保留一行」时会把那行锚成 equal，走不到配对逻辑，
    /// 所以这里直接构造 del/del/add 序列喂 pair_block。
    #[test]
    fn uneven_blocks_pair_similar_lines() {
        let block = vec![
            DiffOp::del(0, "第一段文字内容比较长，保持不变。"),
            DiffOp::del(1, "第二段文字内容比较长，也会保留。"),
            DiffOp::add(0, "第一段文字内容比较长，保持不变。"),
        ];
        let ops = pair_block(&block);
        assert_eq!(kinds(&ops), ["modify", "del"]);
        assert_eq!(ops[0].base_index, Some(0));
        assert_eq!(ops[0].edited_index, Some(0));
        assert_eq!(ops[1].base_index, Some(1));
    }

    #[test]
    fn oversized_input_is_rejected() {
        let edited = paragraphs(&["乙"]);
        let big_paragraph = vec!["段".repeat(MAX_PARAGRAPH_BYTES)];
        assert_eq!(
            ChapterDiffService::new()
                .diff_paragraphs(&big_paragraph, &edited)
                .unwrap_err(),
            DiffUnavailable::TooLarge
        );
        let too_many: Vec<String> = (0..=MAX_DIFF_PARAGRAPHS).map(|i| i.to_string()).collect();
        assert_eq!(
            ChapterDiffService::new()
                .diff_paragraphs(&too_many, &edited)
                .unwrap_err(),
            DiffUnavailable::TooLarge
        );
    }

    #[test]
    fn repeated_diff_hits_cache() {
        let service = ChapterDiffService::new();
        let base = paragraphs(&["一", "二"]);
        let edited = paragraphs(&["一", "二改"]);
        let first = service.diff_paragraphs(&base, &edited).unwrap();
        let second = service.diff_paragraphs(&base, &edited).unwrap();
        assert_eq!(first, second);
        assert_eq!(
            service
                .cache
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .len(),
            1,
            "同一对文本只应缓存一条"
        );
    }

    #[test]
    fn split_to_paragraphs_strips_tags_and_blank_lines() {
        let text = "<p>第一段</p>\n\n  <div>第二段</div>\r\n\u{3000}\n第三段";
        assert_eq!(
            split_to_paragraphs(text),
            vec![
                "第一段".to_string(),
                "第二段".to_string(),
                "第三段".to_string()
            ]
        );
    }

    #[test]
    fn empty_array_produces_empty_input_not_a_blank_line() {
        assert_eq!(diff_input_text(&[]), "");
        assert_eq!(diff_input_text(&["甲".to_string()]), "甲\n");
        assert_eq!(
            diff_input_text(&["甲".to_string(), "乙".to_string()]),
            "甲\n乙\n"
        );
    }

    #[test]
    fn slim_ops_drops_text_that_can_be_recovered_from_content() {
        let mut ops = vec![
            DiffOp::equal(0, 0, "同"),
            DiffOp::add(1, "新增"),
            DiffOp::del(1, "删除"),
            DiffOp {
                kind: "modify".into(),
                base_index: Some(2),
                edited_index: Some(2),
                base_text: Some("旧".into()),
                edited_text: Some("新".into()),
            },
        ];
        slim_ops(&mut ops);
        assert!(ops[0].base_text.is_none() && ops[0].edited_text.is_none());
        assert!(ops[1].base_text.is_none());
        assert!(ops[1].edited_text.is_some());
        assert!(ops[2].base_text.is_some());
        assert!(ops[2].edited_text.is_none());
        assert!(ops[3].base_text.is_some() && ops[3].edited_text.is_some());
    }

    #[test]
    fn diff_with_equal_texts_fills_context_for_contentless_endpoints() {
        let service = ChapterDiffService::new();
        let base = "第一段\n改掉的旧句\n第三段\n第四段";
        let edited = "第一段\n改掉的新句\n第三段\n第四段";
        let ops = service
            .diff_texts_with_equal_texts(base, edited)
            .expect("小输入应可计算");
        let equal_texts: Vec<Option<&str>> = ops
            .iter()
            .filter(|op| op.kind == "equal")
            .map(|op| op.edited_text.as_deref())
            .collect();
        // equal 段带上了编辑版文本，前端不再需要正文来补
        assert_eq!(
            equal_texts,
            vec![Some("第一段"), Some("第三段"), Some("第四段")]
        );
        // modify 两段照常都带
        let modified = ops
            .iter()
            .find(|op| op.kind == "modify")
            .expect("应有 modify");
        assert_eq!(modified.base_text.as_deref(), Some("改掉的旧句"));
        assert_eq!(modified.edited_text.as_deref(), Some("改掉的新句"));
    }
}
