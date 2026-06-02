//! Structural request validation: well-formed routes with the wrong *shape* are
//! rejected with `404` before any upstream request. (Charset/SSRF injection is
//! covered separately in `ssrf.rs`.)

mod common;

use common::TestProxy;

#[tokio::test]
async fn malformed_index_paths_are_rejected() {
    let proxy = TestProxy::builder().start().await;

    // Too many segments, and a traversal segment.
    for path in ["/index/a/b/c/d", "/index/1/.."] {
        assert_eq!(proxy.get(path).await.status(), 404, "path: {path}");
    }
    assert_eq!(proxy.upstream_total().await, 0);
}

#[tokio::test]
async fn malformed_download_paths_are_rejected() {
    let proxy = TestProxy::builder().start().await;

    // Missing the `/download` suffix; wrong segment count.
    for path in [
        "/api/v1/crates/serde/1.0.0",
        "/api/v1/crates/serde/download",
        "/api/v1/crates/a/b/c/download",
    ] {
        assert_eq!(proxy.get(path).await.status(), 404, "path: {path}");
    }
    assert_eq!(proxy.upstream_total().await, 0);
}

#[tokio::test]
async fn unknown_route_is_404() {
    let proxy = TestProxy::builder().start().await;
    assert_eq!(proxy.get("/nope").await.status(), 404);
    assert_eq!(proxy.get("/api/v2/whatever").await.status(), 404);
}
