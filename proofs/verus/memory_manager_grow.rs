//! Verus model for MemoryManager::grow bucket arithmetic.
//!
//! This proof isolates the grow path that maps logical memory pages to backing
//! buckets. It checks monotonic bucket demand and the subtraction/grow deltas
//! used by the production implementation.

use vstd::prelude::*;

verus! {

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

spec fn div_ceil(numerator: nat, denominator: nat) -> nat {
    if numerator == 0 {
        0
    } else {
        (((numerator - 1) / (denominator as int)) + 1) as nat
    }
}

spec fn num_buckets_needed(pages: nat) -> nat {
    div_ceil(pages, bucket_size_in_pages())
}

spec fn checked_add(left: nat, right: nat) -> Option<nat> {
    if left + right <= u64_max() {
        Some(left + right)
    } else {
        None
    }
}

spec fn checked_sub(left: nat, right: nat) -> Option<nat> {
    if right <= left {
        Some((left - right) as nat)
    } else {
        None
    }
}

spec fn pages_needed_for_buckets(target_buckets: nat) -> nat {
    buckets_offset_in_pages() + bucket_size_in_pages() * target_buckets
}

proof fn div_ceil_monotonic(left: nat, right: nat)
    by (nonlinear_arith)
    requires
        left <= right,
    ensures
        div_ceil(left, bucket_size_in_pages()) <= div_ceil(right, bucket_size_in_pages()),
{
}

proof fn bucket_need_is_monotonic(old_size: nat, new_size: nat)
    requires
        old_size <= new_size,
    ensures
        num_buckets_needed(old_size) <= num_buckets_needed(new_size),
{
    div_ceil_monotonic(old_size, new_size);
}

proof fn checked_add_pages_implies_bucket_subtraction_safe(
    old_size: nat,
    pages: nat,
    new_size: nat,
)
    requires
        checked_add(old_size, pages) == Some(new_size),
    ensures
        old_size <= new_size,
        num_buckets_needed(old_size) <= num_buckets_needed(new_size),
        matches!(
            checked_sub(num_buckets_needed(new_size), num_buckets_needed(old_size)),
            Some(_),
        ),
{
    bucket_need_is_monotonic(old_size, new_size);
}

proof fn target_bucket_count_within_max(
    allocated_buckets: nat,
    new_buckets: nat,
    target_allocated_buckets: nat,
)
    requires
        checked_add(allocated_buckets, new_buckets) == Some(target_allocated_buckets),
        target_allocated_buckets <= max_num_buckets(),
    ensures
        target_allocated_buckets <= max_num_buckets(),
        allocated_buckets <= target_allocated_buckets,
{
}

proof fn pages_needed_for_max_buckets_fits_u64(target_allocated_buckets: nat)
    by (nonlinear_arith)
    requires
        target_allocated_buckets <= max_num_buckets(),
    ensures
        pages_needed_for_buckets(target_allocated_buckets) <= u64_max(),
{
}

proof fn backing_grow_delta_is_safe(current_pages: nat, pages_needed: nat)
    requires
        current_pages <= pages_needed,
    ensures
        checked_sub(pages_needed, current_pages) == Some((pages_needed - current_pages) as nat),
{
}

proof fn backing_pages_cover_target_buckets(target_allocated_buckets: nat)
    ensures
        pages_needed_for_buckets(target_allocated_buckets)
            == buckets_offset_in_pages() + bucket_size_in_pages() * target_allocated_buckets,
{
}

}
