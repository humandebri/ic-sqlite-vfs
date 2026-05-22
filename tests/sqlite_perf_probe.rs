//! SQLite VFS性能プローブ。
//!
//! 通常CIからは除外し、DBサイズ増加時の更新・読取・checksum傾向を手元で確認する。

use ic_sqlite_vfs::db::migrate::Migration;
#[cfg(debug_assertions)]
use ic_sqlite_vfs::read_metrics;
use ic_sqlite_vfs::sqlite_vfs::{lock, stable_blob};
use ic_sqlite_vfs::stable::memory;
use ic_sqlite_vfs::stable::meta::Superblock;
use ic_sqlite_vfs::{params, Db};
use serial_test::serial;
use std::time::Instant;

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    sql: "CREATE TABLE bench(
        k TEXT PRIMARY KEY NOT NULL,
        v TEXT NOT NULL
    );",
}];

fn reset() {
    stable_blob::invalidate_read_cache();
    memory::reset_for_tests();
    lock::reset_for_tests();
    Db::init(memory::memory_for_tests()).unwrap();
    Db::migrate(MIGRATIONS).unwrap();
}

fn meta() -> (u64, u64, u64) {
    let block = Superblock::load().unwrap();
    (block.db_size, block.page_count, memory::size_pages())
}

fn print_metric(name: &str, rows: u64, elapsed_ms: u128, db_size: u64, pages: u64) {
    println!("{name}, rows={rows}, elapsed_ms={elapsed_ms}, db_size={db_size}, pages={pages}");
}

#[cfg(debug_assertions)]
fn reset_read_metrics() {
    read_metrics::reset_read_metrics();
}

#[cfg(not(debug_assertions))]
fn reset_read_metrics() {}

#[cfg(debug_assertions)]
fn print_read_metric(name: &str, rows: u64, elapsed_ms: u128, db_size: u64, pages: u64) {
    let metrics = read_metrics::read_metrics_snapshot();
    println!(
        "{name}, rows={rows}, elapsed_ms={elapsed_ms}, db_size={db_size}, pages={pages}, \
         x_read_calls={}, x_read_bytes={}, stable_data_read_calls={}, \
         stable_data_read_bytes={}, page_table_root_hits={}, page_table_root_misses={}, \
         page_table_segment_hits={}, page_table_segment_misses={}, superblock_loads={}",
        metrics.x_read_calls,
        metrics.x_read_bytes,
        metrics.stable_data_read_calls,
        metrics.stable_data_read_bytes,
        metrics.page_table_root_hits,
        metrics.page_table_root_misses,
        metrics.page_table_segment_hits,
        metrics.page_table_segment_misses,
        metrics.superblock_loads
    );
}

#[cfg(not(debug_assertions))]
fn print_read_metric(name: &str, rows: u64, elapsed_ms: u128, db_size: u64, pages: u64) {
    print_metric(name, rows, elapsed_ms, db_size, pages);
}

#[test]
#[ignore]
#[serial]
fn batch_insert_update_and_checksum_scale() {
    for rows in [100_u64, 1_000, 10_000, 20_000, 100_000] {
        reset();

        let start = Instant::now();
        Db::update(|connection| {
            let mut statement = connection.prepare("INSERT INTO bench(k, v) VALUES (?1, ?2)")?;
            for index in 0..rows {
                let key = format!("k{index:08}");
                let value = format!("value-{index:08}-stable-vfs");
                statement.execute_text_text(&key, &value)?;
            }
            Ok(())
        })
        .unwrap();
        let (db_size, _page_count, pages) = meta();
        print_metric(
            "batch_insert",
            rows,
            start.elapsed().as_millis(),
            db_size,
            pages,
        );

        let start = Instant::now();
        Db::update(|connection| {
            connection.execute(
                "INSERT INTO bench(k, v) VALUES (?1, ?2)
                 ON CONFLICT(k) DO UPDATE SET v = excluded.v",
                params!["single-row", "updated-on-large-db"],
            )
        })
        .unwrap();
        let (db_size, _page_count, pages) = meta();
        print_metric(
            "single_update_after_insert",
            rows,
            start.elapsed().as_millis(),
            db_size,
            pages,
        );

        let start = Instant::now();
        let checksum = Db::refresh_checksum().unwrap();
        let (db_size, _page_count, pages) = meta();
        print_metric(
            "refresh_checksum",
            rows,
            start.elapsed().as_millis(),
            db_size,
            pages,
        );
        assert_ne!(checksum, 0);
    }
}

#[test]
#[ignore]
#[serial]
fn indexed_read_scan_and_export_scale() {
    reset();
    let rows = 20_000_u64;
    Db::update(|connection| {
        let mut statement = connection.prepare("INSERT INTO bench(k, v) VALUES (?1, ?2)")?;
        for index in 0..rows {
            let key = format!("k{index:08}");
            let value = format!("value-{index:08}-stable-vfs");
            statement.execute_text_text(&key, &value)?;
        }
        Ok(())
    })
    .unwrap();

    reset_read_metrics();
    let start = Instant::now();
    let mut found = 0_u64;
    Db::query(|connection| {
        let mut statement = connection.prepare("SELECT v FROM bench WHERE k = ?1")?;
        for index in 0..rows {
            let key = format!("k{index:08}");
            if statement.query_optional_string_text(&key)?.is_some() {
                found += 1;
            }
        }
        Ok(())
    })
    .unwrap();
    let (db_size, _page_count, pages) = meta();
    print_read_metric(
        "indexed_point_reads",
        rows,
        start.elapsed().as_millis(),
        db_size,
        pages,
    );
    assert_eq!(found, rows);

    reset_read_metrics();
    let start = Instant::now();
    let count = Db::query(|connection| {
        connection.query_scalar::<i64>(
            "SELECT COUNT(*) FROM bench WHERE v LIKE '%stable%'",
            params![],
        )
    })
    .unwrap();
    let (db_size, _page_count, pages) = meta();
    print_read_metric(
        "full_scan_like",
        rows,
        start.elapsed().as_millis(),
        db_size,
        pages,
    );
    assert_eq!(count, i64::try_from(rows).unwrap());

    reset_read_metrics();
    let start = Instant::now();
    let exported = Db::export_chunk(0, db_size).unwrap();
    print_read_metric(
        "export_full_image",
        rows,
        start.elapsed().as_millis(),
        db_size,
        pages,
    );
    assert_eq!(exported.len(), usize::try_from(db_size).unwrap());
}
