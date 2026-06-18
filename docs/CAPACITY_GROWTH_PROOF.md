# Capacity Growth Proof

この文書は、v8 in-place stable-memory layout が容量肥大化退行を起こさない
ための資源不変条件を、HackMD 型の手証明として固定する。目的は「全バグ
不在」ではなく、ここで列挙する操作が `allocated_bytes`、high-water mark、
`db_base_offset`、`page_table_bytes` の契約に違反しないことを示すこと。

## 1. 公理

**A1: stable memory high-water monotonicity.** IC stable memory は grow できるが
shrink できない。したがって物理確保量 `cap(t)` は単調非減少である。

**A2: overwrite availability.** 確保済み範囲 `p + len <= cap(t)` への write は、
その範囲が過去に使われたかどうかに依存しない。append-only は stable
memory API の要求ではなく、上書きを使わない VFS 層の設計選択である。

**A3: message execution atomicity.** IC では単一 canister の message execution
は逐次かつ atomic である。根拠は ICP Developer Docs の
`Properties of Message Executions on ICP`、Property 1。

**A4: trap rollback.** trap / panic した message execution の canister state
変更は適用されない。根拠は同文書の Property 5。これを stable memory write
にも及ぶ基盤層公理として扱う。

## 2. 状態と操作

状態変数:

- `db_size`: 論理 SQLite image 長。
- `db_base_offset`: SQLite image の固定物理 base。v8 では通常 `64KiB`。
- `allocated_bytes`: 選択 stable memory の high-water byte 数。
- `high_water_mark`: `allocated_bytes` と同じ資源水位を表す抽象名。
- `orphan_bytes_estimate`: `SUPERBLOCK_SIZE + db_size` の外側にある high-water slack。
- `page_table_bytes`: v8 committed state では常に `0`。
- `zero_extents`: truncate された whole-page tail を論理 zero として隠す範囲列。

**D1: append-only commit.** 旧方式は dirty page と page table を commit 開始時の
append boundary 以降へ書き、最後に superblock を publish する。旧物理領域は
再利用しない。

**D2: v8 in-place normal commit.** v8 通常 commit は dirty logical page `n` を
`db_base_offset + n * SQLITE_PAGE_SIZE` に書く。page table を publish せず、
`page_table_offset == 0` と `page_table_bytes == 0` を維持する。

**D3: existing-capacity commit.** final image end と全 active dirty page end が
`allocated_bytes` 以下なら、commit は existing-capacity commit である。

**D4: truncate zero mask.** truncate は非 active whole-page tail を
`zero_extents` へ追加する。後続 dirty write は該当 page の mask を解除する。
追加後の extent は正規化され、空 range、重複、隣接、逆順を持たない。
上限判定は正規化後の extent 数で行う。上限超過はエラーであり、append-only
fallback しない。

**D5: compact no-op.** v8 `compact()` は public API として残るが、stable memory
高水位を下げず、page data も page table も再配置しない。

**D6: import/fresh base.** import と fresh base creation は通常 commit ではない。
完全な logical image を別 base へ staging し、checksum 成功時だけ publish する。
この経路は意図的に容量増加し得る。

## 3. 定理

**T1: append-only is not forced by stable memory.** A2 により確保済み領域は
上書き可能である。したがって D1 の「旧領域を再利用しない」は stable memory
API ではなく VFS 層の設計選択である。

**T2: append-only can grow under repeated updates.** D1 では少なくとも 1 dirty
page を持つ commit ごとに append boundary 以降へ新領域を書く。A1 により
確保量は縮まないため、反復更新では `allocated_bytes` が O(number of commits)
で増加し得る。

**T3: v8 dirty page writes use fixed offsets.** D2 により dirty logical page `n`
の物理 offset は `db_base_offset + n * SQLITE_PAGE_SIZE` のみである。通常
commit は新しい image copy や page table を append しない。

**T4: existing-capacity v8 commits do not grow resources.** D3 の前提が成り立つ
場合、全 write は既存 `allocated_bytes` 内に収まる。A2 により上書きは許可
され、grow は不要である。したがって `db_base_offset`、`allocated_bytes`、
`high_water_mark`、`page_table_bytes` は不変である。

**T5: truncate tail is not stale logical data.** D4 により truncated whole-page
tail は active page ではなく、zero mask により論理読取から隠される。後続
write は mask を解除して materialize する。truncate tail が既存 extent と
重複または隣接する場合は merge され、上限判定は merge 後の正規化済み列に
対して行う。正規化後も zero extent 上限を超える場合は publish せずエラーに
するため、append-only fallback による資源退行はない。

**T6: compact is not reclaim.** D5 と A1 により v8 `compact()` は
`allocated_bytes`、`high_water_mark`、`orphan_bytes_estimate`、`db_base_offset`
を下げない。`compact_recommended == false` は「回収不要」ではなく「この layout
では compact が回収機構ではない」という意味である。

**T7: import is an explicit exception.** D6 は通常 commit ではなく、staging base
へ image を書くため容量増加し得る。checksum mismatch または未完了 finish は
committed image を維持し、staging bytes は high-water slack として残り得る。

**T8: in-place atomicity depends on IC message atomicity.** v8 normal commit が
単一 message execution 内で開始・完了し、commit 中に inter-canister call を
含まない場合、A3 と A4 により成功時は全 write が確定し、trap 時は当該
message の write が破棄される。したがって message 境界の観測者に対して
commit は全か無かに見える。この原子性は VFS 自前の shadow paging ではなく
IC 基盤層に依存する。

## 4. 証明対応

| 手証明 | Verus 抽象モデル | Rust / PocketIC / local coverage |
| --- | --- | --- |
| T1, T2 | 対象外。旧append-onlyの否定的設計分析 | `docs/API_STABILITY.md` の負の仕様 |
| T3 | `dirty_page_write_offset_matches_in_place_layout` | `in_place_commit_keeps_dirty_page_offsets_stable` |
| T4 | `normal_commit_within_capacity_does_not_grow_resources` | `repeated_existing_page_updates_do_not_grow_allocated_bytes`, `bench_capacity_growth_guard` |
| T5 | `truncate_zero_extents_are_normalized`, `truncate_zero_extents_mask_matches_model`, `truncate_zero_extents_preserve_limit`, `truncate_zero_extent_limit_error_blocks_publish` | `zero_extent_limit_is_rejected_without_fallback`, normalized truncate/limit tests, truncate/grow roundtrip tests |
| T6 | `compact_preserves_resource_high_water`, `compact_never_recommends_reclaim` | `compact_preserves_logical_database_contents`, repeated compact resource checks |
| T7 | `checksum_mismatch_preserves_committed_image`, `checksum_match_publishes_staging_image` | `failed_import_preserves_existing_database`, import checksum tests |
| T8 | 対象外。IC公式仕様を公理として採用 | await-free canister API check, failpoint rollback tests, PocketIC upgrade/regression |

## 5. 境界

この証明は production Rust の完全機械証明ではない。Verus は
`proofs/verus/*.rs` の抽象モデルを検証する。以下は仮定または別検証で補う。

- SQLite C core の正しさ。
- C ABI / FFI 境界の正しさ。
- IC stable memory system API と message execution atomicity。
- Rust実装とVerus抽象モデルの完全一致。
- canister が commit 中に inter-canister call を行わないこと。
- 単一 message の命令・汚染 page 上限を超える大 commit の活性。

したがって結論は「全バグ不在」ではなく、v8 in-place layout がこの文書で
列挙した資源不変条件を満たす限り、旧append-only型の通常更新による容量肥大化
退行を起こさない、である。

参考: <https://docs.internetcomputer.org/references/message-execution-properties.md>
