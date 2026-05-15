//! Test-only SQLite feature probe for the precompiled canister build.
//!
//! PocketIC runs this through the `canister-api-test-failpoints` wasm, which
//! links the same `sqlite-precompiled` archive as the production canister build.

use crate::{Db, DbError};

pub fn run() -> Result<(), String> {
    Db::update(|connection| {
        connection
            .execute_batch("CREATE VIRTUAL TABLE temp.db_test_fts USING fts5(title, body);")?;
        connection.execute(
            "INSERT INTO temp.db_test_fts(title, body) VALUES (?1, ?2)",
            crate::params!["alpha", "stable memory backed sqlite"],
        )?;
        connection.execute(
            "INSERT INTO temp.db_test_fts(title, body) VALUES (?1, ?2)",
            crate::params!["beta", "ordinary substring path search"],
        )?;

        let fts_title = connection.query_scalar::<String>(
            "SELECT title FROM temp.db_test_fts WHERE db_test_fts MATCH ?1 ORDER BY rank LIMIT 1",
            crate::params!["sqlite"],
        )?;
        let date = connection
            .query_scalar::<String>("SELECT date('2026-05-15 12:34:56')", crate::params![])?;
        let time = connection
            .query_scalar::<String>("SELECT time('2026-05-15 12:34:56')", crate::params![])?;
        let unix_epoch = connection
            .query_scalar::<String>("SELECT datetime(0, 'unixepoch')", crate::params![])?;
        let year_month = connection
            .query_scalar::<String>("SELECT strftime('%Y-%m', '2026-05-15')", crate::params![])?;
        let json_extract = connection.query_scalar::<i64>(
            "SELECT json_extract('{\"a\":{\"b\":2}}', '$.a.b')",
            crate::params![],
        )?;
        let json_each = connection.query_column::<i64>(
            "SELECT value FROM json_each('[10,20]') ORDER BY key",
            crate::params![],
        )?;
        let jsonb_extract = connection.query_scalar::<String>(
            "SELECT jsonb_extract(jsonb('{\"k\":\"v\"}'), '$.k')",
            crate::params![],
        )?;
        SqliteFeatureProbe {
            fts_title,
            date,
            time,
            unix_epoch,
            year_month,
            json_extract,
            json_each,
            jsonb_extract,
        }
        .validate()
    })
    .map_err(|error| error.to_string())
}

struct SqliteFeatureProbe {
    fts_title: String,
    date: String,
    time: String,
    unix_epoch: String,
    year_month: String,
    json_extract: i64,
    json_each: Vec<i64>,
    jsonb_extract: String,
}

impl SqliteFeatureProbe {
    fn validate(self) -> Result<(), DbError> {
        expect_text("fts5 MATCH title", &self.fts_title, "alpha")?;
        expect_text("date()", &self.date, "2026-05-15")?;
        expect_text("time()", &self.time, "12:34:56")?;
        expect_text(
            "datetime unixepoch",
            &self.unix_epoch,
            "1970-01-01 00:00:00",
        )?;
        expect_text("strftime", &self.year_month, "2026-05")?;
        expect_i64("json_extract", self.json_extract, 2)?;
        if self.json_each.as_slice() != [10, 20] {
            return Err(feature_probe_error(format!(
                "json_each expected [10, 20], got {:?}",
                self.json_each
            )));
        }
        expect_text("jsonb_extract", &self.jsonb_extract, "v")
    }
}

fn expect_text(label: &str, actual: &str, expected: &str) -> Result<(), DbError> {
    if actual == expected {
        Ok(())
    } else {
        Err(feature_probe_error(format!(
            "{label} expected {expected}, got {actual}"
        )))
    }
}

fn expect_i64(label: &str, actual: i64, expected: i64) -> Result<(), DbError> {
    if actual == expected {
        Ok(())
    } else {
        Err(feature_probe_error(format!(
            "{label} expected {expected}, got {actual}"
        )))
    }
}

fn feature_probe_error(message: String) -> DbError {
    DbError::Constraint(format!("SQLite feature probe failed: {message}"))
}

#[cfg(test)]
mod tests {
    use super::run;
    use crate::sqlite_vfs::{lock, stable_blob};
    use crate::stable::memory;
    use crate::{params, Db};
    use serial_test::serial;

    fn reset() {
        stable_blob::invalidate_read_cache();
        memory::reset_for_tests();
        lock::reset_for_tests();
        Db::init(memory::memory_for_tests()).unwrap();
    }

    #[test]
    #[serial]
    fn feature_probe_does_not_leave_main_schema_tables() {
        reset();

        run().unwrap();

        let main_tables = Db::query(|connection| {
            connection.query_scalar::<i64>(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name LIKE 'db_test_fts%'",
                params![],
            )
        })
        .unwrap();
        assert_eq!(main_tables, 0);
    }

    #[test]
    #[serial]
    fn feature_probe_preserves_existing_main_table_name() {
        reset();

        Db::update(|connection| {
            connection.execute_batch(
                "CREATE TABLE db_test_fts(sentinel TEXT NOT NULL);
                 INSERT INTO db_test_fts(sentinel) VALUES ('kept');",
            )
        })
        .unwrap();

        run().unwrap();

        let sentinel = Db::query(|connection| {
            connection.query_scalar::<String>("SELECT sentinel FROM db_test_fts", params![])
        })
        .unwrap();
        assert_eq!(sentinel, "kept");
    }
}
