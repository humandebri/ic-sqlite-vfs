//! Minimal libc symbols required by bundled SQLite on `wasm32-unknown-unknown`.
//!
//! These functions avoid WASI/POSIX imports. Allocation delegates to Rust's
//! global allocator with a small size header so C `free` and `realloc` work.

use std::alloc::{self, alloc, dealloc, Layout};
use std::ffi::{c_char, c_int, c_void};
use std::ptr;

const ALIGN: usize = 16;
const HEADER: usize = 16;

#[no_mangle]
pub unsafe extern "C" fn malloc(size: usize) -> *mut c_void {
    let total = match total_size(size) {
        Some(value) => value,
        None => return ptr::null_mut(),
    };
    let layout = layout(total);
    let base = alloc(layout);
    if base.is_null() {
        return ptr::null_mut();
    }
    base.cast::<usize>().write(size);
    base.add(HEADER).cast::<c_void>()
}

#[no_mangle]
pub unsafe extern "C" fn calloc(count: usize, size: usize) -> *mut c_void {
    let Some(bytes) = count.checked_mul(size) else {
        return ptr::null_mut();
    };
    let out = malloc(bytes);
    if !out.is_null() {
        ptr::write_bytes(out.cast::<u8>(), 0, bytes);
    }
    out
}

#[no_mangle]
pub unsafe extern "C" fn free(ptr: *mut c_void) {
    if ptr.is_null() {
        return;
    }
    let base = ptr.cast::<u8>().sub(HEADER);
    let size = base.cast::<usize>().read();
    let Some(total) = total_size(size) else {
        return;
    };
    dealloc(base, layout(total));
}

#[no_mangle]
pub unsafe extern "C" fn realloc(ptr: *mut c_void, size: usize) -> *mut c_void {
    if ptr.is_null() {
        return malloc(size);
    }
    if size == 0 {
        free(ptr);
        return ptr::null_mut();
    }

    let base = ptr.cast::<u8>().sub(HEADER);
    let old_size = base.cast::<usize>().read();
    let Some(old_total) = total_size(old_size) else {
        return ptr::null_mut();
    };
    let Some(new_total) = total_size(size) else {
        return ptr::null_mut();
    };
    let new_base = alloc::realloc(base, layout(old_total), new_total);
    if new_base.is_null() {
        return ptr::null_mut();
    }
    new_base.cast::<usize>().write(size);
    new_base.add(HEADER).cast::<c_void>()
}

#[no_mangle]
pub unsafe extern "C" fn strcmp(left: *const c_char, right: *const c_char) -> c_int {
    compare(left, right, None)
}

#[no_mangle]
pub unsafe extern "C" fn strncmp(left: *const c_char, right: *const c_char, max: usize) -> c_int {
    compare(left, right, Some(max))
}

#[no_mangle]
pub unsafe extern "C" fn strchr(input: *const c_char, needle: c_int) -> *mut c_char {
    find_forward(input, needle, true)
}

#[no_mangle]
pub unsafe extern "C" fn strrchr(input: *const c_char, needle: c_int) -> *mut c_char {
    if input.is_null() {
        return ptr::null_mut();
    }
    let target = low_byte(needle);
    let mut cursor = input.cast::<u8>();
    let mut found = ptr::null_mut();
    loop {
        let byte = cursor.read();
        if byte == target {
            found = cursor.cast::<c_char>().cast_mut();
        }
        if byte == 0 {
            return found;
        }
        cursor = cursor.add(1);
    }
}

#[no_mangle]
pub unsafe extern "C" fn memchr(input: *const c_void, needle: c_int, len: usize) -> *mut c_void {
    if input.is_null() {
        return ptr::null_mut();
    }
    let target = low_byte(needle);
    let mut cursor = input.cast::<u8>();
    for _ in 0..len {
        if cursor.read() == target {
            return cursor.cast::<c_void>().cast_mut();
        }
        cursor = cursor.add(1);
    }
    ptr::null_mut()
}

#[no_mangle]
pub unsafe extern "C" fn strspn(input: *const c_char, accept: *const c_char) -> usize {
    span(input, accept, true)
}

#[no_mangle]
pub unsafe extern "C" fn strcspn(input: *const c_char, reject: *const c_char) -> usize {
    span(input, reject, false)
}

#[no_mangle]
pub unsafe extern "C" fn localtime_r(_time: *const i64, _out: *mut c_void) -> *mut c_void {
    ptr::null_mut()
}

#[no_mangle]
pub unsafe extern "C" fn gmtime(_time: *const i64) -> *mut c_void {
    ptr::null_mut()
}

#[no_mangle]
pub unsafe extern "C" fn strftime(
    _out: *mut c_char,
    _max: usize,
    _format: *const c_char,
    _time: *const c_void,
) -> usize {
    0
}

fn total_size(size: usize) -> Option<usize> {
    HEADER.checked_add(size)
}

fn layout(size: usize) -> Layout {
    Layout::from_size_align(size, ALIGN).expect("valid libc allocation layout")
}

unsafe fn compare(left: *const c_char, right: *const c_char, max: Option<usize>) -> c_int {
    if left.is_null() || right.is_null() {
        return match (left.is_null(), right.is_null()) {
            (true, true) => 0,
            (true, false) => -1,
            (false, true) => 1,
            (false, false) => 0,
        };
    }
    let mut index = 0_usize;
    loop {
        if max.is_some_and(|limit| index >= limit) {
            return 0;
        }
        let l = left.cast::<u8>().add(index).read();
        let r = right.cast::<u8>().add(index).read();
        if l != r || l == 0 || r == 0 {
            return i32::from(l) - i32::from(r);
        }
        index += 1;
    }
}

unsafe fn find_forward(input: *const c_char, needle: c_int, include_nul: bool) -> *mut c_char {
    if input.is_null() {
        return ptr::null_mut();
    }
    let target = low_byte(needle);
    let mut cursor = input.cast::<u8>();
    loop {
        let byte = cursor.read();
        if byte == target && (include_nul || byte != 0) {
            return cursor.cast::<c_char>().cast_mut();
        }
        if byte == 0 {
            return ptr::null_mut();
        }
        cursor = cursor.add(1);
    }
}

unsafe fn span(input: *const c_char, set: *const c_char, accept_match: bool) -> usize {
    if input.is_null() || set.is_null() {
        return 0;
    }
    let mut len = 0_usize;
    let mut cursor = input.cast::<u8>();
    loop {
        let byte = cursor.read();
        if byte == 0 {
            return len;
        }
        let contains = contains_byte(set, byte);
        if contains != accept_match {
            return len;
        }
        len += 1;
        cursor = cursor.add(1);
    }
}

unsafe fn contains_byte(set: *const c_char, needle: u8) -> bool {
    let mut cursor = set.cast::<u8>();
    loop {
        let byte = cursor.read();
        if byte == 0 {
            return false;
        }
        if byte == needle {
            return true;
        }
        cursor = cursor.add(1);
    }
}

fn low_byte(value: c_int) -> u8 {
    value.to_le_bytes()[0]
}
