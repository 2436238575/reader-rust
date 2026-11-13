//! `/v1/searches` + `/v1/tasks` + `/v1/sse-tickets`：P2 任务子系统端到端。
//!
//! 覆盖 D13 要求的全链路：注册 → 事件 → 续传 → 取消 → 结果分页；
//! 以及契约 §7 的验收项：Last-Event-ID 不重不漏、取消 5 秒内生效且上游
//! 不再发起新请求、第 5 个并发任务 429 + Retry-After、票据一次性。

mod common;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone)]
struct UpstreamState {
    started: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
}

/// 慢请求在飞计数：handler future 被丢弃（客户端 abort/断连）时自动减一，
/// 用「active 归零」证明取消确实关掉了上游连接，而不只是客户端不再读流。
struct ActiveGuard(Arc<AtomicUsize>);

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

async fn start_upstream() -> (String, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let started = Arc::new(AtomicUsize::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let state = UpstreamState {
        started: started.clone(),
        active: active.clone(),
    };
    let app = Router::new()
        .route(
            "/search/a",
            get(|| async {
                Json(json!({
                    "data": { "list": [{
                        "name": "三体",
                        "author": "刘慈欣",
                        "bookUrl": "https://up.example/book/1",
                        "coverUrl": "/cover/1.jpeg",
                        "intro": "地球往事",
                        "kind": "科幻"
                    }] }
                }))
            }),
        )
        .route(
            "/search/b",
            get(|| async {
                Json(json!({
                    "data": { "list": [{
                        "name": "三體（全本）",
                        "author": "劉 慈欣",
                        "bookUrl": "https://up.example/book/1b"
                    }] }
                }))
            }),
        )
        .route(
            "/search/many",
            get(|| async {
                let list: Vec<Value> = (0..60)
                    .map(|index| {
                        json!({
                            "name": format!("书{index:02}"),
                            "author": format!("作者{index:02}"),
                            "bookUrl": format!("https://up.example/many/{index}"),
                        })
                    })
                    .collect();
                Json(json!({ "data": { "list": list } }))
            }),
        )
        .route(
            "/search/prefetch",
            get(|| async {
                Json(json!({
                    "data": { "list": [{
                        "name": "预下载书",
                        "author": "作者",
                        "bookUrl": "/book/pf"
                    }] }
                }))
            }),
        )
        .route(
            "/book/pf",
            get(|| async {
                let list: Vec<Value> = (1..=5)
                    .map(|index| {
                        json!({
                            "name": format!("第{index}章"),
                            "url": format!("/chapter/{index}"),
                        })
                    })
                    .collect();
                Json(json!({ "data": { "list": list } }))
            }),
        )
        .route(
            "/chapter/{n}",
            get(|Path(n): Path<u32>| async move {
                Json(json!({ "content": format!("第{n}章正文。") }))
            }),
        )
        .route(
            "/slow/{n}",
            get(
                |State(state): State<UpstreamState>, Path(_n): Path<u32>| async move {
                    state.started.fetch_add(1, Ordering::SeqCst);
                    state.active.fetch_add(1, Ordering::SeqCst);
                    let _guard = ActiveGuard(state.active.clone());
                    // 30s：远大于取消预算，取消若没有 abort 在飞请求，active 不会归零
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    Json(json!({ "data": { "list": [] } }))
                },
            ),
        )
        .with_state(state);
    let (base_url, _handle) = common::spawn_router(app).await;
    (base_url, started, active)
}

struct TestServer {
    base_url: String,
    temp_dir: std::path::PathBuf,
    client: reqwest::Client,
    token: String,
    upstream: String,
}

async fn boot() -> (TestServer, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let (upstream, started, active) = start_upstream().await;
    common::clean_stale_temp_dirs();
    let temp_dir =
        std::env::temp_dir().join(format!("reader-rust-v1-tasks-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let mut cfg = common::base_config(&temp_dir, "v1-tasks-secret");
    cfg.allow_private_network = true;
    let state = reader_rust::app::bootstrap::build_state(cfg).await.unwrap();
    let app = reader_rust::api::router::build_router(state);
    let (base_url, _handle) = common::spawn_app(app).await;
    let client = reqwest::Client::new();
    let token = common::login(&client, &base_url).await;
    (
        TestServer {
            base_url,
            temp_dir,
            client,
            token,
            upstream,
        },
        started,
        active,
    )
}

impl TestServer {
    fn cleanup(&self) {
        let _ = std::fs::remove_dir_all(&self.temp_dir);
    }

    async fn create_source(&self, name: &str, search_path: &str) -> String {
        self.create_source_opts(name, search_path, None, true).await
    }

    async fn create_source_opts(
        &self,
        name: &str,
        search_path: &str,
        group: Option<&str>,
        enabled: bool,
    ) -> String {
        let response = self
            .client
            .post(format!("{}/v1/sources", self.base_url))
            .bearer_auth(&self.token)
            .json(&json!({
                "bookSourceName": name,
                "bookSourceUrl": format!("{}/source/{}", self.upstream, name),
                "bookSourceGroup": group,
                "enabled": enabled,
                "searchUrl": format!("{}{}?q={{key}}", self.upstream, search_path),
                "ruleSearch": {
                    "bookList": "$.data.list[*]",
                    "name": "$.name",
                    "author": "$.author",
                    "bookUrl": "$.bookUrl",
                    "coverUrl": "$.coverUrl",
                    "intro": "$.intro",
                    "kind": "$.kind"
                }
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body: Value = response.json().await.unwrap();
        body["id"].as_str().unwrap().to_string()
    }

    async fn create_search(&self, body: Value) -> reqwest::Response {
        self.client
            .post(format!("{}/v1/searches", self.base_url))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .unwrap()
    }

    /// 预下载用的书源：搜索 → 目录 → 正文三段规则都指向假上游。
    async fn create_prefetch_source(&self) -> String {
        let response = self
            .client
            .post(format!("{}/v1/sources", self.base_url))
            .bearer_auth(&self.token)
            .json(&json!({
                "bookSourceName": "预下载源",
                "bookSourceUrl": format!("{}/source/prefetch", self.upstream),
                "enabled": true,
                "searchUrl": format!("{}/search/prefetch?q={{key}}", self.upstream),
                "ruleSearch": {
                    "bookList": "$.data.list[*]",
                    "name": "$.name",
                    "author": "$.author",
                    "bookUrl": "$.bookUrl"
                },
                "ruleToc": {
                    "chapterList": "$.data.list[*]",
                    "chapterName": "$.name",
                    "chapterUrl": "$.url"
                },
                "ruleContent": { "content": "$.content" }
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body: Value = response.json().await.unwrap();
        body["id"].as_str().unwrap().to_string()
    }

    async fn get_task(&self, id: &str) -> (StatusCode, Value) {
        let response = self
            .client
            .get(format!("{}/v1/tasks/{id}", self.base_url))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap();
        let status = response.status();
        (status, response.json().await.unwrap())
    }

    async fn wait_terminal(&self, id: &str, timeout: Duration) -> Value {
        let started = Instant::now();
        loop {
            let (status, task) = self.get_task(id).await;
            assert_eq!(status, StatusCode::OK, "{task}");
            let state = task["state"].as_str().unwrap();
            if matches!(state, "succeeded" | "failed" | "cancelled") {
                return task;
            }
            assert!(
                started.elapsed() < timeout,
                "任务未在 {timeout:?} 内结束: {task}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn events(&self, id: &str, last_event_id: Option<u64>) -> reqwest::Response {
        let mut request = self
            .client
            .get(format!("{}/v1/tasks/{id}/events", self.base_url))
            .bearer_auth(&self.token);
        if let Some(last) = last_event_id {
            request = request.header("Last-Event-ID", last.to_string());
        }
        request.send().await.unwrap()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        common::remove_dir_retried(&self.temp_dir);
    }
}

fn sse_field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    line.strip_prefix(name)?.strip_prefix(':').map(str::trim)
}

fn sse_ids(text: &str) -> Vec<u64> {
    text.lines()
        .filter_map(|line| sse_field(line, "id"))
        .filter_map(|value| value.parse::<u64>().ok())
        .collect()
}

fn sse_events(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| sse_field(line, "event"))
        .map(str::to_string)
        .collect()
}

#[tokio::test]
async fn search_task_dedupes_and_events_replay_from_last_event_id() {
    let (server, _started, _active) = boot().await;
    let source_a = server.create_source("甲", "/search/a").await;
    let source_b = server.create_source("乙", "/search/b").await;

    let response = server
        .create_search(json!({
            "q": "三体",
            "sourceIds": [source_a, source_b],
            "limitPerSource": 20
        }))
        .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let location = response.headers()["location"].to_str().unwrap().to_string();
    let created: Value = response.json().await.unwrap();
    let task_id = created["id"].as_str().unwrap().to_string();
    assert_eq!(location, format!("/v1/tasks/{task_id}"));
    assert!(matches!(
        created["state"].as_str(),
        Some("queued") | Some("running")
    ));

    let task = server
        .wait_terminal(&task_id, Duration::from_secs(10))
        .await;
    assert_eq!(task["state"], json!("succeeded"), "{task}");
    assert_eq!(task["progress"]["done"], json!(2));
    assert_eq!(task["progress"]["total"], json!(2));
    assert_eq!(
        task["resultHref"],
        json!(format!("/v1/tasks/{task_id}/result"))
    );

    // 归一化去重：两本书名（繁体/全角/后缀）合成一条，dedupeKey 稳定
    let result: Value = server
        .client
        .get(format!("{}/v1/tasks/{task_id}/result", server.base_url))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(result["total"], json!(1), "{result}");
    assert_eq!(result["truncated"], json!(false));
    let item = &result["items"][0];
    assert_eq!(item["dedupeKey"], json!(r#"["三体","刘慈欣"]"#));
    // 结果顺序是到达顺序：先完成的书源保留其原始书名（归一化只用于合并）
    assert!(
        matches!(item["name"].as_str(), Some("三体") | Some("三體（全本）")),
        "保留先到书源的书名: {item}"
    );
    let source_ids: Vec<i64> = item["bookSourceIds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_i64().unwrap())
        .collect();
    assert!(source_ids.len() == 2, "两个书源的命中应合并: {item}");
    // 封面已收口到本站取图地址，客户端不接触书源地址
    let cover = item["coverUrl"].as_str().unwrap();
    assert!(cover.starts_with("/reader3/image/"), "{cover}");

    // 事件流：progress/item/done，id 单调递增
    let response = server.events(&task_id, None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-type"]
            .to_str()
            .unwrap()
            .split(';')
            .next(),
        Some("text/event-stream")
    );
    let text = response.text().await.unwrap();
    let ids = sse_ids(&text);
    let names = sse_events(&text);
    assert!(!ids.is_empty(), "事件必须带 id: {text}");
    assert!(ids.windows(2).all(|pair| pair[0] < pair[1]), "{ids:?}");
    assert!(names.contains(&"progress".to_string()), "{names:?}");
    assert!(names.contains(&"item".to_string()), "{names:?}");
    assert_eq!(names.last().map(String::as_str), Some("done"), "{names:?}");

    // Last-Event-ID 续传：只重放之后的 id，不重不漏
    let last = ids[0];
    let replay = server
        .events(&task_id, Some(last))
        .await
        .text()
        .await
        .unwrap();
    let replay_ids = sse_ids(&replay);
    assert!(!replay_ids.contains(&last), "不应重放已收到的事件");
    assert!(replay_ids.iter().all(|id| *id > last), "{replay_ids:?}");
    assert_eq!(
        replay_ids,
        ids.iter()
            .copied()
            .filter(|id| *id > last)
            .collect::<Vec<_>>(),
        "续传应是原序列的后缀"
    );
    assert_eq!(sse_events(&replay).last().map(String::as_str), Some("done"));

    // 已收到 done 的重连（Last-Event-ID == done.id）：不应补发重复 done
    let done_id = *ids.last().unwrap();
    let noop = server
        .events(&task_id, Some(done_id))
        .await
        .text()
        .await
        .unwrap();
    assert!(noop.trim().is_empty(), "已到队尾的重连应直接关流: {noop}");

    // ETag/304：任务状态是 GET 资源，条件请求命中即空 body
    let task_url = format!("{}/v1/tasks/{task_id}", server.base_url);
    let first = server
        .client
        .get(&task_url)
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap();
    let etag = first.headers()["etag"].to_str().unwrap().to_string();
    let second = server
        .client
        .get(&task_url)
        .bearer_auth(&server.token)
        .header("If-None-Match", &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::NOT_MODIFIED);

    // 404 / 400 的精确码
    let (status, body) = server.get_task("tsk_999999").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], json!("TASK_NOT_FOUND"));

    // 未知枚举值是 400（契约 §5.5）；limit 越界同码
    let response = server
        .client
        .get(format!("{}/v1/tasks?state=bogus", server.base_url))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response = server
        .client
        .get(format!("{}/v1/tasks?limit=9999", server.base_url))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // 列表分页：limit=1 + nextCursor 走完不重不漏，按最新在前
    let first_page: Value = server
        .client
        .get(format!("{}/v1/tasks?limit=1", server.base_url))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(first_page["items"].as_array().unwrap().len(), 1);
    assert_eq!(first_page["items"][0]["id"], json!(task_id));
    let next = first_page["nextCursor"].as_str();
    if let Some(cursor) = next {
        let second_page: Value = server
            .client
            .get(format!(
                "{}/v1/tasks?limit=1&cursor={cursor}",
                server.base_url
            ))
            .bearer_auth(&server.token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_ne!(
            second_page["items"][0]["id"],
            json!(task_id),
            "翻页不能重复上一页"
        );
    }

    // 缺 q：字段级校验 422 + JSON Pointer
    let response = server
        .create_search(json!({ "sourceIds": [source_a] }))
        .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["code"], json!("VALIDATION_FAILED"));
    assert_eq!(body["errors"][0]["pointer"], json!("/q"));

    server.cleanup();
}

#[tokio::test]
async fn result_pagination_walks_cursor_without_duplicates() {
    let (server, _started, _active) = boot().await;
    let source = server.create_source("多结果", "/search/many").await;

    let response = server
        .create_search(json!({
            "q": "书",
            "sourceIds": [source],
            "limitPerSource": 100
        }))
        .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let task_id: String = response.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let task = server
        .wait_terminal(&task_id, Duration::from_secs(10))
        .await;
    assert_eq!(task["state"], json!("succeeded"));

    let mut cursor: Option<String> = None;
    let mut names = Vec::new();
    loop {
        let mut url = format!("{}/v1/tasks/{task_id}/result?limit=10", server.base_url);
        if let Some(cursor) = &cursor {
            url.push_str(&format!("&cursor={cursor}"));
        }
        let page: Value = server
            .client
            .get(&url)
            .bearer_auth(&server.token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(page["total"], json!(60));
        assert_eq!(page["truncated"], json!(false));
        for item in page["items"].as_array().unwrap() {
            names.push(item["name"].as_str().unwrap().to_string());
        }
        cursor = page["nextCursor"].as_str().map(str::to_string);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(names.len(), 60, "分页应不重不漏");
    let unique: std::collections::HashSet<_> = names.iter().collect();
    assert_eq!(unique.len(), 60);

    server.cleanup();
}

#[tokio::test]
async fn cancel_lands_within_five_seconds_and_stops_new_upstream_requests() {
    let (server, started, active) = boot().await;
    let mut source_ids = Vec::new();
    for index in 0..12 {
        source_ids.push(
            server
                .create_source(&format!("慢源{index}"), &format!("/slow/{index}"))
                .await,
        );
    }

    let response = server
        .create_search(json!({ "q": "慢", "sourceIds": source_ids }))
        .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let task_id: String = response.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // 等到至少一个上游请求真正开始
    let wait_started = Instant::now();
    while started.load(Ordering::SeqCst) == 0 {
        assert!(
            wait_started.elapsed() < Duration::from_secs(3),
            "上游未收到请求"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let cancel_at = Instant::now();
    let response = server
        .client
        .delete(format!("{}/v1/tasks/{task_id}", server.base_url))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let task = server.wait_terminal(&task_id, Duration::from_secs(5)).await;
    assert_eq!(task["state"], json!("cancelled"));
    assert!(
        cancel_at.elapsed() < Duration::from_secs(2),
        "取消应立即落地（工作器 select! 唤醒），实际 {:?}",
        cancel_at.elapsed()
    );

    // 在飞请求被 abort：上游 handler future 被丢弃，active 归零。
    // 慢源睡 30s，如果只是「不再读流」而没有 abort，active 会一直挂着。
    let active_deadline = Instant::now();
    while active.load(Ordering::SeqCst) > 0 {
        assert!(
            active_deadline.elapsed() < Duration::from_secs(3),
            "取消后上游连接未关闭，active={}",
            active.load(Ordering::SeqCst)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // 取消后不再有新的上游请求（并发上限 8，12 个源若取消未生效会在
    // 第一批完成后继续发起剩余 4 个）
    let after_cancel = started.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        started.load(Ordering::SeqCst),
        after_cancel,
        "取消后不应再发起新的上游请求"
    );
    assert!(
        after_cancel <= 8,
        "并发上限内的在飞请求不超过 8: {after_cancel}"
    );

    // 取消释放并发名额
    let response = server
        .create_search(json!({ "q": "再试一次", "sourceIds": [source_ids[0].clone()] }))
        .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    server.cleanup();
}

#[tokio::test]
async fn fifth_concurrent_task_gets_429_with_retry_after() {
    let (server, _started, _active) = boot().await;
    let source = server.create_source("慢源", "/slow/1").await;

    let mut task_ids = Vec::new();
    for _ in 0..4 {
        let response = server
            .create_search(json!({ "q": "并发", "sourceIds": [source.clone()] }))
            .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body: Value = response.json().await.unwrap();
        task_ids.push(body["id"].as_str().unwrap().to_string());
    }
    // 等 4 个任务真的进入 running（而不是仅 queued 占位），再触发第 5 个
    let running_deadline = Instant::now();
    loop {
        let page: Value = server
            .client
            .get(format!("{}/v1/tasks?state=running", server.base_url))
            .bearer_auth(&server.token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if page["items"].as_array().unwrap().len() == 4 {
            break;
        }
        assert!(
            running_deadline.elapsed() < Duration::from_secs(3),
            "任务未进入 running: {page}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let response = server
        .create_search(json!({ "q": "第五个", "sourceIds": [source.clone()] }))
        .await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok()),
        Some("5"),
        "429 必须带 Retry-After: 5"
    );
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["code"], json!("TASK_LIMIT"));

    // 取消全部任务后名额释放
    for id in &task_ids {
        let response = server
            .client
            .delete(format!("{}/v1/tasks/{id}", server.base_url))
            .bearer_auth(&server.token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
    let wait = Instant::now();
    loop {
        let (_, page) = (
            StatusCode::OK,
            server
                .client
                .get(format!("{}/v1/tasks?state=cancelled", server.base_url))
                .bearer_auth(&server.token)
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap(),
        );
        if page["items"].as_array().unwrap().len() == 4 {
            break;
        }
        assert!(
            wait.elapsed() < Duration::from_secs(5),
            "任务未全部取消: {page}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let response = server
        .create_search(json!({ "q": "名额释放", "sourceIds": [source] }))
        .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    server.cleanup();
}

#[tokio::test]
async fn sse_ticket_authorizes_events_once_without_session() {
    let (server, _started, _active) = boot().await;
    let source = server.create_source("甲", "/search/a").await;
    let response = server
        .create_search(json!({ "q": "三体", "sourceIds": [source] }))
        .await;
    let task_id: String = response.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    server
        .wait_terminal(&task_id, Duration::from_secs(10))
        .await;

    // 非法 path 拒绝
    let response = server
        .client
        .post(format!("{}/v1/sse-tickets", server.base_url))
        .bearer_auth(&server.token)
        .json(&json!({ "path": "/v1/me" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let response = server
        .client
        .post(format!("{}/v1/sse-tickets", server.base_url))
        .bearer_auth(&server.token)
        .json(&json!({ "path": format!("/v1/tasks/{task_id}/events") }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(
        response
            .headers()
            .get("cache-control")
            .and_then(|value| value.to_str().ok()),
        Some("no-store"),
        "票据响应不能被缓存"
    );
    let body: Value = response.json().await.unwrap();
    let ticket = body["ticket"].as_str().unwrap().to_string();
    assert!(ticket.starts_with("sst_"));

    // 不带任何会话凭据，仅凭票据即可读事件流
    let response = server
        .client
        .get(format!(
            "{}/v1/tasks/{task_id}/events?ticket={ticket}",
            server.base_url
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let text = response.text().await.unwrap();
    assert_eq!(sse_events(&text).last().map(String::as_str), Some("done"));

    // 一次性：第二次使用 401
    let response = server
        .client
        .get(format!(
            "{}/v1/tasks/{task_id}/events?ticket={ticket}",
            server.base_url
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["code"], json!("AUTH_EXPIRED"));

    server.cleanup();
}

#[tokio::test]
async fn disabled_sources_and_group_matching_follow_source_semantics() {
    let (server, _started, _active) = boot().await;
    let enabled = server
        .create_source_opts("甲", "/search/a", Some("玄幻小说"), true)
        .await;
    let disabled = server
        .create_source_opts("乙", "/search/b", Some("玄幻"), false)
        .await;

    // 显式指定停用书源：409 SOURCE_DISABLED（不静默跳过，调用方要知道）
    let response = server
        .create_search(json!({ "q": "三体", "sourceIds": [disabled] }))
        .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["code"], json!("SOURCE_DISABLED"));

    // 分组精确匹配："玄幻" 不能命中 "玄幻小说"
    let response = server
        .create_search(json!({ "q": "三体", "groupId": "玄幻" }))
        .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["code"], json!("VALIDATION_FAILED"));
    assert_eq!(body["errors"][0]["pointer"], json!("/groupId"));

    // 正确分组名 → 202 且只搜到 1 个书源
    let response = server
        .create_search(json!({ "q": "三体", "groupId": "玄幻小说" }))
        .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let task_id: String = response.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let task = server
        .wait_terminal(&task_id, Duration::from_secs(10))
        .await;
    assert_eq!(task["state"], json!("succeeded"));
    assert_eq!(task["progress"]["total"], json!(1));

    // 不带 sourceIds/groupId：默认只搜启用源（停用源被排除）
    let response = server.create_search(json!({ "q": "三体" })).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let task_id: String = response.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let task = server
        .wait_terminal(&task_id, Duration::from_secs(10))
        .await;
    assert_eq!(task["progress"]["total"], json!(1));
    let result: Value = server
        .client
        .get(format!("{}/v1/tasks/{task_id}/result", server.base_url))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        result["items"][0]["bookSourceId"],
        json!(enabled.trim_start_matches("src_").parse::<i64>().unwrap()),
        "命中的应是启用书源"
    );

    server.cleanup();
}

#[tokio::test]
async fn task_event_stream_has_subscriber_limit() {
    let (server, _started, _active) = boot().await;
    let source = server.create_source("慢源", "/slow/1").await;
    let response = server
        .create_search(json!({ "q": "订阅", "sourceIds": [source] }))
        .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let task_id: String = response.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let deadline = Instant::now();
    loop {
        let page: Value = server
            .client
            .get(format!("{}/v1/tasks?state=running", server.base_url))
            .bearer_auth(&server.token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if page["items"].as_array().unwrap().len() == 1 {
            break;
        }
        assert!(deadline.elapsed() < Duration::from_secs(3), "{page}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // 占满 4 个订阅者；响应体不消费，保持连接活到流自然结束
    let mut streams = Vec::new();
    for _ in 0..4 {
        let response = server.events(&task_id, None).await;
        assert_eq!(response.status(), StatusCode::OK);
        streams.push(response);
    }
    let fifth = server.events(&task_id, None).await;
    assert_eq!(fifth.status(), StatusCode::TOO_MANY_REQUESTS);
    let body: Value = fifth.json().await.unwrap();
    assert_eq!(body["code"], json!("TASK_LIMIT"));
    drop(streams);

    let response = server
        .client
        .delete(format!("{}/v1/tasks/{task_id}", server.base_url))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    server.cleanup();
}

#[tokio::test]
async fn searches_are_idempotent_by_header() {
    let (server, _started, _active) = boot().await;
    let source = server.create_source("甲", "/search/a").await;

    let post = |key: &str, q: &str| {
        server
            .client
            .post(format!("{}/v1/searches", server.base_url))
            .bearer_auth(&server.token)
            .header("Idempotency-Key", key)
            .json(&json!({ "q": q, "sourceIds": [source.clone()] }))
    };

    let first = post("weak-network-retry", "三体").send().await.unwrap();
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    let id1: String = first.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // 同 key + 不同 body：仍返回原任务（不新建、不再消耗名额）
    let second = post("weak-network-retry", "不同的关键字")
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::ACCEPTED);
    let id2: String = second.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(id2, id1, "同 Idempotency-Key 必须返回原任务");

    let list: Value = server
        .client
        .get(format!("{}/v1/tasks", server.base_url))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list["total"], json!(1), "重试不应创建第二个任务: {list}");

    // 新 key → 新任务
    let third = post("another-key", "三体").send().await.unwrap();
    assert_eq!(third.status(), StatusCode::ACCEPTED);
    let id3: String = third.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(id3, id1);

    server.cleanup();
}

#[tokio::test]
async fn prefetch_cache_task_reports_summary_and_skips_cached() {
    let (server, _started, _active) = boot().await;
    let source = server.create_prefetch_source().await;

    // 先用搜索把书登记出来（预下载按 bookId 工作）
    let response = server
        .create_search(json!({ "q": "预下载", "sourceIds": [source] }))
        .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let search_task: String = response.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let search_task = server
        .wait_terminal(&search_task, Duration::from_secs(10))
        .await;
    assert_eq!(search_task["state"], json!("succeeded"), "{search_task}");
    let search_task_id = search_task["id"].as_str().unwrap();
    let result: Value = server
        .client
        .get(format!(
            "{}/v1/tasks/{search_task_id}/result",
            server.base_url
        ))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let book_id = result["items"][0]["bookId"].as_str().unwrap().to_string();

    // 预下载 2 章
    let response = server
        .client
        .post(format!("{}/v1/books/{book_id}/cache", server.base_url))
        .bearer_auth(&server.token)
        .json(&json!({ "from": 0, "count": 2 }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let task: Value = response.json().await.unwrap();
    assert_eq!(task["kind"], json!("prefetch-cache"));
    assert_eq!(task["progress"]["unit"], json!("chapter"));
    assert_eq!(task["progress"]["total"], json!(2));
    let task_id = task["id"].as_str().unwrap().to_string();

    let task = server
        .wait_terminal(&task_id, Duration::from_secs(10))
        .await;
    assert_eq!(task["state"], json!("succeeded"), "{task}");
    let summary: Value = server
        .client
        .get(format!("{}/v1/tasks/{task_id}/result", server.base_url))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        summary,
        json!({ "requested": 2, "cached": 2, "skipped": 0, "failed": 0 }),
        "{summary}"
    );

    // 同一范围再来一次：全部跳过（正文池已有）
    let response = server
        .client
        .post(format!("{}/v1/books/{book_id}/cache", server.base_url))
        .bearer_auth(&server.token)
        .json(&json!({ "from": 0, "count": 2 }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let task_id: String = response.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let task = server
        .wait_terminal(&task_id, Duration::from_secs(10))
        .await;
    assert_eq!(task["state"], json!("succeeded"), "{task}");
    let summary: Value = server
        .client
        .get(format!("{}/v1/tasks/{task_id}/result", server.base_url))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        summary,
        json!({ "requested": 2, "cached": 0, "skipped": 2, "failed": 0 }),
        "{summary}"
    );

    // from 越界：422 + pointer
    let response = server
        .client
        .post(format!("{}/v1/books/{book_id}/cache", server.base_url))
        .bearer_auth(&server.token)
        .json(&json!({ "from": 99, "count": 2 }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["errors"][0]["pointer"], json!("/from"));

    server.cleanup();
}
