//! 书籍/书源整部署共享与 id 不复用的回归测试。

mod common;
use reader_rust::model::book_source::BookSource;
use reader_rust::storage::db;
use reader_rust::storage::db::repo::{
    BookRepo, BookSourceRepo, RegisterBook, ShelfBookRepo, ShelfBookRow,
};

async fn test_pool(name: &str) -> (sqlx::SqlitePool, std::path::PathBuf) {
    common::clean_stale_temp_dirs();
    let dir = std::env::temp_dir().join(format!(
        "reader-rust-global-id-{}-{}",
        name,
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let pool = db::init_pool(&format!(
        "sqlite:{}?mode=rwc",
        dir.join("reader.db").display()
    ))
    .await
    .unwrap();
    (pool, dir)
}

fn item(url: &str) -> RegisterBook<'_> {
    RegisterBook {
        book_url: url,
        book_source_id: None,
        name: None,
        author: None,
        toc_url: None,
    }
}

#[tokio::test]
async fn books_are_shared_and_ids_are_never_reused() {
    let (pool, dir) = test_pool("books").await;
    let repo = BookRepo::new(pool.clone());

    let first = repo
        .register("user_1", item("https://example.test/b"), None)
        .await
        .unwrap();
    let second = repo
        .register("user_2", item("https://example.test/b"), None)
        .await
        .unwrap();
    assert_eq!(first, second, "同一 URL 在整部署内只有一个 bookId");

    // 删掉登记行后新书仍拿到更大的 id，不会复用旧 id
    let mut tx = repo.begin().await.unwrap();
    repo.delete_on(&mut tx, &first).await.unwrap();
    tx.commit().await.unwrap();
    let next = repo
        .register("user_1", item("https://example.test/c"), None)
        .await
        .unwrap();
    assert_ne!(next, first, "book_id 不得复用");

    pool.close().await;
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn non_utf8_source_columns_do_not_panic_reads() {
    let (pool, dir) = test_pool("sources-lossy").await;
    let source_repo = BookSourceRepo::new(pool.clone());
    let book_repo = BookRepo::new(pool.clone());
    let shelf_repo = ShelfBookRepo::new(pool.clone());

    let source = BookSource {
        book_source_name: "番茄Web".to_string(),
        book_source_url: "https://source.test/fqweb".to_string(),
        ..Default::default()
    };
    let source_id = source_repo.upsert("user_1", &source, "{}").await.unwrap();
    let book_id = book_repo
        .register(
            "user_1",
            RegisterBook {
                book_url: "https://source.test/book/1",
                book_source_id: Some(source_id),
                name: Some("测试书"),
                author: Some("作者"),
                toc_url: None,
            },
            None,
        )
        .await
        .unwrap();
    shelf_repo
        .upsert(
            "user_1",
            ShelfBookRow {
                book_id: book_id.clone(),
                ..Default::default()
            },
            Some(1),
        )
        .await
        .unwrap();

    // 模拟外部 sqlite3 在 GBK 控制台下写出的值：TEXT 列里是 GBK 的“番茄”，
    // 不是合法 UTF-8（服务端只 bind Rust `&str`，写不出这种字节）。
    let gbk_name = vec![0xb7u8, 0xac, 0xc7, 0xd1];
    let gbk_json = b"{\"bookSourceName\":\"\xb7\xac\xc7\xd1Web\"}";
    sqlx::query(
        "UPDATE book_sources SET book_source_name = CAST(?1 AS TEXT), json = CAST(?2 AS TEXT) \
         WHERE book_source_id = ?3",
    )
    .bind(gbk_name.as_slice())
    .bind(gbk_json.as_slice())
    .bind(source_id)
    .execute(&pool)
    .await
    .unwrap();

    let (_, json) = source_repo
        .get_by_id("user_1", source_id)
        .await
        .unwrap()
        .expect("坏编码不应让详情查询 panic");
    assert!(json.contains('\u{FFFD}'), "非法字节应被替换而不是 panic");

    let listed = source_repo.list("user_1").await.unwrap();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].1.contains('\u{FFFD}'));

    let shelf = shelf_repo.list("user_1").await.unwrap();
    assert_eq!(shelf.len(), 1);
    let origin_name = shelf[0]
        .origin_name
        .as_deref()
        .expect("LEFT JOIN 应带出书源名");
    assert!(origin_name.contains('\u{FFFD}'));

    pool.close().await;
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn book_sources_are_shared_and_ids_are_never_reused() {
    let (pool, dir) = test_pool("sources").await;
    let repo = BookSourceRepo::new(pool.clone());

    let source = BookSource {
        book_source_name: "共享源".to_string(),
        book_source_url: "https://source.test".to_string(),
        ..Default::default()
    };
    let id = repo.upsert("user_1", &source, "{}").await.unwrap();
    // 另一个用户能看到同一个书源
    let json = repo
        .get_by_id("user_2", id)
        .await
        .unwrap()
        .expect("书源应整部署共享");
    assert_eq!(json.0, "https://source.test");
    assert_eq!(repo.list("user_2").await.unwrap().len(), 1);

    // 删掉后重新导入同 URL，id 不复用
    repo.delete_by_id("user_1", id).await.unwrap();
    let next = repo.upsert("user_1", &source, "{}").await.unwrap();
    assert_ne!(next, id, "book_source_id 不得复用");

    pool.close().await;
    let _ = std::fs::remove_dir_all(dir);
}
