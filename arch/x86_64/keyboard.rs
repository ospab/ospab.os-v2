/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab
PS/2 keyboard driver: interrupt-driven via IRQ1 buffer + scancode decoding.
US QWERTY layout. Supports Shift, CapsLock, Ctrl+C, Backspace, Tab, Escape.
Extended scancodes: arrow keys (Up/Down/Left/Right), Home, End, Delete.
Full scancode set 1 translation.
*/

use core::arch::asm;

// Special key codes (non-ASCII, above 0x7F)
pub const KEY_UP: char    = '\u{0080}';
pub const KEY_DOWN: char  = '\u{0081}';
pub const KEY_LEFT: char  = '\u{0082}';
pub const KEY_RIGHT: char = '\u{0083}';
pub const KEY_HOME: char  = '\u{0084}';
pub const KEY_END: char   = '\u{0085}';
pub const KEY_DELETE: char = '\u{007F}';
pub const KEY_PGUP: char  = '\u{0086}';
pub const KEY_PGDN: char  = '\u{0087}';
pub const KEY_F1: char    = '\u{0088}';
pub const KEY_F2: char    = '\u{0089}';
pub const KEY_F3: char    = '\u{008A}';
pub const KEY_F4: char    = '\u{008B}';
pub const KEY_F5: char    = '\u{008C}';
pub const KEY_F6: char    = '\u{008D}';
pub const KEY_F7: char    = '\u{008E}';
pub const KEY_F8: char    = '\u{008F}';
pub const KEY_F9: char    = '\u{0090}';
pub const KEY_F10: char   = '\u{0091}';

// Modifier key state
static mut LEFT_SHIFT: bool = false;
static mut RIGHT_SHIFT: bool = false;
static mut CTRL_PRESSED: bool = false;
static mut CAPS_LOCK: bool = false;
static mut EXTENDED: bool = false; // 0xE0 prefix seen

/// Initialize keyboard: drain pending scancodes from IRQ buffer
pub fn init() {
    while crate::arch::x86_64::idt::kb_irq_read().is_some() {}
}

/// Poll for key press (blocking). Returns decoded char.
pub fn poll_key() -> Option<char> {
    loop {
        if let Some(c) = try_read_key() {
            return Some(c);
        }
        unsafe { asm!("hlt"); }
    }
}

/// Try to read a key (non-blocking). Returns None if no key available.
pub fn try_read_key() -> Option<char> {
    let scancode = crate::arch::x86_64::idt::kb_irq_read()?;

    // Extended scancode prefix
    if scancode == 0xE0 {
        unsafe { EXTENDED = true; }
        return None;
    }

    let is_extended = unsafe { EXTENDED };
    unsafe { EXTENDED = false; }

    // Handle extended key releases (ignore)
    if is_extended && scancode & 0x80 != 0 {
        return None;
    }

    // Handle extended key presses
    if is_extended {
        return match scancode {
            0x48 => Some(KEY_UP),
            0x50 => Some(KEY_DOWN),
            0x4B => Some(KEY_LEFT),
            0x4D => Some(KEY_RIGHT),
            0x47 => Some(KEY_HOME),
            0x4F => Some(KEY_END),
            0x53 => Some(KEY_DELETE),
            0x49 => Some(KEY_PGUP),
            0x51 => Some(KEY_PGDN),
            _ => None,
        };
    }

    // Standard modifier handling
    unsafe {
        match scancode {
            0x2A => { LEFT_SHIFT = true; return None; }
            0xAA => { LEFT_SHIFT = false; return None; }
            0x36 => { RIGHT_SHIFT = true; return None; }
            0xB6 => { RIGHT_SHIFT = false; return None; }
            0x1D => { CTRL_PRESSED = true; return None; }
            0x9D => { CTRL_PRESSED = false; return None; }
            0x3A => { CAPS_LOCK = !CAPS_LOCK; return None; }
            _ => {}
        }
    }

    // Ignore releases
    if scancode & 0x80 != 0 {
        return None;
    }

    // Ctrl combos
    unsafe {
        if CTRL_PRESSED {
            return match scancode {
                0x1E => Some('\x01'), // Ctrl+A
                0x30 => Some('\x02'), // Ctrl+B
                0x2E => Some('\x03'), // Ctrl+C
                0x20 => Some('\x04'), // Ctrl+D (EOF)
                0x12 => Some('\x05'), // Ctrl+E
                0x21 => Some('\x06'), // Ctrl+F
                0x22 => Some('\x07'), // Ctrl+G
                0x23 => Some('\x08'), // Ctrl+H (same as Backspace)
                0x25 => Some('\x0B'), // Ctrl+K
                0x26 => Some('\x0C'), // Ctrl+L
                0x31 => Some('\x0E'), // Ctrl+N
                0x18 => Some('\x0F'), // Ctrl+O
                0x19 => Some('\x10'), // Ctrl+P
                0x13 => Some('\x12'), // Ctrl+R
                0x1F => Some('\x13'), // Ctrl+S
                0x14 => Some('\x14'), // Ctrl+T
                0x16 => Some('\x15'), // Ctrl+U
                0x2F => Some('\x16'), // Ctrl+V
                0x11 => Some('\x17'), // Ctrl+W
                0x2D => Some('\x18'), // Ctrl+X
                0x15 => Some('\x19'), // Ctrl+Y
                0x2C => Some('\x1A'), // Ctrl+Z
                _ => None,
            };
        }
    }

    let shift_active = unsafe { LEFT_SHIFT || RIGHT_SHIFT };
    let caps_active = unsafe { CAPS_LOCK };

    let base = match scancode {
        0x01 => Some('\x1B'), // Escape
        0x02 => Some('1'),
        0x03 => Some('2'),
        0x04 => Some('3'),
        0x05 => Some('4'),
        0x06 => Some('5'),
        0x07 => Some('6'),
        0x08 => Some('7'),
        0x09 => Some('8'),
        0x0A => Some('9'),
        0x0B => Some('0'),
        0x0C => Some('-'),
        0x0D => Some('='),
        0x0E => Some('\x08'), // Backspace
        0x0F => Some('\t'),   // Tab
        0x10 => Some('q'),
        0x11 => Some('w'),
        0x12 => Some('e'),
        0x13 => Some('r'),
        0x14 => Some('t'),
        0x15 => Some('y'),
        0x16 => Some('u'),
        0x17 => Some('i'),
        0x18 => Some('o'),
        0x19 => Some('p'),
        0x1A => Some('['),
        0x1B => Some(']'),
        0x1C => Some('\n'),
        0x1E => Some('a'),
        0x1F => Some('s'),
        0x20 => Some('d'),
        0x21 => Some('f'),
        0x22 => Some('g'),
        0x23 => Some('h'),
        0x24 => Some('j'),
        0x25 => Some('k'),
        0x26 => Some('l'),
        0x27 => Some(';'),
        0x28 => Some('\''),
        0x29 => Some('`'),
        0x2B => Some('\\'),
        0x2C => Some('z'),
        0x2D => Some('x'),
        0x2E => Some('c'),
        0x2F => Some('v'),
        0x30 => Some('b'),
        0x31 => Some('n'),
        0x32 => Some('m'),
        0x33 => Some(','),
        0x34 => Some('.'),
        0x35 => Some('/'),
        0x37 => Some('*'),
        0x39 => Some(' '),
        0x3B => Some(KEY_F1),  // F1
        0x3C => Some(KEY_F2),  // F2
        0x3D => Some(KEY_F3),  // F3
        0x3E => Some(KEY_F4),  // F4
        0x3F => Some(KEY_F5),  // F5
        0x40 => Some(KEY_F6),  // F6
        0x41 => Some(KEY_F7),  // F7
        0x42 => Some(KEY_F8),  // F8
        0x43 => Some(KEY_F9),  // F9
        0x44 => Some(KEY_F10), // F10
        _ => None,
    };

    base.map(|c| {
        if c.is_ascii_alphabetic() {
            let upper = caps_active ^ shift_active;
            if upper { c.to_ascii_uppercase() } else { c }
        } else if shift_active {
            shift_symbol(c)
        } else {
            c
        }
    })
}

fn shift_symbol(c: char) -> char {
    match c {
        '1' => '!', '2' => '@', '3' => '#', '4' => '$', '5' => '%',
        '6' => '^', '7' => '&', '8' => '*', '9' => '(', '0' => ')',
        '-' => '_', '=' => '+', '[' => '{', ']' => '}', '\\' => '|',
        ';' => ':', '\'' => '"', '`' => '~', ',' => '<', '.' => '>',
        '/' => '?',
        _ => c,
    }
}
