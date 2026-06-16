//! HTTP plumbing shared across routes: response builders, upstream fetch, and
//! HTTP-date handling.

pub(crate) mod fetch;
pub(crate) mod httpdate;
pub(crate) mod response;

pub(crate) use self::httpdate::{fmt_http_date, parse_http_date};
pub(crate) use fetch::{read_capped, FetchError};
pub(crate) use response::{crate_response, error_response, format_json_error, json_response};
