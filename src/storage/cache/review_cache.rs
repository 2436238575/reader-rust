use crate::util::hash::md5_hex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tokio::fs;

use super::file_cache::{dir_usage, remove_dir_counting_files, CacheUsage};

/// 评论缓存：`<root>/<user_ns>/<book_key>/<md5(key)>.json`。
///
/// 评论是**会变的第三方数据**，新评论、点赞数、热度排序每天都在动，因此
/// 保留时间过期，默认 7 天（`REVIEW_CACHE_TTL_SECS`，`0` 表示不过期）。
/// 正文版本是历史，进正文池后不设 TTL、不自动淘汰；两者的回收策略不同。
///
/// 目录层级按 `ns/book_key/文件` 组织，按用户/按书的清理与容量淘汰复用
/// `file_cache` 里的目录工具。
#[derive(Clone)]
pub struct ReviewCache {
    root: PathBuf,
    /// 条目存活时间；`0` 表示不过期。
    ttl: Duration,
    /// 单用户容量上限（字节）；`0` 表示不限制。
    max_user_bytes: u64,
    /// 每用户用量计量（字节）：与目录型缓存相同的
    /// 增量记账策略——评论翻页/换排序都会新增缓存文件，每次 put 全目录
    /// 扫描在重度使用下是 O(N²) 次 stat。
    usage: Arc<Mutex<HashMap<String, u64>>>,
}

impl ReviewCache {
    pub fn new(root: impl AsRef<Path>, ttl_secs: u64, max_user_bytes: u64) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
            ttl: Duration::from_secs(ttl_secs),
            max_user_bytes,
            usage: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 读取缓存条目；不存在或已过期返回 `None`，过期条目顺带删除。
    pub async fn get(
        &self,
        user_ns: &str,
        book_key: &str,
        key: &str,
    ) -> anyhow::Result<Option<String>> {
        let path = self.entry_path(user_ns, book_key, key);
        let Ok(meta) = fs::metadata(&path).await else {
            return Ok(None);
        };
        if self.is_expired(&meta) {
            let _ = fs::remove_file(&path).await;
            self.subtract_usage(user_ns, meta.len());
            return Ok(None);
        }
        Ok(Some(fs::read_to_string(&path).await?))
    }

    /// 写入缓存条目，并按容量上限淘汰最旧文件。
    pub async fn put(
        &self,
        user_ns: &str,
        book_key: &str,
        key: &str,
        value: &str,
    ) -> anyhow::Result<()> {
        let path = self.entry_path(user_ns, book_key, key);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        // 唯一临时文件 + rename 的原子写：并发写同一个 key 时各写各的
        // tmp（此前是固定 `json.tmp`，两个写者会互踩同一个临时文件，
        // 可能把对方写了一半的内容 rename 过去）
        crate::util::atomic_write::write_atomic(&path, value.as_bytes()).await?;
        self.enforce_user_capacity(user_ns, value.len() as u64)
            .await;
        Ok(())
    }

    /// 删除某本书的全部评论缓存，返回删除的文件数。
    /// book_key 必须是 `book_<n>`：非法 id 直接 no-op（纵深防御，与其他缓存路径同手法）。
    pub async fn remove_book(&self, user_ns: &str, book_key: &str) -> anyhow::Result<u64> {
        let Some(id) = crate::storage::db::repo::parse_book_id(book_key) else {
            return Ok(0);
        };
        let path = self.root.join(user_ns).join(format!("book_{id}"));
        let book_usage = dir_usage(&path, 1).await;
        let removed = remove_dir_counting_files(&path).await?;
        self.subtract_usage(user_ns, book_usage.bytes);
        Ok(removed)
    }

    /// 删除某个用户的全部评论缓存，返回删除的文件数。
    pub async fn remove_user(&self, user_ns: &str) -> anyhow::Result<u64> {
        let removed = remove_dir_counting_files(&self.root.join(user_ns)).await?;
        self.clear_usage(user_ns);
        Ok(removed)
    }

    /// 删除所有用户的评论缓存，返回删除的文件数。
    pub async fn remove_all(&self) -> anyhow::Result<u64> {
        let removed = remove_dir_counting_files(&self.root).await?;
        self.usage.lock().unwrap().clear();
        Ok(removed)
    }

    /// 某个用户已占用的评论缓存量。
    pub async fn user_usage(&self, user_ns: &str) -> CacheUsage {
        dir_usage(&self.root.join(user_ns), 3).await
    }

    /// 全部用户已占用的评论缓存量。
    pub async fn total_usage(&self) -> CacheUsage {
        dir_usage(&self.root, 4).await
    }

    fn is_expired(&self, meta: &std::fs::Metadata) -> bool {
        if self.ttl.is_zero() {
            return false;
        }
        let Ok(modified) = meta.modified() else {
            return false;
        };
        match SystemTime::now().duration_since(modified) {
            Ok(age) => age > self.ttl,
            // 修改时间在未来（时钟回拨）：当作未过期，别误删
            Err(_) => false,
        }
    }

    async fn enforce_user_capacity(&self, user_ns: &str, written: u64) {
        let dir = self.root.join(user_ns);
        super::file_cache::enforce_scoped_capacity(
            &self.usage,
            user_ns,
            &dir,
            3,
            self.max_user_bytes,
            written,
        )
        .await;
    }

    fn subtract_usage(&self, user_ns: &str, delta: u64) {
        let mut map = self.usage.lock().unwrap();
        if let Some(entry) = map.get_mut(user_ns) {
            *entry = entry.saturating_sub(delta);
        }
    }

    fn clear_usage(&self, user_ns: &str) {
        self.usage.lock().unwrap().remove(user_ns);
    }

    fn entry_path(&self, user_ns: &str, book_key: &str, key: &str) -> PathBuf {
        // 与其他缓存路径同一道防线：非法 book_id 落无害隔离段
        let segment = match crate::storage::db::repo::parse_book_id(book_key) {
            Some(id) => format!("book_{id}"),
            None => "__invalid_book__".to_string(),
        };
        self.root
            .join(user_ns)
            .join(segment)
            .join(md5_hex(key))
            .with_extension("json")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()))
    }

    #[tokio::test]
    async fn entries_survive_within_ttl_and_drop_after_it() {
        let root = temp_root("reader-review-ttl");
        let cache = ReviewCache::new(&root, 3600, 0);
        cache
            .put("ns", "book", "chapter:1", "cached")
            .await
            .unwrap();
        assert_eq!(
            cache
                .get("ns", "book", "chapter:1")
                .await
                .unwrap()
                .as_deref(),
            Some("cached")
        );

        // 1 纳秒的 TTL：写入即过期，读取时顺带把文件删掉
        let expiring = ReviewCache::new(&root, 0, 0);
        let expiring = ReviewCache {
            ttl: Duration::from_nanos(1),
            ..expiring
        };
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert!(expiring
            .get("ns", "book", "chapter:1")
            .await
            .unwrap()
            .is_none());
        let path = root
            .join("ns")
            .join("book")
            .join(format!("{}.json", md5_hex("chapter:1")));
        assert!(!path.exists(), "过期条目应被删除");
        let _ = fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn zero_ttl_never_expires() {
        let root = temp_root("reader-review-nottl");
        let cache = ReviewCache::new(&root, 0, 0);
        cache.put("ns", "book", "k", "v").await.unwrap();
        assert_eq!(
            cache.get("ns", "book", "k").await.unwrap().as_deref(),
            Some("v")
        );
        let _ = fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn capacity_evicts_oldest_entries() {
        let root = temp_root("reader-review-cap");
        let cache = ReviewCache::new(&root, 0, 512);
        cache
            .put("ns", "book", "a", &"x".repeat(400))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        cache
            .put("ns", "book", "b", &"y".repeat(400))
            .await
            .unwrap();
        assert!(
            cache.get("ns", "book", "a").await.unwrap().is_none(),
            "最旧的条目应被淘汰"
        );
        assert!(cache.get("ns", "book", "b").await.unwrap().is_some());
        let _ = fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn purge_scopes_match_content_cache_shape() {
        let root = temp_root("reader-review-purge");
        let cache = ReviewCache::new(&root, 0, 0);
        // book_key 必须是 book_<n> 格式（非法 id 会被落进无害隔离段）
        cache.put("alice", "book_1", "c1", "a").await.unwrap();
        cache.put("alice", "book_2", "c1", "b").await.unwrap();
        cache.put("bob", "book_1", "c1", "c").await.unwrap();

        assert_eq!(cache.user_usage("alice").await.files, 2);
        assert_eq!(cache.remove_book("alice", "book_1").await.unwrap(), 1);
        assert_eq!(cache.user_usage("alice").await.files, 1);
        assert_eq!(cache.remove_user("alice").await.unwrap(), 1);
        assert!(cache.user_usage("alice").await.files == 0);
        assert_eq!(cache.total_usage().await.files, 1);
        assert_eq!(cache.remove_all().await.unwrap(), 1);
        assert_eq!(cache.total_usage().await.files, 0);
        let _ = fs::remove_dir_all(&root).await;
    }

    /// 多个写者并发 put 同一个 key：最终落盘的必须是其中一方的完整内容
    /// （唯一临时文件 + rename；固定 tmp 名时可能读到写了一半的 JSON）。
    #[tokio::test]
    async fn concurrent_puts_to_same_key_leave_intact_json() {
        let root = temp_root("reader-review-conc");
        let cache = ReviewCache::new(&root, 0, 0);
        let mut handles = Vec::new();
        for i in 0..8 {
            let cache = cache.clone();
            handles.push(tokio::spawn(async move {
                let value = format!("{{\"writer\":{i}}}");
                cache.put("ns", "book_1", "same-key", &value).await.unwrap();
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }
        let raw = cache
            .get("ns", "book_1", "same-key")
            .await
            .unwrap()
            .expect("条目应存在");
        let parsed: serde_json::Value =
            serde_json::from_str(&raw).expect("并发写下读到的必须是完整 JSON");
        let writer = parsed
            .get("writer")
            .and_then(serde_json::Value::as_u64)
            .expect("应有 writer 字段");
        assert!(writer < 8, "writer 值越界：{writer}");
        let _ = fs::remove_dir_all(&root).await;
    }
}
