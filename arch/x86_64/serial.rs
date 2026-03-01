/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab
COM1 serial for early/debug output (from ospab.os v1).
*/
use core::fmt::{self, Write};

const COM1: u16 = 0x3F8;

fn outb(port: u16, val: u8) {
    unsafe { core::arch::asm!("out dx, al", in("dx") port, in("al") val); }
}

fn inb(port: u16) -> u8 {
    let val: u8;
    unsafe { core::arch::asm!("in al, dx", in("dx") port, out("al") val); }
    val
}

/// Init COM1: 38400 8N1, FIFO enabled.
pub fn init() {
    outb(COM1 + 1, 0x00);
    outb(COM1 + 3, 0x80);
    outb(COM1 + 0, 0x03);
    outb(COM1 + 1, 0x00);
    outb(COM1 + 3, 0x03);
    outb(COM1 + 2, 0xC7);
    outb(COM1 + 4, 0x0B);
}

pub fn write_byte(b: u8) {
    while (inb(COM1 + 5) & 0x20) == 0 {}
    outb(COM1, b);
}

pub fn write_str(s: &str) {
    for b in s.bytes() {
        write_byte(b);
    }
}

pub struct SerialWriter;

impl Write for SerialWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        write_str(s);
        Ok(())
    }
}
