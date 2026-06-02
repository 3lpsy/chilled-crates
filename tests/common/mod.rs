//! Shared blackbox-test harness for `chilled-crates`.
//!
//! Each test spins up:
//!   * a `wiremock` mock standing in for BOTH upstreams (the sparse index and
//!     the crate download server — their path namespaces don't collide, so one
//!     mock serves both), and
//!   * an in-process instance of the proxy itself, built with
//!     [`chilled_crates::build_router`] and served on an ephemeral TCP port.
//!
//! Tests then drive the proxy over real HTTP with a `reqwest` client, exactly as
//! cargo would, and assert on responses, upstream hit counts, and the on-disk
//! cache.
//!
//! Every `TestProxy` owns its own mock, port, temp cache dir, and (since the
//! metadata/memo caches are instance state) its own caches — so tests are fully
//! isolated and run concurrently.
//!
//! Layout:
//!   * [`fixtures`] — index bodies, `pubtime` sentinels, and small helpers.
//!   * [`proxy_builder`] — the [`TestProxyBuilder`] that starts a proxy.
//!   * [`proxy`] — the [`TestProxy`] handle (mock mounters, drivers, cache).
//!
//! Each test binary uses only a subset of these re-exports, so unused ones are
//! expected.
#![allow(unused_imports)]

pub mod fixtures;
pub mod proxy;
pub mod proxy_builder;

pub use fixtures::*;
pub use proxy::TestProxy;
pub use proxy_builder::TestProxyBuilder;
