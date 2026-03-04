/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab
Framebuffer-based console for text output.
Replaces VGA text mode with graphical framebuffer.
*/

use core::fmt;
use super::framebuffer;

/// Console colors (RGB)
pub const COLOR_BLACK: u32 = 0x00000000;
pub const COLOR_WHITE: u32 = 0x00FFFFFF;
pub const COLOR_RED: u32 = 0x0000FF00;
pub const COLOR_GREEN: u32 = 0x0000FF00;
pub const COLOR_BLUE: u32 = 0x00FF0000;
pub const COLOR_YELLOW: u32 = 0x0000FFFF;
pub const COLOR_CYAN: u32 = 0x00FFFF00;
pub const COLOR_MAGENTA: u32 = 0x00FF00FF;
pub const COLOR_GRAY: u32 = 0x00808080;

/// Console writer using framebuffer
pub struct ConsoleWriter {
    fg_color: u32,
    bg_color: u32,
}

impl ConsoleWriter {
    pub fn new() -> Self {
        ConsoleWriter {
            fg_color: COLOR_WHITE,
            bg_color: COLOR_BLACK,
        }
    }

    pub fn set_colors(&mut self, fg: u32, bg: u32) {
        self.fg_color = fg;
        self.bg_color = bg;
    }

    pub fn clear(&mut self) {
        framebuffer::clear(self.bg_color);
    }

    pub fn write_char(&mut self, c: char) {
        framebuffer::draw_char(c, self.fg_color, self.bg_color);
    }

    pub fn write_str(&mut self, s: &str) {
        for c in s.chars() {
            self.write_char(c);
        }
    }

    pub fn newline(&mut self) {
        framebuffer::draw_char('\n', self.fg_color, self.bg_color);
    }
}

impl fmt::Write for ConsoleWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_str(s);
        Ok(())
    }
}

/// Global console instance
static mut CONSOLE: Option<ConsoleWriter> = None;

/// Initialize console
pub fn init() {
    unsafe {
        CONSOLE = Some(ConsoleWriter::new());
    }
}

/// Check if console is initialized
pub fn is_initialized() -> bool {
    unsafe { CONSOLE.is_some() }
}

/// Clear console screen
pub fn clear() {
    unsafe {
        if let Some(ref mut console) = CONSOLE {
            console.clear();
        }
    }
}

/// Write string to console
pub fn write(s: &str) {
    unsafe {
        if let Some(ref mut console) = CONSOLE {
            console.write_str(s);
        }
    }
}

/// Write character to console
pub fn putchar(c: char) {
    unsafe {
        if let Some(ref mut console) = CONSOLE {
            console.write_char(c);
        }
    }
}

/// Set console colors
pub fn set_colors(fg: u32, bg: u32) {
    unsafe {
        if let Some(ref mut console) = CONSOLE {
            console.set_colors(fg, bg);
        }
    }
}

/// Print formatted text to console
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::arch::x86_64::fbconsole::_print(format_args!($($arg)*)));
}

pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    unsafe {
        if let Some(ref mut console) = CONSOLE {
            let _ = console.write_fmt(args);
        }
    }
}
