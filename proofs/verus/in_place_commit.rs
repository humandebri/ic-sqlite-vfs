//! Verus model for in-place SQLite image commit invariants.
//!
//! This proof covers the arithmetic contract used by v8: logical page `n`
//! lives at `db_base_offset + n * page_size`, and commits keep that base stable.

use vstd::prelude::*;

verus! {

spec fn page_size() -> nat {
    16_384
}

spec fn page_count_for_size(size: nat) -> nat {
    if size == 0 {
        0
    } else {
        (((size - 1) / (page_size() as int)) + 1) as nat
    }
}

spec fn page_offset(db_base_offset: nat, page_no: nat) -> nat {
    db_base_offset + page_no * page_size()
}

spec fn page_end(db_base_offset: nat, page_no: nat) -> nat {
    page_offset(db_base_offset, page_no) + page_size()
}

spec fn active_page(page_no: nat, db_size: nat) -> bool {
    page_no < page_count_for_size(db_size)
}

struct Image {
    db_base_offset: nat,
    db_size: nat,
}

struct DirtyPage {
    page_no: nat,
}

spec fn commit_image(before: Image, final_size: nat) -> Image {
    Image { db_base_offset: before.db_base_offset, db_size: final_size }
}

spec fn dirty_write_offset(image: Image, dirty: DirtyPage) -> nat {
    page_offset(image.db_base_offset, dirty.page_no)
}

proof fn page_count_bounds(size: nat)
    by (nonlinear_arith)
    ensures
        size <= page_count_for_size(size) * page_size(),
        size == 0 ==> page_count_for_size(size) == 0,
        size > 0 ==> (page_count_for_size(size) - 1) * page_size() < size,
{
}

proof fn dirty_page_write_offset_matches_in_place_layout(image: Image, dirty: DirtyPage)
    ensures
        dirty_write_offset(image, dirty)
            == image.db_base_offset + dirty.page_no * page_size(),
{
}

proof fn commit_keeps_base_offset_stable(before: Image, final_size: nat)
    ensures
        commit_image(before, final_size).db_base_offset == before.db_base_offset,
        commit_image(before, final_size).db_size == final_size,
{
}

proof fn clean_page_offset_stays_stable_after_commit(
    before: Image,
    final_size: nat,
    clean_page_no: nat,
)
    ensures
        page_offset(commit_image(before, final_size).db_base_offset, clean_page_no)
            == page_offset(before.db_base_offset, clean_page_no),
{
}

proof fn active_page_write_fits_committed_image(image: Image, page_no: nat)
    by (nonlinear_arith)
    requires
        active_page(page_no, image.db_size),
    ensures
        page_end(image.db_base_offset, page_no)
            <= image.db_base_offset + page_count_for_size(image.db_size) * page_size(),
{
}

proof fn truncated_tail_page_is_inactive(final_size: nat, page_no: nat)
    requires
        page_no >= page_count_for_size(final_size),
    ensures
        !active_page(page_no, final_size),
{
}

proof fn truncate_page_count_matches_final_size(before: Image, final_size: nat)
    ensures
        commit_image(before, final_size).db_size == final_size,
        page_count_for_size(commit_image(before, final_size).db_size)
            == page_count_for_size(final_size),
{
}

}
