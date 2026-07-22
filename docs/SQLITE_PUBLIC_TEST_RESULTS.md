# SQLite公開テストの検証結果

## 判定

2026年7月11日に、同梱SQLiteと同じsource IDの公開テストをApple Container上のLinuxで再実行した。
SQLite公開TCL `alltest`は、標準構成と同梱相当構成のどちらも全スイートの末尾まで実行できた。
ただし不一致が残るため、「SQLite公開fulltest全件合格」とは扱わない。

残った不一致と従来の途中停止は、SQLite公開テストのfault simulator後始末、recover拡張、および意図的なビルド構成差に限定される。
いずれも出荷するicstable VFSの製品経路ではないため、現行VFSのコード修正は不要と判断する。

公開TCLテストはSQLiteのUnix VFSを使用するため、icstable VFSそのものの合否判定には使用しない。
リリース可否はリポジトリ内のVFS回帰テスト、failure injection、state-machine fuzz、およびPocketICテストで判定する。

## 実行環境

| 項目 | 値 |
|---|---|
| 実行日 | 2026-07-11 |
| SQLite | 3.51.3 |
| source ID | `737ae4a34738ffa0c3ff7f9bb18df914dd1cad163f28fd6b6e114a344fe6d618` |
| コンテナ | Apple Container 1.1.0、Linux/arm64、4 CPU、8 GiB |
| イメージ | Ubuntu 24.04.4 LTS、arm64 digest `sha256:7f622ca8766bccb22f04242ecb6f19f770b2f08827dc4b8c707de5e78a6da7ab` |
| ツール | Tcl 8.6.14、GCC 13.3.0 |
| 実行ユーザー | 非root、UID 501 |

SQLiteソースは読み取り専用でマウントした。
ビルド生成物とテスト用DBはコンテナ内のファイルシステムへ置き、結果だけをホストへ保存した。

## 最終結果

| 構成 | assertion | 不一致 | 全スイート末尾 | fuzzcheck |
|---|---:|---:|---|---|
| SQLite標準構成 | 6,413,918 | 18 | 到達 | 46,391件中エラー0 |
| 同梱相当構成（既知非互換を除外） | 4,414,041 | 2 | 到達 | 46,391件中エラー0 |

標準構成はテストファイルを除外していない。
SQLiteテストハーネスのfault callbackを各テストファイル後に解除する後始末だけを追加した。

同梱相当構成は、公開TCLテストに必要なUnix VFSを残すため`SQLITE_OS_OTHER`を除外した。
また、`SQLITE_OMIT_WAL`、`SQLITE_OMIT_LOCALTIME`、`SQLITE_DEFAULT_FOREIGN_KEYS=1`、RTREE無効、`SQLITE_THREADSAFE=0`などと両立しない既知のテストファイルおよびmutexスイートを許容リストで除外した。
したがって、同梱相当構成の数値は公開スイート全体ではなく、既知非互換を除外した適用可能範囲の結果である。
実際の出荷バイナリは`SQLITE_OS_OTHER=1`でicstable VFSだけを登録する。

### 同梱相当構成の除外リスト

ファイルパターンは`wal*.test`、`e_wal*.test`、`pagerfault.test`を除外した。
加えて、次の46ファイルを既知のcompile flag非互換として除外した。

```text
altercol.test altertab2.test badutf2.test busy2.test chunksize.test
cksumvfs.test corruptL.test date.test date2.test delete_db.test
e_blobopen.test e_fkey.test enc4.test exclusive.test fkey1.test fkey5.test
fts5corrupt3.test incrvacuum.test insert4.test interrupt2.test memsubsys2.test
misc4.test misc7.test nolock.test orderby1.test pager1.test pcache.test
pragma.test pragma4.test recovercorrupt2.test recoverfault.test resetdb.test
rowallock.test schema.test spellfix.test symlink.test sync2.test sysfault.test
table.test tkt-bd484a090c.test tkt1644.test trace.test trace2.test
triggerC.test unixexcl.test zeroblob.test
```

スイートは`no_mutex_try`と`fullmutex`を除外した。
除外リストに追加が必要になった場合は、その原因を分類できない限り同梱相当構成を未合格とする。

## 従来の途中停止の原因

`walrestart.test`はプロセス全体に作用するfault callbackを登録するが、終了時に解除しない。
`alltest`はテストごとにTclスレーブインタプリタを破棄するため、次のテストがSQLiteを開くと、破棄済みインタプリタを参照するcallbackがfault code 500で失敗していた。
このため、除外するテストを変えるたびに`walro.test`、`walro2.test`、`walrofault.test`、`walseh1.test`へ同じ停止が移動した。

各テストファイルの終了後に`sqlite3_test_control_fault_install`を引数なしで呼び、callbackを解除すると、これらを含む標準構成の全スイートが完走した。
SQLite本体、Unix VFS、icstable VFSの障害ではなく、公開TCLテストハーネスの後始末不足である。
SQLiteソースには変更を加えていない。

## 残った不一致

### 標準構成18件

- 16件は`recoverfault`の同じ終端OOM不一致が`full`、`memsubsys1`、`memsubsys2`、`no_mutex_try`の4構成で各4件発生したもの。
- 2件は`inmemory_journal`のmutex一覧に`static_prng`が含まれるかどうかというテストハーネスのグローバル状態差。

`recoverfault.test`は単独実行ではarm64/amd64とも8,697件中不一致0、テストスレーブ経由でも8,659件中不一致0だった。
連続実行時だけ、注入回数0の終端でrecover APIが`out of memory`を返す。
recover拡張は製品で使用しておらず、icstable VFSを経由しない。

### 同梱相当構成2件

- `inmemory_journal.attach4-1.5`は、テストが11個の削除ジャーナルを期待する一方、強制されたメモリジャーナルが1個含まれた。
- `inmemory_journal.incrvacuum3-2.1.4.3`は、同じ強制メモリジャーナル構成でテスト用DBが`database disk image is malformed`を返した。

いずれも`inmemory_journal`テスト構成と同梱ビルドフラグの組合せに限定される。
出荷構成でUnix VFSのインメモリジャーナルを使う経路ではないため、製品修正は不要である。

## リリースでの扱い

SQLite公開TCLテストはadvisoryとし、現在のリリースゲートには追加しない。
同梱SQLiteのsource IDまたは`vendor/sqlite/build-flags.txt`を変更した場合は、公開fuzzとTCLテストを再実行し、許容リスト外の新規不一致がないことを確認する。

2026年7月22日のWasmコード生成変更では、source IDと`vendor/sqlite/build-flags.txt`を変更せず、`vendor/sqlite/wasm-compiler-flags.txt`に`-O3`と`-msimd128`を設定した。
公開TCLテストはネイティブのUnix VFSを使用し、出荷するWasmの最適化やSIMD命令を検証できないため、この変更では再実行していない。
代わりに、precompiled／bundledの両構成でrelease Wasmを生成し、`simd128` feature、`ic0`以外のimport禁止、10 MiBのcode section上限、100 MiBのmodule上限を検査した。
さらにPocketICでVFS回帰、upgrade、failure injection、性能回帰を実行し、出荷経路の挙動と計測値を確認した。

今回、標準構成は従来停止していた箇所以降を含め、無除外で全スイート末尾まで実施した。
同梱相当構成は、上記の既知非互換を除外した適用可能範囲で全スイート末尾まで実施した。
除外した公開TCLテストと、非公開かつ有償のTH3は実施範囲に含まれない。
