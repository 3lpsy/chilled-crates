//! HTTP server bootstrap.

use axum::Router;
use log::info;

/// Server listening address.
pub(crate) enum ListenAddress {
    /// IP address + port.
    SocketAddr(String),
    /// Unix domain socket path.
    UnixPath(String),
}

/// Serves `app` on an already-bound TCP listener until killed.
///
/// Exposed for embedding/tests: bind a `TcpListener` on an ephemeral port,
/// read its `local_addr`, then drive the router over real HTTP. (The binary
/// uses [`serve`], which binds from a [`ListenAddress`].)
pub async fn serve_listener(listener: tokio::net::TcpListener, app: Router) {
    axum::serve(listener, app.into_make_service())
        .await
        .expect("HTTP server error");
}

/// Binds the listener and serves until killed.
pub(crate) async fn serve(listen_addr: ListenAddress, app: Router) {
    match listen_addr {
        ListenAddress::SocketAddr(addr) => {
            info!("proxy: starting HTTP server at: {addr}");
            let listener = tokio::net::TcpListener::bind(&addr)
                .await
                .unwrap_or_else(|e| panic!("failed to bind {addr}: {e}"));
            axum::serve(listener, app.into_make_service())
                .await
                .expect("HTTP server error");
        }
        ListenAddress::UnixPath(path) => {
            info!("proxy: starting HTTP server at Unix socket {path}");
            // Reap a stale socket file before binding.
            std::fs::remove_file(&path).ok();
            let listener = tokio::net::UnixListener::bind(&path)
                .unwrap_or_else(|e| panic!("failed to bind {path}: {e}"));
            axum::serve(listener, app.into_make_service())
                .await
                .expect("HTTP server error");
        }
    }
}
