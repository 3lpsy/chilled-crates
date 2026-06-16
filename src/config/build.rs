//! Configuration bootstrap: parse environment + CLI into a [`Startup`] bundle.

use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use log::info;
use url::Url;

use super::config::Config;
use super::helpers::{normalize_log_level, parse_overrides};
use super::version::version;
use crate::constants::{
    CRATES_IO_URL, DEFAULT_CACHE_DIR, DEFAULT_CACHE_TTL_SECS, DEFAULT_PROXY_URL,
    INDEX_CRATES_IO_URL, LISTEN_ADDRESS,
};
use crate::cooldown;
use crate::server::ListenAddress;

/// Command-line arguments (each also populated from its environment variable).
#[derive(Parser)]
#[command(
    name = "chilled-crates",
    about = "Caching crates.io proxy with sparse-index age-gating",
    disable_version_flag = true
)]
struct Cli {
    /// Raise the log level (-v debug, -vv trace).
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Print version and exit.
    #[arg(short = 'V', long)]
    version: bool,

    /// Log level: error|warn|info|debug|trace|off.
    #[arg(short, long, env = "LOG_LEVEL")]
    log_level: Option<String>,

    /// Expose cached crates at the /metrics endpoint.
    #[arg(short = 'm', long, env = "CRATES_IO_PROXY_ENABLE_METRICS")]
    enable_metrics: bool,

    /// Also refuse to *download* crate versions newer than the cooldown
    /// (not just hide them from the index).
    #[arg(long, env = "CRATES_IO_PROXY_RESTRICT_DOWNLOADS")]
    restrict_downloads: bool,

    /// Unix domain socket path to listen at.
    #[arg(long)]
    listen_unix: Option<String>,

    /// Address and port to listen at.
    #[arg(short = 'L', long, default_value = LISTEN_ADDRESS)]
    listen: String,

    /// Upstream registry index URL.
    #[arg(short = 'I', long, env = "INDEX_CRATES_IO_URL", default_value = INDEX_CRATES_IO_URL)]
    index_url: Url,

    /// Upstream crate download URL.
    #[arg(short = 'U', long, env = "CRATES_IO_URL", default_value = CRATES_IO_URL)]
    upstream_url: Url,

    /// This proxy server's external URL.
    #[arg(short = 'S', long, env = "CRATES_IO_PROXY_URL", default_value = DEFAULT_PROXY_URL)]
    proxy_url: Url,

    /// Proxy cache directory.
    #[arg(short = 'C', long, env = "CRATES_IO_PROXY_CACHE_DIR", default_value = DEFAULT_CACHE_DIR)]
    cache_dir: String,

    /// Index cache entry Time-to-Live, in seconds.
    #[arg(short = 'T', long, env = "CRATES_IO_PROXY_CACHE_TTL", default_value_t = DEFAULT_CACHE_TTL_SECS)]
    cache_ttl: u64,

    /// Hide index versions newer than this (0 = off). Suffixes: s, m, h, d, w.
    #[arg(
        short = 'K',
        long,
        env = "CRATES_IO_PROXY_COOLDOWN",
        default_value = "0",
        value_parser = cooldown::parse_duration
    )]
    cooldown: Duration,

    /// Crates exempt from cooldown (comma-separated list).
    #[arg(
        short = 'O',
        long,
        env = "CRATES_IO_PROXY_COOLDOWN_OVERRIDES",
        default_value = ""
    )]
    cooldown_overrides: String,
}

/// Everything `main` needs to start the server, resolved from the environment
/// and command line.
pub(crate) struct Startup {
    /// Immutable server configuration.
    pub(crate) config: Config,
    /// Resolved log level filter (the logger is initialized by `main`).
    pub(crate) log_level: String,
    /// Whether the `/metrics` endpoint should be routed.
    pub(crate) enable_metrics: bool,
    /// Address the server should listen on.
    pub(crate) listen: ListenAddress,
}

impl Startup {
    /// Logs the effective configuration. Call *after* the logger is initialized.
    pub(crate) fn log(&self) {
        let c = &self.config;
        info!("proxy: using upstream index URL: {}", c.index_url);
        info!("proxy: using upstream download URL: {}", c.upstream_url);
        info!("proxy: using proxy server URL: {}", c.proxy_url);
        info!(
            "cache: using index directory: {}",
            c.index_dir.to_string_lossy()
        );
        info!(
            "cache: using crates directory: {}",
            c.crates_dir.to_string_lossy()
        );
        info!(
            "cache: using index entry TTL = {} seconds",
            c.cache_ttl.as_secs()
        );

        if c.cooldown.as_secs() == 0 {
            info!("cooldown: age-gating disabled (pass-through)");
        } else {
            info!(
                "cooldown: hiding index versions newer than {} seconds ({} crate override(s))",
                c.cooldown.as_secs(),
                c.overrides.len()
            );
            if c.restrict_downloads {
                info!("cooldown: downloads of too-new versions are also refused");
            }
        }

        if self.enable_metrics {
            info!("metrics: /metrics endpoint enabled");
        } else {
            info!("metrics: /metrics endpoint disabled");
        }
    }
}

/// Parses environment variables and command-line arguments into a [`Startup`].
///
/// Returns `None` when `--version` was handled (the caller should then exit).
/// `--help` and parse errors are handled by clap itself (it exits). Does **not**
/// initialize logging — that is left to the caller so the resolved level
/// applies; the trade-off is that argument parsing itself is silent.
pub(crate) fn build() -> Option<Startup> {
    let cli = Cli::parse();

    if cli.version {
        version();
        return None;
    }

    // Resolve the log level: `-v`/`-vv` win over `--log-level`/`LOG_LEVEL`,
    // which wins over the `info` default. (`RUST_LOG` still overrides
    // everything when the logger is built from the environment.)
    let log_level = match cli.verbose {
        0 => normalize_log_level(cli.log_level),
        1 => "debug".to_string(),
        _ => "trace".to_string(),
    };

    let cache_dir = PathBuf::from(cli.cache_dir);
    let config = Config::new(
        cli.index_url,
        cli.upstream_url,
        cli.proxy_url,
        &cache_dir,
        Duration::from_secs(cli.cache_ttl),
        cli.cooldown,
        parse_overrides(&cli.cooldown_overrides),
        cli.restrict_downloads,
    );

    let listen = match cli.listen_unix {
        Some(unix_path) => ListenAddress::UnixPath(unix_path),
        None => ListenAddress::SocketAddr(cli.listen),
    };

    Some(Startup {
        config,
        log_level,
        enable_metrics: cli.enable_metrics,
        listen,
    })
}
