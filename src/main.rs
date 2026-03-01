/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab
AETERNA Microkernel entry point.
Boot sequence: hardware init → memory → scheduler → terminal.
All events logged to kernel ring buffer (klog) and serial.
*/
#![no_std]
#![no_main]
#![allow(warnings)]

extern crate alloc;

mod terminal;
mod version;
mod installer;

use core::arch::asm;
use ospab_os::arch::x86_64::serial;
use ospab_os::arch::x86_64::framebuffer;
use ospab_os::klog;

// ─── Colors for boot log output on framebuffer ───
const COLOR_OK: u32     = 0x0000FF00;   // Green
const COLOR_WARN: u32   = 0x0000FFFF;   // Yellow
const COLOR_FAIL: u32   = 0x000000FF;   // Red
const COLOR_WHITE: u32  = 0x00FFFFFF;
const COLOR_GRAY: u32   = 0x00AAAAAA;
const COLOR_BG: u32     = 0x00000000;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    // ══════════════════════════════════════════════
    // Phase 0: Hardware initialization
    // ══════════════════════════════════════════════
    ospab_os::arch::x86_64::init();

    // First serial messages
    serial::write_str("\r\n");
    serial::write_str("  AETERNA Microkernel ");
    serial::write_str(version::VERSION_STR);
    serial::write_str(" - ospab.os\r\n");
    serial::write_str("  ====================================\r\n\r\n");

    klog::boot("AETERNA started");
    klog::boot("Hardware init complete");

    // Boot log to framebuffer
    boot_ok("Hardware init (SSE, GDT, IDT, PIC)");

    // ══════════════════════════════════════════════
    // Phase 1: Bootloader verification
    // ══════════════════════════════════════════════
    boot_pending("Limine protocol");
    if ospab_os::arch::x86_64::boot::base_revision_supported() {
        boot_ok("Limine protocol");
        klog::boot("Limine protocol OK");
    } else {
        boot_warn("Limine base revision mismatch");
        klog::boot("Limine revision warning");
    }

    boot_ok("Serial (COM1)");
    boot_ok("Framebuffer");

    // ══════════════════════════════════════════════
    // Phase 2: Memory subsystem
    // ══════════════════════════════════════════════
    boot_pending("Memory map");
    if ospab_os::arch::x86_64::boot::memory_map().is_some() {
        boot_ok("Memory map");
        klog::memory("Memory map loaded");
    } else {
        boot_warn("Memory map unavailable");
    }

    boot_pending("Physical memory manager");
    ospab_os::mm::physical::init();
    let stats = ospab_os::mm::physical::stats();
    boot_ok("Physical memory manager");
    klog::memory("Physical allocator ready");

    boot_pending("Kernel heap (128 MiB)");
    ospab_os::mm::heap::init();
    if ospab_os::mm::heap::is_initialized() {
        boot_ok("Kernel heap (128 MiB)");
        klog::memory("Heap allocator initialized");
    } else {
        boot_fail("Kernel heap allocation failed");
        klog::fault("Heap init failed");
    }

    // ══════════════════════════════════════════════
    // Phase 2.5: Virtual memory manager
    // ══════════════════════════════════════════════
    boot_pending("Virtual memory manager");
    ospab_os::mm::r#virtual::init();
    boot_ok("Virtual memory manager (4-level page tables)");
    klog::memory("VMM initialized");

    // ══════════════════════════════════════════════
    // Phase 3: Kernel services
    // ══════════════════════════════════════════════
    boot_pending("Scheduler");
    ospab_os::core::scheduler::init();
    boot_ok("Scheduler (Compute-First)");
    klog::boot("Scheduler ready");

    boot_ok("Syscall interface");
    ospab_os::core::syscall::init_syscall_msr();
    boot_ok("Syscall MSRs (LSTAR, STAR, FMASK)");
    boot_ok("Kernel event log");

    // ══════════════════════════════════════════════
    // Phase 3.2: Virtual Filesystem + RamFS
    // ══════════════════════════════════════════════
    boot_pending("Virtual filesystem");
    ospab_os::fs::init();
    ospab_os::fs::ramfs::init();
    ospab_os::fs::mount("/", ospab_os::fs::ramfs::instance());
    let node_count = ospab_os::fs::ramfs::node_count();
    boot_ok("VFS + RamFS mounted at /");
    klog::boot("VFS ready");
    serial::write_str("  RamFS: ");
    serial_u32(node_count as u32);
    serial::write_str(" nodes populated\r\n");

    // ══════════════════════════════════════════════
    // Phase 3.5: Storage (ATA PIO + AHCI SATA)
    // ══════════════════════════════════════════════
    boot_pending("Storage subsystem");
    let disk_count = ospab_os::drivers::init();
    if disk_count > 0 {
        boot_ok("Storage subsystem");
        klog::boot("Storage drivers ready");
        // Log found drives
        for i in 0..disk_count {
            if let Some(d) = ospab_os::drivers::disk_info(i) {
                let model = ospab_os::drivers::model_str(d);
                serial::write_str("  Disk ");
                serial_u32(i as u32);
                serial::write_str(": ");
                serial::write_str(model);
                serial::write_str(" (");
                serial_u32(d.size_mb as u32);
                serial::write_str(" MiB)\r\n");
            }
        }
        
        // ── Boot recovery: try to restore RamFS from disk ──
        boot_pending("Filesystem recovery");
        if let Some(fs_data) = ospab_os::fs::disk_sync::read_from_disk(8 * 1024 * 1024) {
            if let Some(tree) = ospab_os::fs::disk_sync::deserialize_ramfs(&fs_data) {
                ospab_os::fs::ramfs::restore_from_tree(tree);
                let restored_count = ospab_os::fs::ramfs::node_count();
                boot_ok("Filesystem recovery");
                serial::write_str("  Restored RamFS: ");
                serial_u32(restored_count as u32);
                serial::write_str(" nodes\r\n");
            }
        }
    } else {
        boot_warn("Storage: no drives found (ATA/AHCI)");
        klog::boot("No storage devices");
    }

    // ══════════════════════════════════════════════
    // Phase 4: Network (optional — continues if no NIC)
    // ══════════════════════════════════════════════
    boot_pending("Network stack");
    if ospab_os::net::init() {
        let nic = ospab_os::net::nic_name();
        let mut msg = [0u8; 40];
        let mut pos = 0;
        for b in b"Network stack (" { msg[pos] = *b; pos += 1; }
        for b in nic.as_bytes() { if pos < 38 { msg[pos] = *b; pos += 1; } }
        msg[pos] = b')'; pos += 1;
        if let Ok(s) = core::str::from_utf8(&msg[..pos]) {
            boot_ok(s);
        } else {
            boot_ok("Network stack");
        }
        klog::boot("Network initialized");

        // ── Quick self-test: send one ping to gateway and log result ──
        serial::write_str("[NET-TEST] Sending ICMP ping to 10.0.2.2 ...\r\n");
        ospab_os::net::icmp::send_ping([10, 0, 2, 2], 0);
        // Poll for reply: up to ~3 seconds (54 ticks at 18.2 Hz)
        let mut got_reply = false;
        let deadline = ospab_os::arch::x86_64::idt::timer_ticks() + 54;
        let mut diag_ticks = 0u32;
        loop {
            ospab_os::net::poll_rx();
            if let Some((seq, rtt)) = ospab_os::net::icmp::poll_reply() {
                serial::write_str("[NET-TEST] PING REPLY! seq=");
                serial_u32(seq as u32);
                serial::write_str(" rtt=");
                serial_u32(rtt as u32);
                serial::write_str("ms\r\n");
                got_reply = true;
                break;
            }
            // Print NIC register state every ~0.5s (9 ticks)
            diag_ticks += 1;
            if diag_ticks % 9 == 1 {
                let (isr, cmd, cbr, capr) = ospab_os::net::diag();
                serial::write_str("[NET-DIAG] ISR=0x");
                serial_hex16(isr);
                serial::write_str(" CMD=0x");
                serial_hex8(cmd);
                serial::write_str(" CBR=");
                serial_u32(cbr as u32);
                serial::write_str(" CAPR=");
                serial_u32(capr as u32);
                serial::write_str("\r\n");
            }
            if ospab_os::arch::x86_64::idt::timer_ticks() >= deadline {
                break;
            }
            unsafe { core::arch::asm!("hlt"); }
        }
        if !got_reply {
            serial::write_str("[NET-TEST] No reply (timeout)\r\n");
        }
    } else {
        boot_warn("Network: no supported NIC found");
        klog::boot("Network not available");
    }

    // ══════════════════════════════════════════════
    // Phase 4.5: Userland initialization
    // ══════════════════════════════════════════════
    boot_pending("Init system (seed)");
    ospab_os::seed::init();
    boot_ok("Init system (seed) — services registered");
    klog::boot("seed init ready");

    boot_pending("Shell (plum)");
    ospab_os::plum::init();
    boot_ok("Shell (plum) — env + aliases loaded");
    klog::boot("plum shell ready");

    // ══════════════════════════════════════════════
    // Phase 5: Terminal
    // ══════════════════════════════════════════════
    boot_ok("Console ready");
    klog::boot("Entering terminal");

    // Small visual separator before terminal
    framebuffer::draw_char('\n', COLOR_WHITE, COLOR_BG);

    terminal::run();
}

// ─── Boot log helpers: write to both serial and framebuffer ───

fn boot_ok(msg: &str) {
    serial::write_str("[  OK  ] ");
    serial::write_str(msg);
    serial::write_str("\r\n");

    if framebuffer::is_initialized() {
        framebuffer::draw_string("[  ", COLOR_GRAY, COLOR_BG);
        framebuffer::draw_string("OK", COLOR_OK, COLOR_BG);
        framebuffer::draw_string("  ] ", COLOR_GRAY, COLOR_BG);
        framebuffer::draw_string(msg, COLOR_WHITE, COLOR_BG);
        framebuffer::draw_char('\n', COLOR_WHITE, COLOR_BG);
    }
}

fn boot_pending(msg: &str) {
    serial::write_str("[ .... ] ");
    serial::write_str(msg);
    serial::write_str("\r\n");
    // Don't show pending on framebuffer — only final status
}

fn boot_warn(msg: &str) {
    serial::write_str("[ WARN ] ");
    serial::write_str(msg);
    serial::write_str("\r\n");

    if framebuffer::is_initialized() {
        framebuffer::draw_string("[", COLOR_GRAY, COLOR_BG);
        framebuffer::draw_string("WARN", COLOR_WARN, COLOR_BG);
        framebuffer::draw_string("] ", COLOR_GRAY, COLOR_BG);
        framebuffer::draw_string(msg, COLOR_WARN, COLOR_BG);
        framebuffer::draw_char('\n', COLOR_WHITE, COLOR_BG);
    }
}

fn boot_fail(msg: &str) {
    serial::write_str("[ FAIL ] ");
    serial::write_str(msg);
    serial::write_str("\r\n");

    if framebuffer::is_initialized() {
        framebuffer::draw_string("[", COLOR_GRAY, COLOR_BG);
        framebuffer::draw_string("FAIL", COLOR_FAIL, COLOR_BG);
        framebuffer::draw_string("] ", COLOR_GRAY, COLOR_BG);
        framebuffer::draw_string(msg, COLOR_FAIL, COLOR_BG);
        framebuffer::draw_char('\n', COLOR_WHITE, COLOR_BG);
    }
}

/// Write a u32 decimal number to serial
fn serial_u32(mut n: u32) {
    if n == 0 { serial::write_byte(b'0'); return; }
    let mut buf = [0u8; 10];
    let mut pos = 0;
    while n > 0 {
        buf[pos] = b'0' + (n % 10) as u8;
        n /= 10;
        pos += 1;
    }
    for i in (0..pos).rev() {
        serial::write_byte(buf[i]);
    }
}

fn serial_hex16(v: u16) {
    let hex = b"0123456789ABCDEF";
    for i in (0..4).rev() {
        serial::write_byte(hex[((v >> (i * 4)) & 0xF) as usize]);
    }
}

fn serial_hex8(v: u8) {
    let hex = b"0123456789ABCDEF";
    serial::write_byte(hex[(v >> 4) as usize]);
    serial::write_byte(hex[(v & 0xF) as usize]);
}
