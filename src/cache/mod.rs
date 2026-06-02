//! Local caches and the registry data types they key on.
//!
//! - [`file`] — on-disk cache of sparse-index entry files and `.crate` files.
//! - [`metadata`] — in-memory cache of index entry metadata (etag / mtime).
//! - [`memo`] — in-memory cache of age-gated (filtered) index bodies.
//! - [`crate_info`] / [`index_entry`] — crate/version + index entry models
//!   (name parsing and cache file paths).

pub(crate) mod crate_info;
pub(crate) mod file;
pub(crate) mod index_entry;
pub(crate) mod memo;
pub(crate) mod metadata;

pub(crate) use crate_info::CrateInfo;
pub(crate) use file::{
    cache_fetch_crate, cache_fetch_index_entry, cache_store_crate, cache_store_index_entry,
    cache_try_find_index_entry,
};
pub(crate) use index_entry::IndexEntry;
pub(crate) use memo::{FilteredMemo, MEMO_BUCKET_SECS};
pub(crate) use metadata::MetadataCache;
