//! 书籍/书源整部署共享与 id 不复用的回归测试。

mod common;
use reader_rust::model::book_source::BookSource;
use reader_rust::storage::db;
use reader_rust::storage::db::repo::{BookRepo, BookSourceRepo, RegisterBook};

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
