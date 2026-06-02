//! The [`TestProxy`] handle: drive the running proxy over HTTP, mount upstream
//! responses, and inspect the on-disk cache. Constructed by
//! [`super::proxy_builder::TestProxyBuilder`].
#![allow(dead_code)]

use std::fs::{self, File};
use std::path::PathBuf;
use std::time::SystemTime;

use reqwest::header::HeaderName;
use tempfile::TempDir;
use wiremock::matchers::{header, method, path as match_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::fixtures::index_rel;
use super::proxy_builder::TestProxyBuilder;

/// A running proxy + its mock upstream + temp cache dir.
pub struct TestProxy {
    pub mock_upstream: MockServer,
    pub base_url: String,
    pub cache_dir: PathBuf,
    client: reqwest::Client,
    _tmp: TempDir,
}

impl TestProxy {
    /// Entry point: configure a proxy via the builder.
    pub fn builder() -> TestProxyBuilder {
        TestProxyBuilder::default()
    }

    /// Assembles a handle from the started proxy's parts. Called by the builder.
    pub(crate) fn new(
        mock_upstream: MockServer,
        base_url: String,
        cache_dir: PathBuf,
        client: reqwest::Client,
        tmp: TempDir,
    ) -> Self {
        TestProxy {
            mock_upstream,
            base_url,
            cache_dir,
            client,
            _tmp: tmp,
        }
    }

    // Mock upstream mounting.

    /// Mounts a 200 index response for `name` with the given body + validators.
    pub async fn mock_index(&self, name: &str, body: &str, etag: &str, last_modified: &str) {
        Mock::given(method("GET"))
            .and(match_path(format!("/{}", index_rel(name))))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("etag", etag)
                    .insert_header("last-modified", last_modified)
                    .set_body_string(body.to_string()),
            )
            .mount(&self.mock_upstream)
            .await;
    }

    /// Mounts a 200 index response carrying a non-UTF-8 body.
    pub async fn mock_index_bytes(&self, name: &str, body: Vec<u8>, etag: &str) {
        Mock::given(method("GET"))
            .and(match_path(format!("/{}", index_rel(name))))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("etag", etag)
                    .set_body_bytes(body),
            )
            .mount(&self.mock_upstream)
            .await;
    }

    /// Mounts a higher-priority conditional `304` for `name`, matching requests
    /// whose `If-None-Match` equals `etag` (the unmarked upstream validator).
    pub async fn mock_index_304(&self, name: &str, etag: &str) {
        Mock::given(method("GET"))
            .and(match_path(format!("/{}", index_rel(name))))
            .and(header("if-none-match", etag))
            .respond_with(ResponseTemplate::new(304).insert_header("etag", etag))
            .with_priority(1)
            .mount(&self.mock_upstream)
            .await;
    }

    /// Mounts an arbitrary upstream status (e.g. 404) for an index path.
    pub async fn mock_index_status(&self, name: &str, status: u16) {
        Mock::given(method("GET"))
            .and(match_path(format!("/{}", index_rel(name))))
            .respond_with(ResponseTemplate::new(status).set_body_string("upstream says no"))
            .mount(&self.mock_upstream)
            .await;
    }

    /// Mounts a 200 crate-download response with `bytes`.
    pub async fn mock_crate(&self, name: &str, version: &str, bytes: &[u8]) {
        Mock::given(method("GET"))
            .and(match_path(format!("/api/v1/crates/{name}/{version}/download")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes.to_vec()))
            .mount(&self.mock_upstream)
            .await;
    }

    /// Mounts an arbitrary upstream status for a crate download.
    pub async fn mock_crate_status(&self, name: &str, version: &str, status: u16) {
        Mock::given(method("GET"))
            .and(match_path(format!("/api/v1/crates/{name}/{version}/download")))
            .respond_with(ResponseTemplate::new(status).set_body_string("nope"))
            .mount(&self.mock_upstream)
            .await;
    }

    // Upstream introspection.

    /// Number of upstream requests received whose path equals `path`.
    pub async fn upstream_hits(&self, path: &str) -> usize {
        self.mock_upstream
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.url.path() == path)
            .count()
    }

    /// Total number of upstream requests received.
    pub async fn upstream_total(&self) -> usize {
        self.mock_upstream
            .received_requests()
            .await
            .unwrap_or_default()
            .len()
    }

    /// The upstream index path for a crate (e.g. `/se/rd/serde`).
    pub fn index_upstream_path(&self, name: &str) -> String {
        format!("/{}", index_rel(name))
    }

    // HTTP drivers (act as cargo).

    /// `GET /index/<sparse-path>` with optional extra request headers.
    pub async fn get_index(&self, name: &str, headers: &[(&str, &str)]) -> reqwest::Response {
        let mut req = self
            .client
            .get(format!("{}/index/{}", self.base_url, index_rel(name)));
        for (k, v) in headers {
            req = req.header(HeaderName::from_bytes(k.as_bytes()).unwrap(), *v);
        }
        req.send().await.expect("get_index")
    }

    /// `GET /index/config.json`.
    pub async fn get_config_json(&self) -> reqwest::Response {
        self.get("/index/config.json").await
    }

    /// `GET /api/v1/crates/<name>/<version>/download`.
    pub async fn download(&self, name: &str, version: &str) -> reqwest::Response {
        self.get(&format!("/api/v1/crates/{name}/{version}/download"))
            .await
    }

    /// `GET <path>` against the proxy (path begins with `/`).
    pub async fn get(&self, path: &str) -> reqwest::Response {
        self.client
            .get(format!("{}{}", self.base_url, path))
            .send()
            .await
            .expect("get")
    }

    // On-disk cache helpers.

    /// Absolute path of the cached index entry file for `name`.
    pub fn index_cache_path(&self, name: &str) -> PathBuf {
        self.cache_dir.join("index").join(index_rel(name))
    }

    /// Absolute path of the cached `.crate` file for `name`/`version`.
    pub fn crate_cache_path(&self, name: &str, version: &str) -> PathBuf {
        self.cache_dir
            .join("crates")
            .join(name)
            .join(format!("{name}-{version}.crate"))
    }

    /// Writes a pristine (unfiltered) index entry straight to the on-disk cache,
    /// with the given mtime — bypassing an upstream fetch. Used to set up
    /// stale-cache and restrict-downloads tests deterministically.
    pub fn seed_index_file(&self, name: &str, body: &str, mtime: SystemTime) {
        self.seed_index_bytes(name, body.as_bytes(), mtime);
    }

    /// Like [`Self::seed_index_file`] but writes raw bytes (e.g. a non-UTF-8
    /// body, to exercise the fail-closed download path).
    pub fn seed_index_bytes(&self, name: &str, body: &[u8], mtime: SystemTime) {
        let path = self.index_cache_path(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut f = File::create(&path).unwrap();
        use std::io::Write;
        f.write_all(body).unwrap();
        f.set_modified(mtime).unwrap();
    }
}
