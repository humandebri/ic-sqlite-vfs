//! Stable memory primitives for the SQLite database image.
//!
//! `meta` owns logical metadata. `memory` is the only module that touches the
//! caller-provided virtual memory backend.

pub mod memory;
mod memory_layout;
pub mod memory_manager;
pub mod meta;
pub mod raw_memory;
