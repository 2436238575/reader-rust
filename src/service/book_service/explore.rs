//! 搜索 / 发现 / 书籍详情：书源的对外抓取入口与书籍详情缓存。
//!
//! `search_book` / `explore_book` / `explore_kinds` 是书源三种列表用途的
//! 入口，搜索/发现产出即登记（响应必带 bookId）；`get_book_info` 拉详情并
//! 回填登记表的书名/作者/目录地址，带 1 小时文件缓存（`refresh=1` 旁路
//! 强刷）。`test_book_source_availability` 与 `login_book_source` 供书源
//! 调试端点使用。

use super::fetch::{parse_blocking, request_spec_summary};
use super::{book_id_path_segment, BookService, BOOK_INFO_CACHE_DIR, BOOK_INFO_CACHE_TTL};
use crate::error::error::AppError;
use crate::model::book::Book;
use crate::model::book_source::{BookSource, ExploreKind};
use crate::model::search::SearchBook;
use crate::parser::js::{eval_js, with_book_source, with_user_ns};
use crate::storage::db::repo::RegisterBook;
use crate::util::atomic_write::write_atomic;
use crate::util::bounded_map::BoundedMap;
use crate::util::hash::md5_hex;
use std::path::PathBuf;
use tokio::fs;
use tokio::time::{Duration, Instant};

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BookSourceAvailability {
    pub book_source_url: String,
    pub book_source_name: String,
    pub valid: bool,
    pub search_ok: bool,
    pub explore_ok: bool,
    pub keyword: String,
    pub explore_url: Option<String>,
    pub search_error: Option<String>,
    pub explore_error: Option<String>,
}

impl BookService {
    pub async fn search_book(
        &self,
        user_ns: &str,
        source: &BookSource,
        key: &str,
        page: i32,
    ) -> Result<Vec<SearchBook>, AppError> {
        let search_url = source
            .search_url
            .clone()
            .ok_or_else(|| AppError::BadRequest("missing search_url".to_string()))?;
        tracing::info!(
            "searching book from {}: key={}, page={}, url={}",
            source.book_source_name,
            key,
            page,
            search_url
        );
        let mut spec = self
            .analyze_url_blocking(
                user_ns,
                &search_url,
                key,
                page,
                &source.book_source_url,
                source,
            )
            .await
            .map_err(|e| {
                tracing::error!("analyze_url failed: {:?}", e);
                e
            })?;

        self.apply_source_cookie(user_ns, source, &mut spec.headers)
            .await;

        tracing::debug!("search_book fetched spec: {}", request_spec_summary(&spec));
        let res = self
            .fetch_with_rate(user_ns, source, spec)
            .await
            .map_err(|e| {
                tracing::error!("fetch failed: {:?}", e);
                e
            })?;
        let res = self.login_check_blocking(user_ns, source, res).await?;
        tracing::debug!("fetch success, body length: {}", res.body.len());
        let mut books = self
            .parse_response_blocking(user_ns, source, &res, |p, s, b, u| p.search_books(s, b, u))
            .await?;
        // 搜索结果产出即登记：响应必须带 bookId，后续详情/目录/正文请求只认它
        self.register_search_books(user_ns, source, &mut books)
            .await?;
        tracing::info!("found {} books", books.len());
        Ok(books)
    }

    pub async fn explore_book(
        &self,
        user_ns: &str,
        source: &BookSource,
        rule_find_url: &str,
        page: i32,
    ) -> Result<Vec<SearchBook>, AppError> {
        if rule_find_url.trim().is_empty() {
            return Err(AppError::BadRequest("ruleFindUrl required".to_string()));
        }
        let mut spec = self
            .analyze_url_blocking(
                user_ns,
                rule_find_url,
                "",
                page,
                &source.book_source_url,
                source,
            )
            .await?;

        self.apply_source_cookie(user_ns, source, &mut spec.headers)
            .await;

        let res = self.fetch_with_rate(user_ns, source, spec).await?;
        let res = self.login_check_blocking(user_ns, source, res).await?;
        let mut books = self
            .parse_response_blocking(user_ns, source, &res, |p, s, b, u| p.explore_books(s, b, u))
            .await?;
        self.register_search_books(user_ns, source, &mut books)
            .await?;
        Ok(books)
    }

    /// 解析书源的发现分类（exploreUrl）。
    ///
    /// exploreUrl 可含 `@js:` 脚本：必须带用户命名空间进阻塞线程池执行——否则
    /// JS 里的 cache/kv/Cookie 落进共享的 "public" 桶（跨用户串号），且同步
    /// 求值会直接堵在 tokio worker 上。
    pub async fn explore_kinds(
        &self,
        user_ns: &str,
        source: &BookSource,
    ) -> Result<Vec<ExploreKind>, AppError> {
        let user_ns = user_ns.to_string();
        let source = source.clone();
        parse_blocking(move || with_user_ns(&user_ns, || parse_explore_kinds(&source, &user_ns)))
            .await?
    }

    pub async fn test_book_source_availability(
        &self,
        user_ns: &str,
        source: &BookSource,
        keyword: Option<&str>,
    ) -> BookSourceAvailability {
        let keyword = keyword
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .or_else(|| {
                source
                    .rule_search
                    .as_ref()
                    .and_then(|rule| rule.check_key_word.as_deref())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
            })
            .unwrap_or("斗破苍穹")
            .to_string();

        let (search_ok, search_error) = if source
            .search_url
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
            && source.rule_search.is_some()
        {
            match self.search_book(user_ns, source, &keyword, 1).await {
                Ok(books) => (!books.is_empty(), None),
                Err(err) => (false, Some(format!("{err:?}"))),
            }
        } else {
            (false, Some("missing searchUrl or ruleSearch".to_string()))
        };

        let explore_url = self
            .explore_kinds(user_ns, source)
            .await
            .ok()
            .and_then(|kinds| {
                kinds
                    .into_iter()
                    .filter_map(|kind| kind.url)
                    .map(|url| url.trim().to_string())
                    .find(|url| !url.is_empty())
            });
        let (explore_ok, explore_error) = if let Some(url) = explore_url.as_deref() {
            match self.explore_book(user_ns, source, url, 1).await {
                Ok(books) => (!books.is_empty(), None),
                Err(err) => (false, Some(format!("{err:?}"))),
            }
        } else {
            (false, Some("missing explore category url".to_string()))
        };

        BookSourceAvailability {
            book_source_url: source.book_source_url.clone(),
            book_source_name: source.book_source_name.clone(),
            valid: search_ok || explore_ok,
            search_ok,
            explore_ok,
            keyword,
            explore_url,
            search_error,
            explore_error,
        }
    }

    pub async fn login_book_source(
        &self,
        user_ns: &str,
        source: &BookSource,
    ) -> Result<serde_json::Value, AppError> {
        let login_url = source
            .login_url
            .clone()
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| AppError::BadRequest("missing loginUrl".to_string()))?;

        let spec = self
            .analyze_url_blocking(user_ns, &login_url, "", 1, &source.book_source_url, source)
            .await?;

        let res = self.fetch_with_rate(user_ns, source, spec).await?;
        let check_result = if let Some(login_check_js) = source
            .login_check_js
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            let user_ns_owned = user_ns.to_string();
            let js = login_check_js.to_string();
            let source_owned = source.clone();
            let body = res.body.clone();
            let url = res.url.clone();
            Some(
                parse_blocking(move || {
                    with_user_ns(&user_ns_owned, || {
                        with_book_source(&source_owned, || eval_js(&js, &body, &url))
                    })
                    .unwrap_or_default()
                })
                .await?,
            )
        } else {
            None
        };

        Ok(serde_json::json!({
            "success": true,
            "status": res.status,
            "url": res.url,
            "checkResult": check_result,
            "bodyPreview": res.body.chars().take(500).collect::<String>(),
            "bodyHtml": res.body
        }))
    }

    /// 书籍详情。缓存按 bookId 键控；解析结果顺带回填登记表的
    /// 书名/作者/目录地址（登记时可能只知道 bookUrl）。
    pub async fn get_book_info(
        &self,
        user_ns: &str,
        book_id: &str,
        source: &BookSource,
        book_url: &str,
        force_refresh: bool,
    ) -> Result<Book, AppError> {
        if !force_refresh {
            if let Some(mut cached) = self.load_book_info_cache(user_ns, book_id).await {
                cached.book_id = Some(book_id.to_string());
                cached.book_source_id = source.book_source_id;
                return Ok(cached);
            }
        }
        let res = self
            .fetch_source_url(user_ns, source, book_url, &source.book_source_url)
            .await?;
        let book_url_owned = book_url.to_string();
        let mut book = self
            .parse_response_blocking(user_ns, source, &res, move |p, s, b, u| {
                p.book_info(s, b, u, &book_url_owned)
            })
            .await?;
        book.book_id = Some(book_id.to_string());
        book.book_source_id = source.book_source_id;
        let _ = self
            .register_book(
                user_ns,
                RegisterBook {
                    book_url,
                    book_source_id: source.book_source_id,
                    name: Some(&book.name),
                    author: Some(&book.author),
                    toc_url: book.toc_url.as_deref(),
                },
            )
            .await;
        let _ = self.save_book_info_cache(user_ns, book_id, &book).await;
        Ok(book)
    }

    pub(super) fn book_info_cache_path(&self, user_ns: &str, book_id: &str) -> PathBuf {
        self.storage_dir
            .join("cache")
            .join(BOOK_INFO_CACHE_DIR)
            .join(user_ns)
            .join(format!("{}.json", book_id_path_segment(book_id)))
    }

    async fn load_book_info_cache(&self, user_ns: &str, book_id: &str) -> Option<Book> {
        let path = self.book_info_cache_path(user_ns, book_id);
        let Ok(meta) = fs::metadata(&path).await else {
            return None;
        };
        let expired = meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .map(|age| age > BOOK_INFO_CACHE_TTL)
            .unwrap_or(true);
        if expired {
            let _ = fs::remove_file(&path).await;
            return None;
        }
        let raw = fs::read_to_string(&path).await.ok()?;
        // 损坏的缓存文件按未命中处理并删除，留待重抓覆盖；不让坏文件每次都白读一遍
        match serde_json::from_str(&raw) {
            Ok(book) => Some(book),
            Err(e) => {
                tracing::warn!("book info cache corrupted, dropping: {e}");
                let _ = fs::remove_file(&path).await;
                None
            }
        }
    }

    async fn save_book_info_cache(&self, user_ns: &str, book_id: &str, book: &Book) {
        let path = self.book_info_cache_path(user_ns, book_id);
        if let Ok(data) = serde_json::to_string(book) {
            let _ = write_atomic(&path, data.as_bytes()).await;
        }
    }
}

/// exploreUrl 里 `@js:`/`<js>` 的求值结果缓存。
///
/// 规格 §13 要求按 `MD5(bookSourceUrl + exploreUrl)` 缓存：这类脚本经常要发网络
/// 请求取分类列表，而书海页每次进入都会问一次。键在规格基础上**另加用户命名
/// 空间**——脚本的求值结果可能含用户相关值（`java.androidId`、cache/kv），
/// 不能跨用户共享。另加 1 小时上限——进程长跑时不能永久陈旧的分类列表。
static EXPLORE_URL_CACHE: once_cell::sync::Lazy<std::sync::Mutex<BoundedMap<(String, Instant)>>> =
    once_cell::sync::Lazy::new(|| {
        std::sync::Mutex::new(BoundedMap::new(EXPLORE_URL_CACHE_MAX_ENTRIES))
    });
const EXPLORE_URL_CACHE_TTL: Duration = Duration::from_secs(3600);
const EXPLORE_URL_CACHE_MAX_ENTRIES: usize = 256;

fn cached_explore_script(
    source: &BookSource,
    user_ns: &str,
    raw: &str,
    evaluate: impl FnOnce() -> anyhow::Result<String>,
) -> Result<String, AppError> {
    let key = md5_hex(&format!("{user_ns}|{}|{}", source.book_source_url, raw));
    {
        let cache = EXPLORE_URL_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((value, stored_at)) = cache.get(&key) {
            if stored_at.elapsed() < EXPLORE_URL_CACHE_TTL {
                return Ok(value.clone());
            }
        }
    }

    let value = evaluate().map_err(AppError::Internal)?;
    EXPLORE_URL_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, (value.clone(), Instant::now()));
    Ok(value)
}

/// 本地书（TXT/EPUB 上传）的 bookUrl 形态，限额与清理按它识别。
pub(super) fn is_local_book_url(url: &str) -> bool {
    crate::service::local_txt_book::is_local_txt_url(url)
        || crate::service::local_epub_book::is_local_epub_url(url)
}

fn parse_explore_kinds(source: &BookSource, user_ns: &str) -> Result<Vec<ExploreKind>, AppError> {
    let Some(raw) = source
        .explore_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(Vec::new());
    };

    let text = with_book_source(source, || {
        if let Some(script) = raw.strip_prefix("@js:") {
            cached_explore_script(source, user_ns, raw, || {
                eval_js(script, "", &source.book_source_url)
            })
        } else if let Some(script) = raw
            .strip_prefix("<js>")
            .and_then(|value| value.strip_suffix("</js>"))
        {
            cached_explore_script(source, user_ns, raw, || {
                eval_js(script, "", &source.book_source_url)
            })
        } else {
            Ok(raw.to_string())
        }
    })?;

    for json_text in [&text, &normalize_relaxed_explore_json(&text)] {
        if let Ok(kinds) = serde_json::from_str::<Vec<ExploreKind>>(json_text) {
            return Ok(kinds
                .into_iter()
                .filter(|kind| !kind.title.trim().is_empty())
                .collect());
        }
    }

    let splitter = regex::Regex::new(r"(&&|\n)+").unwrap();
    Ok(splitter
        .split(&text)
        .filter_map(|item| {
            let item = item.trim();
            if item.is_empty() {
                return None;
            }
            let mut parts = item.splitn(2, "::");
            let title = parts.next().unwrap_or_default().trim();
            if title.is_empty() {
                return None;
            }
            let url = parts
                .next()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            Some(ExploreKind {
                title: title.to_string(),
                url,
                style: None,
            })
        })
        .collect())
}

fn normalize_relaxed_explore_json(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    let mut in_string = false;
    let mut quote = '\0';
    let mut escaped = false;

    for ch in text.chars() {
        if in_string {
            normalized.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == quote {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' | '\'' => {
                in_string = true;
                quote = ch;
                normalized.push(ch);
            }
            '<' => normalized.push('{'),
            '>' => normalized.push('}'),
            _ => normalized.push(ch),
        }
    }

    normalized
}
