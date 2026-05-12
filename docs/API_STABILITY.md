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
- checksum algorithm
- compile-time SQLite flags

Patch releases should remain bug-fix only when practical. Production users
should still pin exact versions.

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

`0.1.0` uses:

```text
offset 0..64KiB      superblock
offset 64KiB..       SQLite database image
```

The SQLite database image itself is portable through the chunked export API.

## Road To Stable

The `1.0` line requires:

- frozen superblock format
- documented migration path for layout changes
- stable `Db` facade
- stable import/export checksum format
- build setup that does not require copying repository support files
