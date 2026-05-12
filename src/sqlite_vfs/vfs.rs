//! `sqlite3_vfs` callback bodies for the `icstable` VFS.
//!
//! The VFS recognizes `/main.db` as stable memory and treats every other opened
//! file as volatile heap storage. WAL and mmap callbacks are intentionally absent.

use crate::config::MAIN_DB_PATH;
use crate::sqlite_vfs::ffi;
use crate::sqlite_vfs::file::{self, FileKind};
use crate::sqlite_vfs::temp::TempFile;
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

static VFS_NAME_NUL: &[u8] = b"icstable\0";
static PREPARE_ONCE: Once = Once::new();

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
    let path = path_from_sqlite(name);
    let read_only = (flags & ffi::SQLITE_OPEN_READONLY) != 0;
    if !file.is_null() {
        (*file).pMethods = ptr::null();
    }
    if !out_flags.is_null() {
        *out_flags = flags;
    }

    if is_main_db(path.as_deref()) {
        let Ok(block) = Superblock::load() else {
            return ffi::SQLITE_CANTOPEN;
        };
        if block.is_importing() {
            return ffi::SQLITE_CANTOPEN;
        }
        file::install(file, FileKind::Main, read_only);
        return ffi::SQLITE_OK;
    }

    if (flags & ffi::SQLITE_OPEN_WAL) != 0 {
        return ffi::SQLITE_CANTOPEN;
    }

    file::install(file, FileKind::Temp(TempFile::default()), read_only);
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_delete(
    _vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    _sync_dir: c_int,
) -> c_int {
    let path = path_from_sqlite(name);
    if is_main_db(path.as_deref()) {
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
    let path = path_from_sqlite(name);
    *out = if is_main_db(path.as_deref()) { 1 } else { 0 };
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
    let input = path_from_sqlite(name).unwrap_or_else(|| MAIN_DB_PATH.to_string());
    let path = if is_main_db(Some(&input)) {
        MAIN_DB_PATH
    } else {
        input.as_str()
    };
    let bytes = path.as_bytes();
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
    let parsed = int_time
        .to_string()
        .parse::<f64>()
        .unwrap_or(210_866_760_000_000.0);
    *out = parsed / 86_400_000.0;
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
    _len: c_int,
    _out: *mut c_char,
) -> c_int {
    0
}

fn is_main_db(path: Option<&str>) -> bool {
    match path {
        Some(value) => normalized_main_path(value) == MAIN_DB_PATH,
        None => false,
    }
}

fn normalized_main_path(path: &str) -> &str {
    let without_scheme = path.strip_prefix("file:").unwrap_or(path);
    without_scheme
        .split_once('?')
        .map_or(without_scheme, |(path, _)| path)
}

unsafe fn path_from_sqlite(name: *const c_char) -> Option<String> {
    if name.is_null() {
        return None;
    }
    Some(CStr::from_ptr(name).to_string_lossy().into_owned())
}

fn current_time_nanos() -> u64 {
    #[cfg(target_arch = "wasm32")]
    {
        ic_cdk::api::time()
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
