//! Shared application state handed to every request handler.

use std::sync::Arc;

use crate::cache::{FilteredMemo, MetadataCache};
use crate::config::Config;

/// Shared application state passed to every request handler (cheap to clone).
#[derive(Clone)]
pub(crate) struct AppState {
    /// Immutable server configuration.
    pub(crate) config: Arc<Config>,
    /// Shared connection-pooling HTTP client.
    pub(crate) client: reqwest::Client,
    /// Memoized filtered index bodies.
    pub(crate) memo: Arc<FilteredMemo>,
    /// In-memory index entry metadata cache (etag / mtime).
    pub(crate) metadata: Arc<MetadataCache>,
}
