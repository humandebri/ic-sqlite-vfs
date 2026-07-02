//! Logical `/main.db` access backed by an in-place stable-memory image.
//!
//! SQLite sees a contiguous file. Internally, logical page `n` lives at
//! `db_base_offset + n * SQLITE_PAGE_SIZE`.

mod commit;
mod logical;
mod ops;
mod state;
mod zero_extents;

#[cfg(test)]
use crate::config::SUPERBLOCK_SIZE;
#[cfg(test)]
use crate::sqlite_vfs::overlay::Overlay;
#[cfg(test)]
use crate::stable::memory::StableMemoryError;
#[cfg(test)]
use crate::stable::meta::{
    fnv1a64, Superblock, ZeroExtent, FLAG_CHECKSUM_REFRESHING, FLAG_CHECKSUM_STALE,
    MAX_ZERO_EXTENTS,
};
#[allow(unused_imports)]
pub(crate) use logical::page_count_for_size;
#[cfg(test)]
use logical::{
    active_page_count, checked_add, fold_fnv1a64, import_offset, page_len, page_physical_offset,
    page_size,
};
#[cfg(test)]
pub use ops::{begin_import, cancel_import, finish_import, import_chunk};
#[allow(unused_imports)]
pub(crate) use ops::{
    begin_update, commit_update, ensure_current_layout, file_size, read_at, read_base_at,
    read_base_at_with_block, read_base_at_with_page_cache, read_base_page, rollback_update,
    truncate, write_at,
};
#[allow(unused_imports)]
pub use ops::{
    checksum, compact, export_chunk, refresh_checksum, refresh_checksum_chunk, storage_stats,
};
pub(crate) use state::PageOffsetCache;
#[cfg(test)]
use state::FAR_PAGE_NO;
#[cfg(test)]
pub(crate) use state::{clear_failpoint, set_failpoint, StableBlobFailpoint};
#[allow(unused_imports)]
pub use state::{ChecksumRefresh, StorageStats};
#[cfg(test)]
use zero_extents::{
    add_zero_extent, enforce_temporary_zero_extent_limit, normalize_zero_extents,
    subtract_zero_extent, zero_extents_after_commit,
};

#[cfg(test)]
mod tests;
