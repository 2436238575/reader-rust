//! 书籍服务：书源抓取、目录、正文、评论、书架与缓存的业务编排。
//!
//! 按功能簇拆分为同名目录下的子模块：`fetch` 是出站抓取地基（URL 分析、
//! 限速、登录检查、阻塞线程池解析），`registry` 管书籍登记与书源 Cookie，
//! `explore` 对应搜索/发现/详情，`toc` 管目录抓取与缓存，`content` 管正文
//! 抓取与回源（singleflight），`review` 管章评/段评/章节配图，`shelf` 管
//! 书架读写，`cache` 管可再生缓存的清理与统计，`cached_search` 管缓存内
//! 全文检索。本文件保留 `BookService` 结构体定义、构造与访问器、跨簇共用
//! 的常量与 `book_id_path_segment`，并对对外引用的类型做统一再导出，
//! `crate::service::book_service::*` 的外部路径不因拆分而变化。

use crate::crawler::http_client::HttpClient;
use crate::parser::rule_engine::RuleEngine;
use crate::service::chapter_content_service::ChapterContentService;
use crate::storage::cache::review_cache::ReviewCache;
use crate::storage::db::repo::{BookRepo, ShelfBookRepo};
use crate::util::bounded_map::BoundedMap;
use crate::util::keyed_mutex::KeyedMutex;
use content::RecentBody;
use fetch::RateState;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use toc::TocSnapshotMemoEntry;
use tokio::sync::RwLock;
use tokio::time::Duration;

mod cache;
mod cached_search;
mod content;
mod explore;
mod fetch;
mod registry;
mod review;
mod shelf;
mod toc;

pub use cache::{CacheKind, CachePurgeResult};
pub use cached_search::{CachedSearchHit, CachedSearchOutcome};
pub use content::ChapterContentRead;
pub use explore::BookSourceAvailability;
pub use review::REVIEW_PAGE_SIZE;
pub use toc::{ChapterPagination, TocFillGuard};

#[derive(Clone)]
pub struct BookService {
    http: HttpClient,
    parser: RuleEngine,
    content: Arc<ChapterContentService>,
    review_cache: ReviewCache,
    storage_dir: PathBuf,
    book_repo: BookRepo,
    shelf_repo: ShelfBookRepo,
    source_cookies: Arc<RwLock<HashMap<String, String>>>,
    rate_states: Arc<RwLock<HashMap<String, RateState>>>,
    /// 最近抓到的章节正文响应体，用于求值评论 URL（见 `chapter_source_body`）。
    recent_bodies: Arc<RwLock<HashMap<String, RecentBody>>>,
    /// 单用户书架上限（0 = 不限），来自 `USER_BOOK_LIMIT`。
    user_book_limit: u32,
    /// 单用户本地书上限（0 = 不限），来自 `USER_LOCAL_BOOK_LIMIT`。
    user_local_book_limit: u32,
    /// 正在后台补全目录的 `(user_ns, book_id)` 集合：防止同一本书并发触发
    /// 两个补全任务（竞态下各自抓全部分页并各 append 一次，缓存里出现重复章节）。
    pending_toc_fills: Arc<Mutex<HashSet<(String, String)>>>,
    /// 正在后台回源的 `(book_id, chapter_url)` 集合：到期后的并发请求只起一个任务。
    pending_revalidations: Arc<Mutex<HashSet<(String, String)>>>,
    /// 串行化目录缓存的读-改-写（append）：无锁交错会互相覆盖丢章。
    /// 全局一把锁即可——append 只在目录后台补全时发生，频率低、临界区短。
    toc_append_lock: Arc<tokio::sync::Mutex<()>>,
    /// 目录快照备忘（见 `toc_snapshot_map`）：批量落库复用一份 parse 结果。
    toc_snapshots: Arc<Mutex<BoundedMap<TocSnapshotMemoEntry>>>,
    /// 正文回源 singleflight：`(user_ns, book_id, chapter_url)` -> 按键互斥锁。
    /// 并发未命中/并发到期回源的同一章只有一个真正打上游，其余等锁后
    /// 二次检查缓存直接复用。KeyedMutex 限条数：取消/panic 遗留的条目
    /// 在持有者 drop 后变为空闲可被逐出，在用锁不会被挤掉
    /// （详见 `util::keyed_mutex`）。
    refresh_locks: Arc<KeyedMutex>,
}

/// 备忘条目上限：同时批量缓存的书籍数有限，16 本足以覆盖且控住内存。
const TOC_SNAPSHOT_MEMO_MAX_ENTRIES: usize = 16;
/// 回源 singleflight 锁表上限：正常路径用完即清，上限只为兜住取消/panic
/// 遗留的条目。
const REFRESH_LOCK_MAX_ENTRIES: usize = 256;

/// 评论缓存的默认有效期（7 天）与单用户容量上限（64 MiB）。
const DEFAULT_REVIEW_CACHE_TTL_SECS: u64 = 7 * 24 * 60 * 60;
const DEFAULT_REVIEW_CACHE_BYTES: u64 = 64 * 1024 * 1024;

/// 书籍详情缓存的存活时间（1 小时）与目录名。
///
/// 详情页、仅传 bookUrl 拉目录、翻章按 index 换算 chapterUrl 都会走
/// `get_book_info`，此前每次都打上游。详情不常变，短 TTL 足够新；
/// `refresh=1` 与 saveBook/换源路径走旁路强刷。文件随 `CacheKind::ChapterList`
/// 清理（详情与目录同属书籍元数据）。
const BOOK_INFO_CACHE_TTL: Duration = Duration::from_secs(3600);
const BOOK_INFO_CACHE_DIR: &str = "bookinfo";

impl BookService {
    pub fn new(
        http: HttpClient,
        parser: RuleEngine,
        content: Arc<ChapterContentService>,
        storage_dir: &str,
        book_repo: BookRepo,
        shelf_repo: ShelfBookRepo,
    ) -> Self {
        let storage_dir = PathBuf::from(storage_dir);
        let review_cache = ReviewCache::new(
            storage_dir.join("cache").join("reviews"),
            DEFAULT_REVIEW_CACHE_TTL_SECS,
            DEFAULT_REVIEW_CACHE_BYTES,
        );
        Self {
            http,
            parser,
            content,
            review_cache,
            storage_dir,
            book_repo,
            shelf_repo,
            source_cookies: Arc::new(RwLock::new(HashMap::new())),
            rate_states: Arc::new(RwLock::new(HashMap::new())),
            recent_bodies: Arc::new(RwLock::new(HashMap::new())),
            user_book_limit: 0,
            user_local_book_limit: 0,
            pending_toc_fills: Arc::new(Mutex::new(HashSet::new())),
            pending_revalidations: Arc::new(Mutex::new(HashSet::new())),
            toc_append_lock: Arc::new(tokio::sync::Mutex::new(())),
            toc_snapshots: Arc::new(Mutex::new(BoundedMap::new(TOC_SNAPSHOT_MEMO_MAX_ENTRIES))),
            refresh_locks: Arc::new(KeyedMutex::new("chapter_refresh", REFRESH_LOCK_MAX_ENTRIES)),
        }
    }

    /// 设置评论缓存的有效期与单用户容量上限（0 = 不过期 / 不限制）。
    pub fn with_review_cache(mut self, ttl_secs: u64, max_user_bytes: u64) -> Self {
        self.review_cache = ReviewCache::new(
            self.storage_dir.join("cache").join("reviews"),
            ttl_secs,
            max_user_bytes,
        );
        self
    }

    /// 设置单用户书架上限（`USER_BOOK_LIMIT`；0 = 不限）。
    pub fn with_user_book_limit(mut self, limit: u32) -> Self {
        self.user_book_limit = limit;
        self
    }

    /// 设置单用户本地书上限（`USER_LOCAL_BOOK_LIMIT`；0 = 不限）。
    pub fn with_user_local_book_limit(mut self, limit: u32) -> Self {
        self.user_local_book_limit = limit;
        self
    }

    /// 取某个用户命名空间下的独立 HTTP 客户端（独立 Cookie jar）。
    pub fn http_client(&self, user_ns: &str) -> anyhow::Result<reqwest::Client> {
        self.http.client_for(user_ns)
    }

    // ── 书籍登记表（bookId 的分配与解析） ────────────────────────────────

    pub fn book_repo(&self) -> &BookRepo {
        &self.book_repo
    }

    #[cfg(test)]
    pub fn content(&self) -> &ChapterContentService {
        &self.content
    }
}

/// bookId 拼进缓存文件路径前的归一：别名（`book_007`/`book_+7`）归一为
/// canonical `book_<n>`（同一本书不该有两个缓存文件），非法 id 落隔离段——
/// 与缓存路径的 book_id 归一策略一致：读返回空、写进隔离区、删除是 no-op。
/// handler 层的 resolve_book 是主防线，这层兜底防未来新调用点绕开它。
fn book_id_path_segment(book_id: &str) -> String {
    crate::storage::db::repo::canonical_book_id(book_id)
        .unwrap_or_else(|| "__invalid_book__".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::book::Book;
    use crate::storage::db;
    use crate::storage::db::repo::{BookRepo, ShelfBookRepo};

    /// 返回池句柄：收尾必须先 `pool.close().await` 再删目录（Windows 文件锁，
    /// 见 repo.rs 测试约定）。
    pub(super) async fn test_service(
        storage_dir: &std::path::Path,
    ) -> (BookService, sqlx::SqlitePool) {
        crate::util::test_temp::clean_stale_test_dirs();
        // init_pool 不会建父目录，先落目录再开库
        std::fs::create_dir_all(storage_dir).unwrap();
        let pool = db::init_pool(&format!(
            "sqlite:{}?mode=rwc",
            storage_dir.join("reader.db").display()
        ))
        .await
        .unwrap();
        let content = Arc::new(ChapterContentService::new(
            crate::storage::db::content_repo::ChapterContentRepo::new(pool.clone()),
            0,
        ));
        let service = BookService::new(
            HttpClient::new(5, None).unwrap(),
            RuleEngine::new().unwrap(),
            content,
            storage_dir.to_str().unwrap(),
            BookRepo::new(pool.clone()),
            ShelfBookRepo::new(pool.clone()),
        );
        (service, pool)
    }

    pub(super) fn make_book(url: &str) -> Book {
        Book {
            book_url: url.to_string(),
            origin: "local".to_string(),
            name: format!("书 {url}"),
            ..Default::default()
        }
    }
}
