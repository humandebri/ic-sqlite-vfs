//! Central runtime constants for the SQLite-on-stable-memory layout.
//!
//! Keeping offsets and PRAGMA values here prevents hidden magic numbers in VFS
//! callbacks, migrations, and canister API code.

pub const SQLITE_PAGE_SIZE: u32 = 16_384;
pub const SQLITE_CACHE_SIZE_KIB: i32 = 32_768;
pub const STATEMENT_CACHE_CAPACITY: usize = 32;

pub const STABLE_PAGE_SIZE: u64 = 65_536;
pub const SUPERBLOCK_OFFSET: u64 = 0;
pub const SUPERBLOCK_SIZE: u64 = STABLE_PAGE_SIZE;
pub const DB_REGION_OFFSET: u64 = SUPERBLOCK_OFFSET + SUPERBLOCK_SIZE;

pub const MAIN_DB_PATH: &str = "/main.db";
pub const VFS_NAME: &str = "icstable";
pub const SQLITE_URI: &str = "file:/main.db?vfs=icstable";
pub const VFS_NAME_NUL: &[u8] = b"icstable\0";
pub const SQLITE_URI_NUL: &[u8] = b"file:/main.db?vfs=icstable\0";
