//! Minimal IC system API shim for non-canister modules.
//!
//! Canister API exports still use `ic_cdk`, but low-level VFS diagnostics only
//! need raw `ic0` imports. Keeping them here makes `ic_cdk` optional outside the
//! `canister-api` feature.

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "ic0")]
extern "C" {
    #[link_name = "performance_counter"]
    fn raw_performance_counter(counter_type: u32) -> u64;
    #[link_name = "time"]
    fn raw_time() -> u64;
    #[cfg(feature = "canister-api-test-failpoints")]
    #[link_name = "trap"]
    fn raw_trap(src: usize, size: usize) -> !;
}

#[inline(always)]
pub(crate) fn performance_counter(counter_type: u32) -> u64 {
    #[cfg(target_arch = "wasm32")]
    {
        unsafe { raw_performance_counter(counter_type) }
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = counter_type;
        0
    }
}

#[cfg(target_arch = "wasm32")]
#[inline(always)]
pub(crate) fn time() -> u64 {
    unsafe { raw_time() }
}

#[cfg(all(target_arch = "wasm32", feature = "canister-api-test-failpoints"))]
#[inline(always)]
pub(crate) fn trap(message: &str) -> ! {
    unsafe { raw_trap(message.as_ptr().addr(), message.len()) }
}
