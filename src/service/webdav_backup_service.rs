//! WebDAV 备份文档：服务端快照（书架/分组/书源/书签/替换规则）与「以备份为准」
//! 的整体恢复。
//!
//! 备份文件是 `reader-backup-<时间>.json`，字段与旧版前端组装的格式一致
//! （`bookshelf.books/groups`、`bookSources`、`bookmarks`、`replaceRules`、
//! `localState`），因此旧备份可直接恢复。`localState` 是浏览器 localStorage
//! 的快照：创建备份时由前端放入请求体，服务端只负责原样存回文档、恢复时
//! 原样返回（前端按已知键落地，见 `webdavLocalState.ts`）。`localState`
//! 缺失表示「这份备份没有本地配置」（旧备份/手写文档），与显式空对象不同：
//! 前端对缺失值不清空本地键。
//!
//! 恢复语义是**删除 + 重建**：
//! - 书源：清空后按备份导入（`bookSourceId` 在空表上原位恢复）；
//! - 分组：整表覆盖（不在备份里的组删除，书籍位掩码里的对应位一并清掉）；
//! - 书架：`save_books(restore=true)` 只清当前用户书架，登记表与正文历史保留；
//! - 书签 / 替换规则：`replace_all` 清空重建。
//!
//! 恢复的 bookId 不保证与备份一致：同 URL 的既有登记行会复用原 id，备份
//! 自带 id 被别的 URL 占用时会重新分配。因此书签按 `backup bookId → 恢复后
//! bookId` 映射重写，替换规则的 `book:` / `source:` scope 同样重映射；映射
//! 不上的书签计入 `skippedBookmarks`。
//!
//! **五个域在同一个 `BEGIN IMMEDIATE` 事务里**：任何一步失败（DB 错误、
//! 磁盘满、future 被取消）整体回滚，不会留下「书源已换、书架已清」的半恢复
//! 状态。写库前仍集中校验所有能在落库前发现的非法输入（版本、字段缺失、
//! 书籍 URL/数量/ID、书签 kind/chapterPos），校验失败不开启事务。
//! 非法分组 id（旧版 max_id+1 分配遗留的 3 这种）按旧前端同口径丢弃，书籍
//! 位掩码里的对应位一并剥掉，不让老 bug 借恢复回灌。

use crate::error::error::AppError;
use crate::model::book::Book;
use crate::model::book_group::BookGroup;
use crate::model::book_source::BookSource;
use crate::model::bookmark::Bookmark;
use crate::model::replace_rule::ReplaceRule;
use crate::service::book_group_service::{validate_group_id, BookGroupService};
use crate::service::book_service::BookService;
use crate::service::book_source_service::BookSourceService;
use crate::storage::db::repo::{parse_book_id, BookmarkRepo, ReplaceRuleRepo};
use crate::util::time::{now_ts, rfc3339_secs};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use utoipa::ToSchema;

/// 备份文档版本。旧前端只写 1；恢复只接受 1（未知版本可能带不同字段语义，
/// 静默忽略字段会造成「看起来恢复成功、数据少了半截」）。
pub const WEBDAV_BACKUP_VERSION: u32 = 1;

/// 服务端生成备份时写入的 `app` 标识。旧备份是 `reader-rust-frontend`，
/// 恢复不校验该字段。
pub const WEBDAV_BACKUP_APP: &str = "reader-rust";

/// 备份文档。**必填域不能缺**：`bookshelf.books/groups`、`bookSources`、
/// `bookmarks`、`replaceRules` 少任何一个都会在反序列化阶段 422，绝不按
/// 空数组继续——否则一个 `{"version":1}` 就能把全部数据清空。
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WebdavBackupDocument {
    pub version: u32,
    /// RFC 3339 UTC；旧备份/手写文档允许缺省
    #[serde(default)]
    #[schema(format = DateTime)]
    pub created_at: String,
    #[serde(default)]
    pub app: String,
    pub bookshelf: WebdavBackupBookshelf,
    pub book_sources: Vec<BookSource>,
    pub bookmarks: Vec<WebdavBackupBookmark>,
    pub replace_rules: Vec<ReplaceRule>,
    /// 浏览器本地配置快照；`None` = 这份备份不含本地配置（恢复时不清本地键）
    #[serde(default)]
    pub local_state: Option<BTreeMap<String, String>>,
}

/// 备份里的书签。与 `Bookmark` 的唯一差别是多一个 `bookUrl`：书架外的书签
/// 没有 `bookshelf.books` 条目可映射，恢复时按 URL 找回登记行，避免把
/// 「已移出书架但仍有书签」的数据静默丢掉。旧备份没有该字段，按 id 映射
/// 兜底（映射不上则计入 skipped）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct WebdavBackupBookmark {
    pub book_id: String,
    pub time: i64,
    pub book_name: String,
    pub book_author: String,
    pub chapter_index: i32,
    pub chapter_pos: i32,
    pub chapter_name: String,
    pub book_text: String,
    pub content: String,
    pub kind: String,
    /// 备份时的登记表 URL；书架内的书签可省略（按 bookId 映射即可）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub book_url: Option<String>,
}

impl WebdavBackupBookmark {
    fn into_bookmark(self) -> Bookmark {
        Bookmark {
            book_id: self.book_id,
            time: self.time,
            book_name: self.book_name,
            book_author: self.book_author,
            chapter_index: self.chapter_index,
            chapter_pos: self.chapter_pos,
            chapter_name: self.chapter_name,
            book_text: self.book_text,
            content: self.content,
            kind: self.kind,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WebdavBackupBookshelf {
    /// 书架顺序（`get_bookshelf_ordered` 的位置顺序）
    pub books: Vec<Book>,
    pub groups: Vec<BookGroup>,
}

/// 恢复结果：各域处理条数与需要前端落地的 localStorage 快照。
///
/// 计数是**提交条数**：重复身份（同 URL 书源、同名规则、同五元组书签）会被
/// 存储层 upsert 折叠，最终行数可能更少。
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WebdavRestoreResult {
    pub books: u32,
    pub groups: u32,
    pub book_sources: u32,
    pub bookmarks: u32,
    /// 指向备份书架之外的书籍而跳过的书签数
    pub skipped_bookmarks: u32,
    pub replace_rules: u32,
    /// `None` = 备份本身不含本地配置，前端不应改动现有 localStorage
    pub local_state: Option<BTreeMap<String, String>>,
}

/// WebDAV 备份/恢复服务。依赖的域服务与 repo 都只做数据访问，恢复的顺序与
/// 覆盖语义集中在这里，避免 HTTP 层散落跨域编排。
pub struct WebdavBackupService {
    book_service: Arc<BookService>,
    book_source_service: Arc<BookSourceService>,
    book_group_service: Arc<BookGroupService>,
    bookmark_repo: BookmarkRepo,
    replace_rule_repo: ReplaceRuleRepo,
    /// 同一进程只允许一个恢复在跑（跨表重建 + 100MB 文档整读）
    restore_lock: tokio::sync::Mutex<()>,
}

impl WebdavBackupService {
    pub fn new(
        book_service: Arc<BookService>,
        book_source_service: Arc<BookSourceService>,
        book_group_service: Arc<BookGroupService>,
        bookmark_repo: BookmarkRepo,
        replace_rule_repo: ReplaceRuleRepo,
    ) -> Self {
        Self {
            book_service,
            book_source_service,
            book_group_service,
            bookmark_repo,
            replace_rule_repo,
            restore_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// 当前账号的完整备份快照。`local_state` 由调用方（前端）提供；服务端
    /// 不知道浏览器存储，不能自行采集。
    pub async fn snapshot(
        &self,
        user_ns: &str,
        local_state: Option<BTreeMap<String, String>>,
    ) -> Result<WebdavBackupDocument, AppError> {
        let books: Vec<Book> = self
            .book_service
            .get_bookshelf_ordered(user_ns)
            .await?
            .into_iter()
            .map(|(book, _position)| book)
            .collect();
        let groups = self.book_group_service.get_groups(user_ns).await?;
        let book_sources = self.book_source_service.list(user_ns).await?;
        let marks = self.bookmark_repo.list(user_ns).await?;
        let replace_rules = self.replace_rule_repo.list(user_ns).await?;

        // 书架外的书签没有 books 条目可映射，快照时补 bookUrl 供恢复按 URL
        // 找回登记行；书架内的书签按 bookId 映射即可，不重复写 URL。
        let shelf_ids: HashSet<String> = books
            .iter()
            .filter_map(|book| book.book_id.clone())
            .collect();
        let mut url_cache: HashMap<String, Option<String>> = HashMap::new();
        let mut bookmarks = Vec::with_capacity(marks.len());
        for mark in marks {
            let book_url = if shelf_ids.contains(&mark.book_id) {
                None
            } else {
                match url_cache.get(&mark.book_id) {
                    Some(url) => url.clone(),
                    None => {
                        let url = self
                            .book_service
                            .book_repo()
                            .get(user_ns, &mark.book_id)
                            .await?
                            .map(|row| row.book_url);
                        url_cache.insert(mark.book_id.clone(), url.clone());
                        url
                    }
                }
            };
            bookmarks.push(WebdavBackupBookmark {
                book_id: mark.book_id,
                time: mark.time,
                book_name: mark.book_name,
                book_author: mark.book_author,
                chapter_index: mark.chapter_index,
                chapter_pos: mark.chapter_pos,
                chapter_name: mark.chapter_name,
                book_text: mark.book_text,
                content: mark.content,
                kind: mark.kind,
                book_url,
            });
        }

        Ok(WebdavBackupDocument {
            version: WEBDAV_BACKUP_VERSION,
            created_at: rfc3339_secs(now_ts()),
            app: WEBDAV_BACKUP_APP.to_string(),
            bookshelf: WebdavBackupBookshelf { books, groups },
            book_sources,
            bookmarks,
            replace_rules,
            local_state,
        })
    }

    /// 「以备份为准」恢复：校验在前，五域写入包在同一个事务里。
    pub async fn restore(
        &self,
        user_ns: &str,
        document: WebdavBackupDocument,
    ) -> Result<WebdavRestoreResult, AppError> {
        // 单进程只允许一个恢复：100MB 文档整读 + 五域顺序重建，并发恢复
        // 只会互相覆盖并把内存峰值翻倍
        let _restore_guard = self
            .restore_lock
            .try_lock()
            .map_err(|_| AppError::Conflict("已有恢复任务正在执行".to_string()))?;
        if document.version != WEBDAV_BACKUP_VERSION {
            return Err(AppError::BadRequest(format!(
                "不支持的备份版本 {}（只接受 {WEBDAV_BACKUP_VERSION}）",
                document.version
            )));
        }

        // 非法分组 id 丢弃（旧前端同口径），书籍位掩码只保留有效位
        let groups: Vec<BookGroup> = document
            .bookshelf
            .groups
            .into_iter()
            .filter(|group| validate_group_id(group.group_id).is_ok())
            .collect();
        let group_mask = groups
            .iter()
            .fold(0i64, |mask, group| mask | group.group_id);
        let mut books = document.bookshelf.books;
        for book in &mut books {
            if let Some(group) = book.group {
                book.group = Some(group & group_mask);
            }
        }

        // 写库前集中校验：先按 save_books 同一套规则规范 URL，再查数量/
        // bookId/书签量程/组织结构，任何失败都不碰现有数据
        self.book_service.normalize_books_for_restore(&mut books);
        self.book_service.validate_restore_books(&books, true)?;
        for source in &document.book_sources {
            if source.book_source_url.trim().is_empty() {
                return Err(AppError::BadRequest(
                    "备份包含缺少 bookSourceUrl 的书源".to_string(),
                ));
            }
            if source.book_source_name.trim().is_empty() {
                return Err(AppError::BadRequest(
                    "备份包含缺少 bookSourceName 的书源".to_string(),
                ));
            }
        }
        for rule in &document.replace_rules {
            if rule.name.trim().is_empty() || rule.pattern.is_empty() {
                return Err(AppError::BadRequest(
                    "备份包含缺少 name/pattern 的替换规则".to_string(),
                ));
            }
        }
        for bookmark in &document.bookmarks {
            if bookmark.book_id.trim().is_empty() || parse_book_id(&bookmark.book_id).is_none() {
                return Err(AppError::BadRequest(format!(
                    "备份包含非法书签 bookId: {:?}",
                    bookmark.book_id
                )));
            }
            if !bookmark.kind.is_empty()
                && bookmark.kind != "bookmark"
                && bookmark.kind != "underline"
            {
                return Err(AppError::BadRequest(format!(
                    "书签 kind 只支持 bookmark/underline，收到 {:?}",
                    bookmark.kind
                )));
            }
            if !(0..=10000).contains(&bookmark.chapter_pos) {
                return Err(AppError::BadRequest(
                    "书签 chapterPos 必须在 0–10000 之间".to_string(),
                ));
            }
        }

        // bookId 重映射：同 URL 既有登记行会复用原 id，备份 id 被占用时会重分配
        let backup_book_ids: Vec<Option<String>> =
            books.iter().map(|book| book.book_id.clone()).collect();
        let backup_source_ids: Vec<(String, Option<i64>)> = document
            .book_sources
            .iter()
            .map(|source| (source.book_source_url.clone(), source.book_source_id))
            .collect();

        // 五个域共用同一个 BEGIN IMMEDIATE 事务：任何一步失败（DB/磁盘/
        // 进程取消）整体回滚，不会留下「书源已换、书架已清」的半恢复状态。
        let mut tx = self.bookmark_repo.begin_immediate().await?;

        // 书源先重建：书籍登记行带着 bookSourceId，恢复后要能解析回书源
        let book_sources = document.book_sources;
        let book_sources_count = book_sources.len() as u32;
        let url_to_source_id = self
            .book_source_service
            .replace_all_on(&mut tx, book_sources)
            .await?;
        let mut source_id_map = HashMap::new();
        for (url, backup_id) in &backup_source_ids {
            if let (Some(backup_id), Some(actual_id)) = (backup_id, url_to_source_id.get(url)) {
                source_id_map.insert(*backup_id, *actual_id);
            }
        }
        // 书籍绑定按恢复后的 source id 重写；备份里没有的源一律解绑，避免
        // 恢复后留下指向已删除 source 的悬空 bookSourceId
        for book in &mut books {
            if let Some(source_id) = book.book_source_id {
                book.book_source_id = source_id_map.get(&source_id).copied();
            }
        }

        let groups_count = groups.len() as u32;
        self.book_group_service
            .save_groups_on(&mut tx, user_ns, &groups)
            .await?;

        // restore=true 只清书架；登记表与正文历史保留，书签按 bookId 继续对齐
        let restored_books = self
            .book_service
            .save_books_on(&mut tx, user_ns, books, true)
            .await?;
        let books_count = restored_books.len() as u32;
        let book_id_map = build_book_id_map(&backup_book_ids, &restored_books);
        let restored_urls: HashMap<String, String> = restored_books
            .iter()
            .filter_map(|book| {
                book.book_id
                    .as_ref()
                    .map(|id| (id.clone(), book.book_url.clone()))
            })
            .collect();

        let (mark_items, skipped_bookmarks) = self
            .resolve_bookmarks(user_ns, document.bookmarks, &book_id_map, &restored_urls)
            .await?;
        let bookmarks_count = mark_items.len() as u32;
        self.bookmark_repo
            .replace_all_on(&mut tx, user_ns, &mark_items)
            .await?;

        let mut replace_rules = document.replace_rules;
        for rule in &mut replace_rules {
            if let Some(scope) = &rule.scope {
                rule.scope = Some(remap_scope(scope, &book_id_map, &source_id_map));
            }
        }
        let replace_rules_count = replace_rules.len() as u32;
        self.replace_rule_repo
            .replace_all_on(&mut tx, user_ns, &replace_rules)
            .await?;

        tx.commit().await?;

        Ok(WebdavRestoreResult {
            books: books_count,
            groups: groups_count,
            book_sources: book_sources_count,
            bookmarks: bookmarks_count,
            skipped_bookmarks,
            replace_rules: replace_rules_count,
            local_state: document.local_state,
        })
    }

    /// 书签落库前解析目标书：备份书架内的按 `book_id_map` 映射并用
    /// `restored_urls` 取 URL（事务内未提交的登记行不能被池查询看到）；书架
    /// 外的新备份用快照里的 `bookUrl` 找回已提交的登记行；旧备份没有 URL
    /// 或 URL 不再存在时计入 skipped。绝不按旧 id 回查——目标部署的同 id
    /// 可能已被另一本书占用，会把书签挂错书。
    async fn resolve_bookmarks(
        &self,
        user_ns: &str,
        bookmarks: Vec<WebdavBackupBookmark>,
        book_id_map: &HashMap<String, String>,
        restored_urls: &HashMap<String, String>,
    ) -> Result<(Vec<(Bookmark, String)>, u32), AppError> {
        let mut items = Vec::with_capacity(bookmarks.len());
        let mut skipped = 0u32;
        for backup_mark in bookmarks {
            let (target_id, target_url) =
                if let Some(mapped) = book_id_map.get(&backup_mark.book_id) {
                    let Some(url) = restored_urls.get(mapped) else {
                        skipped += 1;
                        continue;
                    };
                    (mapped.clone(), url.clone())
                } else if let Some(book_url) = backup_mark.book_url.as_deref() {
                    match self
                        .book_service
                        .book_repo()
                        .get_by_url(user_ns, book_url)
                        .await?
                    {
                        Some(row) => (row.book_id, row.book_url),
                        None => {
                            skipped += 1;
                            continue;
                        }
                    }
                } else {
                    skipped += 1;
                    continue;
                };
            let mut bookmark = backup_mark.into_bookmark();
            bookmark.book_id = target_id;
            items.push((bookmark, target_url));
        }
        Ok((items, skipped))
    }
}

/// 备份 bookId → 恢复后 bookId。`save_books` 保持输入顺序，按下标对齐。
fn build_book_id_map(backup_ids: &[Option<String>], restored: &[Book]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for (backup_id, book) in backup_ids.iter().zip(restored) {
        if let (Some(backup_id), Some(restored_id)) = (backup_id, book.book_id.as_ref()) {
            map.insert(backup_id.clone(), restored_id.clone());
        }
    }
    map
}

/// 替换规则的 `book:<id>` / `source:<id>` scope 按恢复后的实际 id 重写；
/// 映射不上（目标不在备份里）时保持原值——scope 只是存储标签，不校验存在性。
fn remap_scope(
    scope: &str,
    book_id_map: &HashMap<String, String>,
    source_id_map: &HashMap<i64, i64>,
) -> String {
    if let Some(book_id) = scope.strip_prefix("book:") {
        if let Some(mapped) = book_id_map.get(book_id) {
            return format!("book:{mapped}");
        }
        return scope.to_string();
    }
    if let Some(raw) = scope.strip_prefix("source:") {
        if let Ok(id) = raw.parse::<i64>() {
            if let Some(mapped) = source_id_map.get(&id) {
                return format!("source:{mapped}");
            }
        }
    }
    scope.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_remap_rewrites_known_ids_only() {
        let mut books = HashMap::new();
        books.insert("book_1".to_string(), "book_9".to_string());
        let mut sources = HashMap::new();
        sources.insert(3, 7);
        assert_eq!(remap_scope("book:book_1", &books, &sources), "book:book_9");
        assert_eq!(remap_scope("book:book_2", &books, &sources), "book:book_2");
        assert_eq!(remap_scope("source:3", &books, &sources), "source:7");
        assert_eq!(remap_scope("source:4", &books, &sources), "source:4");
        assert_eq!(remap_scope("global", &books, &sources), "global");
        assert_eq!(
            remap_scope("source:not-a-number", &books, &sources),
            "source:not-a-number"
        );
    }

    #[test]
    fn book_id_map_aligns_by_position() {
        let backup_ids = vec![Some("book_1".to_string()), None, Some("book_3".to_string())];
        let restored = vec![
            Book {
                book_id: Some("book_7".to_string()),
                ..Book::default()
            },
            Book {
                book_id: Some("book_8".to_string()),
                ..Book::default()
            },
            Book {
                book_id: Some("book_9".to_string()),
                ..Book::default()
            },
        ];
        let map = build_book_id_map(&backup_ids, &restored);
        assert_eq!(map.get("book_1").map(String::as_str), Some("book_7"));
        assert_eq!(map.get("book_3").map(String::as_str), Some("book_9"));
        assert!(!map.contains_key("book_8"));
    }
}
