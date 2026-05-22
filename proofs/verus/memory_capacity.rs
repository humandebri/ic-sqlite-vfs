//! Verus model for stable-memory capacity arithmetic.
//!
//! This proof covers the pure arithmetic behind `ensure_memory_capacity` and
//! `write_growing`: the computed extra page count is enough to contain the
//! requested end offset when checked arithmetic succeeds.

use vstd::prelude::*;

verus! {

spec fn stable_page_size() -> nat {
    65_536
}

spec fn u64_max() -> nat {
    18_446_744_073_709_551_615
}

spec fn checked_add(left: nat, right: nat) -> Option<nat> {
    if left + right <= u64_max() {
        Some(left + right)
    } else {
        None
    }
}

spec fn checked_mul(left: nat, right: nat) -> Option<nat> {
    if left * right <= u64_max() {
        Some(left * right)
    } else {
        None
    }
}

spec fn div_ceil(numerator: nat, denominator: nat) -> nat {
    if numerator == 0 {
        0
    } else {
        (((numerator - 1) / (denominator as int)) + 1) as nat
    }
}

spec fn extra_pages_needed(current_pages: nat, end_offset: nat) -> Option<nat> {
    match checked_mul(current_pages, stable_page_size()) {
        Some(current_bytes) => {
            if end_offset <= current_bytes {
                Some(0)
            } else {
                Some(div_ceil((end_offset - current_bytes) as nat, stable_page_size()))
            }
        },
        None => None,
    }
}

proof fn div_ceil_covers_positive(numerator: nat)
    by (nonlinear_arith)
    requires
        numerator > 0,
    ensures
        numerator <= div_ceil(numerator, stable_page_size()) * stable_page_size(),
        div_ceil(numerator, stable_page_size()) > 0,
{
}

proof fn div_ceil_zero()
    ensures
        div_ceil(0, stable_page_size()) == 0,
{
}

proof fn div_ceil_is_bounded_by_positive_numerator(numerator: nat)
    by (nonlinear_arith)
    requires
        numerator > 0,
    ensures
        div_ceil(numerator, stable_page_size()) <= numerator,
{
}

proof fn extra_pages_cover_end_offset(current_pages: nat, end_offset: nat, pages: nat)
    by (nonlinear_arith)
    requires
        extra_pages_needed(current_pages, end_offset) == Some(pages),
    ensures
        end_offset <= (current_pages + pages) * stable_page_size(),
{
    match checked_mul(current_pages, stable_page_size()) {
        Some(current_bytes) => {
            if end_offset <= current_bytes {
            } else {
                div_ceil_covers_positive((end_offset - current_bytes) as nat);
            }
        },
        None => {},
    }
}

proof fn grow_page_sum_stays_in_u64_for_u64_end(
    current_pages: nat,
    end_offset: nat,
    pages: nat,
)
    by (nonlinear_arith)
    requires
        end_offset <= u64_max(),
        extra_pages_needed(current_pages, end_offset) == Some(pages),
    ensures
        current_pages + pages <= u64_max(),
        matches!(checked_add(current_pages, pages), Some(_)),
{
    match checked_mul(current_pages, stable_page_size()) {
        Some(current_bytes) => {
            if end_offset <= current_bytes {
            } else {
                div_ceil_is_bounded_by_positive_numerator((end_offset - current_bytes) as nat);
            }
        },
        None => {},
    }
}

proof fn grow_success_capacity_covers_end_offset(
    current_pages: nat,
    end_offset: nat,
    pages: nat,
    total_pages: nat,
)
    by (nonlinear_arith)
    requires
        end_offset <= u64_max(),
        extra_pages_needed(current_pages, end_offset) == Some(pages),
        checked_add(current_pages, pages) == Some(total_pages),
    ensures
        end_offset <= total_pages * stable_page_size(),
{
    extra_pages_cover_end_offset(current_pages, end_offset, pages);
}

proof fn no_grow_needed_means_current_capacity_covers(
    current_pages: nat,
    end_offset: nat,
)
    requires
        extra_pages_needed(current_pages, end_offset) == Some(0nat),
    ensures
        end_offset <= current_pages * stable_page_size(),
{
    match checked_mul(current_pages, stable_page_size()) {
        Some(current_bytes) => {},
        None => {},
    }
}

proof fn checked_end_rejects_u64_overflow(offset: nat)
    by (nonlinear_arith)
    requires
        offset <= u64_max(),
    ensures
        checked_add(offset, (u64_max() - offset) as nat) == Some(u64_max()),
        checked_add(offset, (u64_max() - offset + 1) as nat) == Option::<nat>::None,
{
}

}
