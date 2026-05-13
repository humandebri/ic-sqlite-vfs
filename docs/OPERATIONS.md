# Operations

## Transaction Rule

公開 update API は同期関数だけで実装する。`Db::update` は `FnOnce(&mut UpdateConnection<'_>) -> Result<T, DbError>` だけを受け取り、Future を受け取らない。transaction 中の `await` は禁止。

CI では `scripts/check-no-await.sh` で `src` 内の `.await` と `async fn` を拒否する。

SQLite transaction 中の `xWrite` と `xTruncate` は stable memory へ直書きしない。heap overlay にpage単位で蓄積し、SQLite `COMMIT` 成功後にdirty pageと新page tableを追記する。最後にsuperblockのactive page table offsetを更新する。通常の `Err`、SQL rollback、panic はactive page tableを変更しない。

## Query Policy

このライブラリは任意 SQL の index 利用や query complexity を判定しない。保証範囲は同期 transaction、`await` 禁止の検出、query connection の read-only/query-only、管理 checksum/import/export の整合性まで。

利用 canister は業務 query ごとに `WHERE`、index、`LIMIT`、pagination、最大入力長を設計する。reference canister は任意 SQL endpoint を公開しない。

危険扱いにする SQL:

- full scan
- 巨大 result set
- `LIKE '%foo%'`
- join 多用
- unbounded `ORDER BY`
- 巨大 `BLOB`
- primary key/index なし filter

1 call の cycles/instruction 制限を超える大量 row 取得は trap し得る。公開 API は point read、indexed range read、明示 page size を基本にする。

## Storage Choice

key-value、BTree、append-only log で足りる用途は `ic-stable-structures` を優先する。構成が単純で、SQL planner と VFS のリスクがない。

SQLite を選ぶ条件は、schema migration、複合 index、relational constraint、ad-hoc query の価値が storage 複雑性を上回る場合に限定する。

## Migration Failure Recovery

Migration は `Db::migrate` から一括 transaction で実行する。失敗時は SQL を rollback し、`superblock.schema_version` は更新しない。

復旧手順:

1. `db_meta` で `schema_version`, `checksum`, `checksum_stale` を確認する。
2. `db_integrity_check` が `ok` を返すことを確認する。
3. 失敗 migration の SQL を修正する。
4. 同じ canister を upgrade し、`post_upgrade` の `Db::init` 後に管理 update から migration を再実行する。
5. `db_meta.schema_version` が target version へ進むことを確認する。

## Import

Import は controller 限定APIで実行し、checksum 一致を必須にする。

1. import 元で `db_refresh_checksum_chunk(max_bytes)` を `complete == true` まで実行する。
2. `db_meta` で `db_size`, `checksum`, `last_tx_id` を取得する。
3. `db_export_chunk(offset, len)` で全 byte を取得する。
4. export 後に `db_meta.last_tx_id` が変化していないことを確認する。
5. import 先で `db_begin_import(total_size, expected_checksum)` を呼ぶ。
6. offset 0 から順番に `db_import_chunk` を呼ぶ。
7. `db_finish_import` が checksum を検証し、import flag を解除する。

Import 中は SQLite VFS が `/main.db` open を拒否するため、通常 DB API は失敗する。checksum 不一致時は staging 領域を破棄し、既存DBを維持して import flag を解除する。未完了 import を中止する場合は controller が `db_cancel_import` を呼ぶ。

## Capacity

Stable memory grow 失敗時は `current_pages` と `required_pages` を含む error を返す。呼び出し側は retry せず、容量上限・cycle 残量・chunk size を確認する。

通常 commit は安全な publish のため、dirty page、dirty segment table、新root tableを追記してからsuperblockを更新する。小更新の容量増分は概ねdirty page数とdirty segment数に比例する。`db_meta.compact_recommended == true` の場合、controllerが `db_compact` を実行する。

## Integrity

管理系監視は以下を定期確認する。

- `db_integrity_check == "ok"`
- `db_meta.importing == false`
- `db_meta.checksum_stale == false` または controller の checksum refresh job が進行中
- `db_meta.checksum_refreshing == false`
- `db_meta.orphan_ratio_basis_points`
- `db_meta.compact_recommended`

`db_meta.checksum` は last verified checksum。`checksum_stale == true` は更新後の正常状態でも発生する。必要な場合は controller が `db_refresh_checksum_chunk` を完了まで実行して直近検証済みに戻す。
