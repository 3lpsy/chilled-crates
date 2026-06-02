//! `GET /` — minimal liveness status.

use axum::response::Response;

use crate::http::json_response;

/// Liveness body returned at `/`.
const STATUS_JSON: &str = r#"{"status":"running"}"#;

/// Handles `GET /`: a minimal liveness status, always available.
pub(crate) async fn handle_home() -> Response {
    json_response(200, STATUS_JSON.to_owned())
}
