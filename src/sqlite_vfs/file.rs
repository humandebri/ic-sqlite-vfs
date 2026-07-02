//! `sqlite3_file` implementation for `/main.db` and heap temp files.
//!
//! The struct is placed into SQLite-owned memory during `xOpen`; `base` must
//! remain the first field so pointer casts from `sqlite3_file` are valid.

use crate::config::SQLITE_PAGE_SIZE;
use crate::sqlite_vfs::ffi;
use crate::sqlite_vfs::lock;
use crate::sqlite_vfs::stable_blob;
use crate::sqlite_vfs::temp::TempFile;
use crate::sqlite_vfs::vfs;
use crate::stable::memory::{self, ContextId};
use crate::stable::meta::Superblock;
use std::ffi::{c_char, c_int, c_void};
use std::ptr;

#[repr(C)]
pub struct IcStableFile {
    base: ffi::sqlite3_file,
    context: ContextId,
    kind: FileKind,
    read_only: bool,
    read_snapshot: Option<Superblock>,
    read_page_offsets: stable_blob::PageOffsetCache,
    powersafe_overwrite: bool,
}

#[derive(Debug)]
pub enum FileKind {
    Main,
    Temp(TempFile),
}

enum IoMetric {
    Read(usize),
    Write(usize),
    FileSize,
    Lock,
    Unlock,
    CheckReservedLock,
    FileControl,
    DeviceCharacteristics,
}

fn record_metric(metric: IoMetric) {
    #[cfg(any(test, debug_assertions, feature = "bench-profile"))]
    {
        match metric {
            IoMetric::Read(amount) => crate::read_metrics::record_x_read(amount),
            IoMetric::Write(amount) => crate::read_metrics::record_x_write(amount),
            IoMetric::FileSize => crate::read_metrics::record_x_file_size(),
            IoMetric::Lock => crate::read_metrics::record_x_lock(),
            IoMetric::Unlock => crate::read_metrics::record_x_unlock(),
            IoMetric::CheckReservedLock => crate::read_metrics::record_x_check_reserved_lock(),
            IoMetric::FileControl => crate::read_metrics::record_x_file_control(),
            IoMetric::DeviceCharacteristics => {
                crate::read_metrics::record_x_device_characteristics()
            }
        }
    }
    #[cfg(not(any(test, debug_assertions, feature = "bench-profile")))]
    {
        let _ = metric;
    }
}

impl IcStableFile {
    pub fn new(
        kind: FileKind,
        read_only: bool,
        context: ContextId,
        read_snapshot: Option<Superblock>,
    ) -> Self {
        Self {
            base: ffi::sqlite3_file {
                pMethods: ptr::null(),
            },
            context,
            kind,
            read_only,
            read_snapshot,
            read_page_offsets: stable_blob::PageOffsetCache::new(),
            powersafe_overwrite: true,
        }
    }
}

pub static IO_METHODS: ffi::sqlite3_io_methods = ffi::sqlite3_io_methods {
    iVersion: 1,
    xClose: Some(x_close),
    xRead: Some(x_read),
    xWrite: Some(x_write),
    xTruncate: Some(x_truncate),
    xSync: Some(x_sync),
    xFileSize: Some(x_file_size),
    xLock: Some(x_lock),
    xUnlock: Some(x_unlock),
    xCheckReservedLock: Some(x_check_reserved_lock),
    xFileControl: Some(x_file_control),
    xSectorSize: Some(x_sector_size),
    xDeviceCharacteristics: Some(x_device_characteristics),
    xShmMap: None,
    xShmLock: None,
    xShmBarrier: None,
    xShmUnmap: None,
    xFetch: None,
    xUnfetch: None,
};

// === FFI callback boundary ===

/// # Safety
///
/// `p_file` must point to at least `size_of::<IcStableFile>()` bytes allocated
/// by SQLite for the current `xOpen` call. SQLite owns that memory until
/// `xClose`, where the embedded `FileKind` is dropped exactly once.
pub unsafe fn install(
    p_file: *mut ffi::sqlite3_file,
    kind: FileKind,
    read_only: bool,
    context: ContextId,
) {
    let file = p_file.cast::<IcStableFile>();
    ptr::write(file, IcStableFile::new(kind, read_only, context, None));
    (*p_file).pMethods = ptr::addr_of!(IO_METHODS);
}

unsafe extern "C" fn x_close(file: *mut ffi::sqlite3_file) -> c_int {
    ptr::drop_in_place(file.cast::<IcStableFile>());
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_read(
    file: *mut ffi::sqlite3_file,
    buf: *mut c_void,
    amount: c_int,
    offset: ffi::sqlite3_int64,
) -> c_int {
    let Some(amount) = checked_amount(amount) else {
        return ffi::SQLITE_IOERR_READ;
    };
    let Some(offset) = checked_offset(offset) else {
        return ffi::SQLITE_IOERR_READ;
    };
    let dst = std::slice::from_raw_parts_mut(buf.cast::<u8>(), amount);
    let file = &mut *file.cast::<IcStableFile>();
    if let FileKind::Temp(temp) = &mut file.kind {
        return if temp.read(offset, dst) {
            ffi::SQLITE_OK
        } else {
            ffi::SQLITE_IOERR_SHORT_READ
        };
    }
    memory::with_context(file.context, || {
        record_metric(IoMetric::Read(amount));
        let result = if file.read_only {
            let block = match &file.read_snapshot {
                Some(block) => block,
                None => match Superblock::load() {
                    Ok(block) => file.read_snapshot.insert(block),
                    Err(error) => {
                        vfs::record_last_error(ffi::SQLITE_IOERR_READ, error.to_string());
                        return ffi::SQLITE_IOERR_READ;
                    }
                },
            };
            stable_blob::read_base_at_with_page_cache(
                block,
                offset,
                dst,
                &mut file.read_page_offsets,
            )
        } else {
            stable_blob::read_at(offset, dst)
        };
        let complete = match result {
            Ok(value) => value,
            Err(error) => {
                vfs::record_last_error(ffi::SQLITE_IOERR_READ, error.to_string());
                return ffi::SQLITE_IOERR_READ;
            }
        };
        if complete {
            ffi::SQLITE_OK
        } else {
            ffi::SQLITE_IOERR_SHORT_READ
        }
    })
}

unsafe extern "C" fn x_write(
    file: *mut ffi::sqlite3_file,
    buf: *const c_void,
    amount: c_int,
    offset: ffi::sqlite3_int64,
) -> c_int {
    let file = &mut *file.cast::<IcStableFile>();
    if file.read_only {
        vfs::record_last_error(ffi::SQLITE_READONLY, "write attempted on read-only file");
        return ffi::SQLITE_READONLY;
    }
    let Some(amount) = checked_amount(amount) else {
        return ffi::SQLITE_IOERR_WRITE;
    };
    let Some(offset) = checked_offset(offset) else {
        return ffi::SQLITE_IOERR_WRITE;
    };
    let bytes = std::slice::from_raw_parts(buf.cast::<u8>(), amount);
    if let FileKind::Temp(temp) = &mut file.kind {
        return if temp.write(offset, bytes) {
            ffi::SQLITE_OK
        } else {
            ffi::SQLITE_IOERR_WRITE
        };
    }
    memory::with_context(file.context, || {
        record_metric(IoMetric::Write(amount));
        match stable_blob::write_at(offset, bytes) {
            Ok(()) => ffi::SQLITE_OK,
            Err(error) => {
                vfs::record_last_error(ffi::SQLITE_IOERR_WRITE, error.to_string());
                ffi::SQLITE_IOERR_WRITE
            }
        }
    })
}

unsafe extern "C" fn x_truncate(file: *mut ffi::sqlite3_file, size: ffi::sqlite3_int64) -> c_int {
    let file = &mut *file.cast::<IcStableFile>();
    if file.read_only {
        vfs::record_last_error(ffi::SQLITE_READONLY, "truncate attempted on read-only file");
        return ffi::SQLITE_READONLY;
    }
    let Some(size) = checked_offset(size) else {
        return ffi::SQLITE_IOERR_TRUNCATE;
    };
    if let FileKind::Temp(temp) = &mut file.kind {
        return if temp.truncate(size) {
            ffi::SQLITE_OK
        } else {
            ffi::SQLITE_IOERR_TRUNCATE
        };
    }
    memory::with_context(file.context, || match stable_blob::truncate(size) {
        Ok(()) => ffi::SQLITE_OK,
        Err(error) => {
            vfs::record_last_error(ffi::SQLITE_IOERR_TRUNCATE, error.to_string());
            ffi::SQLITE_IOERR_TRUNCATE
        }
    })
}

unsafe extern "C" fn x_sync(_file: *mut ffi::sqlite3_file, _flags: c_int) -> c_int {
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_file_size(
    file: *mut ffi::sqlite3_file,
    out: *mut ffi::sqlite3_int64,
) -> c_int {
    record_metric(IoMetric::FileSize);
    let file = &mut *file.cast::<IcStableFile>();
    if let FileKind::Temp(temp) = &file.kind {
        let Ok(size) = ffi::sqlite3_int64::try_from(temp.len()) else {
            return ffi::SQLITE_IOERR_FSTAT;
        };
        *out = size;
        return ffi::SQLITE_OK;
    }
    if file.read_only {
        if let Some(block) = &file.read_snapshot {
            let Ok(size) = ffi::sqlite3_int64::try_from(block.db_size) else {
                return ffi::SQLITE_IOERR_FSTAT;
            };
            *out = size;
            return ffi::SQLITE_OK;
        }
        return memory::with_context(file.context, || {
            let size = match Superblock::load() {
                Ok(block) => {
                    let size = block.db_size;
                    file.read_snapshot = Some(block);
                    size
                }
                Err(error) => {
                    vfs::record_last_error(ffi::SQLITE_IOERR_FSTAT, error.to_string());
                    return ffi::SQLITE_IOERR_FSTAT;
                }
            };
            let Ok(size) = ffi::sqlite3_int64::try_from(size) else {
                return ffi::SQLITE_IOERR_FSTAT;
            };
            *out = size;
            ffi::SQLITE_OK
        });
    }
    memory::with_context(file.context, || {
        let size = match stable_blob::file_size() {
            Ok(value) => value,
            Err(error) => {
                vfs::record_last_error(ffi::SQLITE_IOERR_FSTAT, error.to_string());
                return ffi::SQLITE_IOERR_FSTAT;
            }
        };
        let Ok(size) = ffi::sqlite3_int64::try_from(size) else {
            return ffi::SQLITE_IOERR_FSTAT;
        };
        *out = size;
        ffi::SQLITE_OK
    })
}

unsafe extern "C" fn x_lock(file: *mut ffi::sqlite3_file, level: c_int) -> c_int {
    record_metric(IoMetric::Lock);
    let file = &mut *file.cast::<IcStableFile>();
    if !matches!(file.kind, FileKind::Main) {
        return ffi::SQLITE_OK;
    }
    lock::lock_for(file.context, level);
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_unlock(file: *mut ffi::sqlite3_file, level: c_int) -> c_int {
    record_metric(IoMetric::Unlock);
    let file = &mut *file.cast::<IcStableFile>();
    if !matches!(file.kind, FileKind::Main) {
        return ffi::SQLITE_OK;
    }
    lock::unlock_for(file.context, level);
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_check_reserved_lock(file: *mut ffi::sqlite3_file, out: *mut c_int) -> c_int {
    record_metric(IoMetric::CheckReservedLock);
    let file = &mut *file.cast::<IcStableFile>();
    if !matches!(file.kind, FileKind::Main) {
        *out = 0;
        return ffi::SQLITE_OK;
    }
    *out = if lock::has_reserved_for(file.context) {
        1
    } else {
        0
    };
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_file_control(
    file: *mut ffi::sqlite3_file,
    op: c_int,
    arg: *mut c_void,
) -> c_int {
    record_metric(IoMetric::FileControl);
    let file = &mut *file.cast::<IcStableFile>();
    match op {
        ffi::SQLITE_FCNTL_LOCKSTATE => write_c_int(arg, lock::level_for(file.context)),
        ffi::SQLITE_FCNTL_LAST_ERRNO => {
            memory::with_context(file.context, || write_c_int(arg, vfs::last_errno()))
        }
        ffi::SQLITE_FCNTL_SIZE_HINT | ffi::SQLITE_FCNTL_CHUNK_SIZE => ffi::SQLITE_OK,
        ffi::SQLITE_FCNTL_HAS_MOVED => write_c_int(arg, 0),
        ffi::SQLITE_FCNTL_VFSNAME => {
            if arg.is_null() {
                return ffi::SQLITE_OK;
            }
            static FORMAT: &[u8] = b"%s\0";
            static NAME: &[u8] = b"icstable\0";
            let out = arg.cast::<*mut c_char>();
            *out = ffi::sqlite3_mprintf(FORMAT.as_ptr().cast(), NAME.as_ptr().cast::<c_char>());
            ffi::SQLITE_OK
        }
        ffi::SQLITE_FCNTL_POWERSAFE_OVERWRITE => {
            if !arg.is_null() {
                let value = *(arg.cast::<c_int>());
                if value >= 0 {
                    file.powersafe_overwrite = value != 0;
                }
                *(arg.cast::<c_int>()) = if file.powersafe_overwrite { 1 } else { 0 };
            }
            ffi::SQLITE_OK
        }
        _ => ffi::SQLITE_NOTFOUND,
    }
}

unsafe extern "C" fn x_sector_size(_file: *mut ffi::sqlite3_file) -> c_int {
    c_int::try_from(SQLITE_PAGE_SIZE).expect("page size fits c_int")
}

unsafe extern "C" fn x_device_characteristics(file: *mut ffi::sqlite3_file) -> c_int {
    record_metric(IoMetric::DeviceCharacteristics);
    let file = &mut *file.cast::<IcStableFile>();
    if file.powersafe_overwrite {
        // Stable writes affect only the requested byte range; image publish
        // happens separately through the superblock.
        ffi::SQLITE_IOCAP_POWERSAFE_OVERWRITE
    } else {
        0
    }
}

// === Safe callback helpers ===

fn checked_amount(amount: c_int) -> Option<usize> {
    if amount < 0 {
        return None;
    }
    usize::try_from(amount).ok()
}

fn checked_offset(offset: ffi::sqlite3_int64) -> Option<u64> {
    if offset < 0 {
        return None;
    }
    u64::try_from(offset).ok()
}

unsafe fn write_c_int(arg: *mut c_void, value: c_int) -> c_int {
    if !arg.is_null() {
        *(arg.cast::<c_int>()) = value;
    }
    ffi::SQLITE_OK
}

#[cfg(test)]
mod tests {
    use super::{
        install, x_check_reserved_lock, x_close, x_device_characteristics, x_file_control,
        x_file_size, x_lock, x_read, x_sync, x_truncate, x_unlock, x_write,
    };
    use super::{FileKind, IcStableFile};
    use crate::sqlite_vfs::{ffi, lock, stable_blob, vfs};
    use crate::stable::memory::{self, ContextId};
    use std::ffi::{c_char, c_void, CStr};
    use std::mem::MaybeUninit;
    use std::ptr;

    fn reset() -> ContextId {
        memory::reset_for_tests();
        lock::reset_for_tests();
        memory::init(memory::memory_for_tests()).unwrap()
    }

    unsafe fn install_main_file(
        storage: &mut MaybeUninit<IcStableFile>,
        context: ContextId,
    ) -> *mut ffi::sqlite3_file {
        let raw = storage.as_mut_ptr().cast::<ffi::sqlite3_file>();
        install(raw, FileKind::Main, false, context);
        raw
    }

    #[test]
    #[serial_test::serial]
    fn file_control_handles_supported_opcodes() {
        let context = reset();
        let mut storage = MaybeUninit::<IcStableFile>::uninit();
        let raw = unsafe { install_main_file(&mut storage, context) };

        unsafe {
            assert_eq!(
                x_device_characteristics(raw),
                ffi::SQLITE_IOCAP_POWERSAFE_OVERWRITE
            );

            lock::lock(ffi::SQLITE_LOCK_RESERVED);
            let mut lock_state = 0;
            assert_eq!(
                x_file_control(
                    raw,
                    ffi::SQLITE_FCNTL_LOCKSTATE,
                    ptr::addr_of_mut!(lock_state).cast::<c_void>(),
                ),
                ffi::SQLITE_OK
            );
            assert_eq!(lock_state, ffi::SQLITE_LOCK_RESERVED);

            vfs::record_last_error(ffi::SQLITE_IOERR_WRITE, "write failed");
            let mut errno = 0;
            assert_eq!(
                x_file_control(
                    raw,
                    ffi::SQLITE_FCNTL_LAST_ERRNO,
                    ptr::addr_of_mut!(errno).cast::<c_void>(),
                ),
                ffi::SQLITE_OK
            );
            assert_eq!(errno, ffi::SQLITE_IOERR_WRITE);

            let mut size_hint: ffi::sqlite3_int64 = 8192;
            assert_eq!(
                x_file_control(
                    raw,
                    ffi::SQLITE_FCNTL_SIZE_HINT,
                    ptr::addr_of_mut!(size_hint).cast::<c_void>(),
                ),
                ffi::SQLITE_OK
            );

            let mut chunk_size = 4096;
            assert_eq!(
                x_file_control(
                    raw,
                    ffi::SQLITE_FCNTL_CHUNK_SIZE,
                    ptr::addr_of_mut!(chunk_size).cast::<c_void>(),
                ),
                ffi::SQLITE_OK
            );

            let mut has_moved = 1;
            assert_eq!(
                x_file_control(
                    raw,
                    ffi::SQLITE_FCNTL_HAS_MOVED,
                    ptr::addr_of_mut!(has_moved).cast::<c_void>(),
                ),
                ffi::SQLITE_OK
            );
            assert_eq!(has_moved, 0);

            let mut vfs_name: *mut c_char = ptr::null_mut();
            assert_eq!(
                x_file_control(
                    raw,
                    ffi::SQLITE_FCNTL_VFSNAME,
                    ptr::addr_of_mut!(vfs_name).cast::<c_void>(),
                ),
                ffi::SQLITE_OK
            );
            assert_eq!(CStr::from_ptr(vfs_name).to_str().unwrap(), "icstable");
            ffi::sqlite3_free(vfs_name.cast::<c_void>());

            let mut psow = 0;
            assert_eq!(
                x_file_control(
                    raw,
                    ffi::SQLITE_FCNTL_POWERSAFE_OVERWRITE,
                    ptr::addr_of_mut!(psow).cast::<c_void>(),
                ),
                ffi::SQLITE_OK
            );
            assert_eq!(psow, 0);
            assert_eq!(x_device_characteristics(raw), 0);

            assert_eq!(
                x_file_control(raw, 999_999, ptr::null_mut()),
                ffi::SQLITE_NOTFOUND
            );
            assert_eq!(x_close(raw), ffi::SQLITE_OK);
        }
    }

    #[test]
    #[serial_test::serial]
    fn reserved_lock_is_visible_across_main_file_handles() {
        let context = reset();
        let mut first = MaybeUninit::<IcStableFile>::uninit();
        let mut second = MaybeUninit::<IcStableFile>::uninit();
        let first_raw = unsafe { install_main_file(&mut first, context) };
        let second_raw = unsafe { install_main_file(&mut second, context) };

        unsafe {
            let mut reserved = 0;
            assert_eq!(x_lock(first_raw, ffi::SQLITE_LOCK_RESERVED), ffi::SQLITE_OK);
            assert_eq!(
                x_check_reserved_lock(second_raw, ptr::addr_of_mut!(reserved)),
                ffi::SQLITE_OK
            );
            assert_eq!(reserved, 1);

            assert_eq!(x_unlock(first_raw, ffi::SQLITE_LOCK_NONE), ffi::SQLITE_OK);
            assert_eq!(
                x_check_reserved_lock(second_raw, ptr::addr_of_mut!(reserved)),
                ffi::SQLITE_OK
            );
            assert_eq!(reserved, 0);

            assert_eq!(x_close(first_raw), ffi::SQLITE_OK);
            assert_eq!(x_close(second_raw), ffi::SQLITE_OK);
        }
    }

    #[test]
    #[serial_test::serial]
    fn main_file_short_read_zero_fills_eof_tail() {
        let context = reset();
        stable_blob::write_at(0, b"abc").unwrap();
        let mut storage = MaybeUninit::<IcStableFile>::uninit();
        let raw = unsafe { install_main_file(&mut storage, context) };
        let mut out = [9_u8; 8];

        unsafe {
            assert_eq!(
                x_read(raw, out.as_mut_ptr().cast::<c_void>(), 8, 1),
                ffi::SQLITE_IOERR_SHORT_READ
            );
            assert_eq!(x_close(raw), ffi::SQLITE_OK);
        }
        assert_eq!(out, [b'b', b'c', 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    #[serial_test::serial]
    fn invalid_main_file_read_boundaries_return_read_errors() {
        let context = reset();
        let mut storage = MaybeUninit::<IcStableFile>::uninit();
        let raw = unsafe { install_main_file(&mut storage, context) };
        let mut out = [0_u8; 1];

        unsafe {
            assert_eq!(
                x_read(raw, out.as_mut_ptr().cast::<c_void>(), -1, 0),
                ffi::SQLITE_IOERR_READ
            );
            assert_eq!(
                x_read(raw, out.as_mut_ptr().cast::<c_void>(), 1, -1),
                ffi::SQLITE_IOERR_READ
            );
            assert_eq!(x_close(raw), ffi::SQLITE_OK);
        }
    }

    #[test]
    #[serial_test::serial]
    fn invalid_main_file_write_and_truncate_boundaries_return_errors() {
        let context = reset();
        let mut storage = MaybeUninit::<IcStableFile>::uninit();
        let raw = unsafe { install_main_file(&mut storage, context) };
        let input = [1_u8; 1];

        unsafe {
            assert_eq!(
                x_write(raw, input.as_ptr().cast::<c_void>(), -1, 0),
                ffi::SQLITE_IOERR_WRITE
            );
            assert_eq!(
                x_write(raw, input.as_ptr().cast::<c_void>(), 1, -1),
                ffi::SQLITE_IOERR_WRITE
            );
            assert_eq!(x_truncate(raw, -1), ffi::SQLITE_IOERR_TRUNCATE);
            assert_eq!(x_close(raw), ffi::SQLITE_OK);
        }
    }

    #[test]
    #[serial_test::serial]
    fn read_only_main_file_write_propagates_last_errno() {
        let context = reset();
        let mut storage = MaybeUninit::<IcStableFile>::uninit();
        let raw = storage.as_mut_ptr().cast::<ffi::sqlite3_file>();
        unsafe {
            install(raw, FileKind::Main, true, context);
        }
        let input = [1_u8; 1];

        let rc = memory::with_context(context, || unsafe {
            x_write(raw, input.as_ptr().cast::<c_void>(), 1, 0)
        });
        assert_eq!(rc, ffi::SQLITE_READONLY);

        unsafe {
            let mut errno = 0;
            assert_eq!(
                x_file_control(
                    raw,
                    ffi::SQLITE_FCNTL_LAST_ERRNO,
                    ptr::addr_of_mut!(errno).cast::<c_void>(),
                ),
                ffi::SQLITE_OK
            );
            assert_eq!(errno, ffi::SQLITE_READONLY);
            assert_eq!(x_close(raw), ffi::SQLITE_OK);
        }
    }

    #[test]
    #[serial_test::serial]
    fn temp_file_callbacks_cover_size_sync_and_truncate_boundaries() {
        let context = reset();
        let mut storage = MaybeUninit::<IcStableFile>::uninit();
        let raw = storage.as_mut_ptr().cast::<ffi::sqlite3_file>();
        unsafe {
            install(raw, FileKind::Temp(Default::default()), false, context);
        }
        let input = *b"abcd";
        let mut out = [9_u8; 6];

        unsafe {
            assert_eq!(
                x_write(raw, input.as_ptr().cast::<c_void>(), 4, 0),
                ffi::SQLITE_OK
            );
            let mut size = -1;
            assert_eq!(x_file_size(raw, ptr::addr_of_mut!(size)), ffi::SQLITE_OK);
            assert_eq!(size, 4);
            assert_eq!(
                x_read(raw, out.as_mut_ptr().cast::<c_void>(), 6, 0),
                ffi::SQLITE_IOERR_SHORT_READ
            );
            assert_eq!(out, [b'a', b'b', b'c', b'd', 0, 0]);
            assert_eq!(x_sync(raw, 0), ffi::SQLITE_OK);
            assert_eq!(x_truncate(raw, -1), ffi::SQLITE_IOERR_TRUNCATE);
            assert_eq!(x_truncate(raw, 2), ffi::SQLITE_OK);
            assert_eq!(x_file_size(raw, ptr::addr_of_mut!(size)), ffi::SQLITE_OK);
            assert_eq!(size, 2);
            assert_eq!(x_close(raw), ffi::SQLITE_OK);
        }
    }
}
