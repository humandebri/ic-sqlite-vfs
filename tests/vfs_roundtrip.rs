use ic_sqlite_vfs::config::{SQLITE_CACHE_SIZE_KIB, SQLITE_PAGE_SIZE};
use ic_sqlite_vfs::db::migrate::Migration;
use ic_sqlite_vfs::test_support::memory;
use ic_sqlite_vfs::test_support::stable_blob;
use ic_sqlite_vfs::test_support::{ffi, lock, vfs, Memory, MemoryIdentity, Superblock};
use ic_sqlite_vfs::DefaultMemoryImpl;
use ic_sqlite_vfs::{params, Db, DbError, DbHandle};
use ic_sqlite_vfs::{MemoryId, MemoryManager};
use serial_test::serial;
use std::cell::RefCell;
use std::ffi::{c_void, CStr, CString};
use std::mem::MaybeUninit;
use std::ptr;
use std::rc::Rc;

struct RawConnection {
    raw: *mut ffi::sqlite3,
}

#[derive(Clone)]
struct CustomMemory {
    identity: MemoryIdentity,
    bytes: Rc<RefCell<Vec<u8>>>,
}

impl CustomMemory {
    fn new(id: u128) -> Self {
        Self {
            identity: MemoryIdentity::custom("vfs-roundtrip/custom-memory", id),
            bytes: Rc::new(RefCell::new(Vec::new())),
        }
    }
}

impl Memory for CustomMemory {
    fn identity(&self) -> MemoryIdentity {
        self.identity.clone()
    }

    fn size(&self) -> u64 {
        u64::try_from(self.bytes.borrow().len()).unwrap() / ic_sqlite_vfs::config::STABLE_PAGE_SIZE
    }

    fn grow(&self, pages: u64) -> i64 {
        let current_pages = self.size();
        let Some(next_pages) = current_pages.checked_add(pages) else {
            return -1;
        };
        let Some(next_bytes) = next_pages.checked_mul(ic_sqlite_vfs::config::STABLE_PAGE_SIZE)
        else {
            return -1;
        };
        let Ok(next_len) = usize::try_from(next_bytes) else {
            return -1;
        };
        self.bytes.borrow_mut().resize(next_len, 0);
        i64::try_from(current_pages).unwrap_or(-1)
    }

    fn read(&self, offset: u64, dst: &mut [u8]) {
        let start = usize::try_from(offset).expect("offset fits usize");
        let end = start.checked_add(dst.len()).expect("read end fits usize");
        dst.copy_from_slice(&self.bytes.borrow()[start..end]);
    }

    fn write(&self, offset: u64, src: &[u8]) {
        let start = usize::try_from(offset).expect("offset fits usize");
        let end = start.checked_add(src.len()).expect("write end fits usize");
        self.bytes.borrow_mut()[start..end].copy_from_slice(src);
    }
}

impl Drop for RawConnection {
    fn drop(&mut self) {
        unsafe {
            ffi::sqlite3_close(self.raw);
        }
    }
}

struct VfsMainFile {
    storage: Vec<MaybeUninit<usize>>,
    raw: *mut ffi::sqlite3_file,
}

impl VfsMainFile {
    fn open() -> Self {
        vfs::register();
        let vfs_name = CString::new(ic_sqlite_vfs::config::VFS_NAME).unwrap();
        let db_name = CString::new(ic_sqlite_vfs::config::MAIN_DB_PATH).unwrap();
        let raw_vfs = unsafe { ffi::sqlite3_vfs_find(vfs_name.as_ptr()) };
        assert!(!raw_vfs.is_null());

        let file_size = usize::try_from(unsafe { (*raw_vfs).szOsFile }).unwrap();
        let word_size = std::mem::size_of::<usize>();
        let word_count = file_size.div_ceil(word_size);
        let mut storage = vec![MaybeUninit::<usize>::uninit(); word_count];
        let raw = storage.as_mut_ptr().cast::<ffi::sqlite3_file>();
        let flags = ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_CREATE | ffi::SQLITE_OPEN_MAIN_DB;
        let mut out_flags = 0;
        let open = unsafe { (*raw_vfs).xOpen.unwrap() };
        let rc = unsafe {
            open(
                raw_vfs,
                db_name.as_ptr(),
                raw,
                flags,
                ptr::addr_of_mut!(out_flags),
            )
        };
        assert_eq!(rc, ffi::SQLITE_OK);

        Self { storage, raw }
    }

    fn read(&mut self, offset: u64, len: usize) -> Vec<u8> {
        let mut out = vec![0_u8; len];
        let methods = unsafe { &*(*self.raw).pMethods };
        let read = methods.xRead.unwrap();
        let rc = unsafe {
            read(
                self.raw,
                out.as_mut_ptr().cast::<c_void>(),
                i32::try_from(out.len()).unwrap(),
                i64::try_from(offset).unwrap(),
            )
        };
        assert_eq!(rc, ffi::SQLITE_OK);
        out
    }

    fn write(&mut self, offset: u64, bytes: &[u8]) {
        let methods = unsafe { &*(*self.raw).pMethods };
        let write = methods.xWrite.unwrap();
        let rc = unsafe {
            write(
                self.raw,
                bytes.as_ptr().cast::<c_void>(),
                i32::try_from(bytes.len()).unwrap(),
                i64::try_from(offset).unwrap(),
            )
        };
        assert_eq!(rc, ffi::SQLITE_OK);
    }

    fn truncate(&mut self, size: u64) {
        let methods = unsafe { &*(*self.raw).pMethods };
        let truncate = methods.xTruncate.unwrap();
        let rc = unsafe { truncate(self.raw, i64::try_from(size).unwrap()) };
        assert_eq!(rc, ffi::SQLITE_OK);
    }

    fn file_size(&mut self) -> u64 {
        let methods = unsafe { &*(*self.raw).pMethods };
        let file_size = methods.xFileSize.unwrap();
        let mut size = 0_i64;
        let rc = unsafe { file_size(self.raw, ptr::addr_of_mut!(size)) };
        assert_eq!(rc, ffi::SQLITE_OK);
        u64::try_from(size).unwrap()
    }
}

impl Drop for VfsMainFile {
    fn drop(&mut self) {
        if self.raw.is_null() {
            return;
        }
        let methods = unsafe { &*(*self.raw).pMethods };
        if let Some(close) = methods.xClose {
            let rc = unsafe { close(self.raw) };
            debug_assert_eq!(rc, ffi::SQLITE_OK);
        }
        let _ = self.storage.len();
        self.raw = ptr::null_mut();
    }
}

fn open_raw(filename: &str, flags: i32) -> Result<RawConnection, String> {
    vfs::register();
    let filename = CString::new(filename).unwrap();
    let vfs = CString::new(ic_sqlite_vfs::config::VFS_NAME).unwrap();
    let mut db = ptr::null_mut();
    let rc = unsafe { ffi::sqlite3_open_v2(filename.as_ptr(), &mut db, flags, vfs.as_ptr()) };
    if rc == ffi::SQLITE_OK {
        return Ok(RawConnection { raw: db });
    }
    let message = unsafe { CStr::from_ptr(ffi::sqlite3_errmsg(db)) }
        .to_string_lossy()
        .into_owned();
    if !db.is_null() {
        unsafe {
            ffi::sqlite3_close(db);
        }
    }
    Err(message)
}

fn exec_raw(connection: &RawConnection, sql: &str) -> Result<(), (i32, String)> {
    let sql = CString::new(sql).unwrap();
    let mut error = ptr::null_mut();
    let rc = unsafe {
        ffi::sqlite3_exec(
            connection.raw,
            sql.as_ptr(),
            None,
            ptr::null_mut(),
            ptr::addr_of_mut!(error),
        )
    };
    if rc == ffi::SQLITE_OK {
        return Ok(());
    }
    let message = if error.is_null() {
        unsafe { CStr::from_ptr(ffi::sqlite3_errmsg(connection.raw)) }
            .to_string_lossy()
            .into_owned()
    } else {
        let value = unsafe { CStr::from_ptr(error) }
            .to_string_lossy()
            .into_owned();
        unsafe {
            ffi::sqlite3_free(error.cast::<c_void>());
        }
        value
    };
    Err((rc, message))
}

fn query_count_raw(connection: &RawConnection, sql: &str) -> i64 {
    let sql = CString::new(sql).unwrap();
    let mut statement = ptr::null_mut();
    let rc = unsafe {
        ffi::sqlite3_prepare_v2(
            connection.raw,
            sql.as_ptr(),
            -1,
            ptr::addr_of_mut!(statement),
            ptr::null_mut(),
        )
    };
    assert_eq!(rc, ffi::SQLITE_OK);
    let step = unsafe { ffi::sqlite3_step(statement) };
    assert_eq!(step, ffi::SQLITE_ROW);
    let value = unsafe { ffi::sqlite3_column_int64(statement, 0) };
    let finalize = unsafe { ffi::sqlite3_finalize(statement) };
    assert_eq!(finalize, ffi::SQLITE_OK);
    value
}

fn reset() {
    memory::reset_for_tests();
    lock::reset_for_tests();
    Db::init(memory::memory_for_tests()).unwrap();
}

#[test]
#[serial]
fn update_and_query_require_explicit_memory_initialization() {
    memory::reset_for_tests();
    lock::reset_for_tests();

    assert!(matches!(
        Db::query(|_| Ok::<_, ic_sqlite_vfs::DbError>(())),
        Err(ic_sqlite_vfs::DbError::StableMemoryNotInitialized)
    ));
    assert!(matches!(
        Db::update(|_| Ok::<_, ic_sqlite_vfs::DbError>(())),
        Err(ic_sqlite_vfs::DbError::StableMemoryNotInitialized)
    ));
}

#[test]
#[serial]
fn db_init_rejects_second_memory_in_same_instance() {
    memory::reset_for_tests();
    lock::reset_for_tests();
    Db::init(memory::memory_for_tests()).unwrap();

    assert!(matches!(
        Db::init(memory::memory_for_tests()),
        Err(ic_sqlite_vfs::DbError::StableMemoryAlreadyInitialized)
    ));
}

#[test]
#[serial]
fn failed_db_init_allows_retry_with_another_memory() {
    let manager = MemoryManager::init(DefaultMemoryImpl::default());

    memory::reset_for_tests();
    lock::reset_for_tests();
    memory::init(manager.get(MemoryId::new(20))).unwrap();
    let mut block = Superblock::fresh();
    block.layout_version = 0;
    block.store().unwrap();

    memory::reset_for_tests();
    let error = Db::init(manager.get(MemoryId::new(20))).unwrap_err();
    assert!(matches!(
        error,
        ic_sqlite_vfs::DbError::Stable(ic_sqlite_vfs::StableMemoryError::UnsupportedLayoutVersion(
            0
        ))
    ));

    Db::init(manager.get(MemoryId::new(21))).unwrap();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE recovered(k TEXT PRIMARY KEY NOT NULL);",
    }])
    .unwrap();
}

#[test]
#[serial]
fn db_init_rejects_v6_page_map_layout_without_auto_migration() {
    let manager = MemoryManager::init(DefaultMemoryImpl::default());

    memory::reset_for_tests();
    lock::reset_for_tests();
    memory::init(manager.get(MemoryId::new(22))).unwrap();
    let mut block = Superblock::fresh();
    block.layout_version = 6;
    block.store().unwrap();

    memory::reset_for_tests();
    lock::reset_for_tests();
    let error = Db::init(manager.get(MemoryId::new(22))).unwrap_err();
    assert!(matches!(
        error,
        ic_sqlite_vfs::DbError::Stable(ic_sqlite_vfs::StableMemoryError::UnsupportedLayoutVersion(
            6
        ))
    ));
}

#[test]
#[serial]
fn db_init_fresh_initializes_empty_virtual_memory() {
    let manager = MemoryManager::init(DefaultMemoryImpl::default());

    memory::reset_for_tests();
    lock::reset_for_tests();
    Db::init(manager.get(MemoryId::new(23))).unwrap();

    let block = Superblock::load().unwrap();
    assert_eq!(
        block.layout_version,
        ic_sqlite_vfs::test_support::meta::CURRENT_LAYOUT_VERSION
    );
}

#[test]
#[serial]
fn db_init_rejects_sqlite_format_image_without_overwriting_bytes() {
    let manager = MemoryManager::init(DefaultMemoryImpl::default());
    let db_memory = manager.get(MemoryId::new(24));
    let header = b"SQLite format 3\0";
    assert_eq!(db_memory.grow(1), 0);
    db_memory.write(0, header);

    memory::reset_for_tests();
    lock::reset_for_tests();
    let error = Db::init(manager.get(MemoryId::new(24))).unwrap_err();
    assert!(matches!(
        error,
        ic_sqlite_vfs::DbError::Stable(ic_sqlite_vfs::StableMemoryError::ForeignStableMemoryImage)
    ));

    let mut preserved = [0_u8; 16];
    db_memory.read(0, &mut preserved);
    assert_eq!(&preserved, header);
}

#[test]
#[serial]
fn db_init_rejects_random_non_empty_memory_without_overwriting_bytes() {
    let manager = MemoryManager::init(DefaultMemoryImpl::default());
    let db_memory = manager.get(MemoryId::new(25));
    let header = *b"not-ic-sqlite!!!";
    assert_eq!(db_memory.grow(1), 0);
    db_memory.write(0, &header);

    memory::reset_for_tests();
    lock::reset_for_tests();
    let error = Db::init(manager.get(MemoryId::new(25))).unwrap_err();
    assert!(matches!(
        error,
        ic_sqlite_vfs::DbError::Stable(ic_sqlite_vfs::StableMemoryError::ForeignStableMemoryImage)
    ));

    let mut preserved = [0_u8; 16];
    db_memory.read(0, &mut preserved);
    assert_eq!(preserved, header);
}

#[test]
#[serial]
fn db_handle_rejects_duplicate_memory_identity() {
    memory::reset_for_tests();
    lock::reset_for_tests();
    let manager = MemoryManager::init(DefaultMemoryImpl::default());
    let first = DbHandle::init(manager.get(MemoryId::new(36))).unwrap();

    let error = DbHandle::init(manager.get(MemoryId::new(36))).unwrap_err();
    assert!(matches!(
        error,
        ic_sqlite_vfs::DbError::Stable(ic_sqlite_vfs::StableMemoryError::MemoryAlreadyRegistered)
    ));
    first.query(|_| Ok::<_, DbError>(())).unwrap();
}

#[test]
#[serial]
fn db_handle_rejects_duplicate_memory_id_across_managers_on_same_backing() {
    memory::reset_for_tests();
    lock::reset_for_tests();
    let backing = DefaultMemoryImpl::default();
    let first_manager = MemoryManager::init(backing.clone());
    let second_manager = MemoryManager::init(backing);
    let first = DbHandle::init(first_manager.get(MemoryId::new(37))).unwrap();

    let error = DbHandle::init(second_manager.get(MemoryId::new(37))).unwrap_err();
    assert!(matches!(
        error,
        ic_sqlite_vfs::DbError::Stable(ic_sqlite_vfs::StableMemoryError::MemoryAlreadyRegistered)
    ));
    first.query(|_| Ok::<_, DbError>(())).unwrap();
}

#[test]
#[serial]
fn db_handle_allows_same_memory_id_on_distinct_backing_memories() {
    memory::reset_for_tests();
    lock::reset_for_tests();
    let first_manager = MemoryManager::init(DefaultMemoryImpl::default());
    let second_manager = MemoryManager::init(DefaultMemoryImpl::default());
    let first = DbHandle::init(first_manager.get(MemoryId::new(38))).unwrap();
    let second = DbHandle::init(second_manager.get(MemoryId::new(38))).unwrap();

    first.query(|_| Ok::<_, DbError>(())).unwrap();
    second.query(|_| Ok::<_, DbError>(())).unwrap();
}

#[test]
#[serial]
fn db_handle_accepts_custom_memory_backend() {
    memory::reset_for_tests();
    lock::reset_for_tests();
    let handle = DbHandle::init(CustomMemory::new(1)).unwrap();

    handle
        .migrate(&[Migration {
            version: 1,
            sql: "CREATE TABLE custom(k TEXT PRIMARY KEY NOT NULL, v TEXT NOT NULL);",
        }])
        .unwrap();
    handle
        .update(|connection| {
            connection.execute("INSERT INTO custom(k, v) VALUES ('k', 'value')", params![])
        })
        .unwrap();

    let value = handle
        .query(|connection| {
            connection.query_scalar::<String>("SELECT v FROM custom WHERE k = 'k'", params![])
        })
        .unwrap();
    assert_eq!(value, "value");
}

#[test]
#[serial]
fn custom_memory_handles_keep_database_images_separate() {
    memory::reset_for_tests();
    lock::reset_for_tests();
    let first = DbHandle::init(CustomMemory::new(2)).unwrap();
    let second = DbHandle::init(CustomMemory::new(3)).unwrap();
    let migrations = [Migration {
        version: 1,
        sql: "CREATE TABLE custom_split(k TEXT PRIMARY KEY NOT NULL, v TEXT NOT NULL);",
    }];

    first.migrate(&migrations).unwrap();
    second.migrate(&migrations).unwrap();
    first
        .update(|connection| {
            connection.execute(
                "INSERT INTO custom_split(k, v) VALUES ('k', 'first')",
                params![],
            )
        })
        .unwrap();
    second
        .update(|connection| {
            connection.execute(
                "INSERT INTO custom_split(k, v) VALUES ('k', 'second')",
                params![],
            )
        })
        .unwrap();

    let first_value = first
        .query(|connection| {
            connection.query_scalar::<String>("SELECT v FROM custom_split WHERE k = 'k'", params![])
        })
        .unwrap();
    let second_value = second
        .query(|connection| {
            connection.query_scalar::<String>("SELECT v FROM custom_split WHERE k = 'k'", params![])
        })
        .unwrap();
    assert_eq!(first_value, "first");
    assert_eq!(second_value, "second");
}

#[test]
#[serial]
fn db_handle_rejects_duplicate_custom_memory_identity() {
    memory::reset_for_tests();
    lock::reset_for_tests();
    let first = DbHandle::init(CustomMemory::new(4)).unwrap();

    let error = DbHandle::init(CustomMemory::new(4)).unwrap_err();
    assert!(matches!(
        error,
        ic_sqlite_vfs::DbError::Stable(ic_sqlite_vfs::StableMemoryError::MemoryAlreadyRegistered)
    ));
    first.query(|_| Ok::<_, DbError>(())).unwrap();
}

#[test]
#[serial]
fn different_memory_ids_keep_database_images_separate() {
    let manager = MemoryManager::init(DefaultMemoryImpl::default());

    memory::reset_for_tests();
    lock::reset_for_tests();
    Db::init(manager.get(MemoryId::new(10))).unwrap();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE split(k TEXT PRIMARY KEY NOT NULL);",
    }])
    .unwrap();
    Db::update(|connection| connection.execute("INSERT INTO split(k) VALUES ('a')", params![]))
        .unwrap();

    memory::reset_for_tests();
    lock::reset_for_tests();
    Db::init(manager.get(MemoryId::new(11))).unwrap();
    let missing = Db::query(|connection| {
        connection.query_optional_scalar::<i64>(
            "SELECT COUNT(*) FROM sqlite_schema WHERE name = 'split'",
            params![],
        )
    })
    .unwrap();
    assert_eq!(missing, Some(0));

    memory::reset_for_tests();
    lock::reset_for_tests();
    Db::init(manager.get(MemoryId::new(10))).unwrap();
    let count = Db::query(|connection| {
        connection.query_scalar::<i64>("SELECT COUNT(*) FROM split", params![])
    })
    .unwrap();
    assert_eq!(count, 1);
}

#[test]
#[serial]
fn db_handles_keep_simultaneous_contexts_separate() {
    memory::reset_for_tests();
    lock::reset_for_tests();
    let manager = MemoryManager::init(DefaultMemoryImpl::default());
    let first = DbHandle::init(manager.get(MemoryId::new(30))).unwrap();
    let second = DbHandle::init(manager.get(MemoryId::new(31))).unwrap();

    let migrations = [Migration {
        version: 1,
        sql: "CREATE TABLE multi(k TEXT PRIMARY KEY NOT NULL, v TEXT NOT NULL);",
    }];
    first.migrate(&migrations).unwrap();
    second.migrate(&migrations).unwrap();

    first
        .update(|connection| {
            connection.execute("INSERT INTO multi(k, v) VALUES ('k', 'first')", params![])
        })
        .unwrap();
    second
        .update(|connection| {
            connection.execute("INSERT INTO multi(k, v) VALUES ('k', 'second')", params![])
        })
        .unwrap();

    let first_value = first
        .query(|connection| {
            connection.query_scalar::<String>("SELECT v FROM multi WHERE k = 'k'", params![])
        })
        .unwrap();
    let second_value = second
        .query(|connection| {
            connection.query_scalar::<String>("SELECT v FROM multi WHERE k = 'k'", params![])
        })
        .unwrap();
    assert_eq!(first_value, "first");
    assert_eq!(second_value, "second");

    first.refresh_checksum().unwrap();
    let second_after_checksum = second
        .query(|connection| {
            connection.query_scalar::<String>("SELECT v FROM multi WHERE k = 'k'", params![])
        })
        .unwrap();
    assert_eq!(second_after_checksum, "second");
}

#[test]
#[serial]
fn db_handle_update_and_checksum_are_scoped_to_handle() {
    memory::reset_for_tests();
    lock::reset_for_tests();
    let manager = MemoryManager::init(DefaultMemoryImpl::default());
    let first = DbHandle::init(manager.get(MemoryId::new(34))).unwrap();
    let second = DbHandle::init(manager.get(MemoryId::new(35))).unwrap();

    let migrations = [Migration {
        version: 1,
        sql: "CREATE TABLE scoped(k TEXT PRIMARY KEY NOT NULL, v TEXT NOT NULL);",
    }];
    first.migrate(&migrations).unwrap();
    second.migrate(&migrations).unwrap();

    first
        .update(|connection| {
            connection.execute("INSERT INTO scoped(k, v) VALUES ('k', 'old')", params![])
        })
        .unwrap();
    second
        .update(|connection| {
            connection.execute("INSERT INTO scoped(k, v) VALUES ('k', 'second')", params![])
        })
        .unwrap();

    first
        .update(|connection| {
            connection.execute("UPDATE scoped SET v = 'new' WHERE k = 'k'", params![])
        })
        .unwrap();
    let first_checksum = first.refresh_checksum().unwrap();
    assert_ne!(first_checksum, 0);

    let first_value = first
        .query(|connection| {
            connection.query_scalar::<String>("SELECT v FROM scoped WHERE k = 'k'", params![])
        })
        .unwrap();
    let second_value = second
        .query(|connection| {
            connection.query_scalar::<String>("SELECT v FROM scoped WHERE k = 'k'", params![])
        })
        .unwrap();

    assert_eq!(first_value, "new");
    assert_eq!(second_value, "second");
}

#[test]
#[serial]
fn persists_rows_after_reopen() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT NOT NULL);",
    }])
    .unwrap();

    Db::update(|connection| {
        connection.execute_batch("INSERT INTO users(name) VALUES ('alice')")?;
        Ok(())
    })
    .unwrap();

    let name = Db::query(|connection| {
        connection.query_scalar::<String>("SELECT name FROM users WHERE id = 1", params![])
    })
    .unwrap();

    assert_eq!(name, "alice");
    assert!(Superblock::load().unwrap().db_size > 0);
}

#[test]
#[serial]
fn reusable_statement_handles_repeated_binds() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE kv(k TEXT PRIMARY KEY, v TEXT NOT NULL);",
    }])
    .unwrap();

    Db::update(|connection| {
        let mut statement = connection.prepare("INSERT INTO kv(k, v) VALUES (?1, ?2)")?;
        for index in 0..16 {
            let key = format!("k{index}");
            let value = format!("v{index}");
            statement.execute(params![key, value])?;
        }
        Ok(())
    })
    .unwrap();

    let joined = Db::query(|connection| {
        let mut statement = connection.prepare("SELECT v FROM kv WHERE k = ?1")?;
        let mut values = Vec::new();
        for index in [0, 7, 15] {
            let key = format!("k{index}");
            values.push(statement.query_optional_string_text(&key)?.unwrap());
        }
        assert!(statement.query_optional_string_text("missing")?.is_none());
        Ok(values.join(","))
    })
    .unwrap();

    assert_eq!(joined, "v0,v7,v15");
}

#[test]
#[serial]
fn text_text_execute_reuses_borrowed_bindings() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE borrowed_text(k TEXT PRIMARY KEY, v TEXT NOT NULL);",
    }])
    .unwrap();

    Db::update(|connection| {
        let mut statement =
            connection.prepare("INSERT INTO borrowed_text(k, v) VALUES (?1, ?2)")?;
        statement.execute_text_text("a", "one")?;
        statement.execute_text_text("b", "two")
    })
    .unwrap();

    let values = Db::query(|connection| {
        connection.query_column::<String>("SELECT v FROM borrowed_text ORDER BY k", params![])
    })
    .unwrap();

    assert_eq!(values, vec!["one", "two"]);
}

#[test]
#[serial]
fn prepared_statement_cache_reuses_sql_with_repeated_binds() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE cached(k TEXT PRIMARY KEY, v TEXT NOT NULL);",
    }])
    .unwrap();

    Db::update(|connection| {
        {
            let mut statement =
                connection.prepare_cached("INSERT INTO cached(k, v) VALUES (?1, ?2)")?;
            statement.execute(params!["a", "one"])?;
        }
        {
            let mut statement =
                connection.prepare_cached("INSERT INTO cached(k, v) VALUES (?1, ?2)")?;
            statement.execute(params!["b", "two"])?;
        }
        Ok(())
    })
    .unwrap();

    let values = Db::query(|connection| {
        connection.query_column::<String>("SELECT v FROM cached ORDER BY k", params![])
    })
    .unwrap();
    assert_eq!(values, vec!["one".to_string(), "two".to_string()]);
}

#[test]
#[serial]
fn blob_boundaries_survive_checksum_refresh() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE boundary_blob(id INTEGER PRIMARY KEY, body BLOB NOT NULL);",
    }])
    .unwrap();

    let page = usize::try_from(SQLITE_PAGE_SIZE).unwrap();
    let blobs = vec![
        deterministic_blob(1, 1),
        deterministic_blob(2, page - 1),
        deterministic_blob(3, page),
        deterministic_blob(4, page + 1),
        deterministic_blob(5, page * 2 + 17),
    ];

    Db::update(|connection| {
        let mut statement =
            connection.prepare("INSERT INTO boundary_blob(id, body) VALUES (?1, ?2)")?;
        for (index, blob) in blobs.iter().enumerate() {
            statement.execute(params![i64::try_from(index + 1).unwrap(), blob.clone()])?;
        }
        Ok(())
    })
    .unwrap();
    assert_boundary_blobs(&blobs);
    Db::refresh_checksum().unwrap();
    assert_boundary_blobs(&blobs);
}

fn assert_boundary_blobs(expected: &[Vec<u8>]) {
    let rows = Db::query(|connection| {
        connection.query_column::<Vec<u8>>("SELECT body FROM boundary_blob ORDER BY id", params![])
    })
    .unwrap();
    assert_eq!(rows, expected);
    assert_eq!(Db::integrity_check().unwrap(), "ok");
}

fn deterministic_blob(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|index| seed.wrapping_add(index as u8).wrapping_mul(17))
        .collect()
}

#[test]
#[serial]
fn vfs_sparse_write_zero_fills_same_partial_page_eof_gap() {
    reset();
    let mut file = VfsMainFile::open();

    file.write(0, b"a");
    file.write(5, b"z");

    let expected = b"a\0\0\0\0z";
    assert_eq!(file.file_size(), 6);
    assert_eq!(file.read(0, 6), expected);
    assert_logical_image_metadata_matches(expected);
}

#[test]
#[serial]
fn vfs_sparse_write_zero_fills_cross_page_eof_gap() {
    reset();
    let page = u64::from(SQLITE_PAGE_SIZE);
    let old_size = page + 17;
    let write_offset = page * 2 + 33;
    let gap_len = usize::try_from(write_offset - old_size).unwrap();
    let expected_len = usize::try_from(write_offset + 1).unwrap();
    let mut file = VfsMainFile::open();

    file.write(old_size - 1, b"a");
    file.write(write_offset, b"z");

    let mut expected = vec![0_u8; expected_len];
    expected[usize::try_from(old_size - 1).unwrap()] = b'a';
    expected[usize::try_from(write_offset).unwrap()] = b'z';

    assert_eq!(file.file_size(), write_offset + 1);
    assert!(file.read(old_size, gap_len).iter().all(|byte| *byte == 0));
    assert_eq!(file.read(write_offset, 1), b"z");
    assert_logical_image_metadata_matches(&expected);
}

#[test]
#[serial]
fn vfs_truncate_reextend_keeps_stale_bytes_hidden_after_checksum() {
    reset();
    let page = u64::from(SQLITE_PAGE_SIZE);
    let write_offset = page + 12;
    let expected_size = write_offset + 1;
    let mut file = VfsMainFile::open();

    file.write(0, b"a");
    file.write(page + 1, b"stale");
    file.truncate(1);
    file.write(write_offset, b"z");

    let before_import = file.read(0, usize::try_from(expected_size).unwrap());
    assert_eq!(before_import[0], b'a');
    assert!(before_import[1..usize::try_from(write_offset).unwrap()]
        .iter()
        .all(|byte| *byte == 0));
    assert_eq!(before_import[usize::try_from(write_offset).unwrap()], b'z');
    assert_logical_image_metadata_matches(&before_import);

    drop(file);
    Db::refresh_checksum().unwrap();

    let mut reopened = VfsMainFile::open();
    assert_eq!(reopened.file_size(), expected_size);
    assert_eq!(
        reopened.read(0, usize::try_from(expected_size).unwrap()),
        before_import
    );
    assert_logical_image_metadata_matches(&before_import);
}

fn assert_logical_image_metadata_matches(expected: &[u8]) {
    let db_size = Superblock::load().unwrap().db_size;
    assert_eq!(db_size, u64::try_from(expected.len()).unwrap());
    assert_eq!(
        Db::db_checksum().unwrap(),
        ic_sqlite_vfs::test_support::meta::fnv1a64(expected)
    );
}

#[test]
#[serial]
fn query_connection_rejects_writes() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE items(id INTEGER PRIMARY KEY);",
    }])
    .unwrap();

    let result = Db::query(|connection| {
        connection.execute_batch("INSERT INTO items DEFAULT VALUES")?;
        Ok(())
    });

    assert!(result.is_err());
}

#[test]
#[serial]
fn read_only_connection_applies_cache_size_pragma() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE cache_size_guard(id INTEGER PRIMARY KEY);",
    }])
    .unwrap();

    let cache_size = Db::query(|connection| {
        connection.query_scalar::<i64>("PRAGMA cache_size", ic_sqlite_vfs::params![])
    })
    .unwrap();

    assert_eq!(cache_size, -i64::from(SQLITE_CACHE_SIZE_KIB));
}

#[test]
#[serial]
fn repeated_existing_page_updates_do_not_grow_allocated_bytes() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE growth(k TEXT PRIMARY KEY NOT NULL, v TEXT NOT NULL);",
    }])
    .unwrap();

    Db::update(|connection| {
        let mut statement = connection.prepare("INSERT INTO growth(k, v) VALUES (?1, ?2)")?;
        for index in 0..5_000 {
            let key = format!("k{index:08}");
            let value = format!("value-{index:08}");
            statement.execute(params![key, value])?;
        }
        Ok(())
    })
    .unwrap();

    let before = Superblock::load().unwrap();
    let before_stats = stable_blob::storage_stats().unwrap();
    for round in 0..128 {
        let value = format!("updated-{round:04}");
        Db::update(|connection| {
            connection.execute(
                "UPDATE growth SET v = ?1 WHERE k = ?2",
                params![value, "k00000042"],
            )
        })
        .unwrap();
    }
    let after = Superblock::load().unwrap();
    let after_stats = stable_blob::storage_stats().unwrap();

    assert!(before.db_size > ic_sqlite_vfs::config::STABLE_PAGE_SIZE);
    assert_eq!(after.db_size, before.db_size);
    assert_eq!(after.page_count, before.page_count);
    assert_eq!(after.db_base_offset, before.db_base_offset);
    assert_eq!(after.page_table_offset, 0);
    assert_eq!(after_stats.page_table_bytes, 0);
    assert_eq!(after_stats.allocated_bytes, before_stats.allocated_bytes);
    assert_eq!(
        after_stats.orphan_bytes_estimate,
        before_stats.orphan_bytes_estimate
    );
    assert_eq!(
        memory::size_pages(),
        before_stats.allocated_bytes / ic_sqlite_vfs::config::STABLE_PAGE_SIZE
    );
}

#[test]
#[serial]
fn grow_truncate_reupdate_keeps_in_place_resource_shape() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE resource_shape(k INTEGER PRIMARY KEY, v BLOB NOT NULL);",
    }])
    .unwrap();
    Db::update(|connection| {
        connection.execute(
            "INSERT INTO resource_shape(k, v) VALUES (1, zeroblob(?1))",
            params![i64::from(SQLITE_PAGE_SIZE) * 12],
        )
    })
    .unwrap();

    let initial = Superblock::load().unwrap();
    Db::update(|connection| {
        connection.execute(
            "UPDATE resource_shape SET v = zeroblob(?1) WHERE k = 1",
            params![1],
        )
    })
    .unwrap();
    Db::update(|connection| {
        connection.execute(
            "UPDATE resource_shape SET v = zeroblob(?1) WHERE k = 1",
            params![i64::from(SQLITE_PAGE_SIZE) * 12],
        )
    })
    .unwrap();

    let before_update = Superblock::load().unwrap();
    let before_stats = stable_blob::storage_stats().unwrap();
    Db::update(|connection| {
        connection.execute(
            "UPDATE resource_shape SET v = ?1 WHERE k = 1",
            params![vec![7_u8; 256]],
        )
    })
    .unwrap();
    let after = Superblock::load().unwrap();
    let after_stats = stable_blob::storage_stats().unwrap();

    assert_eq!(before_update.db_base_offset, initial.db_base_offset);
    assert_eq!(after.db_size, before_update.db_size);
    assert_eq!(after.page_count, before_update.page_count);
    assert_eq!(after.db_base_offset, initial.db_base_offset);
    assert_eq!(after.page_table_offset, 0);
    assert_eq!(after_stats.page_table_bytes, 0);
    assert_eq!(after_stats.allocated_bytes, before_stats.allocated_bytes);
    assert_eq!(
        after_stats.orphan_bytes_estimate,
        before_stats.orphan_bytes_estimate
    );
}

#[test]
#[serial]
fn cached_read_connection_observes_update_path() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE cache_guard(k TEXT PRIMARY KEY NOT NULL, v TEXT NOT NULL);",
    }])
    .unwrap();
    Db::update(|connection| {
        connection.execute_batch("INSERT INTO cache_guard(k, v) VALUES ('key', 'before')")
    })
    .unwrap();

    let before = Db::query(|connection| {
        connection.query_scalar::<String>("SELECT v FROM cache_guard WHERE k = 'key'", params![])
    })
    .unwrap();
    assert_eq!(before, "before");

    Db::update(|connection| {
        connection.execute(
            "UPDATE cache_guard SET v = ?1 WHERE k = 'key'",
            params!["after"],
        )
    })
    .unwrap();
    let after_update = Db::query(|connection| {
        connection.query_scalar::<String>("SELECT v FROM cache_guard WHERE k = 'key'", params![])
    })
    .unwrap();
    assert_eq!(after_update, "after");
}

#[test]
#[serial]
fn query_closure_rejects_mutation_without_panic_or_stale_clear() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE reentrant_clear(k TEXT PRIMARY KEY NOT NULL, v TEXT NOT NULL);",
    }])
    .unwrap();
    Db::update(|connection| {
        connection.execute_batch("INSERT INTO reentrant_clear(k, v) VALUES ('key', 'before')")
    })
    .unwrap();

    let (before, update_blocked, still_before) = Db::query(|connection| {
        let before = connection
            .query_scalar::<String>("SELECT v FROM reentrant_clear WHERE k = 'key'", params![])?;
        let update_blocked = matches!(
            Db::update(|connection| {
                connection.execute(
                    "UPDATE reentrant_clear SET v = ?1 WHERE k = 'key'",
                    params!["after"],
                )
            }),
            Err(DbError::ReadConnectionInUse)
        );
        let still_before = connection
            .query_scalar::<String>("SELECT v FROM reentrant_clear WHERE k = 'key'", params![])?;
        Ok((before, update_blocked, still_before))
    })
    .unwrap();
    Db::update(|connection| {
        connection.execute(
            "UPDATE reentrant_clear SET v = ?1 WHERE k = 'key'",
            params!["after"],
        )
    })
    .unwrap();
    let after = Db::query(|connection| {
        connection
            .query_scalar::<String>("SELECT v FROM reentrant_clear WHERE k = 'key'", params![])
    })
    .unwrap();

    assert_eq!(before, "before");
    assert!(update_blocked);
    assert_eq!(still_before, "before");
    assert_eq!(after, "after");
}

#[test]
#[serial]
fn query_closure_rejects_checksum_refresh_without_panic() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE reentrant_checksum(k TEXT PRIMARY KEY NOT NULL, v TEXT NOT NULL);",
    }])
    .unwrap();
    Db::update(|connection| {
        connection.execute_batch("INSERT INTO reentrant_checksum(k, v) VALUES ('key', 'value')")
    })
    .unwrap();

    let blocked = Db::query(|connection| {
        let value = connection.query_scalar::<String>(
            "SELECT v FROM reentrant_checksum WHERE k = 'key'",
            params![],
        )?;
        Ok((
            value,
            matches!(Db::refresh_checksum(), Err(DbError::ReadConnectionInUse)),
        ))
    })
    .unwrap();

    assert_eq!(blocked, ("value".to_string(), true));
}

#[test]
#[serial]
fn cached_read_connection_sees_committed_update() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE read_connection_guard(k TEXT PRIMARY KEY NOT NULL, v TEXT NOT NULL);",
    }])
    .unwrap();
    Db::update(|connection| {
        connection.execute_batch("INSERT INTO read_connection_guard(k, v) VALUES ('key', 'before')")
    })
    .unwrap();

    let before = Db::query(|connection| {
        connection.query_scalar::<String>(
            "SELECT v FROM read_connection_guard WHERE k = 'key'",
            params![],
        )
    })
    .unwrap();
    assert_eq!(before, "before");

    Db::update(|connection| {
        connection.execute(
            "UPDATE read_connection_guard SET v = ?1 WHERE k = 'key'",
            params!["after"],
        )
    })
    .unwrap();
    let after = Db::query(|connection| {
        connection.query_scalar::<String>(
            "SELECT v FROM read_connection_guard WHERE k = 'key'",
            params![],
        )
    })
    .unwrap();

    assert_eq!(after, "after");
}

#[test]
#[serial]
fn cached_read_connection_survives_failed_update() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE failed_update_guard(k TEXT PRIMARY KEY NOT NULL, v TEXT NOT NULL);",
    }])
    .unwrap();
    Db::update(|connection| {
        connection.execute_batch("INSERT INTO failed_update_guard(k, v) VALUES ('key', 'before')")
    })
    .unwrap();

    let before = Db::query(|connection| {
        connection.query_scalar::<String>(
            "SELECT v FROM failed_update_guard WHERE k = 'key'",
            params![],
        )
    })
    .unwrap();
    assert_eq!(before, "before");

    let result = Db::update(|connection| {
        connection.execute(
            "UPDATE failed_update_guard SET v = ?1 WHERE k = 'key'",
            params!["after"],
        )?;
        connection.execute_batch("INSERT INTO missing_table(value) VALUES (1)")?;
        Ok(())
    });
    assert!(result.is_err());

    let after = Db::query(|connection| {
        connection.query_scalar::<String>(
            "SELECT v FROM failed_update_guard WHERE k = 'key'",
            params![],
        )
    })
    .unwrap();
    assert_eq!(after, "before");
}

#[test]
#[serial]
fn failed_update_rolls_back_transaction() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE logs(id INTEGER PRIMARY KEY, body TEXT NOT NULL);",
    }])
    .unwrap();

    let result = Db::update(|connection| {
        connection.execute_batch("INSERT INTO logs(body) VALUES ('before-error')")?;
        connection.execute_batch("INSERT INTO missing_table(value) VALUES (1)")?;
        Ok(())
    });

    assert!(result.is_err());
    let count = Db::query(|connection| {
        connection.query_scalar::<i64>("SELECT COUNT(*) FROM logs", params![])
    })
    .unwrap();
    assert_eq!(count, 0);
}

#[test]
#[serial]
fn integrity_check_reports_ok() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE checks(id INTEGER PRIMARY KEY);",
    }])
    .unwrap();

    assert_eq!(Db::integrity_check().unwrap(), "ok");
}

#[test]
#[serial]
fn attached_path_containing_vfs_name_stays_separate() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE main_table(id INTEGER PRIMARY KEY);",
    }])
    .unwrap();

    Db::update(|connection| {
        connection.execute_batch(
            "ATTACH DATABASE '/tmp/not-icstable-aux.db' AS aux;
             CREATE TABLE aux.attached_only(id INTEGER PRIMARY KEY);",
        )
    })
    .unwrap();
    let exists = Db::query(|connection| {
        connection.query_scalar::<i64>(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'attached_only'",
            params![],
        )
    })
    .unwrap();

    assert_eq!(exists, 0);
}

#[test]
#[serial]
fn sqlite_uri_mode_ro_opens_main_db_read_only() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE raw_ro(k TEXT PRIMARY KEY NOT NULL);",
    }])
    .unwrap();
    Db::update(|connection| connection.execute_batch("INSERT INTO raw_ro(k) VALUES ('key')"))
        .unwrap();

    let raw = open_raw(
        "file:/main.db?mode=ro",
        ffi::SQLITE_OPEN_READONLY | ffi::SQLITE_OPEN_URI | ffi::SQLITE_OPEN_NOMUTEX,
    )
    .unwrap();
    let count = query_count_raw(&raw, "SELECT COUNT(*) FROM raw_ro");
    let write = exec_raw(&raw, "INSERT INTO raw_ro(k) VALUES ('blocked')");

    assert_eq!(count, 1);
    assert!(matches!(write, Err((code, _)) if code == ffi::SQLITE_READONLY));
}

#[test]
#[serial]
fn wal_journal_mode_is_rejected_by_sqlite_path() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE wal_guard(id INTEGER PRIMARY KEY);",
    }])
    .unwrap();

    let result = Db::update(|connection| {
        connection.query_scalar::<String>("PRAGMA journal_mode=WAL", params![])
    });

    match result {
        Ok(mode) => assert_ne!(mode.to_ascii_lowercase(), "wal"),
        Err(error) => {
            assert!(error.to_string().contains("WAL") || error.to_string().contains("wal"))
        }
    }
}
