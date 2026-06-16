//! `--cooldown-overrides`: crates exempted from age-gating are served unfiltered
//! on the index path and bypass the `--restrict-downloads` gate. Matching is
//! case-insensitive.

mod common;

use common::{ndjson, TestProxy, CRATE_BYTES, OLD, TOO_NEW};

#[tokio::test]
async fn override_crate_is_served_unfiltered() {
    let proxy = TestProxy::builder()
        .cooldown_days(7)
        .override_crate("serde")
        .start()
        .await;
    let body = ndjson("serde", &[("1.0.0", OLD), ("2.0.0", TOO_NEW)]);
    proxy
        .mock_index(
            "serde",
            &body,
            "\"etag123\"",
            "Thu, 01 Jan 1970 00:00:00 GMT",
        )
        .await;

    let resp = proxy.get_index("serde", &[]).await;
    assert_eq!(
        resp.headers()["etag"],
        "\"etag123\"",
        "strong ETag, unfiltered"
    );
    let text = resp.text().await.unwrap();
    assert!(
        text.contains(r#""vers":"2.0.0""#),
        "exempt crate keeps too-new version"
    );
}

#[tokio::test]
async fn override_match_is_case_insensitive() {
    // Override list holds lower-cased names; a mixed-case request for the same
    // crate is still matched (and thus served unfiltered).
    let proxy = TestProxy::builder()
        .cooldown_days(7)
        .override_crate("serde")
        .start()
        .await;
    // Upstream (like crates.io) serves at the lowercased path; the client makes
    // a mixed-case request, which the proxy normalizes when fetching/caching.
    let body = ndjson("serde", &[("1.0.0", OLD), ("2.0.0", TOO_NEW)]);
    proxy
        .mock_index(
            "serde",
            &body,
            "\"etag123\"",
            "Thu, 01 Jan 1970 00:00:00 GMT",
        )
        .await;

    let text = proxy.get_index("Serde", &[]).await.text().await.unwrap();
    assert!(
        text.contains(r#""vers":"2.0.0""#),
        "case-insensitive override exempts crate"
    );
}

#[tokio::test]
async fn override_exempts_download_under_restrict() {
    let proxy = TestProxy::builder()
        .cooldown_days(7)
        .restrict_downloads()
        .override_crate("serde")
        .start()
        .await;
    proxy.mock_crate("serde", "2.0.0", CRATE_BYTES).await;

    // Overridden crate bypasses the gate even for a too-new version.
    assert_eq!(proxy.download("serde", "2.0.0").await.status(), 200);
}
