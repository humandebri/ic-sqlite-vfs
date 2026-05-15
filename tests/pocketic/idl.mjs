import { IDL } from "@dfinity/candid";

const result = (ok) => IDL.Variant({ Ok: ok, Err: IDL.Text });

export const idlFactory = ({ IDL }) => {
  const DbMeta = IDL.Record({
    db_size: IDL.Nat64,
    stable_pages: IDL.Nat64,
    stable_bytes: IDL.Nat64,
    schema_version: IDL.Nat64,
    last_tx_id: IDL.Nat64,
    flags: IDL.Nat64,
    checksum: IDL.Nat64,
    checksum_stale: IDL.Bool,
    checksum_refreshing: IDL.Bool,
    checksum_refresh_offset: IDL.Nat64,
    importing: IDL.Bool,
    import_written_until: IDL.Nat64,
    layout_version: IDL.Nat64,
    page_count: IDL.Nat64,
    page_table_bytes: IDL.Nat64,
    active_bytes: IDL.Nat64,
    allocated_bytes: IDL.Nat64,
    orphan_bytes_estimate: IDL.Nat64,
    orphan_ratio_basis_points: IDL.Nat64,
    compact_recommended: IDL.Bool
  });
  const ChecksumRefresh = IDL.Record({
    complete: IDL.Bool,
    checksum: IDL.Nat64,
    scanned_bytes: IDL.Nat64,
    db_size: IDL.Nat64
  });
  return IDL.Service({
    kv_put: IDL.Func([IDL.Text, IDL.Text], [result(IDL.Null)], []),
    kv_get: IDL.Func([IDL.Text], [result(IDL.Opt(IDL.Text))], ["query"]),
    kv_get_many: IDL.Func([IDL.Vec(IDL.Text)], [result(IDL.Vec(IDL.Opt(IDL.Text)))], ["query"]),
    kv_set_note: IDL.Func([IDL.Text, IDL.Text], [result(IDL.Null)], []),
    kv_get_note: IDL.Func([IDL.Text], [result(IDL.Opt(IDL.Text))], ["query"]),
    kv_count: IDL.Func([], [result(IDL.Nat64)], ["query"]),
    db_meta: IDL.Func([], [result(DbMeta)], ["query"]),
    db_integrity_check: IDL.Func([], [result(IDL.Text)], ["query"]),
    db_checksum: IDL.Func([], [result(IDL.Nat64)], ["query"]),
    db_refresh_checksum: IDL.Func([], [result(IDL.Nat64)], []),
    db_refresh_checksum_chunk: IDL.Func([IDL.Nat64], [result(ChecksumRefresh)], []),
    db_export_chunk: IDL.Func([IDL.Nat64, IDL.Nat64], [result(IDL.Vec(IDL.Nat8))], ["query"]),
    db_begin_import: IDL.Func([IDL.Nat64, IDL.Nat64], [result(IDL.Null)], []),
    db_import_chunk: IDL.Func([IDL.Nat64, IDL.Vec(IDL.Nat8)], [result(IDL.Null)], []),
    db_finish_import: IDL.Func([], [result(IDL.Null)], []),
    db_cancel_import: IDL.Func([], [result(IDL.Null)], []),
    db_compact: IDL.Func([], [result(IDL.Null)], []),
    db_test_sqlite_feature_probe: IDL.Func([], [result(IDL.Null)], []),
    db_test_trap_after_stable_write: IDL.Func([IDL.Nat64], [result(IDL.Null)], []),
    db_test_clear_failpoints: IDL.Func([], [result(IDL.Null)], [])
  });
};

export { IDL };
