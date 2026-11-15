//! `/v1/me/read-events:batch` + `/v1/me/stats` 端到端：阅读事件写入、
//! MAX 语义、统计派生、批次校验与书架 dur_chapter_time 推进。

mod common;

use axum::http::StatusCode;
use reader_rust::api::router::build_router;
use reader_rust::app::bootstrap::build_state;
use serde_json::{json, Value};
use std::path::PathBuf;

struct TestServer {
    base_url: String,
    temp_dir: PathBuf,
    client: reqwest::Client,
    token: String,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        common::remove_dir_retried(&self.temp_dir);
    }
}

impl TestServer {
    async fn start() -> Self {
        common::clean_stale_temp_dirs();
        let temp_dir =
            std::env::temp_dir().join(format!("reader-rust-read-stats-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let cfg = common::base_config(&temp_dir, "read-stats-secret");
        let state = build_state(cfg).await.unwrap();
        let app = build_router(state);
        let (base_url, _handle) = common::spawn_app(app).await;
        let client = reqwest::Client::new();
        let token = common::login(&client, &base_url).await;
        Self {
            base_url,
            temp_dir,
            client,
            token,
        }
    }

    async fn upload_txt(&self, content: &str) -> String {
        let boundary = "----readstatsboundary";
        let body = format!(
            "--{boundary}\r\n\
             Content-Disposition: form-data; name=\"file\"; filename=\"统计测试.txt\"\r\n\
             Content-Type: text/plain\r\n\
             \r\n\
             {content}\r\n\
             --{boundary}--\r\n"
        );
        let response = self
            .client
            .post(format!("{}/reader3/uploadTxtBook", self.base_url))
            .bearer_auth(&self.token)
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = response.json().await.unwrap();
        body["data"]["bookId"].as_str().unwrap().to_string()
    }

    async fn post_events(&self, events: Value) -> reqwest::Response {
        self.client
            .post(format!("{}/v1/me/read-events:batch", self.base_url))
            .bearer_auth(&self.token)
            .json(&json!({ "events": events }))
            .send()
            .await
            .unwrap()
    }

    async fn stats(&self) -> Value {
        self.client
            .get(format!("{}/v1/me/stats", self.base_url))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn read_events_upsert_max_and_stats_are_derived() {
    let server = TestServer::start().await;
    let content = "\
第一章 开始
正文一。
第二章 继续
正文二。
第三章 结尾
正文三。";
    let book_id = server.upload_txt(content).await;

    // 第一次上报：第 0 章 10 秒 + 第 2 章 20 秒
    let response = server
        .post_events(json!([
            { "bookId": book_id, "chapterIndex": 0, "seconds": 10 },
            { "bookId": book_id, "chapterIndex": 2, "seconds": 20 },
        ]))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["accepted"], json!(2));
    assert!(body["dayKey"].is_i64(), "{body}");

    let stats = server.stats().await;
    assert_eq!(stats["totalSeconds"], json!(30), "{stats}");
    assert_eq!(stats["todaySeconds"], json!(30), "{stats}");
    assert_eq!(stats["readChapters"], json!(2), "{stats}");
    // 本地 TXT 不写 chapters 登记表，最后一章按书架 total_chapter_num 判定
    assert_eq!(stats["finishedBooks"], json!(1), "{stats}");
    assert_eq!(stats["streakDays"], json!(1), "{stats}");

    // 同章再报更小的累计值：MAX 语义保持 10，不回落
    let response = server
        .post_events(json!([{ "bookId": book_id, "chapterIndex": 0, "seconds": 5 }]))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let stats = server.stats().await;
    assert_eq!(stats["totalSeconds"], json!(30), "{stats}");

    // 超上限截断到 86400
    let response = server
        .post_events(json!([{ "bookId": book_id, "chapterIndex": 0, "seconds": 999999 }]))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let stats = server.stats().await;
    assert_eq!(stats["totalSeconds"], json!(86_420), "{stats}");
    assert_eq!(stats["todaySeconds"], json!(86_420), "{stats}");

    // 同一事务已推进书架 dur_chapter_time → lastReadAt 非空
    let shelf: Value = server
        .client
        .get(format!("{}/v1/me/shelf/{}", server.base_url, book_id))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(shelf["lastReadAt"].is_string(), "{shelf}");
}

#[tokio::test]
async fn read_events_with_stale_day_key_do_not_contaminate_today() {
    let server = TestServer::start().await;
    let book_id = server
        .upload_txt("第一章 开始\n正文。\n第二章 结束\n正文。")
        .await;

    let response = server
        .post_events(json!([{ "bookId": book_id, "chapterIndex": 0, "seconds": 10 }]))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let day_key = response.json::<Value>().await.unwrap()["dayKey"]
        .as_i64()
        .unwrap();

    // 客户端带着旧 dayKey 上报跨日/离线缓冲：写回旧日，不进今天
    let response = server
        .client
        .post(format!("{}/v1/me/read-events:batch", server.base_url))
        .bearer_auth(&server.token)
        .json(&json!({
            "dayKey": day_key - 1,
            "events": [{ "bookId": book_id, "chapterIndex": 1, "seconds": 50 }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["dayKey"], json!(day_key));

    let stats = server.stats().await;
    assert_eq!(stats["totalSeconds"], json!(60), "{stats}");
    assert_eq!(
        stats["todaySeconds"],
        json!(10),
        "旧 dayKey 的秒数不能算今天：{stats}"
    );
    assert_eq!(stats["readChapters"], json!(2), "{stats}");
}

#[tokio::test]
async fn streak_keeps_yesterday_until_today_is_read() {
    let server = TestServer::start().await;
    let book_id = server
        .upload_txt("第一章 开始\n正文。\n第二章 结束\n正文。")
        .await;

    // 空批次先拿服务端当前 dayKey
    let response = server.post_events(json!([])).await;
    let day_key = response.json::<Value>().await.unwrap()["dayKey"]
        .as_i64()
        .unwrap();

    let response = server
        .client
        .post(format!("{}/v1/me/read-events:batch", server.base_url))
        .bearer_auth(&server.token)
        .json(&json!({
            "dayKey": day_key - 1,
            "events": [{ "bookId": book_id, "chapterIndex": 0, "seconds": 10 }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let stats = server.stats().await;
    assert_eq!(stats["todaySeconds"], json!(0), "{stats}");
    assert_eq!(
        stats["streakDays"],
        json!(1),
        "今天没读时应从昨天起算：{stats}"
    );
    assert_eq!(stats["totalSeconds"], json!(10), "{stats}");
}

#[tokio::test]
async fn read_events_batch_validation_and_atomic_book_lookup() {
    let server = TestServer::start().await;
    let book_id = server
        .upload_txt("第一章 开始\n正文。\n第二章 结束\n正文。")
        .await;

    // 书不在登记表：整批 404，已合法的那条也不能落库
    let response = server
        .post_events(json!([
            { "bookId": book_id, "chapterIndex": 0, "seconds": 7 },
            { "bookId": "book_999999", "chapterIndex": 0, "seconds": 9 },
        ]))
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["code"], json!("BOOK_NOT_FOUND"), "{body}");
    let stats = server.stats().await;
    assert_eq!(stats["totalSeconds"], json!(0), "{stats}");

    // 负章节 → 422，pointer 指向具体事件
    let response = server
        .post_events(json!([{ "bookId": book_id, "chapterIndex": -1, "seconds": 1 }]))
        .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = response.json().await.unwrap();
    assert_eq!(
        body["errors"][0]["pointer"],
        json!("/events/0/chapterIndex")
    );

    // 超过 100 条 → 422
    let events: Vec<Value> = (0..101)
        .map(|index| json!({ "bookId": book_id, "chapterIndex": index, "seconds": 1 }))
        .collect();
    let response = server.post_events(Value::Array(events)).await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["errors"][0]["pointer"], json!("/events"));

    // 空数组合法：accepted=0，同时拿到 dayKey
    let response = server.post_events(json!([])).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["accepted"], json!(0));
    assert!(body["dayKey"].is_i64(), "{body}");
}
