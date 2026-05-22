use ic_sqlite_vfs::stable::memory::{self, StableMemoryError};
use ic_sqlite_vfs::stable::memory_manager::{MemoryId, MemoryManager};
use ic_sqlite_vfs::stable::raw_memory::DefaultMemoryImpl;
use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};

const STABLE_PAGE_SIZE: u64 = ic_sqlite_vfs::config::STABLE_PAGE_SIZE;

#[test]
fn read_outside_capacity_does_not_grow_memory() {
    memory::reset_for_tests();
    let db_memory = MemoryManager::init(DefaultMemoryImpl::default()).get(MemoryId::new(42));
    memory::init(db_memory).expect("memory initializes");

    let mut byte = [0_u8; 1];
    let error = memory::read(0, &mut byte).expect_err("read outside capacity fails");

    assert!(matches!(
        error,
        StableMemoryError::ReadOutOfBounds {
            offset: 0,
            len: 1,
            size_bytes: 0
        }
    ));
    assert_eq!(memory::size_pages(), 0);
}

#[test]
fn read_inside_capacity_succeeds_without_extra_growth() {
    memory::reset_for_tests();
    let db_memory = MemoryManager::init(DefaultMemoryImpl::default()).get(MemoryId::new(42));
    memory::init(db_memory).expect("memory initializes");
    memory::write(0, &[9]).expect("write grows memory");
    let pages = memory::size_pages();
    let mut byte = [0_u8; 1];

    memory::read(0, &mut byte).expect("read inside capacity succeeds");

    assert_eq!(byte, [9]);
    assert_eq!(memory::size_pages(), pages);
}

#[test]
fn pbt_write_grows_capacity_to_cover_checked_end() {
    let mut runner = TestRunner::new(Config {
        cases: 256,
        ..Config::default()
    });

    runner
        .run(
            &(
                stable_memory_offset_strategy(),
                stable_memory_bytes_strategy(),
            ),
            |(offset, bytes)| {
                memory::reset_for_tests();
                let db_memory =
                    MemoryManager::init(DefaultMemoryImpl::default()).get(MemoryId::new(42));
                memory::init(db_memory).expect("memory initializes");

                memory::write(offset, &bytes).expect("write succeeds");
                let end = offset + u64::try_from(bytes.len()).unwrap();
                if bytes.is_empty() {
                    prop_assert_eq!(memory::size_pages(), 0);
                } else {
                    let capacity = memory::size_pages() * STABLE_PAGE_SIZE;
                    prop_assert!(capacity >= end);
                    let mut out = vec![0_u8; bytes.len()];
                    memory::read(offset, &mut out).expect("written range is readable");
                    prop_assert_eq!(out, bytes);
                }
                Ok(())
            },
        )
        .unwrap();
}

fn stable_memory_offset_strategy() -> impl Strategy<Value = u64> {
    prop_oneof![
        0_u64..(STABLE_PAGE_SIZE * 4),
        prop::sample::select(vec![
            0,
            1,
            STABLE_PAGE_SIZE - 1,
            STABLE_PAGE_SIZE,
            STABLE_PAGE_SIZE + 1,
            STABLE_PAGE_SIZE * 2 - 1,
            STABLE_PAGE_SIZE * 2,
            STABLE_PAGE_SIZE * 4 - 1,
        ]),
    ]
}

fn stable_memory_bytes_strategy() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        proptest::collection::vec(any::<u8>(), 0..4096),
        prop::sample::select(vec![0, 1, 4095, 4096])
            .prop_flat_map(|len| { proptest::collection::vec(any::<u8>(), len) }),
    ]
}

#[test]
#[should_panic(expected = "context id overflow")]
fn init_context_rejects_context_id_overflow() {
    memory::reset_for_tests();
    memory::set_next_context_id_for_tests(u64::MAX);
    let db_memory = MemoryManager::init(DefaultMemoryImpl::default()).get(MemoryId::new(42));

    let _context = memory::init_context(db_memory);
}
