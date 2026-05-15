//! Test-only SQLite feature probe for the precompiled canister build.
//!
//! PocketIC runs this through the `canister-api-test-failpoints` wasm, which
//! links the same `sqlite-precompiled` archive as the production canister build.

use crate::Db;

pub fn run() -> Result<(), String> {
    let report = Db::update(|connection| {
        connection.execute_batch(
            "DROP TABLE IF EXISTS db_test_fts;
             CREATE VIRTUAL TABLE db_test_fts USING fts5(title, body);",
        )?;
        connection.execute(
            "INSERT INTO db_test_fts(title, body) VALUES (?1, ?2)",
            crate::params!["alpha", "stable memory backed sqlite"],
        )?;
        connection.execute(
            "INSERT INTO db_test_fts(title, body) VALUES (?1, ?2)",
            crate::params!["beta", "ordinary substring path search"],
        )?;

        let fts_title = connection.query_scalar::<String>(
            "SELECT title FROM db_test_fts WHERE db_test_fts MATCH ?1 ORDER BY rank LIMIT 1",
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
        Ok(SqliteFeatureProbe {
            fts_title,
            date,
            time,
            unix_epoch,
            year_month,
            json_extract,
            json_each,
            jsonb_extract,
        })
    })
    .map_err(|error| error.to_string())?;

    report.validate()
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
    fn validate(self) -> Result<(), String> {
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
            return Err(format!(
                "json_each expected [10, 20], got {:?}",
                self.json_each
            ));
        }
        expect_text("jsonb_extract", &self.jsonb_extract, "v")
    }
}

fn expect_text(label: &str, actual: &str, expected: &str) -> Result<(), String> {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{label} expected {expected}, got {actual}"))
    }
}

fn expect_i64(label: &str, actual: i64, expected: i64) -> Result<(), String> {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{label} expected {expected}, got {actual}"))
    }
}
