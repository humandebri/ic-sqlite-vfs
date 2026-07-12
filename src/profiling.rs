//! Internal profiling hooks for debug, tests, and benchmark canisters.
//!
//! Production release builds compile these helpers to no-ops unless
//! `bench-profile` is enabled.

#[derive(Clone, Copy, Debug)]
pub(crate) struct ProfileTimer {
    start: Option<u64>,
}

impl ProfileTimer {
    #[inline(always)]
    pub(crate) fn start_if(enabled: bool) -> Self {
        Self {
            start: instruction_counter_if_enabled(enabled),
        }
    }

    #[cfg(feature = "bench-profile")]
    #[inline(always)]
    pub(crate) fn start() -> Self {
        Self::start_if(true)
    }

    #[cfg(feature = "bench-profile")]
    #[inline(always)]
    pub(crate) fn elapsed(self) -> u64 {
        elapsed_since(self.start).unwrap_or(0)
    }
}

#[cfg(any(test, debug_assertions, feature = "bench-profile"))]
#[inline(always)]
pub(crate) fn metrics_enabled() -> bool {
    crate::read_metrics::metrics_enabled()
}

#[cfg(not(any(test, debug_assertions, feature = "bench-profile")))]
#[inline(always)]
pub(crate) fn metrics_enabled() -> bool {
    false
}

macro_rules! commit_profile_recorder {
    ($name:ident, $record:ident) => {
        #[cfg(any(test, debug_assertions, feature = "bench-profile"))]
        #[inline(always)]
        pub(crate) fn $name(timer: ProfileTimer) {
            if let Some(elapsed) = elapsed_since(timer.start) {
                crate::read_metrics::$record(elapsed);
            }
        }

        #[cfg(not(any(test, debug_assertions, feature = "bench-profile")))]
        #[inline(always)]
        pub(crate) fn $name(_timer: ProfileTimer) {}
    };
}

commit_profile_recorder!(record_commit_capacity, record_commit_capacity);
commit_profile_recorder!(record_commit_load, record_commit_load);
commit_profile_recorder!(record_commit_page_write, record_commit_page_write);
commit_profile_recorder!(
    record_commit_superblock_store,
    record_commit_superblock_store
);

#[cfg(any(test, debug_assertions, feature = "bench-profile"))]
#[inline(always)]
fn instruction_counter_if_enabled(enabled: bool) -> Option<u64> {
    if enabled {
        Some(crate::ic0_shim::performance_counter(0))
    } else {
        None
    }
}

#[cfg(not(any(test, debug_assertions, feature = "bench-profile")))]
#[inline(always)]
fn instruction_counter_if_enabled(_enabled: bool) -> Option<u64> {
    None
}

#[cfg(any(test, debug_assertions, feature = "bench-profile"))]
#[inline(always)]
fn elapsed_since(start: Option<u64>) -> Option<u64> {
    start.map(|start| crate::ic0_shim::performance_counter(0).saturating_sub(start))
}
