/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab
Boot log: Linux-style init messages to VGA + serial (manifest p.20).
*/
#![no_std]

use crate::console;
use core::fmt::Write;

fn emit(s: &str) {
    ospab_os::arch::x86_64::serial::write_str(s);
    // console::_print(core::format_args!("{}", s));
}

/// "[  OK  ] message" — step completed.
pub fn ok(msg: &str) {
    emit("[  OK  ] ");
    emit(msg);
    emit("\r\n");
}

/// "[ .... ] message" — in progress / info.
pub fn pending(msg: &str) {
    emit("[ .... ] ");
    emit(msg);
    emit("\r\n");
}

/// "[WARN] message"
pub fn warn(msg: &str) {
    emit("[ WARN ] ");
    emit(msg);
    emit("\r\n");
}

/// "[FAIL] message"
pub fn fail(msg: &str) {
    emit("[ FAIL ] ");
    emit(msg);
    emit("\r\n");
}

/// Raw line (no prefix), e.g. for "Starting X...".
pub fn line(msg: &str) {
    emit(msg);
    emit("\r\n");
}
