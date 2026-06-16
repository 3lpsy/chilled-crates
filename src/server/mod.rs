//! HTTP server bootstrap and shared request state.

pub(crate) mod serve;
pub(crate) mod state;

pub use serve::serve_listener;
pub(crate) use serve::{serve, ListenAddress};
pub(crate) use state::AppState;
