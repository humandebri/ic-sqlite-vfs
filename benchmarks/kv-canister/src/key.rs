//! Benchmark text helpers for fixed-width KV workloads.
//!
//! Hot paths use stack buffers for fixed-width ASCII strings. This keeps the
//! benchmark focused on SQLite/VFS work instead of Rust formatting allocation.

const MAX_FIXED_BENCH_KEY_ROWS: u32 = 100_000_000;
const DIGIT_PAIRS: &[u8; 200] =
    b"0001020304050607080910111213141516171819\
      2021222324252627282930313233343536373839\
      4041424344454647484950515253545556575859\
      6061626364656667686970717273747576777879\
      8081828384858687888990919293949596979899";

pub fn bench_key(index: u32, out: &mut [u8; 9]) -> &str {
    prefixed_key(b'k', index, out)
}

pub fn prefixed_key(prefix: u8, index: u32, out: &mut [u8; 9]) -> &str {
    debug_assert!(index < MAX_FIXED_BENCH_KEY_ROWS);
    out[0] = prefix;
    write_fixed_u32(index, &mut out[1..]);
    ascii_str(out)
}

pub fn bench_value(index: u32, out: &mut [u8; 25]) -> &str {
    out[..6].copy_from_slice(b"value-");
    write_fixed_u32(index, &mut out[6..14]);
    out[14..].copy_from_slice(b"-stable-vfs");
    ascii_str(out)
}

pub fn updated_value(index: u32, out: &mut [u8; 27]) -> &str {
    out[..8].copy_from_slice(b"updated-");
    write_fixed_u32(index, &mut out[8..16]);
    out[16..].copy_from_slice(b"-stable-vfs");
    ascii_str(out)
}

pub fn growth_value(index: u32, out: &mut [u8; 26]) -> &str {
    out[..7].copy_from_slice(b"growth-");
    write_fixed_u32(index, &mut out[7..15]);
    out[15..].copy_from_slice(b"-stable-vfs");
    ascii_str(out)
}

pub fn write_value(index: u32, out: &mut [u8; 14]) -> &str {
    out[..6].copy_from_slice(b"write-");
    write_fixed_u32(index, &mut out[6..]);
    ascii_str(out)
}

pub fn order_value(index: u32, out: &mut [u8; 14]) -> &str {
    out[..6].copy_from_slice(b"value-");
    write_fixed_u32(index, &mut out[6..]);
    ascii_str(out)
}

pub fn body_value(index: u32, out: &mut [u8; 13]) -> &str {
    out[..5].copy_from_slice(b"body-");
    write_fixed_u32(index, &mut out[5..]);
    ascii_str(out)
}

pub fn group_label(group: i64, out: &mut [u8; 9]) -> &str {
    debug_assert!((0..100).contains(&group));
    out[..6].copy_from_slice(b"group-");
    let group = u32::try_from(group).expect("group is non-negative");
    out[6] = b'0';
    out[7..].copy_from_slice(digit_pair(group));
    ascii_str(out)
}

pub fn validate_fixed_bench_key_rows(rows: u32) -> Result<(), String> {
    if rows <= MAX_FIXED_BENCH_KEY_ROWS {
        Ok(())
    } else {
        Err(format!(
            "rows must be at most {MAX_FIXED_BENCH_KEY_ROWS} for fixed-width benchmark keys"
        ))
    }
}

pub fn validate_fixed_bench_key_index(index: u32) -> Result<(), String> {
    if index < MAX_FIXED_BENCH_KEY_ROWS {
        Ok(())
    } else {
        Err(format!(
            "benchmark key index must be less than {MAX_FIXED_BENCH_KEY_ROWS}"
        ))
    }
}

pub fn validate_fixed_bench_key_range(start: u32, count: u32) -> Result<(), String> {
    let Some(end) = start.checked_add(count) else {
        return Err(format!(
            "benchmark key index must be less than {MAX_FIXED_BENCH_KEY_ROWS}"
        ));
    };
    if end <= MAX_FIXED_BENCH_KEY_ROWS {
        Ok(())
    } else {
        Err(format!(
            "benchmark key index must be less than {MAX_FIXED_BENCH_KEY_ROWS}"
        ))
    }
}

fn write_fixed_u32(index: u32, out: &mut [u8]) {
    debug_assert_eq!(out.len(), 8);
    if index < 10_000 {
        out[..4].copy_from_slice(b"0000");
        out[4..6].copy_from_slice(digit_pair(index / 100));
        out[6..].copy_from_slice(digit_pair(index % 100));
        return;
    }
    let high = index / 1_000_000;
    let rem = index % 1_000_000;
    let mid_high = rem / 10_000;
    let rem = rem % 10_000;
    let mid_low = rem / 100;
    let low = rem % 100;
    out[..2].copy_from_slice(digit_pair(high));
    out[2..4].copy_from_slice(digit_pair(mid_high));
    out[4..6].copy_from_slice(digit_pair(mid_low));
    out[6..].copy_from_slice(digit_pair(low));
}

fn ascii_str(bytes: &[u8]) -> &str {
    // SAFETY: callers fill the buffers with fixed ASCII prefixes, suffixes,
    // and decimal digits.
    unsafe { std::str::from_utf8_unchecked(bytes) }
}

fn digit_pair(value: u32) -> &'static [u8] {
    let start = usize::try_from(value)
        .expect("two-digit value fits usize")
        .checked_mul(2)
        .expect("digit-pair index fits usize");
    &DIGIT_PAIRS[start..start + 2]
}

#[cfg(test)]
mod tests {
    use super::{
        bench_key, bench_value, body_value, group_label, growth_value, order_value, prefixed_key,
        updated_value, validate_fixed_bench_key_index, validate_fixed_bench_key_range,
        validate_fixed_bench_key_rows, write_value, MAX_FIXED_BENCH_KEY_ROWS,
    };

    #[test]
    fn bench_key_matches_fixed_width_format_boundary() {
        let mut key = [0_u8; 9];
        assert_eq!(bench_key(0, &mut key), "k00000000");
        assert_eq!(bench_key(42, &mut key), "k00000042");
        assert_eq!(bench_key(9_999, &mut key), "k00009999");
        assert_eq!(bench_key(10_000, &mut key), "k00010000");
        assert_eq!(bench_key(99_999_999, &mut key), "k99999999");
        assert_eq!(prefixed_key(b'w', 42, &mut key), "w00000042");
    }

    #[test]
    fn bench_values_match_fixed_width_formats() {
        let mut value = [0_u8; 25];
        let mut updated = [0_u8; 27];
        let mut growth = [0_u8; 26];
        let mut write = [0_u8; 14];
        let mut order = [0_u8; 14];
        let mut body = [0_u8; 13];
        let mut label = [0_u8; 9];
        assert_eq!(bench_value(42, &mut value), "value-00000042-stable-vfs");
        assert_eq!(
            updated_value(42, &mut updated),
            "updated-00000042-stable-vfs"
        );
        assert_eq!(
            growth_value(42, &mut growth),
            "growth-00000042-stable-vfs"
        );
        assert_eq!(write_value(42, &mut write), "write-00000042");
        assert_eq!(order_value(42, &mut order), "value-00000042");
        assert_eq!(body_value(42, &mut body), "body-00000042");
        assert_eq!(group_label(42, &mut label), "group-042");
    }

    #[test]
    fn fixed_bench_key_rows_rejects_truncating_range() {
        assert!(validate_fixed_bench_key_rows(MAX_FIXED_BENCH_KEY_ROWS).is_ok());
        assert!(validate_fixed_bench_key_rows(MAX_FIXED_BENCH_KEY_ROWS + 1).is_err());
        assert!(validate_fixed_bench_key_index(MAX_FIXED_BENCH_KEY_ROWS - 1).is_ok());
        assert!(validate_fixed_bench_key_index(MAX_FIXED_BENCH_KEY_ROWS).is_err());
        assert!(validate_fixed_bench_key_range(0, MAX_FIXED_BENCH_KEY_ROWS).is_ok());
        assert!(validate_fixed_bench_key_range(MAX_FIXED_BENCH_KEY_ROWS, 0).is_ok());
        assert!(validate_fixed_bench_key_range(MAX_FIXED_BENCH_KEY_ROWS - 1, 1).is_ok());
        assert!(validate_fixed_bench_key_range(MAX_FIXED_BENCH_KEY_ROWS - 1, 2).is_err());
        assert!(validate_fixed_bench_key_range(u32::MAX, 1).is_err());
    }
}
