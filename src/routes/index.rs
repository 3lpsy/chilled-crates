//! `GET /index/<path>` — proxied, cached, age-gated sparse-index entries.

use std::path::Path;

use axum::{
    body::Body,
    extract::{Path as UrlPath, State},
    http::{header, HeaderMap},
    response::Response,
};
use bytes::Bytes;
use log::{debug, error, warn};

use crate::cache::{
    cache_fetch_index_entry, cache_store_index_entry, cache_try_find_index_entry, IndexEntry,
    MEMO_BUCKET_SECS,
};
use crate::config::Config;
use crate::constants::{CRATES_API_PATH, INDEX_CTYPE, MAX_INDEX_SIZE};
use crate::cooldown;
use crate::http::{error_response, format_json_error, json_response, read_capped, FetchError};
use crate::server::AppState;

/// Registry configuration file endpoint path (at the sparse-index root).
const CONFIG_JSON_ENDPOINT: &str = "config.json";

/// Checks for the registry configuration file download endpoint.
fn is_config_json_url(index_url: &str) -> bool {
    index_url == CONFIG_JSON_ENDPOINT
}

/// Dynamically generates the registry `config.json`, pointing crate downloads
/// at this proxy server.
fn gen_config_json_file(config: &Config) -> String {
    // Generate the crate download API URL pointing to this same proxy server.
    let dl_url = config
        .proxy_url
        .join(CRATES_API_PATH)
        .expect("invalid proxy server URL");

    // Cargo can not handle trailing slashes in `config.json`.
    let dl = dl_url.as_str().trim_end_matches('/');
    let api = config.upstream_url.as_str().trim_end_matches('/');

    format!(r#"{{"dl":"{dl}","api":"{api}"}}"#)
}

/// Registry index entry download result.
struct IndexResponse {
    /// Index entry plus updated response metadata (etag / last-modified).
    entry: IndexEntry,
    /// Upstream HTTP response status code.
    status: u16,
    /// Upstream HTTP response body.
    data: Vec<u8>,
}

/// The source-content validator used as a memo key and for the weak ETag.
fn entry_validator(entry: &IndexEntry) -> String {
    entry
        .etag()
        .map(ToOwned::to_owned)
        .or_else(|| entry.last_modified())
        .unwrap_or_default()
}

// ETag rewriting for filtered entries.
//
// Serving a *filtered* body under the upstream's strong ETag is incorrect: the
// bytes no longer match the validator, so a shared cache could mix them up. For
// filtered entries we therefore emit a *weak*, cooldown-tagged ETag derived
// from the upstream one, and strip that tag back off when a client revalidates
// (so the upstream conditional GET still uses the real validator).

/// Strips an optional weak prefix and surrounding quotes from an ETag value.
fn etag_inner(value: &str) -> &str {
    value.strip_prefix("W/").unwrap_or(value).trim_matches('"')
}

/// Builds the client-facing weak ETag for a filtered entry from the upstream
/// strong ETag and the cooldown window.
fn filtered_etag(upstream_etag: &str, cooldown_secs: u64) -> String {
    format!("W/\"{}.cd{cooldown_secs}\"", etag_inner(upstream_etag))
}

/// Recovers the upstream strong ETag form from a client `If-None-Match` value,
/// undoing [`filtered_etag`] so upstream revalidation matches.
fn unmark_etag(client_value: &str) -> String {
    let inner = etag_inner(client_value);
    let base = match inner.rsplit_once(".cd") {
        Some((base, digits))
            if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) =>
        {
            base
        }
        _ => inner,
    };
    format!("\"{base}\"")
}

/// Extracts the cooldown window (seconds) encoded in a client ETag marker, or
/// `None` if the ETag is unmarked (i.e. was issued for an unfiltered entry).
fn etag_window(client_value: &str) -> Option<u64> {
    let (_, digits) = etag_inner(client_value).rsplit_once(".cd")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// The cooldown window (seconds) this crate is served under right now, or
/// `None` when it is served unfiltered (cooldown off, or crate overridden).
fn serve_window(config: &Config, name: &str) -> Option<u64> {
    config.cutoff_for(name).map(|_| config.cooldown.as_secs())
}

/// Attaches the cooldown-aware cache validators to an index response builder.
///
/// For a filtered entry: a weak, cooldown-tagged ETag and no `Last-Modified`
/// (the filtered representation is time-dependent). Otherwise: the upstream
/// ETag and `Last-Modified` verbatim.
fn with_index_validators(
    mut builder: axum::http::response::Builder,
    entry: &IndexEntry,
    filtered_secs: Option<u64>,
) -> axum::http::response::Builder {
    match filtered_secs {
        Some(secs) => {
            if let Some(etag) = entry.etag() {
                builder = builder.header(header::ETAG, filtered_etag(etag, secs));
            }
        }
        None => {
            if let Some(etag) = entry.etag() {
                builder = builder.header(header::ETAG, etag);
            }
            if let Some(last_modified) = entry.last_modified() {
                builder = builder.header(header::LAST_MODIFIED, last_modified);
            }
        }
    }
    builder
}

/// Builds an index `304 Not Modified` response (no body).
fn index_not_modified(entry: &IndexEntry, config: &Config, name: &str) -> Response {
    with_index_validators(
        Response::builder().status(304),
        entry,
        serve_window(config, name),
    )
    .body(Body::empty())
    .expect("valid 304 response")
}

/// Builds an index `200 OK` response, age-gating (and memoizing) the body when
/// the crate is subject to cooldown.
async fn index_ok(entry: &IndexEntry, data: Vec<u8>, state: &AppState, name: &str) -> Response {
    let config = &state.config;

    let Some(cutoff) = config.cutoff_for(name) else {
        // Unfiltered: serve verbatim with the upstream validators.
        return with_index_validators(
            Response::builder()
                .status(200)
                .header(header::CONTENT_TYPE, INDEX_CTYPE),
            entry,
            None,
        )
        .body(Body::from(data))
        .expect("valid index response");
    };

    let bucket = cutoff / MEMO_BUCKET_SECS;
    let validator = entry_validator(entry);

    let body = if let Some(cached) = state.memo.get(name, &validator, bucket) {
        cached
    } else {
        // Filter off the async workers; the scan is cheap but entries can be
        // large, and we never want to stall the runtime.
        let filtered = match tokio::task::spawn_blocking(move || {
            cooldown::filter_index(&data, cutoff)
        })
        .await
        {
            Ok(filtered) => Bytes::from(filtered),
            // A panic in the filter must not be served as an empty (and thus
            // version-less) 200 — fail loudly with a 500 instead.
            Err(err) => {
                error!("cooldown: index filter task failed for {name}: {err}");
                return error_response(500);
            }
        };
        state
            .memo
            .put(name.to_owned(), validator, bucket, filtered.clone());
        filtered
    };

    with_index_validators(
        Response::builder()
            .status(200)
            .header(header::CONTENT_TYPE, INDEX_CTYPE),
        entry,
        Some(config.cooldown.as_secs()),
    )
    .body(Body::from(body))
    .expect("valid index response")
}

// Blocking cache operations, run on the blocking pool.

/// Reads a cached index entry file off the blocking thread pool.
async fn cache_read_index(dir: &Path, entry: &IndexEntry) -> Option<Vec<u8>> {
    let dir = dir.to_path_buf();
    let entry = entry.clone();
    tokio::task::spawn_blocking(move || cache_fetch_index_entry(&dir, &entry))
        .await
        .ok()
        .flatten()
}

/// Stores an index entry file off the blocking thread pool.
async fn cache_write_index(dir: &Path, entry: &IndexEntry, data: &[u8]) {
    let dir = dir.to_path_buf();
    let entry = entry.clone();
    let data = data.to_vec();
    let _ = tokio::task::spawn_blocking(move || cache_store_index_entry(&dir, &entry, &data)).await;
}

/// Recreates index metadata from a cache file's mtime off the blocking pool.
async fn cache_find_index(dir: &Path, name: &str) -> Option<IndexEntry> {
    let dir = dir.to_path_buf();
    let name = name.to_owned();
    tokio::task::spawn_blocking(move || cache_try_find_index_entry(&dir, &name))
        .await
        .ok()
        .flatten()
}

// Upstream fetch.

/// Downloads a sparse index entry from the upstream registry, sending the
/// conditional-request headers carried by `entry`.
async fn download_index_entry(
    state: &AppState,
    mut entry: IndexEntry,
) -> Result<IndexResponse, FetchError> {
    let url = state.config.index_url.join(&entry.to_index_url()).unwrap();

    // Pin identity encoding: a compressed body would fail the UTF-8 check and
    // pass through unfiltered, silently disabling age-gating.
    let mut request = state
        .client
        .get(url)
        .header(header::ACCEPT_ENCODING, "identity");
    if let Some(etag) = entry.etag() {
        request = request.header(header::IF_NONE_MATCH, etag);
    } else if let Some(last_modified) = entry.last_modified() {
        request = request.header(header::IF_MODIFIED_SINCE, last_modified);
    }

    let mut response = request.send().await.map_err(FetchError::Http)?;
    let status = response.status().as_u16();

    if let Some(etag) = response
        .headers()
        .get(header::ETAG)
        .and_then(|v| v.to_str().ok())
    {
        entry.set_etag(etag);
    }
    if let Some(last_modified) = response
        .headers()
        .get(header::LAST_MODIFIED)
        .and_then(|v| v.to_str().ok())
    {
        entry.set_last_modified(last_modified);
    }
    entry.set_last_updated();

    let data = read_capped(&mut response, MAX_INDEX_SIZE).await?;

    Ok(IndexResponse {
        entry,
        status,
        data,
    })
}

/// Fetches an index entry from upstream (or stale cache) and serves it.
///
/// `window_ok` indicates the client's cached copy (if any) was filtered at the
/// same cooldown window we serve at now; only then is a `304` safe.
async fn forward_index(
    state: &AppState,
    entry: IndexEntry,
    cached_entry: Option<IndexEntry>,
    name: &str,
    window_ok: bool,
) -> Response {
    // Send the conditional headers from the cached metadata when we have it.
    let req_entry = cached_entry.unwrap_or_else(|| entry.clone());

    let response = match download_index_entry(state, req_entry).await {
        Ok(response) => response,
        Err(err) => {
            // Transport failure: serve a possibly-stale cached copy if present.
            if let Some(data) = cache_read_index(&state.config.index_dir, &entry).await {
                warn!("proxy: forwarding possibly stale cached index for {name}: {err}");
                return index_ok(&entry, data, state, name).await;
            }
            error!("fetch: index connection failed for {name}: {err}");
            return json_response(502, format_json_error(err));
        }
    };

    match response.status {
        200 => {
            debug!("fetch: successfully got index entry for {name}");
            cache_write_index(&state.config.index_dir, &response.entry, &response.data).await;
            state.metadata.store(&response.entry);

            if window_ok && response.entry.is_equivalent(&entry) {
                index_not_modified(&response.entry, &state.config, name)
            } else {
                index_ok(&response.entry, response.data, state, name).await
            }
        }
        304 => {
            debug!("fetch: cached index entry for {name} is up to date");
            state.metadata.store(&response.entry);

            if window_ok && response.entry.is_equivalent(&entry) {
                index_not_modified(&response.entry, &state.config, name)
            } else if let Some(data) = cache_read_index(&state.config.index_dir, &entry).await {
                index_ok(&response.entry, data, state, name).await
            } else {
                error!("cache: lost index cache file for {name}");
                state.metadata.invalidate(&entry);
                error_response(503)
            }
        }
        code => {
            // Forward other upstream statuses (e.g. 404) verbatim.
            warn!("fetch: upstream returned HTTP status {code} for {name}");
            json_response(code, String::from_utf8_lossy(&response.data).into_owned())
        }
    }
}

/// Handles a sparse registry index request: `GET /index/<path>`.
pub(crate) async fn handle_index(
    State(state): State<AppState>,
    UrlPath(path): UrlPath<String>,
    headers: HeaderMap,
) -> Response {
    if is_config_json_url(&path) {
        debug!("proxy: sending registry config file");
        return json_response(200, gen_config_json_file(&state.config));
    }

    let Some(mut index_entry) = IndexEntry::try_from_index_url(&path) else {
        warn!("proxy: malformed registry index path: {path}");
        return error_response(404);
    };
    let name = index_entry.name().to_owned();

    // Extract client cache-control headers, undoing our cooldown ETag marker so
    // the upstream conditional GET uses the real validator. The marker's window
    // is remembered: a `304` is only safe if it matches the window we serve at
    // now (so enabling/changing cooldown invalidates stale client copies).
    let mut client_window = None;
    if let Some(inm) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
    {
        index_entry.set_etag(&unmark_etag(inm));
        client_window = etag_window(inm);
    } else if let Some(ims) = headers
        .get(header::IF_MODIFIED_SINCE)
        .and_then(|v| v.to_str().ok())
    {
        index_entry.set_last_modified(ims);
    }

    let window_ok = client_window == serve_window(&state.config, &name);

    // Serve from cache when the metadata cache is warm and unexpired.
    if let Some(cached_entry) = state.metadata.fetch(&name) {
        if cached_entry.is_expired_with_ttl(&state.config.cache_ttl) {
            debug!("proxy: index cache expired for {name}, refreshing...");
            return forward_index(&state, index_entry, Some(cached_entry), &name, window_ok).await;
        }

        if window_ok && cached_entry.is_equivalent(&index_entry) {
            debug!("proxy: index metadata cache hit for {name}");
            return index_not_modified(&cached_entry, &state.config, &name);
        }

        if let Some(data) = cache_read_index(&state.config.index_dir, &index_entry).await {
            debug!("proxy: index data cache hit for {name}");
            return index_ok(&cached_entry, data, &state, &name).await;
        }
    }

    // Recreate metadata from the cache file mtime, then fetch from upstream.
    let mtimed_entry = cache_find_index(&state.config.index_dir, &name).await;
    forward_index(&state, index_entry, mtimed_entry, &name, window_ok).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etag_marker_round_trip() {
        let upstream = "\"abc123\"";
        let client = filtered_etag(upstream, 604_800);
        assert_eq!(client, "W/\"abc123.cd604800\"");
        // A client revalidation must recover the upstream strong validator.
        assert_eq!(unmark_etag(&client), "\"abc123\"");
        // A raw (unmarked) etag round-trips unchanged.
        assert_eq!(unmark_etag(upstream), "\"abc123\"");
        // The window is recoverable for the 304-safety check; unmarked = None.
        assert_eq!(etag_window(&client), Some(604_800));
        assert_eq!(etag_window(upstream), None);
    }

    #[test]
    fn etag_inner_strips_weak_prefix_and_quotes() {
        assert_eq!(etag_inner("W/\"abc\""), "abc");
        assert_eq!(etag_inner("\"abc\""), "abc");
        assert_eq!(etag_inner("abc"), "abc");
    }

    #[test]
    fn unmark_etag_accepts_weak_input() {
        // A weak marked etag (as actually issued) recovers the strong form.
        assert_eq!(unmark_etag("W/\"abc.cd604800\""), "\"abc\"");
        // A weak *unmarked* etag has no `.cd` suffix to strip.
        assert_eq!(unmark_etag("W/\"abc\""), "\"abc\"");
    }

    #[test]
    fn unmark_etag_strips_only_trailing_marker() {
        // rsplit_once on `.cd` peels just the final marker, so an etag whose own
        // bytes contain `.cd...` is preserved up to the real trailing marker.
        assert_eq!(unmark_etag("W/\"v.cd1.cd2\""), "\"v.cd1\"");
    }

    #[test]
    fn etag_window_rejects_non_digit_marker() {
        // A `.cd` followed by non-digits is not a valid window marker.
        assert_eq!(etag_window("W/\"abc.cdNOPE\""), None);
        assert_eq!(etag_window("W/\"abc.cd\""), None);
    }

    #[test]
    fn filtered_etag_with_zero_window() {
        // A zero window still produces a distinct, recoverable marker.
        let client = filtered_etag("\"abc\"", 0);
        assert_eq!(client, "W/\"abc.cd0\"");
        assert_eq!(etag_window(&client), Some(0));
        assert_eq!(unmark_etag(&client), "\"abc\"");
    }
}
