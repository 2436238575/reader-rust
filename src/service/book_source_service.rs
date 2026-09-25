use crate::error::error::AppError;
use crate::model::book_source::{book_source_from_value, BookSource};
use crate::storage::db::repo::BookSourceRepo;
use crate::util::text::normalize_source_url;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::fs;

pub const INVALID_BOOK_SOURCE_GROUP: &str = "失效";

/// fallback 解析缓存的每命名空间键数上限，满了整表清空。
const RESOLUTION_CACHE_MAX_KEYS: usize = 1024;

#[derive(Clone)]
pub struct BookSourceService {
    repo: BookSourceRepo,
    default_owner_path: PathBuf,
    /// fallback 解析结果缓存：`ns -> 查询键 -> 命中的书源 URL`。
    ///
    /// origin 未直接命中主键 / 仅传 bookUrl 自动发现时，此前每次都全量
    /// 拉取并反序列化该命名空间的所有书源 JSON（几百个源就是几 MB 解析）。
    /// 缓存只存命中的 source URL（单行主键查询重建完整对象），
    /// 任何书源写入/删除都会失效对应命名空间。
    resolution_cache: Arc<Mutex<HashMap<String, HashMap<String, Option<String>>>>>,
}

impl BookSourceService {
    pub fn new(repo: BookSourceRepo, storage_dir: &str) -> Self {
        let default_owner_path = PathBuf::from(storage_dir)
            .join("data")
            .join("__default__")
            .join("defaultBookSourceOwner.txt");
        Self {
            repo,
            default_owner_path,
            resolution_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn cached_resolution(&self, user_ns: &str, key: &str) -> Option<Option<String>> {
        let cache = self
            .resolution_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        cache.get(user_ns).and_then(|m| m.get(key).cloned())
    }

    fn store_resolution(&self, user_ns: &str, key: String, value: Option<String>) {
        let mut cache = self
            .resolution_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let ns_map = cache.entry(user_ns.to_string()).or_default();
        if ns_map.len() >= RESOLUTION_CACHE_MAX_KEYS {
            ns_map.clear();
        }
        ns_map.insert(key, value);
    }

    fn invalidate_resolution(&self, user_ns: &str) {
        self.resolution_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(user_ns);
    }

    /// 归一化 URL 的 fallback 匹配（主键未直接命中时调用），带缓存。
    pub async fn find_by_normalized_url(
        &self,
        user_ns: &str,
        normalized: &str,
    ) -> Result<Option<BookSource>, AppError> {
        let key = format!("norm:{normalized}");
        if let Some(hit) = self.cached_resolution(user_ns, &key) {
            return match hit {
                Some(url) => self.get(user_ns, &url).await,
                None => Ok(None),
            };
        }
        let matched = self
            .list(user_ns)
            .await?
            .into_iter()
            .find(|s| normalize_source_url(&s.book_source_url) == normalized);
        self.store_resolution(
            user_ns,
            key,
            matched.as_ref().map(|s| s.book_source_url.clone()),
        );
        Ok(matched)
    }

    /// 按 bookUrl 的 host / 根域自动发现书源，带缓存。
    pub async fn find_by_book_host(
        &self,
        user_ns: &str,
        b_host: &str,
        b_root: &str,
    ) -> Result<Option<BookSource>, AppError> {
        let key = format!("host:{b_host}|{b_root}");
        if let Some(hit) = self.cached_resolution(user_ns, &key) {
            return match hit {
                Some(url) => self.get(user_ns, &url).await,
                None => Ok(None),
            };
        }
        let mut matched = None;
        for s in self.list(user_ns).await? {
            let normalized_source_url = normalize_source_url(&s.book_source_url);
            if let Ok(s_url) = url::Url::parse(&normalized_source_url) {
                if let Some(s_host) = s_url.host_str() {
                    let s_root = extract_root_domain(s_host);
                    if b_host.ends_with(s_host)
                        || s_host.ends_with(b_host)
                        || (b_root == s_root && !b_root.is_empty())
                    {
                        matched = Some(s);
                        break;
                    }
                }
            }
        }
        self.store_resolution(
            user_ns,
            key,
            matched.as_ref().map(|s| s.book_source_url.clone()),
        );
        Ok(matched)
    }

    pub async fn save(&self, user_ns: &str, source: BookSource) -> Result<(), AppError> {
        let json =
            serde_json::to_string(&source).map_err(|e| AppError::BadRequest(e.to_string()))?;
        self.repo.upsert(user_ns, &source, &json).await?;
        self.invalidate_resolution(user_ns);
        Ok(())
    }

    pub async fn save_many(&self, user_ns: &str, sources: Vec<BookSource>) -> Result<(), AppError> {
        let entries = sources
            .iter()
            .map(|s| {
                serde_json::to_string(s)
                    .map(|json| (s.clone(), json))
                    .map_err(|e| AppError::BadRequest(e.to_string()))
            })
            .collect::<Result<Vec<_>, AppError>>()?;
        self.repo.upsert_many(user_ns, &entries).await?;
        self.invalidate_resolution(user_ns);
        Ok(())
    }

    pub async fn get(
        &self,
        user_ns: &str,
        book_source_url: &str,
    ) -> Result<Option<BookSource>, AppError> {
        let json = self.repo.get(user_ns, book_source_url).await?;
        if let Some(j) = json {
            let value: serde_json::Value =
                serde_json::from_str(&j).map_err(|e| AppError::BadRequest(e.to_string()))?;
            let source =
                book_source_from_value(value).map_err(|e| AppError::BadRequest(e.to_string()))?;
            Ok(Some(source))
        } else {
            Ok(None)
        }
    }

    pub async fn list(&self, user_ns: &str) -> Result<Vec<BookSource>, AppError> {
        let rows = self.repo.list(user_ns).await?;
        let mut out = Vec::with_capacity(rows.len());
        for j in rows {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&j) {
                if let Ok(s) = book_source_from_value(value) {
                    out.push(s);
                }
            } else if let Ok(s) = serde_json::from_str::<BookSource>(&j) {
                out.push(s);
            }
        }
        Ok(out)
    }

    pub async fn delete(&self, user_ns: &str, book_source_url: &str) -> Result<(), AppError> {
        self.repo.delete(user_ns, book_source_url).await?;
        self.invalidate_resolution(user_ns);
        Ok(())
    }

    pub async fn delete_all(&self, user_ns: &str) -> Result<(), AppError> {
        self.repo.delete_all(user_ns).await?;
        self.invalidate_resolution(user_ns);
        Ok(())
    }

    /// Copy sources from one user to another (used for setting default sources)
    pub async fn copy_to(&self, from_ns: &str, to_ns: &str) -> Result<i64, AppError> {
        let count = self.repo.copy_to(from_ns, to_ns).await?;
        self.invalidate_resolution(to_ns);
        Ok(count)
    }

    /// Set a user's sources as the default sources (for new users)
    pub async fn set_as_default(&self, from_ns: &str) -> Result<i64, AppError> {
        let count = self.copy_to(from_ns, "__default__").await?;
        if let Some(dir) = self.default_owner_path.parent() {
            fs::create_dir_all(dir)
                .await
                .map_err(|e| AppError::Internal(e.into()))?;
        }
        fs::write(&self.default_owner_path, from_ns)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
        Ok(count)
    }

    /// Copy default sources to a new user
    pub async fn copy_default_to_user(&self, to_ns: &str) -> Result<i64, AppError> {
        let defaults = self.list("__default__").await?;
        if defaults.is_empty() {
            return Ok(0);
        }
        let count = defaults.len() as i64;
        self.save_many(to_ns, defaults).await?;
        Ok(count)
    }

    pub async fn get_default_owner(&self) -> Result<Option<String>, AppError> {
        match fs::read_to_string(&self.default_owner_path).await {
            Ok(value) => {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(trimmed.to_string()))
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(AppError::Internal(err.into())),
        }
    }
}

/// 取 host 的根域（倒数第二段）：`m.22biqu.com` → `22biqu`。
pub fn extract_root_domain(host: &str) -> String {
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() >= 2 {
        parts[parts.len() - 2].to_string()
    } else {
        host.to_string()
    }
}

pub fn book_source_has_group(source: &BookSource, target: &str) -> bool {
    source
        .book_source_group
        .as_deref()
        .map(split_source_groups)
        .unwrap_or_default()
        .into_iter()
        .any(|group| group == target)
}

pub fn set_invalid_book_source_group(source: &mut BookSource, invalid: bool) -> bool {
    let mut groups = source
        .book_source_group
        .as_deref()
        .map(split_source_groups)
        .unwrap_or_default();
    let had_invalid = groups
        .iter()
        .any(|group| group == INVALID_BOOK_SOURCE_GROUP);

    if invalid {
        if had_invalid {
            return false;
        }
        groups.push(INVALID_BOOK_SOURCE_GROUP.to_string());
    } else {
        if !had_invalid {
            return false;
        }
        groups.retain(|group| group != INVALID_BOOK_SOURCE_GROUP);
    }

    source.book_source_group = if groups.is_empty() {
        None
    } else {
        Some(groups.join(","))
    };
    true
}

fn split_source_groups(raw: &str) -> Vec<String> {
    raw.split(|ch| matches!(ch, ',' | ';' | '；' | '、'))
        .map(str::trim)
        .filter(|group| !group.is_empty())
        .map(str::to_string)
        .fold(Vec::new(), |mut groups, group| {
            if !groups.contains(&group) {
                groups.push(group);
            }
            groups
        })
}
