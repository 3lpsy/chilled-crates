//! Caching HTTP proxy server for the `crates.io` registry, with sparse-index
//! age-gating (cooldown)
//! ===========================================================================
//!
//! `chilled-crates` implements transparent caching for both the sparse registry
//! index at <https://index.crates.io/> and the static crate file download
//! server, and additionally hides crate versions newer than a configurable
//! cooldown window to blunt supply-chain attacks (see [`cooldown`]).
//!
//! Two HTTP endpoints are served:
//!
//! 1. `GET /index/.../{crate}` — proxied to <https://index.crates.io/> and
//!    cached as JSON text files on disk. Served bodies are age-gated unless the
//!    crate is exempt (see `--cooldown-overrides`).
//!
//! 2. `GET /api/v1/crates/{crate}/{version}/download` — proxied to
//!    <https://crates.io/> and cached as `.crate` files on disk. Crate content
//!    is never modified — only index metadata is filtered.
//!
//! The download request for the `config.json` file at the sparse index root is
//! served with a generated replacement pointing crate downloads at this proxy.
//!
//! The server is built on `axum` + `tokio` (async) with a shared `reqwest`
//! client. Blocking filesystem-cache operations run on the blocking thread
//! pool, and CPU-bound age-gating runs off the async workers and is memoized.

mod config_json;
mod cooldown;
mod crate_info;
mod file_cache;
mod index_entry;
mod metadata_cache;
mod valid;

use std::collections::HashMap;
use std::collections::HashSet;
use std::env;
use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::{
    Router,
    body::Body,
    extract::{Path as UrlPath, State},
    http::{HeaderMap, header},
    response::Response,
    routing::get,
};
use bytes::Bytes;
use pico_args::Arguments;

use env_logger::{Builder as LogBuilder, Env as LogEnv};
use log::{debug, error, info, warn};

use url::Url;

use crate::config_json::{gen_config_json_file, is_config_json_url};
use crate::crate_info::CrateInfo;
use crate::file_cache::{
    cache_fetch_crate, cache_fetch_index_entry, cache_store_crate, cache_store_index_entry,
    cache_try_find_index_entry,
};
use crate::index_entry::IndexEntry;
use crate::metadata_cache::{
    metadata_fetch_index_entry, metadata_invalidate_index_entry, metadata_store_index_entry,
};

/// Default listen address and port
const LISTEN_ADDRESS: &str = "0.0.0.0:3080";

/// Upstream `crates.io` registry index URL
const INDEX_CRATES_IO_URL: &str = "https://index.crates.io/";

/// Upstream `crates.io` registry URL
const CRATES_IO_URL: &str = "https://crates.io/";

/// Default external URL of this proxy server
const DEFAULT_PROXY_URL: &str = "http://localhost:3080/";

/// Crates download API path
const CRATES_API_PATH: &str = "/api/v1/crates/";

/// Default crate files cache directory path
const DEFAULT_CACHE_DIR: &str = "/var/cache/chilled-crates";

/// Default index cache entry Time-to-Live in seconds
const DEFAULT_CACHE_TTL_SECS: u64 = 3600;

/// Limit the crate file download size to 16 MiB
const MAX_CRATE_SIZE: usize = 0x100_0000;

/// Limit the sparse-index entry download size to 64 MiB
const MAX_INDEX_SIZE: usize = 0x400_0000;

/// HTTP Content-Type of the registry index entry JSON file
const INDEX_CTYPE: &str = "text/plain";

/// HTTP Content-Type of the crate package file
const CRATE_CTYPE: &str = "application/x-tar";

/// HTTP Content-Type of the crates API JSON response
const JSON_CTYPE: &str = "application/json; charset=utf-8";

/// Program version tag: `"<major>.<minor>.<patch>"`
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// HTTP client User Agent string
const HTTP_USER_AGENT: &str = concat!("chilled-crates/", env!("CARGO_PKG_VERSION"));

/// Granularity (seconds) of the filtered-output memo cutoff bucket.
///
/// The age-gating cutoff advances every second, but the *filtered output* only
/// changes when a version crosses the boundary. Bucketing the cutoff to the
/// hour makes the memo key stable, so repeated hits within the hour reuse the
/// filtered bytes; the cost is at most ~1h of aging-in jitter, irrelevant for a
/// day-scale cooldown.
const MEMO_BUCKET_SECS: u64 = 3600;

/// Maximum number of crates held in the filtered-output memo before it is
/// cleared. Each crate keeps at most one entry, so this bounds memory use.
const MEMO_MAX_ENTRIES: usize = 8192;

/// Proxy server configuration (immutable after startup).
#[derive(Debug, Clone)]
struct ProxyConfig {
    /// Upstream registry index URL (defaults to [`INDEX_CRATES_IO_URL`])
    index_url: Url,

    /// Upstream crate download URL (defaults to [`CRATES_IO_URL`])
    upstream_url: Url,

    /// External URL of this proxy server (defaults to [`DEFAULT_PROXY_URL`])
    proxy_url: Url,

    /// Registry index cache directory
    index_dir: PathBuf,

    /// Crate files cache directory
    crates_dir: PathBuf,

    /// Index entry cache Time-to-Live (defaults to [`DEFAULT_CACHE_TTL_SECS`])
    cache_ttl: Duration,

    /// Sparse-index age-gating window; a zero duration disables filtering.
    cooldown: Duration,

    /// Lower-cased crate names exempt from age-gating (served unfiltered).
    overrides: Arc<HashSet<String>>,

    /// When set, the `/` endpoint reports the cached crates and timestamps;
    /// otherwise it returns only a minimal liveness status.
    show_metrics: bool,
}

/// Shared application state passed to every request handler.
#[derive(Clone)]
struct AppState {
    /// Immutable server configuration.
    config: Arc<ProxyConfig>,
    /// Shared connection-pooling HTTP client.
    client: reqwest::Client,
    /// Memoized filtered index bodies.
    memo: Arc<FilteredMemo>,
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

/// One memoized filtered index body, tagged with the source identity and the
/// cooldown bucket it was produced for.
struct MemoEntry {
    /// Source content validator (upstream etag or last-modified).
    validator: String,
    /// Cutoff bucket (cutoff / [`MEMO_BUCKET_SECS`]) the body was filtered for.
    bucket: u64,
    /// The filtered bytes (cheap to clone).
    data: Bytes,
}

/// Bounded, concurrent cache of filtered index bodies keyed by crate name.
///
/// At most one entry per crate is retained; a lookup hits only when both the
/// source validator and the cutoff bucket still match, so stale or
/// time-shifted output is never reused.
struct FilteredMemo {
    inner: RwLock<HashMap<String, MemoEntry>>,
}

impl FilteredMemo {
    /// Creates an empty memo.
    fn new() -> Self {
        FilteredMemo {
            inner: RwLock::new(HashMap::new()),
        }
    }

    /// Returns the memoized filtered body for `name` if it was produced from
    /// the same source `validator` and cutoff `bucket`.
    fn get(&self, name: &str, validator: &str, bucket: u64) -> Option<Bytes> {
        let map = self.inner.read().unwrap();
        let entry = map.get(name)?;
        (entry.validator == validator && entry.bucket == bucket).then(|| entry.data.clone())
    }

    /// Stores the filtered body for `name`, evicting everything if the memo is
    /// full and this is a new crate (keeps memory bounded).
    fn put(&self, name: String, validator: String, bucket: u64, data: Bytes) {
        let mut map = self.inner.write().unwrap();
        if map.len() >= MEMO_MAX_ENTRIES && !map.contains_key(&name) {
            debug!("memo: cleared filtered-index memo at capacity");
            map.clear();
        }
        map.insert(
            name,
            MemoEntry {
                validator,
                bucket,
                data,
            },
        );
    }
}

/// Current age-gating cutoff (unix seconds), or `None` when filtering is off.
fn cooldown_cutoff(cooldown: Duration) -> Option<u64> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    cooldown::cutoff_from(now, cooldown)
}

/// Returns the cutoff to age-gate this crate by, or `None` if it should be
/// served unfiltered (cooldown disabled, or the crate is in the override set).
fn filter_cutoff(config: &ProxyConfig, name: &str) -> Option<u64> {
    if config.overrides.contains(&name.to_ascii_lowercase()) {
        return None;
    }
    cooldown_cutoff(config.cooldown)
}

/// The source-content validator used as a memo key and for the weak ETag.
fn entry_validator(entry: &IndexEntry) -> String {
    entry
        .etag()
        .map(ToOwned::to_owned)
        .or_else(|| entry.last_modified())
        .unwrap_or_default()
}

// --- ETag rewriting for filtered entries ------------------------------------
//
// Serving a *filtered* body under the upstream's strong ETag is incorrect: the
// bytes no longer match the validator, so a shared cache could mix them up. For
// filtered entries we therefore emit a *weak*, cooldown-tagged ETag derived
// from the upstream one, and strip that tag back off when a client revalidates
// (so the upstream conditional GET still uses the real validator).

/// Strips an optional weak prefix and surrounding quotes from an ETag value.
fn etag_inner(value: &str) -> &str {
    value
        .strip_prefix("W/")
        .unwrap_or(value)
        .trim_matches('"')
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
        Some((base, digits)) if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) => {
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
fn serve_window(config: &ProxyConfig, name: &str) -> Option<u64> {
    filter_cutoff(config, name).map(|_| config.cooldown.as_secs())
}

// --- Response builders -------------------------------------------------------

/// Builds a plain-text error response with an empty body.
fn error_response(status: u16) -> Response {
    Response::builder()
        .status(status)
        .body(Body::empty())
        .expect("valid error response")
}

/// Builds a JSON response (used for `config.json` and forwarded API errors).
fn json_response(status: u16, body: String) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, JSON_CTYPE)
        .body(Body::from(body))
        .expect("valid json response")
}

/// Builds a crate-download response from raw `.crate` bytes.
fn crate_response(data: Bytes) -> Response {
    Response::builder()
        .status(200)
        .header(header::CONTENT_TYPE, CRATE_CTYPE)
        .body(Body::from(data))
        .expect("valid crate response")
}

/// Formats a crates API JSON error body.
fn format_json_error(error: impl Display) -> String {
    format!(r#"{{"errors":[{{"detail":"{error}"}}]}}"#)
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
fn index_not_modified(entry: &IndexEntry, config: &ProxyConfig, name: &str) -> Response {
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

    let Some(cutoff) = filter_cutoff(config, name) else {
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
        let filtered = tokio::task::spawn_blocking(move || cooldown::filter_index(&data, cutoff))
            .await
            .unwrap_or_default();
        let filtered = Bytes::from(filtered);
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

// --- Blocking cache operations, run on the blocking pool ---------------------

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

/// Reads a cached crate file off the blocking thread pool.
async fn cache_read_crate(dir: &Path, info: &CrateInfo) -> Option<Vec<u8>> {
    let dir = dir.to_path_buf();
    let info = info.clone();
    tokio::task::spawn_blocking(move || cache_fetch_crate(&dir, &info))
        .await
        .ok()
        .flatten()
}

// --- Upstream fetches --------------------------------------------------------

/// An upstream fetch failure: either a transport/decode error or a body that
/// exceeded the configured size limit.
enum FetchError {
    /// Connection, TLS, or decode failure.
    Http(reqwest::Error),
    /// Body exceeded the size limit (declared or observed).
    TooLarge,
}

impl Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Http(e) => write!(f, "{e}"),
            FetchError::TooLarge => f.write_str("response body too large"),
        }
    }
}

/// Reads a response body into memory, capped at `max` bytes.
///
/// Rejects up front when `Content-Length` already exceeds the cap, and again
/// while streaming so a chunked (no `Content-Length`) response cannot exhaust
/// memory. Returns [`FetchError::TooLarge`] rather than truncating, so callers
/// never serve a partial body.
async fn read_capped(response: &mut reqwest::Response, max: usize) -> Result<Vec<u8>, FetchError> {
    if let Some(len) = response.content_length() {
        if len as usize > max {
            return Err(FetchError::TooLarge);
        }
    }

    let mut data = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(FetchError::Http)? {
        if data.len().saturating_add(chunk.len()) > max {
            return Err(FetchError::TooLarge);
        }
        data.extend_from_slice(&chunk);
    }
    Ok(data)
}

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

    if let Some(etag) = response.headers().get(header::ETAG).and_then(|v| v.to_str().ok()) {
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

/// Downloads a crate file from the upstream download server.
///
/// On an upstream HTTP error or a transport failure, returns a ready-made
/// error `Response` to forward to the client.
async fn download_crate(state: &AppState, info: &CrateInfo) -> Result<Bytes, Response> {
    let url = state
        .config
        .upstream_url
        .join(CRATES_API_PATH)
        .unwrap()
        .join(&info.to_download_url())
        .unwrap();

    let mut response = state
        .client
        .get(url)
        .send()
        .await
        .map_err(|e| json_response(502, format_json_error(e)))?;

    let status = response.status();
    if !status.is_success() {
        let code = status.as_u16();
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| format_json_error("upstream error"));
        warn!("fetch: upstream returned HTTP status {code} for {info}");
        return Err(json_response(code, body));
    }

    match read_capped(&mut response, MAX_CRATE_SIZE).await {
        Ok(data) => Ok(Bytes::from(data)),
        Err(FetchError::TooLarge) => Err(error_response(507)),
        Err(FetchError::Http(e)) => Err(json_response(502, format_json_error(e))),
    }
}

// --- Request handlers --------------------------------------------------------

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
            metadata_store_index_entry(&response.entry);

            if window_ok && response.entry.is_equivalent(&entry) {
                index_not_modified(&response.entry, &state.config, name)
            } else {
                index_ok(&response.entry, response.data, state, name).await
            }
        }
        304 => {
            debug!("fetch: cached index entry for {name} is up to date");
            metadata_store_index_entry(&response.entry);

            if window_ok && response.entry.is_equivalent(&entry) {
                index_not_modified(&response.entry, &state.config, name)
            } else if let Some(data) = cache_read_index(&state.config.index_dir, &entry).await {
                index_ok(&response.entry, data, state, name).await
            } else {
                error!("cache: lost index cache file for {name}");
                metadata_invalidate_index_entry(&entry);
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
async fn handle_index(
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
    if let Some(inm) = headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) {
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
    if let Some(cached_entry) = metadata_fetch_index_entry(&name) {
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

/// Handles a crate download request: `GET /api/v1/crates/<path>`.
async fn handle_download(State(state): State<AppState>, UrlPath(path): UrlPath<String>) -> Response {
    let Some(crate_info) = CrateInfo::try_from_download_url(&path) else {
        warn!("proxy: unrecognized download API endpoint: {path}");
        return error_response(404);
    };

    if let Some(data) = cache_read_crate(&state.config.crates_dir, &crate_info).await {
        debug!("proxy: local cache hit for {crate_info}");
        return crate_response(Bytes::from(data));
    }

    match download_crate(&state, &crate_info).await {
        Ok(data) => {
            // Store off-thread; `Bytes` clones are cheap (refcounted).
            let dir = state.config.crates_dir.clone();
            let info = crate_info.clone();
            let stored = data.clone();
            let _ =
                tokio::task::spawn_blocking(move || cache_store_crate(&dir, &info, &stored)).await;
            // Cache misses are infrequent (once per crate version), so this is a
            // useful high-level event without polluting the log.
            info!("cache: stored new crate {crate_info} ({} bytes)", data.len());
            crate_response(data)
        }
        Err(response) => response,
    }
}

/// Minimal liveness body returned at `/` when metrics are disabled.
const STATUS_JSON: &str = r#"{"service":"chilled-crates","status":"running"}"#;

/// Handles `GET /`: a liveness/metrics endpoint.
///
/// With `--show-metrics` it reports every cached crate file (name, version,
/// and cache timestamp); otherwise it returns only [`STATUS_JSON`].
async fn handle_home(State(state): State<AppState>) -> Response {
    if !state.config.show_metrics {
        return json_response(200, STATUS_JSON.to_owned());
    }

    let dir = state.config.crates_dir.clone();
    let body = tokio::task::spawn_blocking(move || metrics_json(&dir))
        .await
        .unwrap_or_else(|_| STATUS_JSON.to_owned());
    json_response(200, body)
}

/// Builds the metrics JSON document by scanning the crate file cache.
///
/// Names and versions are restricted to the validated charset, so they are
/// embedded into JSON strings without escaping.
fn metrics_json(crates_dir: &Path) -> String {
    let mut items = scan_cached_crates(crates_dir);
    items.sort();

    let crates: Vec<String> = items
        .iter()
        .map(|(name, version, cached_at)| {
            format!(r#"{{"name":"{name}","version":"{version}","cached_at":{cached_at}}}"#)
        })
        .collect();

    format!(
        r#"{{"service":"chilled-crates","status":"running","cached_count":{},"crates":[{}]}}"#,
        items.len(),
        crates.join(",")
    )
}

/// Scans the crate file cache, returning `(name, version, mtime-unix-secs)` for
/// each cached `.crate` file. Best-effort: unreadable or malformed entries are
/// skipped rather than failing the whole report.
fn scan_cached_crates(crates_dir: &Path) -> Vec<(String, String, u64)> {
    let mut out = Vec::new();
    let Ok(crate_dirs) = std::fs::read_dir(crates_dir) else {
        return out;
    };

    for crate_dir in crate_dirs.flatten() {
        if !crate_dir.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let name = crate_dir.file_name().to_string_lossy().into_owned();
        if !valid::is_crate_name(&name) {
            continue;
        }

        let Ok(files) = std::fs::read_dir(crate_dir.path()) else {
            continue;
        };
        let prefix = format!("{name}-");
        for file in files.flatten() {
            let file_name = file.file_name().to_string_lossy().into_owned();
            let Some(version) = file_name
                .strip_suffix(".crate")
                .and_then(|rest| rest.strip_prefix(&prefix))
            else {
                continue;
            };
            if !valid::is_crate_version(version) {
                continue;
            }
            let cached_at = file
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            out.push((name.clone(), version.to_owned(), cached_at));
        }
    }
    out
}

/// Server listening address.
enum ListenAddress {
    /// IP address + port.
    SocketAddr(String),
    /// Unix domain socket path.
    UnixPath(String),
}

/// Prints the program version banner.
fn version() {
    let build = option_env!("CI_PIPELINE_ID");
    let rev = option_env!("CI_COMMIT_SHORT_SHA");
    let tag = option_env!("CI_COMMIT_REF_NAME");

    if let (Some(build), Some(rev), Some(tag)) = (build, rev, tag) {
        println!("chilled-crates {VERSION}+{build}.g{rev}.{tag}");
    } else {
        println!("chilled-crates {VERSION}");
    }
}

/// Prints the program invocation help page.
fn usage() {
    println!("Usage:\n    chilled-crates [options]\n");
    println!("Options:");
    println!("    -v, --verbose              raise log level (-v debug, -vv trace)");
    println!("    -l, --log-level LEVEL      log level: error|warn|info|debug|trace|off (info)");
    println!("    -m, --show-metrics         report cached crates at the `/` endpoint");
    println!("    -h, --help                 print help and exit");
    println!("    -V, --version              print version and exit");
    println!("    -L, --listen ADDRESS:PORT  address and port to listen at (0.0.0.0:3080)");
    println!("        --listen-unix PATH     Unix domain socket path to listen at");
    println!("    -U, --upstream-url URL     upstream download URL (https://crates.io/)");
    println!("    -I, --index-url URL        upstream index URL (https://index.crates.io/)");
    println!("    -S, --proxy-url URL        this proxy server URL (http://localhost:3080/)");
    println!("    -C, --cache-dir DIR        proxy cache directory (/var/cache/chilled-crates)");
    println!("    -T, --cache-ttl SECONDS    index cache entry Time-to-Live in seconds (3600)");
    println!("    -K, --cooldown DURATION    hide index versions newer than this (0 = off)");
    println!("                               suffixes: s, m, h, d, w (e.g. 7d, 12h, 30m)");
    println!("    -O, --cooldown-overrides L crates exempt from cooldown (comma-separated list)");
    println!("\nEnvironment:");
    println!("    INDEX_CRATES_IO_URL          same as --index-url option");
    println!("    CRATES_IO_URL                same as --upstream-url option");
    println!("    CRATES_IO_PROXY_URL          same as --proxy-url option");
    println!("    CRATES_IO_PROXY_CACHE_DIR    same as --cache-dir option");
    println!("    CRATES_IO_PROXY_CACHE_TTL    same as --cache-ttl option");
    println!("    CRATES_IO_PROXY_COOLDOWN     same as --cooldown option");
    println!("    CRATES_IO_PROXY_COOLDOWN_OVERRIDES  same as --cooldown-overrides option");
    println!("    CRATES_IO_PROXY_SHOW_METRICS same as --show-metrics option");
    println!("    LOG_LEVEL                    same as --log-level option");
    println!("    RUST_LOG                     overrides the log level (module filters allowed)");
}

/// Reads a boolean environment flag (`1`/`true`/`yes`/`on`, case-insensitive).
fn env_flag(name: &str) -> bool {
    env::var(name).is_ok_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
}

/// Normalizes a requested log level to a known value, defaulting to `info`.
fn normalize_log_level(level: Option<String>) -> String {
    match level.as_deref().map(str::trim).map(str::to_ascii_lowercase) {
        Some(l) if matches!(l.as_str(), "error" | "warn" | "info" | "debug" | "trace" | "off") => l,
        _ => "info".to_string(),
    }
}

/// Parses a comma/whitespace-separated crate list into a lower-cased set.
fn parse_overrides(list: &str) -> HashSet<String> {
    list.split([',', ' ', '\t', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

#[tokio::main]
async fn main() {
    let index_crates_io_url =
        env::var("INDEX_CRATES_IO_URL").unwrap_or_else(|_| INDEX_CRATES_IO_URL.to_string());
    let crates_io_url = env::var("CRATES_IO_URL").unwrap_or_else(|_| CRATES_IO_URL.to_string());
    let default_proxy_url =
        env::var("CRATES_IO_PROXY_URL").unwrap_or_else(|_| DEFAULT_PROXY_URL.to_string());
    let default_cache_dir =
        env::var("CRATES_IO_PROXY_CACHE_DIR").unwrap_or_else(|_| DEFAULT_CACHE_DIR.to_string());
    let default_cache_ttl_secs: u64 = env::var("CRATES_IO_PROXY_CACHE_TTL")
        .map_or(DEFAULT_CACHE_TTL_SECS, |s| {
            s.parse().expect("bad CRATES_IO_PROXY_CACHE_TTL value")
        });
    let default_cooldown =
        env::var("CRATES_IO_PROXY_COOLDOWN").unwrap_or_else(|_| "0".to_string());
    let default_overrides =
        env::var("CRATES_IO_PROXY_COOLDOWN_OVERRIDES").unwrap_or_default();
    let default_log_level = env::var("LOG_LEVEL").ok();
    let env_show_metrics = env_flag("CRATES_IO_PROXY_SHOW_METRICS");

    let mut verbose: u32 = 0;
    let mut args = Arguments::from_env();

    if args.contains(["-h", "--help"]) {
        usage();
        return;
    }

    if args.contains(["-V", "--version"]) {
        version();
        return;
    }

    while args.contains(["-v", "--verbose"]) {
        verbose += 1;
    }

    let listen_addr_unix: Option<String> = args
        .opt_value_from_str("--listen-unix")
        .expect("bad listen socket path");

    let listen_addr_ip: String = args
        .opt_value_from_str(["-L", "--listen"])
        .expect("bad listen address argument")
        .unwrap_or_else(|| LISTEN_ADDRESS.to_string());

    let index_url_string: String = args
        .opt_value_from_str(["-I", "--index-url"])
        .expect("bad upstream index URL argument")
        .unwrap_or(index_crates_io_url);

    let upstream_url_string: String = args
        .opt_value_from_str(["-U", "--upstream-url"])
        .expect("bad upstream download URL argument")
        .unwrap_or(crates_io_url);

    let proxy_url_string: String = args
        .opt_value_from_str(["-S", "--proxy-url"])
        .expect("bad proxy URL argument")
        .unwrap_or(default_proxy_url);

    let cache_dir_string: String = args
        .opt_value_from_str(["-C", "--cache-dir"])
        .expect("bad cache directory argument")
        .unwrap_or(default_cache_dir);

    let cache_ttl_secs: u64 = args
        .opt_value_from_str(["-T", "--cache-ttl"])
        .expect("bad cache TTL argument")
        .unwrap_or(default_cache_ttl_secs);

    let cooldown_string: String = args
        .opt_value_from_str(["-K", "--cooldown"])
        .expect("bad cooldown argument")
        .unwrap_or(default_cooldown);

    let overrides_string: String = args
        .opt_value_from_str(["-O", "--cooldown-overrides"])
        .expect("bad cooldown-overrides argument")
        .unwrap_or(default_overrides);

    let log_level_arg: Option<String> = args
        .opt_value_from_str(["-l", "--log-level"])
        .expect("bad log-level argument");

    let show_metrics = env_show_metrics || args.contains(["-m", "--show-metrics"]);

    // Resolve the log level: `--log-level` / `-v` win over `LOG_LEVEL`, which
    // wins over the `info` default. (`RUST_LOG` still overrides everything via
    // `from_env`.) Logs go to stdout.
    let log_level = match verbose {
        0 => normalize_log_level(log_level_arg.or(default_log_level)),
        1 => "debug".to_string(),
        _ => "trace".to_string(),
    };

    LogBuilder::from_env(LogEnv::new().default_filter_or(log_level))
        .target(env_logger::Target::Stdout)
        .init();

    let index_url = Url::parse(&index_url_string).expect("invalid upstream URL format");
    info!("proxy: using upstream index URL: {index_url}");

    let upstream_url = Url::parse(&upstream_url_string).expect("invalid upstream URL format");
    info!("proxy: using upstream download URL: {upstream_url}");

    let proxy_url = Url::parse(&proxy_url_string).expect("invalid proxy URL format");
    info!("proxy: using proxy server URL: {proxy_url}");

    let cache_dir = PathBuf::from(cache_dir_string);
    let index_dir = cache_dir.join("index");
    let crates_dir = cache_dir.join("crates");
    let cache_ttl = Duration::from_secs(cache_ttl_secs);

    info!("cache: using index directory: {}", index_dir.to_string_lossy());
    info!("cache: using crates directory: {}", crates_dir.to_string_lossy());
    info!("cache: using index entry TTL = {cache_ttl_secs} seconds");

    let cooldown = cooldown::parse_duration(&cooldown_string).expect("bad cooldown value");
    let overrides = parse_overrides(&overrides_string);

    if cooldown.as_secs() == 0 {
        info!("cooldown: age-gating disabled (pass-through)");
    } else {
        info!(
            "cooldown: hiding index versions newer than {} seconds ({} crate override(s))",
            cooldown.as_secs(),
            overrides.len()
        );
    }

    let config = ProxyConfig {
        index_url,
        upstream_url,
        proxy_url,
        index_dir,
        crates_dir,
        cache_ttl,
        cooldown,
        overrides: Arc::new(overrides),
        show_metrics,
    };

    info!(
        "metrics: `/` endpoint {}",
        if show_metrics {
            "reports cached crates"
        } else {
            "returns liveness status only"
        }
    );

    let client = reqwest::Client::builder()
        .user_agent(HTTP_USER_AGENT)
        .connect_timeout(Duration::from_secs(30))
        .build()
        .expect("failed to build HTTP client");

    let state = AppState {
        config: Arc::new(config),
        client,
        memo: Arc::new(FilteredMemo::new()),
    };

    let app = Router::new()
        .route("/", get(handle_home))
        .route("/index/{*path}", get(handle_index))
        .route("/api/v1/crates/{*path}", get(handle_download))
        .fallback(|| async { error_response(404) })
        .with_state(state);

    let listen_addr = match listen_addr_unix {
        Some(unix_path) => ListenAddress::UnixPath(unix_path),
        None => ListenAddress::SocketAddr(listen_addr_ip),
    };

    serve(listen_addr, app).await;
}

/// Binds the listener and serves until killed.
async fn serve(listen_addr: ListenAddress, app: Router) {
    match listen_addr {
        ListenAddress::SocketAddr(addr) => {
            info!("proxy: starting HTTP server at: {addr}");
            let listener = tokio::net::TcpListener::bind(&addr)
                .await
                .unwrap_or_else(|e| panic!("failed to bind {addr}: {e}"));
            axum::serve(listener, app.into_make_service())
                .await
                .expect("HTTP server error");
        }
        ListenAddress::UnixPath(path) => {
            info!("proxy: starting HTTP server at Unix socket {path}");
            // Reap a stale socket file before binding.
            std::fs::remove_file(&path).ok();
            let listener = tokio::net::UnixListener::bind(&path)
                .unwrap_or_else(|e| panic!("failed to bind {path}: {e}"));
            axum::serve(listener, app.into_make_service())
                .await
                .expect("HTTP server error");
        }
    }
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
    fn overrides_parse_lowercased() {
        let set = parse_overrides("Serde, tokio ,,FOO\nbar");
        assert!(set.contains("serde"));
        assert!(set.contains("tokio"));
        assert!(set.contains("foo"));
        assert!(set.contains("bar"));
        assert_eq!(set.len(), 4);
        assert!(parse_overrides("").is_empty());
    }

    #[test]
    fn memo_respects_validator_and_bucket() {
        let memo = FilteredMemo::new();
        memo.put("a".into(), "etag1".into(), 10, Bytes::from_static(b"x"));
        assert_eq!(memo.get("a", "etag1", 10), Some(Bytes::from_static(b"x")));
        // Different source content -> miss.
        assert_eq!(memo.get("a", "etag2", 10), None);
        // Different cutoff bucket -> miss.
        assert_eq!(memo.get("a", "etag1", 11), None);
        // Unknown crate -> miss.
        assert_eq!(memo.get("b", "etag1", 10), None);
    }
}
