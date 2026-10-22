//! 章节段落 diff：调系统 git（`git diff --no-index --unified=0`）。
//!
//! git 是必选依赖：二进制路径由 `GIT_PATH` 指定，留空时取 PATH 里的 `git`。
//! 不可用时**不静默回退**——正文照常返回，差异字段缺省并带上原因，由前端展示。
//!
//! 输入是两段正文（编辑版与上游基准），这里负责切段、写临时文件、解析
//! unified 输出并重建段落级操作序列。结果按两份文本的哈希做进程内缓存，
//! 同一章反复阅读不会反复起 git 进程。

use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::process::Command;

/// 每侧段落数与单段长度上限：防超大输入把临时文件和 git 拖垮。
const MAX_DIFF_PARAGRAPHS: usize = 20_000;
const MAX_PARAGRAPH_BYTES: usize = 8192;
/// 双侧合计字节上限：段数×单段字节两个维度在极端值下仍能凑出超大输入
///（2 万段 × 8KB ≈ 160MB），总量再压一道。
const MAX_TOTAL_BYTES: usize = 32 * 1024 * 1024;
const GIT_DIFF_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// 同一时刻最多 4 个 git 进程（每个都要读写临时文件 + 全量 diff）。
const MAX_CONCURRENT_GIT: usize = 4;
/// 往期遗留临时文件的清理门槛：取消/崩溃留下的文件超过这个年龄就回收。
const STALE_TEMP_AGE: std::time::Duration = std::time::Duration::from_secs(600);
/// diff 结果缓存条数：编辑过的章节不多，超过就整表清空（与其它进程内小缓存同策略）。
const MAX_CACHE_ENTRIES: usize = 64;
/// 一段删除和一段新增至少要有这么多相似度才算「同一段被改」，否则保留删+增。
const MIN_PAIR_SIMILARITY: f32 = 0.45;
/// 删除/新增块做相似度配对的 DP 规模上限；超过就按顺序配对。
const PAIRING_DP_MAX_CELLS: usize = 4096;
/// 短段落要求完全相同才算同一段（「……」这类段落不能靠碎片重合乱配）。
const MIN_SIMILARITY_CHARS: usize = 4;

/// 临时文件的兜底清理：正常路径会显式删除并 disarm；取消、提前 return、
/// panic 时由 Drop 兜住（异步代码里没法在 Drop 里 await，用同步删除尽力而为）。
struct TempFiles {
    paths: Vec<PathBuf>,
}

impl TempFiles {
    fn new(paths: Vec<PathBuf>) -> Self {
        Self { paths }
    }

    /// 显式清理已完成的路径：之后 Drop 不再重复删。
    fn disarm(&mut self) {
        self.paths.clear();
    }
}

impl Drop for TempFiles {
    fn drop(&mut self) {
        for path in &self.paths {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// 启动时清一次往期遗留：取消/崩溃留下的 `reader-git-diff-*` 不在任何
/// 测试清理前缀里，不主动扫就永远留在 %TEMP%。只动超过门槛的旧文件。
fn sweep_stale_temp_files() {
    sweep_stale_temp_files_in(&std::env::temp_dir());
}

/// 清扫逻辑本体（目录注入便于测试）。
fn sweep_stale_temp_files_in(dir: &std::path::Path) {
    let cutoff = std::time::SystemTime::now()
        .checked_sub(STALE_TEMP_AGE)
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with("reader-git-diff-")
        {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|t| t < cutoff)
            .unwrap_or(false);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

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
#[derive(Debug, Clone, PartialEq)]
pub enum DiffUnavailable {
    GitMissing,
    TooLarge,
    Timeout,
    Failed(String),
    Parse(String),
    Io(String),
}

impl std::fmt::Display for DiffUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GitMissing => {
                write!(
                    f,
                    "服务器上找不到 git，未计算差异（可用 GIT_PATH 指定路径）"
                )
            }
            Self::TooLarge => write!(f, "章节过长，未计算差异"),
            Self::Timeout => write!(f, "计算差异超时，未计算"),
            Self::Failed(_) => write!(f, "git 执行失败，未计算差异"),
            Self::Parse(_) => write!(f, "差异结果解析失败，未计算差异"),
            Self::Io(_) => write!(f, "临时文件读写失败，未计算差异"),
        }
    }
}

#[derive(Clone)]
pub struct GitDiffService {
    /// git 可执行文件：空串表示用 PATH 里的 `git`。
    git_bin: String,
    /// git 可用性探测缓存：(结果, 探测时刻)。false 结果 60s 后重试——
    /// 用户中途装上 git 不用重启进程。
    available: Arc<Mutex<Option<(bool, std::time::Instant)>>>,
    /// 缺 git 的警告只打一次，避免每章都刷日志。
    warned_missing: Arc<AtomicBool>,
    /// 并发闸门：同一时刻最多 MAX_CONCURRENT_GIT 个 git 进程
    semaphore: Arc<tokio::sync::Semaphore>,
    /// md5(base)+md5(edited) → 操作序列。编辑过的章节反复阅读直接命中。
    cache: Arc<Mutex<HashMap<String, Arc<Vec<DiffOp>>>>>,
}

impl GitDiffService {
    pub fn new(git_path: String) -> Self {
        sweep_stale_temp_files();
        Self {
            git_bin: git_path.trim().to_string(),
            available: Arc::new(Mutex::new(None)),
            warned_missing: Arc::new(AtomicBool::new(false)),
            semaphore: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_GIT)),
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn bin(&self) -> &str {
        if self.git_bin.is_empty() {
            "git"
        } else {
            &self.git_bin
        }
    }

    /// 系统里有没有 git。false 结果缓存 60s，之后重新探测。
    async fn git_available(&self) -> bool {
        {
            let cached = self
                .available
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if let Some((available, at)) = *cached {
                if available || at.elapsed() < std::time::Duration::from_secs(60) {
                    return available;
                }
            }
        }
        let available = Command::new(self.bin())
            .arg("--version")
            .output()
            .await
            .map(|output| output.status.success())
            .unwrap_or(false);
        if !available && !self.warned_missing.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                git = self.bin(),
                "系统未找到可用的 git，章节编辑版将无法计算差异；可用 GIT_PATH 指定 git 路径"
            );
        }
        *self
            .available
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) =
            Some((available, std::time::Instant::now()));
        available
    }

    /// 对两段正文算段落 diff。返回 Err 表示算不出来（原因给前端展示），
    /// 调用方不应把它当成正文请求失败。
    pub async fn diff_texts(
        &self,
        base: &str,
        edited: &str,
    ) -> Result<Vec<DiffOp>, DiffUnavailable> {
        let base = split_to_paragraphs(base);
        let edited = split_to_paragraphs(edited);
        self.diff_paragraphs(&base, &edited).await
    }

    async fn diff_paragraphs(
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
            // 完全一致：不进 git，直接全 equal
            base.iter()
                .enumerate()
                .map(|(i, text)| DiffOp::equal(i, i, text))
                .collect()
        } else {
            if !self.git_available().await {
                return Err(DiffUnavailable::GitMissing);
            }
            self.run_git_diff(base, edited).await?
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

    async fn run_git_diff(
        &self,
        base: &[String],
        edited: &[String],
    ) -> Result<Vec<DiffOp>, DiffUnavailable> {
        // 并发闸门先于写临时文件：否则排队中的请求各自先写 32MB 落盘，
        // 闸门外的等待者照样把磁盘/IO 打满（闸门形同虚设）
        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|_| DiffUnavailable::Failed("diff 并发闸门已关闭".to_string()))?;

        let temp_dir = std::env::temp_dir();
        let tag = format!(
            "reader-git-diff-{}-{}",
            std::process::id(),
            crate::util::crypto::random_string(6)
        );
        let base_path: PathBuf = temp_dir.join(format!("{tag}-a.txt"));
        let edited_path: PathBuf = temp_dir.join(format!("{tag}-b.txt"));
        let write_result = async {
            write_private_file(&base_path, git_text(base).as_bytes()).await?;
            write_private_file(&edited_path, git_text(edited).as_bytes()).await?;
            Ok::<(), std::io::Error>(())
        }
        .await;
        if let Err(e) = write_result {
            // 写失败（含第二个文件没写成）：两个路径都清一遍，不留半成品
            let _ = tokio::fs::remove_file(&base_path).await;
            let _ = tokio::fs::remove_file(&edited_path).await;
            return Err(DiffUnavailable::Io(e.to_string()));
        }
        // 兜底清理：后面任一提前 return/任务被取消/panic 都不会漏文件
        let mut cleanup = TempFiles::new(vec![base_path.clone(), edited_path.clone()]);

        // kill_on_drop：超时分支会把子进程杀掉（否则超时只warn不杀，
        // Windows 下文件还被占着删不掉，泄漏 tmp 文件）
        let run = tokio::time::timeout(
            GIT_DIFF_TIMEOUT,
            Command::new(self.bin())
                .arg("diff")
                .arg("--no-index")
                .arg("--unified=0")
                .arg("--")
                .arg(&base_path)
                .arg(&edited_path)
                .kill_on_drop(true)
                .output(),
        )
        .await;
        // 先等一个 tick 让 kill_on_drop 生效，再删临时文件（Windows 下进程退出
        // 到文件句柄释放有延迟，删除失败再补一次）
        for path in [&base_path, &edited_path] {
            if tokio::fs::remove_file(path).await.is_err() {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                let _ = tokio::fs::remove_file(path).await;
            }
        }
        cleanup.disarm();

        let output = match run {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => return Err(DiffUnavailable::Failed(e.to_string())),
            Err(_) => {
                tracing::debug!("git diff 执行超时（{GIT_DIFF_TIMEOUT:?}）");
                return Err(DiffUnavailable::Timeout);
            }
        };
        // git diff --no-index：退出码 0=无差异，1=有差异，其余=错误
        let status = output.status.code().unwrap_or(-1);
        if status != 0 && status != 1 {
            let stderr: String = String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(200)
                .collect();
            tracing::debug!("git diff 执行失败（退出码 {status}）：{stderr}");
            return Err(DiffUnavailable::Failed(stderr));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        parse_unified_diff(&text, base, edited).map_err(|reason| {
            tracing::debug!("git diff 输出解析失败：{reason}");
            DiffUnavailable::Parse(reason)
        })
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

/// 写进临时文件的文本：段落之间换行，非空时补末尾换行。
/// 空数组必须写成**空文件**——补一个 `\n` 会变成「一个空行」，git 会把
/// 它当成真实的空段落，删除整章时多出一条假的新增。
fn git_text(paragraphs: &[String]) -> String {
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

/// 写临时文件：Unix 下收紧到 0600（多用户主机上别让人读到正文）。
async fn write_private_file(path: &PathBuf, bytes: &[u8]) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        use tokio::io::AsyncWriteExt;
        tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .await?
            .write_all(bytes)
            .await?;
        return Ok(());
    }
    #[cfg(not(unix))]
    {
        tokio::fs::write(path, bytes).await
    }
}

/// 解析 `git diff --unified=0` 的输出，重建完整段落级操作序列。
///
/// hunk 头 `@@ -a,b +c,d @@`：a/c 是 1-based 起始行；行数为 0 时起始行指向
/// 插入/删除点之前的行（该侧 hunk 不含行）。hunk 之间未覆盖的行全是 equal。
fn parse_unified_diff(
    text: &str,
    base: &[String],
    edited: &[String],
) -> Result<Vec<DiffOp>, String> {
    let mut ops: Vec<DiffOp> = Vec::new();
    let mut base_pos = 0usize; // 下一个未消费的基准行（0-based 段下标）
    let mut edited_pos = 0usize;

    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        if !line.starts_with("@@") {
            continue;
        }
        let (ha, hb, hc, hd) = parse_hunk_header(line)?;
        // hunk 覆盖的起始行（转 0-based）：行数 0 时 ha/hc 是「之前的行」
        let hunk_base_start = if hb == 0 { ha + 1 } else { ha }.saturating_sub(1);
        let hunk_edited_start = if hd == 0 { hc + 1 } else { hc }.saturating_sub(1);
        if hunk_base_start < base_pos || hunk_edited_start < edited_pos {
            return Err(format!("hunk 行号回退: {line}"));
        }
        if hunk_base_start > base.len() || hunk_edited_start > edited.len() {
            return Err(format!("hunk 行号超出文本行数: {line}"));
        }
        // hunk 前的 equal 区
        while base_pos < hunk_base_start && edited_pos < hunk_edited_start {
            ops.push(DiffOp::equal(base_pos, edited_pos, &base[base_pos]));
            base_pos += 1;
            edited_pos += 1;
        }
        // hunk 体：peek 看下一行，遇到新 hunk 头就停（不吃掉它）
        while let Some(&line) = lines.peek() {
            if line.starts_with("@@") {
                break;
            }
            lines.next();
            if let Some(text) = line.strip_prefix('-') {
                ops.push(DiffOp::del(base_pos, text));
                base_pos += 1;
            } else if let Some(text) = line.strip_prefix('+') {
                ops.push(DiffOp::add(edited_pos, text));
                edited_pos += 1;
            } else if let Some(text) = line.strip_prefix(' ') {
                ops.push(DiffOp::equal(base_pos, edited_pos, text));
                base_pos += 1;
                edited_pos += 1;
            }
            // "\\ No newline…" 与其余噪声行忽略
        }
    }

    // 尾部剩余全 equal
    while base_pos < base.len() && edited_pos < edited.len() {
        ops.push(DiffOp::equal(base_pos, edited_pos, &base[base_pos]));
        base_pos += 1;
        edited_pos += 1;
    }
    if base_pos != base.len() || edited_pos != edited.len() {
        return Err(format!(
            "行数对不上：基准 {}/{}，编辑 {}/{}",
            base_pos,
            base.len(),
            edited_pos,
            edited.len()
        ));
    }

    Ok(merge_modify(ops))
}

/// `@@ -a,b +c,d @@` → (a, b, c, d)；行数缺省为 1。
fn parse_hunk_header(line: &str) -> Result<(usize, usize, usize, usize), String> {
    let mut parts = line.split_whitespace();
    let _at = parts.next();
    let base_part = parts.next().ok_or("hunk 头缺基准段")?;
    let edited_part = parts.next().ok_or("hunk 头缺编辑段")?;
    let parse_side = |part: &str, prefix: char| -> Result<(usize, usize), String> {
        let part = part.strip_prefix(prefix).ok_or("hunk 头前缀不对")?;
        let mut split = part.split(',');
        let start: usize = split
            .next()
            .and_then(|v| v.parse().ok())
            .ok_or("hunk 头起始行不是数字")?;
        let count: usize = split.next().and_then(|v| v.parse().ok()).unwrap_or(1);
        Ok((start, count))
    };
    let (ha, hb) = parse_side(base_part, '-')?;
    let (hc, hd) = parse_side(edited_part, '+')?;
    Ok((ha, hb, hc, hd))
}

/// 把连续的 del/add 块配对成 modify。
///
/// git 对同一替换块输出的是「先连续 `-`，再连续 `+`」。旧实现只合并紧挨着的
/// 最后一个 del 和第一个 add，三行改词会变成 `del, del, modify, add, add`。
/// 这里把整块的删除序列和新增序列做相似度对齐：相似度够的配成 modify，
/// 配不上的保留 del/add（真·重写）。
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

    fn kinds(ops: &[DiffOp]) -> Vec<&str> {
        ops.iter().map(|op| op.kind.as_str()).collect()
    }

    #[test]
    fn parses_hunks_and_fills_equal_gaps() {
        // 基准 5 行，第 2 行改、第 4 行删、末尾加一行
        let base: Vec<String> = ["一", "二", "三", "四", "五"].map(String::from).into();
        let edited: Vec<String> = ["一", "二改", "三", "五", "六"].map(String::from).into();
        let text = "--- a\n+++ b\n\
                    @@ -2,1 +2,1 @@\n-二\n+二改\n\
                    @@ -4,1 +4,0 @@\n-四\n\
                    @@ -5,0 +5,1 @@\n+六\n";
        let ops = parse_unified_diff(text, &base, &edited).unwrap();
        assert_eq!(
            kinds(&ops),
            ["equal", "modify", "equal", "del", "equal", "add"]
        );
        assert_eq!(ops[1].base_text.as_deref(), Some("二"));
        assert_eq!(ops[1].edited_text.as_deref(), Some("二改"));
        assert_eq!(ops[5].edited_index, Some(4));
    }

    #[test]
    fn parses_insert_at_file_start() {
        let base: Vec<String> = ["甲"].map(String::from).into();
        let edited: Vec<String> = ["新", "甲"].map(String::from).into();
        let text = "--- a\n+++ b\n@@ -0,0 +1,1 @@\n+新\n";
        let ops = parse_unified_diff(text, &base, &edited).unwrap();
        assert_eq!(kinds(&ops), ["add", "equal"]);
        assert_eq!(ops[1].base_index, Some(0));
        assert_eq!(ops[1].edited_index, Some(1));
    }

    #[test]
    fn parses_delete_to_empty() {
        let base: Vec<String> = ["甲", "乙"].map(String::from).into();
        let edited: Vec<String> = vec![];
        let text = "--- a\n+++ b\n@@ -1,2 +0,0 @@\n-甲\n-乙\n";
        let ops = parse_unified_diff(text, &base, &edited).unwrap();
        assert_eq!(kinds(&ops), ["del", "del"]);
    }

    #[test]
    fn rejects_misaligned_hunks() {
        let base: Vec<String> = ["甲"].map(String::from).into();
        let edited: Vec<String> = ["甲", "乙"].map(String::from).into();
        // hunk 头行号超前（基准只有 1 行）
        let text = "--- a\n+++ b\n@@ -3,1 +3,1 @@\n-丙\n+丁\n";
        assert!(parse_unified_diff(text, &base, &edited).is_err());
    }

    /// 同一替换块的多行删除 + 多行新增：按相似度逐行配对，不能只合并最后一对。
    #[test]
    fn multi_line_replacement_pairs_every_line() {
        let base: Vec<String> = [
            "也是在这时，脑海中的反派系统再度出声了。",
            "【所以宿主不要犹豫啊，横竖都是一死，不如我们先看看眼前这个女主的滋味如何？】",
            "“按照剧情，男主的小弟很快就要过来了吧，你还有心思让我去理女主？”",
            "【我们是反派啊，就该做反派的事情才能获得情绪值！】",
            "池小橙没听这废物反派系统的瞎比比，保住小命才是最重要的。",
        ]
        .map(String::from)
        .into();
        let edited: Vec<String> = [
            "也是在这时，脑海中的反派系统再度出声了。",
            "【所以宿主不要犹豫啊，横竖都是一死，不如我们先尝尝眼前这个女主的滋味如何？】",
            "“按照剧情，男主的小弟很快就要过来了吧，你还有心思让我去调教女主？”",
            "【我们是反派啊，就该做反派的事情才能获得技能点！】",
            "池小橙没听这废物反派系统的瞎比比，保住小命才是最重要的。",
        ]
        .map(String::from)
        .into();
        let text = "--- a\n+++ b\n@@ -2,3 +2,3 @@\n\
                    -【所以宿主不要犹豫啊，横竖都是一死，不如我们先看看眼前这个女主的滋味如何？】\n\
                    -“按照剧情，男主的小弟很快就要过来了吧，你还有心思让我去理女主？”\n\
                    -【我们是反派啊，就该做反派的事情才能获得情绪值！】\n\
                    +【所以宿主不要犹豫啊，横竖都是一死，不如我们先尝尝眼前这个女主的滋味如何？】\n\
                    +“按照剧情，男主的小弟很快就要过来了吧，你还有心思让我去调教女主？”\n\
                    +【我们是反派啊，就该做反派的事情才能获得技能点！】\n";
        let ops = parse_unified_diff(text, &base, &edited).unwrap();
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
        let base: Vec<String> = ["今天天气很好，我们一起去公园散步吧。"]
            .map(String::from)
            .into();
        let edited: Vec<String> = ["系统提示：任务完成，奖励已发放。"]
            .map(String::from)
            .into();
        let text = "--- a\n+++ b\n@@ -1,1 +1,1 @@\n\
                    -今天天气很好，我们一起去公园散步吧。\n\
                    +系统提示：任务完成，奖励已发放。\n";
        let ops = parse_unified_diff(text, &base, &edited).unwrap();
        assert_eq!(kinds(&ops), ["del", "add"]);
    }

    #[test]
    fn uneven_blocks_pair_similar_lines() {
        // 基准两行，编辑版合并成一行：相似的那行配成 modify，另一行保留 del
        let base: Vec<String> = [
            "第一段文字内容比较长，保持不变。",
            "第二段文字内容比较长，也会保留。",
        ]
        .map(String::from)
        .into();
        let edited: Vec<String> = ["第一段文字内容比较长，保持不变。"]
            .map(String::from)
            .into();
        let text = "--- a\n+++ b\n@@ -1,2 +1,1 @@\n\
                    -第一段文字内容比较长，保持不变。\n\
                    -第二段文字内容比较长，也会保留。\n\
                    +第一段文字内容比较长，保持不变。\n";
        let ops = parse_unified_diff(text, &base, &edited).unwrap();
        assert_eq!(kinds(&ops), ["modify", "del"]);
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
    fn empty_array_writes_an_empty_file_not_a_blank_line() {
        assert_eq!(git_text(&[]), "");
        assert_eq!(git_text(&["甲".to_string()]), "甲\n");
        assert_eq!(git_text(&["甲".to_string(), "乙".to_string()]), "甲\n乙\n");
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

    #[tokio::test]
    async fn git_diff_roundtrip_when_git_exists() {
        let service = GitDiffService::new(String::new());
        if !service.git_available().await {
            return; // 无 git 的环境跳过
        }
        let base: Vec<String> = ["一", "二", "三"].map(String::from).into();
        let edited: Vec<String> = ["一", "二改", "三", "四"].map(String::from).into();
        let ops = service
            .diff_paragraphs(&base, &edited)
            .await
            .expect("有 git 就该出结果");
        assert_eq!(kinds(&ops), ["equal", "modify", "equal", "add"]);
        // 响应形态：equal 行不带文本
        assert!(ops[0].base_text.is_none());
    }

    #[tokio::test]
    async fn diff_deleting_everything_roundtrips() {
        let service = GitDiffService::new(String::new());
        if !service.git_available().await {
            return;
        }
        let base: Vec<String> = ["甲", "乙"].map(String::from).into();
        let ops = service
            .diff_paragraphs(&base, &[])
            .await
            .expect("空编辑版也要能算");
        assert_eq!(kinds(&ops), ["del", "del"]);
    }

    #[tokio::test]
    async fn missing_git_reports_a_reason_instead_of_panicking() {
        let service = GitDiffService::new("definitely-not-a-git-binary".to_string());
        let err = service
            .diff_paragraphs(&["甲".into()], &["乙".into()])
            .await
            .unwrap_err();
        assert_eq!(err, DiffUnavailable::GitMissing);
        // 相同文本不需要 git：仍然能拿到全 equal
        let ops = service
            .diff_paragraphs(&["甲".into()], &["甲".into()])
            .await
            .unwrap();
        assert_eq!(kinds(&ops), ["equal"]);
    }

    /// 取消/panic 路径靠 Drop 兜底删临时文件；正常路径显式删完就 disarm。
    #[test]
    fn temp_files_guard_removes_unfinished_files_on_drop() {
        let dir = std::env::temp_dir().join(format!(
            "reader-git-diff-guard-{}",
            crate::util::crypto::random_string(6)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.txt");
        let b = dir.join("b.txt");
        std::fs::write(&a, "x").unwrap();
        std::fs::write(&b, "y").unwrap();

        {
            let mut guard = TempFiles::new(vec![a.clone(), b.clone()]);
            guard.disarm();
        }
        assert!(a.exists() && b.exists(), "disarm 后 Drop 不该再删");

        {
            let _guard = TempFiles::new(vec![a.clone(), b.clone()]);
        }
        assert!(!a.exists() && !b.exists(), "Drop 兜底应删除临时文件");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 启动清扫只动自己的前缀、且只动超过门槛的旧文件。
    #[test]
    fn stale_temp_sweep_only_touches_old_own_files() {
        let dir = std::env::temp_dir().join(format!(
            "reader-git-diff-sweep-{}",
            crate::util::crypto::random_string(6)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let fresh = dir.join("reader-git-diff-fresh-a.txt");
        let other = dir.join("someone-else.txt");
        std::fs::write(&fresh, "x").unwrap();
        std::fs::write(&other, "y").unwrap();

        // 刚写入的文件不算「往期遗留」，清扫不碰
        sweep_stale_temp_files_in(&dir);
        assert!(fresh.exists(), "新文件不该被清扫");
        assert!(other.exists(), "非本前缀的文件不该被清扫");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
