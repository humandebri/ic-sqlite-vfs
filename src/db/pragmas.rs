//! Required SQLite PRAGMA setup.
//!
//! These settings keep rollback journal and temp storage in heap memory while the
//! database image itself remains the only stable-memory payload.

use crate::db::connection::Connection;
use crate::db::DbError;

pub fn apply_read_write(connection: &Connection, apply_page_size: bool) -> Result<(), DbError> {
    if apply_page_size {
        connection.execute_batch_nul_terminated(READ_WRITE_SQL)?;
    } else {
        connection.execute_batch_nul_terminated(READ_WRITE_EXISTING_SQL)?;
    }
    Ok(())
}

pub fn apply_read_only(connection: &Connection) -> Result<(), DbError> {
    connection.execute_batch_nul_terminated(READ_ONLY_SQL)?;
    Ok(())
}

const READ_WRITE_SQL: &[u8] = b"PRAGMA page_size = 16384;
PRAGMA journal_mode = MEMORY;
PRAGMA synchronous = OFF;
PRAGMA temp_store = MEMORY;
PRAGMA locking_mode = EXCLUSIVE;
PRAGMA foreign_keys = ON;
PRAGMA cache_size = -32768;\0";

const READ_WRITE_EXISTING_SQL: &[u8] = b"PRAGMA journal_mode = MEMORY;
PRAGMA synchronous = OFF;
PRAGMA temp_store = MEMORY;
PRAGMA locking_mode = EXCLUSIVE;
PRAGMA foreign_keys = ON;
PRAGMA cache_size = -32768;\0";

const READ_ONLY_SQL: &[u8] = b"PRAGMA cache_size = -32768;
PRAGMA query_only = ON;
PRAGMA locking_mode = EXCLUSIVE;
PRAGMA foreign_keys = ON;
PRAGMA temp_store = MEMORY;\0";

#[cfg(test)]
mod tests {
    use super::{READ_ONLY_SQL, READ_WRITE_EXISTING_SQL, READ_WRITE_SQL};
    use crate::config::{SQLITE_CACHE_SIZE_KIB, SQLITE_PAGE_SIZE};

    #[test]
    fn static_pragma_sql_matches_config_values() {
        let read_write = std::str::from_utf8(READ_WRITE_SQL)
            .unwrap()
            .trim_end_matches('\0');
        let read_only = std::str::from_utf8(READ_ONLY_SQL)
            .unwrap()
            .trim_end_matches('\0');

        assert!(read_write.contains(&format!("page_size = {SQLITE_PAGE_SIZE}")));
        assert!(read_write.contains(&format!("cache_size = -{SQLITE_CACHE_SIZE_KIB}")));
        assert!(!std::str::from_utf8(READ_WRITE_EXISTING_SQL)
            .unwrap()
            .contains("page_size"));
        assert!(std::str::from_utf8(READ_WRITE_EXISTING_SQL)
            .unwrap()
            .contains(&format!("cache_size = -{SQLITE_CACHE_SIZE_KIB}")));
        assert!(read_only.contains(&format!("cache_size = -{SQLITE_CACHE_SIZE_KIB}")));
        assert!(read_only.contains("locking_mode = EXCLUSIVE"));
        assert_eq!(READ_WRITE_SQL.last(), Some(&0));
        assert_eq!(READ_WRITE_EXISTING_SQL.last(), Some(&0));
        assert_eq!(READ_ONLY_SQL.last(), Some(&0));
    }
}
