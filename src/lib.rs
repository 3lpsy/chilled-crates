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
//! The server is built on `axum` + `tokio` (async) with a shared `reqwest`
//! client. Blocking filesystem-cache operations run on the blocking thread
//! pool, and CPU-bound age-gating runs off the async workers and is memoized.
//!
//! Module layout:
//! - [`constants`] — compile-time constants.
//! - [`config`] — configuration, CLI bootstrap (`build`), help, and version.
//! - [`server`] — server bootstrap (`serve`) and shared request state (`AppState`).
//! - [`routes`] — one module per HTTP route.
//! - [`cache`] — on-disk / metadata / memo caches and the registry data types.
//! - [`cooldown`] — the age-gating filter and duration parsing.
//! - [`http`] — shared response builders and capped upstream fetch.
//!
//! # Public API
//!
//! The crate is primarily a binary, but exposes a small library surface so the
//! server can be embedded or driven by blackbox integration tests: build a
//! [`Config`] with [`Config::new`], turn it into an `axum` `Router` with
//! [`build_router`], and serve that router however you like. [`run`] is the
//! binary's own entry point (parse CLI/env, init logging, bind, serve).

pub(crate) mod cache;
pub(crate) mod config;
pub(crate) mod constants;
pub(crate) mod cooldown;
pub(crate) mod http;
pub(crate) mod routes;
pub(crate) mod server;
pub(crate) mod valid;

use std::sync::Arc;
use std::time::Duration;

use axum::{routing::get, Router};
use env_logger::{Builder as LogBuilder, Env as LogEnv};

use crate::constants::HTTP_USER_AGENT;
use crate::http::error_response;
use crate::routes::{handle_download, handle_healthz, handle_home, handle_index, handle_metrics};
use crate::server::{serve, AppState};

pub use crate::config::Config;
pub use crate::server::serve_listener;

/// Builds the proxy's `axum` `Router` with every route and the shared request
/// state wired up. `enable_metrics` toggles the optional `/metrics` route.
///
/// Does **not** initialize logging or bind a listener — that is left to the
/// caller. The binary calls this from [`run`]; integration tests call it
/// directly and serve the returned router on an ephemeral port.
pub fn build_router(config: Config, enable_metrics: bool) -> Router {
    let client = reqwest::Client::builder()
        .user_agent(HTTP_USER_AGENT)
        .connect_timeout(Duration::from_secs(30))
        .build()
        .expect("failed to build HTTP client");

    let state = AppState {
        config: Arc::new(config),
        client,
        memo: Arc::new(cache::FilteredMemo::new()),
        metadata: Arc::new(cache::MetadataCache::new()),
    };

    let mut app = Router::new()
        .route("/", get(handle_home))
        .route("/healthz", get(handle_healthz))
        .route("/index/{*path}", get(handle_index))
        .route("/api/v1/crates/{*path}", get(handle_download));

    // The metrics endpoint is only routed when enabled; otherwise it 404s.
    if enable_metrics {
        app = app.route("/metrics", get(handle_metrics));
    }

    app.fallback(|| async { error_response(404) })
        .with_state(state)
}

/// The binary entry point: parse the environment + CLI, initialize logging, and
/// serve until the process is killed. Returns early when `--help`/`--version`
/// were handled by the bootstrap.
pub async fn run() {
    // Parse the environment + CLI. `None` means `--help`/`--version` ran.
    let Some(startup) = config::build() else {
        return;
    };

    // Initialize logging (stdout) before emitting any configuration logs.
    LogBuilder::from_env(LogEnv::new().default_filter_or(startup.log_level.as_str()))
        .target(env_logger::Target::Stdout)
        .init();
    startup.log();

    let app = build_router(startup.config, startup.enable_metrics);
    serve(startup.listen, app).await;
}
