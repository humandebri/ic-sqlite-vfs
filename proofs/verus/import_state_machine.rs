//! Verus model for the stable blob import state machine.
//!
//! The model covers begin, sequential chunks, incomplete finish rejection,
//! abort, and update rejection while importing.

use vstd::prelude::*;

verus! {

struct ImportState {
    importing: bool,
    written_until: nat,
    total_size: nat,
    base_offset: nat,
    expected_checksum: nat,
}

spec fn idle() -> ImportState {
    ImportState {
        importing: false,
        written_until: 0,
        total_size: 0,
        base_offset: 0,
        expected_checksum: 0,
    }
}

spec fn begin_import(total_size: nat, base_offset: nat, expected_checksum: nat) -> ImportState {
    ImportState {
        importing: true,
        written_until: 0,
        total_size,
        base_offset,
        expected_checksum,
    }
}

spec fn import_chunk(state: ImportState, offset: nat, len: nat) -> Option<ImportState> {
    if state.importing && offset == state.written_until && offset + len <= state.total_size {
        Some(ImportState {
            importing: true,
            written_until: offset + len,
            total_size: state.total_size,
            base_offset: state.base_offset,
            expected_checksum: state.expected_checksum,
        })
    } else {
        None
    }
}

spec fn finish_import_state(state: ImportState, actual_checksum: nat) -> Option<ImportState> {
    if state.importing && state.written_until == state.total_size {
        Some(idle())
    } else {
        None
    }
}

spec fn finish_import_success(state: ImportState, actual_checksum: nat) -> bool {
    state.importing
        && state.written_until == state.total_size
        && actual_checksum == state.expected_checksum
}

spec fn finish_import_checksum_mismatch(state: ImportState, actual_checksum: nat) -> bool {
    state.importing
        && state.written_until == state.total_size
        && actual_checksum != state.expected_checksum
}

spec fn abort_import(state: ImportState) -> ImportState {
    if state.importing {
        idle()
    } else {
        state
    }
}

spec fn update_allowed(state: ImportState) -> bool {
    !state.importing
}

proof fn begin_import_enters_importing(total_size: nat, base_offset: nat, expected_checksum: nat)
    ensures
        begin_import(total_size, base_offset, expected_checksum).importing,
        begin_import(total_size, base_offset, expected_checksum).written_until == 0,
        begin_import(total_size, base_offset, expected_checksum).total_size == total_size,
        begin_import(total_size, base_offset, expected_checksum).base_offset == base_offset,
        begin_import(total_size, base_offset, expected_checksum).expected_checksum
            == expected_checksum,
{
}

proof fn accepted_chunk_advances_monotonically(state: ImportState, len: nat, next: ImportState)
    requires
        import_chunk(state, state.written_until, len) == Some(next),
    ensures
        next.importing,
        next.written_until >= state.written_until,
        next.written_until <= state.total_size,
        next.total_size == state.total_size,
        next.base_offset == state.base_offset,
        next.expected_checksum == state.expected_checksum,
{
}

proof fn out_of_order_chunk_is_rejected(state: ImportState, offset: nat, len: nat)
    requires
        state.importing,
        offset != state.written_until,
    ensures
        import_chunk(state, offset, len) == Option::<ImportState>::None,
{
}

proof fn incomplete_finish_is_rejected(state: ImportState)
    requires
        state.importing,
        state.written_until != state.total_size,
    ensures
        finish_import_state(state, state.expected_checksum) == Option::<ImportState>::None,
{
}

proof fn complete_finish_returns_idle(state: ImportState)
    requires
        state.importing,
        state.written_until == state.total_size,
    ensures
        finish_import_state(state, state.expected_checksum) == Some(idle()),
        finish_import_success(state, state.expected_checksum),
{
}

proof fn checksum_mismatch_finish_clears_importing(state: ImportState, actual_checksum: nat)
    requires
        state.importing,
        state.written_until == state.total_size,
        actual_checksum != state.expected_checksum,
    ensures
        finish_import_checksum_mismatch(state, actual_checksum),
        finish_import_state(state, actual_checksum) == Some(idle()),
        !idle().importing,
{
}

proof fn abort_clears_importing(state: ImportState)
    ensures
        !abort_import(state).importing,
{
}

proof fn update_is_rejected_while_importing(state: ImportState)
    requires
        state.importing,
    ensures
        !update_allowed(state),
{
}

}
