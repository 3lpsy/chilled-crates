//! Builder that configures and starts the real proxy in-process against a mock
//! upstream, returning a [`TestProxy`] handle.
#![allow(dead_code)]

use std::collections::HashSet;
use std::time::Duration;

use tempfile::TempDir;
use url::Url;
use wiremock::MockServer;

use super::proxy::TestProxy;

/// Configures and starts a [`TestProxy`].
pub struct TestProxyBuilder {
    cooldown: Duration,
    cache_ttl: Duration,
    overrides: HashSet<String>,
    restrict_downloads: bool,
    enable_metrics: bool,
    proxy_url: String,
    dead_upstream: bool,
}

impl Default for TestProxyBuilder {
    fn default() -> Self {
        TestProxyBuilder {
            cooldown: Duration::ZERO,
            cache_ttl: Duration::from_secs(3600),
            overrides: HashSet::new(),
            restrict_downloads: false,
            enable_metrics: false,
            proxy_url: "http://localhost:3080/".to_string(),
            dead_upstream: false,
        }
    }
}

impl TestProxyBuilder {
    pub fn cooldown(mut self, d: Duration) -> Self {
        self.cooldown = d;
        self
    }

    pub fn cooldown_days(self, days: u64) -> Self {
        self.cooldown(Duration::from_secs(days * 86_400))
    }

    pub fn cache_ttl(mut self, d: Duration) -> Self {
        self.cache_ttl = d;
        self
    }

    /// Adds a crate to the cooldown-override set (stored lower-cased, matching
    /// the app's case-insensitive lookup).
    pub fn override_crate(mut self, name: &str) -> Self {
        self.overrides.insert(name.to_ascii_lowercase());
        self
    }

    pub fn restrict_downloads(mut self) -> Self {
        self.restrict_downloads = true;
        self
    }

    pub fn enable_metrics(mut self) -> Self {
        self.enable_metrics = true;
        self
    }

    pub fn proxy_url(mut self, url: &str) -> Self {
        self.proxy_url = url.to_string();
        self
    }

    /// Points both upstream URLs at a refused port so upstream fetches fail at
    /// the transport layer (for stale-cache / 502 tests).
    pub fn dead_upstream(mut self) -> Self {
        self.dead_upstream = true;
        self
    }

    pub async fn start(self) -> TestProxy {
        let mock_upstream = MockServer::start().await;

        let upstream = if self.dead_upstream {
            // Reserved-but-refused: nothing listens on TCP port 1.
            "http://127.0.0.1:1/".to_string()
        } else {
            format!("{}/", mock_upstream.uri().trim_end_matches('/'))
        };

        let tmp = TempDir::new().expect("create temp cache dir");
        let cache_dir = tmp.path().to_path_buf();

        let config = chilled_crates::Config::new(
            Url::parse(&upstream).unwrap(),
            Url::parse(&upstream).unwrap(),
            Url::parse(&self.proxy_url).unwrap(),
            &cache_dir,
            self.cache_ttl,
            self.cooldown,
            self.overrides,
            self.restrict_downloads,
        );

        let app = chilled_crates::build_router(config, self.enable_metrics);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().unwrap();
        tokio::spawn(chilled_crates::serve_listener(listener, app));

        let client = reqwest::Client::builder()
            .build()
            .expect("build test client");
        let base_url = format!("http://{addr}");

        // Wait until the server is accepting and answering.
        for _ in 0..100 {
            if let Ok(r) = client.get(format!("{base_url}/healthz")).send().await {
                if r.status().is_success() {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        TestProxy::new(mock_upstream, base_url, cache_dir, client, tmp)
    }
}
