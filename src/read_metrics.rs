//! Debug read metrics for SQLite VFS performance probes.
//!
//! Counters are process-local diagnostics. They are compiled out of release
//! builds so canister API and stable data layout stay unchanged.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReadMetrics {
    pub x_read_calls: u64,
    pub x_read_bytes: u64,
    pub x_write_calls: u64,
    pub x_write_bytes: u64,
    pub x_file_size_calls: u64,
    pub x_lock_calls: u64,
    pub x_unlock_calls: u64,
    pub x_check_reserved_lock_calls: u64,
    pub x_file_control_calls: u64,
    pub x_device_characteristics_calls: u64,
    pub stable_data_read_calls: u64,
    pub stable_data_read_bytes: u64,
    pub stable_data_write_calls: u64,
    pub stable_data_write_bytes: u64,
    pub stable_grow_calls: u64,
    pub stable_grow_pages: u64,
    pub superblock_loads: u64,
    pub commit_load: u64,
    pub commit_capacity: u64,
    pub commit_page_write: u64,
    pub commit_superblock_store: u64,
}

static METRICS_ENABLED: AtomicBool = AtomicBool::new(false);

macro_rules! read_metric_counters {
    ($macro:ident) => {
        $macro! {
            X_READ_CALLS,
            X_READ_BYTES,
            X_WRITE_CALLS,
            X_WRITE_BYTES,
            X_FILE_SIZE_CALLS,
            X_LOCK_CALLS,
            X_UNLOCK_CALLS,
            X_CHECK_RESERVED_LOCK_CALLS,
            X_FILE_CONTROL_CALLS,
            X_DEVICE_CHARACTERISTICS_CALLS,
            STABLE_DATA_READ_CALLS,
            STABLE_DATA_READ_BYTES,
            STABLE_DATA_WRITE_CALLS,
            STABLE_DATA_WRITE_BYTES,
            STABLE_GROW_CALLS,
            STABLE_GROW_PAGES,
            SUPERBLOCK_LOADS,
            COMMIT_LOAD,
            COMMIT_CAPACITY,
            COMMIT_PAGE_WRITE,
            COMMIT_SUPERBLOCK_STORE,
        }
    };
}

macro_rules! define_read_metric_counters {
    ($($counter:ident),+ $(,)?) => {
        $(static $counter: AtomicU64 = AtomicU64::new(0);)+

        const COUNTER_COUNT: usize = [$(
            stringify!($counter)
        ),+].len();

        fn counters() -> [&'static AtomicU64; COUNTER_COUNT] {
            [$(&$counter),+]
        }
    };
}

read_metric_counters!(define_read_metric_counters);

#[doc(hidden)]
#[inline(always)]
pub fn reset_read_metrics() {
    for counter in counters() {
        counter.store(0, Ordering::Relaxed);
    }
    METRICS_ENABLED.store(true, Ordering::Relaxed);
}

#[doc(hidden)]
#[inline(always)]
pub fn disable_read_metrics() {
    METRICS_ENABLED.store(false, Ordering::Relaxed);
}

#[doc(hidden)]
#[inline(always)]
pub fn read_metrics_snapshot() -> ReadMetrics {
    ReadMetrics {
        x_read_calls: X_READ_CALLS.load(Ordering::Relaxed),
        x_read_bytes: X_READ_BYTES.load(Ordering::Relaxed),
        x_write_calls: X_WRITE_CALLS.load(Ordering::Relaxed),
        x_write_bytes: X_WRITE_BYTES.load(Ordering::Relaxed),
        x_file_size_calls: X_FILE_SIZE_CALLS.load(Ordering::Relaxed),
        x_lock_calls: X_LOCK_CALLS.load(Ordering::Relaxed),
        x_unlock_calls: X_UNLOCK_CALLS.load(Ordering::Relaxed),
        x_check_reserved_lock_calls: X_CHECK_RESERVED_LOCK_CALLS.load(Ordering::Relaxed),
        x_file_control_calls: X_FILE_CONTROL_CALLS.load(Ordering::Relaxed),
        x_device_characteristics_calls: X_DEVICE_CHARACTERISTICS_CALLS.load(Ordering::Relaxed),
        stable_data_read_calls: STABLE_DATA_READ_CALLS.load(Ordering::Relaxed),
        stable_data_read_bytes: STABLE_DATA_READ_BYTES.load(Ordering::Relaxed),
        stable_data_write_calls: STABLE_DATA_WRITE_CALLS.load(Ordering::Relaxed),
        stable_data_write_bytes: STABLE_DATA_WRITE_BYTES.load(Ordering::Relaxed),
        stable_grow_calls: STABLE_GROW_CALLS.load(Ordering::Relaxed),
        stable_grow_pages: STABLE_GROW_PAGES.load(Ordering::Relaxed),
        superblock_loads: SUPERBLOCK_LOADS.load(Ordering::Relaxed),
        commit_load: COMMIT_LOAD.load(Ordering::Relaxed),
        commit_capacity: COMMIT_CAPACITY.load(Ordering::Relaxed),
        commit_page_write: COMMIT_PAGE_WRITE.load(Ordering::Relaxed),
        commit_superblock_store: COMMIT_SUPERBLOCK_STORE.load(Ordering::Relaxed),
    }
}

#[inline(always)]
pub(crate) fn record_x_read(bytes: usize) {
    if !metrics_enabled() {
        return;
    }
    X_READ_CALLS.fetch_add(1, Ordering::Relaxed);
    X_READ_BYTES.fetch_add(byte_count(bytes), Ordering::Relaxed);
}

#[inline(always)]
pub(crate) fn record_x_write(bytes: usize) {
    if !metrics_enabled() {
        return;
    }
    X_WRITE_CALLS.fetch_add(1, Ordering::Relaxed);
    X_WRITE_BYTES.fetch_add(byte_count(bytes), Ordering::Relaxed);
}

#[inline(always)]
pub(crate) fn record_x_file_size() {
    increment(&X_FILE_SIZE_CALLS);
}

#[inline(always)]
pub(crate) fn record_x_lock() {
    increment(&X_LOCK_CALLS);
}

#[inline(always)]
pub(crate) fn record_x_unlock() {
    increment(&X_UNLOCK_CALLS);
}

#[inline(always)]
pub(crate) fn record_x_check_reserved_lock() {
    increment(&X_CHECK_RESERVED_LOCK_CALLS);
}

#[inline(always)]
pub(crate) fn record_x_file_control() {
    increment(&X_FILE_CONTROL_CALLS);
}

#[inline(always)]
pub(crate) fn record_x_device_characteristics() {
    increment(&X_DEVICE_CHARACTERISTICS_CALLS);
}

#[inline(always)]
pub(crate) fn record_stable_data_read(bytes: usize) {
    if !metrics_enabled() {
        return;
    }
    STABLE_DATA_READ_CALLS.fetch_add(1, Ordering::Relaxed);
    STABLE_DATA_READ_BYTES.fetch_add(byte_count(bytes), Ordering::Relaxed);
}

#[inline(always)]
pub(crate) fn record_stable_data_write(bytes: usize) {
    if !metrics_enabled() {
        return;
    }
    STABLE_DATA_WRITE_CALLS.fetch_add(1, Ordering::Relaxed);
    STABLE_DATA_WRITE_BYTES.fetch_add(byte_count(bytes), Ordering::Relaxed);
}

#[inline(always)]
pub(crate) fn record_stable_grow(pages: u64) {
    if !metrics_enabled() {
        return;
    }
    STABLE_GROW_CALLS.fetch_add(1, Ordering::Relaxed);
    STABLE_GROW_PAGES.fetch_add(pages, Ordering::Relaxed);
}

#[inline(always)]
pub(crate) fn record_superblock_load() {
    increment(&SUPERBLOCK_LOADS);
}

#[inline(always)]
pub(crate) fn record_commit_load(instructions: u64) {
    add(&COMMIT_LOAD, instructions);
}

#[inline(always)]
pub(crate) fn record_commit_capacity(instructions: u64) {
    add(&COMMIT_CAPACITY, instructions);
}

#[inline(always)]
pub(crate) fn record_commit_page_write(instructions: u64) {
    add(&COMMIT_PAGE_WRITE, instructions);
}

#[inline(always)]
pub(crate) fn record_commit_superblock_store(instructions: u64) {
    add(&COMMIT_SUPERBLOCK_STORE, instructions);
}

#[inline(always)]
fn increment(counter: &AtomicU64) {
    if metrics_enabled() {
        counter.fetch_add(1, Ordering::Relaxed);
    }
}

#[inline(always)]
fn add(counter: &AtomicU64, value: u64) {
    if metrics_enabled() {
        counter.fetch_add(value, Ordering::Relaxed);
    }
}

#[inline(always)]
fn byte_count(bytes: usize) -> u64 {
    u64::try_from(bytes).unwrap_or(u64::MAX)
}

#[inline(always)]
pub(crate) fn metrics_enabled() -> bool {
    METRICS_ENABLED.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[serial_test::serial]
    fn every_counter_records_when_metrics_are_enabled() {
        reset_read_metrics();

        record_x_read(3);
        record_x_write(5);
        record_x_file_size();
        record_x_lock();
        record_x_unlock();
        record_x_check_reserved_lock();
        record_x_file_control();
        record_x_device_characteristics();
        record_stable_data_read(7);
        record_stable_data_write(11);
        record_stable_grow(13);
        record_superblock_load();
        record_commit_load(17);
        record_commit_capacity(19);
        record_commit_page_write(23);
        record_commit_superblock_store(29);

        assert_eq!(
            read_metrics_snapshot(),
            ReadMetrics {
                x_read_calls: 1,
                x_read_bytes: 3,
                x_write_calls: 1,
                x_write_bytes: 5,
                x_file_size_calls: 1,
                x_lock_calls: 1,
                x_unlock_calls: 1,
                x_check_reserved_lock_calls: 1,
                x_file_control_calls: 1,
                x_device_characteristics_calls: 1,
                stable_data_read_calls: 1,
                stable_data_read_bytes: 7,
                stable_data_write_calls: 1,
                stable_data_write_bytes: 11,
                stable_grow_calls: 1,
                stable_grow_pages: 13,
                superblock_loads: 1,
                commit_load: 17,
                commit_capacity: 19,
                commit_page_write: 23,
                commit_superblock_store: 29,
            }
        );
    }

    #[test]
    #[serial_test::serial]
    fn reset_read_metrics_clears_every_counter_and_enables_metrics() {
        for counter in counters() {
            counter.store(41, Ordering::Relaxed);
        }
        disable_read_metrics();

        reset_read_metrics();

        assert!(metrics_enabled());
        assert_eq!(counters().len(), COUNTER_COUNT);
        assert_eq!(read_metrics_snapshot(), ReadMetrics::default());
        assert!(counters()
            .iter()
            .all(|counter| counter.load(Ordering::Relaxed) == 0));
    }

    #[test]
    #[serial_test::serial]
    fn disabled_metrics_ignore_recorders() {
        reset_read_metrics();
        disable_read_metrics();

        record_x_read(3);
        record_stable_data_write(5);
        record_commit_superblock_store(7);

        assert_eq!(read_metrics_snapshot(), ReadMetrics::default());
    }
}
