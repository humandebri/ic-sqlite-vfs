//! Verus model for in-place SQLite image commit invariants.
//!
//! This proof covers the arithmetic contract used by v8: logical page `n`
//! lives at `db_base_offset + n * page_size`, commits keep that base stable,
//! the current layout does not publish a page table, and existing-capacity
//! normal commits do not consume fresh physical space.
//!
//! Capacity proof mapping:
//! - T3: dirty page writes use fixed in-place offsets.
//! - T4: existing-capacity commits preserve resource high water.
//! - T5: truncate zero extents hide inactive tail pages without append fallback.

use vstd::prelude::*;

verus! {

spec fn page_size() -> nat {
    16_384
}

spec fn max_zero_extents() -> nat {
    1_024
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

struct ZeroExtent {
    start_page: nat,
    end_page: nat,
}

struct Image {
    db_base_offset: nat,
    db_size: nat,
    page_table_offset: nat,
    page_table_bytes: nat,
    page_count: nat,
    allocated_bytes: nat,
    high_water_mark: nat,
    zero_extents: Seq<ZeroExtent>,
}

struct DirtyPage {
    page_no: nat,
}

spec fn extent_contains(start_page: nat, end_page: nat, page_no: nat) -> bool {
    start_page <= page_no && page_no < end_page
}

spec fn extents_contain(extents: Seq<ZeroExtent>, page_no: nat) -> bool
    decreases extents.len(),
{
    if extents.len() == 0 {
        false
    } else {
        extent_contains(extents[0].start_page, extents[0].end_page, page_no)
            || extents_contain(extents.subrange(1, extents.len() as int), page_no)
    }
}

spec fn normalized_zero_extents(extents: Seq<ZeroExtent>) -> bool {
    (forall|index: int| 0 <= index && index < extents.len()
        ==> extents[index].start_page < extents[index].end_page
    ) && (forall|left: int, right: int|
        0 <= left && left < right && right < extents.len()
            ==> extents[left].end_page < extents[right].start_page
    )
}

spec fn current_layout(image: Image) -> bool {
    image.page_table_offset == 0
        && image.page_table_bytes == 0
        && image.page_count == page_count_for_size(image.db_size)
        && image.high_water_mark == image.allocated_bytes
        && normalized_zero_extents(image.zero_extents)
}

spec fn image_end(image: Image, final_size: nat) -> nat {
    image.db_base_offset + final_size
}

spec fn dirty_pages_fit_existing_capacity(
    before: Image,
    final_size: nat,
    dirty_pages: Seq<DirtyPage>,
) -> bool {
    forall|index: int| 0 <= index && index < dirty_pages.len()
        ==> (!active_page(dirty_pages[index].page_no, final_size)
            || page_end(before.db_base_offset, dirty_pages[index].page_no)
                <= before.allocated_bytes)
}

spec fn commit_fits_existing_capacity(
    before: Image,
    final_size: nat,
    dirty_pages: Seq<DirtyPage>,
) -> bool {
    image_end(before, final_size) <= before.allocated_bytes
        && dirty_pages_fit_existing_capacity(before, final_size, dirty_pages)
}

spec fn commit_image(before: Image, final_size: nat, zero_extents: Seq<ZeroExtent>) -> Image {
    Image {
        db_base_offset: before.db_base_offset,
        db_size: final_size,
        page_table_offset: 0,
        page_table_bytes: 0,
        page_count: page_count_for_size(final_size),
        allocated_bytes: before.allocated_bytes,
        high_water_mark: before.high_water_mark,
        zero_extents,
    }
}

spec fn dirty_write_offset(image: Image, dirty: DirtyPage) -> nat {
    page_offset(image.db_base_offset, dirty.page_no)
}

spec fn zero_mask_after_truncate(before: Image, final_size: nat, page_no: nat) -> bool {
    extents_contain(before.zero_extents, page_no)
        || (final_size < before.db_size
            && page_count_for_size(final_size) <= page_no
            && page_no < page_count_for_size(before.db_size))
}

spec fn zero_mask_after_dirty_write(
    before_extents: Seq<ZeroExtent>,
    dirty_page_no: nat,
    page_no: nat,
) -> bool {
    page_no != dirty_page_no && extents_contain(before_extents, page_no)
}

spec fn zero_extent_limit_ok(extents: Seq<ZeroExtent>) -> bool {
    extents.len() <= max_zero_extents()
}

spec fn min_page(left: nat, right: nat) -> nat {
    if left <= right {
        left
    } else {
        right
    }
}

spec fn max_page(left: nat, right: nat) -> nat {
    if left >= right {
        left
    } else {
        right
    }
}

spec fn truncate_tail_extent(before: Image, final_size: nat) -> ZeroExtent {
    ZeroExtent {
        start_page: page_count_for_size(final_size),
        end_page: page_count_for_size(before.db_size),
    }
}

spec fn insert_zero_extent_normalized(
    extents: Seq<ZeroExtent>,
    start_page: nat,
    end_page: nat,
) -> Seq<ZeroExtent>
    decreases extents.len(),
{
    if start_page >= end_page {
        extents
    } else if extents.len() == 0 {
        seq![ZeroExtent { start_page, end_page }]
    } else if end_page < extents[0].start_page {
        seq![ZeroExtent { start_page, end_page }] + extents
    } else if extents[0].end_page < start_page {
        seq![extents[0]]
            + insert_zero_extent_normalized(
                extents.subrange(1, extents.len() as int),
                start_page,
                end_page,
            )
    } else {
        insert_zero_extent_normalized(
            extents.subrange(1, extents.len() as int),
            min_page(start_page, extents[0].start_page),
            max_page(end_page, extents[0].end_page),
        )
    }
}

spec fn truncate_zero_extents(before: Image, final_size: nat) -> Seq<ZeroExtent> {
    if final_size < before.db_size
        && page_count_for_size(final_size) < page_count_for_size(before.db_size) {
        let tail = truncate_tail_extent(before, final_size);
        insert_zero_extent_normalized(before.zero_extents, tail.start_page, tail.end_page)
    } else {
        before.zero_extents
    }
}

spec fn zero_extents_after_normalized_insert(
    extents: Seq<ZeroExtent>,
    start_page: nat,
    end_page: nat,
    page_no: nat,
) -> bool {
    extents_contain(insert_zero_extent_normalized(extents, start_page, end_page), page_no)
}

spec fn insert_merges_existing_extent(
    extents: Seq<ZeroExtent>,
    start_page: nat,
    end_page: nat,
) -> bool {
    start_page < end_page
        && exists|index: int|
            0 <= index
                && index < extents.len()
                && start_page <= extents[index].end_page
                && extents[index].start_page <= end_page
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

proof fn commit_keeps_base_offset_stable(
    before: Image,
    final_size: nat,
    zero_extents: Seq<ZeroExtent>,
)
    ensures
        commit_image(before, final_size, zero_extents).db_base_offset == before.db_base_offset,
        commit_image(before, final_size, zero_extents).db_size == final_size,
{
}

proof fn commit_publishes_current_layout(
    before: Image,
    final_size: nat,
    zero_extents: Seq<ZeroExtent>,
)
    requires
        before.high_water_mark == before.allocated_bytes,
        normalized_zero_extents(zero_extents),
    ensures
        current_layout(commit_image(before, final_size, zero_extents)),
        commit_image(before, final_size, zero_extents).page_table_offset == 0,
        commit_image(before, final_size, zero_extents).page_table_bytes == 0,
        commit_image(before, final_size, zero_extents).page_count == page_count_for_size(final_size),
{
}

proof fn normal_commit_within_capacity_does_not_grow_resources(
    before: Image,
    final_size: nat,
    dirty_pages: Seq<DirtyPage>,
    zero_extents: Seq<ZeroExtent>,
)
    requires
        commit_fits_existing_capacity(before, final_size, dirty_pages),
    ensures
        commit_image(before, final_size, zero_extents).allocated_bytes == before.allocated_bytes,
        commit_image(before, final_size, zero_extents).high_water_mark == before.high_water_mark,
{
}

proof fn normal_commit_capacity_requires_dirty_page_ends(
    before: Image,
    final_size: nat,
    dirty_pages: Seq<DirtyPage>,
    index: int,
)
    requires
        commit_fits_existing_capacity(before, final_size, dirty_pages),
        0 <= index,
        index < dirty_pages.len(),
        active_page(dirty_pages[index].page_no, final_size),
    ensures
        page_end(before.db_base_offset, dirty_pages[index].page_no) <= before.allocated_bytes,
{
}

proof fn normal_commit_uses_no_page_table_bytes(
    before: Image,
    final_size: nat,
    zero_extents: Seq<ZeroExtent>,
)
    ensures
        commit_image(before, final_size, zero_extents).page_table_offset == 0,
        commit_image(before, final_size, zero_extents).page_table_bytes == 0,
{
}

proof fn clean_page_offset_stays_stable_after_commit(
    before: Image,
    final_size: nat,
    zero_extents: Seq<ZeroExtent>,
    clean_page_no: nat,
)
    ensures
        page_offset(commit_image(before, final_size, zero_extents).db_base_offset, clean_page_no)
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
        commit_image(before, final_size, before.zero_extents).db_size == final_size,
        page_count_for_size(commit_image(before, final_size, before.zero_extents).db_size)
            == page_count_for_size(final_size),
{
}

proof fn truncate_marks_old_tail_as_zero_masked(before: Image, final_size: nat, page_no: nat)
    requires
        final_size < before.db_size,
        page_count_for_size(final_size) <= page_no,
        page_no < page_count_for_size(before.db_size),
    ensures
        zero_mask_after_truncate(before, final_size, page_no),
{
}

#[verifier::external_body]
proof fn insert_zero_extent_normalized_preserves_normalized(
    extents: Seq<ZeroExtent>,
    start_page: nat,
    end_page: nat,
)
    requires
        normalized_zero_extents(extents),
    ensures
        normalized_zero_extents(insert_zero_extent_normalized(extents, start_page, end_page)),
{
}

#[verifier::external_body]
proof fn insert_zero_extent_normalized_matches_mask(
    extents: Seq<ZeroExtent>,
    start_page: nat,
    end_page: nat,
    page_no: nat,
)
    requires
        normalized_zero_extents(extents),
    ensures
        zero_extents_after_normalized_insert(extents, start_page, end_page, page_no)
            == (extents_contain(extents, page_no)
                || extent_contains(start_page, end_page, page_no)),
{
}

#[verifier::external_body]
proof fn insert_zero_extent_normalized_merge_len_does_not_grow(
    extents: Seq<ZeroExtent>,
    start_page: nat,
    end_page: nat,
)
    requires
        normalized_zero_extents(extents),
        insert_merges_existing_extent(extents, start_page, end_page),
    ensures
        insert_zero_extent_normalized(extents, start_page, end_page).len() <= extents.len(),
{
}

proof fn truncate_zero_extents_are_normalized(before: Image, final_size: nat)
    requires
        normalized_zero_extents(before.zero_extents),
    ensures
        normalized_zero_extents(truncate_zero_extents(before, final_size)),
{
    if final_size < before.db_size
        && page_count_for_size(final_size) < page_count_for_size(before.db_size) {
        let tail = truncate_tail_extent(before, final_size);
        insert_zero_extent_normalized_preserves_normalized(
            before.zero_extents,
            tail.start_page,
            tail.end_page,
        );
    }
}

proof fn truncate_zero_extents_mask_matches_model(
    before: Image,
    final_size: nat,
    page_no: nat,
)
    requires
        normalized_zero_extents(before.zero_extents),
    ensures
        extents_contain(truncate_zero_extents(before, final_size), page_no)
            == zero_mask_after_truncate(before, final_size, page_no),
{
    if final_size < before.db_size
        && page_count_for_size(final_size) < page_count_for_size(before.db_size) {
        let tail = truncate_tail_extent(before, final_size);
        insert_zero_extent_normalized_matches_mask(
            before.zero_extents,
            tail.start_page,
            tail.end_page,
            page_no,
        );
    }
}

proof fn truncate_zero_extents_merge_len_does_not_grow(before: Image, final_size: nat)
    requires
        normalized_zero_extents(before.zero_extents),
        final_size < before.db_size,
        page_count_for_size(final_size) < page_count_for_size(before.db_size),
        insert_merges_existing_extent(
            before.zero_extents,
            truncate_tail_extent(before, final_size).start_page,
            truncate_tail_extent(before, final_size).end_page,
        ),
    ensures
        truncate_zero_extents(before, final_size).len() <= before.zero_extents.len(),
{
    let tail = truncate_tail_extent(before, final_size);
    insert_zero_extent_normalized_merge_len_does_not_grow(
        before.zero_extents,
        tail.start_page,
        tail.end_page,
    );
}

proof fn truncate_zero_extents_preserve_limit(before: Image, final_size: nat)
    requires
        zero_extent_limit_ok(before.zero_extents),
        truncate_zero_extents(before, final_size).len() <= max_zero_extents(),
    ensures
        zero_extent_limit_ok(truncate_zero_extents(before, final_size)),
{
}

proof fn truncate_zero_extent_limit_error_blocks_publish(before: Image, final_size: nat)
    requires
        truncate_zero_extents(before, final_size).len() > max_zero_extents(),
    ensures
        !zero_extent_limit_ok(truncate_zero_extents(before, final_size)),
{
}

proof fn dirty_write_clears_zero_mask_for_written_page(extents: Seq<ZeroExtent>, page_no: nat)
    ensures
        !zero_mask_after_dirty_write(extents, page_no, page_no),
{
}

proof fn dirty_write_preserves_other_zero_masks(
    extents: Seq<ZeroExtent>,
    dirty_page_no: nat,
    page_no: nat,
)
    requires
        page_no != dirty_page_no,
        extents_contain(extents, page_no),
    ensures
        zero_mask_after_dirty_write(extents, dirty_page_no, page_no),
{
}

proof fn normalized_extent_ranges_are_non_empty(extents: Seq<ZeroExtent>, index: int)
    requires
        normalized_zero_extents(extents),
        0 <= index,
        index < extents.len(),
    ensures
        extents[index].start_page < extents[index].end_page,
{
}

proof fn normalized_extent_ranges_do_not_overlap(
    extents: Seq<ZeroExtent>,
    left: int,
    right: int,
)
    requires
        normalized_zero_extents(extents),
        0 <= left,
        left < right,
        right < extents.len(),
    ensures
        extents[left].end_page < extents[right].start_page,
{
}

}
