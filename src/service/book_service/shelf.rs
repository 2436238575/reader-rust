//! 书架：单用户书架的读取、upsert、整架替换（含 WebDAV 恢复）与删除。
//!
//! 书架行只存进度/分组等用户态字段，身份与绑定字段（bookUrl/origin/
//! tocUrl/name/author/bookSourceId）在全局登记表 `books` 里，读取时 JOIN
//! 注入（`assemble_shelf_book` / `shelf_row_of` 是两个方向的转换）。
//! 限额（`USER_BOOK_LIMIT` / `USER_LOCAL_BOOK_LIMIT`）与写入收进同一事务，
//! 消除 count 与 upsert 分离的并发超限窗口。

use super::{is_local_book_url, BookService};
use crate::error::error::AppError;
use crate::model::book::Book;
use crate::model::book_source::BookSource;
use crate::storage::db::repo::{RegisterBook, ShelfBookRow, ShelfEntry};
use crate::util::text::{normalize_source_url, repair_encoded_url};

impl BookService {
    pub async fn get_bookshelf(&self, user_ns: &str) -> Result<Vec<Book>, AppError> {
        let entries = self.shelf_repo.list(user_ns).await?;
        Ok(entries.into_iter().map(assemble_shelf_book).collect())
    }

    /// 按 bookId 取书架条目。
    pub async fn get_shelf_book(
        &self,
        user_ns: &str,
        book_id: &str,
    ) -> Result<Option<Book>, AppError> {
        Ok(self
            .shelf_repo
            .get(user_ns, book_id)
            .await?
            .map(assemble_shelf_book))
    }

    /// 就地更新一本书（按 bookId）：单行读-改-写。
    pub async fn update_shelf_book(
        &self,
        user_ns: &str,
        book_id: &str,
        update: impl FnOnce(&mut Book),
    ) -> Result<Option<Book>, AppError> {
        let Some(entry) = self.shelf_repo.get(user_ns, book_id).await? else {
            return Ok(None);
        };
        let mut book = assemble_shelf_book(entry);
        update(&mut book);
        self.shelf_repo
            .upsert(user_ns, shelf_row_of(&book), None)
            .await?;
        Ok(Some(book))
    }

    pub async fn save_book(&self, user_ns: &str, mut book: Book) -> Result<Book, AppError> {
        sanitize_book_urls(&mut book);
        if book.book_url.trim().is_empty() {
            return Err(AppError::BadRequest("bookUrl required".to_string()));
        }
        // 登记/校验 bookId：书架条目与登记表共用同一身份；origin 是数据字段不再校验非空
        let book_id = self
            .register_book(
                user_ns,
                RegisterBook {
                    book_url: &book.book_url,
                    book_source_id: book.book_source_id,
                    name: Some(&book.name),
                    author: Some(&book.author),
                    toc_url: book.toc_url.as_deref(),
                },
            )
            .await?;
        book.book_id = Some(book_id.clone());

        // 既有条目：进度/分组等字段缺省时继承（saveBook 语义是 upsert 更新）
        if let Some(existing) = self.shelf_repo.get(user_ns, &book_id).await? {
            let exist = assemble_shelf_book(existing);
            if book.dur_chapter_index.is_none() {
                book.dur_chapter_index = exist.dur_chapter_index;
            }
            if book.dur_chapter_title.is_none() {
                book.dur_chapter_title = exist.dur_chapter_title;
            }
            if book.dur_chapter_time.is_none() {
                book.dur_chapter_time = exist.dur_chapter_time;
            }
            if book.dur_chapter_pos.is_none() {
                book.dur_chapter_pos = exist.dur_chapter_pos;
            }
            if book.total_chapter_num.is_none() {
                book.total_chapter_num = exist.total_chapter_num;
            }
            if book.last_check_time.is_none() {
                book.last_check_time = exist.last_check_time;
            }
            if book.group.is_none() {
                book.group = exist.group;
            }
        }

        // 存在性检查、限额与写入收进同一事务（消除 count 与 upsert 分离的并发超限窗口）
        self.shelf_repo
            .upsert_with_limit(
                user_ns,
                shelf_row_of(&book),
                i64::from(self.user_book_limit),
                i64::from(self.user_local_book_limit),
                is_local_book_url(&book.book_url),
            )
            .await?;
        Ok(book)
    }

    /// 整架替换（含 WebDAV 恢复）。`restore=true` 时只清空当前用户的书架，
    /// 再按备份自带的 bookId 逐条登记/重建；全局登记表与正文历史不跟着清空，
    /// 书签等按 id 引用的数据仍然对齐。全路径单事务。
    pub async fn save_books(
        &self,
        user_ns: &str,
        books: Vec<Book>,
        restore: bool,
    ) -> Result<Vec<Book>, AppError> {
        // 预校验（不落库）：URL 清洗与限额都在事务外完成
        let mut prepared = Vec::with_capacity(books.len());
        for mut book in books {
            sanitize_book_urls(&mut book);
            if book.book_url.trim().is_empty() {
                return Err(AppError::BadRequest("bookUrl required".to_string()));
            }
            prepared.push(book);
        }
        // save_books 是整架替换（导入场景），按替换后的总量校验上限
        let limit = self.user_book_limit as usize;
        if limit > 0 && prepared.len() > limit {
            return Err(AppError::BadRequest(format!(
                "书架书籍数量不能超过 {limit} 本"
            )));
        }
        // 本地书限额在整架替换后同样成立
        let local_limit = self.user_local_book_limit as usize;
        if local_limit > 0 {
            let local_count = prepared
                .iter()
                .filter(|b| is_local_book_url(&b.book_url))
                .count();
            if local_count > local_limit {
                return Err(AppError::BadRequest(format!(
                    "本地书籍数量不能超过 {local_limit} 本"
                )));
            }
        }

        let mut tx = self.book_repo.begin().await?;
        // 只清书架：books 是全部署共享的登记表，登记行和正文历史不能跟着
        // 一个用户的恢复操作一起删。恢复后重新登记同一 URL 会复用原 book_id。
        if restore {
            self.shelf_repo.wipe_on(&mut tx, user_ns).await?;
        }
        let mut normalized = Vec::with_capacity(prepared.len());
        for mut book in prepared {
            let item = RegisterBook {
                book_url: &book.book_url,
                book_source_id: book.book_source_id,
                name: Some(&book.name),
                author: Some(&book.author),
                toc_url: book.toc_url.as_deref(),
            };
            let desired = if restore {
                book.book_id.as_deref()
            } else {
                None
            };
            let book_id = self
                .book_repo
                .register_on(&mut tx, user_ns, item, desired)
                .await?;
            book.book_id = Some(book_id);
            normalized.push(book);
        }
        let rows = normalized.iter().map(shelf_row_of).collect();
        self.shelf_repo
            .replace_all_on(&mut tx, user_ns, rows)
            .await?;
        tx.commit().await?;
        Ok(normalized)
    }

    /// 按 bookId 从书架删除。登记行与正文历史保留：重新加入同一本书时
    /// book_id 不变，书签、编辑版和历史仍然接得上。
    pub async fn delete_book(&self, user_ns: &str, book_id: &str) -> Result<bool, AppError> {
        let deleted = self.shelf_repo.delete(user_ns, book_id).await?;
        if deleted {
            let _ = self.clear_book_related_cache(user_ns, book_id).await;
        }
        Ok(deleted)
    }

    pub async fn delete_books(
        &self,
        user_ns: &str,
        book_ids: &[String],
    ) -> Result<usize, AppError> {
        let deleted = self.shelf_repo.delete_many(user_ns, book_ids).await?;
        if deleted > 0 {
            for book_id in book_ids {
                // 非法 id 拼进缓存目录路径会越界（`../..` 经 remove_dir_all 落到上级），
                // repo 层对它们是 no-op，这里同样跳过，不把原始输入送进文件系统
                if crate::storage::db::repo::parse_book_id(book_id).is_none() {
                    continue;
                }
                let _ = self.clear_book_related_cache(user_ns, book_id).await;
            }
        }
        Ok(deleted as usize)
    }

    /// 某本书已有正文的章节数：一次 COUNT 查询。
    pub async fn cached_chapter_count(
        &self,
        _user_ns: &str,
        book_id: &str,
    ) -> Result<usize, AppError> {
        Ok(self.content.count_chapters(book_id).await? as usize)
    }

    pub async fn cache_chapter(
        &self,
        user_ns: &str,
        book_id: &str,
        source: &BookSource,
        chapter_url: &str,
        refresh: bool,
    ) -> Result<(), AppError> {
        if refresh {
            self.refresh_chapter_content(user_ns, book_id, source, chapter_url)
                .await?;
        } else {
            self.get_content(user_ns, book_id, source, chapter_url)
                .await?;
        }
        Ok(())
    }
}

fn sanitize_book_urls(book: &mut Book) {
    book.book_url = repair_encoded_url(&book.book_url);
    book.origin = normalize_source_url(&book.origin);
    if let Some(toc_url) = &book.toc_url {
        book.toc_url = Some(repair_encoded_url(toc_url));
    }
    if let Some(cover_url) = &book.cover_url {
        book.cover_url = Some(repair_encoded_url(cover_url));
    }
}

/// 本地书的伪书源标记：登记表里它们的 book_source_id 为 NULL。
fn local_origin_of(book_url: &str) -> (String, Option<String>) {
    if crate::service::local_txt_book::is_local_txt_url(book_url) {
        return (
            crate::service::local_txt_book::LOCAL_TXT_ORIGIN.to_string(),
            Some("本地 TXT".to_string()),
        );
    }
    if crate::service::local_epub_book::is_local_epub_url(book_url) {
        return (
            crate::service::local_epub_book::LOCAL_EPUB_ORIGIN.to_string(),
            Some("本地 EPUB".to_string()),
        );
    }
    (String::new(), None)
}

/// 书架行 + 登记表/书源 JOIN 结果 → 前端 Book 对象。
fn assemble_shelf_book(entry: ShelfEntry) -> Book {
    let (origin, origin_name) = match (entry.origin_url, entry.origin_name) {
        (Some(url), name) => (url, name),
        (None, _) => local_origin_of(&entry.book_url),
    };
    let shelf = entry.shelf;
    Book {
        book_id: Some(shelf.book_id),
        book_source_id: entry.book_source_id,
        name: entry.name,
        author: entry.author,
        book_url: entry.book_url,
        origin,
        origin_name,
        cover_url: shelf.cover_url,
        toc_url: entry.toc_url,
        charset: shelf.charset,
        custom_cover_url: shelf.custom_cover_url,
        can_update: shelf.can_update,
        dur_chapter_index: shelf.dur_chapter_index,
        dur_chapter_pos: shelf.dur_chapter_pos,
        dur_chapter_time: shelf.dur_chapter_time,
        dur_chapter_title: shelf.dur_chapter_title,
        intro: shelf.intro,
        latest_chapter_title: shelf.latest_chapter_title,
        last_check_time: shelf.last_check_time,
        total_chapter_num: shelf.total_chapter_num,
        r#type: shelf.book_type,
        group: shelf.group_bits,
        word_count: shelf.word_count,
        info_html: shelf.info_html,
        toc_html: shelf.toc_html,
        kind: shelf.kind,
        update_time: shelf.update_time,
        can_re_name: shelf.can_re_name,
        download_urls: shelf.download_urls,
    }
}

/// Book → 书架行。身份/绑定字段（bookUrl/origin/tocUrl/name/author/bookSourceId）
/// 不落 shelf_books——它们在 books 登记表里，读取时 JOIN 注入。
fn shelf_row_of(book: &Book) -> ShelfBookRow {
    ShelfBookRow {
        book_id: book.book_id.clone().unwrap_or_default(),
        position: 0, // 由 repo 层在 upsert 时解析（既有行保留原位，新行排末）
        group_bits: book.group,
        dur_chapter_index: book.dur_chapter_index,
        dur_chapter_pos: book.dur_chapter_pos,
        dur_chapter_time: book.dur_chapter_time,
        dur_chapter_title: book.dur_chapter_title.clone(),
        cover_url: book.cover_url.clone(),
        custom_cover_url: book.custom_cover_url.clone(),
        can_update: book.can_update,
        intro: book.intro.clone(),
        latest_chapter_title: book.latest_chapter_title.clone(),
        last_check_time: book.last_check_time,
        total_chapter_num: book.total_chapter_num,
        book_type: book.r#type,
        word_count: book.word_count.clone(),
        info_html: book.info_html.clone(),
        toc_html: book.toc_html.clone(),
        kind: book.kind.clone(),
        update_time: book.update_time.clone(),
        can_re_name: book.can_re_name.clone(),
        download_urls: book.download_urls.clone(),
        charset: book.charset.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::book_service::tests::{make_book, test_service};

    #[tokio::test]
    async fn bookshelf_list_keeps_intro_but_drops_heavy_snapshots() {
        // 窄列拆分（SHELF_LIST_COLS）只该去掉 info_html/toc_html 两个大快照；
        // intro 是详情弹窗直接吃列表对象的字段，丢了书架上的书简介全空
        let storage_dir =
            std::env::temp_dir().join(format!("reader-rust-shelf-intro-{}", std::process::id()));
        let (service, pool) = test_service(&storage_dir).await;

        let mut book = make_book("intro-book");
        book.intro = Some("这是一本测试书的简介。".to_string());
        service.save_book("u", book).await.unwrap();

        let shelf = service.get_bookshelf("u").await.unwrap();
        assert_eq!(shelf.len(), 1);
        assert_eq!(
            shelf[0].intro.as_deref(),
            Some("这是一本测试书的简介。"),
            "书架列表必须带 intro"
        );
        // 大快照字段仍然不进列表（窄列的意义所在）
        assert!(shelf[0].info_html.is_none() || shelf[0].info_html.as_deref() == Some(""));

        pool.close().await;
        let _ = tokio::fs::remove_dir_all(&storage_dir).await;
    }

    #[tokio::test]
    async fn shelf_limit_holds_under_concurrent_saves() {
        let storage_dir =
            std::env::temp_dir().join(format!("reader-rust-limit-race-{}", std::process::id()));
        let (service, pool) = test_service(&storage_dir).await;
        let service = std::sync::Arc::new(service.with_user_book_limit(2));

        // 限额检查与写入在同一事务（upsert_with_limit）：并发 8 本只能进 2 本
        let mut handles = Vec::new();
        for i in 0..8 {
            let svc = service.clone();
            handles.push(tokio::spawn(async move {
                svc.save_book("u", make_book(&format!("race-{i}"))).await
            }));
        }
        let mut ok = 0usize;
        let mut rejected = 0usize;
        for h in handles {
            match h.await.unwrap() {
                Ok(_) => ok += 1,
                Err(_) => rejected += 1,
            }
        }
        assert_eq!(ok, 2, "限额 2 下并发 8 本只能成 2 本");
        assert_eq!(rejected, 6);
        assert_eq!(service.get_bookshelf("u").await.unwrap().len(), 2);

        pool.close().await;
        let _ = tokio::fs::remove_dir_all(&storage_dir).await;
    }

    #[tokio::test]
    async fn user_book_limit_blocks_new_books_but_not_updates() {
        let storage_dir =
            std::env::temp_dir().join(format!("reader-rust-book-limit-{}", std::process::id()));
        let (service, pool) = test_service(&storage_dir).await;
        let service = service.with_user_book_limit(2);

        service.save_book("u", make_book("a")).await.unwrap();
        service.save_book("u", make_book("b")).await.unwrap();
        // 第三本新书被拒
        let err = service.save_book("u", make_book("c")).await.unwrap_err();
        assert!(err.to_string().contains("书架书籍数量不能超过"));
        // 已有书的更新（如阅读进度）不受限
        let mut existing = make_book("a");
        existing.dur_chapter_index = Some(3);
        service.save_book("u", existing).await.unwrap();
        assert_eq!(service.get_bookshelf("u").await.unwrap().len(), 2);

        // 整架替换（saveBooks）按替换后总量校验
        service
            .save_books("u", vec![make_book("x"), make_book("y")], false)
            .await
            .unwrap();
        let err = service
            .save_books(
                "u",
                vec![make_book("x"), make_book("y"), make_book("z")],
                false,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("书架书籍数量不能超过"));

        // 0 = 不限
        let (unlimited, pool2) = test_service(&storage_dir).await;
        for i in 0..5 {
            unlimited
                .save_book("u", make_book(&format!("b{i}")))
                .await
                .unwrap();
        }

        pool.close().await;
        pool2.close().await;
        let _ = tokio::fs::remove_dir_all(&storage_dir).await;
    }

    #[tokio::test]
    async fn user_local_book_limit_blocks_local_books_saved_directly() {
        let storage_dir =
            std::env::temp_dir().join(format!("reader-rust-local-limit-{}", std::process::id()));
        let (service, pool) = test_service(&storage_dir).await;
        let service = service.with_user_local_book_limit(1);

        // 直构 local-txt 书入架（不经上传 handler）同样受本地书限额约束
        service
            .save_book("u", make_book("local-txt:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"))
            .await
            .unwrap();
        let err = service
            .save_book("u", make_book("local-txt:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("本地书籍数量不能超过"));
        // 已在架上的同一本书更新不受限
        service
            .save_book("u", make_book("local-txt:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"))
            .await
            .unwrap();
        // 普通书不受本地书限额影响
        service
            .save_book("u", make_book("https://example.com/book"))
            .await
            .unwrap();

        // 整架替换按替换后的本地书总量校验
        let err = service
            .save_books(
                "u",
                vec![
                    make_book("local-txt:cccccccccccccccccccccccccccccccc"),
                    make_book("local-txt:dddddddddddddddddddddddddddddddd"),
                ],
                false,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("本地书籍数量不能超过"));

        pool.close().await;
        let _ = tokio::fs::remove_dir_all(&storage_dir).await;
    }

    #[tokio::test]
    async fn restore_aligns_backup_book_ids() {
        let storage_dir =
            std::env::temp_dir().join(format!("reader-rust-restore-align-{}", std::process::id()));
        let (service, pool) = test_service(&storage_dir).await;

        let mut a = make_book("https://example.com/a");
        a.book_id = Some("book_7".to_string());
        let mut b = make_book("https://example.com/b");
        b.book_id = Some("book_3".to_string());
        let saved = service.save_books("u", vec![a, b], true).await.unwrap();

        // 空表重建无冲突，备份自带 id 原位对齐
        let ids: Vec<&str> = saved.iter().filter_map(|b| b.book_id.as_deref()).collect();
        assert_eq!(ids, ["book_7", "book_3"]);
        // 后续登记自动分配不撞已占用的 id
        let c = service
            .register_book(
                "u",
                RegisterBook {
                    book_url: "https://example.com/c",
                    book_source_id: None,
                    name: None,
                    author: None,
                    toc_url: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(c, "book_8");

        pool.close().await;
        let _ = tokio::fs::remove_dir_all(&storage_dir).await;
    }

    #[tokio::test]
    async fn restore_rolls_back_atomically_when_book_id_invalid() {
        let storage_dir =
            std::env::temp_dir().join(format!("reader-rust-restore-tx-{}", std::process::id()));
        let (service, pool) = test_service(&storage_dir).await;

        // 先在架两本，作为「恢复前状态」
        service
            .save_books(
                "u",
                vec![
                    make_book("https://example.com/a"),
                    make_book("https://example.com/b"),
                ],
                false,
            )
            .await
            .unwrap();

        // 恢复备份：第一本合法，第二本 bookId 非法 → 事务整体回滚
        let good = make_book("https://example.com/c");
        let mut bad = make_book("https://example.com/d");
        bad.book_id = Some("../evil".to_string());
        let err = service
            .save_books("u", vec![good, bad], true)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("bookId 格式非法"));

        // 书架与登记表都还是恢复前的样子（a/b 在架，c 未写入）
        let shelf = service.get_bookshelf("u").await.unwrap();
        let urls: Vec<&str> = shelf.iter().map(|b| b.book_url.as_str()).collect();
        assert_eq!(urls, ["https://example.com/a", "https://example.com/b"]);
        assert!(service
            .book_repo()
            .get_by_url("u", "https://example.com/c")
            .await
            .unwrap()
            .is_none());

        pool.close().await;
        let _ = tokio::fs::remove_dir_all(&storage_dir).await;
    }
}
