//! Crate download path: proxy + cache, upstream errors, and the
//! `--restrict-downloads` age-gate (the index hides too-new versions, but direct
//! downloads are only blocked when restriction is on).

mod common;

use std::time::SystemTime;

use common::{ndjson, TestProxy, CRATE_BYTES, OLD, TOO_NEW};

fn dl_path(name: &str, version: &str) -> String {
    format!("/api/v1/crates/{name}/{version}/download")
}

#[tokio::test]
async fn download_proxies_then_caches() {
    let proxy = TestProxy::builder().start().await;
    proxy.mock_crate("serde", "1.0.0", CRATE_BYTES).await;

    let resp = proxy.download("serde", "1.0.0").await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["content-type"], "application/x-tar");
    assert_eq!(resp.bytes().await.unwrap().as_ref(), CRATE_BYTES);

    // Bytes cached on disk byte-for-byte.
    let path = proxy.crate_cache_path("serde", "1.0.0");
    assert_eq!(std::fs::read(&path).unwrap(), CRATE_BYTES);

    // Second download served from disk — upstream hit only once.
    assert_eq!(proxy.download("serde", "1.0.0").await.status(), 200);
    assert_eq!(proxy.upstream_hits(&dl_path("serde", "1.0.0")).await, 1);
}

#[tokio::test]
async fn download_forwards_upstream_404() {
    let proxy = TestProxy::builder().start().await;
    proxy.mock_crate_status("serde", "9.9.9", 404).await;

    assert_eq!(proxy.download("serde", "9.9.9").await.status(), 404);
}

#[tokio::test]
async fn too_new_is_downloadable_without_restrict() {
    // Cooldown hides 2.0.0 from the index, but a direct download still works
    // when --restrict-downloads is off.
    let proxy = TestProxy::builder().cooldown_days(7).start().await;
    proxy.mock_crate("serde", "2.0.0", CRATE_BYTES).await;

    assert_eq!(proxy.download("serde", "2.0.0").await.status(), 200);
}

#[tokio::test]
async fn restrict_blocks_too_new_allows_old() {
    let proxy = TestProxy::builder()
        .cooldown_days(7)
        .restrict_downloads()
        .start()
        .await;
    // Pristine index on disk carries the pubtimes the gate reads.
    let index = ndjson("serde", &[("1.0.0", OLD), ("2.0.0", TOO_NEW)]);
    proxy.seed_index_file("serde", &index, SystemTime::now());
    proxy.mock_crate("serde", "1.0.0", CRATE_BYTES).await;

    // Too-new -> refused before any upstream download.
    assert_eq!(proxy.download("serde", "2.0.0").await.status(), 403);
    assert_eq!(proxy.upstream_hits(&dl_path("serde", "2.0.0")).await, 0);
    // Old enough -> allowed and proxied.
    assert_eq!(proxy.download("serde", "1.0.0").await.status(), 200);
}

#[tokio::test]
async fn restrict_is_fail_closed() {
    let proxy = TestProxy::builder()
        .cooldown_days(7)
        .restrict_downloads()
        .start()
        .await;

    // Index never cached -> cannot verify pubtime -> refused.
    assert_eq!(proxy.download("tokio", "1.0.0").await.status(), 403);

    // Index cached but the requested version isn't in it -> also refused.
    proxy.seed_index_file("serde", &ndjson("serde", &[("1.0.0", OLD)]), SystemTime::now());
    assert_eq!(proxy.download("serde", "2.0.0").await.status(), 403);
}

#[tokio::test]
async fn restrict_is_noop_when_cooldown_disabled() {
    let proxy = TestProxy::builder().restrict_downloads().start().await; // cooldown = 0
    proxy.mock_crate("serde", "2.0.0", CRATE_BYTES).await;

    assert_eq!(proxy.download("serde", "2.0.0").await.status(), 200);
}

#[tokio::test]
async fn download_transport_failure_is_502() {
    let proxy = TestProxy::builder().dead_upstream().start().await;
    // Not cached, upstream refused -> transport error mapped to 502.
    assert_eq!(proxy.download("serde", "1.0.0").await.status(), 502);
}

#[tokio::test]
async fn oversized_crate_is_507() {
    // MAX_CRATE_SIZE is 16 MiB; one byte over the cap must be refused, not
    // truncated. (Abuse: an upstream/forged response trying to exhaust memory.)
    const MAX_CRATE_SIZE: usize = 0x100_0000;
    let proxy = TestProxy::builder().start().await;
    proxy
        .mock_crate("serde", "1.0.0", &vec![0u8; MAX_CRATE_SIZE + 1])
        .await;

    assert_eq!(proxy.download("serde", "1.0.0").await.status(), 507);
    // Nothing partial was cached.
    assert!(!proxy.crate_cache_path("serde", "1.0.0").exists());
}

#[tokio::test]
async fn restrict_fail_closed_on_non_utf8_index() {
    let proxy = TestProxy::builder()
        .cooldown_days(7)
        .restrict_downloads()
        .start()
        .await;
    // A cached index entry that isn't valid UTF-8 can't be verified -> refuse.
    proxy.seed_index_bytes("serde", &[0xff, 0xfe, 0x00], SystemTime::now());

    assert_eq!(proxy.download("serde", "1.0.0").await.status(), 403);
}
