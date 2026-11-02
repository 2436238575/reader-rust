mod common;
use axum::{extract::State, http::HeaderMap, response::Html, routing::get, Router};
use reader_rust::crawler::http_client::HttpClient;
use reader_rust::model::book_source::BookSource;
use reader_rust::model::rule::{ContentRule, SearchRule, TocRule};
use reader_rust::parser::rule_engine::RuleEngine;
use reader_rust::service::book_service::BookService;
use reader_rust::service::chapter_content_service::ChapterContentService;
use serde_json::json;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uuid::Uuid;

/// 测试上游监听 127.0.0.1：显式放行私网出站（url_guard 默认拦截私网）。
fn allow_private_network() {
    reader_rust::crawler::url_guard::set_allow_private_network(true);
}

/// 装配一套带书籍登记表的 BookService（独立临时目录 + 临时 SQLite）。
/// 返回池句柄：收尾必须先 `pool.close().await` 再删目录（Windows 文件锁）。
async fn test_book_service(storage_dir: &std::path::Path) -> (BookService, sqlx::SqlitePool) {
    // init_pool 不会建父目录，先落目录再开库
    std::fs::create_dir_all(storage_dir).unwrap();
    let pool = reader_rust::storage::db::init_pool(&format!(
        "sqlite:{}?mode=rwc",
        storage_dir.join("reader.db").display()
    ))
    .await
    .unwrap();
    let service = BookService::new(
        HttpClient::new(5, None).unwrap(),
        RuleEngine::new().unwrap(),
        Arc::new(ChapterContentService::from_pool(pool.clone(), 0)),
        storage_dir.to_str().unwrap(),
        reader_rust::storage::db::repo::BookRepo::new(pool.clone()),
        reader_rust::storage::db::repo::ShelfBookRepo::new(pool.clone()),
    );
    (service, pool)
}

/// 读目录缓存的旁路 meta（service 的 meta 路径是私有方法，测试里按同一布局拼）。
fn read_toc_meta(storage_dir: &std::path::Path, ns: &str, book_id: &str) -> serde_json::Value {
    let path = storage_dir
        .join("cache")
        .join(ns)
        .join("toc")
        .join(format!("{}.meta.json", book_id));
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

async fn search_page(headers: HeaderMap) -> Html<&'static str> {
    if headers
        .get("x-search-token")
        .and_then(|value| value.to_str().ok())
        == Some("ok")
    {
        Html(
            r#"<div class="item"><a class="title" href="/book/1">Raw</a><span class="author">Tester</span></div>"#,
        )
    } else {
        Html("")
    }
}

#[tokio::test]
async fn search_pipeline_uses_url_analyzer_final_url_and_login_check_js() {
    allow_private_network();
    let app = Router::new().route("/search", get(search_page));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    common::clean_stale_temp_dirs();
    let storage_dir =
        std::env::temp_dir().join(format!("reader-rust-search-compat-{}", Uuid::new_v4()));
    let (service, pool) = test_book_service(&storage_dir).await;
    let source = BookSource {
        book_source_name: "Runtime".to_string(),
        book_source_url: format!("http://{}", addr),
        search_url: Some(
            "/search?q={{key}}&page=<1,2>,{\"headers\":{\"X-Search-Token\":\"ok\"}}"
                .to_string(),
        ),
        login_check_js: Some(
            r#"JSON.stringify({body: result.body.replace("Raw", "Checked"), url: result.url, code: result.code, headers: result.headers, isSuccessful: result.isSuccessful})"#.to_string(),
        ),
        rule_search: Some(SearchRule {
            book_list: Some(".item".to_string()),
            name: Some(".title@text".to_string()),
            author: Some(".author@text".to_string()),
            book_url: Some(".title@href".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };

    let books = service
        .search_book("default", &source, "斗破", 2)
        .await
        .unwrap();

    server.abort();
    pool.close().await;
    common::remove_dir_retried(&storage_dir);

    assert_eq!(books.len(), 1);
    assert_eq!(books[0].name, "Checked");
    assert_eq!(books[0].book_url, format!("http://{}/book/1", addr));
}

async fn content_page_one() -> Html<&'static str> {
    Html(
        r#"<html><body><div id="content">第一页正文</div><a class="next" href="/chapters/1-2.html">下一页</a></body></html>"#,
    )
}

async fn content_page_two() -> Html<&'static str> {
    Html(
        r#"<html><body><div id="content">第二页正文</div><a class="next" href="/chapters/2.html">下一章</a></body></html>"#,
    )
}

async fn next_chapter_page(State(hits): State<Arc<AtomicUsize>>) -> Html<&'static str> {
    hits.fetch_add(1, Ordering::SeqCst);
    Html(r#"<html><body><div id="content">第二章正文</div></body></html>"#)
}

#[tokio::test]
async fn content_pagination_stops_before_next_chapter_url() {
    allow_private_network();
    let next_chapter_hits = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/chapters/1.html", get(content_page_one))
        .route("/chapters/1-2.html", get(content_page_two))
        .route("/chapters/2.html", get(next_chapter_page))
        .with_state(next_chapter_hits.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    common::clean_stale_temp_dirs();
    let storage_dir =
        std::env::temp_dir().join(format!("reader-rust-content-test-{}", Uuid::new_v4()));
    let (service, pool) = test_book_service(&storage_dir).await;
    let source = BookSource {
        book_source_name: "Content pagination".to_string(),
        book_source_url: format!("http://{}", addr),
        rule_content: Some(ContentRule {
            content: Some("#content@text".to_string()),
            next_content_url: Some("a.next@href".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };

    let content = service
        .get_content(
            "default",
            "book_1",
            &source,
            &format!("http://{}/chapters/1.html", addr),
        )
        .await
        .unwrap();

    server.abort();
    pool.close().await;
    common::remove_dir_retried(&storage_dir);

    assert!(content.contains("第一页正文"));
    assert!(content.contains("第二页正文"));
    assert!(!content.contains("第二章正文"));
    assert_eq!(next_chapter_hits.load(Ordering::SeqCst), 0);
}

async fn toc_endless_page(State(hits): State<Arc<AtomicUsize>>) -> Html<String> {
    let n = hits.fetch_add(1, Ordering::SeqCst);
    // 每页都给一个从未见过的 next 链接：环检测挡不住，只能靠页数上限停
    Html(format!(
        r#"<html><body><div class="toc"><a href="/ch/{n}.html">第{n}章</a></div><a class="next" href="/toc/{}">下一页</a></body></html>"#,
        n + 1
    ))
}

#[tokio::test]
async fn toc_pagination_stops_at_page_cap_for_endless_next_links() {
    allow_private_network();
    let hits = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/toc/{n}", get(toc_endless_page))
        .with_state(hits.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    common::clean_stale_temp_dirs();
    let storage_dir = std::env::temp_dir().join(format!("reader-rust-toc-cap-{}", Uuid::new_v4()));
    let (service, pool) = test_book_service(&storage_dir).await;
    let source = BookSource {
        book_source_name: "Endless TOC".to_string(),
        book_source_url: format!("http://{}", addr),
        rule_toc: Some(TocRule {
            chapter_list: Some(".toc".to_string()),
            chapter_name: Some("a@text".to_string()),
            chapter_url: Some("a@href".to_string()),
            next_toc_url: Some("a.next@href".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };

    let chapters = service
        .get_chapter_list(
            "default",
            "book_1",
            &source,
            &format!("http://{addr}/toc/0"),
        )
        .await
        .unwrap();

    server.abort();
    pool.close().await;

    // 首页 + MAX_TOC_PAGES（500）页后停止：请求数与章节数都钉在上限上，
    // 不随「每页都有新 next」无限增长
    assert_eq!(hits.load(Ordering::SeqCst), 501);
    assert_eq!(chapters.len(), 501);
    assert_eq!(chapters[0].title, "第0章");
    // 截断目录按未完成落盘：读侧只给 60s 短 TTL，60 秒后重抓即可补全，
    // 而不是把残缺目录当完整目录缓存一小时
    assert_eq!(
        read_toc_meta(&storage_dir, "default", "book_1")["complete"],
        serde_json::Value::Bool(false),
        "达到 MAX_TOC_PAGES 截断应标未完成"
    );
    common::remove_dir_retried(&storage_dir);
}

async fn toc_first_page() -> Html<&'static str> {
    Html(
        r#"<html><body><div class="toc"><a href="/ch/1.html">第一章</a></div><a class="next" href="/toc/last">下一页</a></body></html>"#,
    )
}

async fn toc_last_page() -> Html<&'static str> {
    Html(r#"<html><body><div class="toc"><a href="/ch/2.html">第二章</a></div></body></html>"#)
}

#[tokio::test]
async fn toc_pagination_normal_two_pages_unaffected_by_cap() {
    allow_private_network();
    let app = Router::new()
        .route("/toc/1", get(toc_first_page))
        .route("/toc/last", get(toc_last_page));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    common::clean_stale_temp_dirs();
    let storage_dir = std::env::temp_dir().join(format!("reader-rust-toc-two-{}", Uuid::new_v4()));
    let (service, pool) = test_book_service(&storage_dir).await;
    let source = BookSource {
        book_source_name: "Two page TOC".to_string(),
        book_source_url: format!("http://{}", addr),
        rule_toc: Some(TocRule {
            chapter_list: Some(".toc".to_string()),
            chapter_name: Some("a@text".to_string()),
            chapter_url: Some("a@href".to_string()),
            next_toc_url: Some("a.next@href".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };

    let chapters = service
        .get_chapter_list(
            "default",
            "book_1",
            &source,
            &format!("http://{addr}/toc/1"),
        )
        .await
        .unwrap();

    server.abort();
    pool.close().await;

    assert_eq!(chapters.len(), 2);
    assert_eq!(chapters[0].title, "第一章");
    assert_eq!(chapters[1].title, "第二章");
    assert!(chapters[1].url.ends_with("/ch/2.html"));
    // 单链自然结束：目录完整，缓存按完成落盘（正常路径不受截断改动影响）
    assert_eq!(
        read_toc_meta(&storage_dir, "default", "book_1")["complete"],
        serde_json::Value::Bool(true),
        "正常抓完应保持完整标志"
    );
    common::remove_dir_retried(&storage_dir);
}

async fn toc_dup_first_page() -> Html<&'static str> {
    Html(
        r#"<html><body><div class="toc"><a href="/ch/1.html">第一章</a></div><a class="next" href="/toc/dup">下一页</a></body></html>"#,
    )
}

/// 与首页完全相同的章节列表：整页重复熔断应停（书源可能把多个分页都解析到了同一页）。
async fn toc_dup_page() -> Html<&'static str> {
    Html(
        r#"<html><body><div class="toc"><a href="/ch/1.html">第一章</a></div><a class="next" href="/toc/dup2">下一页</a></body></html>"#,
    )
}

#[tokio::test]
async fn toc_duplicate_page_marks_cache_incomplete() {
    allow_private_network();
    let app = Router::new()
        .route("/toc/1", get(toc_dup_first_page))
        .route("/toc/dup", get(toc_dup_page));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    common::clean_stale_temp_dirs();
    let storage_dir = std::env::temp_dir().join(format!("reader-rust-toc-dup-{}", Uuid::new_v4()));
    let (service, pool) = test_book_service(&storage_dir).await;
    let source = BookSource {
        book_source_name: "Duplicate TOC".to_string(),
        book_source_url: format!("http://{}", addr),
        rule_toc: Some(TocRule {
            chapter_list: Some(".toc".to_string()),
            chapter_name: Some("a@text".to_string()),
            chapter_url: Some("a@href".to_string()),
            next_toc_url: Some("a.next@href".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };

    let chapters = service
        .get_chapter_list(
            "default",
            "book_1",
            &source,
            &format!("http://{addr}/toc/1"),
        )
        .await
        .unwrap();

    server.abort();
    pool.close().await;

    // 整页重复熔断：第二页与首页完全重复，只有首页章节
    assert_eq!(chapters.len(), 1);
    assert_eq!(chapters[0].title, "第一章");
    // 熔断视为截断：缓存按未完成落盘，60 秒后可重试
    assert_eq!(
        read_toc_meta(&storage_dir, "default", "book_1")["complete"],
        serde_json::Value::Bool(false),
        "整页重复熔断应标未完成"
    );
    common::remove_dir_retried(&storage_dir);
}

async fn toc_multi_first_page() -> Html<&'static str> {
    Html(
        r#"<html><body><div class="toc"><a href="/ch/1.html">第一章</a></div><a class="next" href="/toc/a">分支1</a><a class="next" href="/toc/b">分支2</a></body></html>"#,
    )
}

async fn toc_multi_branch_dup() -> Html<&'static str> {
    // 与首页重复的分支页：多分支路径的整页重复熔断（continue，不中断其余分支）
    Html(r#"<html><body><div class="toc"><a href="/ch/1.html">第一章</a></div></body></html>"#)
}

async fn toc_multi_branch_new() -> Html<&'static str> {
    Html(r#"<html><body><div class="toc"><a href="/ch/2.html">第二章</a></div></body></html>"#)
}

#[tokio::test]
async fn toc_multi_branch_duplicate_page_marks_cache_incomplete() {
    allow_private_network();
    let app = Router::new()
        .route("/toc/0", get(toc_multi_first_page))
        .route("/toc/a", get(toc_multi_branch_dup))
        .route("/toc/b", get(toc_multi_branch_new));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    common::clean_stale_temp_dirs();
    let storage_dir =
        std::env::temp_dir().join(format!("reader-rust-toc-multi-{}", Uuid::new_v4()));
    let (service, pool) = test_book_service(&storage_dir).await;
    let source = BookSource {
        book_source_name: "Multi branch TOC".to_string(),
        book_source_url: format!("http://{}", addr),
        rule_toc: Some(TocRule {
            chapter_list: Some(".toc".to_string()),
            chapter_name: Some("a@text".to_string()),
            chapter_url: Some("a@href".to_string()),
            next_toc_url: Some("a.next@href".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };

    let chapters = service
        .get_chapter_list(
            "default",
            "book_1",
            &source,
            &format!("http://{addr}/toc/0"),
        )
        .await
        .unwrap();

    server.abort();
    pool.close().await;

    // 多分支全部处理完（重复分支跳过、新分支照常并入），但重复分支已把
    // complete 置 false：目录可能不齐，按未完成落盘
    assert_eq!(chapters.len(), 2);
    assert_eq!(chapters[0].title, "第一章");
    assert_eq!(chapters[1].title, "第二章");
    assert_eq!(
        read_toc_meta(&storage_dir, "default", "book_1")["complete"],
        serde_json::Value::Bool(false),
        "多分支路径的整页重复也应标未完成"
    );
    common::remove_dir_retried(&storage_dir);
}

#[tokio::test]
async fn explore_kinds_support_text_and_js_rules() {
    common::clean_stale_temp_dirs();
    let storage_dir =
        std::env::temp_dir().join(format!("reader-rust-explore-kinds-{}", Uuid::new_v4()));
    let (service, pool) = test_book_service(&storage_dir).await;
    let text_source = BookSource {
        book_source_name: "Explore".to_string(),
        book_source_url: "https://explore.example".to_string(),
        explore_url: Some("排行::/rank&&完本::/done".to_string()),
        ..Default::default()
    };
    let js_source = BookSource {
        explore_url: Some(
            r#"@js:JSON.stringify([{title:"玄幻",url:"/xuanhuan",style:{layout:"grid"}}])"#
                .to_string(),
        ),
        ..text_source.clone()
    };

    let text = service.explore_kinds("tester", &text_source).await.unwrap();
    let js = service.explore_kinds("tester", &js_source).await.unwrap();

    assert_eq!(text.len(), 2);
    assert_eq!(text[0].title, "排行");
    assert_eq!(text[0].url.as_deref(), Some("/rank"));
    assert_eq!(js.len(), 1);
    assert_eq!(js[0].title, "玄幻");
    assert_eq!(
        js[0].style.as_ref().and_then(|style| style.get("layout")),
        Some(&json!("grid"))
    );
    pool.close().await;
    common::remove_dir_retried(&storage_dir);
}

#[tokio::test]
async fn explore_kinds_accept_relaxed_style_objects() {
    common::clean_stale_temp_dirs();
    let storage_dir = std::env::temp_dir().join(format!(
        "reader-rust-explore-kinds-relaxed-{}",
        Uuid::new_v4()
    ));
    let (service, pool) = test_book_service(&storage_dir).await;
    let source = BookSource {
        book_source_name: "Relaxed explore".to_string(),
        book_source_url: "https://explore.example".to_string(),
        explore_url: Some(
            r#"[{"title":"排行🏷榜单","url":"","style":<"layout_flexBasisPercent":1,"layout_flexGrow":1>},{"title":"总排行榜","url":"/rank/","style":<"layout_flexBasisPercent":0.4,"layout_flexGrow":1>}]"#
                .to_string(),
        ),
        ..Default::default()
    };

    let kinds = service.explore_kinds("tester", &source).await.unwrap();

    assert_eq!(kinds.len(), 2);
    assert_eq!(kinds[0].title, "排行🏷榜单");
    assert_eq!(kinds[0].url.as_deref(), Some(""));
    assert_eq!(kinds[1].title, "总排行榜");
    assert_eq!(kinds[1].url.as_deref(), Some("/rank/"));
    assert_eq!(
        kinds[1]
            .style
            .as_ref()
            .and_then(|style| style.get("layout_flexBasisPercent")),
        Some(&json!(0.4))
    );
    pool.close().await;
    common::remove_dir_retried(&storage_dir);
}

#[tokio::test]
async fn explore_kinds_accept_relaxed_angle_item_objects() {
    common::clean_stale_temp_dirs();
    let storage_dir = std::env::temp_dir().join(format!(
        "reader-rust-explore-kinds-angle-items-{}",
        Uuid::new_v4()
    ));
    let (service, pool) = test_book_service(&storage_dir).await;
    let source = BookSource {
        book_source_name: "Relaxed explore items".to_string(),
        book_source_url: "https://explore.example".to_string(),
        explore_url: Some(
            r#"[<"style":<"layout_flexBasisPercent":1.0,"layout_flexGrow":1>,"title":"书 库","url":"/book/category/catalog.html">,<"style":<"layout_flexBasisPercent":0.25,"layout_flexGrow":1>,"title":"排 行","url":"/book/ranking.html">,<"style":<"layout_flexBasisPercent":0.25,"layout_flexGrow":1>,"title":">","url":"/ranking/hits/2.html">]"#
                .to_string(),
        ),
        ..Default::default()
    };

    let kinds = service.explore_kinds("tester", &source).await.unwrap();

    assert_eq!(kinds.len(), 3);
    assert_eq!(kinds[0].title, "书 库");
    assert_eq!(kinds[0].url.as_deref(), Some("/book/category/catalog.html"));
    assert_eq!(kinds[1].title, "排 行");
    assert_eq!(kinds[1].url.as_deref(), Some("/book/ranking.html"));
    assert_eq!(kinds[2].title, ">");
    assert_eq!(kinds[2].url.as_deref(), Some("/ranking/hits/2.html"));
    assert_eq!(
        kinds[1]
            .style
            .as_ref()
            .and_then(|style| style.get("layout_flexBasisPercent")),
        Some(&json!(0.25))
    );
    pool.close().await;
    common::remove_dir_retried(&storage_dir);
}
