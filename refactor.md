# ic-sqlite-vfs 段階的リファクタリング計画

**✅ 全フェーズ完了(2026-07-03)。** 成果: main ベースの26コミット(`refactor/phase-6-crosscutting` が最終 tip、28ファイル +3748/-3114)。全コミットを Claude(監督)が監査、codex-reviewer が実装。各フェーズの実績は該当セクション参照。監査で検出・修正した問題: commit_load 誤削除(Phase 1)、flaky テスト(Phase 0 起因)、superblock キャッシュ世代リーク(Phase 3)。Phase 5 は調査の結果コメントのみで完結(panic はすべて意図的な契約)。旧 u16 ベースのスタックは backup/u16base-* に保全。

作成日: 2026-07-02
調査方法: 司令塔(Claude)がリポジトリ全体を直接読まず、codex-reviewer に read-only 調査タスク6件(T1〜T6)を委任し、その結果のみを統合した。すべての file:line は codex-reviewer の報告に基づく。

---

## 1. 現状アーキテクチャ(証拠つき)

### モジュール構成と依存方向

```
lib.rs (re-exports: src/lib.rs:28-31, macros :35,:45)
  ├── api (feature: canister-api, src/lib.rs:10-11)
  │     └─→ db, stable (src/api.rs:6-12)
  │     └─→ sqlite_vfs::stable_blob ★違反 (src/api.rs:220,305)
  ├── db (公開ファサード)
  │     └─→ sqlite_vfs::stable_blob, stable (src/db/mod.rs:14-16, src/db/transaction.rs:6-8)
  ├── sqlite_vfs (VFS コールバック層)
  │     └─→ config, stable, meta (src/sqlite_vfs/file.rs:6-13, vfs.rs:6-11)
  └── stable (stable memory バックエンド)
        └─→ config, 内部 raw/memory_manager
        └─→ stable::meta キャッシュ無効化 ★上方向結合 (src/stable/memory.rs:390,418,441)
```

### レイヤリング違反(是正対象)

| # | 内容 | 証拠 |
|---|------|------|
| V1 | `api` が Db ファサードを迂回し `sqlite_vfs::stable_blob` の stats/rollback を直接呼ぶ | src/api.rs:220,305 |
| V2 | 低レベル `stable::memory` が上位 `stable::meta` のキャッシュを無効化(raw メモリ操作とメタデータキャッシュの結合) | src/stable/memory.rs:390,418,441 |
| V3 | `overlay` が `stable_blob` のベースページ読みに依存し、ストレージバックエンド詳細を知っている | src/sqlite_vfs/overlay.rs:8,216,309 |
| V4 | VFS コールバック層が `read_metrics` を直接記録(横断的関心事の散在) | src/sqlite_vfs/file.rs:121,184,229,283,294,305,325,364 |

---

## 2. 負債インベントリ

### 【証拠あり — high confidence】

- **D1 死にコード**: ~~`record_commit_load` は呼び出し元なし~~ **← 実装時に誤りと判明**。`commit_profile_recorder!` マクロ経由で commit_overlay から呼ばれる生きた計測で、PocketIC perf テストが `commit_load > 0n` をアサートしていた。`#[allow(dead_code)]` 属性の方が誤り(Phase 1 で属性のみ除去)。削除の是非は Phase 6 で判断 — src/read_metrics.rs:180
- **D2 legacy エイリアス**: `query_row`/`query_row_named` は `query_one` 系の rusqlite 風別名 — src/db/connection.rs:261,271
- **D3 legacy 命名**: `query_map`/`query_map_named` は rusqlite 名を保ちつつ Vec を返す(イテレータでない) — src/db/connection.rs:333,345
- **D4 panic 経路**: superblock 発行失敗時、ページ書き込み後に panic — src/sqlite_vfs/stable_blob.rs:571,578
- **D5 unsafe 集中**: file.rs に 22 箇所(:82,:98,:157 ほか)、vfs.rs に 20 箇所、statement.rs に 26 箇所(:665-965)、connection.rs に 22 箇所(:119-214,:441,:496)
- **D6 グローバル状態**: `PREPARE_ONCE`/`LAST_ERROR` — src/sqlite_vfs/vfs.rs:43,45。thread-local superblock キャッシュ — src/stable/meta.rs:23,365。overlay の thread-local active 状態 — src/sqlite_vfs/overlay.rs:24
- **D7 dead_code helper 群**: `stable/memory.rs` に 8 箇所の `allow(dead_code)` テスト/failpoint ヘルパ — src/stable/memory.rs:80,84 周辺
- **D8 静的カウンタ散在**: read_metrics に 21 個の static カウンタ + 手動リセット配列 — src/read_metrics.rs:33-54,200

### 【証拠あり — medium confidence】

- **D9 重複**: `reset_and_bind_*` 系がパラメータ数チェック・reset・型変換・bindエラー処理を text/i64/blob で繰り返す — src/db/statement.rs:677-850
- **D10 memory_manager の panic**: bucket size ゼロで panic(:51)、invalid layout panic(:190,:394,:398) — src/stable/memory_manager.rs
- **D11 巨大ファイル**: stable_blob.rs 2207行・113関数、うち :1103-2207 はファイル内テスト。statement.rs 1115行、meta.rs 727行(:403-727 テスト)、connection.rs 718行(:548-718 テスト)

### 【推測(GUESS)— 追加調査が必要】

- **G1** ✅解決(第4波 T12): optional 化は**コード移動が先に必要**。`candid` は canister-api モジュール内のみで clean。だが `ic_cdk` は canister-api 外の4箇所で使用 — src/read_metrics.rs:254(performance_counter)、src/db/statement.rs:1062(performance_counter, bench-profile)、src/sqlite_vfs/vfs.rs:353(time)、src/stable/memory.rs:544(trap, failpoints ゲート)。いずれも `target_arch = wasm32` ゲート。単純な `dep:ic-cdk` 化は wasm ビルドを壊す
- **G2** ✅解決(第3波 T7): import 系関数は **production 到達不能**。canister API・PocketIC テスト・examples・benchmarks・scripts のいずれにも呼び出しなし。`sqlite_vfs` は crate root から private/doc-hidden(src/lib.rs:21)、`test_support` の再エクスポートにも import 系は含まれない(src/test_support.rs:34-35)。docs/API_STABILITY.md:28-32 も import/export/compact は Rust ファサード・参照 canister から非公開と明記 → テスト/内部専用として扱ってよい
- **G3** ✅解決(第4波 T11): production 側(:1-1102)の unwrap/expect は **1 箇所のみ**(src/sqlite_vfs/stable_blob.rs:1083、`page_len()` の「page size fits usize」expect)。残り167箇所はすべてテスト区画(:1103-2207)。当初懸念していた頑健性リスクは実質存在しない

### 【未確認事項 → 判定済み(第4波)】

- legacy エイリアス(D2/D3): クレートは **crates.io 公開済み**(ic-sqlite-vfs 2.0.0、README.md:3 にバッジ、タグ v1.0.1 まで、publish は手動 — docs/RELEASE.md:100-104)。エイリアスは docs/MIGRATING_FROM_IC_SQLITE.md:119,130,147-152 と **凍結 API スナップショット docs/PUBLIC_API_2_0.snapshot:26-27,35-36** に記載された文書化済み互換サーフェス。→ **2.x での削除は不可**。`#[deprecated]` 付与もリリース/API ポリシーとの調整必須
- compat-fixtures/ic-sqlite-vfs-1-0-0: crates.io の 1.0.0 に固定した PocketIC クロスバージョンテスト用 canister(公開済み 1.0 イメージの upgrade/import 成功を証明する目的、src/lib.rs ヘッダに明記)。旧消費者 tests/pocketic/cross_version.test.mjs は現ツリーに存在せず。→ **removable-with-maintainer-signoff**(release アーティファクト内容が変わるため互換ポリシー責任者と調整)

---

## 3. 安全網の評価(リファクタ時に何が守ってくれるか)

- **統合テスト**: vfs_roundtrip.rs(35件・E2E)、typed_api.rs(17)、memory_manager_compat.rs(14)、memory_manager_corruption.rs(13)、stress.rs(7・PBTモデル)、checksum.rs(6)ほか計 ~106 Rust テスト + PocketIC mjs(upgrade/regression/perf/churn)
- **fuzz**: fuzz/fuzz_targets/state_ops.rs — BTreeMap モデルとの差分 + integrity_check
- **Verus 証明**: proofs/verus/ — layout 計算、in-place commit、import 状態機械、superblock エンコード、overlay/compact モデル。**該当コードを変更したら対応する proof の更新が必須**(CI の sqlite-critical-check が Verus を実行、.github/workflows/ci.yml)
- **弱いところ(フェーズ0で補強)**:
  - file.rs / vfs.rs のコールバック単体テストが薄い(統合テスト頼み、GUESS)
  - read_metrics.rs に専用テストなし
  - canister API の native 単体テストが薄い(PocketIC 依存)

---

## 4. フェーズ計画

原則: 各フェーズは独立に merge 可能・CI green を維持・1フェーズ = 1〜3 PR。挙動変更を伴うフェーズは必ず先行フェーズでテストを足してから行う。

### Phase 0 — 安全網の補強(挙動変更なし) ✅実施済み

実績: ブランチ `refactor/phase-0-safety-net`(commit 3a2620d)、テスト11件追加(+297行、production 変更ゼロを監査済み)。

目的: 後続フェーズの回帰検出力を上げる。
1. file.rs / vfs.rs のコールバック単体テストを追加(エラーパス・境界値中心)
2. read_metrics.rs の専用テスト追加(カウンタ登録・リセット網羅)
3. ~~stable_blob.rs の unwrap/expect 内訳計測~~ → 解消済み(production 側は :1083 の1箇所のみ)
4. CI 実行時間のベースライン記録
検証: 既存テスト全 green、カバレッジ計測。
リスク: 低。

### Phase 1 — 死にコード・legacy の整理(削除系、低リスク) ✅実施済み

実績: ブランチ `refactor/phase-1-dead-legacy`(phase-0 にスタック)。監査で D1 の誤判定が発覚し、record_commit_load は削除でなく属性除去に変更(コミット 043304b で復元)。

1. ~~`record_commit_load` 削除~~ → **中止**(生きた計測と判明、上記 D1 参照)。`allow(dead_code)` 属性のみ除去
2. stable/memory.rs の dead_code ヘルパ 8 箇所を精査し、failpoint feature 配下へ移動 or 削除(D7)
3. import 系関数(G2、解決済み: production 到達不能を確認)を `#[cfg(test)]` / failpoint feature 配下へ移動。**削除はしない** — proofs/verus/import_state_machine.rs が begin/逐次 chunk/不完全 finish 拒否/cancel/インポート中 update 拒否を保証しており、将来の公開に備えた検証済み状態機械のため
4. legacy エイリアス(D2/D3)は **2.x では削除不可・現状維持**(凍結スナップショット docs/PUBLIC_API_2_0.snapshot に含まれる文書化済み互換サーフェスのため)。`#[deprecated]` 付与は API ポリシー責任者と調整の上、migration docs 更新とセットで実施。`query_map` が Vec を返す点は docs/MIGRATING_FROM_IC_SQLITE.md:152 で文書化済み
5. `candid`/`ic-cdk` の optional 化(G1)は **2段階**で実施:
   - 5a. canister-api 外の `ic_cdk` 使用4箇所(read_metrics.rs:254、statement.rs:1062、vfs.rs:353、memory.rs:544)を、wasm32 ゲートの薄い内部 shim(time/performance_counter/trap)に集約
   - 5b. その後 `candid`/`ic-cdk` を `optional = true` + `dep:` 構文で canister-api に連動。shim は ic0 直呼び or ic-cdk optional 依存のどちらにするか実装時に判断
   - 検証: `cargo check --no-default-features --features sqlite-bundled`(native)+ wasm 2系統
検証: cargo check 全 feature 組合せ(§6 参照)、既存テスト。
リスク: 低〜中(G1 は依存グラフ変更のため wasm ビルド 2 系統を CI で確認)。

### Phase 2 — 重複統合(statement.rs) ✅実施済み

実績: `refactor/phase-2-statement-dedup`。StaticBind enum + 共通経路で 1108→1056行、公開サーフェス不変。監査中に Phase 0 由来の flaky テスト(global カウンタの serial 欠落)を検出し、13テストに serial 付与で解消(db0e930)。

1. `reset_and_bind_*` 群(D9、src/db/statement.rs:677-850)を、パラメータ数チェック + reset + bind ループを共通化したジェネリックな 1 経路に統合。SQLITE_STATIC 借用バインドの寿命規律(:707-850,:931)は変えない
2. bind/column FFI ヘルパ(:904-987)の unsafe を最小関数単位に分離し、safety コメントを付与
検証: typed_api.rs(17件)、strict_api.rs、public_api.rs。bench-profile ビルド(src/db/statement.rs:51,144,351 に cfg があるため `--features bench-profile` で必ず compile check)。
リスク: 中。バインド性能に影響し得るため benchmarks/kv-canister で前後比較。

### Phase 3 — レイヤリング違反の是正 ✅実施済み

実績: `refactor/phase-3-layering`(6コミット)。V1=Db pub(crate) 経由化、V4=IoMetric enum 集約、V3=BasePageSource trait + ZST アダプタ、V2=世代カウンタ方式(メモリ側が CACHE_GENERATION を所有、meta のキャッシュキーが (context, generation))。監査で旧世代エントリのリークを検出し retain-on-insert で修正(498a2c4)。Verus overlay_model 5/5・in_place_commit 25/25 を独立検証済み。

1. **V1**: `Db` ファサードに stats/rollback 相当の公開メソッドを追加し、api.rs:220,305 の stable_blob 直接呼び出しを置換
2. **V2**: meta キャッシュ無効化を反転。`stable::memory` がキャッシュを直接触る(:390,418,441)代わりに、無効化フック(コールバック or 上位での明示無効化)に変更。thread-local superblock キャッシュ(meta.rs:23,365)の整合性が焦点
3. **V3**: overlay の base-page 読み(overlay.rs:216,309)を trait(`BasePageSource` 等)経由にし、overlay を純粋モデル + アダプタに分離。proofs/verus/overlay_model.rs と対応を維持
4. **V4**: read_metrics の記録呼び出しを計測ポイント用の薄い層に集約(file.rs の 8 箇所)
検証: vfs_roundtrip.rs、stress.rs、checksum.rs、Verus(overlay_model, in_place_commit)、fuzz を各変更後に短時間実行。
リスク: 中〜高。V2 はメタデータキャッシュの stale 化バグを生みやすい — memory_manager_compat/corruption テストを重点確認。

### Phase 4 — 巨大ファイル分割(機械的移動、挙動変更なし)

✅実施済み。実績: `refactor/phase-4-file-split`(6コミット)。stable_blob.rs 2224行 → mod/state/ops/commit/logical/zero_extents/tests の7ファイル、meta.rs・connection.rs のテスト分離、file.rs/vfs.rs はセクション整理。移動純度は正規化差分(62行、全て整形クラス)で監査済み。**全スタックを main ベースに rebase 済み(衝突ゼロ、19コミット)**。

順序が重要: **テスト分離 → 本体分割**。
1. stable_blob.rs(:1103-2207 のテスト)を `stable_blob/tests.rs` 相当へ分離 → 本体を報告済みの区画に沿って分割: 状態/型(:1-144)、layout/公開ops(:144-514)、commit パイプライン(:518-658)、zero-extent(:662-838)、logical read/checksum(:846-1095)
2. meta.rs(:403-727)、connection.rs(:548-718)のファイル内テストを分離
3. file.rs / vfs.rs の unsafe コールバックを「FFI 境界(unsafe)」と「安全なロジック」に二分
検証: 挙動変更ゼロを主張できるよう、分割 PR にはロジック差分を含めない(git diff --stat で移動のみ確認)。全テスト + Verus。
リスク: 低(移動のみ)だが PR レビュー負荷が高い — 1 ファイル = 1 PR。

### Phase 5 — 頑健性(panic / unwrap 経路) ✅実施済み

実績: `refactor/phase-5-robustness`(3コミット、**全てコメント追加のみ +30行**)。調査の結果、変換すべき panic はゼロ: init 到達可能な panic は upstream ic-stable-structures 互換仕様(strict 経路 `init_strict*` は既に MemoryManagerInitError を返す二本立て)、残りは内部不変条件。trap-for-rollback 契約と各不変条件を文書化。

1. **D4(判定確定: 意図的)**: superblock 発行失敗時の panic(stable_blob.rs:571,578)は、docs/API_STABILITY.md:116-121 および docs/CAPACITY_GROWTH_PROOF.md:96-102 が定義する「IC メッセージ実行のアトミシティ + trap ロールバック」契約に合致する意図的な trap。**回復可能な部分成功へ変換してはならない**。許されるのは構造整理と panic サイトへの説明コメント追加のみ(現状、panic 箇所自体に説明コメントなし)。なお proofs/verus/in_place_commit.rs は publish 時失敗をモデル化していない(正常系の in-place commit 算術のみ)ため、この panic 経路の変更は proof では検出されない — レビューで守るべき箇所
2. **D10**: memory_manager の panic(:51,:190,:394,:398)を、初期化時は `MemoryManagerInitError`(既に lib.rs:30 で公開)への変換に統一。実行時 invariant 違反は panic 維持で可
3. production 側 unwrap/expect の整理 — 判明済みの対象は stable_blob.rs:1083 の1箇所のみ(`page_len()` の usize 変換 expect)。invariant コメント付与で十分。connection.rs の 30 箇所(:119-214 周辺)と meta.rs の 15 箇所は production/テスト内訳が未計測のため、着手時に同様の分割集計を行う
検証: memory_manager_corruption.rs(13件)、failpoint_tests.rs、fuzz。
リスク: 高。**IC canister では panic=trap がロールバック手段として意図的な場合がある**ため、1 箇所ずつ意図を確認してから変更。

### Phase 6 — 横断的関心事の再設計(任意・効果対コスト判断) ✅実施済み

実績: `refactor/phase-6-crosscutting`(4コミット)。①`read_metric_counters!` マクロでカウンタ21個のレジストリ化(リセット配列ドリフトを構造的に排除、commit_load は意図的に維持)②src/profiling.rs 新設で bench-profile cfg を集約(commit.rs 6→0)③LAST_ERROR グローバル廃止、per-context ContextState へ統合(context 破棄時のエラー残留も解消)。PREPARE_ONCE は FFI 形状上の必須として維持。

1. read_metrics の 21 static カウンタ(D8)をマクロ生成 or テーブル駆動に集約し、手動リセット配列(:200)の不整合リスクを除去
2. bench-profile の散在 cfg(statement.rs:51,144,351 / stable_blob.rs:583,595,613 / lib.rs:12-18)を計測フック層に集約
3. vfs.rs のグローバル状態(判定確定・第3波 T9):
   - `PREPARE_ONCE`(:43,:85): 現行の FFI 形状では必須(process-global な mutable `sqlite3_vfs` の実行時フィールドを登録前に一度だけ初期化)。SQLite 仕様上の必然ではないが、置換コストに見合わないため**現状維持を推奨**
   - `LAST_ERROR`(:45-46、読み :256,:299、書き :277,:284): **置換可能**。`pAppData` は null のまま未使用(vfs.rs:24、register.rs:21-23)で、現設計は active ContextId でエラーをキー付けしている。ContextId 別ストレージへの統合が可能
検証: `--features bench-profile` ビルド + benchmarks 実行比較。
リスク: 中。

---

## 5. フェーズ依存関係

```
Phase 0 ──→ Phase 1 ──→ Phase 2
   │                        │
   └──→ Phase 3 ←───────────┘ (2と3は並行可、statement/api で衝突なし)
              │
              └──→ Phase 4 ──→ Phase 5 ──→ Phase 6
```
Phase 4(分割)は Phase 3(結合是正)の後に行うこと。先に分割すると V1〜V3 の是正で分割済みファイル間を再度動かすことになる。

## 6. 全フェーズ共通の検証マトリクス

各 PR で最低限:
- `cargo test`(native, default features)
- `cargo check --target wasm32-unknown-unknown --features canister-api`(dfx.json のビルド構成)
- `cargo check --no-default-features --features sqlite-precompiled,canister-api --target wasm32-unknown-unknown`(npm build:wasm 構成)
- `cargo check --features canister-api-test-failpoints` / `--features bench-profile`(散在 cfg の破損検出、T6 指摘)
- feature 相互排他の診断 2 系統(src/lib.rs:7 の compile_error と build.rs:20-26 の panic)を**両方**維持
- 該当領域変更時: Verus proof 再実行(CI の sqlite-critical-check)、fuzz 短時間実行

## 7. 絶対に壊してはいけないもの(互換性契約・第3波 T10 で確定)

全フェーズを通じて以下を不変条件とする:

1. **2.x の rustdoc 可視な公開アイテム・シグネチャ・セマンティクス**(major リリースなしでの変更禁止) — docs/API_STABILITY.md:7-11
2. **v8 stable layout**: ICSQLITE magic、superblock v8、0..64KiB superblock 領域、db_base_offset 以降の固定ページオフセット — docs/API_STABILITY.md:174-180
3. **v6 レイアウトの直接 init は `UnsupportedLayoutVersion(6)` を返し続ける**(暗黙マイグレーション禁止) — docs/API_STABILITY.md:181-182
4. **バンドル MemoryManager のレイアウト互換**: upstream ic-stable-structures 0.7.x とのパリティ — docs/API_STABILITY.md:183、ルート統合テスト(固定 0.7.0)、compat-fixtures/ic-stable-structures-072(固定 0.7.2)、compat-fixtures/ic-stable-structures-latest-07(実行時の最新 0.7.x)で検証。※「MemoryId 0..=32767」は u16 ブランチ由来の記述。リファクタスタックは main(u8 MemoryId)ベースに載せ替え済み(2026-07-02、旧スタックは backup/u16base-* に保全)
5. **下流ビルド経路**: `default-features = false` + `sqlite-precompiled` — docs/API_STABILITY.md:187、build.rs の wasm 限定パス

compat-fixtures の消費関係: compat-fixtures/common/memory_manager_matrix.rs は固定 0.7.2 と最新 0.7.x の fixture が共有し、CI の sqlite-critical-check.sh が両方を必須実行する。ルートの tests/memory_manager_compat.rs は固定 0.7.0 と比較する。これらは Phase 3(V2: meta キャッシュ)・Phase 4(分割)の主要な回帰検出網でもある。

## 8. 調査ステータス(全4波・14タスク完了)

技術的な未確認事項は**ゼロ**。残るのはメンテナ/ポリシー判断のみ:

| 項目 | 判定 | 必要な判断 |
|------|------|-----------|
| G1 ic-cdk optional 化 | 2段階で実施可(Phase 1.5) | shim を ic0 直呼びにするか ic-cdk optional にするか |
| G2 import 系関数 | production 到達不能 | cfg ゲート移動のみ、判断不要 |
| G3 unwrap 内訳 | production 側1箇所のみ | 判断不要 |
| D4 superblock panic | 意図的 trap、変更禁止 | 判断不要(コメント追加のみ) |
| legacy エイリアス | 2.x 削除不可 | `#[deprecated]` 付与時期(API ポリシー責任者) |
| 1-0-0 fixture | 消費者なし | 削除 or クロスバージョンテスト復活(互換ポリシー責任者) |
| vfs.rs LAST_ERROR | 置換可能 | Phase 6 で実施するかの優先度判断 |

補足: 第4波 T13 で、1-0-0 fixture の旧消費者 `tests/pocketic/cross_version.test.mjs` が過去に存在し現在削除されていることが判明。fixture を残すなら**クロスバージョンテストの復活**が本来の選択肢(fixture だけ残って検証が消えている状態は中途半端)。
