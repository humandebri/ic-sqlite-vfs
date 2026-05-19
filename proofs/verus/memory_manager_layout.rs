//! Verus model for the local MemoryManager-compatible layout.
//!
//! This proof stays outside production Rust. It verifies the pure arithmetic
//! contracts that guard bucket mapping and cache containment; tests compare the
//! production fork against `ic-stable-structures` for concrete operation traces.

use vstd::prelude::*;

verus! {

spec fn wasm_page_size() -> nat {
    65_536
}

spec fn bucket_size_in_pages() -> nat {
    128
}

spec fn max_num_buckets() -> nat {
    32_768
}

spec fn buckets_offset_in_pages() -> nat {
    1
}

spec fn u64_max() -> nat {
    18_446_744_073_709_551_615
}

spec fn bucket_size_in_bytes() -> nat {
    bucket_size_in_pages() * wasm_page_size()
}

spec fn max_managed_pages() -> nat {
    max_num_buckets() * bucket_size_in_pages()
}

spec fn bucket_address(bucket_id: nat) -> nat {
    buckets_offset_in_pages() * wasm_page_size() + bucket_size_in_bytes() * bucket_id
}

spec fn range_end(address: nat, length: nat) -> Option<nat> {
    if address + length <= u64_max() {
        Some(address + length)
    } else {
        None
    }
}

spec fn virtual_segment_contains(
    address: nat,
    length: nat,
    other_address: nat,
    other_length: nat,
) -> bool {
    match (range_end(address, length), range_end(other_address, other_length)) {
        (Some(end), Some(other_end)) => address <= other_address && other_end <= end,
        _ => false,
    }
}

proof fn bucket_size_is_8_mib()
    ensures
        bucket_size_in_bytes() == 8_388_608,
{
}

proof fn max_managed_memory_is_4m_pages()
    ensures
        max_managed_pages() == 4_194_304,
{
}

proof fn bucket_zero_starts_after_manager_page()
    ensures
        bucket_address(0) == 65_536,
{
}

proof fn bucket_addresses_are_monotonic(left: nat, right: nat)
    by (nonlinear_arith)
    requires
        left <= right,
    ensures
        bucket_address(left) <= bucket_address(right),
{
}

proof fn bucket_address_stride(bucket_id: nat)
    by (nonlinear_arith)
    ensures
        bucket_address(bucket_id + 1) == bucket_address(bucket_id) + bucket_size_in_bytes(),
{
}

proof fn range_end_accepts_u64_max_boundary()
    ensures
        range_end(u64_max(), 0) == Some(u64_max()),
{
}

proof fn range_end_rejects_u64_overflow()
    by (nonlinear_arith)
    ensures
        range_end(u64_max(), 1) == Option::<nat>::None,
{
}

proof fn segment_contains_accepts_inner_range()
    ensures
        virtual_segment_contains(100, 50, 120, 10),
{
}

proof fn segment_contains_rejects_overflowing_outer_range()
    by (nonlinear_arith)
    ensures
        !virtual_segment_contains((u64_max() - 4) as nat, 8, (u64_max() - 3) as nat, 1),
{
}

proof fn segment_contains_rejects_overflowing_inner_range()
    by (nonlinear_arith)
    ensures
        !virtual_segment_contains(0, 128, (u64_max() - 4) as nat, 8),
{
}

proof fn segment_contains_implies_ordered_bounds(
    address: nat,
    length: nat,
    other_address: nat,
    other_length: nat,
)
    ensures
        virtual_segment_contains(address, length, other_address, other_length) ==>
            address <= other_address,
{
}

proof fn segment_contains_implies_nonoverflowing_ends(
    address: nat,
    length: nat,
    other_address: nat,
    other_length: nat,
)
    ensures
        virtual_segment_contains(address, length, other_address, other_length) ==>
            matches!(range_end(address, length), Some(_)) &&
            matches!(range_end(other_address, other_length), Some(_)),
{
}

proof fn managed_pages_fit_in_u64()
    by (nonlinear_arith)
    ensures
        max_managed_pages() < u64_max(),
{
}

}
