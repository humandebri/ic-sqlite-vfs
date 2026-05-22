//! Verus model for chunked FNV-1a checksum folding.
//!
//! This proof stays outside production Rust. It verifies that folding a later
//! checksum chunk from the stored hash is equivalent to hashing the concatenated
//! byte stream in one pass.

use vstd::prelude::*;

verus! {

spec fn fnv_offset_basis() -> u64 {
    0xcbf2_9ce4_8422_2325u64
}

spec fn fnv_prime() -> u64 {
    0x0000_0100_0000_01b3u64
}

spec fn fnv_step(hash: u64, byte: u64) -> u64 {
    (hash ^ byte).wrapping_mul(fnv_prime())
}

spec fn fold_fnv1a64_from(hash: u64, bytes: Seq<u64>) -> u64
    decreases bytes.len(),
{
    if bytes.len() == 0 {
        hash
    } else {
        fold_fnv1a64_from(fnv_step(hash, bytes[0]), bytes.subrange(1, bytes.len() as int))
    }
}

spec fn fnv1a64(bytes: Seq<u64>) -> u64 {
    fold_fnv1a64_from(fnv_offset_basis(), bytes)
}

proof fn fold_fnv1a64_append(hash: u64, first: Seq<u64>, second: Seq<u64>)
    ensures
        fold_fnv1a64_from(hash, first + second)
            == fold_fnv1a64_from(fold_fnv1a64_from(hash, first), second),
    decreases first.len(),
{
    broadcast use vstd::seq::group_seq_axioms;

    if first.len() == 0 {
        assert(first + second =~= second);
    } else {
        let tail = first.subrange(1, first.len() as int);
        assert((first + second)[0] == first[0]);
        assert((first + second).subrange(1, (first + second).len() as int) =~= tail + second);
        fold_fnv1a64_append(fnv_step(hash, first[0]), tail, second);
    }
}

proof fn fnv1a64_matches_chunked_refresh(first: Seq<u64>, second: Seq<u64>)
    ensures
        fnv1a64(first + second) == fold_fnv1a64_from(fnv1a64(first), second),
{
    fold_fnv1a64_append(fnv_offset_basis(), first, second);
}

spec fn flatten_chunks(chunks: Seq<Seq<u64>>) -> Seq<u64>
    decreases chunks.len(),
{
    if chunks.len() == 0 {
        Seq::<u64>::empty()
    } else {
        chunks[0] + flatten_chunks(chunks.subrange(1, chunks.len() as int))
    }
}

spec fn fold_chunks_from(hash: u64, chunks: Seq<Seq<u64>>) -> u64
    decreases chunks.len(),
{
    if chunks.len() == 0 {
        hash
    } else {
        fold_chunks_from(
            fold_fnv1a64_from(hash, chunks[0]),
            chunks.subrange(1, chunks.len() as int),
        )
    }
}

proof fn fold_chunks_matches_flatten_from(hash: u64, chunks: Seq<Seq<u64>>)
    ensures
        fold_chunks_from(hash, chunks) == fold_fnv1a64_from(hash, flatten_chunks(chunks)),
    decreases chunks.len(),
{
    broadcast use vstd::seq::group_seq_axioms;

    if chunks.len() == 0 {
    } else {
        let tail = chunks.subrange(1, chunks.len() as int);
        fold_chunks_matches_flatten_from(fold_fnv1a64_from(hash, chunks[0]), tail);
        fold_fnv1a64_append(hash, chunks[0], flatten_chunks(tail));
    }
}

proof fn fnv1a64_matches_any_chunk_partition(chunks: Seq<Seq<u64>>)
    ensures
        fnv1a64(flatten_chunks(chunks)) == fold_chunks_from(fnv_offset_basis(), chunks),
{
    fold_chunks_matches_flatten_from(fnv_offset_basis(), chunks);
}

proof fn empty_refresh_starts_at_offset_basis()
    ensures
        fnv1a64(Seq::<u64>::empty()) == fnv_offset_basis(),
{
}

}
