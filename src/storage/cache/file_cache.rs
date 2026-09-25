use crate::util::hash::md5_hex;
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use tokio::fs;

/// 平铺目录的进程内用量计量（按目录路径记账），供 `enforce_flat_dir_capacity` 使用。
static FLAT_DIR_USAGE: Lazy<Mutex<HashMap<PathBuf, u64>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// 章节正文缓存：`<root>/<user_ns>/<book_key>/<md5(chapter_key)>.txt`。
///
/// 缓存**不按时间过期**：内容一旦抓取就持续有效，直到被显式清理
/// （`purgeCache`）或超出容量被淘汰。容量上限因此是唯一的自动回收手段，
/// 由 `CACHE_USER_LIMIT_BYTES` 配置，`0` 表示不限制。
#[derive(Clone)]
pub struct FileCache {
    root: PathBuf,
    /// 单用户容量上限（字节）；`0` 表示不限制。
    max_user_bytes: u64,
    /// 每用户用量计量（字节）：put/remove 增量维护，越过上限才做一次全目录
    /// 扫描校准。批量缓存整本书时若每次 put 都全扫描，整体是 O(N²) 次 stat。
    /// 进程内状态：外部改动目录会让计量偏差，靠校准自愈；多实例共享目录时
    /// 各自计量偏小，淘汰时机略滞后（有上限兜底，只是可能短暂超出）。
    usage: Arc<Mutex<HashMap<String, u64>>>,
}

impl FileCache {
    /// `max_user_bytes` 为单用户容量上限，`0` 表示不限制。
    pub fn new(root: impl AsRef<Path>, max_user_bytes: u64) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
            max_user_bytes,
            usage: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 读取章节正文；不存在即未命中。
    pub async fn get(
        &self,
        user_ns: &str,
        book_key: &str,
        chapter_key: &str,
    ) -> anyhow::Result<Option<String>> {
        let path = self.chapter_path(user_ns, book_key, chapter_key);
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(fs::read_to_string(&path).await?))
    }

    /// 写入章节正文，并按容量上限淘汰最旧条目。
    pub async fn put(
        &self,
        user_ns: &str,
        book_key: &str,
        chapter_key: &str,
        value: &str,
    ) -> anyhow::Result<()> {
        let path = self.chapter_path(user_ns, book_key, chapter_key);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        fs::write(&path, value).await?;
        self.enforce_user_capacity(user_ns, value.len() as u64)
            .await;
        Ok(())
    }

    /// 删除单章缓存。
    pub async fn remove(
        &self,
        user_ns: &str,
        book_key: &str,
        chapter_key: &str,
    ) -> anyhow::Result<()> {
        let path = self.chapter_path(user_ns, book_key, chapter_key);
        if path.exists() {
            let len = fs::metadata(&path).await.map(|m| m.len()).unwrap_or(0);
            fs::remove_file(path).await?;
            self.subtract_usage(user_ns, len);
        }
        Ok(())
    }

    pub async fn exists(&self, user_ns: &str, book_key: &str, chapter_key: &str) -> bool {
        self.chapter_path(user_ns, book_key, chapter_key).exists()
    }

    /// 删除整本书的缓存目录。
    pub async fn remove_book(&self, user_ns: &str, book_key: &str) -> anyhow::Result<bool> {
        let path = self.book_path(user_ns, book_key);
        if path.exists() {
            let book_usage = dir_usage(&path, 1).await;
            fs::remove_dir_all(&path).await?;
            self.subtract_usage(user_ns, book_usage.bytes);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// 删除某个用户的全部章节缓存，返回删除的文件数。
    pub async fn remove_user(&self, user_ns: &str) -> anyhow::Result<u64> {
        let removed = remove_dir_counting_files(&self.root.join(user_ns)).await?;
        self.clear_usage(user_ns);
        Ok(removed)
    }

    /// 某个用户已占用的缓存量。
    pub async fn user_usage(&self, user_ns: &str) -> CacheUsage {
        dir_usage(&self.root.join(user_ns), 2).await
    }

    /// 全部用户已占用的缓存量。
    pub async fn total_usage(&self) -> CacheUsage {
        dir_usage(&self.root, 2).await
    }

    /// 列出所有已产生缓存的用户命名空间。
    pub async fn users(&self) -> Vec<String> {
        let mut names = Vec::new();
        let Ok(mut entries) = fs::read_dir(&self.root).await else {
            return names;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            if entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
                if let Some(name) = entry.file_name().to_str() {
                    names.push(name.to_string());
                }
            }
        }
        names.sort();
        names
    }

    /// 统计单个用户缓存目录，超限时按修改时间最旧优先淘汰。
    ///
    /// 目录结构为 `root/user_ns/book_key/chapter_hash.txt`，下钻两层统计文件。
    /// 平时只做增量记账（`written` 累加），计量越过上限才全量扫描校准一次；
    /// 校准发现实际未超限（外部清理过目录）就继续用校准值。
    async fn enforce_user_capacity(&self, user_ns: &str, written: u64) {
        if self.max_user_bytes == 0 {
            return;
        }
        let (tracked, fresh) = {
            let mut map = self.usage.lock().unwrap();
            match map.get_mut(user_ns) {
                Some(entry) => {
                    *entry = entry.saturating_add(written);
                    (*entry, false)
                }
                None => {
                    map.insert(user_ns.to_string(), written);
                    (written, true)
                }
            }
        };
        // 首次触达（进程冷启动或整用户清理后）必须校准一次：磁盘可能已有存量，
        // 从零记账会漏淘汰；之后增量越过上限才再扫描。
        if !fresh && tracked <= self.max_user_bytes {
            return;
        }
        let dir = self.root.join(user_ns);
        let usage = dir_usage(&dir, 2).await;
        if usage.bytes <= self.max_user_bytes {
            self.set_usage(user_ns, usage.bytes);
            return;
        }
        let freed = evict_oldest(&dir, usage.bytes - self.max_user_bytes).await;
        self.set_usage(user_ns, usage.bytes - freed);
    }

    fn subtract_usage(&self, user_ns: &str, delta: u64) {
        let mut map = self.usage.lock().unwrap();
        if let Some(entry) = map.get_mut(user_ns) {
            *entry = entry.saturating_sub(delta);
        }
    }

    fn set_usage(&self, user_ns: &str, value: u64) {
        self.usage
            .lock()
            .unwrap()
            .insert(user_ns.to_string(), value);
    }

    fn clear_usage(&self, user_ns: &str) {
        self.usage.lock().unwrap().remove(user_ns);
    }

    fn book_path(&self, user_ns: &str, book_key: &str) -> PathBuf {
        self.root.join(user_ns).join(book_key)
    }

    fn chapter_path(&self, user_ns: &str, book_key: &str, chapter_key: &str) -> PathBuf {
        let name = md5_hex(chapter_key);
        self.book_path(user_ns, book_key)
            .join(name)
            .with_extension("txt")
    }
}

/// 一个目录的占用统计。
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct CacheUsage {
    pub files: u64,
    pub bytes: u64,
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
/// 用于封面缓存这类「不入 FileCache 的两级结构、但同样不能无界增长」的目录
/// （`/cover` 换 URL 即可写盘，没有上限就是磁盘耗尽）。
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
    async fn entries_never_expire_on_their_own() {
        let root = temp_root("reader-cache-nottl");
        let cache = FileCache::new(&root, 0);
        cache.put("ns", "book", "chapter", "content").await.unwrap();
        // 容量上限（默认 512MiB）远未触及，条目必须一直可读
        assert!(cache.exists("ns", "book", "chapter").await);
        assert_eq!(
            cache.get("ns", "book", "chapter").await.unwrap().as_deref(),
            Some("content")
        );
        let _ = fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn capacity_evicts_oldest_entries() {
        let root = temp_root("reader-cache-cap");
        let cache = FileCache::new(&root, 1024);
        // 单条就超过上限，写入后应立即被淘汰，不会无界堆积
        let huge = "x".repeat(4096);
        cache.put("ns", "book", "chapter", &huge).await.unwrap();
        assert!(
            !cache.exists("ns", "book", "chapter").await,
            "超过容量上限的条目应被淘汰"
        );
        let _ = fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn zero_limit_disables_eviction() {
        let root = temp_root("reader-cache-unlimited");
        let cache = FileCache::new(&root, 0);
        let huge = "x".repeat(4096);
        cache.put("ns", "book", "chapter", &huge).await.unwrap();
        assert!(cache.exists("ns", "book", "chapter").await);
        let _ = fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn purge_removes_only_the_requested_scope() {
        let root = temp_root("reader-cache-purge");
        let cache = FileCache::new(&root, 0);
        cache.put("alice", "book", "c1", "a").await.unwrap();
        cache.put("alice", "book", "c2", "b").await.unwrap();
        cache.put("bob", "book", "c1", "c").await.unwrap();

        assert_eq!(cache.user_usage("alice").await.files, 2);
        assert_eq!(cache.users().await, vec!["alice", "bob"]);

        assert_eq!(cache.remove_user("alice").await.unwrap(), 2);
        assert_eq!(cache.user_usage("alice").await.files, 0);
        assert_eq!(cache.user_usage("bob").await.files, 1);

        assert_eq!(cache.remove_user("bob").await.unwrap(), 1);
        assert!(cache.users().await.is_empty());
        let _ = fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn capacity_accounting_keeps_eviction_correct() {
        let root = temp_root("reader-cache-acct");
        let cache = FileCache::new(&root, 1000);
        // 增量记账路径：4 次写入后越过上限触发校准+淘汰，第 5 次写入重复该流程，
        // 最终占用必须被压回上限内（证明记账没有漏淘汰）
        for i in 0..5 {
            cache
                .put("ns", "book", &format!("c{i}"), &"x".repeat(300))
                .await
                .unwrap();
        }
        let usage = cache.user_usage("ns").await;
        assert!(
            usage.bytes <= 1000,
            "实际占用应被压回上限内，当前 {} 字节",
            usage.bytes
        );
        let _ = fs::remove_dir_all(&root).await;
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
