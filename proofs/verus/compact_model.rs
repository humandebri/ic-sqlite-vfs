//! Verus model for v8 no-op compact invariants.
//!
//! The public compact API remains available, but the in-place layout does not
//! rewrite page data or metadata beyond rejecting unsupported layout versions.
//!
//! Capacity proof mapping:
//! - T6: compact is no-op, not reclaim, and preserves resource high water.

use vstd::prelude::*;

verus! {

struct Image {
    db_base_offset: nat,
    db_size: nat,
    page_table_offset: nat,
    page_table_bytes: nat,
    page_count: nat,
    allocated_bytes: nat,
    high_water_mark: nat,
    orphan_bytes_estimate: nat,
    compact_recommended: bool,
    checksum: nat,
    last_tx_id: nat,
}

spec fn compact(image: Image) -> Image {
    image
}

proof fn compact_preserves_db_base_offset(image: Image)
    ensures
        compact(image).db_base_offset == image.db_base_offset,
{
}

proof fn compact_preserves_logical_size(image: Image)
    ensures
        compact(image).db_size == image.db_size,
        compact(image).page_count == image.page_count,
{
}

proof fn compact_preserves_page_table_absence(image: Image)
    ensures
        compact(image).page_table_offset == image.page_table_offset,
        compact(image).page_table_bytes == image.page_table_bytes,
{
}

proof fn compact_preserves_resource_high_water(image: Image)
    ensures
        compact(image).allocated_bytes == image.allocated_bytes,
        compact(image).high_water_mark == image.high_water_mark,
        compact(image).orphan_bytes_estimate == image.orphan_bytes_estimate,
        compact(image).compact_recommended == image.compact_recommended,
{
}

proof fn compact_never_recommends_reclaim(image: Image)
    requires
        image.compact_recommended == false,
    ensures
        compact(image).compact_recommended == false,
{
}

proof fn compact_preserves_checksum_metadata(image: Image)
    ensures
        compact(image).checksum == image.checksum,
        compact(image).last_tx_id == image.last_tx_id,
{
}

}
