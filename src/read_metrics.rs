//! Debug read metrics for SQLite VFS performance probes.
//!
//! Counters are process-local diagnostics. They are compiled out of release
//! builds so canister API and stable data layout stay unchanged.

use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReadMetrics {
    pub x_read_calls: u64,
    pub x_read_bytes: u64,
    pub stable_data_read_calls: u64,
    pub stable_data_read_bytes: u64,
    pub page_table_root_hits: u64,
    pub page_table_root_misses: u64,
    pub page_table_segment_hits: u64,
    pub page_table_segment_misses: u64,
    pub superblock_loads: u64,
}

static X_READ_CALLS: AtomicU64 = AtomicU64::new(0);
static X_READ_BYTES: AtomicU64 = AtomicU64::new(0);
static STABLE_DATA_READ_CALLS: AtomicU64 = AtomicU64::new(0);
static STABLE_DATA_READ_BYTES: AtomicU64 = AtomicU64::new(0);
static PAGE_TABLE_ROOT_HITS: AtomicU64 = AtomicU64::new(0);
static PAGE_TABLE_ROOT_MISSES: AtomicU64 = AtomicU64::new(0);
static PAGE_TABLE_SEGMENT_HITS: AtomicU64 = AtomicU64::new(0);
static PAGE_TABLE_SEGMENT_MISSES: AtomicU64 = AtomicU64::new(0);
static SUPERBLOCK_LOADS: AtomicU64 = AtomicU64::new(0);

#[doc(hidden)]
pub fn reset_read_metrics() {
    for counter in counters() {
        counter.store(0, Ordering::Relaxed);
    }
}

#[doc(hidden)]
pub fn read_metrics_snapshot() -> ReadMetrics {
    ReadMetrics {
        x_read_calls: X_READ_CALLS.load(Ordering::Relaxed),
        x_read_bytes: X_READ_BYTES.load(Ordering::Relaxed),
        stable_data_read_calls: STABLE_DATA_READ_CALLS.load(Ordering::Relaxed),
        stable_data_read_bytes: STABLE_DATA_READ_BYTES.load(Ordering::Relaxed),
        page_table_root_hits: PAGE_TABLE_ROOT_HITS.load(Ordering::Relaxed),
        page_table_root_misses: PAGE_TABLE_ROOT_MISSES.load(Ordering::Relaxed),
        page_table_segment_hits: PAGE_TABLE_SEGMENT_HITS.load(Ordering::Relaxed),
        page_table_segment_misses: PAGE_TABLE_SEGMENT_MISSES.load(Ordering::Relaxed),
        superblock_loads: SUPERBLOCK_LOADS.load(Ordering::Relaxed),
    }
}

pub(crate) fn record_x_read(bytes: usize) {
    increment(&X_READ_CALLS);
    add_bytes(&X_READ_BYTES, bytes);
}

pub(crate) fn record_stable_data_read(bytes: usize) {
    increment(&STABLE_DATA_READ_CALLS);
    add_bytes(&STABLE_DATA_READ_BYTES, bytes);
}

pub(crate) fn record_page_table_root_hit() {
    increment(&PAGE_TABLE_ROOT_HITS);
}

pub(crate) fn record_page_table_root_miss() {
    increment(&PAGE_TABLE_ROOT_MISSES);
}

pub(crate) fn record_page_table_segment_hit() {
    increment(&PAGE_TABLE_SEGMENT_HITS);
}

pub(crate) fn record_page_table_segment_miss() {
    increment(&PAGE_TABLE_SEGMENT_MISSES);
}

pub(crate) fn record_superblock_load() {
    increment(&SUPERBLOCK_LOADS);
}

fn counters() -> [&'static AtomicU64; 9] {
    [
        &X_READ_CALLS,
        &X_READ_BYTES,
        &STABLE_DATA_READ_CALLS,
        &STABLE_DATA_READ_BYTES,
        &PAGE_TABLE_ROOT_HITS,
        &PAGE_TABLE_ROOT_MISSES,
        &PAGE_TABLE_SEGMENT_HITS,
        &PAGE_TABLE_SEGMENT_MISSES,
        &SUPERBLOCK_LOADS,
    ]
}

fn increment(counter: &AtomicU64) {
    add(counter, 1);
}

fn add_bytes(counter: &AtomicU64, bytes: usize) {
    let value = u64::try_from(bytes).unwrap_or(u64::MAX);
    add(counter, value);
}

fn add(counter: &AtomicU64, value: u64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_add(value))
    });
}
