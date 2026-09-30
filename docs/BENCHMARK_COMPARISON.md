# Benchmark comparison — 2026-09-30

## Result

The existing API workloads changed by **−0.683% to +0.659%** in measured IC instructions. The uncompressed benchmark Wasm grew by **14,215 bytes (+0.878%)**. Two baseline and two final-current full suite executions passed; intermediate runs also passed. Each execution produced 47 reports, including 34 instruction measurements; every report was identical between repetitions of the same build. Reported row counts, checksums, database sizes and stable-memory pages/bytes matched between baseline and current wherever those fields were present.

This is a small, reproducible cost increase for several existing operations; it is not zero overhead. These workloads did not show a large regression.

## Conditions

- Baseline: immutable HEAD `1386239acff1dd7ede5ac78a2f0a22ef495195de`, exported with `git archive` into an ignored source directory; SQLite 3.51.3.
- Current: working-tree implementation on `feat/query-budgets-and-metrics`, including SQLite 3.53.4, dependency updates and the query/update budget APIs. Includes the follow-up fixes for public-error compatibility and host panic cleanup.
- Both: Rust 1.95.0, `wasm32-unknown-unknown`, release, `--offline --locked`, benchmark manifest unchanged, `default-features = false`, `bench-profile,sqlite-precompiled`; PocketIC 12.0.0 arm64.
- Identical benchmark source and regression harness, SQL, data and scenario order. The temporary harness only selects a Wasm using an environment variable and saves structured reports. Each run uses a fresh PocketIC instance; connection warming follows the existing scenario sequence.
- Both Wasms import only `ic0`.
- Measurements use the existing endpoints’ `performance_counter(0)` scopes. They do not include full response encoding/transport, do not measure wall-clock latency and are not production cycle-price estimates.
- This comparison combines SQLite and library changes. It does not isolate the contribution of each change. The new budgeted APIs are not invoked by the existing benchmark.
- Stable-memory consumption is compared; heap high-water usage was not measured. No universal performance bound or production message-limit guarantee is implied.

## Existing workload IC instructions

| Workload | Baseline | Current | Change |
| --- | ---: | ---: | ---: |
| `bench_reset_1000_clean` | 10,786,493 | 10,813,205 | +0.248% |
| `bench_reset_1000_for_point_read` | 10,786,493 | 10,813,205 | +0.248% |
| `bench_read_1` | 47,969 | 48,016 | +0.098% |
| `bench_read_10` | 134,926 | 135,408 | +0.357% |
| `bench_read_100` | 1,007,546 | 1,012,438 | +0.486% |
| `bench_read_1000` | 10,047,793 | 10,101,735 | +0.537% |
| `bench_read_public_helper_1000` | 12,428,022 | 12,481,646 | +0.431% |
| `bench_read_prepare_each_1000` | 39,413,896 | 39,144,838 | -0.683% |
| `bench_read_profile` | 12,286,876 | 12,340,818 | +0.439% |
| `bench_reset_1000_for_write` | 10,786,493 | 10,813,205 | +0.248% |
| `bench_write_1000_clean` | 13,321,616 | 13,365,616 | +0.330% |
| `bench_reset_1000_for_write_profile` | 10,786,493 | 10,813,205 | +0.248% |
| `bench_write_profile` | 15,117,110 | 15,161,110 | +0.291% |
| `bench_insert_only_1000` | 10,282,618 | 10,313,287 | +0.298% |
| `bench_insert_only_5000` | 60,909,013 | 61,137,950 | +0.376% |
| `bench_append_insert_5000_1000` | 13,539,598 | 13,594,666 | +0.407% |
| `bench_update_only_1000` | 16,756,221 | 16,849,375 | +0.556% |
| `bench_update_only_5000` | 91,051,271 | 91,651,424 | +0.659% |
| `bench_reset_5000_clean` | 61,413,094 | 61,638,076 | +0.366% |
| `bench_many_rows_100` | 235,665 | 234,986 | -0.288% |
| `bench_many_rows_1000` | 1,371,369 | 1,370,694 | -0.049% |
| `bench_many_rows_5000` | 6,458,012 | 6,457,359 | -0.010% |
| `bench_get_many_in_100` | 1,307,604 | 1,314,198 | +0.504% |
| `bench_get_many_in_1000` | 14,351,276 | 14,420,035 | +0.479% |
| `bench_get_many_in_profile` | 14,355,117 | 14,423,876 | +0.479% |
| `bench_large_blob_64k` | 950,326 | 949,690 | -0.067% |
| `bench_large_blob_256k` | 2,205,993 | 2,205,387 | -0.027% |
| `bench_unbounded_order_by_5000` | 66,449,667 | 66,382,478 | -0.101% |
| `bench_join_2000` | 17,017,523 | 17,004,852 | -0.074% |
| `bench_growth_1000_20` | 3,391,510 | 3,404,792 | +0.392% |
| `bench_growth_5000_20` | 3,420,148 | 3,433,646 | +0.395% |
| `bench_growth_profile_1000_20` | 3,472,053 | 3,485,335 | +0.383% |
| `bench_capacity_growth_guard_1000_128` | 21,745,574 | 21,832,374 | +0.399% |
| `bench_capacity_growth_guard_5000_256` | 43,792,392 | 43,969,156 | +0.404% |

## Wasm size

Uncompressed benchmark artifacts; these are not the library archive size or a consumer application size guarantee.

| Build | Bytes | SHA-256 |
| --- | ---: | --- |
| baseline | 1,618,926 | `9f06ed7ce217bba9f9f5a22e6d84ff5f48af04de8346ff516552b552a9dd9b50` |
| current | 1,633,141 | `ed3b45542e212cc963bcab519698d9434a2ac8879ed260f920cd378d4bbeb514` |

## Opting into new query APIs

The separate fixed 10,000-row workload in [QUERY_BUDGET_MEASUREMENTS.md](QUERY_BUDGET_MEASUREMENTS.md) measures 47,839,045 instructions on the existing collection path, 50,443,222 on `query_profiled` (**+5.44%**), and 57,685,939 on `query_with_budget` (**+20.58%**). Those are additional costs when the new APIs are used; they are not the before/after KV benchmark figures above. Detailed optional VFS metrics are disabled in those numbers.

## Artifacts and reproduction

Local raw structured reports and test logs are retained under ignored `target/bench-comparison/`: `baseline.jsonl`, `current-final.jsonl`, `baseline-repeat.jsonl`, `current-final-repeat.jsonl`, and matching `.log` files. They include all 47 reports with storage and profiling fields. The temporary runner is `comparison.test.mjs`; original benchmark and test sources were not edited.

From the repository root, with the same dependencies and PocketIC binary installed:

```sh
mkdir -p target/bench-comparison/baseline-source
git archive 1386239acff1dd7ede5ac78a2f0a22ef495195de | tar -x -C target/bench-comparison/baseline-source

cargo build --manifest-path target/bench-comparison/baseline-source/benchmarks/kv-canister/Cargo.toml \
  --target wasm32-unknown-unknown --release --offline --locked \
  --target-dir target/bench-comparison/baseline-build
cargo build --manifest-path benchmarks/kv-canister/Cargo.toml \
  --target wasm32-unknown-unknown --release --offline --locked \
  --target-dir target/bench-comparison/current-build
```

Generate the temporary runner from the existing suite:

```python
from pathlib import Path
source = Path("tests/pocketic/perf_regression.test.mjs").read_text()
source = source.replace('import assert from "node:assert/strict";',
    'import assert from "node:assert/strict";\nimport { appendFileSync } from "node:fs";')
source = source.replace('from "./server.mjs"', 'from "../../tests/pocketic/server.mjs"')
source = source.replace('resolve("target/pocketic/ic_sqlite_vfs_kv_bench.wasm")',
    'resolve(process.env.BENCH_WASM)')
source = source.replace("const report = result.Ok;",
    'const report = result.Ok;\n  appendFileSync(process.env.BENCH_REPORT, '
    'JSON.stringify({name, ...report}, (_, v) => typeof v === "bigint" ? v.toString() : v) + "\\n");')
Path("target/bench-comparison/comparison.test.mjs").write_text(source)
```

Run once for each build, using a fresh output file for each repetition (the runner appends):

```sh
BENCH_WASM=target/bench-comparison/baseline-build/wasm32-unknown-unknown/release/ic_sqlite_vfs_kv_bench.wasm \
BENCH_REPORT=target/bench-comparison/baseline-fresh.jsonl \
node --test target/bench-comparison/comparison.test.mjs

BENCH_WASM=target/bench-comparison/current-build/wasm32-unknown-unknown/release/ic_sqlite_vfs_kv_bench.wasm \
BENCH_REPORT=target/bench-comparison/current-fresh.jsonl \
node --test target/bench-comparison/comparison.test.mjs
```
