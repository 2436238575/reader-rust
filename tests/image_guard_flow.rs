//! v1 图片解析入口的严格出站策略：默认（`ALLOW_PRIVATE_NETWORK=false`）
//! 拒绝环回/私网地址。单独一个测试二进制：出站守卫是进程级全局开关，
//! 与需要放行 127.0.0.1 假上游的 `image_pipeline.rs` 放在同一进程会互相污染。

mod common;

use axum::http::StatusCode;
use reader_rust::api::router::build_router;
use reader_rust::app::bootstrap::build_state;
use serde_json::{json, Value};

#[tokio::test]
async fn v1_image_resolve_rejects_private_address() {
    common::clean_stale_temp_dirs();
    let temp_dir =
        std::env::temp_dir().join(format!("reader-rust-image-guard-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_dir).unwrap();

    let cfg = common::base_config(&temp_dir, "image-guard-secret");
    assert!(!cfg.allow_private_network, "本测试要求默认拦截私网");
    let state = build_state(cfg).await.unwrap();
    let app = build_router(state);
    let (base_url, _handle) = common::spawn_app(app).await;
    let client = reqwest::Client::new();
    let token = common::login(&client, &base_url).await;

    let response = client
        .get(format!("{base_url}/v1/images:resolve"))
        .bearer_auth(&token)
        .query(&[("url", "http://127.0.0.1:1/plain.jpg")])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["code"], json!("UPSTREAM_BLOCKED"), "{body}");

    common::remove_dir_retried(&temp_dir);
}
