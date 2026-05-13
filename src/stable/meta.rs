//! Superblock encoding for the stable-memory SQLite image.
//!
//! The format is deliberately fixed-width little-endian data so upgrades can
//! inspect and migrate it without deserializing a Rust-specific structure.

use crate::config::{SQLITE_PAGE_SIZE, SUPERBLOCK_OFFSET, SUPERBLOCK_SIZE};
use crate::stable::memory::{self, StableMemoryError};

const MAGIC: [u8; 8] = *b"ICSQLITE";
const VERSION: u32 = 6;
const ENCODED_LEN: usize = 152;
pub const PAGE_MAP_LAYOUT_VERSION: u64 = 6;
pub const FLAG_IMPORTING: u64 = 1;
pub const FLAG_CHECKSUM_STALE: u64 = 1 << 1;
pub const FLAG_CHECKSUM_REFRESHING: u64 = 1 << 2;

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
        memory::ensure_capacity(SUPERBLOCK_OFFSET + SUPERBLOCK_SIZE)?;
        let mut bytes = [0_u8; ENCODED_LEN];
        memory::read(SUPERBLOCK_OFFSET, &mut bytes)?;
        let block = Self::decode(&bytes);
        if block.magic != MAGIC {
            let fresh = Self::fresh();
            fresh.store()?;
            return Ok(fresh);
        }
        if !block.verify_checksum() {
            return Err(StableMemoryError::MetaChecksumMismatch);
        }
        Ok(block)
    }

    pub fn store(&self) -> Result<(), StableMemoryError> {
        let mut block = self.clone();
        block.version = VERSION;
        block.meta_checksum = block.compute_meta_checksum();
        memory::write(SUPERBLOCK_OFFSET, &block.encode())
    }

    pub fn set_db_size(size: u64) -> Result<(), StableMemoryError> {
        let mut block = Self::load()?;
        block.db_size = size;
        block.store()
    }

    pub fn record_committed_tx() -> Result<(), StableMemoryError> {
        let mut block = Self::load()?;
        block.last_tx_id = block.last_tx_id.saturating_add(1);
        block.flags |= FLAG_CHECKSUM_STALE;
        block.clear_checksum_refresh();
        block.store()
    }

    pub fn commit_db_image(db_base_offset: u64, db_size: u64) -> Result<(), StableMemoryError> {
        let mut block = Self::load()?;
        block.db_base_offset = db_base_offset;
        block.db_size = db_size;
        block.last_tx_id = block.last_tx_id.saturating_add(1);
        block.flags |= FLAG_CHECKSUM_STALE;
        block.clear_checksum_refresh();
        block.store()
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
        block.store()
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
        block.store()
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
