/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab
Global panic handler: framebuffer-based, NEVER halts.
Displays red KERNEL PANIC banner + event log + panic info.
After display, enters a safe spin loop with HLT (low power).
Also outputs to serial for headless debugging.
*/

use core::panic::PanicInfo;
use core::arch::asm;
use core::sync::atomic::{AtomicBool, Ordering};
use crate::arch::x86_64::framebuffer;
use crate::arch::x86_64::serial;

// Prevent recursive panics
static PANICKED: AtomicBool = AtomicBool::new(false);

// Colors (BGR format for Limine/UEFI framebuffer)
const COLOR_PANIC_BG: u32 = 0x00000000;     // Black background
const COLOR_PANIC_RED: u32 = 0x000000FF;     // Pure red (BGR)
const COLOR_PANIC_WHITE: u32 = 0x00FFFFFF;   // White
const COLOR_PANIC_GRAY: u32 = 0x00AAAAAA;    // Light gray
const COLOR_PANIC_YELLOW: u32 = 0x0000FFFF;  // Yellow (BGR)

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Disable interrupts immediately — no IRQs during panic display
    unsafe { asm!("cli"); }

    // Prevent recursive panics — if we're already in panic handler, just halt
    if PANICKED.swap(true, Ordering::SeqCst) {
        serial::write_str("\r\n[PANIC] Recursive panic detected, halting immediately.\r\n");
        loop {
            unsafe { asm!("cli; hlt"); }
        }
    }

    // === Serial output (always works, even without framebuffer) ===
    serial::write_str("\r\n\r\n");
    serial::write_str("========================================\r\n");
    serial::write_str(" KERNEL PANIC - AETERNA\r\n");
    serial::write_str("========================================\r\n");

    if let Some(loc) = info.location() {
        serial::write_str("Location: ");
        serial::write_str(loc.file());
        serial::write_str(":");
        serial_write_u32(loc.line());
        serial::write_str("\r\n");
    }

    // Print panic message to serial
    serial::write_str("Message: ");
    if let Some(msg) = info.message().as_str() {
        serial::write_str(msg);
    } else {
        serial::write_str("(formatted message)");
    }
    serial::write_str("\r\n\r\n");

    serial::write_str("System halted. Check screen or serial output.\r\n");
    serial::write_str("========================================\r\n");

    serial::write_str("System halted. Check screen or serial output.\r\n");
    serial::write_str("========================================\r\n");

    // === Framebuffer output (if available) ===
    if framebuffer::is_initialized() {
        // Clear screen to black
        framebuffer::clear(COLOR_PANIC_BG);

        // Red title bar
        let y_start = 20u64;
        framebuffer::draw_string_at(40, y_start, "KERNEL PANIC", COLOR_PANIC_RED, COLOR_PANIC_BG);
        
        let mut y = y_start + 40;
        
        // Location
        if let Some(loc) = info.location() {
            framebuffer::draw_string_at(40, y, "Location: ", COLOR_PANIC_YELLOW, COLOR_PANIC_BG);
            framebuffer::draw_string_at(180, y, loc.file(), COLOR_PANIC_WHITE, COLOR_PANIC_BG);
            y += 20;
            framebuffer::draw_string_at(180, y, "Line: ", COLOR_PANIC_GRAY, COLOR_PANIC_BG);
            let line = loc.line();
            framebuffer::draw_char_at(250, y, (b'0' + ((line / 100) % 10) as u8) as char, COLOR_PANIC_WHITE, COLOR_PANIC_BG);
            framebuffer::draw_char_at(258, y, (b'0' + ((line / 10) % 10) as u8) as char, COLOR_PANIC_WHITE, COLOR_PANIC_BG);
            framebuffer::draw_char_at(266, y, (b'0' + (line % 10) as u8) as char, COLOR_PANIC_WHITE, COLOR_PANIC_BG);
            y += 40;
        }
        
        // Message
        framebuffer::draw_string_at(40, y, "Cause:", COLOR_PANIC_YELLOW, COLOR_PANIC_BG);
        y += 20;
        if let Some(msg) = info.message().as_str() {
            framebuffer::draw_string_at(40, y, msg, COLOR_PANIC_WHITE, COLOR_PANIC_BG);
        } else {
            framebuffer::draw_string_at(40, y, "(see serial output)", COLOR_PANIC_GRAY, COLOR_PANIC_BG);
        }
        y += 40;
        
        // Bottom message
        framebuffer::draw_string_at(40, y, "System halted. Reboot to continue.", COLOR_PANIC_RED, COLOR_PANIC_BG);
    }

    // Safe halt loop — low power, never returns
    loop {
        unsafe { asm!("hlt"); }
    }
}

/// Print u32 to serial (simple, no allocation)
fn serial_write_u32(val: u32) {
    if val == 0 {
        serial::write_byte(b'0');
        return;
    }
    let mut buf = [0u8; 10];
    let mut n = val;
    let mut i = 0;
    while n > 0 {
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        i += 1;
    }
    for j in (0..i).rev() {
        serial::write_byte(buf[j]);
    }
}