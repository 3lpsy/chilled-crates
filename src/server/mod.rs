//! HTTP server bootstrap and shared request state.

pub(crate) mod serve;
pub(crate) mod state;

pub(crate) use serve::{ListenAddress, serve};
pub use serve::serve_listener;
pub(crate) use state::AppState;
