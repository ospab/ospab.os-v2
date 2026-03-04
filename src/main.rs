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

// ─── C stdlib stubs (LLVM may emit these via optimisation) ───────────────────
// Only needed when building without the DOOM C library (which normally provides them).
// When doom/libdoom.a is linked, its ospab_libc.c supplies these; this fallback
// prevents link errors on clean/no-doom builds.
#[cfg(not(doom_supported))]
#[no_mangle]
pub unsafe extern "C" fn strlen(s: *const u8) -> usize {
    let mut n = 0;
    while *s.add(n) != 0 { n += 1; }
    n
}

// ─── Colors for boot log output on framebuffer ───
const COLOR_OK: u32     = 0x0000FF00;   // Green
const COLOR_WARN: u32   = 0x0000FFFF;   // Yellow
const COLOR_FAIL: u32   = 0x000000FF;   // Red
const COLOR_WHITE: u32  = 0x00FFFFFF;
const COLOR_GRAY: u32   = 0x00AAAAAA;
const COLOR_BG: u32     = 0x00000000;

// Filesystem persistence sector
const FS_SUPER_LBA: u64 = 2048;

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

    // Log kernel load addresses (critical for DMA validation)
    {
        let phys = ospab_os::arch::x86_64::boot::kernel_phys_base();
        let virt = ospab_os::arch::x86_64::boot::kernel_virt_base();
        serial::write_str("[AETERNA] Kernel phys base: 0x");
        serial_hex64(phys);
        serial::write_str(", virt base: 0x");
        serial_hex64(virt);
        serial::write_str("\r\n");
        if phys != 0x200000 {
            serial::write_str("[AETERNA] *** Kernel RELOCATED by Limine (not at 0x200000) ***\r\n");
        }
    }

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

    boot_pending("Kernel heap");
    ospab_os::mm::heap::init();
    if ospab_os::mm::heap::is_initialized() {
        let mib = ospab_os::mm::heap::heap_size() / (1024 * 1024);
        if mib >= 128 {
            boot_ok("Kernel heap (128 MiB)");
        } else {
            // Still OK, just less than ideal
            boot_ok("Kernel heap (reduced)");
        }
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
    // Phase 2.8: PCI bus enumeration
    // ══════════════════════════════════════════════
    boot_pending("PCI bus enumeration");
    let pci_count = ospab_os::pci::enumerate();
    ospab_os::pci::print_devices();
    if pci_count > 0 {
        boot_ok("PCI bus enumeration");
        klog::boot("PCI devices enumerated");
    } else {
        boot_warn("PCI: no devices found");
    }

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

        // ── Superblock inspection: dump sector 2048 header ──
        let mut lba_buf = [0u8; 512];
        let mut super_ok = false;
        if let Some(disk_idx) = ospab_os::fs::disk_sync::primary_disk_index() {
            if ospab_os::drivers::read(disk_idx, FS_SUPER_LBA, 1, &mut lba_buf) {
                let mut sb = [0u8; 8];
                sb.copy_from_slice(&lba_buf[..8]);
                let super_magic = u64::from_le_bytes(sb);
                super_ok = super_magic == ospab_os::fs::disk_sync::SUPER_MAGIC;

                serial::write_str("  [DISK] LBA 2048 super=0x");
                serial_hex64(super_magic);
                serial::write_str("\r\n");
                serial::write_str("  [DISK] LBA2048 bytes[0..64]: ");
                for i in 0..64 {
                    serial_hex8(lba_buf[i]);
                    serial::write_byte(b' ');
                }
                serial::write_str("\r\n");

                if !super_ok {
                    boot_warn("Filesystem superblock missing or corrupt");
                }
            } else {
                boot_warn("Disk read failed for superblock");
            }
        } else {
            boot_warn("No persistence disk available");
        }
        
        // ── Boot recovery: try to restore RamFS from disk ──
        boot_pending("Filesystem recovery");
        let mut fs_recovered = false;
        if let Some(fs_data) = ospab_os::fs::disk_sync::read_from_disk() {
            let data_bytes = fs_data.len();
            if let Some(tree) = ospab_os::fs::disk_sync::deserialize_ramfs(&fs_data) {
                ospab_os::fs::ramfs::restore_from_tree(tree);
                let restored_count = ospab_os::fs::ramfs::node_count();
                fs_recovered = true;

                // Visual feedback on framebuffer
                boot_ok("Filesystem recovery");
                boot_info_recovery(restored_count, data_bytes);

                serial::write_str("  Restored RamFS: ");
                serial_u32(restored_count as u32);
                serial::write_str(" nodes, ");
                serial_u32(data_bytes as u32);
                serial::write_str(" bytes\r\n");
            }
        }
        if !fs_recovered {
            serial::write_str("[FS] No persisted FS found, creating new\r\n");
            boot_warn_new_fs();
        }

        // ── Enable auto-sync: every VFS write will flush to disk ──
        ospab_os::fs::ramfs::AUTOSYNC_ENABLED.store(true, core::sync::atomic::Ordering::SeqCst);
        serial::write_str("[FS] Auto-sync ENABLED — all writes persist to disk\r\n");
        boot_ok("Disk persistence: auto-sync enabled");
    } else {
        boot_warn("Storage: no drives found (ATA/AHCI)");
        klog::boot("No storage devices");
    }

    // ══════════════════════════════════════════════
    // Phase 3.4: GPU / Display acceleration
    // ══════════════════════════════════════════════
    boot_pending("GPU / Display acceleration");
    if ospab_os::drivers::gpu::init() {
        boot_ok("GPU: VMware SVGA II + VMMouse initialized");
        klog::boot("GPU ready");
    } else {
        boot_warn("GPU: no accelerated display found (VMware SVGA II not present)");
        klog::boot("GPU not available");
    }

    // ══════════════════════════════════════════════
    // Phase 3.5: Audio subsystem (Intel HDA)
    // ══════════════════════════════════════════════
    boot_pending("Audio subsystem");
    if ospab_os::drivers::audio::init() {
        boot_ok("Audio subsystem (Intel HDA, 44100Hz/16-bit/2ch)");
        klog::boot("HDA audio initialized");
    } else {
        boot_warn("Audio: no HDA controller found (add -device intel-hda,id=sound0 -device hda-duplex,bus=sound0.0 to QEMU)");
        klog::boot("Audio not available");
    }

    // ══════════════════════════════════════════════
    // Phase 3.6: NVMe SSD + GPT + DevFS + AeternaFS
    // ══════════════════════════════════════════════
    boot_pending("NVMe storage");
    if ospab_os::drivers::nvme::probe_and_init() {
        // Build a NvmeDisk wrapper and try to parse GPT
        let mut nvme_disk = ospab_os::drivers::block::NvmeDisk::new();
        let gpt_parts = ospab_os::drivers::gpt::parse(&mut nvme_disk).unwrap_or(0);

        // Register /dev/nvme0n1 and /dev/nvme0n1pN entries in VFS
        ospab_os::drivers::block::register_devices();

        if gpt_parts > 0 {
            let mut msg = [0u8; 60];
            let mut pos = 0;
            for b in b"NVMe SSD + GPT (" { msg[pos] = *b; pos += 1; }
            let mut p = gpt_parts;
            if p >= 10 { msg[pos] = b'0' + (p / 10) as u8; pos += 1; }
            msg[pos] = b'0' + (p % 10) as u8; pos += 1;
            for b in b" partitions)" { if pos < 58 { msg[pos] = *b; pos += 1; } }
            if let Ok(s) = core::str::from_utf8(&msg[..pos]) {
                boot_ok(s);
            } else {
                boot_ok("NVMe SSD + GPT");
            }
            klog::boot("NVMe + GPT initialized");

            // Try to auto-mount first AeternaFS partition at /mnt/target
            for pi in 0..gpt_parts {
                if let Some(part) = ospab_os::drivers::gpt::get_partition(pi) {
                    let ss = ospab_os::drivers::nvme::sector_size();
                    if ospab_os::fs::aeternafs::mount_nvme_partition(part.start_lba, ss) {
                        ospab_os::fs::mount("/mnt/target",
                            ospab_os::fs::aeternafs::instance());
                        serial::write_str("  [AeternaFS] Auto-mounted partition ");
                        serial::write_byte(b'0' + pi as u8);
                        serial::write_str(" at /mnt/target\r\n");
                        break;
                    }
                }
            }
        } else {
            boot_ok("NVMe SSD (no GPT — run `fdisk -l` then `mkfs`)");
            klog::boot("NVMe initialized, no GPT");
        }
    } else {
        boot_warn("NVMe: no NVMe controller found");
        klog::boot("NVMe not available");
    }

    // ══════════════════════════════════════════════
    // Phase 3.7: /sys virtual filesystem nodes
    // ══════════════════════════════════════════════
    boot_pending("/sys VFS nodes");
    ospab_os::fs::mkdir("/sys");
    // Write initial /sys/status with boot-time process snapshot.
    // The terminal `ps` command calls refresh_sys_status() to keep it current.
    refresh_sys_status();
    boot_ok("/sys/status ready");
    klog::boot("/sys/status populated");

    // Create /dev tree — /dev/audio is a virtual device file (writes go to HDA)
    ospab_os::fs::mkdir("/dev");
    ospab_os::fs::write_file("/dev/audio", b""); // placeholder; writes intercepted in VFS layer

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
        // Poll for reply: up to ~3 seconds (300 ticks at 100 Hz)
        let mut got_reply = false;
        let deadline = ospab_os::arch::x86_64::idt::timer_ticks() + 300; // 3 s @ 100 Hz
        let mut diag_ticks = 0u32;
        loop {
            ospab_os::net::poll_rx();
            if let Some(r) = ospab_os::net::icmp::poll_reply() {
                serial::write_str("[NET-TEST] PING REPLY! seq=");
                serial_u32(r.seq as u32);
                serial::write_str(" rtt=");
                serial_u32((r.rtt_us / 1000) as u32);
                serial::write_str("ms ttl=");
                serial_u32(r.ttl as u32);
                serial::write_str("\r\n");
                got_reply = true;
                break;
            }
            // Print NIC register state every ~0.5s (9 ticks)
            diag_ticks += 1;
            if diag_ticks % 9 == 1 {
                let (r0, r1, r2, r3) = ospab_os::net::diag();
                let nic_name = ospab_os::net::nic_name();
                serial::write_str("[NET-DIAG] ");
                serial::write_str(nic_name);
                if nic_name == "Intel e1000" {
                    serial::write_str(" STATUS=0x"); serial_hex32(r0);
                    serial::write_str(" ICR=0x"); serial_hex32(r1);
                    serial::write_str(" RDH="); serial_u32(r2);
                    serial::write_str(" RDT="); serial_u32(r3);
                } else {
                    serial::write_str(" ISR=0x"); serial_hex16(r0 as u16);
                    serial::write_str(" CMD=0x"); serial_hex8(r1 as u8);
                    serial::write_str(" CBR="); serial_u32(r2);
                    serial::write_str(" CAPR="); serial_u32(r3);
                }
                serial::write_str("\r\n");
            }
            if ospab_os::arch::x86_64::idt::timer_ticks() >= deadline {
                break;
            }
            unsafe { core::arch::asm!("hlt"); }
        }
        if !got_reply {
            serial::write_str("[NET-TEST] No reply (timeout)\r\n");
            // Dump RTL8139 RX buffer for DMA debugging
            if ospab_os::net::nic_name() == "RTL8139" {
                ospab_os::net::rtl8139::rx_buffer_dump(64);
            }
            // Run full network diagnostic on failure
            ospab_os::net::diag::run_full_diagnostic();
        }
    } else {
        boot_warn("Network: no supported NIC found");
        klog::boot("Network not available");
        // Still run PCI scan diagnostic so we see what hardware exists
        ospab_os::net::diag::dump_pci_nics();
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
    // Phase 5: Terminal or Installer (cmdline-selected)
    // ══════════════════════════════════════════════
    let boot_mode = ospab_os::arch::x86_64::boot::cmdline_get("mode");
    serial::write_str("[BOOT] mode=");
    serial::write_str(boot_mode);
    serial::write_str("\r\n");

    if boot_mode == "installer" {
        serial::write_str("[BOOT] Launching installer (mode=installer)\r\n");
        boot_ok("Launching installer");
        klog::boot("Entering installer");
        framebuffer::draw_char('\n', COLOR_WHITE, COLOR_BG);
        crate::installer::run();
        loop { unsafe { core::arch::asm!("hlt"); } }
    } else {
        boot_ok("Console ready");
        klog::boot("Entering terminal");
        framebuffer::draw_char('\n', COLOR_WHITE, COLOR_BG);
        terminal::run();
    }
}

// ─── /sys/status: write current task table to VFS ───

/// Build a text snapshot of all live tasks and write it to /sys/status.
/// Call at boot and whenever the task table changes (e.g. from `ps`).
fn refresh_sys_status() {
    use alloc::vec::Vec;

    let mut snapshots = [ospab_os::core::syscall::TaskInfo {
        pid: 0,
        priority: ospab_os::core::scheduler::Priority::Idle,
        state: ospab_os::core::scheduler::TaskState::Dead,
        cr3: 0,
        cpu_ticks: 0,
        memory_bytes: 0,
        name: [0u8; 24],
        name_len: 0,
    }; 64];
    let n = ospab_os::core::syscall::sys_get_tasks(&mut snapshots);

    let mut out: Vec<u8> = Vec::with_capacity(n * 48 + 32);

    // Header
    for &b in b"PID   TICKS      STATE    NAME\n" { out.push(b); }
    for &b in b"---   -----      -----    ----\n" { out.push(b); }

    for snap in snapshots.iter().take(n) {
        // PID (right-pad to 6)
        let pid_s = dec_str(snap.pid as u64);
        for &b in pid_s.as_bytes() { out.push(b); }
        for _ in pid_s.len()..6 { out.push(b' '); }

        // CPU ticks (right-pad to 11)
        let tick_s = dec_str(snap.cpu_ticks);
        for &b in tick_s.as_bytes() { out.push(b); }
        for _ in tick_s.len()..11 { out.push(b' '); }

        // State (fixed 9 chars)
        let state_str: &[u8] = match snap.state {
            ospab_os::core::scheduler::TaskState::Running => b"running  ",
            ospab_os::core::scheduler::TaskState::Ready   => b"ready    ",
            ospab_os::core::scheduler::TaskState::Waiting => b"waiting  ",
            ospab_os::core::scheduler::TaskState::Dead    => b"dead     ",
        };
        for &b in state_str { out.push(b); }

        // Name
        let name_len = snap.name_len as usize;
        for j in 0..name_len { out.push(snap.name[j]); }
        out.push(b'\n');
    }

    ospab_os::fs::write_file("/sys/status", &out);
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

/// Display [INFO] recovery message with node count and byte count on framebuffer
fn boot_info_recovery(node_count: usize, byte_count: usize) {
    if !framebuffer::is_initialized() { return; }

    // Format: [INFO] Root filesystem mounted from disk. N nodes, B bytes recovered.
    framebuffer::draw_string("[", COLOR_GRAY, COLOR_BG);
    framebuffer::draw_string("INFO", COLOR_OK, COLOR_BG);
    framebuffer::draw_string("] ", COLOR_GRAY, COLOR_BG);
    framebuffer::draw_string("Root filesystem mounted from disk. ", COLOR_WHITE, COLOR_BG);
    framebuffer::draw_string(&dec_str(node_count as u64), COLOR_OK, COLOR_BG);
    framebuffer::draw_string(" nodes, ", COLOR_WHITE, COLOR_BG);
    framebuffer::draw_string(&dec_str(byte_count as u64), COLOR_OK, COLOR_BG);
    framebuffer::draw_string(" bytes recovered.", COLOR_WHITE, COLOR_BG);
    framebuffer::draw_char('\n', COLOR_WHITE, COLOR_BG);
}

/// Display [WARN] when no filesystem found on disk
fn boot_warn_new_fs() {
    serial::write_str("[ WARN ] No filesystem found on disk. Creating new.\r\n");
    if !framebuffer::is_initialized() { return; }

    framebuffer::draw_string("[", COLOR_GRAY, COLOR_BG);
    framebuffer::draw_string("WARN", COLOR_WARN, COLOR_BG);
    framebuffer::draw_string("] ", COLOR_GRAY, COLOR_BG);
    framebuffer::draw_string("No filesystem found on disk. Creating new.", COLOR_WARN, COLOR_BG);
    framebuffer::draw_char('\n', COLOR_WHITE, COLOR_BG);
}

/// Format a u64 into a decimal string (for framebuffer output)
fn dec_str(mut n: u64) -> alloc::string::String {
    if n == 0 { return alloc::string::String::from("0"); }
    let mut buf = [0u8; 20];
    let mut pos = 20;
    while n > 0 {
        pos -= 1;
        buf[pos] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    alloc::string::String::from(core::str::from_utf8(&buf[pos..]).unwrap_or("0"))
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

fn serial_hex32(v: u32) {
    let hex = b"0123456789ABCDEF";
    for i in (0..8).rev() {
        serial::write_byte(hex[((v >> (i * 4)) & 0xF) as usize]);
    }
}

fn serial_hex64(v: u64) {
    let hex = b"0123456789ABCDEF";
    for i in (0..16).rev() {
        serial::write_byte(hex[((v >> (i * 4)) & 0xF) as usize]);
    }
}

fn serial_hex8(v: u8) {
    let hex = b"0123456789ABCDEF";
    serial::write_byte(hex[(v >> 4) as usize]);
    serial::write_byte(hex[(v & 0xF) as usize]);
}
