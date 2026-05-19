use ic_sqlite_vfs::config::STABLE_PAGE_SIZE;
use ic_sqlite_vfs::stable::memory_manager::{MemoryId, MemoryManager};
use ic_sqlite_vfs::stable::raw_memory::{DefaultMemoryImpl, Memory};

#[test]
fn memory_manager_reloads_interleaved_bucket_layout() {
    let backing = DefaultMemoryImpl::default();
    let manager = MemoryManager::init_with_bucket_size(backing.clone(), 1);
    let first = manager.get(MemoryId::new(1));
    let second = manager.get(MemoryId::new(2));

    assert_eq!(first.grow(2), 0);
    assert_eq!(second.grow(1), 0);
    first.write(STABLE_PAGE_SIZE - 1, &[1, 2, 3]);
    second.write(0, &[4, 5, 6]);

    let mut magic = [0_u8; 3];
    backing.read(0, &mut magic);
    assert_eq!(&magic, b"MGR");

    let reloaded = MemoryManager::init(backing);
    let first = reloaded.get(MemoryId::new(1));
    let second = reloaded.get(MemoryId::new(2));
    let mut first_bytes = [0_u8; 3];
    let mut second_bytes = [0_u8; 3];

    first.read(STABLE_PAGE_SIZE - 1, &mut first_bytes);
    second.read(0, &mut second_bytes);

    assert_eq!(first_bytes, [1, 2, 3]);
    assert_eq!(second_bytes, [4, 5, 6]);
}
