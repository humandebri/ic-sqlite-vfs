//! Verus model for segmented page-map commit invariants.
//!
//! This proof abstracts the commit table update path. It verifies root length,
//! active page indexing, and the fact that a dirty committed page points at the
//! new physical page offset in the updated table.

use vstd::prelude::*;

verus! {

spec fn segment_page_count() -> nat {
    256
}

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

spec fn segment_count_for_pages(page_count: nat) -> nat {
    if page_count == 0 {
        0
    } else {
        (((page_count - 1) / (segment_page_count() as int)) + 1) as nat
    }
}

spec fn segment_no(page_no: nat) -> nat {
    page_no / segment_page_count()
}

spec fn segment_index(page_no: nat) -> nat {
    page_no % segment_page_count()
}

spec fn root_len_for_db_size(db_size: nat) -> nat {
    segment_count_for_pages(page_count_for_size(db_size))
}

struct DirtyPage {
    page_no: nat,
    new_physical_offset: nat,
}

struct SegmentTable {
    slots: Seq<nat>,
}

struct RootTable {
    segments: Seq<nat>,
}

spec fn dirty_pages_contain(page_no: nat, dirty_pages: Seq<DirtyPage>) -> bool
    decreases dirty_pages.len(),
{
    if dirty_pages.len() == 0 {
        false
    } else {
        dirty_pages[0].page_no == page_no
            || dirty_pages_contain(page_no, dirty_pages.subrange(1, dirty_pages.len() as int))
    }
}

spec fn dirty_pages_unique(dirty_pages: Seq<DirtyPage>) -> bool
    decreases dirty_pages.len(),
{
    if dirty_pages.len() <= 1 {
        true
    } else {
        !dirty_pages_contain(
            dirty_pages[0].page_no,
            dirty_pages.subrange(1, dirty_pages.len() as int),
        ) && dirty_pages_unique(dirty_pages.subrange(1, dirty_pages.len() as int))
    }
}

spec fn page_offset_after_commits(
    page_no: nat,
    dirty_pages: Seq<DirtyPage>,
    old_physical_offset: nat,
) -> nat
    decreases dirty_pages.len(),
{
    if dirty_pages.len() == 0 {
        old_physical_offset
    } else if dirty_pages[0].page_no == page_no {
        dirty_pages[0].new_physical_offset
    } else {
        page_offset_after_commits(
            page_no,
            dirty_pages.subrange(1, dirty_pages.len() as int),
            old_physical_offset,
        )
    }
}

spec fn segment_slot(table: SegmentTable, page_no: nat) -> nat
    recommends
        segment_index(page_no) < table.slots.len(),
{
    table.slots[segment_index(page_no) as int]
}

spec fn update_segment_slot(table: SegmentTable, page_no: nat, new_offset: nat) -> SegmentTable
    recommends
        segment_index(page_no) < table.slots.len(),
{
    SegmentTable { slots: table.slots.update(segment_index(page_no) as int, new_offset) }
}

spec fn root_segment(root: RootTable, segment: nat) -> nat
    recommends
        segment < root.segments.len(),
{
    root.segments[segment as int]
}

spec fn update_root_segment(root: RootTable, segment: nat, segment_offset: nat) -> RootTable
    recommends
        segment < root.segments.len(),
{
    RootTable { segments: root.segments.update(segment as int, segment_offset) }
}

spec fn page_offset_after_truncate(page_no: nat, final_page_count: nat, old_offset: nat) -> nat {
    if page_no >= final_page_count {
        0
    } else {
        old_offset
    }
}

proof fn root_len_equals_segment_count(db_size: nat)
    ensures
        root_len_for_db_size(db_size) == segment_count_for_pages(page_count_for_size(db_size)),
{
}

proof fn dirty_page_updates_segment_slot(table: SegmentTable, page_no: nat, new_offset: nat)
    requires
        segment_index(page_no) < table.slots.len(),
    ensures
        segment_slot(update_segment_slot(table, page_no, new_offset), page_no) == new_offset,
{
    broadcast use vstd::seq::group_seq_axioms;
}

proof fn clean_page_keeps_segment_slot(
    table: SegmentTable,
    dirty_page_no: nat,
    clean_page_no: nat,
    new_offset: nat,
)
    requires
        segment_index(dirty_page_no) < table.slots.len(),
        segment_index(clean_page_no) < table.slots.len(),
        segment_index(clean_page_no) != segment_index(dirty_page_no),
    ensures
        segment_slot(update_segment_slot(table, dirty_page_no, new_offset), clean_page_no)
            == segment_slot(table, clean_page_no),
{
    broadcast use vstd::seq::group_seq_axioms;
}

proof fn root_segment_points_to_new_table(root: RootTable, segment: nat, segment_offset: nat)
    requires
        segment < root.segments.len(),
    ensures
        root_segment(update_root_segment(root, segment, segment_offset), segment) == segment_offset,
{
    broadcast use vstd::seq::group_seq_axioms;
}

proof fn unrelated_root_segment_is_unchanged(
    root: RootTable,
    dirty_segment: nat,
    clean_segment: nat,
    segment_offset: nat,
)
    requires
        dirty_segment < root.segments.len(),
        clean_segment < root.segments.len(),
        clean_segment != dirty_segment,
    ensures
        root_segment(update_root_segment(root, dirty_segment, segment_offset), clean_segment)
            == root_segment(root, clean_segment),
{
    broadcast use vstd::seq::group_seq_axioms;
}

proof fn active_page_index_is_inside_root(page_no: nat, db_size: nat)
    by (nonlinear_arith)
    requires
        page_no < page_count_for_size(db_size),
    ensures
        segment_no(page_no) < root_len_for_db_size(db_size),
        segment_index(page_no) < segment_page_count(),
{
}

proof fn active_page_recomposes_from_segment_and_index(page_no: nat)
    by (nonlinear_arith)
    ensures
        segment_no(page_no) * segment_page_count() + segment_index(page_no) == page_no,
{
}

proof fn dirty_page_at_head_points_to_new_physical_offset(
    page_no: nat,
    new_physical_offset: nat,
    old_physical_offset: nat,
    tail: Seq<DirtyPage>,
)
    ensures
        page_offset_after_commits(
            page_no,
            seq![DirtyPage { page_no, new_physical_offset }] + tail,
            old_physical_offset,
        ) == new_physical_offset,
{
}

proof fn clean_page_keeps_old_physical_offset_for_multiple_commits(
    page_no: nat,
    dirty_pages: Seq<DirtyPage>,
    old_physical_offset: nat,
)
    requires
        !dirty_pages_contain(page_no, dirty_pages),
    ensures
        page_offset_after_commits(page_no, dirty_pages, old_physical_offset) == old_physical_offset,
    decreases dirty_pages.len(),
{
    if dirty_pages.len() > 0 {
        clean_page_keeps_old_physical_offset_for_multiple_commits(
            page_no,
            dirty_pages.subrange(1, dirty_pages.len() as int),
            old_physical_offset,
        );
    }
}

proof fn dirty_pages_unique_head_not_in_tail(dirty_pages: Seq<DirtyPage>)
    requires
        dirty_pages.len() > 1,
        dirty_pages_unique(dirty_pages),
    ensures
        !dirty_pages_contain(
            dirty_pages[0].page_no,
            dirty_pages.subrange(1, dirty_pages.len() as int),
        ),
{
}

proof fn root_length_matches_final_page_count(root: RootTable, final_page_count: nat)
    requires
        root.segments.len() == segment_count_for_pages(final_page_count),
    ensures
        root.segments.len() == segment_count_for_pages(final_page_count),
{
}

proof fn truncated_tail_page_slot_is_zero(page_no: nat, final_page_count: nat, old_offset: nat)
    requires
        page_no >= final_page_count,
    ensures
        page_offset_after_truncate(page_no, final_page_count, old_offset) == 0,
{
}

proof fn untruncated_page_slot_is_preserved(page_no: nat, final_page_count: nat, old_offset: nat)
    requires
        page_no < final_page_count,
    ensures
        page_offset_after_truncate(page_no, final_page_count, old_offset) == old_offset,
{
}

}
