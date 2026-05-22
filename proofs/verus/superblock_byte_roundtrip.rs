//! Verus model for byte-level Superblock little-endian round trips.
//!
//! This file complements the field-level Superblock proof by modeling the
//! concrete u32/u64 little-endian byte encodings used by production Rust.

use vstd::prelude::*;
use vstd::arithmetic::div_mod::{
    lemma_div_is_ordered, lemma_fundamental_div_mod, lemma_fundamental_div_mod_converse,
    lemma_mod_bound,
};

verus! {

spec fn u32_max() -> nat {
    4_294_967_295
}

spec fn u64_max() -> nat {
    18_446_744_073_709_551_615
}

spec fn two32() -> nat {
    4_294_967_296
}

spec fn encoded_len() -> nat {
    152
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

spec fn encode_u32(value: nat) -> Seq<nat> {
    seq![
        value % 256,
        (value / 256) % 256,
        (value / 65_536) % 256,
        (value / 16_777_216) % 256
    ]
}

spec fn decode_u32(bytes: Seq<nat>) -> nat
    recommends
        bytes.len() == 4,
{
    bytes[0] + bytes[1] * 256 + bytes[2] * 65_536 + bytes[3] * 16_777_216
}

spec fn low32(value: nat) -> nat {
    value % two32()
}

spec fn high32(value: nat) -> nat {
    value / two32()
}

spec fn encode_u64(value: nat) -> Seq<nat> {
    encode_u64_from_halves(low32(value), high32(value))
}

spec fn encode_u64_from_halves(low: nat, high: nat) -> Seq<nat> {
    encode_u32(low) + encode_u32(high)
}

spec fn decode_u64(bytes: Seq<nat>) -> nat
    recommends
        bytes.len() == 8,
{
    decode_u32(bytes.subrange(0, 4)) + decode_u32(bytes.subrange(4, 8)) * two32()
}

spec fn zero_meta_checksum(fields: Seq<nat>) -> Seq<nat>
    recommends
        fields.len() == 20,
{
    fields.update(19, 0)
}

proof fn encode_u32_has_four_bytes(value: nat)
    ensures
        encode_u32(value).len() == 4,
{
    broadcast use vstd::seq::group_seq_axioms;
}

proof fn encode_u64_has_eight_bytes(value: nat)
    ensures
        encode_u64(value).len() == 8,
{
    broadcast use vstd::seq::group_seq_axioms;
    encode_u32_has_four_bytes(value % two32());
    encode_u32_has_four_bytes(value / two32());
}

proof fn u32_little_endian_round_trip(value: nat)
    by (nonlinear_arith)
    requires
        value <= u32_max(),
    ensures
        decode_u32(encode_u32(value)) == value,
{
    broadcast use vstd::seq::group_seq_axioms;
}

proof fn u64_little_endian_halves_round_trip(low: nat, high: nat)
    requires
        low <= u32_max(),
        high <= u32_max(),
    ensures
        decode_u64(encode_u64_from_halves(low, high)) == low + high * two32(),
{
    broadcast use vstd::seq::group_seq_axioms;
    u32_little_endian_round_trip(low);
    u32_little_endian_round_trip(high);
    assert(encode_u64_from_halves(low, high).subrange(0, 4) == encode_u32(low));
    assert(encode_u64_from_halves(low, high).subrange(4, 8) == encode_u32(high));
}

proof fn u64_low_high_parts_are_bounded(value: nat)
    requires
        value <= u64_max(),
    ensures
        low32(value) <= u32_max(),
        high32(value) <= u32_max(),
        value == low32(value) + high32(value) * two32(),
{
    lemma_mod_bound(value as int, two32() as int);
    lemma_fundamental_div_mod(value as int, two32() as int);
    assert(low32(value) < two32());
    assert(low32(value) <= u32_max()) by (nonlinear_arith);

    assert(0 <= u32_max() < two32()) by (nonlinear_arith);
    assert(u64_max() == u32_max() * two32() + u32_max()) by (nonlinear_arith);
    lemma_fundamental_div_mod_converse(
        u64_max() as int,
        two32() as int,
        u32_max() as int,
        u32_max() as int,
    );
    lemma_div_is_ordered(value as int, u64_max() as int, two32() as int);
    assert(high32(value) <= u32_max());
    assert(value == high32(value) * two32() + low32(value));
    assert(value == low32(value) + high32(value) * two32()) by (nonlinear_arith);
}

proof fn u64_little_endian_round_trip(value: nat)
    requires
        value <= u64_max(),
    ensures
        decode_u64(encode_u64(value)) == value,
{
    u64_low_high_parts_are_bounded(value);
    u64_little_endian_halves_round_trip(low32(value), high32(value));
}

proof fn field_ranges_do_not_overlap(left: nat, right: nat)
    by (nonlinear_arith)
    requires
        left < right < 20,
    ensures
        field_end(left) <= field_start(right),
        field_end(right) <= encoded_len(),
{
}

proof fn zero_meta_checksum_changes_only_checksum_field(fields: Seq<nat>, index: nat)
    requires
        fields.len() == 20,
        index < 20,
        index != 19,
    ensures
        zero_meta_checksum(fields)[index as int] == fields[index as int],
        zero_meta_checksum(fields)[19] == 0,
{
}

}
