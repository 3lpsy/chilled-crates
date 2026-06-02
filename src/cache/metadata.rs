//! Index entry file metadata cache (in-memory, per-server instance).

use std::collections::BTreeMap;
use std::sync::RwLock;

use log::debug;

use super::index_entry::IndexEntry;

/// Maximum number of crates held in the metadata cache before it is cleared.
/// Bounds memory use; mirrors the filtered-output memo's capacity policy. On
/// overflow the whole map is dropped (a cheap, rare event) and entries simply
/// repopulate from upstream on the next request.
const METADATA_MAX_ENTRIES: usize = 8192;

/// Bounded, concurrent cache of registry index entry metadata (etag / mtime),
/// keyed by crate name.
///
/// Held as instance state on [`crate::server::AppState`] (like the filtered-body
/// memo) rather than a process-wide global, so the server is cleanly
/// instantiable more than once.
pub(crate) struct MetadataCache {
    inner: RwLock<BTreeMap<String, IndexEntry>>,
}

impl MetadataCache {
    /// Creates an empty metadata cache.
    pub(crate) fn new() -> Self {
        MetadataCache {
            inner: RwLock::new(BTreeMap::new()),
        }
    }

    /// Caches the index entry metadata in memory.
    pub(crate) fn store(&self, entry: &IndexEntry) {
        let name = entry.name().to_owned();

        let mut map = self.inner.write().unwrap();
        if map.len() >= METADATA_MAX_ENTRIES && !map.contains_key(&name) {
            debug!("metadata: cleared index metadata cache at capacity");
            map.clear();
        }
        map.insert(name, entry.clone());
    }

    /// Fetches the cached index entry metadata from memory.
    pub(crate) fn fetch(&self, name: &str) -> Option<IndexEntry> {
        self.inner.read().unwrap().get(name).map(ToOwned::to_owned)
    }

    /// Erases the cached index entry metadata from memory.
    pub(crate) fn invalidate(&self, entry: &IndexEntry) {
        self.inner.write().unwrap().remove(entry.name());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_fetch_invalidate_round_trip() {
        let cache = MetadataCache::new();
        let mut entry = IndexEntry::new("serde");
        entry.set_etag("\"abc\"");

        assert_eq!(cache.fetch("serde"), None);
        cache.store(&entry);
        assert_eq!(cache.fetch("serde"), Some(entry.clone()));
        cache.invalidate(&entry);
        assert_eq!(cache.fetch("serde"), None);
    }
}
