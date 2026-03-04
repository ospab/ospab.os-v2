/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab
Low-level memory functions (memcpy, memset, memmove) for no_std kernel.
*/

/// Copy `n` bytes from `src` to `dst` (non-overlapping).
#[no_mangle]
pub unsafe extern "C" fn memcpy(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    let mut i = 0;
    while i < n {
        *dst.add(i) = *src.add(i);
        i += 1;
    }
    dst
}

/// Set `n` bytes at `dst` to value `c`.
#[no_mangle]
pub unsafe extern "C" fn memset(dst: *mut u8, c: i32, n: usize) -> *mut u8 {
    let val = c as u8;
    let mut i = 0;
    while i < n {
        *dst.add(i) = val;
        i += 1;
    }
    dst
}

/// Copy `n` bytes from `src` to `dst` (handles overlapping regions).
#[no_mangle]
pub unsafe extern "C" fn memmove(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    if src < dst as *const u8 {
        // Copy backwards to handle overlap
        let mut i = n;
        while i > 0 {
            i -= 1;
            *dst.add(i) = *src.add(i);
        }
    } else {
        // Copy forwards
        let mut i = 0;
        while i < n {
            *dst.add(i) = *src.add(i);
            i += 1;
        }
    }
    dst
}

/// Compare `n` bytes at `s1` and `s2`.
#[no_mangle]
pub unsafe extern "C" fn memcmp(s1: *const u8, s2: *const u8, n: usize) -> i32 {
    let mut i = 0;
    while i < n {
        let a = *s1.add(i);
        let b = *s2.add(i);
        if a != b {
            return a as i32 - b as i32;
        }
        i += 1;
    }
    0
}
