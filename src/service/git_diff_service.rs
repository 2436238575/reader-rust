//! 章节段落的 diff：优先调系统 git（`git diff --no-index --unified=0`），
//! 系统没有 git 或执行失败时返回 None，由前端回退到本地 LCS 计算。
//!
//! 输入是前端切好的段落数组（段 = 非空行），写成两个临时文件后跑 git；
//! 解析 unified 输出重建与前端 `ParagraphDiffOp` 同构的操作序列。

use crate::error::error::AppError;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
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
}

#[derive(Clone)]
pub struct GitDiffService {
    /// git 可用性探测缓存：(结果, 探测时刻)。false 结果 60s 后重试——
    /// 用户中途装上 git 不用重启进程。
    available: Arc<std::sync::Mutex<Option<(bool, std::time::Instant)>>>,
    /// 并发闸门：同一时刻最多 MAX_CONCURRENT_GIT 个 git 进程
    semaphore: Arc<tokio::sync::Semaphore>,
}

impl Default for GitDiffService {
    fn default() -> Self {
        Self::new()
    }
}

impl GitDiffService {
    pub fn new() -> Self {
        Self {
            available: Arc::new(std::sync::Mutex::new(None)),
            semaphore: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_GIT)),
        }
    }

    /// 系统里有没有 git。false 结果缓存 60s，之后重新探测。
    async fn git_available(&self) -> bool {
        {
            let cached = self.available.lock().unwrap();
            if let Some((available, at)) = *cached {
                if available || at.elapsed() < std::time::Duration::from_secs(60) {
                    return available;
                }
            }
        }
        let available = Command::new("git")
            .arg("--version")
            .output()
            .await
            .map(|output| output.status.success())
            .unwrap_or(false);
        if !available {
            tracing::info!("系统未找到可用的 git，章节 diff 回退到前端本地计算");
        }
        *self.available.lock().unwrap() = Some((available, std::time::Instant::now()));
        available
    }

    /// 对两份段落数组跑 git diff。返回 None 表示 git 不可用或执行失败（前端回退）。
    pub async fn diff_paragraphs(
        &self,
        base: &[String],
        edited: &[String],
    ) -> Result<Option<Vec<DiffOp>>, AppError> {
        let total_bytes: usize = base.iter().map(|p| p.len()).sum::<usize>()
            + edited.iter().map(|p| p.len()).sum::<usize>();
        if base.len() > MAX_DIFF_PARAGRAPHS
            || edited.len() > MAX_DIFF_PARAGRAPHS
            || total_bytes > MAX_TOTAL_BYTES
            || base.iter().any(|p| p.len() > MAX_PARAGRAPH_BYTES)
            || edited.iter().any(|p| p.len() > MAX_PARAGRAPH_BYTES)
        {
            return Err(AppError::BadRequest("diff 输入超出上限".to_string()));
        }
        if !self.git_available().await {
            return Ok(None);
        }
        // 完全一致：不进 git，直接全 equal
        if base == edited {
            return Ok(Some(
                base.iter()
                    .enumerate()
                    .map(|(i, text)| DiffOp::equal(i, i, text))
                    .collect(),
            ));
        }

        // 并发闸门先于写临时文件：否则排队中的请求各自先写 32MB 落盘，
        // 闸门外的等待者照样把磁盘/IO 打满（闸门形同虚设）
        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("diff 并发闸门已关闭: {e}")))?;

        let temp_dir = std::env::temp_dir();
        let tag = format!(
            "reader-git-diff-{}-{}",
            std::process::id(),
            crate::util::crypto::random_string(6)
        );
        let base_path: PathBuf = temp_dir.join(format!("{tag}-a.txt"));
        let edited_path: PathBuf = temp_dir.join(format!("{tag}-b.txt"));
        let write_result = async {
            write_private_file(&base_path, format!("{}\n", base.join("\n")).as_bytes()).await?;
            write_private_file(&edited_path, format!("{}\n", edited.join("\n")).as_bytes()).await?;
            Ok::<(), std::io::Error>(())
        }
        .await;
        if let Err(e) = write_result {
            // 写失败（含第二个文件没写成）：两个路径都清一遍，不留半成品
            let _ = tokio::fs::remove_file(&base_path).await;
            let _ = tokio::fs::remove_file(&edited_path).await;
            return Err(AppError::Internal(e.into()));
        }

        // kill_on_drop：超时分支会把子进程杀掉（否则超时只warn不杀，
        // Windows 下文件还被占着删不掉，泄漏 tmp 文件）
        let run = tokio::time::timeout(
            GIT_DIFF_TIMEOUT,
            Command::new("git")
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

        let output = match run {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => return Err(AppError::Internal(e.into())),
            Err(_) => {
                tracing::debug!("git diff 执行超时（{GIT_DIFF_TIMEOUT:?}），回退前端本地 diff");
                return Ok(None);
            }
        };
        // git diff --no-index：退出码 0=无差异，1=有差异，其余=错误
        let status = output.status.code().unwrap_or(-1);
        if status != 0 && status != 1 {
            tracing::debug!(
                "git diff 执行失败（退出码 {status}）：{}",
                String::from_utf8_lossy(&output.stderr)
                    .chars()
                    .take(200)
                    .collect::<String>()
            );
            return Ok(None);
        }
        let text = String::from_utf8_lossy(&output.stdout);
        match parse_unified_diff(&text, base, edited) {
            Ok(ops) => Ok(Some(ops)),
            Err(reason) => {
                tracing::debug!("git diff 输出解析失败（{reason}），回退前端本地 diff");
                Ok(None)
            }
        }
    }
}

/// 写临时文件：Unix 下收紧到 0600（多用户主机上别让人读到正文）。
async fn write_private_file(path: &PathBuf, bytes: &[u8]) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
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

/// 与前端 diffParagraphs 同规则：del 紧跟 add 的一对一相邻合并为 modify。
fn merge_modify(ops: Vec<DiffOp>) -> Vec<DiffOp> {
    let mut merged: Vec<DiffOp> = Vec::with_capacity(ops.len());
    for op in ops {
        let last = merged.last_mut();
        if let Some(last) = last {
            if last.kind == "del" && op.kind == "add" {
                *last = DiffOp {
                    kind: "modify".into(),
                    base_index: last.base_index,
                    edited_index: op.edited_index,
                    base_text: last.base_text.take(),
                    edited_text: op.edited_text,
                };
                continue;
            }
        }
        merged.push(op);
    }
    merged
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

    #[tokio::test]
    async fn git_diff_roundtrip_when_git_exists() {
        let service = GitDiffService::new();
        if !service.git_available().await {
            return; // 无 git 的环境跳过（回退路径由 None 分支覆盖）
        }
        let base: Vec<String> = ["一", "二", "三"].map(String::from).into();
        let edited: Vec<String> = ["一", "二改", "三", "四"].map(String::from).into();
        let ops = service
            .diff_paragraphs(&base, &edited)
            .await
            .unwrap()
            .expect("有 git 就该出结果");
        assert_eq!(kinds(&ops), ["equal", "modify", "equal", "add"]);
    }
}
