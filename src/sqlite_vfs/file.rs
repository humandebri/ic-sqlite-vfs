//! `sqlite3_file` implementation for `/main.db` and heap temp files.
//!
//! The struct is placed into SQLite-owned memory during `xOpen`; `base` must
//! remain the first field so pointer casts from `sqlite3_file` are valid.

use crate::config::SQLITE_PAGE_SIZE;
use crate::sqlite_vfs::ffi;
use crate::sqlite_vfs::stable_blob;
use crate::sqlite_vfs::temp::TempFile;
use std::ffi::{c_int, c_void};
use std::ptr;

#[repr(C)]
pub struct IcStableFile {
    base: ffi::sqlite3_file,
    kind: FileKind,
    read_only: bool,
    lock_level: c_int,
}

#[derive(Debug)]
pub enum FileKind {
    Main,
    Temp(TempFile),
}

impl IcStableFile {
    pub fn new(kind: FileKind, read_only: bool) -> Self {
        Self {
            base: ffi::sqlite3_file {
                pMethods: ptr::null(),
            },
            kind,
            read_only,
            lock_level: 0,
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

/// # Safety
///
/// `p_file` must point to at least `size_of::<IcStableFile>()` bytes allocated
/// by SQLite for the current `xOpen` call. SQLite owns that memory until
/// `xClose`, where the embedded `FileKind` is dropped exactly once.
pub unsafe fn install(p_file: *mut ffi::sqlite3_file, kind: FileKind, read_only: bool) {
    let file = p_file.cast::<IcStableFile>();
    ptr::write(file, IcStableFile::new(kind, read_only));
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
    let complete = match &mut file.kind {
        FileKind::Main => {
            #[cfg(any(test, debug_assertions))]
            crate::read_metrics::record_x_read(amount);
            match stable_blob::read_at(offset, dst) {
                Ok(value) => value,
                Err(_) => return ffi::SQLITE_IOERR_READ,
            }
        }
        FileKind::Temp(temp) => temp.read(offset, dst),
    };
    if complete {
        ffi::SQLITE_OK
    } else {
        ffi::SQLITE_IOERR_SHORT_READ
    }
}

unsafe extern "C" fn x_write(
    file: *mut ffi::sqlite3_file,
    buf: *const c_void,
    amount: c_int,
    offset: ffi::sqlite3_int64,
) -> c_int {
    let file = &mut *file.cast::<IcStableFile>();
    if file.read_only {
        return ffi::SQLITE_READONLY;
    }
    let Some(amount) = checked_amount(amount) else {
        return ffi::SQLITE_IOERR_WRITE;
    };
    let Some(offset) = checked_offset(offset) else {
        return ffi::SQLITE_IOERR_WRITE;
    };
    let bytes = std::slice::from_raw_parts(buf.cast::<u8>(), amount);
    match &mut file.kind {
        FileKind::Main => stable_blob::write_at(offset, bytes)
            .map(|()| ffi::SQLITE_OK)
            .unwrap_or(ffi::SQLITE_IOERR_WRITE),
        FileKind::Temp(temp) => {
            if temp.write(offset, bytes) {
                ffi::SQLITE_OK
            } else {
                ffi::SQLITE_IOERR_WRITE
            }
        }
    }
}

unsafe extern "C" fn x_truncate(file: *mut ffi::sqlite3_file, size: ffi::sqlite3_int64) -> c_int {
    let file = &mut *file.cast::<IcStableFile>();
    if file.read_only {
        return ffi::SQLITE_READONLY;
    }
    let Some(size) = checked_offset(size) else {
        return ffi::SQLITE_IOERR_TRUNCATE;
    };
    match &mut file.kind {
        FileKind::Main => stable_blob::truncate(size)
            .map(|()| ffi::SQLITE_OK)
            .unwrap_or(ffi::SQLITE_IOERR_TRUNCATE),
        FileKind::Temp(temp) => {
            if temp.truncate(size) {
                ffi::SQLITE_OK
            } else {
                ffi::SQLITE_IOERR_TRUNCATE
            }
        }
    }
}

unsafe extern "C" fn x_sync(_file: *mut ffi::sqlite3_file, _flags: c_int) -> c_int {
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_file_size(
    file: *mut ffi::sqlite3_file,
    out: *mut ffi::sqlite3_int64,
) -> c_int {
    let file = &mut *file.cast::<IcStableFile>();
    let size = match &mut file.kind {
        FileKind::Main => match stable_blob::file_size() {
            Ok(value) => value,
            Err(_) => return ffi::SQLITE_IOERR_FSTAT,
        },
        FileKind::Temp(temp) => temp.len(),
    };
    let Ok(size) = ffi::sqlite3_int64::try_from(size) else {
        return ffi::SQLITE_IOERR_FSTAT;
    };
    *out = size;
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_lock(file: *mut ffi::sqlite3_file, level: c_int) -> c_int {
    let file = &mut *file.cast::<IcStableFile>();
    if level > file.lock_level {
        file.lock_level = level;
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_unlock(file: *mut ffi::sqlite3_file, level: c_int) -> c_int {
    let file = &mut *file.cast::<IcStableFile>();
    file.lock_level = level;
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_check_reserved_lock(file: *mut ffi::sqlite3_file, out: *mut c_int) -> c_int {
    let file = &mut *file.cast::<IcStableFile>();
    *out = if file.lock_level >= ffi::SQLITE_LOCK_RESERVED {
        1
    } else {
        0
    };
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_file_control(
    _file: *mut ffi::sqlite3_file,
    _op: c_int,
    _arg: *mut c_void,
) -> c_int {
    ffi::SQLITE_NOTFOUND
}

unsafe extern "C" fn x_sector_size(_file: *mut ffi::sqlite3_file) -> c_int {
    c_int::try_from(SQLITE_PAGE_SIZE).expect("page size fits c_int")
}

unsafe extern "C" fn x_device_characteristics(_file: *mut ffi::sqlite3_file) -> c_int {
    0
}

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
