//! `sqlite3_vfs` callback bodies for the `icstable` VFS.
//!
//! The VFS recognizes `/main.db` as stable memory and treats every other opened
//! file as volatile heap storage. WAL and mmap callbacks are intentionally absent.

use crate::config::{MAIN_DB_PATH, VFS_NAME_NUL};
use crate::sqlite_vfs::ffi;
use crate::sqlite_vfs::file::{self, FileKind};
use crate::sqlite_vfs::temp::TempFile;
use crate::stable::memory;
use crate::stable::meta::Superblock;
use std::ffi::{c_char, c_int, CStr};
use std::ptr;
use std::sync::Once;

pub static mut VFS: ffi::sqlite3_vfs = ffi::sqlite3_vfs {
    iVersion: 1,
    szOsFile: 0,
    mxPathname: 256,
    pNext: ptr::null_mut(),
    zName: ptr::null(),
    pAppData: ptr::null_mut(),
    xOpen: Some(x_open),
    xDelete: Some(x_delete),
    xAccess: Some(x_access),
    xFullPathname: Some(x_full_pathname),
    xDlOpen: None,
    xDlError: None,
    xDlSym: None,
    xDlClose: None,
    xRandomness: Some(x_randomness),
    xSleep: Some(x_sleep),
    xCurrentTime: Some(x_current_time),
    xGetLastError: Some(x_get_last_error),
    xCurrentTimeInt64: Some(x_current_time_int64),
    xSetSystemCall: None,
    xGetSystemCall: None,
    xNextSystemCall: None,
};

static PREPARE_ONCE: Once = Once::new();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OpenKind {
    MainDb,
    MainJournal,
    TempDb,
    TempJournal,
    TransientDb,
    Wal,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OpenAccess {
    ReadOnly,
    ReadWrite { create: bool },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct OpenOptions {
    pub kind: OpenKind,
    pub access: OpenAccess,
    pub uri: bool,
    pub delete_on_close: bool,
}

// === FFI callback boundary ===

/// # Safety
///
/// SQLite calls this during global VFS initialization. The returned pointer is a
/// process-global static and must only be handed back to SQLite.
pub unsafe fn prepare() -> *mut ffi::sqlite3_vfs {
    PREPARE_ONCE.call_once(|| {
        let vfs = ptr::addr_of_mut!(VFS);
        (*vfs).szOsFile = c_int::try_from(std::mem::size_of::<file::IcStableFile>())
            .expect("sqlite file handle size fits c_int");
        (*vfs).zName = VFS_NAME_NUL.as_ptr().cast::<c_char>();
    });
    ptr::addr_of_mut!(VFS)
}

unsafe extern "C" fn x_open(
    _vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    file: *mut ffi::sqlite3_file,
    flags: c_int,
    out_flags: *mut c_int,
) -> c_int {
    let opens_main_db = is_main_db_path(name);
    let options = classify_open_flags(flags);
    let context = match memory::active_context_id() {
        Ok(context) => context,
        Err(error) => {
            record_last_error(ffi::SQLITE_CANTOPEN, error.to_string());
            return ffi::SQLITE_CANTOPEN;
        }
    };
    if !file.is_null() {
        (*file).pMethods = ptr::null();
    }
    if !out_flags.is_null() {
        *out_flags = flags;
    }

    if opens_main_db {
        let Ok(block) = Superblock::load() else {
            record_last_error(ffi::SQLITE_CANTOPEN, "failed to load SQLite superblock");
            return ffi::SQLITE_CANTOPEN;
        };
        if block.is_importing() {
            record_last_error(ffi::SQLITE_CANTOPEN, "database import is in progress");
            return ffi::SQLITE_CANTOPEN;
        }
        let read_only = options.access == OpenAccess::ReadOnly;
        file::install(file, FileKind::Main, read_only, context);
        return ffi::SQLITE_OK;
    }

    if options.kind == OpenKind::Wal {
        record_last_error(ffi::SQLITE_CANTOPEN, "WAL files are unsupported");
        return ffi::SQLITE_CANTOPEN;
    }

    file::install(
        file,
        FileKind::Temp(TempFile::default()),
        options.access == OpenAccess::ReadOnly,
        context,
    );
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_delete(
    _vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    _sync_dir: c_int,
) -> c_int {
    if is_main_db_path(name) {
        return ffi::SQLITE_IOERR_DELETE;
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_access(
    _vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    _flags: c_int,
    out: *mut c_int,
) -> c_int {
    *out = if is_main_db_path(name) { 1 } else { 0 };
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_full_pathname(
    _vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    out_len: c_int,
    out: *mut c_char,
) -> c_int {
    let Some(max_len) = usize::try_from(out_len).ok() else {
        return ffi::SQLITE_CANTOPEN;
    };
    if max_len == 0 {
        return ffi::SQLITE_CANTOPEN;
    }
    let bytes = if name.is_null() {
        MAIN_DB_PATH.as_bytes()
    } else {
        let input = CStr::from_ptr(name).to_bytes();
        if normalized_main_path_bytes(input) == MAIN_DB_PATH.as_bytes() {
            MAIN_DB_PATH.as_bytes()
        } else {
            input
        }
    };
    if bytes.len() >= max_len {
        return ffi::SQLITE_CANTOPEN;
    }
    ptr::copy_nonoverlapping(bytes.as_ptr().cast::<c_char>(), out, bytes.len());
    *out.add(bytes.len()) = 0;
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_randomness(
    _vfs: *mut ffi::sqlite3_vfs,
    amount: c_int,
    out: *mut c_char,
) -> c_int {
    let Some(amount) = usize::try_from(amount).ok() else {
        return 0;
    };
    let seed = Superblock::load()
        .map(|block| block.last_tx_id ^ block.db_size)
        .unwrap_or(0);
    let mut state = seed ^ 0x9e37_79b9_7f4a_7c15;
    let dst = std::slice::from_raw_parts_mut(out.cast::<u8>(), amount);
    for byte in dst {
        state ^= state << 7;
        state ^= state >> 9;
        state = state.wrapping_mul(0x0000_0100_0000_01b3);
        *byte = state.to_le_bytes()[0];
    }
    c_int::try_from(amount).unwrap_or(c_int::MAX)
}

unsafe extern "C" fn x_sleep(_vfs: *mut ffi::sqlite3_vfs, _microseconds: c_int) -> c_int {
    0
}

unsafe extern "C" fn x_current_time(vfs: *mut ffi::sqlite3_vfs, out: *mut f64) -> c_int {
    let mut int_time: ffi::sqlite3_int64 = 0;
    let rc = x_current_time_int64(vfs, ptr::addr_of_mut!(int_time));
    if rc != ffi::SQLITE_OK {
        return rc;
    }
    *out = (int_time as f64) / 86_400_000.0;
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_current_time_int64(
    _vfs: *mut ffi::sqlite3_vfs,
    out: *mut ffi::sqlite3_int64,
) -> c_int {
    let unix_ms = current_time_nanos() / 1_000_000;
    let value = 210_866_760_000_000_u64.saturating_add(unix_ms);
    let Ok(value) = ffi::sqlite3_int64::try_from(value) else {
        return ffi::SQLITE_IOERR;
    };
    *out = value;
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_get_last_error(
    _vfs: *mut ffi::sqlite3_vfs,
    len: c_int,
    out: *mut c_char,
) -> c_int {
    if out.is_null() || len <= 0 {
        return 0;
    }
    let Some(max_len) = usize::try_from(len).ok() else {
        return 0;
    };
    let Ok(context) = memory::active_context_id() else {
        *out = 0;
        return 0;
    };
    let Some(error) = memory::last_error(context) else {
        *out = 0;
        return 0;
    };
    let bytes = error.message.as_bytes();
    let copy_len = bytes.len().min(max_len.saturating_sub(1));
    ptr::copy_nonoverlapping(bytes.as_ptr().cast::<c_char>(), out, copy_len);
    *out.add(copy_len) = 0;
    c_int::try_from(copy_len).unwrap_or(c_int::MAX)
}

// === Safe VFS logic and helpers ===

pub(crate) fn record_last_error(errno: c_int, message: impl Into<String>) {
    if let Ok(context) = memory::active_context_id() {
        record_last_error_for(context, errno, message);
    }
}

pub(crate) fn record_last_error_for(
    context: memory::ContextId,
    errno: c_int,
    message: impl Into<String>,
) {
    memory::record_last_error(context, errno, message.into());
}

pub(crate) fn last_errno() -> c_int {
    let Ok(context) = memory::active_context_id() else {
        return 0;
    };
    memory::last_errno(context)
}

pub(crate) fn classify_open_flags(flags: c_int) -> OpenOptions {
    let kind = if (flags & ffi::SQLITE_OPEN_WAL) != 0 {
        OpenKind::Wal
    } else if (flags & ffi::SQLITE_OPEN_MAIN_JOURNAL) != 0 {
        OpenKind::MainJournal
    } else if (flags & ffi::SQLITE_OPEN_TEMP_DB) != 0 {
        OpenKind::TempDb
    } else if (flags & ffi::SQLITE_OPEN_TEMP_JOURNAL) != 0 {
        OpenKind::TempJournal
    } else if (flags & ffi::SQLITE_OPEN_TRANSIENT_DB) != 0 {
        OpenKind::TransientDb
    } else if (flags & ffi::SQLITE_OPEN_MAIN_DB) != 0 {
        OpenKind::MainDb
    } else {
        OpenKind::Other
    };
    let access = if (flags & ffi::SQLITE_OPEN_READONLY) != 0 {
        OpenAccess::ReadOnly
    } else {
        OpenAccess::ReadWrite {
            create: (flags & ffi::SQLITE_OPEN_CREATE) != 0,
        }
    };
    OpenOptions {
        kind,
        access,
        uri: (flags & ffi::SQLITE_OPEN_URI) != 0,
        delete_on_close: (flags & ffi::SQLITE_OPEN_DELETEONCLOSE) != 0,
    }
}

unsafe fn is_main_db_path(name: *const c_char) -> bool {
    if name.is_null() {
        return false;
    }
    normalized_main_path_bytes(CStr::from_ptr(name).to_bytes()) == MAIN_DB_PATH.as_bytes()
}

fn normalized_main_path_bytes(path: &[u8]) -> &[u8] {
    let without_scheme = path.strip_prefix(b"file:").unwrap_or(path);
    for (index, byte) in without_scheme.iter().enumerate() {
        if *byte == b'?' {
            return &without_scheme[..index];
        }
    }
    without_scheme
}

fn current_time_nanos() -> u64 {
    #[cfg(target_arch = "wasm32")]
    {
        crate::ic0_shim::time()
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::time::{SystemTime, UNIX_EPOCH};
        let duration = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        classify_open_flags, x_access, x_delete, x_full_pathname, x_get_last_error, x_open,
        OpenAccess, OpenKind,
    };
    use crate::sqlite_vfs::{ffi, lock};
    use crate::stable::memory;
    use std::ffi::{CStr, CString};
    use std::mem::MaybeUninit;
    use std::ptr;

    #[test]
    fn classify_open_flags_covers_sqlite_file_kinds() {
        assert_eq!(
            classify_open_flags(ffi::SQLITE_OPEN_READONLY | ffi::SQLITE_OPEN_MAIN_DB).kind,
            OpenKind::MainDb
        );
        assert_eq!(
            classify_open_flags(ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_MAIN_JOURNAL).kind,
            OpenKind::MainJournal
        );
        assert_eq!(
            classify_open_flags(ffi::SQLITE_OPEN_CREATE | ffi::SQLITE_OPEN_TEMP_DB).kind,
            OpenKind::TempDb
        );
        assert_eq!(
            classify_open_flags(ffi::SQLITE_OPEN_TEMP_JOURNAL).kind,
            OpenKind::TempJournal
        );
        assert_eq!(
            classify_open_flags(ffi::SQLITE_OPEN_TRANSIENT_DB).kind,
            OpenKind::TransientDb
        );
        assert_eq!(
            classify_open_flags(ffi::SQLITE_OPEN_WAL).kind,
            OpenKind::Wal
        );
    }

    #[test]
    fn classify_open_flags_tracks_access_and_uri_bits() {
        let read_only = classify_open_flags(ffi::SQLITE_OPEN_READONLY | ffi::SQLITE_OPEN_URI);
        assert_eq!(read_only.access, OpenAccess::ReadOnly);
        assert!(read_only.uri);

        let read_write = classify_open_flags(
            ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_CREATE | ffi::SQLITE_OPEN_DELETEONCLOSE,
        );
        assert_eq!(read_write.access, OpenAccess::ReadWrite { create: true });
        assert!(!read_write.uri);
        assert!(read_write.delete_on_close);
    }

    #[test]
    #[serial_test::serial]
    fn x_open_accepts_supported_open_classes_and_rejects_wal() {
        memory::reset_for_tests();
        lock::reset_for_tests();
        memory::init(memory::memory_for_tests()).unwrap();

        let cases = [
            (
                "/main.db",
                ffi::SQLITE_OPEN_READONLY | ffi::SQLITE_OPEN_MAIN_DB,
            ),
            (
                "/main.db",
                ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_MAIN_DB,
            ),
            (
                "/main.db",
                ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_CREATE | ffi::SQLITE_OPEN_MAIN_DB,
            ),
            (
                "/main.db-journal",
                ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_MAIN_JOURNAL,
            ),
            (
                "",
                ffi::SQLITE_OPEN_READWRITE
                    | ffi::SQLITE_OPEN_CREATE
                    | ffi::SQLITE_OPEN_TEMP_DB
                    | ffi::SQLITE_OPEN_DELETEONCLOSE,
            ),
            (
                "",
                ffi::SQLITE_OPEN_READWRITE
                    | ffi::SQLITE_OPEN_CREATE
                    | ffi::SQLITE_OPEN_TEMP_JOURNAL
                    | ffi::SQLITE_OPEN_DELETEONCLOSE,
            ),
            (
                "",
                ffi::SQLITE_OPEN_READWRITE
                    | ffi::SQLITE_OPEN_CREATE
                    | ffi::SQLITE_OPEN_TRANSIENT_DB
                    | ffi::SQLITE_OPEN_DELETEONCLOSE,
            ),
            (
                "file:/main.db?mode=ro",
                ffi::SQLITE_OPEN_READONLY | ffi::SQLITE_OPEN_MAIN_DB | ffi::SQLITE_OPEN_URI,
            ),
        ];

        for (name, flags) in cases {
            let mut storage = MaybeUninit::<crate::sqlite_vfs::file::IcStableFile>::uninit();
            let mut out_flags = 0;
            let name = CString::new(name).unwrap();
            let file = storage.as_mut_ptr().cast::<ffi::sqlite3_file>();
            let rc = unsafe {
                x_open(
                    ptr::null_mut(),
                    name.as_ptr(),
                    file,
                    flags,
                    ptr::addr_of_mut!(out_flags),
                )
            };
            assert_eq!(rc, ffi::SQLITE_OK, "flags={flags}");
            assert_eq!(out_flags, flags);
            unsafe {
                assert!(!(*file).pMethods.is_null());
                ((*(*file).pMethods).xClose.unwrap())(file);
            }
        }

        let mut storage = MaybeUninit::<crate::sqlite_vfs::file::IcStableFile>::uninit();
        let wal = CString::new("/main.db-wal").unwrap();
        let rc = unsafe {
            x_open(
                ptr::null_mut(),
                wal.as_ptr(),
                storage.as_mut_ptr().cast::<ffi::sqlite3_file>(),
                ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_WAL,
                ptr::null_mut(),
            )
        };
        assert_eq!(rc, ffi::SQLITE_CANTOPEN);
    }

    #[test]
    fn x_full_pathname_normalizes_main_db_without_touching_other_paths() {
        let main = CString::new("file:/main.db?mode=ro").unwrap();
        let temp = CString::new("/tmp/sqlite-temp").unwrap();
        let mut out = [0_i8; 64];

        let rc = unsafe {
            x_full_pathname(
                ptr::null_mut(),
                main.as_ptr(),
                i32::try_from(out.len()).unwrap(),
                out.as_mut_ptr(),
            )
        };
        assert_eq!(rc, ffi::SQLITE_OK);
        assert_eq!(
            unsafe { CStr::from_ptr(out.as_ptr()) }.to_str().unwrap(),
            "/main.db"
        );

        let rc = unsafe {
            x_full_pathname(
                ptr::null_mut(),
                temp.as_ptr(),
                i32::try_from(out.len()).unwrap(),
                out.as_mut_ptr(),
            )
        };
        assert_eq!(rc, ffi::SQLITE_OK);
        assert_eq!(
            unsafe { CStr::from_ptr(out.as_ptr()) }.to_str().unwrap(),
            "/tmp/sqlite-temp"
        );
    }

    #[test]
    fn x_full_pathname_rejects_zero_or_too_short_output_buffers() {
        let main = CString::new("/main.db").unwrap();
        let mut out = [0_i8; 4];

        assert_eq!(
            unsafe { x_full_pathname(ptr::null_mut(), main.as_ptr(), 0, out.as_mut_ptr()) },
            ffi::SQLITE_CANTOPEN
        );
        assert_eq!(
            unsafe {
                x_full_pathname(
                    ptr::null_mut(),
                    main.as_ptr(),
                    i32::try_from(out.len()).unwrap(),
                    out.as_mut_ptr(),
                )
            },
            ffi::SQLITE_CANTOPEN
        );
    }

    #[test]
    fn x_access_and_delete_distinguish_main_db_from_temp_paths() {
        let main = CString::new("/main.db").unwrap();
        let temp = CString::new("/tmp/sqlite-temp").unwrap();
        let mut exists = -1;

        unsafe {
            assert_eq!(
                x_access(ptr::null_mut(), main.as_ptr(), 0, ptr::addr_of_mut!(exists)),
                ffi::SQLITE_OK
            );
            assert_eq!(exists, 1);
            assert_eq!(
                x_delete(ptr::null_mut(), main.as_ptr(), 0),
                ffi::SQLITE_IOERR_DELETE
            );

            assert_eq!(
                x_access(ptr::null_mut(), temp.as_ptr(), 0, ptr::addr_of_mut!(exists)),
                ffi::SQLITE_OK
            );
            assert_eq!(exists, 0);
            assert_eq!(x_delete(ptr::null_mut(), temp.as_ptr(), 0), ffi::SQLITE_OK);
        }
    }

    #[test]
    #[serial_test::serial]
    fn x_get_last_error_copies_recorded_message() {
        memory::reset_for_tests();
        memory::init(memory::memory_for_tests()).unwrap();
        super::record_last_error(ffi::SQLITE_CANTOPEN, "WAL files are unsupported");

        let mut buf = [0_i8; 64];
        let copied = unsafe {
            x_get_last_error(
                ptr::null_mut(),
                i32::try_from(buf.len()).unwrap(),
                buf.as_mut_ptr(),
            )
        };

        assert!(copied > 0);
        assert_eq!(
            unsafe { CStr::from_ptr(buf.as_ptr()) }.to_str().unwrap(),
            "WAL files are unsupported"
        );
    }

    #[test]
    #[serial_test::serial]
    fn x_get_last_error_truncates_to_output_buffer() {
        memory::reset_for_tests();
        memory::init(memory::memory_for_tests()).unwrap();
        super::record_last_error(ffi::SQLITE_CANTOPEN, "WAL files are unsupported");

        let mut buf = [0_i8; 5];
        let copied = unsafe {
            x_get_last_error(
                ptr::null_mut(),
                i32::try_from(buf.len()).unwrap(),
                buf.as_mut_ptr(),
            )
        };

        assert_eq!(copied, 4);
        assert_eq!(
            unsafe { CStr::from_ptr(buf.as_ptr()) }.to_str().unwrap(),
            "WAL "
        );
    }

    #[test]
    #[serial_test::serial]
    fn x_open_wal_rejection_sets_last_error_message() {
        memory::reset_for_tests();
        lock::reset_for_tests();
        memory::init(memory::memory_for_tests()).unwrap();
        let mut storage = MaybeUninit::<crate::sqlite_vfs::file::IcStableFile>::uninit();
        let wal = CString::new("/main.db-wal").unwrap();

        let rc = unsafe {
            x_open(
                ptr::null_mut(),
                wal.as_ptr(),
                storage.as_mut_ptr().cast::<ffi::sqlite3_file>(),
                ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_WAL,
                ptr::null_mut(),
            )
        };
        assert_eq!(rc, ffi::SQLITE_CANTOPEN);

        let mut buf = [0_i8; 64];
        let copied = unsafe {
            x_get_last_error(
                ptr::null_mut(),
                i32::try_from(buf.len()).unwrap(),
                buf.as_mut_ptr(),
            )
        };
        assert!(copied > 0);
        assert_eq!(
            unsafe { CStr::from_ptr(buf.as_ptr()) }.to_str().unwrap(),
            "WAL files are unsupported"
        );
    }
}
