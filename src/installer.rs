/*
 * AETERNA Installer — aeterna-install
 *
 * Interactive, step-by-step disk installer for AETERNA Microkernel.
 *
 * Flow:
 *   1. Scan ATA/AHCI + NVMe disks
 *   2. User selects target disk
 *   3. Configure: partition layout, hostname, root password, locale
 *   4. Press [i] → real write:
 *        Sector  0       Protective MBR (0xEE)
 *        Sector  1       GPT header (EFI PART)
 *        Sectors 2-33    GPT partition entries (128 × 128 B)
 *        ESP (FAT32):
 *          /EFI/BOOT/BOOTX64.EFI  — Limine UEFI payload
 *          /boot/                 — boot directory
 *          /boot/KERNEL           — AETERNA kernel ELF
 *          /limine.conf           — boot config (Limine5 syntax)
 *        Root partition:
 *          Identity sector        — magic + version + hostname + geometry
 *        Backup GPT               — at last sector
 *   5. Readback verify: GPT signature, FAT32 BPB, AETERNA magic, kernel first sector
 */

#![allow(dead_code)]

extern crate alloc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use ospab_os::arch::x86_64::framebuffer;
use ospab_os::arch::x86_64::keyboard;
use ospab_os::arch::x86_64::serial;

// ── Colours (32-bit BGRA: 0x00RRGGBB in BGR channel order) ──────────────────
const FG: u32      = 0x00FFFFFF; // white
const FG_DIM: u32  = 0x00AAAAAA; // grey
const FG_OK: u32   = 0x0000FF00; // green
const FG_WARN: u32 = 0x0000CCFF; // yellow/cyan
const FG_ERR: u32  = 0x004444FF; // red  (R=0xFF, G=0x44, B=0x44 in BGR)
const FG_HL: u32   = 0x0000CCFF; // highlight (cyan)
const FG_STEP: u32 = 0x00FF8800; // orange step labels
const BG: u32      = 0x00000000;

static ABORT: AtomicBool = AtomicBool::new(false);

fn puts(s: &str)  { framebuffer::draw_string(s, FG, BG); }
fn dim(s: &str)   { framebuffer::draw_string(s, FG_DIM, BG); }
fn ok(s: &str)    { framebuffer::draw_string(s, FG_OK, BG); }
fn warn(s: &str)  { framebuffer::draw_string(s, FG_WARN, BG); }
fn err(s: &str)   { framebuffer::draw_string(s, FG_ERR, BG); }
fn hl(s: &str)    { framebuffer::draw_string(s, FG_HL, BG); }
fn step(s: &str)  { framebuffer::draw_string(s, FG_STEP, BG); }
fn putc(c: char)  { framebuffer::draw_char(c, FG, BG); }

fn put_u64(mut n: u64) {
    if n == 0 { puts("0"); return; }
    let mut buf = [0u8; 20]; let mut i = 0;
    while n > 0 { buf[i] = b'0' + (n % 10) as u8; n /= 10; i += 1; }
    for k in (0..i).rev() { putc(buf[k] as char); }
}

fn put_size(mb: u64) {
    if mb >= 1024 {
        let gib = mb / 1024; let rem = (mb % 1024) * 10 / 1024;
        put_u64(gib); puts("."); put_u64(rem); puts(" GiB");
    } else { put_u64(mb); puts(" MiB"); }
}

fn put_hex32(v: u32) {
    const HEX: &[u8] = b"0123456789abcdef";
    for i in (0..8).rev() {
        putc(HEX[((v >> (i * 4)) & 0xF) as usize] as char);
    }
}

// ── Keyboard ─────────────────────────────────────────────────────────────────
fn check_abort() -> bool {
    if let Some('\x03') = keyboard::try_read_key() { ABORT.store(true, Ordering::Relaxed); }
    ABORT.load(Ordering::Relaxed)
}

fn wait_enter() -> bool {
    loop {
        match keyboard::poll_key() {
            Some('\n') => return true,
            Some('\x03') => { ABORT.store(true, Ordering::Relaxed); return false; }
            _ => unsafe { core::arch::asm!("hlt"); }
        }
    }
}

fn read_digit_line(prompt: &str, max: usize) -> Option<usize> {
    puts(prompt);
    let mut inp = [0u8; 3]; let mut ilen = 0usize;
    loop {
        match keyboard::poll_key() {
            Some('\n') => { puts("\n"); break; }
            Some('\x03') => { ABORT.store(true, Ordering::Relaxed); puts("\n"); return None; }
            Some('\x08') if ilen > 0 => {
                ilen -= 1;
                framebuffer::draw_char('\x08', FG, BG);
                framebuffer::draw_char(' ', FG, BG);
                framebuffer::draw_char('\x08', FG, BG);
            }
            Some(c) if c.is_ascii_digit() && ilen < 2 => { inp[ilen] = c as u8; ilen += 1; putc(c); }
            _ => unsafe { core::arch::asm!("hlt"); }
        }
    }
    if ilen == 0 { return None; }
    let n = inp[..ilen].iter().fold(0usize, |a, &b| a * 10 + (b - b'0') as usize);
    if n < 1 || n > max { None } else { Some(n - 1) }
}

fn read_text_line(prompt: &str, buf: &mut [u8]) -> Option<usize> {
    puts(prompt);
    let mut len = 0usize;
    loop {
        match keyboard::poll_key() {
            Some('\n') => { puts("\n"); return Some(len); }
            Some('\x03') => { ABORT.store(true, Ordering::Relaxed); puts("\n"); return None; }
            Some('\x08') if len > 0 => {
                len -= 1;
                framebuffer::draw_char('\x08', FG, BG);
                framebuffer::draw_char(' ', FG, BG);
                framebuffer::draw_char('\x08', FG, BG);
            }
            Some(c) if (c.is_ascii_graphic() || c == ' ') && len < buf.len().saturating_sub(1) => {
                buf[len] = c as u8; len += 1; putc(c);
            }
            _ => unsafe { core::arch::asm!("hlt"); }
        }
    }
}

fn abort_screen() {
    puts("\n\n"); warn("  Installation aborted.\n");
    dim("  Press ENTER to return to shell...\n  > ");
    loop { match keyboard::poll_key() { Some('\n') => break, _ => unsafe { core::arch::asm!("hlt"); } } }
    framebuffer::clear(BG); framebuffer::set_cursor_pos(0, 0);
}

// ── Disk abstraction ─────────────────────────────────────────────────────────
/// `index == usize::MAX` → NVMe;  otherwise AHCI/ATA disk index.
#[derive(Clone, Copy)]
struct VDisk {
    index:    usize,
    sectors:  u64,
    size_mb:  u64,
    kind_str: &'static str,
}
impl VDisk {
    fn name(&self) -> &'static str { self.kind_str }
    fn is_nvme(&self) -> bool { self.index == usize::MAX }
    /// NVMe uses "p" before partition number (nvme0n1p1); AHCI uses "" (sda1).
    fn part_sep(&self) -> &'static str { if self.is_nvme() { "p" } else { "" } }
}

fn ahci_dev_name(d: &ospab_os::drivers::DiskInfo) -> &'static str {
    let idx = ospab_os::drivers::disk_info_count_before(d.index, d.kind);
    match d.kind {
        ospab_os::drivers::DiskKind::Ahci => match idx { 0=>"sda",1=>"sdb",2=>"sdc",_=>"sdX" },
        ospab_os::drivers::DiskKind::Ata  => match idx { 0=>"hda",1=>"hdb",2=>"hdc",_=>"hdX" },
    }
}

fn scan_disks() -> Vec<VDisk> {
    let mut v: Vec<VDisk> = Vec::new();
    if ospab_os::drivers::nvme::is_initialized() {
        let sc = ospab_os::drivers::nvme::sector_count();
        let ss = ospab_os::drivers::nvme::sector_size();
        let mb = (sc as u64).saturating_mul(ss as u64) / (1024 * 1024);
        v.push(VDisk { index: usize::MAX, sectors: sc, size_mb: mb, kind_str: "nvme0n1" });
    }
    for i in 0..ospab_os::drivers::disk_count() {
        if let Some(d) = ospab_os::drivers::disk_info(i) {
            v.push(VDisk { index: d.index, sectors: d.sectors, size_mb: d.size_mb,
                           kind_str: ahci_dev_name(d) });
        }
    }
    v
}

// ── I/O with retry ────────────────────────────────────────────────────────────
fn disk_write(disk: usize, lba: u64, count: u32, data: &[u8]) -> bool {
    if disk == usize::MAX { return ospab_os::drivers::nvme::write_sectors(lba, count, data); }
    for attempt in 0..3 {
        if ospab_os::drivers::write(disk, lba, count, data) { return true; }
        if attempt < 2 { for _ in 0..100_000usize { unsafe { core::arch::asm!("pause"); } } }
    }
    false
}

fn disk_read(disk: usize, lba: u64, count: u32, buf: &mut [u8]) -> bool {
    if disk == usize::MAX { return ospab_os::drivers::nvme::read_sectors(lba, count, buf); }
    for attempt in 0..3 {
        if ospab_os::drivers::read(disk, lba, count, buf) { return true; }
        if attempt < 2 { for _ in 0..100_000usize { unsafe { core::arch::asm!("pause"); } } }
    }
    false
}

// ── Write-cache flush barrier ─────────────────────────────────────────────────
//
// After writing the GPT and FAT32 structures, we issue a storage write-barrier
// so that UEFI firmware reads consistent sector data on the next power-cycle.
// NVMe : Flush command (opcode 0x00), writes volatile WC to non-volatile store.
// AHCI : ATA FLUSH CACHE EXT (0xEA), same effect for SATA/AHCI drives.
// Also adds a brief pause so the controller settles before we read back.
fn disk_flush(disk: usize) {
    slog("[INSTALLER] Issuing write-cache flush...\r\n");
    if disk == usize::MAX {
        ospab_os::drivers::nvme::flush();
    } else {
        ospab_os::drivers::ahci::flush_cache(disk);
    }
    // Short stall to let the controller drain its internal pipeline
    for _ in 0..500_000usize { unsafe { core::arch::asm!("pause"); } }
    slog("[INSTALLER] Flush done\r\n");
}

// ── Binary sources from Limine boot modules ───────────────────────────────────
fn get_limine_efi_binary() -> (&'static [u8], usize) {
    use ospab_os::arch::x86_64::boot;
    // Pass 1: by name
    if let Some(mut mods) = boot::modules() {
        while let Some(m) = mods.next() {
            let path = unsafe {
                if m.path.is_null() { continue; }
                let mut len = 0;
                while *m.path.add(len) != 0 { len += 1; }
                core::str::from_utf8_unchecked(
                    core::slice::from_raw_parts(m.path as *const u8, len))
            };
            if path.contains("BOOTX64") || path.contains("bootx64") || path.contains("limine") {
                let size = m.size as usize;
                slog("[INSTALLER] Limine EFI: "); slog(path); slog("\r\n");
                return (unsafe { core::slice::from_raw_parts(m.address, size) }, size);
            }
        }
    }
    // Pass 2: by MZ signature
    if let Some(mut mods) = boot::modules() {
        while let Some(m) = mods.next() {
            let size = m.size as usize;
            if size >= 2 {
                let data = unsafe { core::slice::from_raw_parts(m.address, size) };
                if data[0] == b'M' && data[1] == b'Z' {
                    slog("[INSTALLER] Limine EFI: MZ match\r\n");
                    return (data, size);
                }
            }
        }
    }
    slog("[INSTALLER] No Limine EFI module\r\n");
    (&[], 0)
}

fn get_kernel_binary() -> (&'static [u8], usize) {
    use ospab_os::arch::x86_64::boot;
    if let Some(mut mods) = boot::modules() {
        while let Some(m) = mods.next() {
            let path = unsafe {
                if m.path.is_null() { continue; }
                let mut len = 0;
                while *m.path.add(len) != 0 { len += 1; }
                core::str::from_utf8_unchecked(
                    core::slice::from_raw_parts(m.path as *const u8, len))
            };
            if path.contains("aeterna") || path.contains("ospab")
                || path.contains("KERNEL") || path.contains("kernel")
            {
                let size = m.size as usize;
                slog("[INSTALLER] Kernel: "); slog(path); slog("\r\n");
                return (unsafe { core::slice::from_raw_parts(m.address, size) }, size);
            }
        }
    }
    slog("[INSTALLER] No kernel module\r\n");
    (&[], 0)
}

fn slog(s: &str) { serial::write_str(s); }
fn slog_dec(mut v: u64) {
    if v == 0 { serial::write_byte(b'0'); return; }
    let mut b = [0u8; 20]; let mut i = 0;
    while v > 0 { b[i] = b'0' + (v % 10) as u8; v /= 10; i += 1; }
    for j in (0..i).rev() { serial::write_byte(b[j]); }
}
fn slog_hex32(v: u32) {
    const HEX: &[u8] = b"0123456789abcdef";
    serial::write_str("0x");
    for i in (0..8).rev() { serial::write_byte(HEX[((v >> (i * 4)) & 0xF) as usize]); }
}

// ── GPT ───────────────────────────────────────────────────────────────────────
const ESP_GUID:  [u8; 16] = [0x28,0x73,0x2A,0xC1,0x1F,0xF8,0xD2,0x11,0xBA,0x4B,0x00,0xA0,0xC9,0x3E,0xC9,0x3B];
const LINUX_GUID:[u8; 16] = [0xAF,0x3D,0xC6,0x0F,0x83,0x84,0x72,0x47,0x8E,0x79,0x3D,0x69,0xD8,0x47,0x7D,0xE4];

fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data { crc ^= b as u32; for _ in 0..8 { crc = if crc & 1 != 0 { (crc>>1)^0xEDB8_8320 } else { crc>>1 }; } }
    !crc
}
fn make_uuid(a: u64, b: u64) -> [u8; 16] {
    let mut u = [0u8; 16];
    let x = a.wrapping_mul(0x5DEECE66D).wrapping_add(b) ^ 0xAE05AE05AE05AE05;
    let y = x.wrapping_mul(0x6C078965).wrapping_add(1);
    u[0..8].copy_from_slice(&x.to_le_bytes()); u[8..16].copy_from_slice(&y.to_le_bytes());
    u[6] = (u[6]&0x0F)|0x40; u[8] = (u[8]&0x3F)|0x80; u
}
fn w16(buf: &mut [u8], o: usize, v: u16) { buf[o..o+2].copy_from_slice(&v.to_le_bytes()); }
fn w32(buf: &mut [u8], o: usize, v: u32) { buf[o..o+4].copy_from_slice(&v.to_le_bytes()); }
fn w64(buf: &mut [u8], o: usize, v: u64) { buf[o..o+8].copy_from_slice(&v.to_le_bytes()); }

// ── UI ────────────────────────────────────────────────────────────────────────
fn draw_header() {
    framebuffer::clear(BG); framebuffer::set_cursor_pos(0, 0);
    hl("+----------------------------------------------+\n");
    hl("|"); puts("   aeterna-install  --  AETERNA Microkernel   "); hl("|\n");
    hl("|"); dim("           x86_64   |   UEFI + BIOS           "); hl("|\n");
    hl("+----------------------------------------------+\n\n");
}
fn draw_sep() { dim("  ------------------------------------------------\n"); }

// ════════════════════════════════════════════════════════════════════════════
// Main entry point
// ════════════════════════════════════════════════════════════════════════════
pub fn run() {
    ABORT.store(false, Ordering::Relaxed);
    let all_disks = scan_disks();

    let mut sel_disk:   Option<usize> = None;
    let mut esp_mb:     u64 = 256;
    let mut hname_buf:  [u8; 32] = [0u8; 32];
    let mut hname_len:  usize = 0;
    {  // default hostname = "aeterna"
        let def = b"aeterna";
        hname_buf[..def.len()].copy_from_slice(def); hname_len = def.len();
    }
    let mut pass_buf:   [u8; 32] = [0u8; 32];
    let mut pass_len:   usize = 0;
    let mut locale_idx: usize = 0;
    const LOCALES: &[&str] = &["en_US.UTF-8","ru_RU.UTF-8","de_DE.UTF-8","fr_FR.UTF-8","zh_CN.UTF-8"];

    loop {
        if ABORT.load(Ordering::Relaxed) { abort_screen(); return; }

        draw_header();
        puts("  Configure and install AETERNA:\n\n");

        // [1] Drive
        puts("  "); hl("[1]"); puts("  Drive              > ");
        match sel_disk {
            None => warn("(not selected)\n"),
            Some(i) => if let Some(vd) = all_disks.get(i) {
                ok("/dev/"); ok(vd.name()); puts("  ("); put_size(vd.size_mb); puts(")\n");
            } else { warn("(not selected)\n"); },
        }
        // [2] Partition layout
        puts("  "); hl("[2]"); puts("  Partition layout   > ");
        put_size(esp_mb); ok(" ESP"); puts(" + rest → AETERNA root\n");
        // [3] Hostname
        puts("  "); hl("[3]"); puts("  Hostname           > ");
        for i in 0..hname_len { putc(hname_buf[i] as char); } puts("\n");
        // [4] Password
        puts("  "); hl("[4]"); puts("  Root password      > ");
        if pass_len == 0 { dim("(none)\n"); } else { dim("(set)\n"); }
        // [5] Locale
        puts("  "); hl("[5]"); puts("  Locale             > ");
        ok(LOCALES[locale_idx]); puts("\n");

        puts("\n"); draw_sep(); puts("\n");

        // Disk count hint
        puts("  Disks: "); put_u64(all_disks.len() as u64);
        if ospab_os::drivers::nvme::is_initialized() { dim("  (NVMe detected)"); }
        puts("\n\n");

        match sel_disk {
            None => { dim("  [i]  "); dim("Install  "); warn("← select a drive first\n"); }
            Some(_) => { hl("[i]  "); hl("Install  "); ok("← ready\n"); }
        }
        puts("  "); dim("[q]  Quit\n\n");
        draw_sep();
        puts("  > ");

        let ch = loop { match keyboard::poll_key() { Some(c) => break c, None => unsafe { core::arch::asm!("hlt"); } } };
        putc(ch); puts("\n");

        match ch {
            '1' => {
                draw_header();
                puts("  Available disks:\n\n");
                if all_disks.is_empty() {
                    err("  No disks detected.\n\n");
                    warn("  Attach a disk to QEMU:\n");
                    dim("    -drive file=disk.img,format=raw,if=none,id=d0\n");
                    dim("    -device ahci,id=ahci0 -device ide-hd,drive=d0,bus=ahci0.0\n\n");
                    dim("  Press ENTER...\n  > "); wait_enter(); continue;
                }
                for (i, vd) in all_disks.iter().enumerate() {
                    puts("  ["); put_u64(i as u64 + 1); puts("]  /dev/"); hl(vd.name());
                    puts("  "); put_size(vd.size_mb); puts("  ");
                    if vd.is_nvme() { dim("NVMe SSD"); }
                    else if let Some(d) = ospab_os::drivers::disk_info(vd.index) {
                        let model = ospab_os::drivers::model_str(d);
                        if !model.is_empty() { puts(model); } else { dim("(unknown)"); }
                        puts("  "); dim(match d.kind {
                            ospab_os::drivers::DiskKind::Ahci => "AHCI/SATA",
                            ospab_os::drivers::DiskKind::Ata  => "ATA/IDE",
                        });
                    }
                    puts("\n");
                }
                puts("\n"); warn("  WARNING: selected disk will be ERASED.\n\n");
                match read_digit_line("  Drive [1-N] (Enter=cancel): ", all_disks.len()) {
                    Some(idx) => {
                        if all_disks[idx].sectors < 204_800 {
                            err("  Disk too small (need >= 100 MiB).\n");
                            dim("  Press ENTER...\n  > "); wait_enter();
                        } else { sel_disk = Some(idx); }
                    }
                    None => { if ABORT.load(Ordering::Relaxed) { abort_screen(); return; } }
                }
            }
            '2' => {
                draw_header();
                puts("  Choose ESP size (rest = AETERNA root):\n\n");
                puts("  [1]  256 MiB  "); ok("← recommended\n");
                puts("  [2]  512 MiB\n");
                puts("  [3]  128 MiB\n");
                puts("  [4]   64 MiB  "); warn("(small disks only)\n");
                puts("\n");
                match read_digit_line("  Layout [1-4] (Enter=cancel): ", 4) {
                    Some(0) => esp_mb = 256, Some(1) => esp_mb = 512,
                    Some(2) => esp_mb = 128, Some(3) => esp_mb = 64,
                    _ => { if ABORT.load(Ordering::Relaxed) { abort_screen(); return; } }
                }
            }
            '3' => {
                draw_header();
                puts("  Hostname (default: aeterna):\n  Current: ");
                for i in 0..hname_len { putc(hname_buf[i] as char); } puts("\n\n");
                let mut tmp = [0u8; 32];
                match read_text_line("  Hostname: ", &mut tmp) {
                    Some(0) => {}
                    Some(n) => { hname_buf = tmp; hname_len = n; }
                    None => { if ABORT.load(Ordering::Relaxed) { abort_screen(); return; } }
                }
            }
            '4' => {
                draw_header(); puts("  Root password (empty = none):\n\n");
                let mut tmp = [0u8; 32];
                match read_text_line("  Password: ", &mut tmp) {
                    Some(n) => { pass_buf = tmp; pass_len = n; }
                    None => { if ABORT.load(Ordering::Relaxed) { abort_screen(); return; } }
                }
            }
            '5' => {
                draw_header(); puts("  Select locale:\n\n");
                for (i, loc) in LOCALES.iter().enumerate() {
                    puts("  ["); put_u64(i as u64 + 1); puts("]  ");
                    if i == locale_idx { ok(loc); puts("  ← current\n"); } else { puts(loc); puts("\n"); }
                }
                puts("\n");
                match read_digit_line("  Locale [1-N] (Enter=cancel): ", LOCALES.len()) {
                    Some(idx) => locale_idx = idx,
                    None => { if ABORT.load(Ordering::Relaxed) { abort_screen(); return; } }
                }
            }
            'i' | 'I' => {
                let idx = match sel_disk { Some(i) => i, None => {
                    err("  No drive selected.  Use option [1].\n");
                    dim("  Press ENTER...\n  > "); wait_enter(); continue;
                }};
                let disk = match all_disks.get(idx) { Some(&d) => d, None => { err("  Drive unavailable.\n"); continue; } };
                do_install(disk, esp_mb, &hname_buf[..hname_len], &pass_buf[..pass_len], LOCALES[locale_idx]);
                framebuffer::clear(BG); framebuffer::set_cursor_pos(0, 0);
                return;
            }
            'q' | 'Q' | '\x03' => { framebuffer::clear(BG); framebuffer::set_cursor_pos(0, 0); return; }
            _ => {}
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// do_install — writes everything to disk
// ════════════════════════════════════════════════════════════════════════════
fn do_install(disk: VDisk, esp_size_mb: u64, hostname: &[u8], _password: &[u8], locale: &str) {
    ABORT.store(false, Ordering::Relaxed);

    // ── Layout ────────────────────────────────────────────────────────────
    let esp_start:   u64 = 2048;
    let esp_sectors: u64 = esp_size_mb * 2048; // 512-byte sectors
    let esp_end:     u64 = esp_start + esp_sectors - 1;
    let root_start:  u64 = esp_end + 1;
    let root_end:    u64 = disk.sectors.saturating_sub(34);
    let root_mb:     u64 = (root_end.saturating_sub(root_start) + 1) / 2048;

    // ── Confirm ───────────────────────────────────────────────────────────
    draw_header();
    puts("  Installing AETERNA to  /dev/"); hl(disk.name()); puts("\n\n");
    puts("  Partition plan:\n");
    dim("  Dev         Start       End         Size        Type\n");
    dim("  ---------------------------------------------------\n");
    puts("  /dev/"); puts(disk.name()); puts(disk.part_sep()); hl("1");
    puts("   "); put_u64(esp_start); puts("  -  "); put_u64(esp_end);
    puts("   "); put_size(esp_size_mb); puts("  EFI System (FAT32)\n");
    puts("  /dev/"); puts(disk.name()); puts(disk.part_sep()); hl("2");
    puts("   "); put_u64(root_start); puts("  -  "); put_u64(root_end);
    puts("   "); put_size(root_mb); puts("  AETERNA root\n\n");

    puts("  Files to write in ESP:\n");
    dim("    /EFI/BOOT/BOOTX64.EFI   Limine UEFI bootloader\n");
    dim("    /boot/KERNEL            AETERNA kernel ELF\n");
    dim("    /limine.conf            Boot config\n\n");

    puts("  Hostname : "); for &b in hostname { putc(b as char); } puts("\n");
    puts("  Locale   : "); puts(locale); puts("\n\n");

    warn("  ALL DATA ON THIS DISK WILL BE ERASED.\n");
    puts("  Press ENTER to start, Ctrl+C to abort.\n  > ");
    if !wait_enter() || ABORT.load(Ordering::Relaxed) { abort_screen(); return; }

    // Fetch binaries first (fail-early if Limine is missing)
    let (efi_data, efi_size)       = get_limine_efi_binary();
    let (kernel_data, kernel_size) = get_kernel_binary();

    slog("[INSTALLER] /dev/"); slog(disk.name()); slog("\r\n");
    slog("[INSTALLER] EFI=");   slog_dec(efi_size as u64);    slog("B kernel=");
    slog_dec(kernel_size as u64); slog("B\r\n");

    // Step counter
    let total: u32 = 8;
    let mut sn: u32 = 0;
    macro_rules! hdr {
        ($msg:expr) => {{ sn += 1; puts("  ["); put_u64(sn as u64); puts("/"); put_u64(total as u64); puts("]  "); step($msg); puts(" ... "); }}
    }
    macro_rules! die {
        ($msg:expr) => {{
            err("FAILED\n"); err("  ✗  "); err($msg); err("\n");
            dim("  Press ENTER...\n  > "); wait_enter();
            framebuffer::clear(BG); framebuffer::set_cursor_pos(0, 0); return;
        }}
    }
    puts("\n"); draw_sep(); puts("\n");

    // ══════════════════════════════════════════════════════════════════════
    // 1 — Protective MBR
    hdr!("Protective MBR");
    slog("[INSTALLER] Step 1: Protective MBR\r\n");
    {
        let mut mbr = [0u8; 512];
        mbr[446]=0; mbr[447]=0; mbr[448]=2; mbr[449]=0; mbr[450]=0xEE;
        mbr[451]=0xFF; mbr[452]=0xFF; mbr[453]=0xFF;
        w32(&mut mbr, 454, 1);
        w32(&mut mbr, 458, (disk.sectors - 1).min(0xFFFF_FFFF) as u32);
        mbr[510]=0x55; mbr[511]=0xAA;
        if !disk_write(disk.index, 0, 1, &mbr) { die!("MBR write error"); }
    }
    slog("[INSTALLER] MBR OK\r\n");
    ok("OK\n"); if check_abort() { abort_screen(); return; }

    // ══════════════════════════════════════════════════════════════════════
    // 2 — GPT partition entries (sectors 2-33) + backup
    hdr!("GPT partition entries");
    slog("[INSTALLER] Step 2: GPT partition entries\r\n");
    let disk_guid = make_uuid(disk.index as u64 * 3, 0xFF);
    let esp_guid  = make_uuid(disk.index as u64 * 3, 0);
    let root_guid = make_uuid(disk.index as u64 * 3, 1);
    let mut entries = [0u8; 32 * 512];
    {
        let e = &mut entries[0..128];
        e[0..16].copy_from_slice(&ESP_GUID); e[16..32].copy_from_slice(&esp_guid);
        w64(e,32,esp_start); w64(e,40,esp_end); w64(e,48,0);
        for (i,c) in "EFI System".chars().enumerate() { if 56+i*2+1<128 { w16(e,56+i*2,c as u16); } }
    }
    {
        let e = &mut entries[128..256];
        e[0..16].copy_from_slice(&LINUX_GUID); e[16..32].copy_from_slice(&root_guid);
        w64(e,32,root_start); w64(e,40,root_end); w64(e,48,0);
        for (i,c) in "AETERNA".chars().enumerate() { if 56+i*2+1<128 { w16(e,56+i*2,c as u16); } }
    }
    let entries_crc = crc32(&entries);
    let bak_entries_lba = disk.sectors - 33;
    slog("  ESP: LBA "); slog_dec(esp_start); slog(".."); slog_dec(esp_end); slog("\r\n");
    slog("  ROOT: LBA "); slog_dec(root_start); slog(".."); slog_dec(root_end); slog("\r\n");
    slog("  entries_crc="); slog_hex32(entries_crc); slog("\r\n");
    for s in 0u64..32 {
        let o = (s as usize)*512;
        if !disk_write(disk.index, 2+s, 1, &entries[o..o+512]) { die!("GPT entries write error"); }
    }
    for s in 0u64..32 { let o=(s as usize)*512; disk_write(disk.index,bak_entries_lba+s,1,&entries[o..o+512]); }
    slog("[INSTALLER] GPT entries OK (backup LBA "); slog_dec(bak_entries_lba); slog(")\r\n");
    ok("OK\n"); if check_abort() { abort_screen(); return; }

    // ══════════════════════════════════════════════════════════════════════
    // 3 — GPT headers (primary + backup)
    hdr!("GPT headers");
    slog("[INSTALLER] Step 3: GPT headers\r\n");
    {
        let mut hdr = [0u8; 512];
        hdr[0..8].copy_from_slice(b"EFI PART");
        w32(&mut hdr,8,0x00010000); w32(&mut hdr,12,92); w32(&mut hdr,16,0); w32(&mut hdr,20,0);
        w64(&mut hdr,24,1); w64(&mut hdr,32,disk.sectors-1);
        w64(&mut hdr,40,34); w64(&mut hdr,48,disk.sectors-34);
        hdr[56..72].copy_from_slice(&disk_guid);
        w64(&mut hdr,72,2); w32(&mut hdr,80,128); w32(&mut hdr,84,128); w32(&mut hdr,88,entries_crc);
        let hcrc = crc32(&hdr[..92]); w32(&mut hdr,16,hcrc);
        slog("  primary hdr CRC="); slog_hex32(hcrc); slog("\r\n");
        if !disk_write(disk.index,1,1,&hdr) { die!("GPT primary header write error"); }
        // Backup
        w32(&mut hdr,16,0);
        w64(&mut hdr,24,disk.sectors-1); w64(&mut hdr,32,1); w64(&mut hdr,72,bak_entries_lba);
        let bcrc = crc32(&hdr[..92]); w32(&mut hdr,16,bcrc);
        slog("  backup  hdr CRC="); slog_hex32(bcrc); slog("\r\n");
        disk_write(disk.index,disk.sectors-1,1,&hdr);
    }
    ok("OK\n"); if check_abort() { abort_screen(); return; }

    // Flush write-cache so the UEFI firmware reads back committed data on boot
    slog("[INSTALLER] Flushing write-cache after GPT...\r\n");
    disk_flush(disk.index);

    // ══════════════════════════════════════════════════════════════════════
    // 4 — FAT32 ESP  BPB + FAT tables
    hdr!("FAT32 BPB + FAT tables");

    // ── Compute FAT32 geometry ────────────────────────────────────────────
    //
    // EDK2/Tianocore (VMware UEFI) determines FAT type by cluster count:
    //   CountOfClusters < 65525  →  FAT16  (WRONG for us → "No Media")
    //   CountOfClusters ≥ 65525  →  FAT32  (correct)
    //
    // So we MUST choose SPC (sectors-per-cluster) small enough to push
    // the cluster count above 65525.  For 256 MiB ESP, SPC=8 gives only
    // ~65400 clusters (< 65525) → EDK2 misidentifies as FAT16 → corrupt.
    //
    // We use the Microsoft FAT specification formula for FATSz:
    //   TmpVal1 = DskSize - BPB_RsvdSecCnt
    //   TmpVal2 = 256 * BPB_SecPerClus + BPB_NumFATs
    //   FATSz   = ceil(TmpVal1 / TmpVal2)

    const RSVD: u16 = 32;
    const NFAT: u8  = 2;
    let fat32_total = esp_sectors as u32;

    // Pick SPC so that data_clusters ≥ 65600 (safe margin above 65525)
    let spc: u8 = {
        let mut s: u32 = 8;
        loop {
            let tmp2 = 256 * s + NFAT as u32;
            let fsz  = (fat32_total - RSVD as u32 + tmp2 - 1) / tmp2;
            let drel = RSVD as u32 + fsz * NFAT as u32;
            let dcls = (fat32_total.saturating_sub(drel)) / s;
            if dcls >= 65600 || s == 1 { break; }
            s /= 2;
        }
        s as u8
    };

    let tmp1    = fat32_total - RSVD as u32;
    let tmp2    = 256u32 * spc as u32 + NFAT as u32;
    let fat_sec = (tmp1 + tmp2 - 1) / tmp2;
    let data_rel = RSVD as u32 + fat_sec * NFAT as u32;
    let data_clusters = (fat32_total - data_rel) / spc as u32;

    slog("[INSTALLER] FAT32 geometry:\r\n");
    slog("  total_sectors="); slog_dec(fat32_total as u64);
    slog("  SPC="); slog_dec(spc as u64);
    slog("  RSVD="); slog_dec(RSVD as u64);
    slog("  NFAT="); slog_dec(NFAT as u64); slog("\r\n");
    slog("  fat_sec="); slog_dec(fat_sec as u64);
    slog("  data_rel="); slog_dec(data_rel as u64);
    slog("  data_clusters="); slog_dec(data_clusters as u64);
    if data_clusters >= 65525 { slog(" (FAT32 OK)\r\n"); }
    else { slog(" (< 65525 = EDK2 sees FAT16 = BUG!)\r\n"); }

    {
        let mut vbr = [0u8; 512];
        vbr[0]=0xEB; vbr[1]=0x58; vbr[2]=0x90;
        vbr[3..11].copy_from_slice(b"AETERNA ");
        w16(&mut vbr,11,512); vbr[13]=spc; w16(&mut vbr,14,RSVD);
        vbr[16]=NFAT; w16(&mut vbr,17,0); w16(&mut vbr,19,0); vbr[21]=0xF8;
        w16(&mut vbr,22,0); w16(&mut vbr,24,63); w16(&mut vbr,26,255);
        w32(&mut vbr,28,esp_start as u32); w32(&mut vbr,32,fat32_total);
        w32(&mut vbr,36,fat_sec); w16(&mut vbr,40,0); w16(&mut vbr,42,0);
        w32(&mut vbr,44,2); w16(&mut vbr,48,1); w16(&mut vbr,50,6);
        vbr[64]=0x80; vbr[66]=0x29; w32(&mut vbr,67,0xAE05AE05);
        vbr[71..82].copy_from_slice(b"AETERNA ESP"); vbr[82..90].copy_from_slice(b"FAT32   ");
        vbr[510]=0x55; vbr[511]=0xAA;
        if !disk_write(disk.index,esp_start,1,&vbr) { die!("FAT32 VBR write error"); }
        slog("[INSTALLER] VBR written at LBA "); slog_dec(esp_start); slog("\r\n");

        let mut fsi = [0u8; 512];
        w32(&mut fsi,0,0x41615252); w32(&mut fsi,484,0x61417272);
        w32(&mut fsi,488,data_clusters.saturating_sub(8)); w32(&mut fsi,492,8);
        fsi[510]=0x55; fsi[511]=0xAA;
        disk_write(disk.index,esp_start+1,1,&fsi);
        disk_write(disk.index,esp_start+6,1,&vbr);  // backup VBR
        disk_write(disk.index,esp_start+7,1,&fsi);  // backup FSInfo
        slog("[INSTALLER] FSInfo + backups written\r\n");
    }
    {
        let zero = [0u8; 512];
        let fat1 = esp_start + RSVD as u64;
        let fat2 = fat1 + fat_sec as u64;
        slog("[INSTALLER] FAT1 LBA="); slog_dec(fat1);
        slog("  FAT2 LBA="); slog_dec(fat2);
        slog("  fat_sec="); slog_dec(fat_sec as u64); slog("\r\n");
        for fc in [fat1, fat2] { for s in 0..fat_sec as u64 { disk_write(disk.index,fc+s,1,&zero); } }
        let mut fat0 = [0u8; 512];
        w32(&mut fat0,0,0x0FFF_FFF8); w32(&mut fat0,4,0x0FFF_FFFF); w32(&mut fat0,8,0x0FFF_FFFF);
        disk_write(disk.index,fat1,1,&fat0); disk_write(disk.index,fat2,1,&fat0);
        slog("[INSTALLER] FAT tables zeroed + FAT[0..2] initialized\r\n");
    }
    ok("OK\n"); if check_abort() { abort_screen(); return; }

    // FAT LBAs — needed in Steps 5, 6, 7
    let fat1_lba = esp_start + RSVD as u64;
    let fat2_lba = fat1_lba + fat_sec as u64;

    // ══════════════════════════════════════════════════════════════════════
    // 5 — ESP directory tree (dynamic, non-overlapping cluster allocation)
    hdr!("ESP directory tree");
    slog("[INSTALLER] Step 5: ESP directory tree\r\n");
    let cb = spc as usize * 512; // bytes per cluster
    // Helper: sector LBA for cluster N  (cluster 2 = data region start)
    let clba = |c: u32| -> u64 { esp_start + data_rel as u64 + (c as u64 - 2) * spc as u64 };

    // Allocate clusters in order — no overlaps possible
    let mut nc   = 2u32;
    let root_c   = nc; nc += 1;  // 2  root directory
    let efi_dc   = nc; nc += 1;  // 3  /EFI/
    let boot_dc  = nc; nc += 1;  // 4  /EFI/BOOT/
    let sysb_c   = nc; nc += 1;  // 5  /boot/
    let conf_c   = nc; nc += 1;  // 6  /limine.conf

    let conf_content = build_limine_conf();

    let efi_nc = if efi_size > 0 { ((efi_size + cb - 1) / cb) as u32 } else { 1 };
    let efi_c  = nc; nc += efi_nc;

    let ker_nc = if kernel_size > 0 { ((kernel_size + cb - 1) / cb) as u32 } else { 1 };
    let ker_c  = nc; // nc += ker_nc; (last allocation)
    let _ = nc;

    // Zero directory clusters
    let zero = [0u8; 512];
    for c in [root_c, efi_dc, boot_dc, sysb_c] {
        let lba = clba(c);
        for s in 0..spc as u64 { disk_write(disk.index, lba+s, 1, &zero); }
    }

    // Root dir /
    {
        let mut d = [0u8; 512];
        d[0..11].copy_from_slice(b"AETERNA ESP"); d[11]=0x08; // volume label
        dir_ent(&mut d, 32, b"EFI        ", 0x10, efi_dc, 0);
        dir_ent(&mut d, 64, b"BOOT       ", 0x10, sysb_c, 0);
        // "limine.conf" — extension .conf is 4 chars, doesn't fit 8.3.
        // Without an LFN entry, FAT shows "LIMINE.CFG" and Limine
        // can't find its config.  We write 1 LFN entry + 8.3 short entry.
        {
            let sn: &[u8; 11] = b"LIMINE~1CON";
            let cksum = lfn_checksum(sn);
            // "limine.conf" in UCS-2LE
            let long_name: [u16; 11] = [
                0x6C, 0x69, 0x6D, 0x69, 0x6E, 0x65,  // l i m i n e
                0x2E,                                  // .
                0x63, 0x6F, 0x6E, 0x66,                // c o n f
            ];
            lfn_entry(&mut d, 96, 0x41, &long_name, cksum); // seq=1|LAST
            dir_ent(&mut d, 128, sn, 0x20, conf_c, conf_content.len() as u32);
        }
        disk_write(disk.index, clba(root_c), 1, &d);
        slog("[INSTALLER] Root dir: LFN entry for limine.conf written\r\n");
    }
    // /EFI/
    {
        let mut d = [0u8; 512];
        dot_ents(&mut d, efi_dc, root_c);
        dir_ent(&mut d, 64, b"BOOT       ", 0x10, boot_dc, 0);
        disk_write(disk.index, clba(efi_dc), 1, &d);
    }
    // /EFI/BOOT/
    {
        let mut d = [0u8; 512];
        dot_ents(&mut d, boot_dc, efi_dc);
        dir_ent(&mut d, 64, b"BOOTX64 EFI", 0x20, efi_c, efi_size as u32);
        disk_write(disk.index, clba(boot_dc), 1, &d);
    }
    // /boot/
    {
        let mut d = [0u8; 512];
        dot_ents(&mut d, sysb_c, root_c);
        dir_ent(&mut d, 64, b"KERNEL     ", 0x20, ker_c, kernel_size as u32);
        disk_write(disk.index, clba(sysb_c), 1, &d);
    }

    // FAT chain entries for all directories — without these, UEFI reads
    // FAT[cluster]=0 ("free") and treats the filesystem as corrupt.
    // root_c=2 is already end-of-chain in fat0; write_chain the rest.
    write_chain(disk.index, fat1_lba, fat2_lba, root_c,  1); // cluster 2
    write_chain(disk.index, fat1_lba, fat2_lba, efi_dc,  1); // cluster 3  /EFI/
    write_chain(disk.index, fat1_lba, fat2_lba, boot_dc, 1); // cluster 4  /EFI/BOOT/
    write_chain(disk.index, fat1_lba, fat2_lba, sysb_c,  1); // cluster 5  /boot/
    slog("[INSTALLER] Dir clusters: root="); slog_dec(root_c as u64);
    slog(" efi_dc="); slog_dec(efi_dc as u64);
    slog(" boot_dc="); slog_dec(boot_dc as u64);
    slog(" sysb="); slog_dec(sysb_c as u64);
    slog(" conf="); slog_dec(conf_c as u64); slog("\r\n");
    slog("  efi_c="); slog_dec(efi_c as u64); slog(" efi_nc="); slog_dec(efi_nc as u64);
    slog("  ker_c="); slog_dec(ker_c as u64); slog(" ker_nc="); slog_dec(ker_nc as u64); slog("\r\n");

    ok("OK\n"); if check_abort() { abort_screen(); return; }

    // ══════════════════════════════════════════════════════════════════════
    // 6 — /EFI/BOOT/BOOTX64.EFI
    hdr!("/EFI/BOOT/BOOTX64.EFI");
    if efi_size > 0 {
        write_clusters(disk.index, efi_data, efi_size, efi_c, efi_nc, &clba, spc);
        write_chain(disk.index, fat1_lba, fat2_lba, efi_c, efi_nc);
        slog("[INSTALLER] EFI written: clusters "); slog_dec(efi_c as u64);
        slog("..+"); slog_dec(efi_nc as u64);
        slog("  size="); slog_dec(efi_size as u64); slog("\r\n");
        ok("OK"); puts(" ("); put_u64(efi_size as u64); puts(" B)\n");
    } else {
        warn("SKIP (no Limine EFI module)\n");
    }
    if check_abort() { abort_screen(); return; }

    // ══════════════════════════════════════════════════════════════════════
    // 7 — /boot/KERNEL
    hdr!("/boot/KERNEL");
    if kernel_size > 0 {
        write_clusters(disk.index, kernel_data, kernel_size, ker_c, ker_nc, &clba, spc);
        write_chain(disk.index, fat1_lba, fat2_lba, ker_c, ker_nc);
        slog("[INSTALLER] KERNEL written: clusters "); slog_dec(ker_c as u64);
        slog("..+"); slog_dec(ker_nc as u64);
        slog("  size="); slog_dec(kernel_size as u64); slog("\r\n");
        ok("OK"); puts(" ("); put_u64(kernel_size as u64); puts(" B, ");
        put_u64(ker_nc as u64); puts(" clusters)\n");
    } else {
        warn("SKIP (no kernel module)\n");
        warn("  Install will not boot — rerun with kernel module present.\n");
    }
    if check_abort() { abort_screen(); return; }

    // Write /limine.conf (chain = 1 cluster)
    slog("[INSTALLER] Writing /limine.conf at cluster "); slog_dec(conf_c as u64); slog("\r\n");
    write_chain(disk.index, fat1_lba, fat2_lba, conf_c, 1);
    {
        let cb_lba = clba(conf_c);
        let cb_bytes = conf_content.as_bytes();
        let full = cb_bytes.len() / 512;
        for s in 0..full { disk_write(disk.index, cb_lba+s as u64, 1, &cb_bytes[s*512..(s+1)*512]); }
        let partial = cb_bytes.len() % 512;
        if partial > 0 || full == 0 {
            let mut last = [0u8; 512];
            last[..cb_bytes.len()-full*512].copy_from_slice(&cb_bytes[full*512..]);
            disk_write(disk.index, cb_lba+full as u64, 1, &last);
        }
    }

    // Flush after all ESP file writes (EFI + kernel + limine.conf)
    slog("[INSTALLER] Flushing write-cache after ESP files...\r\n");
    disk_flush(disk.index);

    // ══════════════════════════════════════════════════════════════════════
    // 8 — AETERNA identity sector on root partition
    hdr!("AETERNA identity record");
    slog("[INSTALLER] Step 8: Identity at LBA "); slog_dec(root_start); slog("\r\n");
    {
        let mut id = [0u8; 512];
        id[0..8].copy_from_slice(b"AETERNA ");
        let ver = crate::version::VERSION_STR.as_bytes();
        id[8..8+ver.len().min(16)].copy_from_slice(&ver[..ver.len().min(16)]);
        let arch = crate::version::ARCH.as_bytes();
        id[24..24+arch.len().min(8)].copy_from_slice(&arch[..arch.len().min(8)]);
        let date = crate::version::BUILD_DATE.as_bytes();
        id[32..32+date.len().min(16)].copy_from_slice(&date[..date.len().min(16)]);
        id[48..48+hostname.len().min(32)].copy_from_slice(&hostname[..hostname.len().min(32)]);
        let loc = locale.as_bytes();
        id[80..80+loc.len().min(16)].copy_from_slice(&loc[..loc.len().min(16)]);
        w64(&mut id, 96, esp_start);
        w64(&mut id, 104, root_start);
        w64(&mut id, 112, disk.size_mb);
        id[510]=0xAE; id[511]=0x05;
        if !disk_write(disk.index, root_start, 1, &id) { die!("Identity write error"); }
    }
    ok("OK\n");

    // Final flush: all FAT32 + kernel + identity data committed before readback
    slog("[INSTALLER] Final flush before verification...\r\n");
    disk_flush(disk.index);

    // ══════════════════════════════════════════════════════════════════════
    // Verify
    puts("\n"); draw_sep(); puts("  Verifying...\n\n");
    slog("[INSTALLER] === Verification ===\r\n");
    let mut all_ok = true;
    {
        let mut buf = [0u8; 512];
        puts("    Protective MBR    : ");
        if disk_read(disk.index,0,1,&mut buf) && buf[450]==0xEE { ok("OK\n"); } else { err("FAIL\n"); all_ok=false; }
    }
    // ── GPT: signature + header CRC32 + entries CRC32 ────────────────────────
    {
        let mut buf = [0u8; 512];
        puts("    GPT signature     : ");
        if disk_read(disk.index,1,1,&mut buf) && &buf[..8]==b"EFI PART" {
            ok("OK\n");
        } else { err("FAIL (LBA 1 missing 'EFI PART')\n"); all_ok=false; }

        puts("    GPT header CRC32  : ");
        if disk_read(disk.index,1,1,&mut buf) && &buf[..8]==b"EFI PART" {
            let stored = u32::from_le_bytes([buf[16],buf[17],buf[18],buf[19]]);
            buf[16]=0; buf[17]=0; buf[18]=0; buf[19]=0;   // zero field before check
            let computed = crc32(&buf[..92]);
            if stored == computed {
                ok("OK (0x"); put_hex32(stored); ok(")\n");
            } else {
                err("MISMATCH stored=0x"); put_hex32(stored);
                err(" computed=0x"); put_hex32(computed); err("\n");
                err("  !! UEFI will see this as corrupt and show No Media !!\n");
                all_ok = false;
            }
        } else { err("unreadable\n"); all_ok = false; }

        puts("    GPT entries CRC32 : ");
        if disk_read(disk.index,1,1,&mut buf) && &buf[..8]==b"EFI PART" {
            let hdr_ecrc = u32::from_le_bytes([buf[88],buf[89],buf[90],buf[91]]);
            extern crate alloc;
            let mut ebuf = alloc::vec![0u8; 32usize * 512];
            let mut read_ok = true;
            for s in 0u64..32 {
                let o = s as usize * 512;
                if !disk_read(disk.index, 2+s, 1, &mut ebuf[o..o+512]) { read_ok=false; break; }
            }
            if read_ok {
                let comp = crc32(&ebuf);
                if comp == hdr_ecrc { ok("OK (0x"); put_hex32(comp); ok(")\n"); }
                else {
                    err("MISMATCH stored=0x"); put_hex32(hdr_ecrc);
                    err(" computed=0x"); put_hex32(comp); err("\n");
                    all_ok = false;
                }
            } else { err("read error\n"); all_ok = false; }
        } else { err("header unreadable\n"); all_ok = false; }
    }
    {
        let mut buf = [0u8; 512];
        puts("    FAT32 VBR         : ");
        if disk_read(disk.index,esp_start,1,&mut buf) && buf[510]==0x55 && buf[511]==0xAA {
            ok("OK\n");
            // Dump key BPB fields to serial for debugging
            let bpb_spc  = buf[13];
            let bpb_rsvd = u16::from_le_bytes([buf[14], buf[15]]);
            let bpb_nfat = buf[16];
            let bpb_fatsz = u32::from_le_bytes([buf[36], buf[37], buf[38], buf[39]]);
            let bpb_tot32 = u32::from_le_bytes([buf[32], buf[33], buf[34], buf[35]]);
            let dr = bpb_rsvd as u32 + bpb_fatsz * bpb_nfat as u32;
            let dc = (bpb_tot32 - dr) / bpb_spc as u32;
            slog("[VERIFY] BPB: SPC="); slog_dec(bpb_spc as u64);
            slog(" RSVD="); slog_dec(bpb_rsvd as u64);
            slog(" NFAT="); slog_dec(bpb_nfat as u64);
            slog(" FATSz="); slog_dec(bpb_fatsz as u64);
            slog(" Tot32="); slog_dec(bpb_tot32 as u64); slog("\r\n");
            slog("[VERIFY] data_rel="); slog_dec(dr as u64);
            slog(" data_clusters="); slog_dec(dc as u64);
            if dc >= 65525 { slog(" -> FAT32 OK\r\n"); }
            else { slog(" -> FAT16 BUG! EDK2 will reject!\r\n"); }
        } else { err("FAIL\n"); all_ok=false; }
    }
    // Read-back FAT sector 0 — dump first 8 entries to serial
    {
        let mut fbuf = [0u8; 512];
        if disk_read(disk.index, fat1_lba, 1, &mut fbuf) {
            slog("[VERIFY] FAT1 sector 0 (first 8 entries):\r\n");
            for i in 0..8u32 {
                let off = i as usize * 4;
                let val = u32::from_le_bytes([fbuf[off], fbuf[off+1], fbuf[off+2], fbuf[off+3]]);
                slog("  FAT["); slog_dec(i as u64); slog("]="); slog_hex32(val); slog("\r\n");
            }
        }
    }
    {
        let mut buf = [0u8; 512];
        puts("    BOOTX64.EFI (MZ)  : ");
        if efi_size == 0 { warn("skipped\n"); }
        else if disk_read(disk.index, clba(efi_c), 1, &mut buf) && buf[0]==b'M' && buf[1]==b'Z' { ok("OK\n"); }
        else { err("FAIL\n"); all_ok=false; }
    }
    {
        let mut buf = [0u8; 512];
        puts("    /boot/KERNEL      : ");
        if kernel_size == 0 { warn("skipped\n"); }
        else if disk_read(disk.index, clba(ker_c), 1, &mut buf) && buf[..4]!=[0u8;4] { ok("OK\n"); }
        else { err("FAIL\n"); all_ok=false; }
    }
    {
        let mut buf = [0u8; 512];
        puts("    AETERNA identity  : ");
        if disk_read(disk.index,root_start,1,&mut buf) && &buf[..8]==b"AETERNA " && buf[510]==0xAE && buf[511]==0x05 { ok("OK\n"); }
        else { err("FAIL\n"); all_ok=false; }
    }

    // ══════════════════════════════════════════════════════════════════════
    // Summary
    puts("\n"); draw_sep();
    if all_ok { ok("  [OK] Installation complete!\n"); }
    else { warn("  [!!] Complete with warnings - check FAIL entries above.\n"); }
    draw_sep(); puts("\n");

    puts("  /dev/"); hl(disk.name()); puts(" - partition layout:\n\n");
    dim("    Sector 0          Protective MBR\n");
    dim("    Sector 1          GPT header\n");
    dim("    Sectors 2-33      GPT entries\n");
    puts("    "); put_u64(esp_start); puts(" - "); put_u64(esp_end);
    puts("   ESP FAT32 ("); put_size(esp_size_mb); puts(")\n");
    puts("    "); put_u64(root_start); puts(" - "); put_u64(root_end);
    puts("   AETERNA root ("); put_size(root_mb); puts(")\n\n");

    puts("  ESP:\n");
    dim("    /EFI/BOOT/BOOTX64.EFI  ");
    if efi_size>0 { ok("written ("); put_u64(efi_size as u64); ok(" B)\n"); } else { warn("not available\n"); }
    dim("    /boot/KERNEL           ");
    if kernel_size>0 { ok("written ("); put_u64(kernel_size as u64); ok(" B)\n"); } else { warn("not available\n"); }
    dim("    /limine.conf           "); ok("written\n\n");

    puts("  Hostname : "); for &b in hostname { putc(b as char); } puts("\n");
    puts("  Locale   : "); puts(locale); puts("\n\n");

    if all_ok { ok("  [+]  Disk is UEFI-bootable via Limine.\n\n"); }
    else { warn("  [!]  Some checks failed.  May not boot.\n\n"); }

    puts("  Next steps:\n");
    dim("    1. Power off and remove the live ISO\n");
    dim("    2. Boot the installed disk — UEFI picks /EFI/BOOT/BOOTX64.EFI\n\n");

    draw_sep(); dim("  Press ENTER to return to shell...\n  > ");
    wait_enter();
    framebuffer::clear(BG); framebuffer::set_cursor_pos(0, 0);
}

// ════════════════════════════════════════════════════════════════════════════
// FAT32 helpers
// ════════════════════════════════════════════════════════════════════════════
fn dir_ent(dir: &mut [u8], off: usize, name: &[u8; 11], attr: u8, clust: u32, size: u32) {
    let e = &mut dir[off..off+32];
    e[0..11].copy_from_slice(name); e[11]=attr;
    w16(e,20,(clust>>16) as u16); w16(e,26,(clust&0xFFFF) as u16); w32(e,28,size);
}
fn dot_ents(dir: &mut [u8], my: u32, parent: u32) {
    dir_ent(dir, 0,  b".          ", 0x10, my, 0);
    dir_ent(dir, 32, b"..         ", 0x10, parent, 0);
}
/// 8.3 short-name checksum used by FAT32 Long File Name entries.
fn lfn_checksum(sn: &[u8; 11]) -> u8 {
    let mut sum: u8 = 0;
    for i in 0..11 {
        sum = (if sum & 1 != 0 { 0x80u8 } else { 0u8 })
            .wrapping_add(sum >> 1)
            .wrapping_add(sn[i]);
    }
    sum
}
/// Write one LFN directory entry at `dir[off..off+32]`.
/// `seq` = ordinal (1-based, OR 0x40 for last entry).
/// `name` = full long name as UCS-2LE code units.
/// `cksum` = checksum of the corresponding 8.3 short name.
fn lfn_entry(dir: &mut [u8], off: usize, seq: u8, name: &[u16], cksum: u8) {
    let base = (seq & 0x3F) as usize - 1;  // 0-based index
    let start = base * 13;
    let e = &mut dir[off..off+32];
    e[0] = seq;
    // Chars 1-5  (bytes 1..10)
    for i in 0..5usize {
        let ch = if start+i < name.len() { name[start+i] }
                 else if start+i == name.len() { 0x0000 }
                 else { 0xFFFF };
        e[1+i*2]   = ch as u8;
        e[1+i*2+1] = (ch >> 8) as u8;
    }
    e[11] = 0x0F; // LFN attribute
    e[12] = 0x00; // type
    e[13] = cksum;
    // Chars 6-11 (bytes 14..25)
    for i in 0..6usize {
        let ch = if start+5+i < name.len() { name[start+5+i] }
                 else if start+5+i == name.len() { 0x0000 }
                 else { 0xFFFF };
        e[14+i*2]   = ch as u8;
        e[14+i*2+1] = (ch >> 8) as u8;
    }
    e[26] = 0; e[27] = 0; // first cluster = 0
    // Chars 12-13 (bytes 28..31)
    for i in 0..2usize {
        let ch = if start+11+i < name.len() { name[start+11+i] }
                 else if start+11+i == name.len() { 0x0000 }
                 else { 0xFFFF };
        e[28+i*2]   = ch as u8;
        e[28+i*2+1] = (ch >> 8) as u8;
    }
}
fn write_clusters<F>(disk: usize, data: &[u8], size: usize, start: u32, nc: u32, clba: &F, spc: u8)
where F: Fn(u32) -> u64 {
    let cb = spc as usize * 512;
    for ci in 0..nc as usize {
        let off = ci * cb;
        if off >= size { break; }
        let lba = clba(start + ci as u32);
        let rem  = (size - off).min(cb);
        let full = rem / 512;
        for s in 0..full { disk_write(disk, lba+s as u64, 1, &data[off+s*512..off+s*512+512]); }
        let part = rem % 512;
        if part > 0 {
            let mut last = [0u8; 512]; let so = off + full*512;
            last[..part].copy_from_slice(&data[so..so+part]);
            disk_write(disk, lba+full as u64, 1, &last);
        }
    }
}
fn write_chain(disk: usize, fat1: u64, fat2: u64, start: u32, nc: u32) {
    for i in 0..nc {
        let c   = start + i;
        let fao = c * 4;
        let fs  = fao / 512;
        let bo  = (fao % 512) as usize;
        let val: u32 = if i == nc-1 { 0x0FFF_FFFF } else { c+1 };
        for &fb in &[fat1, fat2] {
            let mut sec = [0u8; 512];
            disk_read(disk, fb+fs as u64, 1, &mut sec);
            w32(&mut sec, bo, val);
            disk_write(disk, fb+fs as u64, 1, &sec);
        }
    }
}

// ── limine.conf — matches the live ISO format exactly ────────────────────────
fn build_limine_conf() -> &'static str {
    "timeout: 5\nserial: yes\n\n/AETERNA Microkernel\n\tprotocol: limine\n\tkernel_path: boot():/boot/KERNEL\n"
}
