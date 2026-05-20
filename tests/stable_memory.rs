use ic_sqlite_vfs::stable::memory::{self, StableMemoryError};
use ic_sqlite_vfs::stable::memory_manager::{MemoryId, MemoryManager};
use ic_sqlite_vfs::stable::raw_memory::DefaultMemoryImpl;

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
#[should_panic(expected = "context id overflow")]
fn init_context_rejects_context_id_overflow() {
    memory::reset_for_tests();
    memory::set_next_context_id_for_tests(u64::MAX);
    let db_memory = MemoryManager::init(DefaultMemoryImpl::default()).get(MemoryId::new(42));

    let _context = memory::init_context(db_memory);
}
