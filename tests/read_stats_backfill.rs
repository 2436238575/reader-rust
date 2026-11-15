//! 阅读统计启动回填：旧 `readChapterHistory` + `reading-stats.totalSeconds`
//! 落到新明细表；URL 反查 index、找不到丢弃、回填行排除今日/连续天数，
//! 且重复启动不回填第二次。

use reader_rust::service::json_document_service::JsonDocumentService;
use reader_rust::service::read_stats_service::{ReadEventInput, ReadStatsService};
use reader_rust::storage::db;
use reader_rust::storage::db::chapters_repo::ChaptersRepo;
use reader_rust::storage::db::read_stats_repo::ReadStatsRepo;
use serde_json::json;
use std::path::PathBuf;

mod common;

async fn setup() -> (sqlx::SqlitePool, ReadStatsService, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "reader-rust-read-backfill-{}",
        reader_rust::util::crypto::random_string(8)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let database_url = format!("sqlite:{}?mode=rwc", dir.join("reader.db").display());
    let pool = db::init_pool(&database_url).await.unwrap();
    let now = 1_700_000_000i64;

    sqlx::query(
        "INSERT INTO users (uid, username, password_hash, token_version, created_at, last_login_at) \
         VALUES (1, 'reader1', 'unused', 0, ?1, 0)",
    )
    .bind(now)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO books (book_id, book_url, name, author, created_at, updated_at) \
         VALUES (1, 'https://up.example/book/1', '书', '作者', ?1, ?1)",
    )
    .bind(now)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO books (book_id, book_url, name, author, created_at, updated_at) \
         VALUES (2, 'local-txt:local-book', '本地书', '作者', ?1, ?1)",
    )
    .bind(now)
    .execute(&pool)
    .await
    .unwrap();
    // 目录被截断的网络书：chapters 只到 50，书架总数 100，complete=0
    sqlx::query(
        "INSERT INTO books (book_id, book_url, name, author, created_at, updated_at) \
         VALUES (3, 'https://up.example/book/3', '截断目录书', '作者', ?1, ?1)",
    )
    .bind(now)
    .execute(&pool)
    .await
    .unwrap();
    for (index, url) in [
        (0, "https://up.example/ch/0"),
        (1, "https://up.example/ch/1"),
    ] {
        sqlx::query(
            "INSERT INTO chapters (book_id, chapter_index, url, title) VALUES (1, ?1, ?2, ?3)",
        )
        .bind(index)
        .bind(url)
        .bind(format!("第{index}章"))
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO chapters (book_id, chapter_index, url, title) VALUES (3, 50, 'https://up.example/ch/50', '第50章')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO book_toc_state (book_id, complete, fetched_at) VALUES (1, 1, ?1)")
        .bind(now)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO book_toc_state (book_id, complete, fetched_at) VALUES (3, 0, ?1)")
        .bind(now)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO shelf_books (user_ns, book_id, position, total_chapter_num, created_at, updated_at) \
         VALUES ('user_1', 1, 0, NULL, ?1, ?1), ('user_1', 2, 1, 2, ?1, ?1), ('user_1', 3, 2, 100, ?1, ?1)",
    )
    .bind(now)
    .execute(&pool)
    .await
    .unwrap();

    let history = json!({
        "book_1": [
            "book_1::https://up.example/ch/0",
            "book_1::index:1",
            "book_1::index:99",          // 不存在的 index：保留（index 形式直接信任）
            "book_1::https://up.example/ch/missing" // URL 查不到：丢弃
        ],
        // 本地书没有 chapters 行，URL 末尾 #index 直接解析
        "book_2": ["book_2::local-txt:local-book#1"]
    });
    sqlx::query(
        "INSERT INTO json_documents (user_ns, name, json, updated_at) VALUES \
         ('user_1', 'readChapterHistory', ?1, ?3), \
         ('user_1', 'reading-stats', ?2, ?3)",
    )
    .bind(serde_json::to_string(&history).unwrap())
    .bind(r#"{"totalSeconds":1234,"openedBooks":["x"],"readChapters":[],"completedBooks":[]}"#)
    .bind(now)
    .execute(&pool)
    .await
    .unwrap();

    let service = ReadStatsService::new(
        ReadStatsRepo::new(pool.clone()),
        JsonDocumentService::new(pool.clone()),
        ChaptersRepo::new(pool.clone()),
    );
    (pool, service, dir)
}

#[tokio::test]
async fn backfill_is_idempotent_and_excludes_backfilled_from_today_and_streak() {
    let (pool, service, dir) = setup().await;
    service.backfill_all().await.unwrap();

    let stats = service.stats("user_1").await.unwrap();
    // legacy_total_seconds 计入总时长；回填行秒数 0，不计今日/连续天数
    assert_eq!(stats.total_seconds, 1234);
    assert_eq!(stats.today_seconds, 0);
    // URL + index 两种网络书形式有效；index:99 是 index 形式（保留），
    // 本地书 local-txt:...#1 从 # 后解析；URL 缺失丢弃
    assert_eq!(stats.read_chapters, 4);
    assert_eq!(stats.finished_books, 2, "网络书与本地书的最后一章都已回填");
    assert_eq!(stats.streak_days, 0, "回填日不能制造连续天数");

    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM read_chapter_days WHERE user_ns='user_1' AND backfilled=1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 4);

    // 第二次启动/调用：已有数据或 backfilled_at，应跳过且不重复写
    service.backfill_all().await.unwrap();
    let count_again: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM read_chapter_days WHERE user_ns='user_1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count_again, 4);
    assert_eq!(service.stats("user_1").await.unwrap().total_seconds, 1234);

    // 真实上报同一章：upsert 把该行 backfilled 清 0，今日/连续天数开始生效
    service
        .record_batch(
            "user_1",
            &[ReadEventInput {
                book_id: "book_1".to_string(),
                chapter_index: 0,
                seconds: 30,
            }],
            None,
        )
        .await
        .unwrap();
    let stats = service.stats("user_1").await.unwrap();
    assert_eq!(stats.total_seconds, 1264);
    assert_eq!(stats.today_seconds, 30);
    assert_eq!(stats.streak_days, 1);
    assert_eq!(stats.read_chapters, 4);

    // 目录截断（complete=0, total=100）的书只读到最后注册章 index=50：
    // 不能按 chapters 的最大 index 误判「全书读完」
    service
        .record_batch(
            "user_1",
            &[ReadEventInput {
                book_id: "book_3".to_string(),
                chapter_index: 50,
                seconds: 5,
            }],
            None,
        )
        .await
        .unwrap();
    let stats = service.stats("user_1").await.unwrap();
    assert_eq!(stats.read_chapters, 5);
    assert_eq!(
        stats.finished_books, 2,
        "截断目录的最后一章 index=50 不是 total-1=99，不能算读完"
    );

    // 最后一个真实事件已推进书架时间
    let dur: Option<i64> =
        sqlx::query_scalar("SELECT dur_chapter_time FROM shelf_books WHERE user_ns='user_1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(dur.unwrap_or_default() > 0);

    pool.close().await;
    common::remove_dir_retried(&dir);
}
