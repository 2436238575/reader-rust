//! 原子落盘工具：先写同目录临时文件，再 rename 覆盖目标。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::fs;

/// 临时文件编号：同目录内唯一即可，避免并发写者互踩对方的 tmp 文件。
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 目标文件的临时落盘路径：与目标同目录（同卷，rename 才是原子的），
/// 文件名带进程号 + 进程内唯一编号，多进程共目录与进程内并发写同一
/// 目标时都不会互踩对方的 tmp 文件。
fn tmp_path_for(path: &Path) -> PathBuf {
    let seq = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(format!(".{pid}.{seq}.tmp"));
    path.with_file_name(name)
}

/// 原子写入文件：`<目标>.<pid>.<n>.tmp` 写完整内容后 rename 覆盖目标。
///
/// 读者要么看到旧内容、要么看到完整新内容，绝不会读到半截写入
/// （进程中途被杀只会留下一个 `.tmp` 孤儿文件，不影响目标）。
/// Windows 上 rename 对应 `MoveFileExW(MOVEFILE_REPLACE_EXISTING)`，
/// 可覆盖已存在的目标；rename 失败时清理本次的 tmp 文件。
pub async fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    let tmp = tmp_path_for(path);
    fs::write(&tmp, contents).await?;
    match fs::rename(&tmp, path).await {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&tmp).await;
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn write_atomic_replaces_existing_and_leaves_no_tmp() {
        let dir = std::env::temp_dir().join(format!(
            "reader-rust-atomic-write-{}-{}",
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("data.json");

        write_atomic(&target, b"first").await.unwrap();
        write_atomic(&target, b"second").await.unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"second");

        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "不应残留 tmp 文件");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn write_atomic_concurrent_writers_all_land_intact() {
        let dir = std::env::temp_dir().join(format!(
            "reader-rust-atomic-write-conc-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("shared.json");
        // 8 个并发写者：最终内容必是其中一方的完整写入，且读到的永远是合法 JSON
        let mut handles = Vec::new();
        for i in 0..8 {
            let target = target.clone();
            handles.push(tokio::spawn(async move {
                let body = format!("{{\"writer\":{i}}}");
                write_atomic(&target, body.as_bytes()).await.unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        let raw = std::fs::read_to_string(&target).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("必须是完整 JSON");
        assert!(parsed.get("writer").is_some());
        std::fs::remove_dir_all(&dir).ok();
    }
}
