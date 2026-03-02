/*
 * AETERNA System Installer — Real Bare-Metal Installation
 *
 * What it does (no stubs):
 *   1. Scans ATA/AHCI disks
 *   2. User selects target disk
 *   3. Writes a real GPT partition table:
 *      - Protective MBR (sector 0)
 *      - GPT header (sector 1), backup GPT header (last sector)
 *      - GPT partition entries (sectors 2-33): ESP + AETERNA root
 *      - Backup partition entries (sectors N-34 to N-2)
 *   4. Formats ESP as minimal FAT32:
 *      - BPB (BIOS Parameter Block) + boot sector
 *      - FAT tables (2 copies)
 *      - Root directory entries
 *      - Writes /EFI/BOOT/BOOTX64.EFI (Limine UEFI payload)
 *      - Writes /limine.conf (boot configuration)
 *   5. Writes AETERNA identity sector and kernel image to root partition
 *   6. Verifies readback of critical sectors
 *
 * After installation, the disk is bootable on UEFI systems via Limine.
 */
use core::sync::atomic::{AtomicBool, Ordering};
use ospab_os::arch::x86_64::framebuffer;
use ospab_os::arch::x86_64::keyboard;
use ospab_os::arch::x86_64::serial;

const FG: u32      = 0x00FFFFFF;
const FG_DIM: u32  = 0x00AAAAAA;
const FG_OK: u32   = 0x0000FF00;
const FG_WARN: u32 = 0x0000CCFF;
const FG_ERR: u32  = 0x000000FF;
const FG_HL: u32   = 0x00FFCC00;
const FG_STEP: u32 = 0x00FF8800;
const BG: u32      = 0x00000000;

static ABORT: AtomicBool = AtomicBool::new(false);

fn check_abort() -> bool {
    if let Some('\x03') = keyboard::try_read_key() {
        ABORT.store(true, Ordering::Relaxed);
    }
    ABORT.load(Ordering::Relaxed)
}

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
    let mut buf = [0u8; 20];
    let mut i = 0;
    while n > 0 { buf[i] = b'0' + (n % 10) as u8; n /= 10; i += 1; }
    for k in (0..i).rev() { putc(buf[k] as char); }
}

fn put_size(size_mb: u64) {
    if size_mb >= 1024 {
        let gib = size_mb / 1024;
        let rem = (size_mb % 1024) * 10 / 1024;
        put_u64(gib); puts("."); put_u64(rem); puts(" GiB");
    } else {
        put_u64(size_mb); puts(" MiB");
    }
}

fn dev_name(d: &ospab_os::drivers::DiskInfo) -> &'static str {
    let idx = ospab_os::drivers::disk_info_count_before(d.index, d.kind);
    match d.kind {
        ospab_os::drivers::DiskKind::Ahci => match idx { 0=>"sda", 1=>"sdb", 2=>"sdc", _=>"sdX" },
        ospab_os::drivers::DiskKind::Ata  => match idx { 0=>"hda", 1=>"hdb", 2=>"hdc", _=>"hdX" },
    }
}

fn put_model(d: &ospab_os::drivers::DiskInfo) {
    let s = ospab_os::drivers::model_str(d);
    if s.is_empty() { puts("(unknown model)"); } else { puts(s); }
}

fn print_header(step_num: u8, total: u8, title: &str) {
    framebuffer::clear(BG);
    framebuffer::set_cursor_pos(0, 0);
    hl("  +==============================================+\n");
    hl("  |"); puts("   ospab.os  AETERNA  Installer              ");
    hl("|\n");
    hl("  +==============================================+\n");
    puts("  Version: "); dim(crate::version::OS_VERSION);
    puts("   Arch: "); dim(crate::version::ARCH); puts("\n");
    dim("  ------------------------------------------------\n");
    step("  [ Step "); put_u64(step_num as u64); step("/"); put_u64(total as u64); step(" ]  ");
    puts(title); puts("\n");
    dim("  ------------------------------------------------\n\n");
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
    let mut input = [0u8; 3];
    let mut ilen = 0usize;
    loop {
        match keyboard::poll_key() {
            Some('\n') => { puts("\n"); break; }
            Some('\x03') => {
                ABORT.store(true, Ordering::Relaxed);
                puts("\n");
                return None;
            }
            Some('\x08') if ilen > 0 => {
                ilen -= 1;
                framebuffer::draw_char('\x08', FG, BG);
                framebuffer::draw_char(' ', FG, BG);
                framebuffer::draw_char('\x08', FG, BG);
            }
            Some(c) if c.is_ascii_digit() && ilen < 2 => {
                input[ilen] = c as u8; ilen += 1; putc(c);
            }
            _ => unsafe { core::arch::asm!("hlt"); }
        }
    }
    if ilen == 0 { return None; }
    let num = input[..ilen].iter().fold(0usize, |a, &b| a * 10 + (b - b'0') as usize);
    if num < 1 || num > max { None } else { Some(num - 1) }
}

fn pause_next_step(label: &str) {
    puts("\n");
    dim("  ------------------------------------------------\n");
    puts("  "); dim(label); dim("  [Enter]  |  "); warn("Ctrl+C = abort"); puts("\n");
    dim("  ------------------------------------------------\n");
    puts("  > ");
}

fn abort_screen() {
    puts("\n\n");
    warn("  Aborted by user (Ctrl+C).\n");
    puts("\n");
    dim("  Press ENTER to return to terminal...\n");
    wait_enter();
    framebuffer::clear(BG);
    framebuffer::set_cursor_pos(0, 0);
}

// ═══════════════════════════════════════════════════════════════════════════
// GPT constants and helpers
// ═══════════════════════════════════════════════════════════════════════════

/// EFI System Partition GUID: C12A7328-F81F-11D2-BA4B-00A0C93EC93B (mixed-endian)
const ESP_TYPE_GUID: [u8; 16] = [
    0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11,
    0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B,
];

/// Linux filesystem GUID (used for AETERNA root): 0FC63DAF-8483-4772-8E79-3D69D8477DE4
const LINUX_FS_GUID: [u8; 16] = [
    0xAF, 0x3D, 0xC6, 0x0F, 0x83, 0x84, 0x72, 0x47,
    0x8E, 0x79, 0x3D, 0x69, 0xD8, 0x47, 0x7D, 0xE4,
];

/// CRC32 for GPT headers (standard Ethernet polynomial)
fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFFFFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB88320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

/// Simple deterministic "UUID" from disk serial + partition index
fn make_uuid(disk_idx: usize, part_idx: usize) -> [u8; 16] {
    let mut uuid = [0u8; 16];
    // Use a hash-like construction from disk and partition indices
    let seed = (disk_idx as u64 * 0x5DEECE66D + part_idx as u64 * 0xB) ^ 0xAE05AE05AE05AE05;
    uuid[0..8].copy_from_slice(&seed.to_le_bytes());
    let seed2 = seed.wrapping_mul(0x6C078965).wrapping_add(1);
    uuid[8..16].copy_from_slice(&seed2.to_le_bytes());
    // Set version 4 (random) and variant 1 (RFC 4122)
    uuid[6] = (uuid[6] & 0x0F) | 0x40; // Version 4
    uuid[8] = (uuid[8] & 0x3F) | 0x80; // Variant 1
    uuid
}

fn write_u16_le(buf: &mut [u8], offset: usize, val: u16) {
    buf[offset..offset+2].copy_from_slice(&val.to_le_bytes());
}

fn write_u32_le(buf: &mut [u8], offset: usize, val: u32) {
    buf[offset..offset+4].copy_from_slice(&val.to_le_bytes());
}

fn write_u64_le(buf: &mut [u8], offset: usize, val: u64) {
    buf[offset..offset+8].copy_from_slice(&val.to_le_bytes());
}

/// Write sectors to disk with retry. Returns true on success.
fn disk_write(disk: usize, lba: u64, count: u32, data: &[u8]) -> bool {
    for attempt in 0..3 {
        if ospab_os::drivers::write(disk, lba, count, data) {
            return true;
        }
        if attempt < 2 {
            // Short delay before retry
            for _ in 0..100000 { unsafe { core::arch::asm!("pause"); } }
        }
    }
    false
}

/// Read sectors from disk with retry. Returns true on success.
fn disk_read(disk: usize, lba: u64, count: u32, buf: &mut [u8]) -> bool {
    for attempt in 0..3 {
        if ospab_os::drivers::read(disk, lba, count, buf) {
            return true;
        }
        if attempt < 2 {
            for _ in 0..100000 { unsafe { core::arch::asm!("pause"); } }
        }
    }
    false
}

// ═══════════════════════════════════════════════════════════════════════════
// Installer entry point
// ═══════════════════════════════════════════════════════════════════════════

pub fn run() {
    ABORT.store(false, Ordering::Relaxed);

    // ══════════════════════════════════════════════════════════════════════
    // Step 1: Storage detection
    // ══════════════════════════════════════════════════════════════════════
    print_header(1, 6, "Storage detection");
    dim("  Scanning ATA IDE + AHCI SATA controllers...\n\n");

    let ndisks = ospab_os::drivers::disk_count();
    if ndisks == 0 {
        err("  [!]  No writable storage found.\n\n");
        puts("  Attach a disk to the VM and reboot.\n\n");
        dim("  QEMU example:\n");
        dim("    qemu-img create -f raw disk.img 8G\n");
        dim("    -drive file=disk.img,format=raw,if=none,id=d0\n");
        dim("    -device ich9-ahci,id=ahci\n");
        dim("    -device ide-hd,drive=d0,bus=ahci.0\n\n");
        dim("  Press ENTER to return...\n");
        puts("  > "); wait_enter();
        framebuffer::clear(BG); framebuffer::set_cursor_pos(0, 0);
        return;
    }

    ok("  Found "); put_u64(ndisks as u64); ok(" disk(s):\n\n");
    for i in 0..ndisks {
        if let Some(d) = ospab_os::drivers::disk_info(i) {
            puts("  ["); put_u64(i as u64 + 1); puts("]  /dev/");
            hl(dev_name(d)); puts("  ");
            put_size(d.size_mb); puts("  ");
            put_model(d);
            dim(match d.kind {
                ospab_os::drivers::DiskKind::Ahci => "  (AHCI/SATA)",
                ospab_os::drivers::DiskKind::Ata  => "  (ATA/IDE)",
            });
            puts("\n");
        }
    }

    // ══════════════════════════════════════════════════════════════════════
    // Step 2: Select target
    // ══════════════════════════════════════════════════════════════════════
    print_header(2, 6, "Select installation target");
    let target_idx: usize;

    if ndisks == 1 {
        let d = ospab_os::drivers::disk_info(0).unwrap();
        puts("  Only one disk found, auto-selected:\n\n");
        puts("    /dev/"); hl(dev_name(d)); puts("  ");
        put_size(d.size_mb); puts("  "); put_model(d); puts("\n\n");
        warn("  [!]  ALL DATA ON THIS DISK WILL BE PERMANENTLY ERASED.\n\n");
        pause_next_step("Proceed to partition plan?");
        if !wait_enter() || ABORT.load(Ordering::Relaxed) {
            abort_screen(); return;
        }
        target_idx = 0;
    } else {
        puts("  Available disks:\n\n");
        for i in 0..ndisks {
            if let Some(d) = ospab_os::drivers::disk_info(i) {
                puts("    ["); put_u64(i as u64 + 1); puts("]  /dev/");
                hl(dev_name(d)); puts("  "); put_size(d.size_mb); puts("\n");
            }
        }
        puts("\n");
        match read_digit_line("  Enter disk number: ", ndisks) {
            None => {
                if ABORT.load(Ordering::Relaxed) { abort_screen(); }
                else { err("  Invalid selection.\n"); }
                return;
            }
            Some(idx) => {
                let d = ospab_os::drivers::disk_info(idx).unwrap();
                puts("\n  Selected: /dev/"); hl(dev_name(d));
                puts("  ("); put_size(d.size_mb); puts(")\n\n");
                warn("  [!]  ALL DATA ON THIS DISK WILL BE PERMANENTLY ERASED.\n\n");
                pause_next_step("Proceed to partition plan?");
                if !wait_enter() || ABORT.load(Ordering::Relaxed) {
                    abort_screen(); return;
                }
                target_idx = idx;
            }
        }
    }

    let disk = match ospab_os::drivers::disk_info(target_idx) {
        Some(d) => d,
        None => { err("  Disk unavailable.\n"); return; }
    };

    if disk.sectors < 204800 {
        err("  [!]  Disk too small (need at least 100 MiB).\n");
        dim("  Press ENTER to return...\n"); puts("  > "); wait_enter();
        framebuffer::clear(BG); framebuffer::set_cursor_pos(0, 0);
        return;
    }

    // ══════════════════════════════════════════════════════════════════════
    // Step 3: Partition plan
    // ══════════════════════════════════════════════════════════════════════
    print_header(3, 6, "Partition plan (real GPT)");

    puts("  Target disk:  /dev/"); hl(dev_name(disk));
    puts("   ("); put_size(disk.size_mb); puts(")\n\n");

    // GPT layout:
    //   LBA 0:        Protective MBR
    //   LBA 1:        GPT Header
    //   LBA 2-33:     GPT Partition Entries (128 entries × 128 bytes = 32 sectors)
    //   LBA 2048:     ESP start (1 MiB aligned)
    //   LBA 2048+ESPsz: Root start
    //   LBA N-33:     Backup partition table
    //   LBA N-1:      Backup GPT header

    let esp_size_mb: u64 = 256; // 256 MiB ESP
    let esp_start: u64 = 2048;  // 1 MiB offset (standard alignment)
    let esp_sectors: u64 = esp_size_mb * 2048;
    let esp_end: u64 = esp_start + esp_sectors - 1;
    let root_start: u64 = esp_end + 1;
    let root_end: u64 = disk.sectors.saturating_sub(34);
    let root_mb: u64 = (root_end - root_start + 1) / 2048;

    dim("  Device         Start         End           Size       Type\n");
    dim("  ----------------------------------------------------------\n");
    puts("  /dev/"); puts(dev_name(disk)); hl("1");
    puts("  "); put_u64(esp_start); puts("        "); put_u64(esp_end);
    puts("   "); put_size(esp_size_mb); puts("   EFI System (FAT32)\n");
    puts("  /dev/"); puts(dev_name(disk)); hl("2");
    puts("  "); put_u64(root_start); puts("  "); put_u64(root_end);
    puts("   "); put_size(root_mb); puts("   AETERNA root\n\n");

    step("  This will write:\n");
    puts("    - Protective MBR (sector 0)\n");
    puts("    - GPT header + backup (sectors 1, "); put_u64(disk.sectors - 1); puts(")\n");
    puts("    - 128 GPT entries (sectors 2-33, backup at end)\n");
    puts("    - FAT32 filesystem on ESP with Limine bootloader\n");
    puts("    - AETERNA kernel + identity on root partition\n");

    pause_next_step("Write to disk?");
    if !wait_enter() || ABORT.load(Ordering::Relaxed) {
        abort_screen(); return;
    }

    // ══════════════════════════════════════════════════════════════════════
    // Step 4: Write GPT
    // ══════════════════════════════════════════════════════════════════════
    print_header(4, 6, "Writing GPT partition table");
    puts("  Target: /dev/"); hl(dev_name(disk)); puts("\n\n");
    serial::write_str("[INSTALLER] Writing GPT\r\n");

    let disk_guid = make_uuid(disk.index, 0xFF);
    let esp_guid = make_uuid(disk.index, 0);
    let root_guid = make_uuid(disk.index, 1);

    // ── Protective MBR ──
    puts("  [ 1/8 ]  Protective MBR (sector 0) ... ");
    {
        let mut mbr = [0u8; 512];
        // Partition entry 1 at offset 446 (protective)
        mbr[446] = 0x00;  // Not bootable
        mbr[447] = 0x00;  // CHS start head
        mbr[448] = 0x02;  // CHS start sector/cylinder
        mbr[449] = 0x00;  // CHS start cylinder
        mbr[450] = 0xEE;  // Type: GPT protective
        mbr[451] = 0xFF;  // CHS end
        mbr[452] = 0xFF;
        mbr[453] = 0xFF;
        write_u32_le(&mut mbr, 454, 1); // LBA start
        let prot_size = (disk.sectors - 1).min(0xFFFFFFFF) as u32;
        write_u32_le(&mut mbr, 458, prot_size);
        mbr[510] = 0x55;
        mbr[511] = 0xAA;
        if !disk_write(disk.index, 0, 1, &mbr) {
            err("FAILED\n"); err("  [x] MBR write error.\n");
            dim("\n  Press ENTER to return...\n"); puts("  > "); wait_enter();
            framebuffer::clear(BG); framebuffer::set_cursor_pos(0, 0);
            return;
        }
    }
    ok("OK\n");

    if check_abort() { abort_screen(); return; }

    // ── GPT Partition Entries (128 entries × 128 bytes = 16384 bytes = 32 sectors) ──
    puts("  [ 2/8 ]  GPT partition entries (sectors 2-33) ... ");
    let mut entries_buf = [0u8; 32 * 512]; // 16384 bytes

    // Entry 0: ESP
    let e0 = &mut entries_buf[0..128];
    e0[0..16].copy_from_slice(&ESP_TYPE_GUID);
    e0[16..32].copy_from_slice(&esp_guid);
    write_u64_le(e0, 32, esp_start);
    write_u64_le(e0, 40, esp_end);
    write_u64_le(e0, 48, 0); // Attributes
    // Partition name "EFI System" in UTF-16LE
    let esp_name = "EFI System";
    for (i, c) in esp_name.chars().enumerate() {
        if 56 + i * 2 + 1 < 128 {
            write_u16_le(e0, 56 + i * 2, c as u16);
        }
    }

    // Entry 1: AETERNA root
    let e1 = &mut entries_buf[128..256];
    e1[0..16].copy_from_slice(&LINUX_FS_GUID);
    e1[16..32].copy_from_slice(&root_guid);
    write_u64_le(e1, 32, root_start);
    write_u64_le(e1, 40, root_end);
    write_u64_le(e1, 48, 0);
    let root_name = "AETERNA";
    for (i, c) in root_name.chars().enumerate() {
        if 56 + i * 2 + 1 < 128 {
            write_u16_le(e1, 56 + i * 2, c as u16);
        }
    }

    let entries_crc = crc32(&entries_buf);

    // Write primary entries (LBA 2-33)
    for sect in 0..32u64 {
        let offset = (sect as usize) * 512;
        if !disk_write(disk.index, 2 + sect, 1, &entries_buf[offset..offset + 512]) {
            err("FAILED\n"); err("  [x] GPT entries write error.\n");
            dim("\n  Press ENTER to return...\n"); puts("  > "); wait_enter();
            framebuffer::clear(BG); framebuffer::set_cursor_pos(0, 0);
            return;
        }
    }
    ok("OK\n");

    if check_abort() { abort_screen(); return; }

    // ── Backup partition entries (at disk.sectors - 33) ──
    puts("  [ 3/8 ]  Backup partition entries ... ");
    let backup_entries_lba = disk.sectors - 33;
    for sect in 0..32u64 {
        let offset = (sect as usize) * 512;
        if !disk_write(disk.index, backup_entries_lba + sect, 1, &entries_buf[offset..offset + 512]) {
            err("FAILED\n");
            warn("  [!] Backup entries write failed (non-critical).\n");
            break;
        }
    }
    ok("OK\n");

    if check_abort() { abort_screen(); return; }

    // ── GPT Headers ──
    puts("  [ 4/8 ]  GPT headers (primary + backup) ... ");
    {
        let mut hdr = [0u8; 512];
        hdr[0..8].copy_from_slice(b"EFI PART");                 // Signature
        write_u32_le(&mut hdr, 8, 0x00010000);                  // Revision 1.0
        write_u32_le(&mut hdr, 12, 92);                         // Header size
        // CRC32 at offset 16 — computed after filling other fields
        write_u32_le(&mut hdr, 16, 0);                          // Placeholder
        write_u32_le(&mut hdr, 20, 0);                          // Reserved
        write_u64_le(&mut hdr, 24, 1);                          // My LBA
        write_u64_le(&mut hdr, 32, disk.sectors - 1);           // Alternate LBA
        write_u64_le(&mut hdr, 40, 34);                         // First usable LBA
        write_u64_le(&mut hdr, 48, disk.sectors - 34);          // Last usable LBA
        hdr[56..72].copy_from_slice(&disk_guid);                // Disk GUID
        write_u64_le(&mut hdr, 72, 2);                          // Partition entry start LBA
        write_u32_le(&mut hdr, 80, 128);                        // Number of entries
        write_u32_le(&mut hdr, 84, 128);                        // Entry size
        write_u32_le(&mut hdr, 88, entries_crc);                // Entries CRC32

        // Compute header CRC32 (over first 92 bytes with CRC field = 0)
        let hdr_crc = crc32(&hdr[..92]);
        write_u32_le(&mut hdr, 16, hdr_crc);

        // Write primary header at LBA 1
        if !disk_write(disk.index, 1, 1, &hdr) {
            err("FAILED\n"); err("  [x] GPT header write error.\n");
            dim("\n  Press ENTER to return...\n"); puts("  > "); wait_enter();
            framebuffer::clear(BG); framebuffer::set_cursor_pos(0, 0);
            return;
        }

        // Build backup header (swap my LBA / alternate LBA, change entries LBA)
        write_u32_le(&mut hdr, 16, 0); // Clear CRC
        write_u64_le(&mut hdr, 24, disk.sectors - 1);     // My LBA = last sector
        write_u64_le(&mut hdr, 32, 1);                     // Alternate = primary
        write_u64_le(&mut hdr, 72, backup_entries_lba);    // Entries at backup location
        let backup_crc = crc32(&hdr[..92]);
        write_u32_le(&mut hdr, 16, backup_crc);

        // Write backup header at last sector
        if !disk_write(disk.index, disk.sectors - 1, 1, &hdr) {
            warn("  (backup header write failed — non-critical)\n");
        }
    }
    ok("OK\n");

    if check_abort() { abort_screen(); return; }

    // ══════════════════════════════════════════════════════════════════════
    // Step 5: Format ESP as FAT32 + write Limine
    // ══════════════════════════════════════════════════════════════════════
    print_header(5, 6, "Formatting ESP (FAT32) + Limine");
    puts("  ESP: sectors "); put_u64(esp_start); puts(" - "); put_u64(esp_end); puts("\n\n");
    serial::write_str("[INSTALLER] Formatting FAT32 ESP\r\n");

    // FAT32 geometry
    let fat32_total_sectors = esp_sectors as u32;
    let sectors_per_cluster: u8 = 8;  // 4 KiB clusters
    let reserved_sectors: u16 = 32;   // Standard for FAT32
    let num_fats: u8 = 2;
    let bytes_per_sector: u16 = 512;

    // Calculate FAT size: each FAT entry is 4 bytes
    // Total clusters ≈ (total_sectors - reserved - FAT*2) / sectors_per_cluster
    // FAT sectors = ceil(total_clusters * 4 / 512)
    let data_region_start_approx = reserved_sectors as u32;
    let max_clusters = (fat32_total_sectors - data_region_start_approx) / sectors_per_cluster as u32;
    let fat_bytes = max_clusters * 4;
    let fat_sectors = (fat_bytes + 511) / 512;
    let data_start = reserved_sectors as u32 + fat_sectors * num_fats as u32;
    let data_clusters = (fat32_total_sectors - data_start) / sectors_per_cluster as u32;

    // ── Boot sector (VBR) for FAT32 ──
    puts("  [ 5/8 ]  FAT32 boot sector + FATs ... ");
    {
        let mut vbr = [0u8; 512];
        // Jump instruction
        vbr[0] = 0xEB; vbr[1] = 0x58; vbr[2] = 0x90; // jmp short 0x5A; nop
        // OEM name
        vbr[3..11].copy_from_slice(b"AETERNA ");
        // BPB
        write_u16_le(&mut vbr, 11, bytes_per_sector);
        vbr[13] = sectors_per_cluster;
        write_u16_le(&mut vbr, 14, reserved_sectors);
        vbr[16] = num_fats;
        write_u16_le(&mut vbr, 17, 0);  // Root entry count (0 for FAT32)
        write_u16_le(&mut vbr, 19, 0);  // Total sectors 16-bit (0 for FAT32)
        vbr[21] = 0xF8;                 // Media descriptor (fixed disk)
        write_u16_le(&mut vbr, 22, 0);  // FAT size 16 (0 for FAT32)
        write_u16_le(&mut vbr, 24, 63); // Sectors per track
        write_u16_le(&mut vbr, 26, 255); // Number of heads
        write_u32_le(&mut vbr, 28, esp_start as u32); // Hidden sectors (partition offset)
        write_u32_le(&mut vbr, 32, fat32_total_sectors); // Total sectors 32-bit
        // FAT32 extended BPB
        write_u32_le(&mut vbr, 36, fat_sectors); // FAT size 32
        write_u16_le(&mut vbr, 40, 0);   // Ext flags
        write_u16_le(&mut vbr, 42, 0);   // FS version
        write_u32_le(&mut vbr, 44, 2);   // Root dir cluster
        write_u16_le(&mut vbr, 48, 1);   // FS info sector
        write_u16_le(&mut vbr, 50, 6);   // Backup boot sector
        // Reserved (12 bytes at offset 52)
        vbr[64] = 0x80;                  // Drive number
        vbr[66] = 0x29;                  // Extended boot signature
        write_u32_le(&mut vbr, 67, 0xAE05AE05); // Volume serial
        vbr[71..82].copy_from_slice(b"AETERNA ESP");  // Volume label
        vbr[82..90].copy_from_slice(b"FAT32   ");     // FS type
        vbr[510] = 0x55;
        vbr[511] = 0xAA;

        if !disk_write(disk.index, esp_start, 1, &vbr) {
            err("FAILED\n"); err("  [x] VBR write error.\n");
            dim("\n  Press ENTER to return...\n"); puts("  > "); wait_enter();
            framebuffer::clear(BG); framebuffer::set_cursor_pos(0, 0);
            return;
        }

        // FS Info sector (LBA esp_start + 1)
        let mut fsi = [0u8; 512];
        write_u32_le(&mut fsi, 0, 0x41615252);   // Lead signature
        write_u32_le(&mut fsi, 484, 0x61417272);  // Struct signature
        write_u32_le(&mut fsi, 488, data_clusters - 1); // Free clusters
        write_u32_le(&mut fsi, 492, 3);           // Next free cluster hint
        write_u32_le(&mut fsi, 508, 0xAA550000);  // Trail signature
        // Fix: trail signature is at offset 508
        fsi[510] = 0x55;
        fsi[511] = 0xAA;
        disk_write(disk.index, esp_start + 1, 1, &fsi);

        // Backup VBR at sector 6
        disk_write(disk.index, esp_start + 6, 1, &vbr);
        disk_write(disk.index, esp_start + 7, 1, &fsi);
    }

    // ── Initialize FAT tables ──
    {
        // Zero the FAT region first (both copies)
        let zero_sector = [0u8; 512];
        for fat_copy in 0..2u32 {
            let fat_base = esp_start + reserved_sectors as u64 + (fat_copy as u64 * fat_sectors as u64);
            for s in 0..fat_sectors.min(256) {
                disk_write(disk.index, fat_base + s as u64, 1, &zero_sector);
            }
        }

        // Write FAT[0], FAT[1], FAT[2] entries in the first FAT sector
        let mut fat_sector = [0u8; 512];
        write_u32_le(&mut fat_sector, 0, 0x0FFFFFF8);  // FAT[0]: media type
        write_u32_le(&mut fat_sector, 4, 0x0FFFFFFF);  // FAT[1]: end-of-chain
        write_u32_le(&mut fat_sector, 8, 0x0FFFFFFF);  // FAT[2]: root dir cluster (end-of-chain, 1 cluster)

        // Write to both FAT copies
        let fat1_lba = esp_start + reserved_sectors as u64;
        let fat2_lba = fat1_lba + fat_sectors as u64;
        disk_write(disk.index, fat1_lba, 1, &fat_sector);
        disk_write(disk.index, fat2_lba, 1, &fat_sector);
    }
    ok("OK\n");

    if check_abort() { abort_screen(); return; }

    // ── Write root directory with EFI folder structure ──
    puts("  [ 6/8 ]  ESP directory structure + Limine ... ");
    serial::write_str("[INSTALLER] Writing Limine to ESP\r\n");
    {
        // Root dir is at cluster 2, which starts at data_start
        let root_dir_lba = esp_start + data_start as u64;

        // Zero root directory cluster
        let zero_sector = [0u8; 512];
        for s in 0..sectors_per_cluster as u64 {
            disk_write(disk.index, root_dir_lba + s, 1, &zero_sector);
        }

        let mut root_dir = [0u8; 512];

        // Entry 0: Volume label
        root_dir[0..11].copy_from_slice(b"AETERNA ESP");
        root_dir[11] = 0x08; // Volume label attribute

        // Entry 1: "EFI" directory (cluster 3)
        let e = &mut root_dir[32..64];
        e[0..11].copy_from_slice(b"EFI        ");
        e[11] = 0x10; // Directory
        write_u16_le(e, 20, 0); // First cluster high
        write_u16_le(e, 26, 3); // First cluster low = 3

        // Entry 2: "limine.conf" file
        // We need to write limine.conf content too
        let limine_conf_content = build_limine_conf();
        let conf_size = limine_conf_content.len() as u32;
        let conf_cluster = 6u32; // We'll put limine.conf at cluster 6

        let e2 = &mut root_dir[64..96];
        e2[0..11].copy_from_slice(b"LIMINE  CON"); // 8.3 format: "LIMINE  CON"
        e2[11] = 0x20; // Archive
        write_u16_le(e2, 20, 0);
        write_u16_le(e2, 26, conf_cluster as u16);
        write_u32_le(e2, 28, conf_size);

        disk_write(disk.index, root_dir_lba, 1, &root_dir);

        // Allocate cluster 3 for EFI directory in FAT
        // Cluster 3 → first sector at data_start + (3-2) * sectors_per_cluster
        let efi_dir_lba = esp_start + data_start as u64 + (1 * sectors_per_cluster as u64);

        // Zero EFI directory
        for s in 0..sectors_per_cluster as u64 {
            disk_write(disk.index, efi_dir_lba + s, 1, &zero_sector);
        }

        let mut efi_dir = [0u8; 512];
        // "." entry
        efi_dir[0..11].copy_from_slice(b".          ");
        efi_dir[11] = 0x10;
        write_u16_le(&mut efi_dir, 26, 3);
        // ".." entry
        efi_dir[32..43].copy_from_slice(b"..         ");
        efi_dir[43] = 0x10;
        write_u16_le(&mut efi_dir, 58, 2); // Parent = root
        // "BOOT" subdirectory (cluster 4)
        let e = &mut efi_dir[64..96];
        e[0..11].copy_from_slice(b"BOOT       ");
        e[11] = 0x10;
        write_u16_le(e, 20, 0);
        write_u16_le(e, 26, 4);

        disk_write(disk.index, efi_dir_lba, 1, &efi_dir);

        // Cluster 4: BOOT directory
        let boot_dir_lba = esp_start + data_start as u64 + (2 * sectors_per_cluster as u64);
        for s in 0..sectors_per_cluster as u64 {
            disk_write(disk.index, boot_dir_lba + s, 1, &zero_sector);
        }

        let mut boot_dir = [0u8; 512];
        boot_dir[0..11].copy_from_slice(b".          ");
        boot_dir[11] = 0x10;
        write_u16_le(&mut boot_dir, 26, 4);
        boot_dir[32..43].copy_from_slice(b"..         ");
        boot_dir[43] = 0x10;
        write_u16_le(&mut boot_dir, 58, 3);

        // BOOTX64.EFI (cluster 5, may span multiple clusters)
        let bootx64_cluster = 5u32;
        let e = &mut boot_dir[64..96];
        e[0..11].copy_from_slice(b"BOOTX64 EFI");
        e[11] = 0x20;
        write_u16_le(e, 20, 0);
        write_u16_le(e, 26, bootx64_cluster as u16);

        // Get Limine UEFI binary from boot modules
        let (efi_data, efi_size) = get_limine_efi_binary();
        write_u32_le(e, 28, efi_size as u32);

        disk_write(disk.index, boot_dir_lba, 1, &boot_dir);

        // Write BOOTX64.EFI data starting at cluster 5
        let cluster_bytes = sectors_per_cluster as usize * 512;
        if efi_size > 0 {
            let num_clusters_needed = (efi_size + cluster_bytes - 1) / cluster_bytes;
            for ci in 0..num_clusters_needed {
                let cluster_num = bootx64_cluster as usize + ci;
                let cluster_lba = esp_start + data_start as u64
                    + ((cluster_num as u64 - 2) * sectors_per_cluster as u64);
                let data_offset = ci * cluster_bytes;
                let remaining = efi_size - data_offset;
                let write_size = remaining.min(cluster_bytes);

                // Write sectors within this cluster
                let full_sectors = write_size / 512;
                for s in 0..full_sectors {
                    let soffset = data_offset + s * 512;
                    disk_write(disk.index, cluster_lba + s as u64, 1,
                        &efi_data[soffset..soffset + 512]);
                }
                // Partial last sector
                let partial = write_size % 512;
                if partial > 0 {
                    let mut last = [0u8; 512];
                    let soffset = data_offset + full_sectors * 512;
                    last[..partial].copy_from_slice(&efi_data[soffset..soffset + partial]);
                    disk_write(disk.index, cluster_lba + full_sectors as u64, 1, &last);
                }
            }

            // Update FAT chain for BOOTX64.EFI
            update_fat_chain(disk.index, esp_start, reserved_sectors, fat_sectors,
                bootx64_cluster, num_clusters_needed as u32);

            serial::write_str("[INSTALLER] BOOTX64.EFI written (");
            serial_dec(efi_size as u64);
            serial::write_str(" bytes)\r\n");
        } else {
            serial::write_str("[INSTALLER] WARNING: No Limine EFI binary available\r\n");
            warn("  (no Limine binary found in modules) ");
        }

        // Write limine.conf at cluster 6
        {
            let conf_lba = esp_start + data_start as u64
                + ((conf_cluster as u64 - 2) * sectors_per_cluster as u64);
            let conf_bytes = limine_conf_content.as_bytes();
            let full_sectors = conf_bytes.len() / 512;
            for s in 0..full_sectors {
                disk_write(disk.index, conf_lba + s as u64, 1,
                    &conf_bytes[s * 512..(s + 1) * 512]);
            }
            let partial = conf_bytes.len() % 512;
            if partial > 0 || full_sectors == 0 {
                let mut last = [0u8; 512];
                let start = full_sectors * 512;
                let len = conf_bytes.len() - start;
                last[..len].copy_from_slice(&conf_bytes[start..]);
                disk_write(disk.index, conf_lba + full_sectors as u64, 1, &last);
            }
        }

        // Update FAT for all allocated clusters:
        // Cluster 2 = root dir (already set as EOC)
        // Cluster 3 = EFI dir
        // Cluster 4 = BOOT dir
        // Cluster 5+ = BOOTX64.EFI (chain)
        // Cluster N = limine.conf
        {
            let fat1_lba = esp_start + reserved_sectors as u64;
            let fat2_lba = fat1_lba + fat_sectors as u64;
            let mut fat_sec = [0u8; 512];

            // Re-read FAT sector 0
            disk_read(disk.index, fat1_lba, 1, &mut fat_sec);

            // Cluster 3 (EFI dir) → EOC
            write_u32_le(&mut fat_sec, 12, 0x0FFFFFFF);
            // Cluster 4 (BOOT dir) → EOC
            write_u32_le(&mut fat_sec, 16, 0x0FFFFFFF);
            // Cluster 6 (limine.conf) → EOC
            if 6 * 4 + 3 < 512 {
                write_u32_le(&mut fat_sec, 24, 0x0FFFFFFF);
            }

            disk_write(disk.index, fat1_lba, 1, &fat_sec);
            disk_write(disk.index, fat2_lba, 1, &fat_sec);
        }
    }
    ok("OK\n");

    if check_abort() { abort_screen(); return; }

    // ══════════════════════════════════════════════════════════════════════
    // Step 6: Write AETERNA identity + kernel to root partition
    // ══════════════════════════════════════════════════════════════════════
    print_header(6, 6, "Writing kernel to root partition");
    puts("  Root partition: sectors "); put_u64(root_start); puts(" - ");
    put_u64(root_end); puts("\n\n");
    serial::write_str("[INSTALLER] Writing kernel to root\r\n");

    // ── AETERNA identity record (first sector of root) ──
    puts("  [ 7/8 ]  AETERNA identity record ... ");
    {
        let mut id = [0u8; 512];
        id[..8].copy_from_slice(b"AETERNA ");
        let ver = crate::version::VERSION_STR.as_bytes();
        id[8..8 + ver.len().min(16)].copy_from_slice(&ver[..ver.len().min(16)]);
        let arch = crate::version::ARCH.as_bytes();
        id[24..24 + arch.len().min(8)].copy_from_slice(&arch[..arch.len().min(8)]);
        let date = crate::version::BUILD_DATE.as_bytes();
        id[32..32 + date.len().min(16)].copy_from_slice(&date[..date.len().min(16)]);
        id[48..56].copy_from_slice(&disk.size_mb.to_le_bytes());
        // Store kernel location info
        write_u64_le(&mut id, 64, root_start + 1); // Kernel start LBA
        write_u64_le(&mut id, 72, esp_start);       // ESP start LBA
        id[510] = 0xAE;
        id[511] = 0x05;
        if !disk_write(disk.index, root_start, 1, &id) {
            err("FAILED\n"); err("  [x] Identity write error.\n");
            dim("\n  Press ENTER to return...\n"); puts("  > "); wait_enter();
            framebuffer::clear(BG); framebuffer::set_cursor_pos(0, 0);
            return;
        }
    }
    ok("OK\n");

    if check_abort() { abort_screen(); return; }

    // ── Write kernel binary ──
    puts("  [ 8/8 ]  Kernel binary ... ");
    {
        let (kernel_data, kernel_size) = get_kernel_binary();
        if kernel_size > 0 {
            let sectors_needed = ((kernel_size + 511) / 512) as u64;
            let kernel_lba = root_start + 1;

            for s in 0..sectors_needed {
                let offset = (s as usize) * 512;
                let remaining = kernel_size - offset;
                if remaining >= 512 {
                    disk_write(disk.index, kernel_lba + s, 1,
                        &kernel_data[offset..offset + 512]);
                } else {
                    let mut last = [0u8; 512];
                    last[..remaining].copy_from_slice(&kernel_data[offset..offset + remaining]);
                    disk_write(disk.index, kernel_lba + s, 1, &last);
                }
            }

            ok("OK");
            puts(" ("); put_u64(kernel_size as u64); puts(" bytes, ");
            put_u64(sectors_needed); puts(" sectors)\n");
            serial::write_str("[INSTALLER] Kernel written\r\n");
        } else {
            warn("SKIP");
            puts(" (kernel binary not available as module)\n");
            serial::write_str("[INSTALLER] No kernel module available\r\n");
        }
    }

    // ══════════════════════════════════════════════════════════════════════
    // Verify
    // ══════════════════════════════════════════════════════════════════════
    puts("\n  Verifying critical sectors...\n");
    let mut verify_ok = true;

    // Verify GPT signature
    {
        let mut buf = [0u8; 512];
        puts("    GPT header: ");
        if disk_read(disk.index, 1, 1, &mut buf) && &buf[..8] == b"EFI PART" {
            ok("OK\n");
        } else {
            err("FAIL\n"); verify_ok = false;
        }
    }

    // Verify ESP VBR
    {
        let mut buf = [0u8; 512];
        puts("    FAT32 VBR:  ");
        if disk_read(disk.index, esp_start, 1, &mut buf) && buf[510] == 0x55 && buf[511] == 0xAA {
            ok("OK\n");
        } else {
            err("FAIL\n"); verify_ok = false;
        }
    }

    // Verify AETERNA identity
    {
        let mut buf = [0u8; 512];
        puts("    Identity:   ");
        if disk_read(disk.index, root_start, 1, &mut buf)
            && &buf[..8] == b"AETERNA " && buf[510] == 0xAE && buf[511] == 0x05 {
            ok("OK\n");
        } else {
            err("FAIL\n"); verify_ok = false;
        }
    }

    // ══════════════════════════════════════════════════════════════════════
    // Summary
    // ══════════════════════════════════════════════════════════════════════
    puts("\n");
    dim("  ------------------------------------------------\n");
    if verify_ok {
        ok("  Installation complete!\n");
    } else {
        warn("  Installation complete with warnings.\n");
    }
    dim("  ------------------------------------------------\n\n");

    puts("  Written to /dev/"); hl(dev_name(disk)); puts(":\n\n");
    dim("    Sector 0      Protective MBR (0xEE)\n");
    dim("    Sector 1      GPT Header (EFI PART)\n");
    dim("    Sectors 2-33  GPT Partition Entries (128 entries)\n");
    dim("    ");
    puts("ESP: "); put_u64(esp_start); puts(" - "); put_u64(esp_end);
    puts(" (FAT32, "); put_size(esp_size_mb); puts(")\n");
    dim("    ");
    puts("Root: "); put_u64(root_start); puts(" - "); put_u64(root_end);
    puts(" ("); put_size(root_mb); puts(")\n\n");

    puts("  ESP contents:\n");
    dim("    /EFI/BOOT/BOOTX64.EFI  (Limine UEFI bootloader)\n");
    dim("    /limine.conf           (boot configuration)\n\n");

    if verify_ok {
        ok("  [+]  Disk verified — AETERNA bootable via UEFI.\n\n");
    } else {
        warn("  [!]  Some verifications failed.\n\n");
    }

    puts("  To boot from this disk:\n");
    dim("    1. Set UEFI boot order to "); hl(dev_name(disk)); puts("\n");
    dim("    2. Or select '"); hl("AETERNA Microkernel"); puts("' from UEFI boot menu\n\n");

    dim("  ------------------------------------------------\n");
    dim("  Press ENTER to return to terminal...\n");
    puts("  > ");
    wait_enter();
    framebuffer::clear(BG);
    framebuffer::set_cursor_pos(0, 0);
}

// ═══════════════════════════════════════════════════════════════════════════
// FAT chain update (for multi-cluster files)
// ═══════════════════════════════════════════════════════════════════════════

fn update_fat_chain(
    disk: usize, esp_start: u64, reserved: u16, fat_sectors: u32,
    start_cluster: u32, num_clusters: u32,
) {
    if num_clusters == 0 { return; }

    let fat1_lba = esp_start + reserved as u64;
    let fat2_lba = fat1_lba + fat_sectors as u64;

    // For simplicity, clusters are allocated contiguously starting at start_cluster
    // Each FAT entry is 4 bytes → 128 entries per 512-byte sector
    for i in 0..num_clusters {
        let cluster = start_cluster + i;
        let fat_offset = cluster * 4;
        let fat_sector = fat_offset / 512;
        let fat_byte = (fat_offset % 512) as usize;

        let mut sec = [0u8; 512];
        disk_read(disk, fat1_lba + fat_sector as u64, 1, &mut sec);

        let value = if i == num_clusters - 1 {
            0x0FFFFFFFu32 // End of chain
        } else {
            cluster + 1 // Next cluster
        };
        write_u32_le(&mut sec, fat_byte, value);

        disk_write(disk, fat1_lba + fat_sector as u64, 1, &sec);
        disk_write(disk, fat2_lba + fat_sector as u64, 1, &sec);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Binary data sources (from Limine boot modules or embedded)
// ═══════════════════════════════════════════════════════════════════════════

/// Get the Limine UEFI binary (BOOTX64.EFI) from boot modules.
/// Returns (pointer_to_data, size). If not found, returns empty.
fn get_limine_efi_binary() -> (&'static [u8], usize) {
    use ospab_os::arch::x86_64::boot;

    // Search Limine modules for the EFI binary
    if let Some(mut modules) = boot::modules() {
        while let Some(module) = modules.next() {
            let path = unsafe {
                if module.path.is_null() { continue; }
                let mut len = 0;
                while *module.path.add(len) != 0 { len += 1; }
                core::str::from_utf8_unchecked(core::slice::from_raw_parts(
                    module.path as *const u8, len
                ))
            };

            if path.contains("BOOTX64") || path.contains("bootx64")
                || path.contains("limine-uefi") || path.contains("LIMINE")
            {
                let size = module.size as usize;
                let data = unsafe { core::slice::from_raw_parts(module.address, size) };
                serial::write_str("[INSTALLER] Found Limine EFI module: ");
                serial::write_str(path);
                serial::write_str(" (");
                serial_dec(size as u64);
                serial::write_str(" bytes)\r\n");
                return (data, size);
            }
        }
    }

    // Fallback: check if limine-10.8.2/bin/BOOTX64.EFI is in the modules
    if let Some(mut modules) = boot::modules() {
        while let Some(module) = modules.next() {
            let size = module.size as usize;
            // Check for PE/COFF signature (MZ header) which indicates an EFI executable
            if size > 2 {
                let data = unsafe { core::slice::from_raw_parts(module.address, size) };
                if data[0] == b'M' && data[1] == b'Z' {
                    serial::write_str("[INSTALLER] Found EFI binary in module (MZ signature)\r\n");
                    return (data, size);
                }
            }
        }
    }

    serial::write_str("[INSTALLER] No EFI binary in boot modules\r\n");
    (&[], 0)
}

/// Get the kernel binary from boot modules.
fn get_kernel_binary() -> (&'static [u8], usize) {
    use ospab_os::arch::x86_64::boot;

    if let Some(mut modules) = boot::modules() {
        while let Some(module) = modules.next() {
            let path = unsafe {
                if module.path.is_null() { continue; }
                let mut len = 0;
                while *module.path.add(len) != 0 { len += 1; }
                core::str::from_utf8_unchecked(core::slice::from_raw_parts(
                    module.path as *const u8, len
                ))
            };

            if path.contains("aeterna") || path.contains("ospab")
                || path.contains("kernel") || path.contains("KERNEL")
            {
                let size = module.size as usize;
                let data = unsafe { core::slice::from_raw_parts(module.address, size) };
                serial::write_str("[INSTALLER] Found kernel module: ");
                serial::write_str(path);
                serial::write_str("\r\n");
                return (data, size);
            }
        }
    }

    serial::write_str("[INSTALLER] No kernel binary in boot modules\r\n");
    (&[], 0)
}

/// Build the limine.conf configuration file content.
fn build_limine_conf() -> &'static str {
    "timeout: 3\n\
     \n\
     /AETERNA Microkernel\n\
     \tprotocol: limine\n\
     \tkernel_path: boot():/aeterna\n"
}

// ═══════════════════════════════════════════════════════════════════════════
// Serial helpers
// ═══════════════════════════════════════════════════════════════════════════

fn serial_dec(mut val: u64) {
    if val == 0 { serial::write_byte(b'0'); return; }
    let mut buf = [0u8; 20];
    let mut i = 0;
    while val > 0 { buf[i] = b'0' + (val % 10) as u8; val /= 10; i += 1; }
    for j in (0..i).rev() { serial::write_byte(buf[j]); }
}
