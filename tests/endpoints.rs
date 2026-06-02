//! Status surface: `/`, `/healthz`, `/metrics` (gated), and generated
//! `config.json`.

mod common;

use common::{TestProxy, CRATE_BYTES};
use serde_json::Value;

#[tokio::test]
async fn home_reports_running() {
    let proxy = TestProxy::builder().start().await;

    let resp = proxy.get("/").await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()["content-type"],
        "application/json; charset=utf-8"
    );
    assert_eq!(resp.text().await.unwrap(), r#"{"status":"running"}"#);
}

#[tokio::test]
async fn healthz_is_ok() {
    let proxy = TestProxy::builder().start().await;

    let resp = proxy.get("/healthz").await;
    assert_eq!(resp.status(), 200);
    assert!(resp.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/plain"));
    assert_eq!(resp.text().await.unwrap(), "ok\n");
}

#[tokio::test]
async fn metrics_404_when_disabled() {
    let proxy = TestProxy::builder().start().await;
    assert_eq!(proxy.get("/metrics").await.status(), 404);
}

#[tokio::test]
async fn metrics_empty_when_enabled_with_no_cache() {
    let proxy = TestProxy::builder().enable_metrics().start().await;

    let resp = proxy.get("/metrics").await;
    assert_eq!(resp.status(), 200);
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["service"], "chilled-crates");
    assert_eq!(json["cached_count"], 0);
    assert_eq!(json["crates"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn metrics_lists_cached_crate_after_download() {
    let proxy = TestProxy::builder().enable_metrics().start().await;
    proxy.mock_crate("serde", "1.0.0", CRATE_BYTES).await;

    // Populate the crate cache.
    assert_eq!(proxy.download("serde", "1.0.0").await.status(), 200);

    let json: Value = proxy.get("/metrics").await.json().await.unwrap();
    assert_eq!(json["service"], "chilled-crates");
    assert_eq!(json["cached_count"], 1);
    let entry = &json["crates"][0];
    assert_eq!(entry["name"], "serde");
    assert_eq!(entry["version"], "1.0.0");
    assert!(entry["cached_at"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn config_json_points_downloads_at_proxy() {
    let proxy = TestProxy::builder()
        .proxy_url("http://proxy.test/")
        .start()
        .await;

    let resp = proxy.get_config_json().await;
    assert_eq!(resp.status(), 200);
    let json: Value = resp.json().await.unwrap();

    // `dl` is rewritten to this proxy; `api` is the upstream, both trimmed.
    assert_eq!(json["dl"], "http://proxy.test/api/v1/crates");
    assert_eq!(json["api"], proxy.mock_upstream.uri().trim_end_matches('/'));
}
