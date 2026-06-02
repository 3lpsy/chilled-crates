//! HTTP request handlers, one module per route.

pub(crate) mod download;
pub(crate) mod healthz;
pub(crate) mod home;
pub(crate) mod index;
pub(crate) mod metrics;

pub(crate) use download::handle_download;
pub(crate) use healthz::handle_healthz;
pub(crate) use home::handle_home;
pub(crate) use index::handle_index;
pub(crate) use metrics::handle_metrics;
