use crate::util::hash::md5_hex;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tokio::fs;

/// 章节缓存的最长保留时间；超期条目读取时视为未命中并就地清理（L4）。
const MAX_CACHE_AGE: Duration = Duration::from_secs(7 * 24 * 3600);
/// 单个用户缓存目录的容量上限（字节）；写入时超限按最旧优先淘汰（L4）。
const MAX_USER_CACHE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone)]
pub struct FileCache {
    root: PathBuf,
    max_age: Duration,
}

impl FileCache {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self::with_max_age(root, MAX_CACHE_AGE)
    }

    /// 允许调用方指定缓存保留时长（测试可用 `Duration::ZERO` 模拟“立刻过期”）。
    pub fn with_max_age(root: impl AsRef<Path>, max_age: Duration) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
            max_age,
        }
    }

    /// Get cached content for a specific book
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
        // TTL：过期即删除并按未命中处理，避免陈旧章节内容长期驻留
        if self.is_expired(&path).await {
            let _ = fs::remove_file(&path).await;
            return Ok(None);
        }
        let data = fs::read_to_string(&path).await?;
        Ok(Some(data))
    }

    /// Put cached content for a specific book
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
        // 容量控制：防止缓存无限增长打满磁盘，超限按最旧优先淘汰
        self.enforce_user_capacity(user_ns).await;
        Ok(())
    }

    /// Remove a single chapter cache
    pub async fn remove(
        &self,
        user_ns: &str,
        book_key: &str,
        chapter_key: &str,
    ) -> anyhow::Result<()> {
        let path = self.chapter_path(user_ns, book_key, chapter_key);
        if path.exists() {
            fs::remove_file(path).await?;
        }
        Ok(())
    }

    /// Check if a chapter cache exists
    pub async fn exists(&self, user_ns: &str, book_key: &str, chapter_key: &str) -> bool {
        let path = self.chapter_path(user_ns, book_key, chapter_key);
        // 过期条目等同于不存在
        path.exists() && !self.is_expired(&path).await
    }

    /// Remove all cache for a book (delete the book's cache directory)
    pub async fn remove_book(&self, user_ns: &str, book_key: &str) -> anyhow::Result<bool> {
        let path = self.book_path(user_ns, book_key);
        if path.exists() {
            fs::remove_dir_all(&path).await?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn is_expired(&self, path: &Path) -> bool {
        let Ok(meta) = fs::metadata(path).await else {
            return false;
        };
        let Ok(modified) = meta.modified() else {
            return false;
        };
        modified
            .elapsed()
            .map(|e| e > self.max_age)
            .unwrap_or(false)
    }

    /// 统计单个用户缓存目录，若总大小超过上限则按修改时间最旧优先淘汰。
    ///
    /// 目录结构为 `root/user_ns/book_key/chapter_hash.txt`，这里下钻两层统计文件。
    async fn enforce_user_capacity(&self, user_ns: &str) {
        let dir = self.root.join(user_ns);
        let Ok(mut books) = fs::read_dir(&dir).await else {
            return;
        };
        let mut files: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
        let mut total: u64 = 0;
        while let Ok(Some(book_entry)) = books.next_entry().await {
            let book_path = book_entry.path();
            if !book_path.is_dir() {
                continue;
            }
            let Ok(mut chapters) = fs::read_dir(&book_path).await else {
                continue;
            };
            while let Ok(Some(chapter_entry)) = chapters.next_entry().await {
                let Ok(meta) = chapter_entry.metadata().await else {
                    continue;
                };
                if !meta.is_file() {
                    continue;
                }
                total += meta.len();
                let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                files.push((modified, meta.len(), chapter_entry.path()));
            }
        }
        if total <= MAX_USER_CACHE_BYTES {
            return;
        }
        files.sort_by_key(|(modified, _, _)| *modified);
        let mut excess = total - MAX_USER_CACHE_BYTES;
        for (_, size, path) in files {
            if excess == 0 {
                break;
            }
            if fs::remove_file(&path).await.is_ok() {
                excess = excess.saturating_sub(size);
            }
        }
    }

    /// Get the directory path for a book's cache
    fn book_path(&self, user_ns: &str, book_key: &str) -> PathBuf {
        self.root.join(user_ns).join(book_key)
    }

    /// Get the file path for a specific chapter
    fn chapter_path(&self, user_ns: &str, book_key: &str, chapter_key: &str) -> PathBuf {
        let name = md5_hex(chapter_key);
        self.book_path(user_ns, book_key)
            .join(name)
            .with_extension("txt")
    }
}

/// 对一个平铺目录做容量控制：总大小超上限时按修改时间最旧优先删除。
///
/// 用于封面缓存这类「不入 FileCache 的两级结构、但同样不能无界增长」的目录
/// （`/cover` 允许匿名访问，换 URL 即可写盘，没有上限就是匿名磁盘耗尽）。
pub async fn enforce_flat_dir_capacity(dir: &Path, max_bytes: u64) {
    let Ok(mut entries) = fs::read_dir(dir).await else {
        return;
    };
    let mut files: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
    let mut total: u64 = 0;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let Ok(meta) = entry.metadata().await else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        total += meta.len();
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        files.push((modified, meta.len(), entry.path()));
    }
    if total <= max_bytes {
        return;
    }
    files.sort_by_key(|(modified, _, _)| *modified);
    let mut excess = total - max_bytes;
    for (_, size, path) in files {
        if excess == 0 {
            break;
        }
        if fs::remove_file(&path).await.is_ok() {
            excess = excess.saturating_sub(size);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn expired_entries_are_treated_as_missing() {
        let root = std::env::temp_dir().join(format!(
            "reader-cache-test-{}",
            uuid::Uuid::new_v4()
        ));
        // 保留时长置零，模拟“写入即过期”
        let cache = FileCache::with_max_age(&root, Duration::ZERO);
        cache
            .put("ns", "book", "chapter", "content")
            .await
            .unwrap();
        assert!(
            !cache.exists("ns", "book", "chapter").await,
            "过期条目应视为不存在"
        );
        assert_eq!(
            cache.get("ns", "book", "chapter").await.unwrap(),
            None,
            "过期条目应返回未命中"
        );
        let _ = fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn capacity_evicts_oldest_entries() {
        // 临时把上限压到很小（通过根目录里塞入超过阈值的数据不太可控），
        // 这里直接验证：写入一个超大 value（大于上限本身）后，该条目会被
        // 立刻按“最旧优先”淘汰掉，从而不会无界堆积。
        let root = std::env::temp_dir().join(format!(
            "reader-cache-cap-{}",
            uuid::Uuid::new_v4()
        ));
        let cache = FileCache::new(&root);
        let huge = "x".repeat((MAX_USER_CACHE_BYTES + 1024) as usize);
        cache.put("ns", "book", "chapter", &huge).await.unwrap();
        // 超限后最旧的（也就是刚写的这一条）被淘汰
        assert!(
            !cache.exists("ns", "book", "chapter").await,
            "超过容量上限的条目应被淘汰"
        );
        let _ = fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn flat_dir_capacity_evicts_oldest_files() {
        let dir = std::env::temp_dir().join(format!(
            "reader-flat-cap-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).await.unwrap();
        // 写三个文件，mtime 依次递增；总量超过上限后最旧的应被淘汰
        for (name, size) in [("a.bin", 600usize), ("b.bin", 600), ("c.bin", 600)] {
            fs::write(dir.join(name), vec![0u8; size]).await.unwrap();
            // 保证 mtime 可分辨
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        enforce_flat_dir_capacity(&dir, 1000).await;
        assert!(!dir.join("a.bin").exists(), "最旧的 a.bin 应被淘汰");
        assert!(dir.join("c.bin").exists(), "最新的 c.bin 应保留");
        let _ = fs::remove_dir_all(&dir).await;
    }
}
