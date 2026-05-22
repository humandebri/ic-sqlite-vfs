use ic_sqlite_vfs::config::STABLE_PAGE_SIZE;
use ic_sqlite_vfs::stable::memory_manager::{
    MemoryId as LocalMemoryId, MemoryManager as LocalMemoryManager,
};
use ic_sqlite_vfs::stable::raw_memory::{DefaultMemoryImpl, Memory as LocalMemory};
use ic_stable_structures::memory_manager::{
    MemoryId as UpstreamMemoryId, MemoryManager as UpstreamMemoryManager,
};
use ic_stable_structures::{Memory as UpstreamMemory, VectorMemory};

const MEMORY_COUNT: usize = 5;

#[test]
fn upstream_and_fork_memory_manager_layouts_are_interchangeable() {
    for bucket_size in [1_u16, 2, 7, 128] {
        assert_compatible_bucket_layout(bucket_size);
    }
}

fn assert_compatible_bucket_layout(bucket_size_in_pages: u16) {
    let local_backing = DefaultMemoryImpl::default();
    let upstream_backing = VectorMemory::default();
    let local_manager =
        LocalMemoryManager::init_with_bucket_size(local_backing.clone(), bucket_size_in_pages);
    let upstream_manager = UpstreamMemoryManager::init_with_bucket_size(
        upstream_backing.clone(),
        bucket_size_in_pages,
    );
    let local_memories: Vec<_> = (0_u8..MEMORY_COUNT_U8)
        .map(|id| local_manager.get(LocalMemoryId::new(id)))
        .collect();
    let upstream_memories: Vec<_> = (0_u8..MEMORY_COUNT_U8)
        .map(|id| upstream_manager.get(UpstreamMemoryId::new(id)))
        .collect();
    let mut sizes = [0_u64; MEMORY_COUNT];

    assert_backing_equal(&local_backing, &upstream_backing, "initial layout");

    for step in 0_u64..192 {
        let id = memory_index(step);
        match step % 4 {
            0 => {
                let pages = projected_grow_pages(step, bucket_size_in_pages);
                let local_old = LocalMemory::grow(&local_memories[id], pages);
                let upstream_old = UpstreamMemory::grow(&upstream_memories[id], pages);
                assert_eq!(local_old, upstream_old, "grow old size mismatch at step {step}");
                sizes[id] = sizes[id].checked_add(pages).expect("test growth fits u64");
            }
            1 | 2 if sizes[id] > 0 => {
                let capacity = sizes[id] * STABLE_PAGE_SIZE;
                let len = projected_len(step, capacity, bucket_size_in_pages);
                let offset = projected_offset(step, capacity, len, bucket_size_in_pages);
                let bytes = deterministic_bytes(step, len);

                LocalMemory::write(&local_memories[id], offset, &bytes);
                UpstreamMemory::write(&upstream_memories[id], offset, &bytes);
            }
            _ if sizes[id] > 0 => {
                assert_read_equal(&local_memories[id], &upstream_memories[id], sizes[id], step);
            }
            _ => {}
        }

        for memory_id in 0..MEMORY_COUNT {
            assert_eq!(
                LocalMemory::size(&local_memories[memory_id]),
                UpstreamMemory::size(&upstream_memories[memory_id]),
                "memory size mismatch at step {step}"
            );
        }
        assert_backing_equal(&local_backing, &upstream_backing, "parallel operation");
    }

    assert_cross_reload_and_continue(local_backing, upstream_backing, sizes, bucket_size_in_pages);
}

const MEMORY_COUNT_U8: u8 = 5;

fn assert_cross_reload_and_continue(
    local_backing: DefaultMemoryImpl,
    upstream_backing: VectorMemory,
    mut sizes: [u64; MEMORY_COUNT],
    bucket_size_in_pages: u16,
) {
    let local_from_upstream = LocalMemoryManager::init(upstream_backing.clone());
    let upstream_from_local = UpstreamMemoryManager::init(local_backing.clone());
    let local_memories: Vec<_> = (0_u8..MEMORY_COUNT_U8)
        .map(|id| local_from_upstream.get(LocalMemoryId::new(id)))
        .collect();
    let upstream_memories: Vec<_> = (0_u8..MEMORY_COUNT_U8)
        .map(|id| upstream_from_local.get(UpstreamMemoryId::new(id)))
        .collect();

    for id in 0..MEMORY_COUNT {
        assert_eq!(LocalMemory::size(&local_memories[id]), sizes[id]);
        assert_eq!(UpstreamMemory::size(&upstream_memories[id]), sizes[id]);
        if sizes[id] > 0 {
            assert_read_equal(
                &local_memories[id],
                &upstream_memories[id],
                sizes[id],
                u64::try_from(id).unwrap(),
            );
        }
    }

    for step in 192_u64..224 {
        let id = memory_index(step);
        match step % 3 {
            0 => {
                let pages = projected_grow_pages(step, bucket_size_in_pages);
                let local_old = LocalMemory::grow(&local_memories[id], pages);
                let upstream_old = UpstreamMemory::grow(&upstream_memories[id], pages);
                assert_eq!(local_old, upstream_old, "cross grow mismatch at step {step}");
                sizes[id] = sizes[id].checked_add(pages).expect("test growth fits u64");
            }
            _ if sizes[id] > 0 => {
                let capacity = sizes[id] * STABLE_PAGE_SIZE;
                let len = projected_len(step, capacity, bucket_size_in_pages);
                let offset = projected_offset(step, capacity, len, bucket_size_in_pages);
                let bytes = deterministic_bytes(step, len);

                LocalMemory::write(&local_memories[id], offset, &bytes);
                UpstreamMemory::write(&upstream_memories[id], offset, &bytes);
            }
            _ => {}
        }
        assert_backing_equal(&local_backing, &upstream_backing, "cross operation");
    }

    let reloaded_local = LocalMemoryManager::init(upstream_backing.clone());
    let reloaded_upstream = UpstreamMemoryManager::init(local_backing.clone());
    for id in 0_u8..MEMORY_COUNT_U8 {
        let index = usize::from(id);
        let local = reloaded_local.get(LocalMemoryId::new(id));
        let upstream = reloaded_upstream.get(UpstreamMemoryId::new(id));
        assert_eq!(LocalMemory::size(&local), sizes[index]);
        assert_eq!(UpstreamMemory::size(&upstream), sizes[index]);
        if sizes[index] > 0 {
            assert_read_equal(&local, &upstream, sizes[index], u64::from(id));
        }
    }
}

fn assert_read_equal<L, U>(local: &L, upstream: &U, pages: u64, seed: u64)
where
    L: LocalMemory,
    U: UpstreamMemory,
{
    let capacity = pages * STABLE_PAGE_SIZE;
    let len = projected_len(seed.wrapping_add(11), capacity, 7);
    let offset = projected_offset(seed.wrapping_add(17), capacity, len, 7);
    let mut local_bytes = vec![0_u8; len];
    let mut upstream_bytes = vec![0_u8; len];

    LocalMemory::read(local, offset, &mut local_bytes);
    UpstreamMemory::read(upstream, offset, &mut upstream_bytes);
    assert_eq!(local_bytes, upstream_bytes, "read mismatch seed {seed}");
}

fn assert_backing_equal(local: &DefaultMemoryImpl, upstream: &VectorMemory, context: &str) {
    assert_eq!(
        local.borrow().as_slice(),
        upstream.borrow().as_slice(),
        "stable backing diverged during {context}"
    );
}

fn memory_index(seed: u64) -> usize {
    usize::try_from(seed % u64::try_from(MEMORY_COUNT).unwrap()).unwrap()
}

fn projected_grow_pages(seed: u64, bucket_size_in_pages: u16) -> u64 {
    let bucket = u64::from(bucket_size_in_pages);
    let candidates = [
        0,
        1,
        bucket.saturating_sub(1),
        bucket,
        bucket.saturating_add(1),
        seed % 5,
    ];
    candidates[usize::try_from(seed % u64::try_from(candidates.len()).unwrap()).unwrap()]
}

fn projected_len(seed: u64, capacity: u64, bucket_size_in_pages: u16) -> usize {
    let candidates = [
        1,
        2,
        STABLE_PAGE_SIZE.saturating_sub(1),
        STABLE_PAGE_SIZE,
        STABLE_PAGE_SIZE.saturating_add(1),
        u64::from(bucket_size_in_pages).saturating_add(1),
        (seed % 4096).saturating_add(1),
    ];
    let index = usize::try_from(seed % u64::try_from(candidates.len()).unwrap()).unwrap();
    usize::try_from(candidates[index].min(capacity)).unwrap()
}

fn projected_offset(seed: u64, capacity: u64, len: usize, bucket_size_in_pages: u16) -> u64 {
    let max = capacity - u64::try_from(len).unwrap();
    let bucket_bytes = u64::from(bucket_size_in_pages) * STABLE_PAGE_SIZE;
    let candidates = [
        0,
        1,
        STABLE_PAGE_SIZE.saturating_sub(1),
        STABLE_PAGE_SIZE,
        STABLE_PAGE_SIZE.saturating_add(1),
        bucket_bytes.saturating_sub(1),
        bucket_bytes,
        bucket_bytes.saturating_add(1),
        max.saturating_sub(1),
        max,
        seed % (max + 1),
    ];
    let index = usize::try_from(seed % u64::try_from(candidates.len()).unwrap()).unwrap();
    candidates[index].min(max)
}

fn deterministic_bytes(seed: u64, len: usize) -> Vec<u8> {
    (0..len)
        .map(|index| {
            let index = u64::try_from(index).unwrap();
            seed.wrapping_add(index).wrapping_mul(31).to_le_bytes()[0]
        })
        .collect()
}
