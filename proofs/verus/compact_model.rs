//! Verus model for compacting page tables.
//!
//! The model proves that compacting preserves logical page contents, keeps zero
//! pages zero, and assigns dense physical offsets to non-zero pages.

use vstd::prelude::*;

verus! {

spec fn page_size() -> nat {
    16_384
}

spec fn is_non_zero_page(page: nat) -> bool {
    page != 0
}

spec fn non_zero_count_before(table: Seq<nat>, index: nat) -> nat
    decreases index,
{
    if index == 0 {
        0
    } else {
        let previous = (index - 1) as nat;
        non_zero_count_before(table, previous)
            + if is_non_zero_page(table[previous as int]) { 1nat } else { 0nat }
    }
}

spec fn compacted_offset(table: Seq<nat>, base: nat, index: nat) -> nat {
    if is_non_zero_page(table[index as int]) {
        base + non_zero_count_before(table, index) * page_size()
    } else {
        0
    }
}

spec fn logical_page_preserved(old_table: Seq<nat>, new_table: Seq<nat>, index: nat) -> bool {
    if old_table[index as int] == 0 {
        new_table[index as int] == 0
    } else {
        new_table[index as int] != 0
    }
}

spec fn compacted_page_content(
    old_table: Seq<nat>,
    old_contents: Seq<nat>,
    new_contents: Seq<nat>,
    index: nat,
) -> nat {
    if old_table[index as int] == 0 {
        0
    } else {
        new_contents[index as int]
    }
}

spec fn compact_preserves_content_at(
    old_table: Seq<nat>,
    old_contents: Seq<nat>,
    new_contents: Seq<nat>,
    index: nat,
) -> bool {
    if old_table[index as int] == 0 {
        new_contents[index as int] == 0
    } else {
        new_contents[index as int] == old_contents[index as int]
    }
}

spec fn compacted_table_entry(old_table: Seq<nat>, base: nat, index: nat) -> nat {
    compacted_offset(old_table, base, index)
}

spec fn compacted_content_entry(
    old_table: Seq<nat>,
    old_contents: Seq<nat>,
    index: nat,
) -> nat {
    if old_table[index as int] == 0 {
        0
    } else {
        old_contents[index as int]
    }
}

spec fn compact_entry_relation(
    old_table: Seq<nat>,
    new_table: Seq<nat>,
    old_contents: Seq<nat>,
    new_contents: Seq<nat>,
    base: nat,
    index: nat,
) -> bool {
    new_table[index as int] == compacted_table_entry(old_table, base, index)
        && new_contents[index as int] == compacted_content_entry(old_table, old_contents, index)
}

proof fn zero_page_remains_zero(table: Seq<nat>, base: nat, index: nat)
    requires
        index < table.len(),
        table[index as int] == 0,
    ensures
        compacted_offset(table, base, index) == 0,
{
}

proof fn non_zero_page_gets_dense_offset(table: Seq<nat>, base: nat, index: nat)
    requires
        index < table.len(),
        table[index as int] != 0,
    ensures
        compacted_offset(table, base, index)
            == base + non_zero_count_before(table, index) * page_size(),
        compacted_offset(table, base, index) >= base,
{
}

proof fn non_zero_count_is_bounded_by_index(table: Seq<nat>, index: nat)
    by (nonlinear_arith)
    requires
        index <= table.len(),
    ensures
        non_zero_count_before(table, index) <= index,
    decreases index,
{
    if index > 0 {
        non_zero_count_is_bounded_by_index(table, (index - 1) as nat);
    }
}

proof fn compacted_offset_is_inside_compacted_data(table: Seq<nat>, base: nat, index: nat)
    by (nonlinear_arith)
    requires
        index < table.len(),
        table[index as int] != 0,
    ensures
        compacted_offset(table, base, index) < base + table.len() * page_size(),
{
    non_zero_count_is_bounded_by_index(table, index);
}

proof fn non_zero_page_preserves_logical_content(
    old_table: Seq<nat>,
    old_contents: Seq<nat>,
    new_contents: Seq<nat>,
    index: nat,
)
    requires
        index < old_table.len(),
        index < old_contents.len(),
        index < new_contents.len(),
        old_table[index as int] != 0,
        compact_preserves_content_at(old_table, old_contents, new_contents, index),
    ensures
        compacted_page_content(old_table, old_contents, new_contents, index)
            == old_contents[index as int],
{
}

proof fn zero_page_has_zero_logical_content_after_compact(
    old_table: Seq<nat>,
    old_contents: Seq<nat>,
    new_contents: Seq<nat>,
    index: nat,
)
    requires
        index < old_table.len(),
        index < old_contents.len(),
        index < new_contents.len(),
        old_table[index as int] == 0,
        compact_preserves_content_at(old_table, old_contents, new_contents, index),
    ensures
        compacted_page_content(old_table, old_contents, new_contents, index) == 0,
{
}

proof fn compact_relation_preserves_non_zero_page(
    old_table: Seq<nat>,
    new_table: Seq<nat>,
    old_contents: Seq<nat>,
    new_contents: Seq<nat>,
    base: nat,
    index: nat,
)
    requires
        index < old_table.len(),
        index < new_table.len(),
        index < old_contents.len(),
        index < new_contents.len(),
        old_table[index as int] != 0,
        compact_entry_relation(old_table, new_table, old_contents, new_contents, base, index),
    ensures
        new_table[index as int] == base + non_zero_count_before(old_table, index) * page_size(),
        new_contents[index as int] == old_contents[index as int],
{
}

proof fn compact_relation_preserves_zero_page(
    old_table: Seq<nat>,
    new_table: Seq<nat>,
    old_contents: Seq<nat>,
    new_contents: Seq<nat>,
    base: nat,
    index: nat,
)
    requires
        index < old_table.len(),
        index < new_table.len(),
        index < old_contents.len(),
        index < new_contents.len(),
        old_table[index as int] == 0,
        compact_entry_relation(old_table, new_table, old_contents, new_contents, base, index),
    ensures
        new_table[index as int] == 0,
        new_contents[index as int] == 0,
{
}

proof fn adjacent_non_zero_offsets_are_dense(table: Seq<nat>, base: nat, index: nat)
    by (nonlinear_arith)
    requires
        index + 1 < table.len(),
        table[index as int] != 0,
        table[(index + 1) as int] != 0,
    ensures
        compacted_offset(table, base, index + 1) == compacted_offset(table, base, index) + page_size(),
{
}

proof fn logical_zero_preservation(old_table: Seq<nat>, new_table: Seq<nat>, index: nat)
    requires
        index < old_table.len(),
        index < new_table.len(),
        old_table[index as int] == 0,
        new_table[index as int] == 0,
    ensures
        logical_page_preserved(old_table, new_table, index),
{
}

}
