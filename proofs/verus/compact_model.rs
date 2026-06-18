//! Verus model for v8 no-op compact invariants.
//!
//! The public compact API remains available, but the in-place layout does not
//! rewrite page data or metadata beyond rejecting unsupported layout versions.

use vstd::prelude::*;

verus! {

struct Image {
    db_base_offset: nat,
    db_size: nat,
    page_count: nat,
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

proof fn compact_preserves_checksum_metadata(image: Image)
    ensures
        compact(image).checksum == image.checksum,
        compact(image).last_tx_id == image.last_tx_id,
{
}

}
