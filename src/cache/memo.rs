//! Memoization of age-gated (filtered) index bodies.
//!
//! Filtering an index entry is CPU work repeated on every served request. The
//! memo caches the filtered bytes keyed by `(crate, source-validator, cutoff
//! bucket)`, so repeated hits reuse the result instead of re-filtering, while a
//! mismatch on either tag guarantees stale or time-shifted output is never
//! returned.

use std::collections::HashMap;
use std::sync::RwLock;

use bytes::Bytes;
use log::debug;

/// Granularity (seconds) of the filtered-output memo cutoff bucket.
///
/// The age-gating cutoff advances every second, but the *filtered output* only
/// changes when a version crosses the boundary. Bucketing the cutoff to the
/// hour makes the memo key stable, so repeated hits within the hour reuse the
/// filtered bytes; the cost is at most ~1h of aging-in jitter, irrelevant for a
/// day-scale cooldown.
pub(crate) const MEMO_BUCKET_SECS: u64 = 3600;

/// Maximum number of crates held in the filtered-output memo before it is
/// cleared. Each crate keeps at most one entry, so this bounds memory use.
const MEMO_MAX_ENTRIES: usize = 8192;

/// One memoized filtered index body, tagged with the source identity and the
/// cooldown bucket it was produced for.
struct MemoEntry {
    /// Source content validator (upstream etag or last-modified).
    validator: String,
    /// Cutoff bucket (cutoff / [`MEMO_BUCKET_SECS`]) the body was filtered for.
    bucket: u64,
    /// The filtered bytes (cheap to clone).
    data: Bytes,
}

/// Bounded, concurrent cache of filtered index bodies keyed by crate name.
///
/// At most one entry per crate is retained; a lookup hits only when both the
/// source validator and the cutoff bucket still match, so stale or
/// time-shifted output is never reused.
pub(crate) struct FilteredMemo {
    inner: RwLock<HashMap<String, MemoEntry>>,
}

impl FilteredMemo {
    /// Creates an empty memo.
    pub(crate) fn new() -> Self {
        FilteredMemo {
            inner: RwLock::new(HashMap::new()),
        }
    }

    /// Returns the memoized filtered body for `name` if it was produced from
    /// the same source `validator` and cutoff `bucket`.
    pub(crate) fn get(&self, name: &str, validator: &str, bucket: u64) -> Option<Bytes> {
        let map = self.inner.read().unwrap();
        let entry = map.get(name)?;
        (entry.validator == validator && entry.bucket == bucket).then(|| entry.data.clone())
    }

    /// Stores the filtered body for `name`, evicting everything if the memo is
    /// full and this is a new crate (keeps memory bounded).
    pub(crate) fn put(&self, name: String, validator: String, bucket: u64, data: Bytes) {
        let mut map = self.inner.write().unwrap();
        if map.len() >= MEMO_MAX_ENTRIES && !map.contains_key(&name) {
            debug!("memo: cleared filtered-index memo at capacity");
            map.clear();
        }
        map.insert(
            name,
            MemoEntry {
                validator,
                bucket,
                data,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memo_respects_validator_and_bucket() {
        let memo = FilteredMemo::new();
        memo.put("a".into(), "etag1".into(), 10, Bytes::from_static(b"x"));
        assert_eq!(memo.get("a", "etag1", 10), Some(Bytes::from_static(b"x")));
        // Different source content -> miss.
        assert_eq!(memo.get("a", "etag2", 10), None);
        // Different cutoff bucket -> miss.
        assert_eq!(memo.get("a", "etag1", 11), None);
        // Unknown crate -> miss.
        assert_eq!(memo.get("b", "etag1", 10), None);
    }
}
