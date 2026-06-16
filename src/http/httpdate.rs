//! Minimal HTTP-date (RFC 7231 IMF-fixdate) formatting and parsing.
//!
//! Vendored in place of the `httpdate` crate: we only need to round-trip the
//! `Last-Modified` header for sparse-index entries, which crates.io serves as
//! IMF-fixdate (e.g. `Sun, 06 Nov 1994 08:49:37 GMT`). Date math uses Howard
//! Hinnant's civil-date algorithms.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SECS_PER_DAY: u64 = 86_400;
const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Formats a `SystemTime` as an IMF-fixdate string (always in GMT).
///
/// Times before the Unix epoch are clamped to the epoch (never produced for
/// real file mtimes).
pub(crate) fn fmt_http_date(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let days = (secs / SECS_PER_DAY) as i64;
    let (h, mi, s) = {
        let rem = secs % SECS_PER_DAY;
        (rem / 3600, (rem % 3600) / 60, rem % 60)
    };
    // 1970-01-01 was a Thursday (index 4 with Sunday = 0).
    let weekday = ((days + 4).rem_euclid(7)) as usize;
    let (y, m, d) = civil_from_days(days);

    format!(
        "{wd}, {d:02} {mon} {y:04} {h:02}:{mi:02}:{s:02} GMT",
        wd = DAYS[weekday],
        mon = MONTHS[(m - 1) as usize],
    )
}

/// Parses an IMF-fixdate string into a `SystemTime`, or `None` if malformed.
///
/// Lenient about the leading weekday token; the date/time fields must be the
/// fixed-width IMF-fixdate form. The obsolete RFC 850 / asctime formats are not
/// supported (crates.io does not use them).
pub(crate) fn parse_http_date(s: &str) -> Option<SystemTime> {
    // e.g. "Sun, 06 Nov 1994 08:49:37 GMT"
    let mut it = s.split_whitespace();
    let _weekday = it.next()?; // "Sun," — ignored
    let day: u32 = it.next()?.parse().ok()?;
    let month = month_index(it.next()?)?;
    let year: i32 = it.next()?.parse().ok()?;
    let time = it.next()?;
    if !matches!(it.next(), Some("GMT")) {
        return None;
    }

    let mut tp = time.split(':');
    let h: u64 = tp.next()?.parse().ok()?;
    let mi: u64 = tp.next()?.parse().ok()?;
    let sec: u64 = tp.next()?.parse().ok()?;
    if tp.next().is_some() || h > 23 || mi > 59 || sec > 60 {
        return None;
    }

    let days = days_from_civil(year, month, day)?;
    if days < 0 {
        return None;
    }
    let secs = days as u64 * SECS_PER_DAY + h * 3600 + mi * 60 + sec;
    Some(UNIX_EPOCH + Duration::from_secs(secs))
}

/// Month abbreviation → 1-based month number.
fn month_index(name: &str) -> Option<u32> {
    MONTHS.iter().position(|m| *m == name).map(|i| i as u32 + 1)
}

/// Civil date → days since 1970-01-01 (Hinnant `days_from_civil`).
fn days_from_civil(y: i32, m: u32, d: u32) -> Option<i64> {
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = i64::from(if m <= 2 { y - 1 } else { y });
    let m = i64::from(m);
    let d = i64::from(d);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * if m > 2 { m - 3 } else { m + 9 } + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// Days since 1970-01-01 → civil date (Hinnant `civil_from_days`).
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc_example_round_trip() {
        let s = "Sun, 06 Nov 1994 08:49:37 GMT";
        let t = parse_http_date(s).unwrap();
        // 784111777 is the canonical unix time for this date.
        assert_eq!(t.duration_since(UNIX_EPOCH).unwrap().as_secs(), 784_111_777);
        assert_eq!(fmt_http_date(t), s);
    }

    #[test]
    fn epoch() {
        let t = UNIX_EPOCH;
        assert_eq!(fmt_http_date(t), "Thu, 01 Jan 1970 00:00:00 GMT");
        assert_eq!(parse_http_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(t));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_http_date(""), None);
        assert_eq!(parse_http_date("not a date"), None);
        assert_eq!(parse_http_date("Sun, 06 Xxx 1994 08:49:37 GMT"), None);
        assert_eq!(parse_http_date("Sun, 06 Nov 1994 08:49:37 PST"), None);
    }
}
