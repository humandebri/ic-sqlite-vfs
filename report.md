# Review Code Report

## Summary
- Scope: Rust library, bundled SQLite VFS, stable-memory layout, MemoryManager fork, reference canister, examples, compatibility fixtures, release/test automation
- Last updated: 2026-07-11
- Commands: `cargo fmt --check`; release/public-API/no-await/package gates; `cargo test --all-targets`; `cargo test --features canister-api`; Clippy default/API with `-D warnings`; native/release/Wasm/example builds; compatibility fixture; `VERUS_REQUIRED=1 bash scripts/sqlite-critical-check.sh`; PocketIC regression/performance; 30-second `state_ops` fuzz campaign
- Current phase: complete; all discovered surfaces retested after fixes
- Story counts: TODO 0 / TESTING 0 / PASS 31 / FAIL 0 / FIXED 0 / RETEST PASS 1 / BLOCKED 0
- Finding counts: P0 0 / P1 1 / P2 0 / P3 1
- Blocking issues: none
- Untested scope: bundled-incompatible SQLite public TCL cases and private TH3; neither is part of the repository-defined icstable VFS release matrix

## Status Legend
- TODO: story identified but not tested
- TESTING: currently under test
- PASS: verified behavior matches expected behavior
- FAIL: verified user-facing error exists
- FIXED: fix applied, awaiting retest
- RETEST PASS: fixed and verified after retest
- BLOCKED: cannot test because of missing setup, auth, service, data, or unclear requirement

## Allowed Status Transitions
| From | To |
|---|---|
| TODO | TESTING |
| TESTING | PASS / FAIL / BLOCKED |
| FAIL | FIXED / BLOCKED / deferred |
| FIXED | RETEST PASS / FAIL / BLOCKED |
| PASS | RETEST PASS / FAIL |

## Feature Inventory
| ID | Surface | Entry Point | Evidence | Stories | Status |
|---|---|---|---|---|---|
| FI-01 | Initialization and public facade | `Db`, `DbHandle` | `src/db/mod.rs`, `tests/strict_api.rs` | US-001..US-004 | PASS |
| FI-02 | Update/query transactions | `Db::update`, `Db::query` | `src/db/transaction.rs`, `tests/typed_api.rs` | US-005..US-008 | PASS |
| FI-03 | Typed SQL and statements | `Connection`, `Statement`, `Row` | `src/db/connection/mod.rs`, `src/db/statement.rs` | US-009..US-013 | PASS |
| FI-04 | Migrations and SQLite features | `Db::migrate`, SQLite pragmas | `src/db/migrate.rs`, `tests/sqlite_features.rs`, `tests/fts5.rs` | US-014..US-016 | PASS |
| FI-05 | Checksums and integrity | checksum facade | `tests/checksum.rs`, `src/sqlite_vfs/stable_blob/ops.rs` | US-017..US-019 | PASS |
| FI-06 | VFS data semantics | SQLite VFS callbacks | `tests/vfs_roundtrip.rs`, `tests/stress.rs` | US-020..US-023 | PASS |
| FI-07 | Failure atomicity | overlay and failpoints | `src/sqlite_vfs/failpoint_tests.rs`, `tests/pocketic/upgrade.test.mjs` | US-024..US-025 | PASS |
| FI-08 | Stable-memory safety | superblock/layout validation | `tests/stable_memory.rs`, `tests/memory_manager_corruption.rs` | US-026..US-027 | PASS |
| FI-09 | MemoryManager compatibility | local fork and upstream 0.7 | `tests/memory_manager_compat.rs`, `compat-fixtures/` | US-028..US-029 | PASS |
| FI-10 | Canister upgrades and capacity | reference canister | `tests/pocketic/upgrade.test.mjs`, `tests/pocketic/perf_regression.test.mjs` | US-030 | PASS |
| FI-11 | Downstream build and packaging | feature matrix and package gates | `Cargo.toml`, `scripts/check-release-package.sh` | US-031 | PASS |
| FI-12 | Example and release workflow | minimal KV, CI/release scripts | `examples/minimal-kv`, `.github/workflows/ci.yml`, `scripts/sqlite-critical-check.sh` | US-032 | RETEST PASS |

## User Stories
| ID | Surface | Story | Expected Behavior | Story Evidence | Status | Test Method | Test Evidence | Findings |
|---|---|---|---|---|---|---|---|---|
| US-001 | Initialization | As a canister author, I can initialize one default database on a selected virtual memory. | Fresh memory initializes once and becomes query/update capable. | `src/db/mod.rs:90`, `tests/strict_api.rs:14` | PASS | Rust integration tests | Passed; command-level evidence is summarized in Retest Log. | - |
| US-002 | Initialization | As a canister author, I get typed errors before initialization or on duplicate initialization. | No implicit backend use or silent reinitialization occurs. | `src/db/mod.rs:34`, `tests/strict_api.rs:39` | PASS | Rust integration tests | Passed; command-level evidence is summarized in Retest Log. | - |
| US-003 | Multi database | As a canister author, I can use multiple independent `DbHandle`s. | Data and cached connections stay isolated by context. | `src/db/mod.rs:85`, `tests/typed_api.rs:682` | PASS | Rust integration tests | Passed; command-level evidence is summarized in Retest Log. | - |
| US-004 | Multi database | As a canister author, I cannot register the same memory identity twice. | Duplicate backing-memory/MemoryId registration is rejected. | `src/stable/memory.rs:104`, `tests/strict_api.rs:130` | PASS | Rust integration tests | Passed; command-level evidence is summarized in Retest Log. | - |
| US-005 | Update | As an application, I can commit a synchronous SQL update atomically. | Successful closure commits and returns its value. | `src/db/transaction.rs:64`, `tests/typed_api.rs:14` | PASS | Rust integration tests | Passed; command-level evidence is summarized in Retest Log. | - |
| US-006 | Rollback | As an application, a failed update leaves the previous database image active. | Closure, SQLite, bind, and commit failures do not publish partial state. | `src/db/transaction.rs:64`, `src/sqlite_vfs/failpoint_tests.rs:75` | PASS | Unit/integration failpoint tests | Passed; command-level evidence is summarized in Retest Log. | - |
| US-007 | Query | As an application, I can perform read-only queries without mutating data. | Query connections enforce query-only behavior. | `src/db/mod.rs:214`, `tests/typed_api.rs:63` | PASS | Rust integration tests | Passed; command-level evidence is summarized in Retest Log. | - |
| US-008 | Nested access | As an application, conflicting nested read/write access is rejected without corrupting state. | Active reads block mutation; safe supported nesting retains connection state. | `src/db/mod.rs:195`, `src/db/connection/tests.rs:93` | PASS | Rust unit/integration tests | Passed; command-level evidence is summarized in Retest Log. | - |
| US-009 | Binding | As an application, I can bind positional and named integer/text/blob/null values. | Values round-trip with correct SQLite types. | `src/db/value.rs`, `tests/typed_api.rs:86` | PASS | Rust integration tests | Passed; command-level evidence is summarized in Retest Log. | - |
| US-010 | Validation | As an application, malformed SQL and parameter mismatches return typed errors. | NUL, empty/trailing SQL, missing/anonymous/extra parameters are rejected. | `src/db/mod.rs:34`, `tests/typed_api.rs:353` | PASS | Rust integration tests | Passed; command-level evidence is summarized in Retest Log. | - |
| US-011 | Results | As an application, I can retrieve one, optional, all, column, and scalar results. | Cardinality helpers return values or the documented `NotFound`/optional result. | `src/db/statement.rs:406`, `tests/typed_api.rs:236` | PASS | Rust integration tests | Passed; command-level evidence is summarized in Retest Log. | - |
| US-012 | Results | As an application, invalid column indexes/types return typed errors. | No unchecked conversion or out-of-range read occurs. | `src/db/row.rs:34`, `tests/typed_api.rs:278` | PASS | Rust integration tests | Passed; command-level evidence is summarized in Retest Log. | - |
| US-013 | Statement cache | As an application, I can reuse cached statements safely across executions and failures. | Reset/clear-binding behavior prevents stale values and connection poisoning. | `src/db/connection/mod.rs:224`, `src/db/connection/tests.rs:24` | PASS | Rust unit/integration tests | Passed; command-level evidence is summarized in Retest Log. | - |
| US-014 | Migrations | As an application, I can apply strictly ordered migrations exactly once. | Duplicate/out-of-order/oversized versions fail and committed schema version advances. | `src/db/migrate.rs`, `tests/typed_api.rs:547` | PASS | Rust integration tests | Passed; command-level evidence is summarized in Retest Log. | - |
| US-015 | SQLite settings | As an application, required page, foreign-key, temp, lock, and query-only pragmas are enforced. | Runtime settings match the documented durability model. | `src/db/pragmas.rs`, `tests/sqlite_features.rs:17` | PASS | Rust integration tests | Passed; command-level evidence is summarized in Retest Log. | - |
| US-016 | SQLite features | As an application, I can use the documented bundled SQLite features including FTS5. | Feature probes and FTS queries succeed in native and supported Wasm builds. | `src/api/sqlite_feature_probe.rs`, `tests/fts5.rs:18` | PASS | Rust tests/build matrix | Passed; command-level evidence is summarized in Retest Log. | - |
| US-017 | Checksum | As an operator, I can detect when stored checksum metadata is stale. | Normal updates mark checksum stale and do not claim verification. | `src/sqlite_vfs/stable_blob/ops.rs:350`, `tests/checksum.rs:14` | PASS | Rust integration tests | Passed; command-level evidence is summarized in Retest Log. | - |
| US-018 | Checksum | As an operator, I can refresh the checksum in bounded chunks. | Progress is monotonic, bounded, resumable, and finishes at the full checksum. | `src/sqlite_vfs/stable_blob/ops.rs:298`, `tests/checksum.rs:88` | PASS | Rust integration tests | Passed; command-level evidence is summarized in Retest Log. | - |
| US-019 | Integrity | As an operator, I can run SQLite integrity checking and obtain its result. | A healthy image reports `ok` without mutation. | `src/db/mod.rs:239`, `tests/typed_api.rs:779` | PASS | Rust integration tests | Passed; command-level evidence is summarized in Retest Log. | - |
| US-020 | VFS I/O | As SQLite, I can read/write/truncate/sync the main image at arbitrary valid offsets. | Bytes, file size, short reads, and zero-fill semantics match SQLite VFS rules. | `src/sqlite_vfs/vfs.rs`, `tests/vfs_roundtrip.rs:201` | PASS | VFS integration tests | Passed; command-level evidence is summarized in Retest Log. | - |
| US-021 | Temp I/O | As SQLite, I can use isolated heap-backed journal/temp files. | Temp files resize, zero-fill, and never persist into stable DB storage. | `src/sqlite_vfs/temp.rs`, `tests/vfs_roundtrip.rs:390` | PASS | VFS integration tests | Passed; command-level evidence is summarized in Retest Log. | - |
| US-022 | Sparse/truncate | As SQLite, truncated or sparse regions remain logically zero without exposing stale bytes. | Zero extents normalize and materialize correctly within their fixed limit. | `src/sqlite_vfs/overlay.rs`, `src/sqlite_vfs/stable_blob/tests.rs:572` | PASS | Property/stress tests | Passed; command-level evidence is summarized in Retest Log. | - |
| US-023 | Determinism/stress | As a replicated canister, deterministic operation sequences preserve consistent SQLite state. | Reopen, churn, random/blob operations, and deterministic RNG behavior remain consistent. | `tests/stress.rs`, `tests/vfs_roundtrip.rs:1120` | PASS | Stress/property tests | Passed; command-level evidence is summarized in Retest Log. | - |
| US-024 | Native failure safety | As an application, injected VFS and stable-memory failures return errors and preserve committed metadata. | No failed step publishes a partial transaction. | `src/sqlite_vfs/failpoint_tests.rs` | PASS | Rust failpoint tests | Passed; command-level evidence is summarized in Retest Log. | - |
| US-025 | IC rollback safety | As a canister, a trap after dirty writes but before superblock publish rolls back stable writes. | PocketIC upgrade/failpoint scenario retains old data and metadata. | `tests/pocketic/upgrade.test.mjs` | PASS | PocketIC regression | Passed; command-level evidence is summarized in Retest Log. | - |
| US-026 | Foreign/corrupt image | As an operator, foreign, unsupported, truncated, or corrupt stable images are rejected. | Initialization does not overwrite them; strict mode returns typed errors where contracted. | `tests/stable_memory.rs:69`, `tests/memory_manager_corruption.rs` | PASS | Rust integration tests | Passed; command-level evidence is summarized in Retest Log. | - |
| US-027 | Layout persistence | As an operator, a valid v8 image survives reload/upgrade with metadata and data intact. | Superblock encoding and database offsets remain stable. | `src/stable/meta/tests.rs`, `tests/stable_memory.rs:129` | PASS | Unit/integration/PocketIC tests | Passed; command-level evidence is summarized in Retest Log. | - |
| US-028 | MemoryManager | As a canister author, virtual memories grow/read/write independently and safely. | Bounds, allocation metadata, grow failure, and reload behavior are correct. | `src/stable/memory_manager.rs`, `tests/memory_manager_compat.rs` | PASS | Unit/property tests | Passed; command-level evidence is summarized in Retest Log. | - |
| US-029 | Compatibility | As an existing 0.7 user, the forked MemoryManager layout remains interchangeable with supported upstream versions. | Parallel and cross-reload bytes match the supported compatibility matrix. | `compat-fixtures/common/memory_manager_matrix.rs` | PASS | Compatibility fixture tests | Passed; command-level evidence is summarized in Retest Log. | - |
| US-030 | Canister lifecycle | As an operator, install, update, query, upgrade, checksum refresh, and bounded capacity paths work in PocketIC. | Data persists across upgrade and resource/performance guards remain within committed thresholds. | `tests/pocketic/upgrade.test.mjs`, `tests/pocketic/perf_regression.test.mjs` | PASS | PocketIC regression/perf | Passed; command-level evidence is summarized in Retest Log. | - |
| US-031 | Distribution | As a downstream crate, I can build supported feature combinations and receive the frozen public/package surface. | Native, Wasm, bundled/precompiled, API snapshot, and package gates pass. | `Cargo.toml`, `docs/PUBLIC_API_2_0.snapshot` | PASS | Build/release scripts | Passed; command-level evidence is summarized in Retest Log. | - |
| US-032 | Adoption workflow | As a maintainer or new user, the example and documented CI/release commands are executable from the repository. | No command references missing files or stale fixture paths. | `examples/minimal-kv`, `.github/workflows/ci.yml`, `scripts/sqlite-critical-check.sh` | RETEST PASS | Example builds and script execution | Passed; command-level evidence is summarized in Retest Log. | F-001, F-002 |

## Findings
| ID | Severity | Story | Type | Evidence | Expected | Actual | Reproduction | Fix Status |
|---|---|---|---|---|---|---|---|---|
| F-001 | P1 | US-032 | logistical | `scripts/sqlite-critical-check.sh` referenced a deleted fixed-0.7.0 fixture and required a latest-0.7.x fixture that was also deleted | The mandatory CI/release gate covers fixed 0.7.0, fixed 0.7.2, and the latest available 0.7.x using present test entry points. | Gate stopped at missing manifests, while simply removing both invocations would weaken the required latest-0.7.x compatibility check. | Run the critical gate before the fix. | retest-pass |
| F-002 | P3 | US-032 | requirement-conflict | `refactor.md` described stale fixture consumers and script line numbers. | Maintainer documentation describes the active compatibility matrix. | Fixed 0.7.0 is covered by the root integration test; fixed 0.7.2 and latest 0.7.x share the independent fixture matrix. | Compare `refactor.md` with `compat-fixtures/`, `Cargo.toml`, and the critical script. | retest-pass |

## Fix Log
| Finding | Change | Files | Verification |
|---|---|---|---|
| F-001 | Removed the duplicate fixed-0.7.0 fixture invocation, restored the latest-0.7.x fixture, and kept fixed 0.7.2 plus latest 0.7.x as mandatory fixture gates. | `scripts/sqlite-critical-check.sh`, `compat-fixtures/` | Fixed 0.7.0, fixed 0.7.2, and latest 0.7.x compatibility tests passed. |
| F-002 | Updated the compatibility-matrix description to match the root 0.7.0 comparison and the fixed/latest independent fixtures. | `refactor.md` | Repository-wide references were reconciled with the active files and commands. |

## Retest Log
| Story | Scope | Result | Evidence |
|---|---|---|---|
| US-001..US-029 | Native/library/VFS/stable-memory matrix | PASS | Both default and `canister-api` suites passed; all-target run passed; Clippy default/API passed with warnings denied. |
| US-030 | PocketIC lifecycle, rollback, permissions, performance, capacity | PASS | Upgrade 4/4 and performance 1/1 passed; integrated gate repeated both successfully. |
| US-031 | Build/distribution matrix | PASS | Public API, package, release, native, Wasm bundled/precompiled, and example gates passed. |
| US-032 | Release/adoption workflow after fixes | RETEST PASS | The required gate covers the root fixed-0.7.0 test, fixed-0.7.2 fixture, and updated latest-0.7.x fixture before downstream stages. |
| US-022..US-024 | Additional state-machine exploration | PASS | libFuzzer completed 2,600 runs in 31 seconds with no crash; coverage 1,381, feature count 7,021. |

## E2E Candidates
| Story | Reason | Suggested Coverage |
|---|---|---|
| US-006 | Transaction rollback is data-safety critical. | Commit and each injected failure stage with reopen verification. |
| US-025 | IC trap rollback is a release-blocking runtime contract. | PocketIC failpoint around dirty write/superblock publish. |
| US-026 | Foreign/corrupt image rejection protects existing data. | Fixture matrix for raw SQLite, unsupported version, truncation, corrupt metadata. |
| US-030 | Upgrade and resource behavior is high regression risk. | Install/write/upgrade/read plus capacity/checksum assertions. |

## Untested / Deferred Scope
| Surface | Reason | Next Action |
|---|---|---|
| SQLite public TCL tests excluded by bundled compile flags | The bundled-equivalent advisory run excludes the documented incompatibility allowlist. | Re-run the documented applicable subset when the SQLite source ID or compile flags change. |
| TH3 | SQLite TH3 is private and paid. | Treat the public suite and repository-defined VFS gates as the available evidence. |

## Open Questions
| ID | Question | Why It Blocks |
|---|---|---|
