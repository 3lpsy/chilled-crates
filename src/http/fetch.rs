//! Upstream HTTP fetch helpers shared by the index and download routes.

use std::fmt::Display;

/// An upstream fetch failure: either a transport/decode error or a body that
/// exceeded the configured size limit.
pub(crate) enum FetchError {
    /// Connection, TLS, or decode failure.
    Http(reqwest::Error),
    /// Body exceeded the size limit (declared or observed).
    TooLarge,
}

impl Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Http(e) => write!(f, "{e}"),
            FetchError::TooLarge => f.write_str("response body too large"),
        }
    }
}

/// Reads a response body into memory, capped at `max` bytes.
///
/// Rejects up front when `Content-Length` already exceeds the cap, and again
/// while streaming so a chunked (no `Content-Length`) response cannot exhaust
/// memory. Returns [`FetchError::TooLarge`] rather than truncating, so callers
/// never serve a partial body.
pub(crate) async fn read_capped(
    response: &mut reqwest::Response,
    max: usize,
) -> Result<Vec<u8>, FetchError> {
    if let Some(len) = response.content_length() {
        if len as usize > max {
            return Err(FetchError::TooLarge);
        }
    }

    let mut data = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(FetchError::Http)? {
        if data.len().saturating_add(chunk.len()) > max {
            return Err(FetchError::TooLarge);
        }
        data.extend_from_slice(&chunk);
    }
    Ok(data)
}
