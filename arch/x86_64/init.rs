/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab
Early CPU/board init: SSE, Limine check, GDT, IDT, PIC, serial, framebuffer.
*/
use core::arch::asm;
use super::boot;
use super::gdt_simple;
use super::idt;
use super::pic;
use super::serial;
use super::framebuffer;

/// Disable CPU interrupts.
pub fn disable_interrupts() {
    unsafe { asm!("cli"); }
}

/// Enable CPU interrupts.
pub fn enable_interrupts() {
    unsafe { asm!("sti"); }
}

/// Enable SSE/SSE2 (CR4.OSFXSR, OSXMMEXCPT; CR0.EM clear, MP set).
pub fn enable_sse() {
    unsafe {
        asm!(
            "mov rax, cr4",
            "or rax, 0x660",
            "mov cr4, rax",
            "mov rax, cr0",
            "and rax, 0xFFFFFFFFFFFFFFFB",
            "or rax, 0x2",
            "mov cr0, rax",
            options(nostack, preserves_flags)
        );
    }
}

/// Full arch init: SSE, Limine check, GDT, IDT, PIC, serial, framebuffer.
pub fn init() {
    disable_interrupts();
    enable_sse();

    serial::init();
    serial::write_str("[AETERNA] Serial OK\r\n");

    if !boot::base_revision_supported() {
        serial::write_str("[AETERNA] WARN: Limine base revision not 0\r\n");
    }
    if let Some(off) = boot::hhdm_offset() {
        serial::write_str("[AETERNA] HHDM offset: 0x");
        serial_hex(off);
        serial::write_str("\r\n");
    }

    gdt_simple::init();
    serial::write_str("[AETERNA] GDT loaded\r\n");

    idt::init();
    serial::write_str("[AETERNA] IDT loaded\r\n");

    // Initialize PIC (remap IRQs to IDT 32-47)
    pic::init();

    // Initialize framebuffer if available
    if let Some(fb) = boot::framebuffer() {
        unsafe {
            framebuffer::init(
                fb.address as *mut u32,
                fb.width,
                fb.height,
                fb.pitch,
                fb.bpp,
            );
        }
        serial::write_str("[AETERNA] Framebuffer initialized\r\n");

        // Initialize fbconsole for text output
        super::fbconsole::init();

        // Clear screen and draw welcome text (minimalist - white only)
        framebuffer::clear(0x00000000);
        framebuffer::draw_string_at(10, 10, "AETERNA Microkernel", 0x00FFFFFF, 0x00000000);
        framebuffer::draw_string_at(10, 26, "===================", 0x00FFFFFF, 0x00000000);
        framebuffer::draw_string_at(10, 50, "Framebuffer: OK", 0x00FFFFFF, 0x00000000);
        framebuffer::draw_string_at(10, 66, "Serial: OK", 0x00FFFFFF, 0x00000000);
        framebuffer::draw_string_at(10, 82, "GDT: OK", 0x00FFFFFF, 0x00000000);
        framebuffer::draw_string_at(10, 98, "IDT: OK", 0x00FFFFFF, 0x00000000);
        framebuffer::draw_string_at(10, 114, "PIC: OK", 0x00FFFFFF, 0x00000000);
    } else {
        serial::write_str("[AETERNA] No framebuffer available\r\n");
    }

    // Enable hardware interrupts
    enable_interrupts();
    serial::write_str("[AETERNA] Interrupts enabled\r\n");
}

pub fn serial_hex(mut val: u64) {
    const HEX: [u8; 16] = *b"0123456789ABCDEF";
    let mut buf = [0u8; 16];
    for i in (0..16).rev() {
        buf[i] = HEX[(val & 0xF) as usize];
        val >>= 4;
    }
    for b in buf {
        serial::write_byte(b);
    }
}
