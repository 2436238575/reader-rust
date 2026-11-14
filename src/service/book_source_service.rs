use crate::error::error::AppError;
use crate::model::book_source::{book_source_from_value, BookSource, BookSourceSummary};
use crate::storage::db::repo::BookSourceRepo;

pub const INVALID_BOOK_SOURCE_GROUP: &str = "失效";

#[derive(Clone)]
pub struct BookSourceService {
    repo: BookSourceRepo,
}

impl BookSourceService {
    pub fn new(repo: BookSourceRepo) -> Self {
        Self { repo }
    }

    /// 编辑器单条保存：携带的 bookSourceId 命中时按 id 更新（允许改 URL）；
    /// 未携带或 id 已不存在（陈旧会话）时回落为按 URL upsert。
    pub async fn save(&self, user_ns: &str, source: BookSource) -> Result<i64, AppError> {
        // id 由表列携带，不写进 json blob，避免同一身份两处存储漂移
        let json = serialize_without_id(&source)?;
        if let Some(id) = source.book_source_id {
            if self.repo.update_by_id(user_ns, id, &source, &json).await? {
                return Ok(id);
            }
        }
        self.repo.upsert(user_ns, &source, &json).await
    }

    /// 批量导入/订阅/恢复：URL 去重优先（保本地 id），自带 id 仅作空位提示。
    pub async fn save_many(&self, user_ns: &str, sources: Vec<BookSource>) -> Result<(), AppError> {
        let entries = sources
            .iter()
            .map(|s| {
                serialize_without_id(s)
                    .map(|json| (s.clone(), json, s.book_source_id))
                    .map_err(|e| AppError::BadRequest(e.to_string()))
            })
            .collect::<Result<Vec<_>, AppError>>()?;
        self.repo.import_many(user_ns, &entries).await
    }

    /// 事务版整表替换（WebDAV 恢复）：清空 + 导入落在调用方事务内，返回
    /// `URL → 实际 id` 供书籍重绑定。
    pub async fn replace_all_on(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        sources: Vec<BookSource>,
    ) -> Result<std::collections::HashMap<String, i64>, AppError> {
        let entries = sources
            .iter()
            .map(|s| {
                serialize_without_id(s)
                    .map(|json| (s.clone(), json, s.book_source_id))
                    .map_err(|e| AppError::BadRequest(e.to_string()))
            })
            .collect::<Result<Vec<_>, AppError>>()?;
        self.repo.delete_all_on(tx).await?;
        self.repo.import_many_on(tx, &entries).await
    }

    pub async fn get_by_id(&self, user_ns: &str, id: i64) -> Result<Option<BookSource>, AppError> {
        let row = self.repo.get_by_id(user_ns, id).await?;
        match row {
            Some((_url, json)) => Ok(Some(parse_and_inject_id(&json, id)?)),
            None => Ok(None),
        }
    }

    pub async fn list(&self, user_ns: &str) -> Result<Vec<BookSource>, AppError> {
        let rows = self.repo.list(user_ns).await?;
        let mut out = Vec::with_capacity(rows.len());
        for (id, j) in rows {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&j) {
                if let Ok(mut s) = book_source_from_value(value) {
                    s.book_source_id = Some(id);
                    out.push(s);
                }
            }
        }
        Ok(out)
    }

    /// 摘要列表：列表场景只回展示字段 + exploreUrl，不下发 MB 级规则 JSON。
    /// 每行 blob 仍在服务端解析（复用旧版迁移逻辑），用 CPU 换下行流量；
    /// 单条全量走 get_by_id，编辑/翻转开关按需取。
    ///
    /// **每次调用全量载入并逐行反序列化**：`/v1/sources` 的游标分页目前建立在
    /// 这个全量结果上（几百个书源可接受，但翻页会重复解析）。书源量增长到
    /// 万级时，分页需下沉到 SQL（`ORDER BY book_source_id` 的 keyset 查询）。
    pub async fn list_summary(&self, user_ns: &str) -> Result<Vec<BookSourceSummary>, AppError> {
        let rows = self.repo.list(user_ns).await?;
        let mut out = Vec::with_capacity(rows.len());
        for (id, j) in rows {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&j) {
                if let Ok(s) = book_source_from_value(value) {
                    out.push(BookSourceSummary::from_source(id, &s));
                }
            }
        }
        Ok(out)
    }

    /// 返回是否真的删除了行（不存在的 id 忽略）。
    pub async fn delete_by_id(&self, user_ns: &str, id: i64) -> Result<bool, AppError> {
        self.repo.delete_by_id(user_ns, id).await
    }

    /// 清空全部书源，返回删除行数。
    pub async fn delete_all(&self, user_ns: &str) -> Result<u64, AppError> {
        self.repo.delete_all(user_ns).await
    }
}

/// 序列化书源为存储 blob：id 由表列携带，blob 里不落，避免冗余漂移。
fn serialize_without_id(source: &BookSource) -> Result<String, AppError> {
    let mut value =
        serde_json::to_value(source).map_err(|e| AppError::BadRequest(e.to_string()))?;
    if let Some(obj) = value.as_object_mut() {
        obj.remove("bookSourceId");
    }
    serde_json::to_string(&value).map_err(|e| AppError::BadRequest(e.to_string()))
}

/// 解析存储 blob 并注入表列里的 id。
fn parse_and_inject_id(json: &str, id: i64) -> Result<BookSource, AppError> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| AppError::BadRequest(e.to_string()))?;
    let mut source =
        book_source_from_value(value).map_err(|e| AppError::BadRequest(e.to_string()))?;
    source.book_source_id = Some(id);
    Ok(source)
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

pub(crate) fn split_source_groups(raw: &str) -> Vec<String> {
    raw.split([',', ';', '；', '、'])
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
