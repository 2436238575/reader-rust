//! 书籍登记与书源 Cookie：bookId 的分配/复用与按域名的 Cookie 注入。
//!
//! `register_book` / `register_search_books` 是登记表（`books` 表）的唯一
//! 写入入口：URL 已登记时复用原 id（删除后不复用），搜索/发现产出即登记。
//! Cookie 按 `(user_ns, 域名)` 键控（`cookie_domain` 归一到注册域），
//! 抓取时在请求头缺少 Cookie 时注入。

use super::BookService;
use crate::error::error::AppError;
use crate::model::book_source::BookSource;
use crate::model::search::SearchBook;
use crate::storage::db::repo::RegisterBook;
use crate::util::text::normalize_source_url;

impl BookService {
    /// 登记一本书并返回 bookId；URL 已登记时复用原 id 并补全可变字段。
    pub async fn register_book(
        &self,
        user_ns: &str,
        item: RegisterBook<'_>,
    ) -> Result<String, AppError> {
        self.book_repo.register(user_ns, item, None).await
    }

    /// 批量登记搜索/发现结果，把 bookId/bookSourceId 写回结果对象。
    pub async fn register_search_books(
        &self,
        user_ns: &str,
        source: &BookSource,
        books: &mut [SearchBook],
    ) -> Result<(), AppError> {
        let items: Vec<RegisterBook> = books
            .iter()
            .filter(|b| !b.book_url.trim().is_empty())
            .map(|b| RegisterBook {
                book_url: &b.book_url,
                book_source_id: source.book_source_id,
                name: Some(&b.name),
                author: Some(&b.author),
                toc_url: None,
            })
            .collect();
        if items.is_empty() {
            return Ok(());
        }
        let ids = self.book_repo.register_many(user_ns, &items).await?;
        for book in books.iter_mut() {
            book.book_id = ids.get(&book.book_url).cloned();
            book.book_source_id = source.book_source_id;
        }
        Ok(())
    }

    fn source_cookie_key(&self, user_ns: &str, source_url: &str) -> String {
        format!("{}::{}", user_ns, cookie_domain(source_url))
    }

    pub(super) async fn apply_source_cookie(
        &self,
        user_ns: &str,
        source: &BookSource,
        headers: &mut Vec<(String, String)>,
    ) {
        let key = self.source_cookie_key(user_ns, &source.book_source_url);
        if let Some(cookie) = self.source_cookies.read().await.get(&key).cloned() {
            if !headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("cookie"))
            {
                headers.push(("Cookie".to_string(), cookie));
            }
        }
    }

    pub async fn set_source_cookie(&self, user_ns: &str, source_url: &str, cookie: &str) {
        let cookie = cookie.trim();
        if cookie.is_empty() {
            return;
        }
        let key = self.source_cookie_key(user_ns, source_url);
        self.source_cookies
            .write()
            .await
            .insert(key, cookie.to_string());
    }
}

fn cookie_domain(source_url: &str) -> String {
    let normalized = normalize_source_url(source_url);
    let host = url::Url::parse(&normalized)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or(normalized);
    if host.parse::<std::net::IpAddr>().is_ok() {
        return host;
    }
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let parts = host.split('.').collect::<Vec<_>>();
    if parts.len() <= 2 {
        return host.to_string();
    }
    let second_level = parts[parts.len() - 2];
    let last = parts[parts.len() - 1];
    if last.len() == 2
        && matches!(second_level, "com" | "net" | "org" | "gov" | "edu" | "co")
        && parts.len() >= 3
    {
        parts[parts.len() - 3..].join(".")
    } else {
        parts[parts.len() - 2..].join(".")
    }
}
