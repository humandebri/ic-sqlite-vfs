//! Verus model for MemoryManager allocation-table invariants.
//!
//! The production loader rejects allocated buckets without an owner, unallocated
//! buckets with an owner, and memory size / bucket count mismatches.

use vstd::prelude::*;

verus! {

spec fn max_num_memories() -> nat {
    255
}

spec fn max_num_buckets() -> nat {
    32_768
}

spec fn unallocated_marker() -> nat {
    max_num_memories()
}

spec fn bucket_size_in_pages() -> nat {
    128
}

spec fn bucket_count_for_pages(pages: nat) -> nat {
    if pages == 0 {
        0
    } else {
        (((pages - 1) / (bucket_size_in_pages() as int)) + 1) as nat
    }
}

spec fn allocated_owner_valid(bucket: nat, allocated_buckets: nat, owner: nat) -> bool {
    bucket < allocated_buckets ==> owner < max_num_memories()
}

spec fn unallocated_owner_valid(bucket: nat, allocated_buckets: nat, owner: nat) -> bool {
    allocated_buckets <= bucket < max_num_buckets() ==> owner == unallocated_marker()
}

spec fn memory_size_matches_bucket_count(size_pages: nat, bucket_count: nat) -> bool {
    bucket_count_for_pages(size_pages) == bucket_count
}

proof fn allocated_bucket_owner_is_memory_id(bucket: nat, allocated_buckets: nat, owner: nat)
    requires
        bucket < allocated_buckets,
        allocated_owner_valid(bucket, allocated_buckets, owner),
    ensures
        owner < max_num_memories(),
{
}

proof fn unallocated_bucket_uses_marker(bucket: nat, allocated_buckets: nat, owner: nat)
    requires
        allocated_buckets <= bucket < max_num_buckets(),
        unallocated_owner_valid(bucket, allocated_buckets, owner),
    ensures
        owner == unallocated_marker(),
{
}

proof fn bucket_count_covers_memory_size(size_pages: nat)
    by (nonlinear_arith)
    ensures
        size_pages <= bucket_count_for_pages(size_pages) * bucket_size_in_pages(),
{
}

proof fn matching_memory_size_has_expected_bucket_count(size_pages: nat, bucket_count: nat)
    requires
        memory_size_matches_bucket_count(size_pages, bucket_count),
    ensures
        bucket_count == bucket_count_for_pages(size_pages),
{
}

proof fn allocated_bucket_count_is_within_table(allocated_buckets: nat)
    requires
        allocated_buckets <= max_num_buckets(),
    ensures
        allocated_buckets <= 32_768,
{
}

}
