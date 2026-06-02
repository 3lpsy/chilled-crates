#
# Dockerfile for the chilled-crates server application.
#
# chilled-crates is a friendly fork of crates-io-proxy (ravenexp) with added
# sparse-index age-gating; see README.md for attribution.
#

### First stage: Build the application itself.
FROM rust:alpine AS builder

WORKDIR /builds/chilled-crates

# Copy source data (see .dockerignore for excludes).
COPY . .

# Build deps: musl-dev plus the toolchain aws-lc-rs (rustls crypto backend,
# pulled in by reqwest's rustls-tls) needs on Alpine — a C/C++ compiler, cmake,
# and clang/libclang for bindgen.
RUN \
apk add --no-cache musl-dev build-base cmake clang-dev && \
cargo build --release

### Second stage: Copy the built application into the runtime image.
FROM alpine:latest AS runner

LABEL version="0.2.5"
LABEL description="chilled-crates: caching crates.io proxy with sparse-index age-gating"
LABEL maintainer="3lpsy"

# Install the compiled executable into the system.
COPY --from=builder /builds/chilled-crates/target/release/chilled-crates /usr/bin/chilled-crates

# Add the proxy service user and create the crate files cache directory writable by it.
RUN \
adduser -SHD -u 777 -h /var/empty -s /sbin/nologin -g "chilled-crates proxy" app && \
mkdir /var/cache/chilled-crates && \
chown app /var/cache/chilled-crates

# Switch to the service user to run the proxy process.
USER app
WORKDIR /var/empty

# Default sparse-index access endpoint.
EXPOSE 3080

# Configuration is read from the environment (or CLI flags) at run time. These
# are NOT declared with `ENV` on purpose: an `ENV` default would shadow the
# binary's own built-in default, creating two sources of truth. Pass any of
# them with `-e NAME=value` / `--env-file`; unset vars use the code defaults.
#
#   INDEX_CRATES_IO_URL                upstream index URL   (https://index.crates.io/)
#   CRATES_IO_URL                      upstream download URL (https://crates.io/)
#   CRATES_IO_PROXY_URL                this proxy's external URL (http://localhost:3080/)
#   CRATES_IO_PROXY_CACHE_DIR          cache directory      (/var/cache/chilled-crates)
#   CRATES_IO_PROXY_CACHE_TTL          index entry TTL, seconds (3600)
#   CRATES_IO_PROXY_COOLDOWN           age-gate window, e.g. 7d (0 = off)
#   CRATES_IO_PROXY_COOLDOWN_OVERRIDES comma-separated crates exempt from cooldown
#   CRATES_IO_PROXY_ENABLE_METRICS     1/true to expose cached crates at /metrics
#   LOG_LEVEL                          error|warn|info|debug|trace|off (info)
#   RUST_LOG                           overrides the log level (module filters allowed)
#
# Note: the default cache dir is baked into the image (created + owned above);
# override CRATES_IO_PROXY_CACHE_DIR only if you mount a different writable path.

# Run the proxy server (info logging to stdout). ENTRYPOINT (not CMD) so that
# flags passed to `docker run <image> --flag ...` append to the binary instead
# of replacing it.
ENTRYPOINT ["chilled-crates"]
