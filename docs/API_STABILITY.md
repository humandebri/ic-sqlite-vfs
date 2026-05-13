# API Stability

`ic-sqlite-vfs` starts at `0.1.0`.

## Stability Contract

The project has not promised compatibility for deployed canisters yet.

For all `0.x` releases, breaking changes may include:

- stable memory superblock fields
- DB image region layout
- `Db` facade signatures
- migration API
- import/export API
- checksum meaning and algorithm
- compile-time SQLite flags

Patch releases should remain bug-fix only when practical. `0.1.2` is an
exception: it includes a small breaking facade cleanup before the crate has a
stable API promise. Production users should still pin exact versions.

## Release Notes

`0.1.2` adds `params!`, `named_params!`, scalar query helpers, and column query
helpers. It removes ad-hoc string/integer helpers such as `execute_with_texts`,
`query_i64`, `query_string`, and `query_optional_string_with_text`.

## Upgrade Contract

Canister upgrades are tested for the same crate version.

Cross-version upgrade compatibility is not guaranteed in `0.x`. If a release
changes the stable layout, migrate with export/import:

1. deploy the old version
2. run `db_integrity_check`
3. export the full DB image with `db_export_chunk`
4. deploy the new version to a fresh canister or controlled upgrade path
5. import with `db_begin_import`, `db_import_chunk`, `db_finish_import`
6. run `db_integrity_check`

## Current Layout

`0.1.2` uses:

```text
offset 0..64KiB      superblock
offset 64KiB..       immutable SQLite pages and page tables
```

The superblock stores the active page table offset, logical page count, and
logical size. The SQLite database image itself is still portable through the
chunked export API.

In `0.1.2`, `checksum` is last verified checksum metadata. It is not a
durability boundary. Update commits use a heap write overlay, publish dirty
pages and a new page table through the superblock, advance `last_tx_id`, and may set
`checksum_stale`. `db_refresh_checksum` and `db_refresh_checksum_chunk` are the
only operations that persistently update the stored checksum after a normal
commit.

## Road To Stable

The `1.0` line requires:

- frozen superblock format
- documented migration path for layout changes
- stable `Db` facade
- stable import/export checksum format
- build setup that does not require copying repository support files
