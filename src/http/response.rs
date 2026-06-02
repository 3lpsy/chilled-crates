//! Shared HTTP response builders.

use std::fmt::Display;

use axum::{body::Body, http::header, response::Response};
use bytes::Bytes;

use crate::constants::{CRATE_CTYPE, JSON_CTYPE};

/// Builds an error response with an empty body.
pub(crate) fn error_response(status: u16) -> Response {
    Response::builder()
        .status(status)
        .body(Body::empty())
        .expect("valid error response")
}

/// Builds a JSON response (used for `config.json` and forwarded API errors).
pub(crate) fn json_response(status: u16, body: String) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, JSON_CTYPE)
        .body(Body::from(body))
        .expect("valid json response")
}

/// Builds a crate-download response from raw `.crate` bytes.
pub(crate) fn crate_response(data: Bytes) -> Response {
    Response::builder()
        .status(200)
        .header(header::CONTENT_TYPE, CRATE_CTYPE)
        .body(Body::from(data))
        .expect("valid crate response")
}

/// Formats a crates API JSON error body.
///
/// The error text comes from `reqwest`/internal sources and can contain `"`,
/// `\`, or control characters, so it is JSON-string-escaped to keep the body
/// well-formed.
pub(crate) fn format_json_error(error: impl Display) -> String {
    format!(
        r#"{{"errors":[{{"detail":"{}"}}]}}"#,
        json_escape(&error.to_string())
    )
}

/// Escapes a string for safe embedding inside a JSON string literal (per
/// RFC 8259): the mandatory `"` and `\` plus the C0 control characters.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_json_metacharacters() {
        assert_eq!(json_escape(r#"a"b\c"#), r#"a\"b\\c"#);
        assert_eq!(json_escape("line\nbreak\ttab"), "line\\nbreak\\ttab");
        assert_eq!(json_escape("ctrl\u{0001}"), "ctrl\\u0001");
        // Non-ASCII passes through unchanged (valid UTF-8 in a JSON string).
        assert_eq!(json_escape("café"), "café");
    }

    #[test]
    fn error_body_is_well_formed() {
        let body = format_json_error("bad \"quote\" and \\slash");
        assert_eq!(
            body,
            r#"{"errors":[{"detail":"bad \"quote\" and \\slash"}]}"#
        );
    }
}
