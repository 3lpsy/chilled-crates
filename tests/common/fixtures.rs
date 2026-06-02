//! Test data and small helpers: sparse-index bodies, sentinel `pubtime`s, and
//! the sparse-path prefix rule.
#![allow(dead_code)]

use std::time::{SystemTime, UNIX_EPOCH};

/// A `pubtime` that is always older than any plausible cooldown cutoff.
pub const OLD: &str = "2000-01-01T00:00:00Z";
/// A `pubtime` that is always newer than the cutoff (for any cooldown > 0).
pub const TOO_NEW: &str = "2999-01-01T00:00:00Z";

/// Opaque `.crate` payload. The proxy treats crate bytes as opaque, so the exact
/// contents don't matter — only that they round-trip byte-for-byte.
pub const CRATE_BYTES: &[u8] = b"\x1f\x8b\x08\x00chilled-crates-test-crate-bytes\x00";

/// Builds a newline-terminated sparse-index body (one compact JSON line per
/// version) for `name`, in the shape crates.io serves.
pub fn ndjson(name: &str, versions: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (vers, pubtime) in versions {
        out.push_str(&format!(
            r#"{{"name":"{name}","vers":"{vers}","deps":[],"cksum":"{cksum}","features":{{}},"yanked":false,"pubtime":"{pubtime}"}}"#,
            cksum = "0".repeat(64),
        ));
        out.push('\n');
    }
    out
}

/// Formats `now + offset_secs` as an RFC3339 UTC timestamp (`...Z`). Used by the
/// boundary test, which needs a `pubtime` exactly at the cooldown cutoff.
pub fn rfc3339_from_now(offset_secs: i64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    rfc3339(now + offset_secs)
}

fn rfc3339(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Days since 1970-01-01 → civil date (Howard Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The relative sparse-index path for a crate name (the 1/2/3/4+ prefix rule).
pub fn index_rel(name: &str) -> String {
    match name.len() {
        1 => format!("1/{name}"),
        2 => format!("2/{name}"),
        3 => format!("3/{}/{name}", &name[..1]),
        _ => format!("{}/{}/{name}", &name[..2], &name[2..4]),
    }
}
