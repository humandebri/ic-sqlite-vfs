//! Verus model for superblock byte-level field offsets.
//!
//! This models the concrete byte offsets used by production encode/decode and
//! proves that fixed-width header fields are non-overlapping inside the 160-byte
//! prefix before variable zero-extent entries.

use vstd::prelude::*;

verus! {

spec fn encoded_len() -> nat {
    160
}

spec fn field_start(index: nat) -> nat {
    if index == 0 {
        0
    } else if index <= 2 {
        8 + ((index - 1) as nat) * 4
    } else {
        16 + ((index - 3) as nat) * 8
    }
}

spec fn field_width(index: nat) -> nat {
    if index == 0 {
        8
    } else if index <= 2 {
        4
    } else {
        8
    }
}

spec fn field_end(index: nat) -> nat {
    field_start(index) + field_width(index)
}

spec fn le_byte(value: nat, index: nat) -> nat {
    (value / pow256(index)) % 256
}

spec fn pow256(index: nat) -> nat
    decreases index,
{
    if index == 0 {
        1
    } else {
        256 * pow256((index - 1) as nat)
    }
}

proof fn field_zero_magic_range()
    ensures
        field_start(0) == 0,
        field_end(0) == 8,
{
}

proof fn version_and_page_size_offsets()
    ensures
        field_start(1) == 8,
        field_end(1) == 12,
        field_start(2) == 12,
        field_end(2) == 16,
{
}

proof fn u64_field_offsets_are_eight_byte_aligned(index: nat)
    by (nonlinear_arith)
    requires
        3 <= index < 21,
    ensures
        field_width(index) == 8,
        field_start(index) % 8 == 0,
        field_end(index) <= encoded_len(),
{
}

proof fn adjacent_fields_do_not_overlap(index: nat)
    by (nonlinear_arith)
    requires
        index < 20,
    ensures
        field_end(index) <= field_start(index + 1),
{
}

proof fn last_field_ends_at_encoded_len()
    ensures
        field_start(19) == 144,
        field_end(19) == 152,
        field_start(20) == 152,
        field_end(20) == encoded_len(),
{
}

proof fn little_endian_byte_is_byte_sized(value: nat, index: nat)
    by (nonlinear_arith)
    ensures
        le_byte(value, index) < 256,
{
}

}
