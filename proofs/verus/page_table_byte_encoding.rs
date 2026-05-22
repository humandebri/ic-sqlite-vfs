//! Verus model for page-table little-endian byte encoding.
//!
//! This proof mirrors the fixed u64 table format used for root and segment
//! tables. It stays proof-only and does not verify production Rust directly.

use vstd::arithmetic::div_mod::{
    lemma_div_is_ordered, lemma_fundamental_div_mod, lemma_fundamental_div_mod_converse,
    lemma_mod_bound,
};
use vstd::prelude::*;

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

spec fn segment_page_count() -> nat {
    256
}

spec fn page_table_entry_len() -> nat {
    8
}

spec fn root_table_bytes(entry_count: nat) -> nat {
    entry_count * page_table_entry_len()
}

spec fn segment_table_bytes() -> nat {
    segment_page_count() * page_table_entry_len()
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
    encode_u32(low32(value)) + encode_u32(high32(value))
}

spec fn decode_u64(bytes: Seq<nat>) -> nat
    recommends
        bytes.len() == 8,
{
    decode_u32(bytes.subrange(0, 4)) + decode_u32(bytes.subrange(4, 8)) * two32()
}

spec fn encode_table(entries: Seq<nat>) -> Seq<nat>
    decreases entries.len(),
{
    if entries.len() == 0 {
        seq![]
    } else {
        encode_u64(entries[0]) + encode_table(entries.subrange(1, entries.len() as int))
    }
}

spec fn decode_table(bytes: Seq<nat>, entries_len: nat) -> Seq<nat>
    recommends
        bytes.len() == entries_len * page_table_entry_len(),
    decreases entries_len,
{
    if entries_len == 0 {
        seq![]
    } else {
        seq![decode_u64(bytes.subrange(0, 8))]
            + decode_table(bytes.subrange(8, bytes.len() as int), (entries_len - 1) as nat)
    }
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
    broadcast use vstd::seq::group_seq_axioms;
    u64_low_high_parts_are_bounded(value);
    u32_little_endian_round_trip(low32(value));
    u32_little_endian_round_trip(high32(value));
    assert(encode_u64(value).subrange(0, 4) == encode_u32(low32(value)));
    assert(encode_u64(value).subrange(4, 8) == encode_u32(high32(value)));
}

proof fn encode_table_len(entries: Seq<nat>)
    ensures
        encode_table(entries).len() == entries.len() * page_table_entry_len(),
    decreases entries.len(),
{
    broadcast use vstd::seq::group_seq_axioms;
    if entries.len() > 0 {
        encode_table_len(entries.subrange(1, entries.len() as int));
    }
}

proof fn table_round_trip(entries: Seq<nat>)
    requires
        forall|index: int| 0 <= index < entries.len() ==> entries[index] <= u64_max(),
    ensures
        decode_table(encode_table(entries), entries.len()) == entries,
    decreases entries.len(),
{
    broadcast use vstd::seq::group_seq_axioms;
    if entries.len() > 0 {
        let tail = entries.subrange(1, entries.len() as int);
        assert(tail.len() == entries.len() - 1);
        assert forall|index: int| 0 <= index < tail.len() implies tail[index] <= u64_max() by {
            assert(tail[index] == entries[index + 1]);
        }
        u64_little_endian_round_trip(entries[0]);
        encode_table_len(tail);
        table_round_trip(tail);
        assert(encode_table(entries).subrange(0, 8) == encode_u64(entries[0]));
        assert(encode_table(entries).subrange(8, encode_table(entries).len() as int)
            == encode_table(tail));
    }
}

proof fn root_table_byte_count_matches_encoded_len(entries: Seq<nat>)
    ensures
        encode_table(entries).len() == root_table_bytes(entries.len()),
{
    encode_table_len(entries);
}

proof fn segment_table_byte_count()
    ensures
        root_table_bytes(segment_page_count()) == segment_table_bytes(),
{
}

}
