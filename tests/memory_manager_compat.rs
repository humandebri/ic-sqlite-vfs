use ic_sqlite_vfs::config::STABLE_PAGE_SIZE;
use ic_sqlite_vfs::stable::memory_manager::{MemoryId, MemoryManager};
use ic_sqlite_vfs::stable::raw_memory::{DefaultMemoryImpl, Memory};
use upstream_ic_stable_structures::memory_manager::{
    MemoryId as UpstreamMemoryId, MemoryManager as UpstreamMemoryManager,
};
use upstream_ic_stable_structures::Memory as UpstreamMemory;

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
    Memory::read(&backing, 0, &mut magic);
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

#[test]
fn memory_manager_matches_upstream_layout_for_valid_operations() {
    assert_matches_upstream_layout(1);
    assert_matches_upstream_layout(128);
}

fn assert_matches_upstream_layout(bucket_size_in_pages: u16) {
    let local_backing = DefaultMemoryImpl::default();
    let upstream_backing = upstream_ic_stable_structures::VectorMemory::default();
    let local_manager =
        MemoryManager::init_with_bucket_size(local_backing.clone(), bucket_size_in_pages);
    let upstream_manager = UpstreamMemoryManager::init_with_bucket_size(
        upstream_backing.clone(),
        bucket_size_in_pages,
    );
    let local_memories: Vec<_> = (0_u8..5)
        .map(|id| local_manager.get(MemoryId::new(id)))
        .collect();
    let upstream_memories: Vec<_> = (0_u8..5)
        .map(|id| upstream_manager.get(UpstreamMemoryId::new(id)))
        .collect();
    let mut sizes = [0_u64; 5];

    assert_eq!(
        local_backing.borrow().as_slice(),
        upstream_backing.borrow().as_slice()
    );

    for step in 0..384_u64 {
        let id = usize::try_from(step % sizes.len() as u64).unwrap();
        match step % 4 {
            0 => {
                let pages = (step.wrapping_mul(7) % 5) + 1;
                let local_old = Memory::grow(&local_memories[id], pages);
                let upstream_old = UpstreamMemory::grow(&upstream_memories[id], pages);
                assert_eq!(
                    local_old, upstream_old,
                    "grow old size mismatch at step {step}"
                );
                sizes[id] += pages;
            }
            1 | 2 if sizes[id] > 0 => {
                let capacity = sizes[id] * STABLE_PAGE_SIZE;
                let len = ((step.wrapping_mul(13) % 4096) + 1).min(capacity) as usize;
                let offset = step.wrapping_mul(7_919) % (capacity - len as u64 + 1);
                let bytes = deterministic_bytes(step, len);

                Memory::write(&local_memories[id], offset, &bytes);
                UpstreamMemory::write(&upstream_memories[id], offset, &bytes);
            }
            _ if sizes[id] > 0 => {
                let capacity = sizes[id] * STABLE_PAGE_SIZE;
                let len = ((step.wrapping_mul(17) % 2048) + 1).min(capacity) as usize;
                let offset = step.wrapping_mul(4_099) % (capacity - len as u64 + 1);
                let mut local = vec![0_u8; len];
                let mut upstream = vec![0_u8; len];

                Memory::read(&local_memories[id], offset, &mut local);
                UpstreamMemory::read(&upstream_memories[id], offset, &mut upstream);
                assert_eq!(local, upstream, "read mismatch at step {step}");
            }
            _ => {}
        }

        for memory_id in 0..sizes.len() {
            assert_eq!(
                Memory::size(&local_memories[memory_id]),
                UpstreamMemory::size(&upstream_memories[memory_id]),
                "memory size mismatch at step {step}"
            );
        }
        assert_eq!(
            local_backing.borrow().as_slice(),
            upstream_backing.borrow().as_slice(),
            "stable layout diverged at step {step}"
        );
    }

    assert_can_reload_with_either_manager(local_backing.clone(), upstream_backing.clone(), &sizes);
}

fn assert_can_reload_with_either_manager(
    local_backing: DefaultMemoryImpl,
    upstream_backing: upstream_ic_stable_structures::VectorMemory,
    sizes: &[u64; 5],
) {
    let local_from_upstream = MemoryManager::init(upstream_backing.clone());
    let upstream_from_local = UpstreamMemoryManager::init(local_backing.clone());

    for id in 0_u8..5 {
        let local = local_from_upstream.get(MemoryId::new(id));
        let upstream = upstream_from_local.get(UpstreamMemoryId::new(id));
        assert_eq!(Memory::size(&local), sizes[id as usize]);
        assert_eq!(UpstreamMemory::size(&upstream), sizes[id as usize]);
    }
}

fn deterministic_bytes(seed: u64, len: usize) -> Vec<u8> {
    (0..len)
        .map(|index| seed.wrapping_add(index as u64).wrapping_mul(31) as u8)
        .collect()
}
