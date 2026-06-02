//! `GET /api/v1/crates/<path>` — proxied, cached crate file downloads.

use std::path::Path;

use axum::{
    extract::{Path as UrlPath, State},
    response::Response,
};
use bytes::Bytes;
use log::{info, warn};

use crate::cache::{
    CrateInfo, IndexEntry, cache_fetch_crate, cache_fetch_index_entry, cache_store_crate,
};
use crate::constants::{CRATES_API_PATH, MAX_CRATE_SIZE};
use crate::cooldown;
use crate::http::{
    FetchError, crate_response, error_response, format_json_error, json_response, read_capped,
};
use crate::server::AppState;

/// Reads a cached crate file off the blocking thread pool.
async fn cache_read_crate(dir: &Path, info: &CrateInfo) -> Option<Vec<u8>> {
    let dir = dir.to_path_buf();
    let info = info.clone();
    tokio::task::spawn_blocking(move || cache_fetch_crate(&dir, &info))
        .await
        .ok()
        .flatten()
}

/// Whether this version may be downloaded under `--restrict-downloads`.
///
/// The requested version's `pubtime` must be at or before `cutoff`. We read it
/// from the locally cached (pristine, unfiltered) index entry — populated when
/// cargo fetched the index to resolve. **Fail-closed**: if the index isn't
/// cached, or the version isn't found, or its `pubtime` is newer than `cutoff`,
/// the download is refused. (For normal cargo use the index is always cached
/// first, so only direct/forged requests for too-new versions are blocked.)
async fn download_old_enough(state: &AppState, info: &CrateInfo, cutoff: u64) -> bool {
    let dir = state.config.index_dir.clone();
    let entry = IndexEntry::new(info.name());
    let body = tokio::task::spawn_blocking(move || cache_fetch_index_entry(&dir, &entry))
        .await
        .ok()
        .flatten();

    let Some(body) = body else { return false };
    let Ok(text) = std::str::from_utf8(&body) else {
        return false;
    };
    matches!(cooldown::version_pubtime(text, info.version()), Some(pt) if pt <= cutoff)
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

/// Handles a crate download request: `GET /api/v1/crates/<path>`.
pub(crate) async fn handle_download(
    State(state): State<AppState>,
    UrlPath(path): UrlPath<String>,
) -> Response {
    let Some(crate_info) = CrateInfo::try_from_download_url(&path) else {
        warn!("proxy: unrecognized download API endpoint: {path}");
        return error_response(404);
    };

    // With --restrict-downloads, refuse versions newer than the cooldown even
    // if requested directly (e.g. a hand-edited Cargo.lock).
    if state.config.restrict_downloads {
        if let Some(cutoff) = state.config.cutoff_for(crate_info.name()) {
            if !download_old_enough(&state, &crate_info, cutoff).await {
                warn!("download: refused {crate_info}: newer than cooldown or unverifiable");
                return error_response(403);
            }
        }
    }

    if let Some(data) = cache_read_crate(&state.config.crates_dir, &crate_info).await {
        info!("cache: served cached crate {crate_info} ({} bytes)", data.len());
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
