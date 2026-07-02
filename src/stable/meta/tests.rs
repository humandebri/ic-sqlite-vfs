use super::*;
use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};

fn sample_block() -> Superblock {
    let mut block = Superblock::fresh();
    block.db_size = 0x0102_0304_0506_0708;
    block.schema_version = 0x1112_1314_1516_1718;
    block.last_tx_id = 0x2122_2324_2526_2728;
    block.flags = FLAG_IMPORTING | FLAG_CHECKSUM_STALE | FLAG_CHECKSUM_REFRESHING;
    block.checksum = 0x3132_3334_3536_3738;
    block.import_expected_checksum = 0x4142_4344_4546_4748;
    block.import_written_until = 0x5152_5354_5556_5758;
    block.import_total_size = 0x6162_6364_6566_6768;
    block.import_base_offset = 0x7172_7374_7576_7778;
    block.checksum_refresh_offset = 0x8182_8384_8586_8788;
    block.checksum_refresh_hash = 0x9192_9394_9596_9798;
    block.checksum_refresh_tx_id = 0xa1a2_a3a4_a5a6_a7a8;
    block.db_base_offset = 0xb1b2_b3b4_b5b6_b7b8;
    block.page_table_offset = 0xc1c2_c3c4_c5c6_c7c8;
    block.page_count = 0xd1d2_d3d4_d5d6_d7d8;
    block.layout_version = CURRENT_LAYOUT_VERSION;
    block.zero_extents = vec![
        ZeroExtent {
            start_page: 2,
            end_page: 5,
        },
        ZeroExtent {
            start_page: 9,
            end_page: 11,
        },
    ];
    block.meta_checksum = block.compute_meta_checksum();
    block
}

#[test]
fn superblock_encode_decode_uses_fixed_little_endian_offsets() {
    let block = sample_block();
    let encoded = block.encode();

    assert_eq!(&encoded[0..8], b"ICSQLITE");
    assert_eq!(&encoded[8..12], &VERSION.to_le_bytes());
    assert_eq!(&encoded[12..16], &SQLITE_PAGE_SIZE.to_le_bytes());
    assert_eq!(&encoded[16..24], &block.db_size.to_le_bytes());
    assert_eq!(&encoded[80..88], &block.import_base_offset.to_le_bytes());
    assert_eq!(&encoded[120..128], &block.page_table_offset.to_le_bytes());
    assert_eq!(&encoded[144..152], &2_u64.to_le_bytes());
    assert_eq!(&encoded[152..160], &block.meta_checksum.to_le_bytes());
    assert_eq!(&encoded[160..168], &2_u64.to_le_bytes());
    assert_eq!(&encoded[168..176], &5_u64.to_le_bytes());
    assert_eq!(&encoded[176..184], &9_u64.to_le_bytes());
    assert_eq!(&encoded[184..192], &11_u64.to_le_bytes());
    assert_eq!(Superblock::decode(&encoded), block);
}

#[test]
fn superblock_meta_digest_zeroes_only_meta_field() {
    let block = sample_block();
    let mut checksum_input = block.encode();
    checksum_input[152..160].copy_from_slice(&0_u64.to_le_bytes());

    let mut changed_checksum = block.clone();
    changed_checksum.meta_checksum ^= u64::MAX;

    let mut changed_field = block.clone();
    changed_field.last_tx_id = changed_field.last_tx_id.wrapping_add(1);

    let mut changed_extent = block.clone();
    changed_extent.zero_extents[0].end_page += 1;
    changed_extent.meta_checksum = changed_extent.compute_meta_checksum();

    assert_eq!(
        block.compute_meta_checksum(),
        fnv1a64(&checksum_input[..block.encoded_len()])
    );
    assert_eq!(
        changed_checksum.compute_meta_checksum(),
        block.compute_meta_checksum()
    );
    assert_ne!(
        changed_field.compute_meta_checksum(),
        block.compute_meta_checksum()
    );
    assert_ne!(changed_extent.meta_checksum, block.compute_meta_checksum());
}

#[test]
fn pbt_superblock_encoding_matches_fixed_field_model() {
    let mut runner = TestRunner::new(Config {
        cases: 256,
        ..Config::default()
    });

    runner
        .run(&any::<[u64; 16]>(), |fields| {
            let block = block_from_fields(fields);
            let encoded = block.encode();

            prop_assert_eq!(encoded.len(), ENCODED_LEN);
            prop_assert_eq!(&encoded[0..8], b"ICSQLITE");
            prop_assert_eq!(&encoded[8..12], &VERSION.to_le_bytes());
            prop_assert_eq!(&encoded[12..16], &SQLITE_PAGE_SIZE.to_le_bytes());
            assert_u64_field_offsets(&encoded, &block)?;
            prop_assert_eq!(Superblock::decode(&encoded), block.clone());

            let mut changed_meta = block.clone();
            changed_meta.meta_checksum ^= u64::MAX;
            prop_assert_eq!(
                changed_meta.compute_meta_checksum(),
                block.compute_meta_checksum()
            );

            let mut checksum_input = encoded;
            checksum_input[152..160].copy_from_slice(&0_u64.to_le_bytes());
            prop_assert_eq!(
                block.compute_meta_checksum(),
                fnv1a64(&checksum_input[..block.encoded_len()])
            );
            Ok(())
        })
        .unwrap();
}

#[test]
#[serial_test::serial]
fn cache_insert_prunes_stale_generations() {
    crate::stable::memory::reset_for_tests();
    crate::stable::memory::init(crate::stable::memory::memory_for_tests()).unwrap();
    Superblock::load().unwrap();
    assert_eq!(superblock_cache_len(), 1);

    crate::stable::memory::reset_for_tests();
    assert_eq!(superblock_cache_len(), 1);
    crate::stable::memory::init(crate::stable::memory::memory_for_tests()).unwrap();
    Superblock::load().unwrap();

    assert_eq!(superblock_cache_len(), 1);
}

#[test]
#[serial_test::serial]
fn load_rejects_raw_zero_extent_count_above_limit() {
    crate::stable::memory::reset_for_tests();
    crate::stable::memory::init(crate::stable::memory::memory_for_tests()).unwrap();

    let block = Superblock::fresh();
    let mut encoded = block.encode();
    encoded[144..152].copy_from_slice(&((MAX_ZERO_EXTENTS as u64) + 1).to_le_bytes());
    crate::stable::memory::write(SUPERBLOCK_OFFSET, &encoded).unwrap();
    clear_superblock_cache();

    assert!(matches!(
        Superblock::load(),
        Err(StableMemoryError::MetaChecksumMismatch)
    ));
}

#[test]
#[serial_test::serial]
fn load_classifies_foreign_magic_before_v8_zero_extent_count() {
    crate::stable::memory::reset_for_tests();
    crate::stable::memory::init(crate::stable::memory::memory_for_tests()).unwrap();

    let block = Superblock::fresh();
    let mut encoded = block.encode();
    encoded[0..8].copy_from_slice(b"NOTSQLIT");
    encoded[144..152].copy_from_slice(&((MAX_ZERO_EXTENTS as u64) + 1).to_le_bytes());
    crate::stable::memory::write(SUPERBLOCK_OFFSET, &encoded).unwrap();
    clear_superblock_cache();

    assert!(matches!(
        Superblock::load(),
        Err(StableMemoryError::ForeignStableMemoryImage)
    ));
}

#[test]
#[serial_test::serial]
fn load_classifies_older_layout_before_v8_zero_extent_count() {
    crate::stable::memory::reset_for_tests();
    crate::stable::memory::init(crate::stable::memory::memory_for_tests()).unwrap();

    let mut block = Superblock::fresh();
    block.layout_version = 6;
    block.meta_checksum = block.compute_meta_checksum();
    let mut encoded = block.encode();
    encoded[144..152].copy_from_slice(&0xDEAD_BEEF_DEAD_BEEFu64.to_le_bytes());
    crate::stable::memory::write(SUPERBLOCK_OFFSET, &encoded).unwrap();
    clear_superblock_cache();

    assert!(matches!(
        Superblock::load(),
        Err(StableMemoryError::UnsupportedLayoutVersion(6))
    ));
}

#[test]
#[serial_test::serial]
fn load_rejects_older_superblock_version() {
    assert_rejects_superblock_version(7);
}

#[test]
#[serial_test::serial]
fn load_rejects_newer_superblock_version() {
    assert_rejects_superblock_version(9);
}

#[test]
#[serial_test::serial]
fn commit_db_image_rejects_over_limit_zero_extents_without_publish() {
    crate::stable::memory::reset_for_tests();
    crate::stable::memory::init(crate::stable::memory::memory_for_tests()).unwrap();

    let before = Superblock::load().unwrap();
    let mut next_start = 0_u64;
    let extents = (0..=MAX_ZERO_EXTENTS)
        .map(|_| {
            let extent = ZeroExtent {
                start_page: next_start,
                end_page: next_start + 1,
            };
            next_start += 2;
            extent
        })
        .collect::<Vec<_>>();

    let result = Superblock::commit_db_image(before.db_base_offset, before.db_size, extents);

    assert!(matches!(
        result,
        Err(StableMemoryError::ZeroExtentLimitExceeded {
            limit: MAX_ZERO_EXTENTS
        })
    ));
    clear_superblock_cache();
    assert_eq!(Superblock::load().unwrap(), before);
}

#[test]
#[serial_test::serial]
fn store_db_image_without_tx_rejects_unnormalized_zero_extents_without_publish() {
    crate::stable::memory::reset_for_tests();
    crate::stable::memory::init(crate::stable::memory::memory_for_tests()).unwrap();

    let before = Superblock::load().unwrap();
    let extents = vec![
        ZeroExtent {
            start_page: 2,
            end_page: 3,
        },
        ZeroExtent {
            start_page: 1,
            end_page: 2,
        },
    ];

    let result =
        Superblock::store_db_image_without_tx(before.db_base_offset, before.db_size, extents);

    assert!(matches!(
        result,
        Err(StableMemoryError::MetaChecksumMismatch)
    ));
    clear_superblock_cache();
    assert_eq!(Superblock::load().unwrap(), before);
}

fn assert_u64_field_offsets(
    encoded: &[u8; ENCODED_LEN],
    block: &Superblock,
) -> Result<(), TestCaseError> {
    let fields = [
        (16, block.db_size),
        (24, block.schema_version),
        (32, block.last_tx_id),
        (40, block.flags),
        (48, block.checksum),
        (56, block.import_expected_checksum),
        (64, block.import_written_until),
        (72, block.import_total_size),
        (80, block.import_base_offset),
        (88, block.checksum_refresh_offset),
        (96, block.checksum_refresh_hash),
        (104, block.checksum_refresh_tx_id),
        (112, block.db_base_offset),
        (120, block.page_table_offset),
        (128, block.page_count),
        (136, block.layout_version),
        (144, block.zero_extents.len() as u64),
        (152, block.meta_checksum),
    ];

    for (offset, expected) in fields {
        let actual = u64::from_le_bytes(eight(encoded, offset));
        prop_assert_eq!(actual, expected);
    }
    Ok(())
}

fn assert_rejects_superblock_version(version: u32) {
    crate::stable::memory::reset_for_tests();
    crate::stable::memory::init(crate::stable::memory::memory_for_tests()).unwrap();

    let mut block = Superblock::fresh();
    block.version = version;
    block.meta_checksum = block.compute_meta_checksum();
    let encoded = block.encode();
    crate::stable::memory::write(SUPERBLOCK_OFFSET, &encoded[..block.encoded_len()]).unwrap();
    clear_superblock_cache();

    assert!(matches!(
        Superblock::load(),
        Err(StableMemoryError::UnsupportedLayoutVersion(found)) if found == u64::from(version)
    ));
}

fn block_from_fields(fields: [u64; 16]) -> Superblock {
    let mut block = Superblock::fresh();
    block.db_size = fields[0];
    block.schema_version = fields[1];
    block.last_tx_id = fields[2];
    block.flags = fields[3];
    block.checksum = fields[4];
    block.import_expected_checksum = fields[5];
    block.import_written_until = fields[6];
    block.import_total_size = fields[7];
    block.import_base_offset = fields[8];
    block.checksum_refresh_offset = fields[9];
    block.checksum_refresh_hash = fields[10];
    block.checksum_refresh_tx_id = fields[11];
    block.db_base_offset = fields[12];
    block.page_table_offset = fields[13];
    block.page_count = fields[14];
    block.layout_version = fields[15];
    block.meta_checksum = block.compute_meta_checksum();
    block
}
