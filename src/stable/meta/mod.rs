//! Superblock encoding for the stable-memory SQLite image.
//!
//! The format is deliberately fixed-width little-endian data so upgrades can
//! inspect and migrate it without deserializing a Rust-specific structure.

use crate::config::{SQLITE_PAGE_SIZE, SUPERBLOCK_OFFSET};
use crate::stable::memory::{self, ContextId, StableMemoryError};
use std::cell::RefCell;
use std::collections::BTreeMap;

const MAGIC: [u8; 8] = *b"ICSQLITE";
const VERSION: u32 = 8;
pub const MAX_ZERO_EXTENTS: usize = 1024;
const ZERO_EXTENT_BYTES: usize = 16;
const EXTENTS_OFFSET: usize = 160;
const META_CHECKSUM_OFFSET: usize = 152;
const ENCODED_LEN: usize = EXTENTS_OFFSET + MAX_ZERO_EXTENTS * ZERO_EXTENT_BYTES;
pub const CURRENT_LAYOUT_VERSION: u64 = 8;
pub const FLAG_IMPORTING: u64 = 1;
pub const FLAG_CHECKSUM_STALE: u64 = 1 << 1;
pub const FLAG_CHECKSUM_REFRESHING: u64 = 1 << 2;

thread_local! {
    static SUPERBLOCK_CACHE: RefCell<BTreeMap<SuperblockCacheKey, Superblock>> = const { RefCell::new(BTreeMap::new()) };
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SuperblockCacheKey {
    context: ContextId,
    generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ZeroExtent {
    pub(crate) start_page: u64,
    pub(crate) end_page: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Superblock {
    pub magic: [u8; 8],
    pub version: u32,
    pub sqlite_page_size: u32,
    pub db_size: u64,
    pub schema_version: u64,
    pub last_tx_id: u64,
    pub flags: u64,
    pub checksum: u64,
    pub import_expected_checksum: u64,
    pub import_written_until: u64,
    pub import_total_size: u64,
    pub import_base_offset: u64,
    pub checksum_refresh_offset: u64,
    pub checksum_refresh_hash: u64,
    pub checksum_refresh_tx_id: u64,
    pub db_base_offset: u64,
    pub page_table_offset: u64,
    pub page_count: u64,
    pub layout_version: u64,
    pub(crate) zero_extents: Vec<ZeroExtent>,
    pub meta_checksum: u64,
}

impl Superblock {
    pub fn fresh() -> Self {
        let mut block = Self {
            magic: MAGIC,
            version: VERSION,
            sqlite_page_size: SQLITE_PAGE_SIZE,
            db_size: 0,
            schema_version: 0,
            last_tx_id: 0,
            flags: 0,
            checksum: 0,
            import_expected_checksum: 0,
            import_written_until: 0,
            import_total_size: 0,
            import_base_offset: 0,
            checksum_refresh_offset: 0,
            checksum_refresh_hash: 0,
            checksum_refresh_tx_id: 0,
            db_base_offset: crate::config::DB_REGION_OFFSET,
            page_table_offset: 0,
            page_count: 0,
            layout_version: CURRENT_LAYOUT_VERSION,
            zero_extents: Vec::new(),
            meta_checksum: 0,
        };
        block.meta_checksum = block.compute_meta_checksum();
        block
    }

    pub fn load() -> Result<Self, StableMemoryError> {
        let key = superblock_cache_key()?;
        if let Some(block) = SUPERBLOCK_CACHE.with(|cache| cache.borrow().get(&key).cloned()) {
            return Ok(block);
        }
        #[cfg(any(test, debug_assertions, feature = "bench-profile"))]
        crate::read_metrics::record_superblock_load();
        if memory::size_pages() == 0 {
            let fresh = Self::fresh();
            fresh.store()?;
            return Ok(fresh);
        }
        let mut bytes = [0_u8; ENCODED_LEN];
        memory::read_preallocated(SUPERBLOCK_OFFSET, &mut bytes)?;
        let block = Self::decode(&bytes);
        if block.magic != MAGIC {
            return Err(StableMemoryError::ForeignStableMemoryImage);
        }
        if block.version != VERSION {
            return Err(StableMemoryError::UnsupportedLayoutVersion(u64::from(
                block.version,
            )));
        }
        if block.layout_version != CURRENT_LAYOUT_VERSION {
            return Err(StableMemoryError::UnsupportedLayoutVersion(
                block.layout_version,
            ));
        }
        let Ok(zero_extent_count) = usize::try_from(u64::from_le_bytes(eight(&bytes, 144))) else {
            return Err(StableMemoryError::MetaChecksumMismatch);
        };
        if zero_extent_count > MAX_ZERO_EXTENTS {
            return Err(StableMemoryError::MetaChecksumMismatch);
        }
        if !block.verify_checksum() {
            return Err(StableMemoryError::MetaChecksumMismatch);
        }
        if !block.has_normalized_zero_extents() {
            return Err(StableMemoryError::MetaChecksumMismatch);
        }
        cache_superblock(&block);
        Ok(block)
    }

    pub fn store(&self) -> Result<(), StableMemoryError> {
        self.store_with_capacity_check()
    }

    fn store_with_capacity_check(&self) -> Result<(), StableMemoryError> {
        let mut block = self.clone();
        block.validate_zero_extents()?;
        block.version = VERSION;
        block.meta_checksum = block.compute_meta_checksum();
        let encoded = block.encode();
        // The zero-extent count bounds the encoded metadata. Bytes from an older
        // longer extent list may remain after this write, but load/checksum
        // ignore everything past the current count.
        memory::write(SUPERBLOCK_OFFSET, &encoded[..block.encoded_len()])?;
        cache_superblock_owned(block);
        Ok(())
    }

    fn store_preallocated(&self) -> Result<(), StableMemoryError> {
        let mut block = self.clone();
        block.validate_zero_extents()?;
        block.version = VERSION;
        block.meta_checksum = block.compute_meta_checksum();
        let encoded = block.encode();
        // The zero-extent count bounds the encoded metadata. Bytes from an older
        // longer extent list may remain after this write, but load/checksum
        // ignore everything past the current count.
        memory::write_prechecked(SUPERBLOCK_OFFSET, &encoded[..block.encoded_len()])?;
        cache_superblock_owned(block);
        Ok(())
    }

    #[allow(dead_code)]
    pub fn set_db_size(size: u64) -> Result<(), StableMemoryError> {
        let mut block = Self::load()?;
        block.db_size = size;
        block.store_preallocated()
    }

    #[allow(dead_code)]
    pub fn record_committed_tx() -> Result<(), StableMemoryError> {
        let mut block = Self::load()?;
        block.last_tx_id = block.last_tx_id.saturating_add(1);
        block.flags |= FLAG_CHECKSUM_STALE;
        block.clear_checksum_refresh();
        block.store_preallocated()
    }

    pub(crate) fn commit_db_image(
        db_base_offset: u64,
        db_size: u64,
        zero_extents: Vec<ZeroExtent>,
    ) -> Result<(), StableMemoryError> {
        let mut block = Self::load()?;
        block.db_base_offset = db_base_offset;
        block.db_size = db_size;
        block.page_table_offset = 0;
        block.page_count = page_count_for_size(db_size);
        block.layout_version = CURRENT_LAYOUT_VERSION;
        block.zero_extents = zero_extents;
        block.last_tx_id = block.last_tx_id.saturating_add(1);
        block.flags |= FLAG_CHECKSUM_STALE;
        block.clear_checksum_refresh();
        block.store_preallocated()
    }

    pub(crate) fn store_db_image_without_tx(
        db_base_offset: u64,
        db_size: u64,
        zero_extents: Vec<ZeroExtent>,
    ) -> Result<(), StableMemoryError> {
        let mut block = Self::load()?;
        block.db_base_offset = db_base_offset;
        block.db_size = db_size;
        block.page_table_offset = 0;
        block.page_count = page_count_for_size(db_size);
        block.layout_version = CURRENT_LAYOUT_VERSION;
        block.zero_extents = zero_extents;
        block.store_preallocated()
    }

    pub fn verify_checksum(&self) -> bool {
        self.meta_checksum == self.compute_meta_checksum()
    }

    pub fn is_importing(&self) -> bool {
        self.flags & FLAG_IMPORTING != 0
    }

    #[allow(dead_code)]
    pub fn is_checksum_stale(&self) -> bool {
        self.flags & FLAG_CHECKSUM_STALE != 0
    }

    pub fn is_checksum_refreshing(&self) -> bool {
        self.flags & FLAG_CHECKSUM_REFRESHING != 0
    }

    pub(crate) fn zero_extents(&self) -> &[ZeroExtent] {
        &self.zero_extents
    }

    #[allow(dead_code)]
    pub(crate) fn zero_extent_count(&self) -> usize {
        self.zero_extents.len()
    }

    #[allow(dead_code)]
    pub(crate) fn clear_zero_extents(&mut self) {
        self.zero_extents.clear();
    }

    pub fn clear_checksum_refresh(&mut self) {
        self.flags &= !FLAG_CHECKSUM_REFRESHING;
        self.checksum_refresh_offset = 0;
        self.checksum_refresh_hash = 0;
        self.checksum_refresh_tx_id = 0;
    }

    fn encode(&self) -> [u8; ENCODED_LEN] {
        let mut out = [0_u8; ENCODED_LEN];
        out[0..8].copy_from_slice(&self.magic);
        out[8..12].copy_from_slice(&self.version.to_le_bytes());
        out[12..16].copy_from_slice(&self.sqlite_page_size.to_le_bytes());
        out[16..24].copy_from_slice(&self.db_size.to_le_bytes());
        out[24..32].copy_from_slice(&self.schema_version.to_le_bytes());
        out[32..40].copy_from_slice(&self.last_tx_id.to_le_bytes());
        out[40..48].copy_from_slice(&self.flags.to_le_bytes());
        out[48..56].copy_from_slice(&self.checksum.to_le_bytes());
        out[56..64].copy_from_slice(&self.import_expected_checksum.to_le_bytes());
        out[64..72].copy_from_slice(&self.import_written_until.to_le_bytes());
        out[72..80].copy_from_slice(&self.import_total_size.to_le_bytes());
        out[80..88].copy_from_slice(&self.import_base_offset.to_le_bytes());
        out[88..96].copy_from_slice(&self.checksum_refresh_offset.to_le_bytes());
        out[96..104].copy_from_slice(&self.checksum_refresh_hash.to_le_bytes());
        out[104..112].copy_from_slice(&self.checksum_refresh_tx_id.to_le_bytes());
        out[112..120].copy_from_slice(&self.db_base_offset.to_le_bytes());
        out[120..128].copy_from_slice(&self.page_table_offset.to_le_bytes());
        out[128..136].copy_from_slice(&self.page_count.to_le_bytes());
        out[136..144].copy_from_slice(&self.layout_version.to_le_bytes());
        out[144..152].copy_from_slice(
            &u64::try_from(self.zero_extents.len())
                .unwrap_or(u64::MAX)
                .to_le_bytes(),
        );
        out[152..160].copy_from_slice(&self.meta_checksum.to_le_bytes());
        for (index, extent) in self.zero_extents.iter().take(MAX_ZERO_EXTENTS).enumerate() {
            let offset = EXTENTS_OFFSET + index * ZERO_EXTENT_BYTES;
            out[offset..offset + 8].copy_from_slice(&extent.start_page.to_le_bytes());
            out[offset + 8..offset + 16].copy_from_slice(&extent.end_page.to_le_bytes());
        }
        out
    }

    fn decode(bytes: &[u8; ENCODED_LEN]) -> Self {
        let zero_extent_count = u64::from_le_bytes(eight(bytes, 144));
        let mut zero_extents = Vec::new();
        if let Ok(count) = usize::try_from(zero_extent_count) {
            if count <= MAX_ZERO_EXTENTS {
                zero_extents.reserve(count);
                for index in 0..count {
                    let offset = EXTENTS_OFFSET + index * ZERO_EXTENT_BYTES;
                    zero_extents.push(ZeroExtent {
                        start_page: u64::from_le_bytes(eight(bytes, offset)),
                        end_page: u64::from_le_bytes(eight(bytes, offset + 8)),
                    });
                }
            }
        }
        Self {
            magic: eight(bytes, 0),
            version: u32::from_le_bytes(four(bytes, 8)),
            sqlite_page_size: u32::from_le_bytes(four(bytes, 12)),
            db_size: u64::from_le_bytes(eight(bytes, 16)),
            schema_version: u64::from_le_bytes(eight(bytes, 24)),
            last_tx_id: u64::from_le_bytes(eight(bytes, 32)),
            flags: u64::from_le_bytes(eight(bytes, 40)),
            checksum: u64::from_le_bytes(eight(bytes, 48)),
            import_expected_checksum: u64::from_le_bytes(eight(bytes, 56)),
            import_written_until: u64::from_le_bytes(eight(bytes, 64)),
            import_total_size: u64::from_le_bytes(eight(bytes, 72)),
            import_base_offset: u64::from_le_bytes(eight(bytes, 80)),
            checksum_refresh_offset: u64::from_le_bytes(eight(bytes, 88)),
            checksum_refresh_hash: u64::from_le_bytes(eight(bytes, 96)),
            checksum_refresh_tx_id: u64::from_le_bytes(eight(bytes, 104)),
            db_base_offset: u64::from_le_bytes(eight(bytes, 112)),
            page_table_offset: u64::from_le_bytes(eight(bytes, 120)),
            page_count: u64::from_le_bytes(eight(bytes, 128)),
            layout_version: u64::from_le_bytes(eight(bytes, 136)),
            zero_extents,
            meta_checksum: u64::from_le_bytes(eight(bytes, META_CHECKSUM_OFFSET)),
        }
    }

    fn compute_meta_checksum(&self) -> u64 {
        let mut copy = self.clone();
        copy.meta_checksum = 0;
        let encoded = copy.encode();
        fnv1a64(&encoded[..copy.encoded_len()])
    }

    fn has_normalized_zero_extents(&self) -> bool {
        self.validate_zero_extents().is_ok()
    }

    fn validate_zero_extents(&self) -> Result<(), StableMemoryError> {
        if self.zero_extents.len() > MAX_ZERO_EXTENTS {
            return Err(StableMemoryError::ZeroExtentLimitExceeded {
                limit: MAX_ZERO_EXTENTS,
            });
        }
        let mut previous_end = None;
        for extent in &self.zero_extents {
            if extent.start_page >= extent.end_page {
                return Err(StableMemoryError::MetaChecksumMismatch);
            }
            if previous_end.is_some_and(|end| extent.start_page <= end) {
                return Err(StableMemoryError::MetaChecksumMismatch);
            }
            previous_end = Some(extent.end_page);
        }
        Ok(())
    }

    fn encoded_len(&self) -> usize {
        EXTENTS_OFFSET + self.zero_extents.len().min(MAX_ZERO_EXTENTS) * ZERO_EXTENT_BYTES
    }
}

#[doc(hidden)]
#[cfg(test)]
pub fn clear_superblock_cache() {
    SUPERBLOCK_CACHE.with(|cache| cache.borrow_mut().clear());
}

#[cfg(test)]
fn superblock_cache_len() -> usize {
    SUPERBLOCK_CACHE.with(|cache| cache.borrow().len())
}

fn cache_superblock(block: &Superblock) {
    cache_superblock_owned(block.clone());
}

fn cache_superblock_owned(block: Superblock) {
    if let Ok(key) = superblock_cache_key() {
        SUPERBLOCK_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            cache.retain(|cached_key, _| cached_key.generation == key.generation);
            cache.insert(key, block);
        });
    }
}

fn superblock_cache_key() -> Result<SuperblockCacheKey, StableMemoryError> {
    Ok(SuperblockCacheKey {
        context: memory::active_context_id()?,
        generation: memory::cache_generation(),
    })
}

fn four(bytes: &[u8; ENCODED_LEN], start: usize) -> [u8; 4] {
    let mut out = [0_u8; 4];
    out.copy_from_slice(&bytes[start..start + 4]);
    out
}

fn eight(bytes: &[u8; ENCODED_LEN], start: usize) -> [u8; 8] {
    let mut out = [0_u8; 8];
    out.copy_from_slice(&bytes[start..start + 8]);
    out
}

pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn page_count_for_size(size: u64) -> u64 {
    size.div_ceil(u64::from(SQLITE_PAGE_SIZE))
}

#[cfg(test)]
mod tests;
