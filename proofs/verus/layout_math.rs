//! Verus model for SQLite stable-memory layout arithmetic.
//!
//! This proof file intentionally stays separate from production Rust. It checks
//! arithmetic invariants for the layout model; Rust tests compare boundary
//! behavior against production helpers.

use vstd::prelude::*;

verus! {

spec fn sqlite_page_size() -> nat {
    16_384
}

spec fn segment_page_count() -> nat {
    256
}

spec fn page_table_entry_len() -> nat {
    8
}

spec fn u64_max() -> nat {
    18_446_744_073_709_551_615
}

spec fn page_count_for_size(size: nat) -> nat {
    if size == 0 {
        0
    } else {
        (((size - 1) / (sqlite_page_size() as int)) + 1) as nat
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

spec fn page_no_from_segment(segment: nat, index: nat) -> nat {
    segment * segment_page_count() + index
}

spec fn root_table_bytes(entry_count: nat) -> nat {
    entry_count * page_table_entry_len()
}

spec fn import_offset_fits(base: nat, offset: nat) -> bool {
    base + offset <= u64_max()
}

spec fn import_area_fits(base: nat, total_size: nat) -> bool {
    base + total_size <= u64_max()
}

spec fn import_chunk_accepts(written_until: nat, offset: nat, len: nat, total_size: nat) -> bool {
    offset == written_until && offset + len <= total_size && import_offset_fits(offset, len)
}

spec fn import_chunk_accepts_at_base(
    base: nat,
    written_until: nat,
    offset: nat,
    len: nat,
    total_size: nat,
) -> bool {
    import_area_fits(base, total_size)
        && import_chunk_accepts(written_until, offset, len, total_size)
}

spec fn import_written_until_after(offset: nat, len: nat) -> nat {
    offset + len
}

proof fn page_count_zero()
    ensures
        page_count_for_size(0) == 0,
{
}

proof fn page_count_within_first_page(size: nat)
    by (nonlinear_arith)
    requires
        0 < size <= sqlite_page_size(),
    ensures
        page_count_for_size(size) == 1,
{
}

proof fn segment_count_zero()
    ensures
        segment_count_for_pages(0) == 0,
{
}

proof fn segment_index_is_bounded(page_no: nat)
    by (nonlinear_arith)
    ensures
        segment_index(page_no) < segment_page_count(),
{
}

proof fn page_recomposes_from_segment_and_index(page_no: nat)
    by (nonlinear_arith)
    ensures
        page_no_from_segment(segment_no(page_no), segment_index(page_no)) == page_no,
{
}

proof fn segment_count_covers_page_count(page_count: nat)
    by (nonlinear_arith)
    ensures
        page_count <= segment_count_for_pages(page_count) * segment_page_count(),
{
}

proof fn root_table_bytes_scales_by_entry_count(entry_count: nat)
    ensures
        root_table_bytes(entry_count) == entry_count * 8,
{
}

proof fn import_offset_at_u64_max_fits()
    ensures
        import_offset_fits(u64_max(), 0),
{
}

proof fn import_offset_past_u64_max_overflows()
    by (nonlinear_arith)
    ensures
        !import_offset_fits(u64_max(), 1),
{
}

proof fn accepted_import_chunk_advances_monotonically(
    written_until: nat,
    len: nat,
    total_size: nat,
)
    by (nonlinear_arith)
    requires
        import_chunk_accepts(written_until, written_until, len, total_size),
    ensures
        import_written_until_after(written_until, len) >= written_until,
        import_written_until_after(written_until, len) <= total_size,
{
}

proof fn accepted_import_chunk_stays_inside_import_area(
    base: nat,
    written_until: nat,
    offset: nat,
    len: nat,
    total_size: nat,
)
    by (nonlinear_arith)
    requires
        import_chunk_accepts_at_base(base, written_until, offset, len, total_size),
    ensures
        import_offset_fits(base, offset),
        import_offset_fits(base, offset + len),
        base + offset + len <= base + total_size,
        base + offset + len <= u64_max(),
{
}

}
