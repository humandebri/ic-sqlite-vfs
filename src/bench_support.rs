//! Benchmark-only helpers outside the stable public API.
//!
//! The repository benchmark canister needs internal counters and metadata, but
//! downstream applications should use the `Db` / `DbHandle` facade.

pub use crate::stable::meta::Superblock;

pub mod memory {
    pub use crate::stable::memory::size_pages;
}

pub mod read_metrics {
    pub use crate::read_metrics::{
        disable_read_metrics, read_metrics_snapshot, reset_read_metrics, ReadMetrics,
    };
}
