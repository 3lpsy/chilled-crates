//! `chilled-crates` binary entry point.
//!
//! All logic lives in the library crate (see `src/lib.rs`); this is a thin
//! wrapper that runs the server on a `tokio` runtime.

#[tokio::main]
async fn main() {
    chilled_crates::run().await;
}
