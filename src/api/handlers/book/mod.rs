//! 书籍相关 HTTP 端点：书架、目录、正文、搜索/发现、换源与本地书上传。
//!
//! 按功能拆分为同名目录下的子模块；本文件只保留跨子模块共享的
//! 书籍/书源解析层（`resolve_book` / `resolve_source`）与统一再导出。

use crate::api::AppState;
use crate::error::error::AppError;
use crate::model::{book::Book, book_source::BookSource};
use crate::service::local_epub_book::{is_local_epub_origin, is_local_epub_url, LOCAL_EPUB_ORIGIN};
use crate::service::local_txt_book::{is_local_txt_origin, is_local_txt_url, LOCAL_TXT_ORIGIN};

mod available_source;
mod content;
mod explore;
mod local_upload;
mod search;
mod shelf;
mod sse;
mod toc;

pub use available_source::*;
pub use content::*;
pub use explore::*;
pub use local_upload::*;
pub use search::*;
pub use shelf::*;
pub use sse::*;
pub use toc::*;

/// 书源级接口（搜索/发现/调试）的入口：bookSourceId 直查。
pub(crate) async fn resolve_source(
    state: &AppState,
    user_ns: &str,
    book_source_id: Option<i64>,
) -> Result<BookSource, AppError> {
    let id =
        book_source_id.ok_or_else(|| AppError::BadRequest("bookSourceId required".to_string()))?;
    state
        .book_source_service
        .get_by_id(user_ns, id)
        .await?
        .ok_or_else(|| AppError::NotFound("bookSource not found".to_string()))
}

/// 一本书的完整解析结果：登记表行 + 绑定书源。
pub(crate) struct ResolvedBook {
    pub book_url: String,
    pub toc_url: Option<String>,
    pub source: BookSource,
}

/// 书籍级接口的统一入口：bookId → 登记表 → 书源。
///
/// 本地书（source_id 为 NULL）按 book_url 前缀构造伪书源；绑定的书源被删时
/// 报「书源不存在」（与旧版按 URL 找不到书源同语义）。
pub(crate) async fn resolve_book(
    state: &AppState,
    user_ns: &str,
    book_id: &str,
) -> Result<ResolvedBook, AppError> {
    let row = state
        .book_service
        .book_repo()
        .get(user_ns, book_id)
        .await?
        .ok_or_else(|| AppError::NotFound("书籍不存在".to_string()))?;
    let source = if is_local_txt_url(&row.book_url) {
        BookSource {
            book_source_name: "本地 TXT".to_string(),
            book_source_url: LOCAL_TXT_ORIGIN.to_string(),
            ..BookSource::default()
        }
    } else if is_local_epub_url(&row.book_url) {
        BookSource {
            book_source_name: "本地 EPUB".to_string(),
            book_source_url: LOCAL_EPUB_ORIGIN.to_string(),
            ..BookSource::default()
        }
    } else {
        let source_id = row
            .book_source_id
            .ok_or_else(|| AppError::NotFound("书源不存在".to_string()))?;
        state
            .book_source_service
            .get_by_id(user_ns, source_id)
            .await?
            .ok_or_else(|| AppError::NotFound("书源不存在".to_string()))?
    };
    Ok(ResolvedBook {
        book_url: row.book_url.clone(),
        toc_url: row.toc_url.clone(),
        source,
    })
}

fn is_local_book(book: &Book) -> bool {
    is_local_txt_origin(&book.origin)
        || is_local_txt_url(&book.book_url)
        || is_local_epub_origin(&book.origin)
        || is_local_epub_url(&book.book_url)
}

fn local_book_limit_exceeded(books: &[Book], incoming_book_url: &str, limit: u32) -> bool {
    if limit == 0 {
        return false;
    }
    if books
        .iter()
        .any(|book| is_local_book(book) && book.book_url == incoming_book_url)
    {
        return false;
    }
    books.iter().filter(|book| is_local_book(book)).count() >= limit as usize
}

#[cfg(test)]
mod tests {
    use super::local_book_limit_exceeded;
    use crate::model::book::Book;

    #[test]
    fn local_book_limit_counts_txt_and_epub_but_allows_existing_book() {
        let books = vec![
            Book {
                origin: "local-txt".to_string(),
                book_url: "local-txt:abc".to_string(),
                ..Book::default()
            },
            Book {
                origin: "local-epub".to_string(),
                book_url: "local-epub:def".to_string(),
                ..Book::default()
            },
            Book {
                origin: "https://source.test".to_string(),
                book_url: "https://source.test/book/1".to_string(),
                ..Book::default()
            },
        ];

        assert!(!local_book_limit_exceeded(&books, "local-epub:new", 0));
        assert!(local_book_limit_exceeded(&books, "local-epub:new", 2));
        assert!(!local_book_limit_exceeded(&books, "local-epub:def", 2));
        assert!(!local_book_limit_exceeded(&books, "local-epub:new", 3));
    }
}
