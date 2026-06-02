//! Immutable proxy server configuration.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use url::Url;

use crate::cooldown;

/// Proxy server configuration (immutable after startup).
///
/// The struct is public so the library's [`crate::build_router`] entry point can
/// accept one, but its fields stay crate-private; construct it with
/// [`Config::new`].
#[derive(Debug, Clone)]
pub struct Config {
    /// Upstream registry index URL (defaults to [`crate::constants::INDEX_CRATES_IO_URL`]).
    pub(crate) index_url: Url,

    /// Upstream crate download URL (defaults to [`crate::constants::CRATES_IO_URL`]).
    pub(crate) upstream_url: Url,

    /// External URL of this proxy server (defaults to [`crate::constants::DEFAULT_PROXY_URL`]).
    pub(crate) proxy_url: Url,

    /// Registry index cache directory.
    pub(crate) index_dir: PathBuf,

    /// Crate files cache directory.
    pub(crate) crates_dir: PathBuf,

    /// Index entry cache Time-to-Live (defaults to [`crate::constants::DEFAULT_CACHE_TTL_SECS`]).
    pub(crate) cache_ttl: Duration,

    /// Sparse-index age-gating window; a zero duration disables filtering.
    pub(crate) cooldown: Duration,

    /// Lower-cased crate names exempt from age-gating (served unfiltered).
    pub(crate) overrides: Arc<HashSet<String>>,

    /// When set, the download endpoint also refuses crate versions newer than
    /// the cooldown (not just hiding them from the index).
    pub(crate) restrict_downloads: bool,
}

impl Config {
    /// Builds a configuration from explicit values, deriving the `index`/`crates`
    /// cache subdirectories from `cache_dir`. Shared by the CLI/env bootstrap
    /// ([`super::build`]) and by external callers (e.g. integration tests) that
    /// drive [`crate::build_router`] directly.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        index_url: Url,
        upstream_url: Url,
        proxy_url: Url,
        cache_dir: &Path,
        cache_ttl: Duration,
        cooldown: Duration,
        overrides: HashSet<String>,
        restrict_downloads: bool,
    ) -> Self {
        Config {
            index_url,
            upstream_url,
            proxy_url,
            index_dir: cache_dir.join("index"),
            crates_dir: cache_dir.join("crates"),
            cache_ttl,
            cooldown,
            overrides: Arc::new(overrides),
            restrict_downloads,
        }
    }

    /// The age-gating cutoff (unix seconds) for `name`, or `None` when it is
    /// served unfiltered — cooldown disabled, or the crate is overridden.
    ///
    /// Shared by the index serve path and the `--restrict-downloads` check.
    pub(crate) fn cutoff_for(&self, name: &str) -> Option<u64> {
        if self.overrides.contains(&name.to_ascii_lowercase()) {
            return None;
        }
        let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
        cooldown::cutoff_from(now, self.cooldown)
    }
}
