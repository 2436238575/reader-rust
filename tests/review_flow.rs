//! 章评 / 段评的端到端行为：规则解析、7 天缓存与按类型清理。
//!
//! 上游用一个假的「番茄式」服务顶替（真实 FQWeb 需要一台装着番茄小说的
//! 手机），响应结构与真实接口一致，因此书源规则、URL 求值、解析与缓存
//! 走的都是生产路径。

use axum::{
    extract::{Query, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use reader_rust::api::router::build_router;
use reader_rust::app::bootstrap::build_state;
use reader_rust::app::config::AppConfig;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const BOOK_ID: &str = "7143038691944959011";
const ITEM_ID: &str = "7173615518858150414";
const ITEM_VERSION: &str = "52754e01aa26a8dbd8c15cbcf7f4f64b_1_v5";

type HitCounter = Arc<Mutex<HashMap<String, usize>>>;

/// 假上游：只实现评论链路需要的几个接口，并统计每个路径被请求的次数。
async fn start_upstream() -> (String, HitCounter) {
    let hits: HitCounter = Arc::new(Mutex::new(HashMap::new()));

    let content = json!({
        "data": { "data": {
            "content": "第一段正文\n第二段正文\n第三段正文",
            "novel_data": {
                "book_id": BOOK_ID,
                "item_id": ITEM_ID,
                "version": ITEM_VERSION,
            }
        }}
    });

    let chapter_comments = json!({
        "data": { "data": {
            "comment_cnt": 67,
            "has_more": true,
            "comment": [{
                "comment_id": "c1",
                "text": "第一条章评",
                "create_timestamp": 1717597455,
                "digg_count": 68,
                "reply_count": 1,
                "user_info": { "user_name": "读者甲", "user_avatar": "https://img.example/a.jpg" },
                "reply_list": [
                    { "text": "同感", "user_info": { "user_name": "读者乙" } }
                ]
            }]
        }}
    });

    let para_index = json!({
        "data": { "data": { "idea_data": {
            "0": { "idea_count": 140 },
            "2": { "idea_count": 30 },
            "-1": { "idea_count": 9 }
        }}}
    });

    let para_comments = json!({
        "data": { "data": {
            "count": 192,
            "has_more": false,
            "comments": [{
                "comment_id": "p1",
                "text": "这一段写得真好",
                "create_timestamp": 1738911978,
                "digg_count": 216,
                "reply_count": 1,
                // 番茄同一张图给多个格式变体，后端按原序返回，由客户端挑
                "image_url": [
                    "https://img.example/a.heic?sign=1",
                    "https://img.example/a.jpeg?sign=2"
                ],
                "user_info": { "user_name": "读者丙" },
                "reply_list": [
                    { "text": "同感", "user_info": { "user_name": "读者丁" } }
                ]
            }]
        }}
    });

    let app = Router::new()
        .route(
            "/content",
            get({
                let content = content.clone();
                move || {
                    let content = content.clone();
                    async move { Json(content) }
                }
            }),
        )
        .route(
            "/comment/item",
            get({
                let chapter_comments = chapter_comments.clone();
                move |Query(params): Query<HashMap<String, String>>| {
                    let mut body = chapter_comments.clone();
                    // 只在 sort=time 时换一份内容：既能证明参数真的进了上游 URL，
                    // 又不影响其余用例对默认内容的断言
                    if params.get("sort").map(String::as_str) == Some("time") {
                        body["data"]["data"]["comment"][0]["text"] = json!("最新序第一条");
                    }
                    // page=99 返回空列表，用于验证「空页不缓存」
                    if params.get("page").map(String::as_str) == Some("99") {
                        body["data"]["data"]["comment"] = json!([]);
                    }
                    async move { Json(body) }
                }
            }),
        )
        .route(
            "/comment/para/list",
            get({
                let para_index = para_index.clone();
                move || {
                    let para_index = para_index.clone();
                    async move { Json(para_index) }
                }
            }),
        )
        .route(
            "/comment/para",
            get({
                let para_comments = para_comments.clone();
                move |Query(params): Query<HashMap<String, String>>| {
                    let mut body = para_comments.clone();
                    if params.get("sort").map(String::as_str) == Some("time_desc") {
                        body["data"]["data"]["comments"][0]["text"] = json!("最新序段评");
                    }
                    async move { Json(body) }
                }
            }),
        )
        .route(
            "/__hits",
            get(|State(hits): State<HitCounter>| async move {
                let snapshot: HashMap<String, usize> = hits.lock().unwrap().clone();
                Json(serde_json::to_value(snapshot).unwrap())
            }),
        )
        .with_state(hits.clone());

    // 计数中间件：包住上面所有路由，按路径累加
    let counter = hits.clone();
    let app = app.layer(axum::middleware::from_fn(
        move |request: axum::http::Request<axum::body::Body>, next: axum::middleware::Next| {
            let counter = counter.clone();
            async move {
                let path = request.uri().path().to_string();
                *counter.lock().unwrap().entry(path).or_insert(0) += 1;
                next.run(request).await
            }
        },
    ));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), hits)
}

struct TestServer {
    base_url: String,
    upstream_url: String,
    hits: HitCounter,
    temp_dir: PathBuf,
    client: reqwest::Client,
    token: String,
}

fn book_source(upstream_url: &str) -> Value {
    json!({
        "bookSourceName": "假番茄",
        "bookSourceUrl": upstream_url,
        // 关掉限速，测试里连打多个接口
        "concurrentRate": "0",
        "enabled": true,
        "ruleContent": { "content": "$.data.data.content" },
        "ruleReview": {
            "reviewUrl": "comment/item?item_id={{$.data.data.novel_data.item_id}}&book_id={{$.data.data.novel_data.book_id}}&page={{page}}&count={{count}}&sort={{sort === 'hot' ? 'smart_hot' : 'time'}}",
            "listRule": "$.data.data.comment[*]",
            "totalRule": "$.data.data.comment_cnt",
            "hasMoreRule": "$.data.data.has_more",
            "idRule": "$.comment_id",
            "nameRule": "$.user_info.user_name",
            "avatarRule": "$.user_info.user_avatar",
            "contentRule": "$.text",
            "postTimeRule": "$.create_timestamp",
            "diggRule": "$.digg_count",
            "replyCountRule": "$.reply_count",
            "replyListRule": "$.reply_list[*]",
            "replyNameRule": "$.user_info.user_name",
            "replyContentRule": "$.text",
            "imageRule": "$.image_url[*]"
        },
        "ruleParaReview": {
            "indexUrl": "comment/para/list?book_id={{$.data.data.novel_data.book_id}}&item_id={{$.data.data.novel_data.item_id}}&item_version={{$.data.data.novel_data.version}}",
            "indexListRule": "$.data.data.idea_data",
            "indexCountRule": "$.idea_count",
            "reviewUrl": "comment/para?book_id={{$.data.data.novel_data.book_id}}&item_id={{$.data.data.novel_data.item_id}}&para_index={{paraIndex}}&item_version={{$.data.data.novel_data.version}}&page={{page}}&count={{count}}&sort={{sort === 'hot' ? 'hot' : 'time_desc'}}",
            "listRule": "$.data.data.comments[*]",
            "totalRule": "$.data.data.count",
            "hasMoreRule": "$.data.data.has_more",
            "idRule": "$.comment_id",
            "nameRule": "$.user_info.user_name",
            "contentRule": "$.text",
            "postTimeRule": "$.create_timestamp",
            "diggRule": "$.digg_count",
            "replyCountRule": "$.reply_count",
            "replyListRule": "$.reply_list[*]",
            "replyNameRule": "$.user_info.user_name",
            "replyContentRule": "$.text",
            "imageRule": "$.image_url[*]"
        }
    })
}

impl TestServer {
    async fn start() -> Self {
        let (upstream_url, hits) = start_upstream().await;

        let temp_dir =
            std::env::temp_dir().join(format!("reader-rust-review-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let cfg = AppConfig {
            server_host: "127.0.0.1".to_string(),
            server_port: 0,
            database_url: format!("sqlite:{}?mode=rwc", temp_dir.join("reader.db").display()),
            storage_dir: temp_dir.to_string_lossy().to_string(),
            web_root: temp_dir.to_string_lossy().to_string(),
            assets_dir: temp_dir.join("assets").to_string_lossy().to_string(),
            jwt_secret: "review-test-secret".to_string(),
            request_timeout_secs: 10,
            // bootstrap 用预设凭据建号，测试直接登录
            admin_username: "reader1".to_string(),
            admin_password: "password123".to_string(),
            // 集成测试从同一地址重复登录，豁免 IP 限速
            rate_limit_disabled: true,
            ..AppConfig::default()
        };

        let state = build_state(cfg).await.unwrap();
        let app = build_router(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await;
        });

        let client = reqwest::Client::new();
        let base_url = format!("http://{addr}");
        let token = register(&client, &base_url, "reader1").await;
        let mut server = Self {
            base_url,
            upstream_url,
            hits,
            temp_dir,
            client,
            token,
        };
        server.save_source().await;
        server
    }

    async fn save_source(&mut self) {
        let resp = self
            .client
            .post(format!("{}/reader3/saveBookSource", self.base_url))
            .bearer_auth(&self.token)
            .json(&book_source(&self.upstream_url))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    async fn post(&self, path: &str, body: Value) -> Value {
        let resp = self
            .client
            .post(format!("{}/reader3/{path}", self.base_url))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{path} 应返回 200");
        let payload: Value = resp.json().await.unwrap();
        assert_eq!(payload["isSuccess"], json!(true), "{path} 应成功");
        payload["data"].clone()
    }

    fn book_url(&self) -> String {
        format!("{}/info?book_id={BOOK_ID}", self.upstream_url)
    }

    fn chapter_url(&self) -> String {
        format!("{}/content?item_id={ITEM_ID}", self.upstream_url)
    }

    fn review_body(&self, extra: Value) -> Value {
        let mut body = json!({
            "bookUrl": self.book_url(),
            "chapterUrl": self.chapter_url(),
            "bookSourceUrl": self.upstream_url,
        });
        for (key, value) in extra.as_object().unwrap() {
            body[key] = value.clone();
        }
        body
    }

    fn hits_for(&self, path: &str) -> usize {
        *self.hits.lock().unwrap().get(path).unwrap_or(&0)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.temp_dir);
    }
}

async fn register(client: &reqwest::Client, base_url: &str, username: &str) -> String {
    let resp = client
        .post(format!("{base_url}/reader3/login"))
        .json(&json!({
            "username": username,
            "password": "password123",
        }))
        .send()
        .await
        .unwrap();
    let payload: Value = resp.json().await.unwrap();
    assert_eq!(payload["isSuccess"], json!(true), "登录应成功: {payload}");
    payload["data"]["accessToken"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn chapter_and_para_comments_are_parsed_from_the_source_rules() {
    let server = TestServer::start().await;

    let chapter = server
        .post("getChapterComments", server.review_body(json!({})))
        .await;
    assert_eq!(chapter["enabled"], json!(true));
    assert_eq!(chapter["data"]["total"], json!(67));
    assert_eq!(chapter["data"]["hasMore"], json!(true));
    assert_eq!(chapter["data"]["page"], json!(1));
    let items = chapter["data"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["name"], json!("读者甲"));
    assert_eq!(items[0]["content"], json!("第一条章评"));
    assert_eq!(items[0]["digg"], json!(68));
    assert_eq!(items[0]["replies"][0]["name"], json!("读者乙"));
    assert_eq!(items[0]["replies"][0]["content"], json!("同感"));

    let index = server
        .post("getParaCommentIndex", server.review_body(json!({})))
        .await;
    assert_eq!(index["enabled"], json!(true));
    let paras = index["data"]["paras"].as_array().unwrap();
    // -1 是整章聚合桶，不属于任何段落，必须被过滤掉
    assert_eq!(paras.len(), 2);
    assert_eq!(paras[0]["paraIndex"], json!(0));
    assert_eq!(paras[0]["count"], json!(140));
    // 段号会因替换规则漂移，后端要带上段落原文做兜底定位
    assert_eq!(paras[0]["text"], json!("第一段正文"));

    let para = server
        .post(
            "getParaComments",
            server.review_body(json!({ "paraIndex": 2 })),
        )
        .await;
    assert_eq!(para["data"]["total"], json!(192));
    // 模板里的 JS 映射把 hot/time 翻译成站点要的 hot/time_desc，
    // 所以下面这个请求（默认 hot）不该触发换序分支
    let item = &para["data"]["items"][0];
    assert_eq!(item["content"], json!("这一段写得真好"));
    assert_eq!(para["serverSort"], json!(true));

    let time_para = server
        .post(
            "getParaComments",
            server.review_body(json!({ "paraIndex": 2, "sort": "time" })),
        )
        .await;
    assert_eq!(
        time_para["data"]["items"][0]["content"],
        json!("最新序段评"),
        "sort 应经 JS 映射成 time_desc 传给上游"
    );
    // 段评的内联回复与章评一样要解析出来
    assert_eq!(item["replyCount"], json!(1));
    assert_eq!(item["replies"][0]["name"], json!("读者丁"));
    assert_eq!(item["replies"][0]["content"], json!("同感"));
    // 图片按原序返回（HEIC 在前、JPEG 在后），挑格式是客户端的事
    assert_eq!(
        item["images"],
        json!([
            "https://img.example/a.heic?sign=1",
            "https://img.example/a.jpeg?sign=2"
        ])
    );
}

#[tokio::test]
async fn para_comments_require_a_para_index() {
    let server = TestServer::start().await;
    let resp = server
        .client
        .post(format!("{}/reader3/getParaComments", server.base_url))
        .bearer_auth(&server.token)
        .json(&server.review_body(json!({})))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn comments_are_cached_for_seven_days_and_reused() {
    let server = TestServer::start().await;

    server
        .post("getChapterComments", server.review_body(json!({})))
        .await;
    let after_first = server.hits_for("/comment/item");
    assert_eq!(after_first, 1, "首次应真的打上游");

    server
        .post("getChapterComments", server.review_body(json!({})))
        .await;
    assert_eq!(
        server.hits_for("/comment/item"),
        after_first,
        "第二次应命中缓存，不再打上游"
    );

    // refresh=1 绕过缓存
    server
        .post(
            "getChapterComments",
            server.review_body(json!({ "refresh": 1 })),
        )
        .await;
    assert_eq!(server.hits_for("/comment/item"), after_first + 1);

    // 段评概览同理
    server
        .post("getParaCommentIndex", server.review_body(json!({})))
        .await;
    let after_index = server.hits_for("/comment/para/list");
    server
        .post("getParaCommentIndex", server.review_body(json!({})))
        .await;
    assert_eq!(server.hits_for("/comment/para/list"), after_index);
}

#[tokio::test]
async fn purge_kind_review_clears_the_comment_cache() {
    let server = TestServer::start().await;

    server
        .post("getChapterComments", server.review_body(json!({})))
        .await;
    assert_eq!(server.hits_for("/comment/item"), 1);

    let purged = server
        .post("purgeCache", json!({ "scope": "kind", "kind": "review" }))
        .await;
    assert!(
        purged["purged"]["review"].as_u64().unwrap() >= 1,
        "应报告清理掉的评论缓存文件数: {purged}"
    );

    // 清完之后必须重新抓，说明缓存真的被删掉了
    server
        .post("getChapterComments", server.review_body(json!({})))
        .await;
    assert_eq!(server.hits_for("/comment/item"), 2);

    // 缓存文件也确实写回了
    let stats = server
        .client
        .get(format!("{}/reader3/cacheStats", server.base_url))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert!(stats["data"]["review"]["files"].as_u64().unwrap() >= 1);
}

#[tokio::test]
async fn sort_reaches_the_source_and_has_its_own_cache_entry() {
    let server = TestServer::start().await;

    // 默认（最热）：模板里用了 {{sort}}，但站点按自己的热度序返回
    let hot = server
        .post("getChapterComments", server.review_body(json!({})))
        .await;
    assert_eq!(hot["serverSort"], json!(true));
    assert_eq!(hot["data"]["items"][0]["content"], json!("第一条章评"));

    // 最新：{{sort}} 求值成 time 并传给了上游
    let time = server
        .post(
            "getChapterComments",
            server.review_body(json!({ "sort": "time" })),
        )
        .await;
    assert_eq!(time["data"]["items"][0]["content"], json!("最新序第一条"));
    assert_eq!(server.hits_for("/comment/item"), 2, "两种排序各打一次上游");

    // 再各来一次：命中各自的缓存，不该再多打上游
    server
        .post("getChapterComments", server.review_body(json!({})))
        .await;
    server
        .post(
            "getChapterComments",
            server.review_body(json!({ "sort": "time" })),
        )
        .await;
    assert_eq!(server.hits_for("/comment/item"), 2, "排序不同即缓存键不同");

    // 认不出来的取值按默认的「最热」处理，不报错
    let unknown = server
        .post(
            "getChapterComments",
            server.review_body(json!({ "sort": "whatever" })),
        )
        .await;
    assert_eq!(unknown["data"]["items"][0]["content"], json!("第一条章评"));
}

#[tokio::test]
async fn sources_without_sort_in_the_rule_report_client_side_sorting() {
    let server = TestServer::start().await;

    let mut source = book_source(&server.upstream_url);
    // 去掉模板里的 {{sort}}：站点不做排序，前端只能重排已加载的条目
    source["ruleReview"]["reviewUrl"] = json!(
        "comment/item?item_id={{$.data.data.novel_data.item_id}}         &book_id={{$.data.data.novel_data.book_id}}&page={{page}}&count={{count}}"
    );
    let resp = server
        .client
        .post(format!("{}/reader3/saveBookSource", server.base_url))
        .bearer_auth(&server.token)
        .json(&source)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let page = server
        .post("getChapterComments", server.review_body(json!({})))
        .await;
    assert_eq!(page["serverSort"], json!(false));
}

#[tokio::test]
async fn empty_comment_pages_are_not_cached() {
    let server = TestServer::start().await;
    let body = server.review_body(json!({ "page": 99 }));

    let first = server.post("getChapterComments", body.clone()).await;
    assert_eq!(first["data"]["items"].as_array().unwrap().len(), 0);
    let hits_after_first = server.hits_for("/comment/item");

    server.post("getChapterComments", body).await;
    assert_eq!(
        server.hits_for("/comment/item"),
        hits_after_first + 1,
        "空页不该进缓存，第二次必须重新打上游——否则上游一次抖动就会让评论空白七天"
    );
}

#[tokio::test]
async fn sources_without_review_rules_report_disabled() {
    let server = TestServer::start().await;

    // 覆盖成一个没有评论规则的书源
    let bare = json!({
        "bookSourceName": "无评论规则",
        "bookSourceUrl": server.upstream_url,
        "concurrentRate": "0",
        "enabled": true,
    });
    let resp = server
        .client
        .post(format!("{}/reader3/saveBookSource", server.base_url))
        .bearer_auth(&server.token)
        .json(&bare)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let chapter = server
        .post("getChapterComments", server.review_body(json!({})))
        .await;
    assert_eq!(chapter["enabled"], json!(false));
    assert!(chapter["data"]["items"].as_array().unwrap().is_empty());

    let index = server
        .post("getParaCommentIndex", server.review_body(json!({})))
        .await;
    assert_eq!(index["enabled"], json!(false));
    assert!(index["data"]["paras"].as_array().unwrap().is_empty());

    // 不支持时不该白跑一趟上游
    assert_eq!(server.hits_for("/comment/item"), 0);
    assert_eq!(server.hits_for("/comment/para/list"), 0);
}
