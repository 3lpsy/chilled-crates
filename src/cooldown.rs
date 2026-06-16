//! Sparse-index age-gating ("cooldown") filter.
//!
//! Strips crate version lines from a sparse-index entry whose `pubtime` is
//! newer than a cutoff timestamp. Holding back too-new versions gives the
//! community time to detect and yank a malicious release before `cargo update`
//! can pull it.
//!
//! The line-filtering and date-parsing logic is ported from menhera.org's
//! crates.io cooldown proxy
//! (<https://www.menhera.org/crates-io-cooldown-proxy-mitigating-supply-chain-attacks/>);
//! it is self-contained (depends only on `std`). Each sparse-index line is
//! compact JSON, so the `pubtime` field is extracted with a targeted byte scan
//! rather than a full JSON parse — this avoids allocating a `serde_json::Value`
//! (and walking large `deps` arrays) for every line of every served entry.

use std::time::Duration;

/// Number of seconds in one calendar day.
const SECS_PER_DAY: u64 = 86_400;

/// Filter raw sparse-index bytes, dropping any version line whose `pubtime` is
/// newer than `cutoff` (unix seconds).
///
/// If the body is not valid UTF-8 it is returned unchanged (the proxy then
/// passes it through untouched, same as the upstream would).
pub fn filter_index(data: &[u8], cutoff: u64) -> Vec<u8> {
    match std::str::from_utf8(data) {
        Ok(body) => filter_body(body, cutoff),
        Err(_) => data.to_vec(),
    }
}

/// Compute the cutoff (unix seconds) for a cooldown window measured back from
/// `now_secs`. A zero window means "no filtering" and yields `None`.
pub fn cutoff_from(now_secs: u64, cooldown: Duration) -> Option<u64> {
    let secs = cooldown.as_secs();
    if secs == 0 {
        None
    } else {
        Some(now_secs.saturating_sub(secs))
    }
}

/// Extract the `pubtime` (unix seconds) of a specific `version` from a
/// sparse-index body, or `None` if that version is absent or has no parseable
/// `pubtime`. Used by `--restrict-downloads` to age-gate the download path.
///
/// Matches the compact `"vers":"<version>"` token (closing quote included, so
/// `1.0` does not match `1.0.1`).
pub(crate) fn version_pubtime(body: &str, version: &str) -> Option<u64> {
    let needle = format!("\"vers\":\"{version}\"");
    body.lines()
        .find(|line| line.contains(&needle))
        .and_then(line_pubtime_secs)
}

/// Parse a cooldown duration string.
///
/// Accepts a bare integer (interpreted as seconds) or an integer followed by a
/// single unit suffix:
///
/// | suffix | unit    |
/// |--------|---------|
/// | `s`    | seconds |
/// | `m`    | minutes |
/// | `h`    | hours   |
/// | `d`    | days    |
/// | `w`    | weeks   |
///
/// Months and years are intentionally unsupported: a supply-chain cooldown is a
/// short window, and dropping them keeps `m` unambiguously *minutes* (no
/// ISO-8601 / `humantime` month-vs-minute confusion). Returns an error string
/// on an empty input, an unknown suffix, a non-numeric value, or overflow.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty duration".to_string());
    }

    // A trailing unit suffix, if any, is the last byte (all units are ASCII).
    let last = s.as_bytes()[s.len() - 1];
    let (digits, mult) = if last.is_ascii_digit() {
        (s, 1)
    } else {
        let mult = match last {
            b's' => 1,
            b'm' => 60,
            b'h' => 3_600,
            b'd' => SECS_PER_DAY,
            b'w' => 7 * SECS_PER_DAY,
            _ => {
                return Err(format!(
                    "invalid duration unit '{}' in '{s}' (use s, m, h, d, or w)",
                    last as char
                ));
            }
        };
        (&s[..s.len() - 1], mult)
    };

    let value: u64 = digits
        .parse()
        .map_err(|_| format!("invalid duration value in '{s}'"))?;

    value
        .checked_mul(mult)
        .map(Duration::from_secs)
        .ok_or_else(|| format!("duration '{s}' is too large"))
}

/// Walk the sparse-index body line by line, dropping any line whose `pubtime`
/// is strictly newer than `cutoff`. Lines without a `pubtime` (blanks or
/// malformed JSON) are kept verbatim, newlines preserved.
fn filter_body(body: &str, cutoff: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len());
    for line in body.split_inclusive('\n') {
        let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
        if trimmed.is_empty() {
            out.extend_from_slice(line.as_bytes());
            continue;
        }
        match line_pubtime_secs(trimmed) {
            Some(secs) if secs > cutoff => {}
            _ => out.extend_from_slice(line.as_bytes()),
        }
    }
    out
}

/// Extract the `pubtime` field from a single sparse-index JSON line and return
/// it as unix seconds, or `None` if the field is missing or unparseable.
///
/// Scans for the `"pubtime"` *key* (one followed by a colon, so a string
/// *value* that happens to read `pubtime` is skipped) and reads its quoted
/// timestamp. Because a JSON string can never contain an unescaped `"`, the
/// value runs cleanly to the next `"`. Returning `None` keeps the line — but
/// for a well-formed crates.io entry the key is always found, so a too-new
/// version is never missed.
fn line_pubtime_secs(line: &str) -> Option<u64> {
    let mut start = 0;
    while let Some(rel) = line[start..].find("\"pubtime\"") {
        let after = line[start + rel + "\"pubtime\"".len()..].trim_start();
        match after.strip_prefix(':') {
            Some(rest) => {
                let rest = rest.trim_start().strip_prefix('"')?;
                let end = rest.find('"')?;
                return parse_rfc3339z(&rest[..end]);
            }
            // This occurrence was a string value, not a key — keep looking.
            None => start += rel + "\"pubtime\"".len(),
        }
    }
    None
}

/// Parse `YYYY-MM-DDTHH:MM:SS[.fff]Z` into unix seconds. Fractional seconds are truncated.
fn parse_rfc3339z(s: &str) -> Option<u64> {
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let mut dp = date.split('-');
    let y: i32 = dp.next()?.parse().ok()?;
    let mo: u32 = dp.next()?.parse().ok()?;
    let d: u32 = dp.next()?.parse().ok()?;
    if dp.next().is_some() {
        return None;
    }
    let mut tp = time.split(':');
    let h: u64 = tp.next()?.parse().ok()?;
    let mi: u64 = tp.next()?.parse().ok()?;
    let sec: u64 = tp.next()?.split('.').next()?.parse().ok()?;
    if tp.next().is_some() || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let days = days_since_epoch(y, mo, d)?;
    if days < 0 {
        return None;
    }
    Some(days as u64 * SECS_PER_DAY + h * 3_600 + mi * 60 + sec)
}

/// Civil UTC date → days since 1970-01-01. Based on Howard Hinnant's `days_from_civil`.
fn days_since_epoch(y: i32, m: u32, d: u32) -> Option<i64> {
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let m = m as i64;
    let d = d as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y as i64 - era as i64 * 400;
    let m_adj = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * m_adj + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era as i64 * 146_097 + doe - 719_468)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_units() {
        assert_eq!(parse_duration("3600"), Ok(Duration::from_secs(3600)));
        assert_eq!(parse_duration("3600s"), Ok(Duration::from_secs(3600)));
        assert_eq!(parse_duration("30m"), Ok(Duration::from_secs(1800)));
        assert_eq!(parse_duration("12h"), Ok(Duration::from_secs(43_200)));
        assert_eq!(parse_duration("7d"), Ok(Duration::from_secs(604_800)));
        assert_eq!(parse_duration("1w"), Ok(Duration::from_secs(604_800)));
        assert_eq!(parse_duration("0"), Ok(Duration::from_secs(0)));
        assert_eq!(parse_duration(" 7d "), Ok(Duration::from_secs(604_800)));
    }

    #[test]
    fn duration_rejects() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("1M").is_err()); // months unsupported
        assert!(parse_duration("1y").is_err()); // years unsupported
        assert!(parse_duration("abc").is_err());
        assert!(parse_duration("d").is_err());
        assert!(parse_duration("7dd").is_err());
    }

    #[test]
    fn duration_overflow() {
        // A bare value at the u64 ceiling parses; multiplying by a unit overflows.
        let max = u64::MAX.to_string();
        assert_eq!(parse_duration(&max), Ok(Duration::from_secs(u64::MAX)));
        let err = parse_duration(&format!("{max}w")).unwrap_err();
        assert!(err.contains("too large"), "unexpected error: {err}");
        // A value that does not even fit in u64 is a parse error, not overflow.
        assert!(parse_duration("99999999999999999999999").is_err());
    }

    #[test]
    fn cutoff_disabled_when_zero() {
        assert_eq!(cutoff_from(1_000_000, Duration::from_secs(0)), None);
        assert_eq!(
            cutoff_from(1_000_000, Duration::from_secs(SECS_PER_DAY)),
            Some(1_000_000 - SECS_PER_DAY)
        );
    }

    #[test]
    fn filter_drops_too_new() {
        let body = concat!(
            r#"{"name":"a","vers":"1","pubtime":"2026-01-01T00:00:00Z"}"#,
            "\n",
            r#"{"name":"a","vers":"2","pubtime":"2026-03-20T00:00:00Z"}"#,
            "\n",
        );
        // cutoff = 2026-02-01: the 03-20 release is newer → dropped.
        let cutoff = parse_rfc3339z("2026-02-01T00:00:00Z").unwrap();
        let out = String::from_utf8(filter_body(body, cutoff)).unwrap();
        assert!(out.contains(r#""vers":"1""#));
        assert!(!out.contains(r#""vers":"2""#));
    }

    #[test]
    fn filter_keeps_lines_without_pubtime() {
        // Blank lines, lines with no pubtime, and a missing trailing newline are
        // all preserved verbatim, regardless of cutoff.
        let body = "\n{\"name\":\"a\",\"vers\":\"1\"}\nnot json";
        let out = String::from_utf8(filter_body(body, 0)).unwrap();
        assert_eq!(out, body);
    }

    #[test]
    fn filter_preserves_crlf_endings() {
        let body = concat!(
            "{\"vers\":\"1\",\"pubtime\":\"2026-01-01T00:00:00Z\"}\r\n",
            "{\"vers\":\"2\",\"pubtime\":\"2026-03-20T00:00:00Z\"}\r\n",
        );
        let cutoff = parse_rfc3339z("2026-02-01T00:00:00Z").unwrap();
        let out = String::from_utf8(filter_body(body, cutoff)).unwrap();
        // The kept line retains its CRLF; the too-new line is dropped whole.
        assert_eq!(
            out,
            "{\"vers\":\"1\",\"pubtime\":\"2026-01-01T00:00:00Z\"}\r\n"
        );
    }

    #[test]
    fn filter_keeps_line_at_cutoff_boundary() {
        // Only strictly-newer-than-cutoff is dropped; pubtime == cutoff stays.
        let pubtime = "2026-03-20T00:00:00Z";
        let cutoff = parse_rfc3339z(pubtime).unwrap();
        let body = format!("{{\"vers\":\"1\",\"pubtime\":\"{pubtime}\"}}\n");
        let out = String::from_utf8(filter_body(&body, cutoff)).unwrap();
        assert_eq!(out, body);
        // One second older a cutoff and the same line is dropped.
        assert!(String::from_utf8(filter_body(&body, cutoff - 1))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn filter_index_passes_through_non_utf8() {
        // Invalid UTF-8 is returned untouched rather than mangled.
        let data = [0xff, 0xfe, 0x00, 0x01];
        assert_eq!(filter_index(&data, 0), data.to_vec());
    }

    #[test]
    fn version_pubtime_finds_exact_version() {
        let body = concat!(
            r#"{"name":"a","vers":"1.0.0","pubtime":"2026-01-01T00:00:00Z"}"#,
            "\n",
            r#"{"name":"a","vers":"1.0.1","pubtime":"2026-03-20T00:00:00Z"}"#,
            "\n",
        );
        assert_eq!(
            version_pubtime(body, "1.0.1"),
            parse_rfc3339z("2026-03-20T00:00:00Z")
        );
        // Absent version -> None.
        assert_eq!(version_pubtime(body, "9.9.9"), None);
    }

    #[test]
    fn version_pubtime_requires_full_version_match() {
        // The closing quote in the needle prevents `1.0` matching `1.0.1`.
        let body = r#"{"name":"a","vers":"1.0.1","pubtime":"2026-03-20T00:00:00Z"}"#;
        assert_eq!(version_pubtime(body, "1.0"), None);
        assert_eq!(
            version_pubtime(body, "1.0.1"),
            parse_rfc3339z("2026-03-20T00:00:00Z")
        );
    }

    #[test]
    fn version_pubtime_none_without_pubtime() {
        let body = r#"{"name":"a","vers":"1.0.0"}"#;
        assert_eq!(version_pubtime(body, "1.0.0"), None);
    }

    #[test]
    fn rfc3339_epoch() {
        assert_eq!(parse_rfc3339z("1970-01-01T00:00:00Z"), Some(0));
    }

    #[test]
    fn rfc3339_sample() {
        // 2026-03-20T03:13:45Z
        let got = parse_rfc3339z("2026-03-20T03:13:45Z").unwrap();
        // 2026-03-20 is day 20532 since 1970-01-01 (56y * 365 + 14 leap days + 78 days into 2026).
        assert_eq!(got, 20532 * 86_400 + 3 * 3600 + 13 * 60 + 45);
    }

    #[test]
    fn rfc3339_fractional() {
        assert_eq!(
            parse_rfc3339z("2026-03-20T03:13:45.999Z"),
            parse_rfc3339z("2026-03-20T03:13:45Z"),
        );
    }

    #[test]
    fn line_with_pubtime() {
        let line = r#"{"name":"a","vers":"1","pubtime":"2026-03-20T03:13:45Z"}"#;
        assert_eq!(
            line_pubtime_secs(line),
            parse_rfc3339z("2026-03-20T03:13:45Z")
        );
    }

    #[test]
    fn line_without_pubtime() {
        let line = r#"{"name":"a","vers":"1"}"#;
        assert_eq!(line_pubtime_secs(line), None);
    }

    #[test]
    fn line_pubtime_realistic() {
        // Compact crates.io-style line with deps before pubtime.
        let line = r#"{"name":"serde","vers":"1.0.1","deps":[{"name":"x","req":"^1"}],"cksum":"ab","features":{},"yanked":false,"pubtime":"2026-03-20T03:13:45Z"}"#;
        assert_eq!(
            line_pubtime_secs(line),
            parse_rfc3339z("2026-03-20T03:13:45Z")
        );
    }

    #[test]
    fn line_pubtime_value_not_key_is_ignored() {
        // A string *value* reading "pubtime" must not be mistaken for the key;
        // the real key still wins.
        let line = r#"{"note":"pubtime","pubtime":"2026-03-20T03:13:45Z"}"#;
        assert_eq!(
            line_pubtime_secs(line),
            parse_rfc3339z("2026-03-20T03:13:45Z")
        );
        // ...and with no real key, the value occurrence yields nothing.
        assert_eq!(line_pubtime_secs(r#"{"note":"pubtime"}"#), None);
    }
}
