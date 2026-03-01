/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab
CUI console: 80x25 VGA text, newline, scroll. Shared for boot log and terminal.
*/
#![no_std]

use core::fmt::{self, Write};

const VGA_BUFFER: *mut u8 = 0xb8000 as *mut u8;
const WIDTH: usize = 80;
const HEIGHT: usize = 25;
const BYTES_PER_CHAR: usize = 2;

pub struct Writer {
    pub col: usize,
    pub row: usize,
    pub color: u8,
}

impl Writer {
    pub const fn new() -> Self {
        Self { col: 0, row: 0, color: 0x07 }
    }

    pub fn set_color(&mut self, fg: u8) {
        self.color = fg & 0x0f;
    }

    fn offset(&self) -> usize {
        (self.row * WIDTH + self.col) * BYTES_PER_CHAR
    }

    fn write_byte(&mut self, byte: u8) {
        if byte == b'\n' {
            self.col = 0;
            self.row += 1;
            if self.row >= HEIGHT {
                self.scroll_up();
                self.row = HEIGHT - 1;
            }
            return;
        }
        if byte == b'\r' {
            self.col = 0;
            return;
        }
        unsafe {
            let off = self.offset();
            if self.col < WIDTH {
                core::ptr::write_volatile(VGA_BUFFER.add(off), byte);
                core::ptr::write_volatile(VGA_BUFFER.add(off + 1), self.color);
            }
            self.col += 1;
            if self.col >= WIDTH {
                self.col = 0;
                self.row += 1;
                if self.row >= HEIGHT {
                    self.scroll_up();
                    self.row = HEIGHT - 1;
                }
            }
        }
    }

    fn scroll_up(&mut self) {
        unsafe {
            let row_bytes = WIDTH * BYTES_PER_CHAR;
            for r in 0..(HEIGHT - 1) {
                let src = (r + 1) * row_bytes;
                let dst = r * row_bytes;
                for i in 0..row_bytes {
                    let b = core::ptr::read_volatile(VGA_BUFFER.add(src + i));
                    core::ptr::write_volatile(VGA_BUFFER.add(dst + i), b);
                }
            }
            let last_row = (HEIGHT - 1) * row_bytes;
            for i in 0..WIDTH {
                core::ptr::write_volatile(VGA_BUFFER.add(last_row + i * 2), b' ');
                core::ptr::write_volatile(VGA_BUFFER.add(last_row + i * 2 + 1), 0x07);
            }
        }
    }

    /// Clear screen and home cursor.
    pub fn clear(&mut self) {
        unsafe {
            for i in 0..(WIDTH * HEIGHT * BYTES_PER_CHAR) {
                if i % 2 == 0 {
                    core::ptr::write_volatile(VGA_BUFFER.add(i), b' ');
                } else {
                    core::ptr::write_volatile(VGA_BUFFER.add(i), 0x07);
                }
            }
        }
        self.col = 0;
        self.row = 0;
    }
}

impl Write for Writer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            self.write_byte(b);
        }
        Ok(())
    }
}

use core::sync::atomic::{AtomicBool, Ordering};

static LOCK: AtomicBool = AtomicBool::new(false);
static mut WRITER: Writer = Writer::new();

fn lock_writer<'a>() -> &'a mut Writer {
    while LOCK.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {}
    unsafe { &mut WRITER }
}

fn unlock_writer() {
    LOCK.store(false, Ordering::Release);
}

pub fn _print(args: core::fmt::Arguments) {
    let w = lock_writer();
    let _ = w.write_fmt(args);
    unlock_writer();
}

/// Single global writer for boot_log/terminal (same console).
pub fn with_writer<F, R>(f: F) -> R
where
    F: FnOnce(&mut Writer) -> R,
{
    let w = lock_writer();
    let r = f(w);
    unlock_writer();
    r
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {
        $crate::console::_print(core::format_args!($($arg)*));
    };
}

#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($fmt:expr) => { $crate::print!(concat!($fmt, "\n")) };
    ($fmt:expr, $($arg:tt)*) => { $crate::print!(concat!($fmt, "\n"), $($arg)*) };
}
