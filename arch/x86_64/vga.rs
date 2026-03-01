/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab
Distributed under the Boost Software License, Version 1.1.
See LICENSE or https://www.boost.org/LICENSE_1_0.txt for details.
*/

use core::fmt::{self, Write};

const VGA_BUFFER: *mut u8 = 0xb8000 as *mut u8;

pub struct VgaWriter {
    col: usize,
    color: u8,
}

impl VgaWriter {
    pub const fn new() -> Self {
        Self { col: 0, color: 0x07 }
    }

    pub fn set_color(&mut self, fg: u8) {
        self.color = fg & 0x0f;
    }

    fn write_byte(&mut self, byte: u8) {
        unsafe {
            let offset = 2 * self.col;
            core::ptr::write_volatile(VGA_BUFFER.add(offset), byte);
            core::ptr::write_volatile(VGA_BUFFER.add(offset + 1), self.color);
            self.col = (self.col + 1) % 80;
        }
    }

    fn write_bytes(&mut self, s: &[u8]) {
        for &b in s { self.write_byte(b); }
    }
}

impl Write for VgaWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_bytes(s.as_bytes());
        Ok(())
    }
}
