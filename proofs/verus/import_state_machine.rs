//! Verus model for the stable blob import state machine.
//!
//! The model covers begin, sequential chunks, incomplete finish rejection,
//! cancel, and update rejection while importing.
//!
//! Capacity proof mapping:
//! - T7: import is an explicit capacity exception; checksum mismatch preserves
//!   the logical committed image instead of publishing staging bytes, while
//!   staging writes may still increase the resource high-water mark.

use vstd::prelude::*;

verus! {

struct ImportState {
    importing: bool,
    written_until: nat,
    total_size: nat,
    base_offset: nat,
    expected_checksum: nat,
}

struct CommittedImage {
    db_base_offset: nat,
    db_size: nat,
    checksum: nat,
    schema_version: nat,
}

struct ResourceHighWater {
    allocated_bytes: nat,
    high_water_mark: nat,
}

struct ImportMachine {
    committed: CommittedImage,
    resource: ResourceHighWater,
    import: ImportState,
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

spec fn max_nat(left: nat, right: nat) -> nat {
    if left >= right {
        left
    } else {
        right
    }
}

spec fn imported_schema_version_after_finish() -> nat {
    0
}

spec fn staging_total_end(state: ImportState) -> nat {
    state.base_offset + state.total_size
}

spec fn staging_written_end(state: ImportState) -> nat {
    state.base_offset + state.written_until
}

spec fn allocated_after_staging(resource: ResourceHighWater, state: ImportState) -> nat {
    max_nat(resource.allocated_bytes, staging_total_end(state))
}

spec fn allocated_after_written_staging(resource: ResourceHighWater, state: ImportState) -> nat {
    max_nat(resource.allocated_bytes, staging_written_end(state))
}

spec fn resource_after_complete_staging(
    resource: ResourceHighWater,
    state: ImportState,
) -> ResourceHighWater {
    let allocated = allocated_after_staging(resource, state);
    ResourceHighWater { allocated_bytes: allocated, high_water_mark: allocated }
}

spec fn resource_after_written_staging(
    resource: ResourceHighWater,
    state: ImportState,
) -> ResourceHighWater {
    let allocated = allocated_after_written_staging(resource, state);
    ResourceHighWater { allocated_bytes: allocated, high_water_mark: allocated }
}

spec fn finish_import_machine(machine: ImportMachine, actual_checksum: nat) -> Option<ImportMachine> {
    if machine.import.importing && machine.import.written_until == machine.import.total_size {
        if actual_checksum == machine.import.expected_checksum {
            Some(ImportMachine {
                committed: CommittedImage {
                    db_base_offset: machine.import.base_offset,
                    db_size: machine.import.total_size,
                    checksum: actual_checksum,
                    schema_version: imported_schema_version_after_finish(),
                },
                resource: resource_after_complete_staging(machine.resource, machine.import),
                import: idle(),
            })
        } else {
            Some(ImportMachine {
                committed: machine.committed,
                resource: resource_after_complete_staging(machine.resource, machine.import),
                import: idle(),
            })
        }
    } else {
        None
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

spec fn cancel_import_machine(machine: ImportMachine) -> ImportMachine {
    if machine.import.importing {
        ImportMachine {
            committed: machine.committed,
            resource: resource_after_written_staging(machine.resource, machine.import),
            import: idle(),
        }
    } else {
        machine
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

proof fn checksum_mismatch_preserves_committed_image(
    machine: ImportMachine,
    actual_checksum: nat,
    next: ImportMachine,
)
    requires
        machine.import.importing,
        machine.import.written_until == machine.import.total_size,
        actual_checksum != machine.import.expected_checksum,
        finish_import_machine(machine, actual_checksum) == Some(next),
    ensures
        next.committed == machine.committed,
        next.resource.allocated_bytes >= machine.resource.allocated_bytes,
        next.resource.high_water_mark == next.resource.allocated_bytes,
        !next.import.importing,
{
}

proof fn checksum_match_publishes_staging_image(
    machine: ImportMachine,
    next: ImportMachine,
)
    requires
        machine.import.importing,
        machine.import.written_until == machine.import.total_size,
        finish_import_machine(machine, machine.import.expected_checksum) == Some(next),
    ensures
        next.committed.db_base_offset == machine.import.base_offset,
        next.committed.db_size == machine.import.total_size,
        next.committed.checksum == machine.import.expected_checksum,
        next.committed.schema_version == 0,
        next.resource.allocated_bytes == allocated_after_staging(machine.resource, machine.import),
        next.resource.allocated_bytes >= machine.resource.allocated_bytes,
        next.resource.high_water_mark == next.resource.allocated_bytes,
        !next.import.importing,
{
}

proof fn successful_import_resets_schema_version(
    machine: ImportMachine,
    next: ImportMachine,
)
    requires
        machine.import.importing,
        machine.import.written_until == machine.import.total_size,
        finish_import_machine(machine, machine.import.expected_checksum) == Some(next),
    ensures
        next.committed.schema_version == 0,
{
}

proof fn complete_import_may_increase_resource_high_water(
    machine: ImportMachine,
    actual_checksum: nat,
    next: ImportMachine,
)
    requires
        machine.import.importing,
        machine.import.written_until == machine.import.total_size,
        finish_import_machine(machine, actual_checksum) == Some(next),
    ensures
        next.resource.allocated_bytes == allocated_after_staging(machine.resource, machine.import),
        next.resource.allocated_bytes >= machine.resource.allocated_bytes,
        next.resource.high_water_mark == next.resource.allocated_bytes,
{
}

proof fn abort_clears_importing(state: ImportState)
    ensures
        !abort_import(state).importing,
{
}

proof fn cancel_preserves_committed_image(machine: ImportMachine)
    ensures
        cancel_import_machine(machine).committed == machine.committed,
        cancel_import_machine(machine).resource.allocated_bytes >= machine.resource.allocated_bytes,
        !cancel_import_machine(machine).import.importing,
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
