Chilled Crates
==============

A vibed fork (prompted with no mistakes, no bugs) of [`crates-io-proxy`](https://github.com/ravenexp/crates-io-proxy) that adds a
configurable **cooldown delay**: the proxy hides sparse-index crate versions newer than a chosen
window, so freshly-published (possibly malicious) releases are withheld until the community has
had time to detect and yank them. The age-gating logic is ported from
[menhera.org's cooldown proxy](https://www.menhera.org/crates-io-cooldown-proxy-mitigating-supply-chain-attacks/).

**Warning:** This project has not been reviewed for vulnerabilities and security issues. It is
not recommended to expose the service to the internet or adversarial networks. This project is
intended to be used in a personal homelab environment. If you wish to leverage the public Docker
builds, it is recommended to pin a tagged version or hash as breaking changes are to be expected.

Enable the delay with the `--cooldown` flag (or `CRATES_IO_PROXY_COOLDOWN` env var), taking a
duration with an `s`/`m`/`h`/`d`/`w` suffix; `0` (the default) disables it. Clients use the proxy
exactly as they would the upstream — point cargo at it via source replacement in `.cargo/config`:

```
# server: 7-day cooldown
chilled-crates --cooldown 7d

# client: .cargo/config
[source.crates-io]
replace-with = "chilled-crates"

[registries.chilled-crates]
index = "sparse+http://chilled-crates.example.com:3080/index/"
```

### Exempting crates from the cooldown

Specific crates can bypass the cooldown (always served unfiltered) with
`--cooldown-overrides` (or the `CRATES_IO_PROXY_COOLDOWN_OVERRIDES` env var), taking a
comma-separated list of crate names. Use it for first-party crates you publish and consume
yourself, where the delay only gets in your way. Matching is case-insensitive against the
canonical index name (it does **not** normalize `-` vs `_`).

```
# 7-day cooldown for everything except `my-app` and `my-lib`
chilled-crates --cooldown 7d --cooldown-overrides my-app,my-lib

# equivalently, via the environment
CRATES_IO_PROXY_COOLDOWN_OVERRIDES="my-app,my-lib" chilled-crates --cooldown 7d
```

### Restricting downloads

By default the cooldown only hides too-new versions from the *index*, so cargo never resolves
them. The crate **download** endpoint stays version-agnostic — a client that already knows an
exact `name/version` (e.g. a hand-edited `Cargo.lock`) could still fetch a too-new crate
directly. Enable `--restrict-downloads` (or `CRATES_IO_PROXY_RESTRICT_DOWNLOADS=1`) to also
enforce the cooldown on downloads: a version whose publish time is newer than the window is
refused with `403`.

```
chilled-crates --cooldown 7d --restrict-downloads
```

The check reads the requested version's publish time from the locally cached index entry, and is
**fail-closed**: if that entry isn't cached (or the version isn't in it), the download is refused.
In normal use cargo fetches the index before downloading, so only direct/forged requests for
too-new versions are blocked. Crates in `--cooldown-overrides` are exempt here too.

### Logging

Logs are written to **stdout**. The level defaults to `info` and is set with `--log-level`
(or the `LOG_LEVEL` env var): `error`, `warn`, `info`, `debug`, `trace`, or `off`. `-v`/`-vv`
are shortcuts for `debug`/`trace`, and `RUST_LOG` still overrides everything (and allows
per-module filters). `info` stays quiet during normal operation — errors, malformed requests,
and bad upstream responses are logged, but routine cache hits are `debug`.

```
chilled-crates --log-level debug
LOG_LEVEL=warn chilled-crates
```

### Status, health, and metrics endpoints

`GET /` is a minimal liveness endpoint, always available:

```json
{"status": "running"}
```

`GET /healthz` is a health-check endpoint for probes/load balancers — HTTP 200 with a plain
`ok` body (the conventional `healthz` contract).

`GET /metrics` lists the crate files currently in the cache, with versions and cache timestamps
(unix seconds). It is **only routed when enabled** with `--enable-metrics` (or
`CRATES_IO_PROXY_ENABLE_METRICS=1`); otherwise it returns `404`.

```json
{"service":"chilled-crates","cached_count":2,
 "crates":[{"name":"cfg-if","version":"1.0.0","cached_at":1780376385}]}
```

```
chilled-crates --enable-metrics
```

---

## Upstream documentation

`chilled-crates` keeps the caching-proxy core of `crates-io-proxy` (transparent caching of the
sparse index and the crate download server, source-replacement setup, static git-index mirror
mode, and the build-time `native-certs` feature). For that base behavior and its configuration,
see the upstream README:

<https://github.com/ravenexp/crates-io-proxy#readme>

## Command-line options

Every option also reads from its environment variable (shown as `[env: ...]`). Run
`chilled-crates --help` to print:

```
Caching crates.io proxy with sparse-index age-gating

Usage: chilled-crates [OPTIONS]

Options:
  -v, --verbose...
          Raise the log level (-v debug, -vv trace)
  -V, --version
          Print version and exit
  -l, --log-level <LOG_LEVEL>
          Log level: error|warn|info|debug|trace|off [env: LOG_LEVEL=]
  -m, --enable-metrics
          Expose cached crates at the /metrics endpoint [env: CRATES_IO_PROXY_ENABLE_METRICS=]
      --restrict-downloads
          Also refuse to *download* crate versions newer than the cooldown (not just hide them from the index) [env: CRATES_IO_PROXY_RESTRICT_DOWNLOADS=]
      --listen-unix <LISTEN_UNIX>
          Unix domain socket path to listen at
  -L, --listen <LISTEN>
          Address and port to listen at [default: 0.0.0.0:3080]
  -I, --index-url <INDEX_URL>
          Upstream registry index URL [env: INDEX_CRATES_IO_URL=] [default: https://index.crates.io/]
  -U, --upstream-url <UPSTREAM_URL>
          Upstream crate download URL [env: CRATES_IO_URL=] [default: https://crates.io/]
  -S, --proxy-url <PROXY_URL>
          This proxy server's external URL [env: CRATES_IO_PROXY_URL=] [default: http://localhost:3080/]
  -C, --cache-dir <CACHE_DIR>
          Proxy cache directory [env: CRATES_IO_PROXY_CACHE_DIR=] [default: /var/cache/chilled-crates]
  -T, --cache-ttl <CACHE_TTL>
          Index cache entry Time-to-Live, in seconds [env: CRATES_IO_PROXY_CACHE_TTL=] [default: 3600]
  -K, --cooldown <COOLDOWN>
          Hide index versions newer than this (0 = off). Suffixes: s, m, h, d, w [env: CRATES_IO_PROXY_COOLDOWN=] [default: 0]
  -O, --cooldown-overrides <COOLDOWN_OVERRIDES>
          Crates exempt from cooldown (comma-separated list) [env: CRATES_IO_PROXY_COOLDOWN_OVERRIDES=] [default: ""]
  -h, --help
          Print help
```

## Acknowledgements

This project would not exist without the work it is built on:

- **[`crates-io-proxy`](https://github.com/ravenexp/crates-io-proxy)** by Sergey Kvachonok
  (ravenexp) — the caching HTTP proxy core for the `crates.io` sparse index and crate download
  server that `chilled-crates` is forked from. Licensed under `MIT OR Apache-2.0`, carried over
  unchanged.
- **[menhera.org's crates.io cooldown proxy](https://www.menhera.org/crates-io-cooldown-proxy-mitigating-supply-chain-attacks/)**
  by metastable-void — the sparse-index age-gating approach that the supply-chain mitigation in
  `src/cooldown.rs` is ported from.
- **[`httpdate`](https://crates.io/crates/httpdate)** by Pyfisch — the IMF-fixdate
  formatting/parsing logic (and Howard Hinnant's civil-date algorithms it builds on) that
  `src/http/httpdate.rs` vendors in place of the crate.
