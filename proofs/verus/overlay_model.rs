//! Verus model for overlay page slicing.
//!
//! This proof covers arithmetic used by partial page writes and truncation. It
//! does not model heap vectors or base-memory I/O.

use vstd::prelude::*;

verus! {

spec fn page_size() -> nat {
    16_384
}

spec fn page_no(offset: nat) -> nat {
    offset / page_size()
}

spec fn page_offset(offset: nat) -> nat {
    offset % page_size()
}

spec fn copied_len(offset: nat, remaining: nat) -> nat {
    let available = (page_size() - page_offset(offset)) as nat;
    if available <= remaining {
        available
    } else {
        remaining
    }
}

spec fn tail_zeroed_after_truncate(size: nat, index: nat) -> bool {
    size == 0 || page_offset(size) == 0 || index >= page_offset(size)
}

spec fn later_write_byte(
    first_start: nat,
    first_len: nat,
    second_start: nat,
    second_len: nat,
    index: nat,
) -> int {
    if second_start <= index && index < second_start + second_len {
        2
    } else if first_start <= index && index < first_start + first_len {
        1
    } else {
        0
    }
}

proof fn page_offset_is_bounded(offset: nat)
    by (nonlinear_arith)
    ensures
        page_offset(offset) < page_size(),
{
}

proof fn copied_len_stays_inside_page(offset: nat, remaining: nat)
    by (nonlinear_arith)
    ensures
        copied_len(offset, remaining) <= remaining,
        page_offset(offset) + copied_len(offset, remaining) <= page_size(),
{
    page_offset_is_bounded(offset);
}

proof fn full_page_offset_is_zero(page: nat)
    by (nonlinear_arith)
    ensures
        page_offset(page * page_size()) == 0,
        page_no(page * page_size()) == page,
{
}

proof fn truncate_tail_marks_later_offsets_zeroed(size: nat, index: nat)
    requires
        page_offset(size) <= index < page_size(),
        page_offset(size) != 0,
    ensures
        tail_zeroed_after_truncate(size, index),
{
}

proof fn later_overlapping_write_wins(
    first_start: nat,
    first_len: nat,
    second_start: nat,
    second_len: nat,
    index: nat,
)
    requires
        first_start <= index < first_start + first_len,
        second_start <= index < second_start + second_len,
    ensures
        later_write_byte(first_start, first_len, second_start, second_len, index) == 2,
{
}

}
