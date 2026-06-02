//! Compile-time constants shared across the crate.

/// Default listen address and port.
pub(crate) const LISTEN_ADDRESS: &str = "0.0.0.0:3080";

/// Upstream `crates.io` registry index URL.
pub(crate) const INDEX_CRATES_IO_URL: &str = "https://index.crates.io/";

/// Upstream `crates.io` registry URL.
pub(crate) const CRATES_IO_URL: &str = "https://crates.io/";

/// Default external URL of this proxy server.
pub(crate) const DEFAULT_PROXY_URL: &str = "http://localhost:3080/";

/// Crates download API path.
pub(crate) const CRATES_API_PATH: &str = "/api/v1/crates/";

/// Default crate files cache directory path.
pub(crate) const DEFAULT_CACHE_DIR: &str = "/var/cache/chilled-crates";

/// Default index cache entry Time-to-Live in seconds.
pub(crate) const DEFAULT_CACHE_TTL_SECS: u64 = 3600;

/// Limit the crate file download size to 16 MiB.
pub(crate) const MAX_CRATE_SIZE: usize = 0x100_0000;

/// Limit the sparse-index entry download size to 64 MiB.
pub(crate) const MAX_INDEX_SIZE: usize = 0x400_0000;

/// HTTP Content-Type of the registry index entry JSON file.
pub(crate) const INDEX_CTYPE: &str = "text/plain";

/// HTTP Content-Type of the crate package file.
pub(crate) const CRATE_CTYPE: &str = "application/x-tar";

/// HTTP Content-Type of the crates API JSON response.
pub(crate) const JSON_CTYPE: &str = "application/json; charset=utf-8";

/// Program version tag: `"<major>.<minor>.<patch>"`.
pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

/// HTTP client User Agent string.
pub(crate) const HTTP_USER_AGENT: &str = concat!("chilled-crates/", env!("CARGO_PKG_VERSION"));
