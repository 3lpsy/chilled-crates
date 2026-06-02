//! `GET /healthz` — health-check endpoint.

use axum::{body::Body, http::header, response::Response};

/// Handles `GET /healthz`: a health-check endpoint for probes/load balancers.
///
/// Follows the conventional `healthz` contract — HTTP 200 with a plain `ok`
/// body — which is all a Kubernetes/LB liveness probe inspects.
pub(crate) async fn handle_healthz() -> Response {
    Response::builder()
        .status(200)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from("ok\n"))
        .expect("valid healthz response")
}
