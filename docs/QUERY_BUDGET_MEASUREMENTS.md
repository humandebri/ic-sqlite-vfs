# Query budget measurements

Measured on 2026-09-30 with Rust 1.95.0, the repository's precompiled SQLite 3.53.4
archive, `wasm32-unknown-unknown` release builds and PocketIC 12.0.0. These
figures describe fixed regression workloads, not production cost estimates.
The tests use controller-only endpoints compiled under
`canister-api-test-failpoints`; production `canister-api` builds omit them.

## Soft instruction limit and recovery

The fixed recursive aggregate computes the sum of 1 through 100,000 and
returns one INTEGER. Connection acquisition is included in reported totals;
statements are prepared fresh. Cold means the first read connection acquisition
after a write; warm means the same connection has been used previously.

| Detailed VFS metrics | Cold IC instructions | Warm IC instructions | 1,000,000-instruction budget |
| --- | ---: | ---: | ---: |
| Disabled | 420,004,552 | 419,624,439 | interrupted at 1,020,738 |
| Enabled | 420,004,786 | 419,624,437 | interrupted at 1,020,736 |

The interrupted query returned `QueryError::BudgetExceeded(Instructions)`.
The same cached connection could read a previously committed sentinel row
immediately afterward, in the same message. The test uses a progress interval
of 100 SQLite VM steps. This observed overshoot is not a maximum guarantee:
callback spacing, VFS work and mapper work can produce larger overshoots.
Zero instruction budget, row overflow, byte overflow and successful execution
with a larger instruction budget are also tested.

## Parameter binding interruption

`SELECT ?1000` binds 1,000 references to a 64 KiB blob with a 10,000,000
instruction soft budget. The original reproduction with SQLite 3.51.3,
before checking between parameter copies, consumed 67,504,779 instructions (66,897,852 during binding). With checks before
and after each copy, the current build interrupts at 10,117,741 instructions
(9,962,423 during binding) without detailed metrics, and 10,117,739 with them. The same-message sentinel
read succeeds afterward. A single copy can still overshoot the budget; input
value limits and cleanup headroom remain necessary. The regression ceiling is
12,000,000 instructions for this fixed case.

## Collection overhead

A second fixed recursive query returns INTEGERs 1 through 10,000. All three
paths prepare a fresh statement on a warm connection and collect the same
`Vec<i64>`. Totals are sampled outside each API, before result comparison and
Candid encoding. The budgeted path uses a 1,000,000,000-instruction soft limit,
10,000 rows, 80,000 payload bytes and a progress interval of 1,000 VM steps.

| Path | VFS metrics disabled | VFS metrics enabled | Overhead vs plain (disabled) |
| --- | ---: | ---: | ---: |
| Existing `Db::query` + `query_all` | 47,839,045 | 47,856,292 | baseline |
| `Db::query_profiled` | 50,443,222 | 50,460,540 | about 5.5% |
| `Db::query_with_budget` | 57,685,939 | 57,703,254 | about 20.6% |

The regression test allows 25% overhead for profiling and 50% for budgeted
collection on this workload. These deliberately loose ceilings catch large
regressions without promising a universal overhead bound. Instruction-budget
checks at Rust row boundaries account for part of the budgeted cost. Profiling
without an instruction budget avoids per-row performance-counter reads.

These recursive workloads are CPU-heavy and have little stable data I/O.
The feature comparison does not establish overhead for I/O-heavy queries.

## Budgeted update and commit

The fixed insert generates 1,000 rows with 128-byte blobs. With a 100,000,000
instruction budget and 20,000,000 reserved for publication/cleanup, it consumes
9,322,577 instructions, including 498,150 for SQLite COMMIT and stable
publication. It commits 11 dirty 16 KiB pages (180,224 bytes). Detailed metrics
produce 9,321,300 total / 496,921 commit instructions on the same workload.

A 100,000-row insert with a 2,000,000 total budget and 1,000,000 reserve fails
with `UpdateError::BudgetExceeded(Instructions)` at 1,132,445 instructions,
leaving zero rows. One-page and sub-page byte caps also fail with zero stored
rows, including when pages flush during COMMIT. Follow-up reads succeed in
the same message. Host tests additionally cover trigger effects, DDL, FTS5,
constraints, forbidden transaction-control SQL and multiple database handles.

The test warms the same table shape, then deliberately configures a tiny
one-instruction reserve and a soft total just below the previous observed cost.
The published update returns success, retains all 1,000 rows and sets
`committed_over_soft_limit`, rather than returning a misleading failure after
publication. This tests reporting semantics; a tiny reserve is not an operating
recommendation. Commit cost depends on dirty pages and workload, and none of
these numbers guarantees safety below an IC message ceiling.

## Wasm size

The same reference canister with all test probes included has the following
uncompressed release Wasm sizes:

| Build | Bytes |
| --- | ---: |
| `sqlite-precompiled,canister-api-test-failpoints` | 1,794,157 |
| Above plus `query-metrics` | 1,801,704 |

Optional detailed instrumentation adds 7,547 bytes (about 0.42%) in this fixture.
This compares the optional feature, not total size growth against the previous
library release. Heap high-water usage was not measured in this experiment.

## Reproduce

```sh
npm ci
npm run test:pocketic:query-budget

cargo build --release --target wasm32-unknown-unknown --no-default-features \
  --features sqlite-precompiled,canister-api-test-failpoints,query-metrics
cp target/wasm32-unknown-unknown/release/ic_sqlite_vfs.wasm \
  target/pocketic/ic_sqlite_vfs_query_metrics.wasm
QUERY_BUDGET_WASM=target/pocketic/ic_sqlite_vfs_query_metrics.wasm \
  node --test tests/pocketic/query_budget.test.mjs
```

The npm package's postinstall currently downloads an x86_64 macOS binary.
On an arm64 Mac without x86 emulation, use the matching official PocketIC
12.0.0 `pocket-ic-arm64-darwin.gz` asset in
`node_modules/@dfinity/pic/pocket-ic` before running the tests. Both measurements
above used the same server version.
