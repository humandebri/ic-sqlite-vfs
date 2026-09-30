# Migration checksum validation — 2026-09-30

Implemented on `feat/migration-checksums`, based on budget/security implementation
`8ec0f3947d711ad28c3fad8ba03f003fb1f0cffd`. SQLite 3.53.4, Rust 1.95.0,
precompiled release Wasm, PocketIC 12.0.0 arm64.

## Correctness

- 215 Rust tests passed, 2 ignored; optional-metrics/failpoint configuration.
- SHA-256 known vector, fresh/repeated migrations, changed SQL preflight,
  version-only legacy history, explicit adoption, atomic failures and DB-handle isolation.
- Caught checksum mismatches do not change stable bytes or schema version.
- Real Wasm feature probe checks new/checksummed and legacy/adopted histories
  inside a rolled-back savepoint. The old-version-history upgrade suite passed
  all 4 tests; the budget/query suite and KV regression suite passed as well.
- Clippy with warnings denied, fmt, public API snapshot and no-await checks passed.

## Storage and benchmark impact

One checksum side table preserves the version-only history schema. Storage is
one 32-byte SHA-256 digest per recorded migration, plus SQLite row/page overhead.
Unverified legacy versions receive no digest automatically. Repeated calls do
not append digest records. There are no staging regions or stable-layout changes.

The KV fixture has one migration. Its logical DB grows by one 16 KiB page.
Some workloads report one additional 64 KiB page in the selected **virtual
memory**. The benchmark's `stable_pages` / `stable_bytes` describe that virtual
memory, not the canister's raw stable-memory allocation.

The default MemoryManager acquires raw memory in 128-page (8 MiB) buckets,
plus its metadata. Virtual growth within an existing bucket need not grow raw
memory; crossing a bucket boundary can require another 8 MiB bucket. Neither
virtual high-water usage nor raw allocation should be assumed to shrink after
deleting data. Use raw `stable64_size` and canister resource metrics when
assessing physical capacity and cycle costs.

A separate reference-canister review measured 327,680 virtual bytes (320 KiB)
and 8,454,144 raw stable bytes (8.0625 MiB). This is the existing allocator's
behavior, not a measured before/after allocation increase from this feature.
The KV comparison above did not measure before/after raw allocation.

One final KV regression run was compared with the retained pre-checksum reports
from commit `8ec0f39`. Reported row counts and checksums matched. IC instruction
changes ranged from -0.044% to +0.890% across the 34 measurements; these endpoint
scopes exclude initial migration execution and response encoding. This comparison
does not measure hash cost for large migration lists or startup latency.

| Workload | Before instructions | After instructions | Logical DB before / after |
| --- | ---: | ---: | ---: |
| `bench_reset_1000_clean` | 10,813,205 | 10,808,816 | 98,304 / 114,688 |
| `bench_read_1000` | 10,101,735 | 10,101,735 | 98,304 / 114,688 |
| `bench_update_only_5000` | 91,651,424 | 91,634,161 | 278,528 / 294,912 |
| `bench_many_rows_5000` | 6,457,359 | 6,457,307 | 278,528 / 294,912 |
| `bench_large_blob_64k` | 949,690 | 958,146 | 131,072 / 147,456 |

Benchmark release Wasm grew from **1,633,141 to 1,645,294 bytes**,
an increase of **12,153 bytes (0.744%)**. This measures the KV fixture, not a
universal consumer size bound. Heap high-water use was not measured.

Raw final benchmark reports and logs are under ignored
`target/migration-checksums/bench.jsonl` and `bench.log`; the before reports are
under `target/bench-comparison/current-final.jsonl`. The runner is a temporary
copy of the unchanged KV regression harness with configurable Wasm/report paths.

## Reproduction

```sh
cargo test --offline --features query-metrics,canister-api-test-failpoints
cargo clippy --offline --all-targets --features query-metrics,canister-api-test-failpoints -- -D warnings
cargo build --offline --locked --release --target wasm32-unknown-unknown \
  --no-default-features --features sqlite-precompiled,canister-api-test-failpoints
cp target/wasm32-unknown-unknown/release/ic_sqlite_vfs.wasm target/pocketic/ic_sqlite_vfs_failpoints.wasm
```

Build/copy the production `sqlite-precompiled,canister-api` artifact to
`target/pocketic/ic_sqlite_vfs.wasm`, keeping the pre-checksum Wasm separately.

```sh
UPGRADE_FROM_WASM=/path/to/pre-checksum.wasm node --test tests/pocketic/upgrade.test.mjs
node --test tests/pocketic/query_budget.test.mjs
```

The commit exposes no new public canister maintenance method; adoption is a
Rust facade API. Authorization belongs to the consuming application.

## IC upgrade failure check

A regression test in `tests/pocketic/upgrade.test.mjs` changes only the casing of
`CREATE` in migration version 1, preserving SQL semantics while changing its
SHA-256. PocketIC rejects its upgrade with a checksum mismatch. The installed
module hash, complete raw stable-memory byte array, DB metadata and stored rows
remain unchanged, and the next ordinary update succeeds. This agrees with the
[IC install_code atomicity contract](https://docs.internetcomputer.org/references/ic-interface-spec/management-canister/).

The fixture is generated in memory from the current production Wasm and requires
exactly one match for the original migration SQL. It installs the current artifact
independently of `UPGRADE_FROM_WASM`, since legacy histories have no checksums to
verify. It also verifies that an unchanged migration can still upgrade after the
rejection. The test runs through `npm run test:pocketic:regression`, which is part
of both CI and release validation; no additional fixture build is needed.
