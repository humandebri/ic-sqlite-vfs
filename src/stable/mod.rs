//! Stable memory primitives for the SQLite database image.
//!
//! `meta` owns logical metadata. `memory` is the only module that touches the IC
//! stable memory API or the native test backend.

pub mod memory;
pub mod meta;
