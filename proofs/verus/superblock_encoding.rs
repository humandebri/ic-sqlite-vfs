//! Verus model for fixed-field superblock encoding.
//!
//! The production code uses fixed-width little-endian bytes. This abstract
//! model proves the field layout is lossless and that metadata checksums ignore
//! the stored checksum field by zeroing it before hashing.

use vstd::prelude::*;

verus! {

struct Superblock {
    magic: u64,
    version: u64,
    sqlite_page_size: u64,
    db_size: u64,
    schema_version: u64,
    last_tx_id: u64,
    flags: u64,
    checksum: u64,
    import_expected_checksum: u64,
    import_written_until: u64,
    import_total_size: u64,
    import_base_offset: u64,
    checksum_refresh_offset: u64,
    checksum_refresh_hash: u64,
    checksum_refresh_tx_id: u64,
    db_base_offset: u64,
    page_table_offset: u64,
    page_count: u64,
    layout_version: u64,
    meta_checksum: u64,
}

struct EncodedSuperblock {
    fields: Seq<u64>,
}

spec fn encoded_len() -> nat {
    20
}

spec fn encode(block: Superblock) -> EncodedSuperblock {
    EncodedSuperblock {
        fields: seq![
            block.magic,
            block.version,
            block.sqlite_page_size,
            block.db_size,
            block.schema_version,
            block.last_tx_id,
            block.flags,
            block.checksum,
            block.import_expected_checksum,
            block.import_written_until,
            block.import_total_size,
            block.import_base_offset,
            block.checksum_refresh_offset,
            block.checksum_refresh_hash,
            block.checksum_refresh_tx_id,
            block.db_base_offset,
            block.page_table_offset,
            block.page_count,
            block.layout_version,
            block.meta_checksum
        ],
    }
}

spec fn decode(encoded: EncodedSuperblock) -> Superblock
    recommends
        encoded.fields.len() == encoded_len(),
{
    Superblock {
        magic: encoded.fields[0],
        version: encoded.fields[1],
        sqlite_page_size: encoded.fields[2],
        db_size: encoded.fields[3],
        schema_version: encoded.fields[4],
        last_tx_id: encoded.fields[5],
        flags: encoded.fields[6],
        checksum: encoded.fields[7],
        import_expected_checksum: encoded.fields[8],
        import_written_until: encoded.fields[9],
        import_total_size: encoded.fields[10],
        import_base_offset: encoded.fields[11],
        checksum_refresh_offset: encoded.fields[12],
        checksum_refresh_hash: encoded.fields[13],
        checksum_refresh_tx_id: encoded.fields[14],
        db_base_offset: encoded.fields[15],
        page_table_offset: encoded.fields[16],
        page_count: encoded.fields[17],
        layout_version: encoded.fields[18],
        meta_checksum: encoded.fields[19],
    }
}

spec fn zero_meta_checksum(block: Superblock) -> Superblock {
    Superblock { meta_checksum: 0, ..block }
}

spec fn meta_checksum_input(block: Superblock) -> EncodedSuperblock {
    encode(zero_meta_checksum(block))
}

proof fn encode_has_fixed_field_count(block: Superblock)
    ensures
        encode(block).fields.len() == encoded_len(),
{
    broadcast use vstd::seq::group_seq_axioms;
}

proof fn decode_encode_round_trip(block: Superblock)
    ensures
        decode(encode(block)) == block,
{
    broadcast use vstd::seq::group_seq_axioms;
    encode_has_fixed_field_count(block);
}

proof fn checksum_input_zeroes_only_meta_checksum(block: Superblock)
    ensures
        decode(meta_checksum_input(block)).meta_checksum == 0,
        decode(meta_checksum_input(block)).db_size == block.db_size,
        decode(meta_checksum_input(block)).last_tx_id == block.last_tx_id,
        decode(meta_checksum_input(block)).checksum == block.checksum,
{
    broadcast use vstd::seq::group_seq_axioms;
    encode_has_fixed_field_count(zero_meta_checksum(block));
}

}
