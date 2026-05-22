//! Superblock encoding for the stable-memory SQLite image.
//!
//! The format is deliberately fixed-width little-endian data so upgrades can
//! inspect and migrate it without deserializing a Rust-specific structure.

use crate::config::{SQLITE_PAGE_SIZE, SUPERBLOCK_OFFSET, SUPERBLOCK_SIZE};
use crate::stable::memory::{self, ContextId, StableMemoryError};
use std::cell::RefCell;
use std::collections::BTreeMap;

const MAGIC: [u8; 8] = *b"ICSQLITE";
const VERSION: u32 = 6;
const ENCODED_LEN: usize = 152;
pub const PAGE_MAP_LAYOUT_VERSION: u64 = 6;
pub const FLAG_IMPORTING: u64 = 1;
pub const FLAG_CHECKSUM_STALE: u64 = 1 << 1;
pub const FLAG_CHECKSUM_REFRESHING: u64 = 1 << 2;

thread_local! {
    static SUPERBLOCK_CACHE: RefCell<BTreeMap<ContextId, Superblock>> = const { RefCell::new(BTreeMap::new()) };
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
            layout_version: PAGE_MAP_LAYOUT_VERSION,
            meta_checksum: 0,
        };
        block.meta_checksum = block.compute_meta_checksum();
        block
    }

    pub fn load() -> Result<Self, StableMemoryError> {
        let context = memory::active_context_id()?;
        if let Some(block) = SUPERBLOCK_CACHE.with(|cache| cache.borrow().get(&context).cloned()) {
            return Ok(block);
        }
        #[cfg(any(test, debug_assertions, feature = "bench-profile"))]
        crate::read_metrics::record_superblock_load();
        memory::ensure_capacity(SUPERBLOCK_OFFSET + SUPERBLOCK_SIZE)?;
        let mut bytes = [0_u8; ENCODED_LEN];
        memory::read_preallocated(SUPERBLOCK_OFFSET, &mut bytes)?;
        let block = Self::decode(&bytes);
        if block.magic != MAGIC {
            let fresh = Self::fresh();
            fresh.store()?;
            return Ok(fresh);
        }
        if !block.verify_checksum() {
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
        block.version = VERSION;
        block.meta_checksum = block.compute_meta_checksum();
        memory::write(SUPERBLOCK_OFFSET, &block.encode())?;
        cache_superblock_owned(block);
        Ok(())
    }

    fn store_preallocated(&self) -> Result<(), StableMemoryError> {
        let mut block = self.clone();
        block.version = VERSION;
        block.meta_checksum = block.compute_meta_checksum();
        memory::write_prechecked(SUPERBLOCK_OFFSET, &block.encode())?;
        cache_superblock_owned(block);
        Ok(())
    }

    fn store_preallocated_unmetered(&self) -> Result<(), StableMemoryError> {
        let mut block = self.clone();
        block.version = VERSION;
        block.meta_checksum = block.compute_meta_checksum();
        memory::write_prechecked_unmetered(SUPERBLOCK_OFFSET, &block.encode())?;
        cache_superblock_owned(block);
        Ok(())
    }

    pub fn set_db_size(size: u64) -> Result<(), StableMemoryError> {
        let mut block = Self::load()?;
        block.db_size = size;
        block.store_preallocated()
    }

    pub fn record_committed_tx() -> Result<(), StableMemoryError> {
        let mut block = Self::load()?;
        block.last_tx_id = block.last_tx_id.saturating_add(1);
        block.flags |= FLAG_CHECKSUM_STALE;
        block.clear_checksum_refresh();
        block.store_preallocated()
    }

    pub fn commit_db_image(db_base_offset: u64, db_size: u64) -> Result<(), StableMemoryError> {
        let mut block = Self::load()?;
        block.db_base_offset = db_base_offset;
        block.db_size = db_size;
        block.last_tx_id = block.last_tx_id.saturating_add(1);
        block.flags |= FLAG_CHECKSUM_STALE;
        block.clear_checksum_refresh();
        block.store_preallocated()
    }

    pub fn commit_page_map(
        page_table_offset: u64,
        page_count: u64,
        db_size: u64,
    ) -> Result<(), StableMemoryError> {
        let mut block = Self::load()?;
        block.page_table_offset = page_table_offset;
        block.page_count = page_count;
        block.db_size = db_size;
        block.layout_version = PAGE_MAP_LAYOUT_VERSION;
        block.last_tx_id = block.last_tx_id.saturating_add(1);
        block.flags |= FLAG_CHECKSUM_STALE;
        block.clear_checksum_refresh();
        block.store_preallocated()
    }

    pub fn commit_page_map_unmetered(
        page_table_offset: u64,
        page_count: u64,
        db_size: u64,
    ) -> Result<(), StableMemoryError> {
        let mut block = Self::load()?;
        block.page_table_offset = page_table_offset;
        block.page_count = page_count;
        block.db_size = db_size;
        block.layout_version = PAGE_MAP_LAYOUT_VERSION;
        block.last_tx_id = block.last_tx_id.saturating_add(1);
        block.flags |= FLAG_CHECKSUM_STALE;
        block.clear_checksum_refresh();
        block.store_preallocated_unmetered()
    }

    pub fn store_page_map_without_tx(
        page_table_offset: u64,
        page_count: u64,
        db_size: u64,
    ) -> Result<(), StableMemoryError> {
        let mut block = Self::load()?;
        block.page_table_offset = page_table_offset;
        block.page_count = page_count;
        block.db_size = db_size;
        block.layout_version = PAGE_MAP_LAYOUT_VERSION;
        block.store_preallocated()
    }

    pub fn store_page_map_without_tx_unmetered(
        page_table_offset: u64,
        page_count: u64,
        db_size: u64,
    ) -> Result<(), StableMemoryError> {
        let mut block = Self::load()?;
        block.page_table_offset = page_table_offset;
        block.page_count = page_count;
        block.db_size = db_size;
        block.layout_version = PAGE_MAP_LAYOUT_VERSION;
        block.store_preallocated_unmetered()
    }

    pub fn verify_checksum(&self) -> bool {
        self.meta_checksum == self.compute_meta_checksum()
    }

    pub fn is_importing(&self) -> bool {
        self.flags & FLAG_IMPORTING != 0
    }

    pub fn is_checksum_stale(&self) -> bool {
        self.flags & FLAG_CHECKSUM_STALE != 0
    }

    pub fn is_checksum_refreshing(&self) -> bool {
        self.flags & FLAG_CHECKSUM_REFRESHING != 0
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
        out[144..152].copy_from_slice(&self.meta_checksum.to_le_bytes());
        out
    }

    fn decode(bytes: &[u8; ENCODED_LEN]) -> Self {
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
            meta_checksum: u64::from_le_bytes(eight(bytes, 144)),
        }
    }

    fn compute_meta_checksum(&self) -> u64 {
        let mut copy = self.clone();
        copy.meta_checksum = 0;
        fnv1a64(&copy.encode())
    }
}

#[doc(hidden)]
pub fn clear_superblock_cache() {
    SUPERBLOCK_CACHE.with(|cache| cache.borrow_mut().clear());
}

fn cache_superblock(block: &Superblock) {
    cache_superblock_owned(block.clone());
}

fn cache_superblock_owned(block: Superblock) {
    if let Ok(context) = memory::active_context_id() {
        SUPERBLOCK_CACHE.with(|cache| {
            cache.borrow_mut().insert(context, block);
        });
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_block() -> Superblock {
        let mut block = Superblock::fresh();
        block.db_size = 0x0102_0304_0506_0708;
        block.schema_version = 0x1112_1314_1516_1718;
        block.last_tx_id = 0x2122_2324_2526_2728;
        block.flags = FLAG_IMPORTING | FLAG_CHECKSUM_STALE | FLAG_CHECKSUM_REFRESHING;
        block.checksum = 0x3132_3334_3536_3738;
        block.import_expected_checksum = 0x4142_4344_4546_4748;
        block.import_written_until = 0x5152_5354_5556_5758;
        block.import_total_size = 0x6162_6364_6566_6768;
        block.import_base_offset = 0x7172_7374_7576_7778;
        block.checksum_refresh_offset = 0x8182_8384_8586_8788;
        block.checksum_refresh_hash = 0x9192_9394_9596_9798;
        block.checksum_refresh_tx_id = 0xa1a2_a3a4_a5a6_a7a8;
        block.db_base_offset = 0xb1b2_b3b4_b5b6_b7b8;
        block.page_table_offset = 0xc1c2_c3c4_c5c6_c7c8;
        block.page_count = 0xd1d2_d3d4_d5d6_d7d8;
        block.layout_version = PAGE_MAP_LAYOUT_VERSION;
        block.meta_checksum = block.compute_meta_checksum();
        block
    }

    #[test]
    fn superblock_encode_decode_uses_fixed_little_endian_offsets() {
        let block = sample_block();
        let encoded = block.encode();

        assert_eq!(&encoded[0..8], b"ICSQLITE");
        assert_eq!(&encoded[8..12], &VERSION.to_le_bytes());
        assert_eq!(&encoded[12..16], &SQLITE_PAGE_SIZE.to_le_bytes());
        assert_eq!(&encoded[16..24], &block.db_size.to_le_bytes());
        assert_eq!(&encoded[80..88], &block.import_base_offset.to_le_bytes());
        assert_eq!(&encoded[120..128], &block.page_table_offset.to_le_bytes());
        assert_eq!(&encoded[144..152], &block.meta_checksum.to_le_bytes());
        assert_eq!(Superblock::decode(&encoded), block);
    }

    #[test]
    fn superblock_meta_digest_zeroes_only_meta_field() {
        let block = sample_block();
        let mut checksum_input = block.encode();
        checksum_input[144..152].copy_from_slice(&0_u64.to_le_bytes());

        let mut changed_checksum = block.clone();
        changed_checksum.meta_checksum ^= u64::MAX;

        let mut changed_field = block.clone();
        changed_field.last_tx_id = changed_field.last_tx_id.wrapping_add(1);

        assert_eq!(block.compute_meta_checksum(), fnv1a64(&checksum_input));
        assert_eq!(
            changed_checksum.compute_meta_checksum(),
            block.compute_meta_checksum()
        );
        assert_ne!(
            changed_field.compute_meta_checksum(),
            block.compute_meta_checksum()
        );
    }
}
