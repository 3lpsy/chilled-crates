//! `GET /metrics` — report cached crates (only routed when enabled).

use std::path::Path;
use std::time::UNIX_EPOCH;

use axum::{extract::State, response::Response};

use crate::http::{error_response, json_response};
use crate::server::AppState;
use crate::valid;

/// Handles `GET /metrics`: reports every cached crate file (name, version, and
/// cache timestamp). Only routed when metrics are enabled, so reaching this
/// handler means the operator opted in.
pub(crate) async fn handle_metrics(State(state): State<AppState>) -> Response {
    let dir = state.config.crates_dir.clone();
    match tokio::task::spawn_blocking(move || metrics_json(&dir)).await {
        Ok(body) => json_response(200, body),
        Err(_) => error_response(500),
    }
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
        r#"{{"service":"chilled-crates","cached_count":{},"crates":[{}]}}"#,
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
