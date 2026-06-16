//! Index serve path: caching, conditional GET / ETag, TTL revalidation, and
//! upstream-error handling. Cooldown is left off here (covered separately) so
//! bodies pass through verbatim.

mod common;

use std::fs;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use common::{ndjson, TestProxy, OLD};

const EPOCH_HTTPDATE: &str = "Thu, 01 Jan 1970 00:00:00 GMT";

#[tokio::test]
async fn not_cached_fetches_upstream_and_writes_disk() {
    let proxy = TestProxy::builder().start().await;
    let body = ndjson("serde", &[("1.0.0", OLD)]);
    proxy
        .mock_index("serde", &body, "\"etag123\"", EPOCH_HTTPDATE)
        .await;

    let resp = proxy.get_index("serde", &[]).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), body);

    // Exactly one upstream fetch, and the pristine body landed on disk...
    assert_eq!(
        proxy
            .upstream_hits(&proxy.index_upstream_path("serde"))
            .await,
        1
    );
    let path = proxy.index_cache_path("serde");
    assert_eq!(fs::read_to_string(&path).unwrap(), body);
    // ...with its mtime set from the upstream Last-Modified (epoch here).
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), UNIX_EPOCH);
}

#[tokio::test]
async fn cached_within_ttl_serves_from_disk_without_upstream() {
    let proxy = TestProxy::builder()
        .cache_ttl(Duration::from_secs(3600))
        .start()
        .await;
    let body = ndjson("serde", &[("1.0.0", OLD)]);
    proxy
        .mock_index("serde", &body, "\"etag123\"", EPOCH_HTTPDATE)
        .await;

    assert_eq!(proxy.get_index("serde", &[]).await.status(), 200);
    let second = proxy.get_index("serde", &[]).await;
    assert_eq!(second.status(), 200);
    assert_eq!(second.text().await.unwrap(), body);

    // Second request served from the warm cache — upstream hit only once.
    assert_eq!(
        proxy
            .upstream_hits(&proxy.index_upstream_path("serde"))
            .await,
        1
    );
}

#[tokio::test]
async fn ttl_zero_revalidates_each_request() {
    let proxy = TestProxy::builder().cache_ttl(Duration::ZERO).start().await;
    let body = ndjson("serde", &[("1.0.0", OLD)]);
    proxy
        .mock_index("serde", &body, "\"etag123\"", EPOCH_HTTPDATE)
        .await;
    proxy.mock_index_304("serde", "\"etag123\"").await;

    assert_eq!(proxy.get_index("serde", &[]).await.status(), 200);
    // Expired immediately -> conditional revalidation; upstream 304, body still served.
    let second = proxy.get_index("serde", &[]).await;
    assert_eq!(second.status(), 200);
    assert_eq!(second.text().await.unwrap(), body);

    assert_eq!(
        proxy
            .upstream_hits(&proxy.index_upstream_path("serde"))
            .await,
        2
    );
}

#[tokio::test]
async fn client_revalidation_gets_304() {
    let proxy = TestProxy::builder().start().await;
    let body = ndjson("serde", &[("1.0.0", OLD)]);
    proxy
        .mock_index("serde", &body, "\"etag123\"", EPOCH_HTTPDATE)
        .await;

    let first = proxy.get_index("serde", &[]).await;
    let etag = first.headers()["etag"].to_str().unwrap().to_owned();
    assert_eq!(etag, "\"etag123\"");

    // Client presents the validator it was given -> 304, empty body, no upstream.
    let second = proxy.get_index("serde", &[("if-none-match", &etag)]).await;
    assert_eq!(second.status(), 304);
    assert!(second.text().await.unwrap().is_empty());
    assert_eq!(
        proxy
            .upstream_hits(&proxy.index_upstream_path("serde"))
            .await,
        1
    );
}

#[tokio::test]
async fn client_if_modified_since_gets_304() {
    let proxy = TestProxy::builder().start().await;
    let body = ndjson("serde", &[("1.0.0", OLD)]);
    let last_modified = "Mon, 04 Feb 2019 06:09:26 GMT"; // 2019-02-04 is a Monday
    proxy
        .mock_index("serde", &body, "\"etag123\"", last_modified)
        .await;

    let first = proxy.get_index("serde", &[]).await;
    assert_eq!(first.headers()["last-modified"], last_modified);

    // Revalidate via If-Modified-Since (no ETag) -> 304 from the warm cache.
    let second = proxy
        .get_index("serde", &[("if-modified-since", last_modified)])
        .await;
    assert_eq!(second.status(), 304);
    assert!(second.text().await.unwrap().is_empty());
    assert_eq!(
        proxy
            .upstream_hits(&proxy.index_upstream_path("serde"))
            .await,
        1
    );
}

#[tokio::test]
async fn expired_with_client_validator_relays_upstream_304() {
    // TTL-expired entry forces a conditional upstream fetch; upstream answers
    // 304 and the client (which sent a matching validator) gets a 304 too.
    let proxy = TestProxy::builder().cache_ttl(Duration::ZERO).start().await;
    let body = ndjson("serde", &[("1.0.0", OLD)]);
    proxy
        .mock_index("serde", &body, "\"etag123\"", EPOCH_HTTPDATE)
        .await;
    proxy.mock_index_304("serde", "\"etag123\"").await;

    proxy.get_index("serde", &[]).await; // populate
    let resp = proxy
        .get_index("serde", &[("if-none-match", "\"etag123\"")])
        .await;
    assert_eq!(resp.status(), 304);
    assert!(resp.text().await.unwrap().is_empty());
    // First populate + this revalidation = two upstream requests.
    assert_eq!(
        proxy
            .upstream_hits(&proxy.index_upstream_path("serde"))
            .await,
        2
    );
}

#[tokio::test]
async fn stale_cache_served_when_upstream_unreachable() {
    let proxy = TestProxy::builder().dead_upstream().start().await;
    let body = ndjson("serde", &[("1.0.0", OLD)]);
    // Pre-seed a pristine entry; upstream is a refused port.
    proxy.seed_index_file("serde", &body, SystemTime::now());

    let resp = proxy.get_index("serde", &[]).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), body);
}

#[tokio::test]
async fn no_cache_and_unreachable_upstream_is_502() {
    let proxy = TestProxy::builder().dead_upstream().start().await;
    // Nothing seeded, nothing cached, upstream dead.
    assert_eq!(proxy.get_index("serde", &[]).await.status(), 502);
}

#[tokio::test]
async fn upstream_error_status_is_forwarded() {
    let proxy = TestProxy::builder().start().await;
    proxy.mock_index_status("serde", 404).await;

    assert_eq!(proxy.get_index("serde", &[]).await.status(), 404);
    assert_eq!(
        proxy
            .upstream_hits(&proxy.index_upstream_path("serde"))
            .await,
        1
    );
}
