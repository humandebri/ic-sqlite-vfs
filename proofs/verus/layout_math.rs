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

spec fn db_base_offset() -> nat {
    65_536
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

spec fn page_offset(base: nat, page_no: nat) -> nat {
    base + page_no * sqlite_page_size()
}

spec fn page_offset_fits(base: nat, page_no: nat) -> bool {
    page_offset(base, page_no) <= u64_max()
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

proof fn first_page_starts_at_base(base: nat)
    ensures
        page_offset(base, 0) == base,
{
    assert(0 * sqlite_page_size() == 0);
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

proof fn accepted_import_at_layout_base_stays_in_u64(offset: nat, len: nat, total_size: nat)
    by (nonlinear_arith)
    requires
        import_chunk_accepts_at_base(db_base_offset(), offset, offset, len, total_size),
    ensures
        db_base_offset() + offset + len <= u64_max(),
{
}

proof fn page_offsets_are_page_size_apart(base: nat, page_no: nat)
    by (nonlinear_arith)
    ensures
        page_offset(base, page_no + 1) == page_offset(base, page_no) + sqlite_page_size(),
{
}

proof fn fitting_page_offset_implies_base_fits(base: nat, page_no: nat)
    by (nonlinear_arith)
    requires
        page_offset_fits(base, page_no),
    ensures
        base <= u64_max(),
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
