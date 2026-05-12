# Operations

## Transaction Rule

公開 update API は同期関数だけで実装する。`Db::update` は `FnOnce(&Connection) -> Result<T, DbError>` だけを受け取り、Future を受け取らない。transaction 中の `await` は禁止。

CI では `scripts/check-no-await.sh` で `src` 内の `.await` と `async fn` を拒否する。

## Migration Failure Recovery

Migration は `Db::migrate` から一括 transaction で実行する。失敗時は SQL を rollback し、`superblock.schema_version` は更新しない。

復旧手順:

1. `db_meta` で `schema_version` と `checksum` を確認する。
2. `db_integrity_check` が `ok` を返すことを確認する。
3. 失敗 migration の SQL を修正する。
4. 同じ canister を upgrade し、`post_upgrade` の `Db::init` 後に管理 update から migration を再実行する。
5. `db_meta.schema_version` が target version へ進むことを確認する。

## Import

Import は controller 限定APIで実行し、checksum 一致を必須にする。

1. import 元で `db_meta.db_size` と `db_checksum` を取得する。
2. `db_export_chunk(offset, len)` で全 byte を取得する。
3. import 先で `db_begin_import(total_size, expected_checksum)` を呼ぶ。
4. offset 0 から順番に `db_import_chunk` を呼ぶ。
5. `db_finish_import` が checksum を検証し、import flag を解除する。

Import 中は SQLite VFS が `/main.db` open を拒否するため、通常 DB API は失敗する。checksum 不一致時は staging 領域を破棄し、既存DBを維持して import flag を解除する。

## Capacity

Stable memory grow 失敗時は `current_pages` と `required_pages` を含む error を返す。呼び出し側は retry せず、容量上限・cycle 残量・chunk size を確認する。

## Integrity

管理系監視は以下を定期確認する。

- `db_integrity_check == "ok"`
- `db_meta.importing == false`
- `db_meta.checksum == db_checksum`
