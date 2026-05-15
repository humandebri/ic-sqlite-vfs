//! Benchmark key helpers for fixed-width point-read workloads.
//!
//! Seed and write paths use `format!("k{index:08}")`; point-read hot paths use
//! a stack buffer for the range where that formatting is exactly 9 bytes.

const MAX_FIXED_BENCH_KEY_ROWS: u32 = 100_000_000;

pub fn bench_key(index: u32, out: &mut [u8; 9]) -> &str {
    debug_assert!(index < MAX_FIXED_BENCH_KEY_ROWS);
    out[0] = b'k';
    let mut value = index;
    for byte in out[1..].iter_mut().rev() {
        *byte = b'0' + u8::try_from(value % 10).expect("digit fits u8");
        value /= 10;
    }
    // SAFETY: bytes are always ASCII `k` followed by decimal digits.
    unsafe { std::str::from_utf8_unchecked(out) }
}

pub fn validate_fixed_bench_key_rows(rows: u32) -> Result<(), String> {
    if rows <= MAX_FIXED_BENCH_KEY_ROWS {
        Ok(())
    } else {
        Err(format!(
            "rows must be at most {MAX_FIXED_BENCH_KEY_ROWS} for fixed-width point reads"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{bench_key, validate_fixed_bench_key_rows, MAX_FIXED_BENCH_KEY_ROWS};

    #[test]
    fn bench_key_matches_fixed_width_format_boundary() {
        let mut key = [0_u8; 9];
        assert_eq!(bench_key(0, &mut key), "k00000000");
        assert_eq!(bench_key(42, &mut key), "k00000042");
        assert_eq!(bench_key(99_999_999, &mut key), "k99999999");
    }

    #[test]
    fn fixed_bench_key_rows_rejects_truncating_range() {
        assert!(validate_fixed_bench_key_rows(MAX_FIXED_BENCH_KEY_ROWS).is_ok());
        assert!(validate_fixed_bench_key_rows(MAX_FIXED_BENCH_KEY_ROWS + 1).is_err());
    }
}
