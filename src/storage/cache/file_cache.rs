//! 目录型缓存的容量控制与占用统计。
//!
//! 章节正文已经进数据库，这里只剩评论缓存、图片缓存等仍以文件保存的缓存
//! 所需的公共工具：目录占用统计、按最旧优先淘汰、平铺目录容量控制。

use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;
use tokio::fs;

/// 平铺目录的进程内用量计量（按目录路径记账），供 `enforce_flat_dir_capacity` 使用。
static FLAT_DIR_USAGE: Lazy<Mutex<HashMap<PathBuf, u64>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// 一个目录的占用统计。
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct CacheUsage {
    pub files: u64,
    pub bytes: u64,
}

/// 两层缓存（评论/图片等目录）共用的容量控制：平时只增量记账（`written`
/// 累加），计量越过上限才做一次全目录扫描校准，确实超限时按修改时间最旧
/// 优先淘汰。首次触达（进程冷启动或整用户清理后）必须校准一次：磁盘可能
/// 已有存量，从零记账会漏淘汰；之后增量越过上限才再扫描。
pub(crate) async fn enforce_scoped_capacity(
    usage: &Mutex<HashMap<String, u64>>,
    scope: &str,
    dir: &Path,
    depth: usize,
    max_bytes: u64,
    written: u64,
) {
    if max_bytes == 0 {
        return;
    }
    let (tracked, fresh) = {
        let mut map = usage.lock().unwrap();
        match map.get_mut(scope) {
            Some(entry) => {
                *entry = entry.saturating_add(written);
                (*entry, false)
            }
            None => {
                map.insert(scope.to_string(), written);
                (written, true)
            }
        }
    };
    if !fresh && tracked <= max_bytes {
        return;
    }
    let usage_now = dir_usage(dir, depth).await;
    if usage_now.bytes <= max_bytes {
        usage
            .lock()
            .unwrap()
            .insert(scope.to_string(), usage_now.bytes);
        return;
    }
    let freed = evict_oldest(dir, usage_now.bytes - max_bytes).await;
    usage
        .lock()
        .unwrap()
        .insert(scope.to_string(), usage_now.bytes - freed);
}

/// 统计目录占用。`depth` 为向下递归的层数（`1` 只看直接子文件）。
pub async fn dir_usage(dir: &Path, depth: usize) -> CacheUsage {
    let mut usage = CacheUsage::default();
    let mut stack = vec![(dir.to_path_buf(), depth)];
    while let Some((current, remaining)) = stack.pop() {
        let Ok(mut entries) = fs::read_dir(&current).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let Ok(file_type) = entry.file_type().await else {
                continue;
            };
            if file_type.is_dir() {
                if remaining > 0 {
                    stack.push((entry.path(), remaining - 1));
                }
                continue;
            }
            if let Ok(meta) = entry.metadata().await {
                usage.files += 1;
                usage.bytes += meta.len();
            }
        }
    }
    usage
}

/// 按修改时间最旧优先删除，直到释放出 `excess` 字节；返回实际释放的字节数。
pub(crate) async fn evict_oldest(dir: &Path, excess: u64) -> u64 {
    let mut files = collect_files(dir, 2).await;
    files.sort_by_key(|(modified, _, _)| *modified);
    let mut remaining = excess;
    let mut freed = 0u64;
    for (_, size, path) in files {
        if remaining == 0 {
            break;
        }
        if fs::remove_file(&path).await.is_ok() {
            remaining = remaining.saturating_sub(size);
            freed += size;
        }
    }
    freed
}

async fn collect_files(dir: &Path, depth: usize) -> Vec<(SystemTime, u64, PathBuf)> {
    let mut files = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), depth)];
    while let Some((current, remaining)) = stack.pop() {
        let Ok(mut entries) = fs::read_dir(&current).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let Ok(file_type) = entry.file_type().await else {
                continue;
            };
            if file_type.is_dir() {
                if remaining > 0 {
                    stack.push((entry.path(), remaining - 1));
                }
                continue;
            }
            if let Ok(meta) = entry.metadata().await {
                files.push((
                    meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    meta.len(),
                    entry.path(),
                ));
            }
        }
    }
    files
}

/// 递归删除目录并统计删除的文件数；目录不存在时返回 0。
pub async fn remove_dir_counting_files(dir: &Path) -> anyhow::Result<u64> {
    if !dir.exists() {
        return Ok(0);
    }
    let files = collect_files(dir, usize::MAX).await.len() as u64;
    fs::remove_dir_all(dir).await?;
    Ok(files)
}

/// 删除单个文件；存在时返回 1，否则 0。
pub async fn remove_file_counting(path: &Path) -> anyhow::Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    fs::remove_file(path).await?;
    Ok(1)
}

/// 对一个平铺目录做容量控制：总大小超上限时按修改时间最旧优先删除。
///
/// 用于封面缓存这类「不入两层结构、但同样不能无界增长」的目录。
/// `max_bytes` 为 `0` 表示不限制；`written` 为本次写入的字节数，
/// 用于增量记账（越过上限才全量扫描校准，避免每次写都遍历目录）。
pub async fn enforce_flat_dir_capacity(dir: &Path, max_bytes: u64, written: u64) {
    if max_bytes == 0 {
        return;
    }
    let (tracked, fresh) = {
        let mut map = FLAT_DIR_USAGE.lock().unwrap();
        match map.get_mut(dir) {
            Some(entry) => {
                *entry = entry.saturating_add(written);
                (*entry, false)
            }
            None => {
                map.insert(dir.to_path_buf(), written);
                (written, true)
            }
        }
    };
    // 首次触达先校准一次：目录里可能有重启前/外部写入的存量文件。
    if !fresh && tracked <= max_bytes {
        return;
    }
    let usage = dir_usage(dir, 1).await;
    if usage.bytes <= max_bytes {
        FLAT_DIR_USAGE
            .lock()
            .unwrap()
            .insert(dir.to_path_buf(), usage.bytes);
        return;
    }
    let freed = evict_oldest(dir, usage.bytes - max_bytes).await;
    FLAT_DIR_USAGE
        .lock()
        .unwrap()
        .insert(dir.to_path_buf(), usage.bytes - freed);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()))
    }

    #[tokio::test]
    async fn flat_dir_capacity_evicts_oldest_files() {
        let dir = temp_root("reader-flat-cap");
        fs::create_dir_all(&dir).await.unwrap();
        // 写三个文件，mtime 依次递增；总量超过上限后最旧的应被淘汰
        for (name, size) in [("a.bin", 600usize), ("b.bin", 600), ("c.bin", 600)] {
            fs::write(dir.join(name), vec![0u8; size]).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        enforce_flat_dir_capacity(&dir, 1000, 600).await;
        assert!(!dir.join("a.bin").exists(), "最旧的 a.bin 应被淘汰");
        assert!(dir.join("c.bin").exists(), "最新的 c.bin 应保留");
        let _ = fs::remove_dir_all(&dir).await;
    }
}
