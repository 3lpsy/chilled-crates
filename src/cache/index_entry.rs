//! Registry index entry handling helpers

use std::fmt::{Display, Formatter, Result};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use crate::http::{fmt_http_date, parse_http_date};

/// Registry index entry structure
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct IndexEntry {
    /// Crate name
    name: String,
    /// HTTP entity tag header
    etag: Option<String>,
    /// Index file modification time
    mtime: Option<SystemTime>,
    /// Last index entry update check time
    atime: Option<Instant>,
}

impl Display for IndexEntry {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        f.write_str(&self.name)
    }
}

impl IndexEntry {
    /// Creates a registry index entry object for a crate.
    #[must_use]
    pub fn new(name: &str) -> Self {
        IndexEntry {
            name: name.to_owned(),
            etag: None,
            mtime: None,
            atime: None,
        }
    }

    /// Creates an entry from the sparse index URL path.
    ///
    /// Rejects crate names outside the crates.io character set, closing off
    /// SSRF and path-traversal via crafted index paths.
    #[must_use]
    pub fn try_from_index_url(url: &str) -> Option<Self> {
        let mut i = url.split('/');

        let name = match i.next() {
            Some("1" | "2") => match (i.next(), i.next()) {
                (Some(name), None) => name,
                _ => return None,
            },
            _ => match (i.next(), i.next(), i.next()) {
                (Some(_), Some(name), None) => name,
                _ => return None,
            },
        };

        crate::valid::is_crate_name(name).then(|| IndexEntry::new(name))
    }

    /// Gets the crate name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Checks if this index entry file contents is the same
    /// as `other` according to the associated metadata.
    #[must_use]
    pub fn is_equivalent(&self, other: &IndexEntry) -> bool {
        (self.etag().is_some() && (self.etag() == other.etag()))
            || (self.last_modified().is_some() && (self.last_modified() == other.last_modified()))
    }

    /// Checks if this index entry is expired according for the TTL given.
    #[must_use]
    pub fn is_expired_with_ttl(&self, ttl: &Duration) -> bool {
        self.atime.is_some_and(|atime| atime.elapsed() > *ttl)
    }

    /// Gets the HTTP entity tag metadata.
    #[must_use]
    pub fn etag(&self) -> Option<&str> {
        self.etag.as_deref()
    }

    /// Gets the HTTP Last-Modified metadata.
    #[must_use]
    pub fn last_modified(&self) -> Option<String> {
        self.mtime.map(fmt_http_date)
    }

    /// Gets the file modification time metadata.
    #[must_use]
    pub fn mtime(&self) -> Option<SystemTime> {
        self.mtime
    }

    /// Sets the HTTP entity tag metadata.
    pub fn set_etag(&mut self, etag: &str) {
        self.etag = Some(etag.to_owned());
    }

    /// Sets the HTTP Last-Modified metadata.
    pub fn set_last_modified(&mut self, last_modified: &str) {
        self.mtime = parse_http_date(last_modified);
    }

    /// Sets the file modification time metadata.
    pub fn set_mtime(&mut self, mtime: SystemTime) {
        self.mtime = Some(mtime);
    }

    /// Updates the last upstream server access time metadata.
    pub fn set_last_updated(&mut self) {
        self.atime = Some(Instant::now());
    }

    /// Builds the index entry download URL (relative).
    #[must_use]
    pub fn to_index_url(&self) -> String {
        let name = &self.name;

        match name.len() {
            0 => String::new(),
            sz @ (1 | 2) => format!("{sz}/{name}"),
            3 => format!("3/{first}/{name}", first = &name[..1]),
            _ => format!(
                "{first}/{second}/{name}",
                first = &name[0..2],
                second = &name[2..4]
            ),
        }
    }

    /// Builds the relative index entry file path for cache storage.
    #[must_use]
    pub fn to_file_path(&self) -> PathBuf {
        PathBuf::from(self.to_index_url())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_url() {
        assert_eq!(IndexEntry::try_from_index_url(""), None);
        assert_eq!(IndexEntry::try_from_index_url("abc"), None);
        assert_eq!(IndexEntry::try_from_index_url("a/bc"), None);
        assert_eq!(IndexEntry::try_from_index_url("a/b/c/d"), None);

        assert_eq!(
            IndexEntry::try_from_index_url("1/a"),
            Some(IndexEntry::new("a"))
        );
        assert_eq!(
            IndexEntry::try_from_index_url("2/ab"),
            Some(IndexEntry::new("ab"))
        );
        assert_eq!(
            IndexEntry::try_from_index_url("3/a/abc"),
            Some(IndexEntry::new("abc"))
        );
        assert_eq!(
            IndexEntry::try_from_index_url("ab/cd/abcd"),
            Some(IndexEntry::new("abcd"))
        );
    }

    #[test]
    fn test_to_url() {
        assert_eq!(IndexEntry::new("").to_index_url(), "");
        assert_eq!(IndexEntry::new("a").to_index_url(), "1/a");
        assert_eq!(IndexEntry::new("ab").to_index_url(), "2/ab");
        assert_eq!(IndexEntry::new("abc").to_index_url(), "3/a/abc");
        assert_eq!(IndexEntry::new("abcd").to_index_url(), "ab/cd/abcd");
    }

    #[test]
    fn equivalent_matches_on_etag() {
        let mut a = IndexEntry::new("serde");
        let mut b = IndexEntry::new("serde");
        a.set_etag("\"x\"");
        b.set_etag("\"x\"");
        assert!(a.is_equivalent(&b));
        b.set_etag("\"y\"");
        assert!(!a.is_equivalent(&b));
    }

    #[test]
    fn equivalent_matches_on_last_modified_when_no_etag() {
        let when = "Sun, 06 Nov 1994 08:49:37 GMT";
        let mut a = IndexEntry::new("serde");
        let mut b = IndexEntry::new("serde");
        a.set_last_modified(when);
        b.set_last_modified(when);
        assert!(a.is_equivalent(&b));
        b.set_last_modified("Mon, 07 Nov 1994 08:49:37 GMT");
        assert!(!a.is_equivalent(&b));
    }

    #[test]
    fn equivalent_false_when_no_metadata() {
        // Two bare entries carry no validators, so equivalence cannot be proven.
        let a = IndexEntry::new("serde");
        let b = IndexEntry::new("serde");
        assert!(!a.is_equivalent(&b));
    }

    #[test]
    fn expiry_tracks_atime_and_ttl() {
        // No access time recorded yet -> never considered expired.
        let mut entry = IndexEntry::new("serde");
        assert!(!entry.is_expired_with_ttl(&Duration::from_secs(3600)));

        entry.set_last_updated();
        // A generous TTL is not yet expired; a zero TTL is, once any time passes.
        assert!(!entry.is_expired_with_ttl(&Duration::from_secs(3600)));
        std::thread::sleep(Duration::from_millis(2));
        assert!(entry.is_expired_with_ttl(&Duration::from_secs(0)));
    }
}
