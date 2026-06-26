//! Debug-build helpers for repository integration tests.
//!
//! These symbols keep internal storage and VFS modules out of the release API
//! while still allowing integration tests to exercise low-level invariants.

pub use crate::sqlite_vfs::ffi;
pub use crate::stable::memory::{ContextId, StableMemoryError};
pub use crate::stable::meta::Superblock;
pub use crate::stable::raw_memory::{Memory, MemoryIdentity, VectorMemory};

pub mod meta {
    pub use crate::stable::meta::{fnv1a64, CURRENT_LAYOUT_VERSION};
}

pub mod memory {
    pub use crate::stable::memory::{
        active_context_id, init, init_context, memory_for_tests, read, reset_for_tests,
        restore_for_tests, set_next_context_id_for_tests, size_pages, snapshot_for_tests, write,
    };
}

pub mod memory_manager {
    pub use crate::stable::memory_manager::{MemoryId, MemoryManager};
}

pub mod lock {
    pub use crate::sqlite_vfs::lock::reset_for_tests;
}

pub mod vfs {
    pub use crate::sqlite_vfs::register;
}

pub mod stable_blob {
    pub use crate::sqlite_vfs::stable_blob::{storage_stats, ChecksumRefresh, StorageStats};
}

#[cfg(any(test, debug_assertions, feature = "bench-profile"))]
pub mod read_metrics {
    pub use crate::read_metrics::{
        disable_read_metrics, read_metrics_snapshot, reset_read_metrics,
    };
}
