/*
 * ospab.os Installer — AETERNA microkernel
 *
 * Honest about what it does:
 *   - Detects real disks via ATA/AHCI drivers
 *   - Lets the user pick a target
 *   - Writes MBR signature, AETERNA identity record, GPT stub to real sectors
 *   - No fake formatting — we write raw sectors only
 *   - Full FS support (FAT32/ext4) is planned for v3.0
 */
use core::sync::atomic::{AtomicBool, Ordering};
use ospab_os::arch::x86_64::framebuffer;
use ospab_os::arch::x86_64::keyboard;
use ospab_os::arch::x86_64::serial;

const FG: u32      = 0x00FFFFFF;
const FG_DIM: u32  = 0x00AAAAAA;
const FG_OK: u32   = 0x0000FF00;
const FG_WARN: u32 = 0x0000CCFF;  // yellow
const FG_ERR: u32  = 0x000000FF;  // red
const FG_HL: u32   = 0x00FFCC00;  // gold
const FG_STEP: u32 = 0x00FF8800;  // orange for step headers
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
    // Top border
    hl("  +==============================================+\n");
    hl("  |"); puts("   ospab.os  AETERNA  Installer              ");
    hl("|\n");
    hl("  +==============================================+\n");
    puts("  Version: "); dim(crate::version::OS_VERSION);
    puts("   Arch: "); dim(crate::version::ARCH); puts("\n");
    dim("  ------------------------------------------------\n");
    // Step indicator
    step("  [ Step "); put_u64(step_num as u64); step("/"); put_u64(total as u64); step(" ]  ");
    puts(title); puts("\n");
    dim("  ------------------------------------------------\n\n");
}

/// Wait for Enter or Ctrl+C (CPU-friendly: uses hlt between polls).
/// Returns false if Ctrl+C pressed.
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

// ─────────────────────────────────────────────────────────────────────────
// Main installer entry point
// ─────────────────────────────────────────────────────────────────────────

pub fn run() {
    ABORT.store(false, Ordering::Relaxed);

    // ══════════════════════════════════════════════════════════════════════
    // Step 1: Storage detection
    // ══════════════════════════════════════════════════════════════════════
    print_header(1, 4, "Storage detection");

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
    print_header(2, 4, "Select installation target");

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

    // ══════════════════════════════════════════════════════════════════════
    // Step 3: Partition plan
    // ══════════════════════════════════════════════════════════════════════
    print_header(3, 4, "Partition plan");

    puts("  Target disk:  /dev/"); hl(dev_name(disk));
    puts("   ("); put_size(disk.size_mb); puts(")\n\n");

    let esp_mb: u64  = 512;
    let root_mb: u64 = disk.size_mb.saturating_sub(esp_mb);
    let esp_start:   u64 = 2048;
    let esp_end:     u64 = esp_start + (esp_mb * 2048) - 1;
    let root_start:  u64 = esp_end + 1;
    let root_end:    u64 = disk.sectors.saturating_sub(34);

    dim("  Device         Start       End         Size       Type\n");
    dim("  ----------------------------------------------------------\n");
    puts("  /dev/"); puts(dev_name(disk)); hl("1");
    puts("  "); put_u64(esp_start); puts("  "); put_u64(esp_end);
    puts("  "); put_size(esp_mb); puts("  EFI System\n");
    puts("  /dev/"); puts(dev_name(disk)); hl("2");
    puts("  "); put_u64(root_start); puts("  "); put_u64(root_end);
    puts("  "); put_size(root_mb); puts("  AETERNA root\n\n");

    warn("  Note:  AETERNA v2 writes raw sectors only.\n");
    dim("         FAT32/ext4 filesystem write is planned for v3.0.\n");
    dim("         This step writes: MBR boot record, identity sector,\n");
    dim("         and a GPT header stub to sectors 0-2.\n");

    pause_next_step("Write to disk?");
    if !wait_enter() || ABORT.load(Ordering::Relaxed) {
        abort_screen(); return;
    }

    // ══════════════════════════════════════════════════════════════════════
    // Step 4: Write
    // ══════════════════════════════════════════════════════════════════════
    print_header(4, 4, "Writing to disk");

    puts("  Target: /dev/"); hl(dev_name(disk));
    puts("  ("); put_size(disk.size_mb); puts(")\n\n");

    serial::write_str("[INSTALLER] Step 4: Writing to disk\r\n");

    // ── Sector 0: MBR ──────────────────────────────────────────────────
    puts("  [ 1/4 ]  MBR boot record  (sector 0) ... ");
    serial::write_str("[INSTALLER] Writing MBR (sector 0)...\r\n");
    {
        let mut mbr = [0u8; 512];
        mbr[446] = 0x80;  // partition 1 bootable
        mbr[510] = 0x55;
        mbr[511] = 0xAA;
        if !ospab_os::drivers::write(disk.index, 0, 1, &mbr) {
            serial::write_str("[INSTALLER] MBR write FAILED\r\n");
            err("FAILED\n\n");
            err("  [x]  MBR write error. Check disk connection.\n");
            dim("\n  Press ENTER to return...\n"); puts("  > "); wait_enter();
            framebuffer::clear(BG); framebuffer::set_cursor_pos(0, 0);
            return;
        }
    }
    serial::write_str("[INSTALLER] MBR OK\r\n");
    ok("OK\n");

    if check_abort() { abort_screen(); return; }

    // ── Sector 1: AETERNA identity ──────────────────────────────────────
    puts("  [ 2/4 ]  AETERNA identity record  (sector 1) ... ");
    serial::write_str("[INSTALLER] Writing identity (sector 1)...\r\n");
    {
        let mut id = [0u8; 512];
        id[..8].copy_from_slice(b"AETERNA ");
        let ver = crate::version::VERSION_STR.as_bytes();
        id[8..8+ver.len().min(16)].copy_from_slice(&ver[..ver.len().min(16)]);
        let arch = crate::version::ARCH.as_bytes();
        id[24..24+arch.len().min(8)].copy_from_slice(&arch[..arch.len().min(8)]);
        let date = crate::version::BUILD_DATE.as_bytes();
        id[32..32+date.len().min(16)].copy_from_slice(&date[..date.len().min(16)]);
        id[48..56].copy_from_slice(&disk.size_mb.to_le_bytes());
        id[510] = 0xAE;
        id[511] = 0x05;
        if !ospab_os::drivers::write(disk.index, 1, 1, &id) {
            serial::write_str("[INSTALLER] Identity write FAILED\r\n");
            err("FAILED\n\n");
            err("  [x]  Identity record write error.\n");
            dim("\n  Press ENTER to return...\n"); puts("  > "); wait_enter();
            framebuffer::clear(BG); framebuffer::set_cursor_pos(0, 0);
            return;
        }
    }
    serial::write_str("[INSTALLER] Identity OK\r\n");
    ok("OK\n");

    if check_abort() { abort_screen(); return; }

    // ── Sector 2: GPT header stub ───────────────────────────────────────
    puts("  [ 3/4 ]  GPT header stub  (sector 2) ... ");
    serial::write_str("[INSTALLER] Writing GPT stub (sector 2)...\r\n");
    {
        let mut gpt = [0u8; 512];
        gpt[..8].copy_from_slice(b"EFI PART");
        gpt[8..12].copy_from_slice(&[0x00, 0x00, 0x01, 0x00]);  // rev 1.0
        gpt[12..16].copy_from_slice(&92u32.to_le_bytes());       // header size
        gpt[24..32].copy_from_slice(&2u64.to_le_bytes());        // my LBA
        gpt[32..40].copy_from_slice(&disk.sectors.saturating_sub(1).to_le_bytes());
        gpt[40..48].copy_from_slice(&esp_start.to_le_bytes());
        gpt[48..56].copy_from_slice(&root_end.to_le_bytes());
        if !ospab_os::drivers::write(disk.index, 2, 1, &gpt) {
            serial::write_str("[INSTALLER] GPT stub write FAILED\r\n");
            err("FAILED\n\n");
            err("  [x]  GPT stub write error.\n");
            dim("\n  Press ENTER to return...\n"); puts("  > "); wait_enter();
            framebuffer::clear(BG); framebuffer::set_cursor_pos(0, 0);
            return;
        }
    }
    serial::write_str("[INSTALLER] GPT stub OK\r\n");
    ok("OK\n");

    // ── Verify sector 1 ─────────────────────────────────────────────────
    puts("  [ 4/4 ]  Verify readback  (sector 1) ... ");
    serial::write_str("[INSTALLER] Verifying sector 1...\r\n");
    let verify_ok;
    {
        let mut v = [0u8; 512];
        verify_ok = ospab_os::drivers::read(disk.index, 1, 1, &mut v)
            && &v[..8] == b"AETERNA ";
    }
    if verify_ok {
        serial::write_str("[INSTALLER] Verify OK\r\n");
        ok("OK\n");
    } else {
        serial::write_str("[INSTALLER] Verify FAILED\r\n");
        warn("WARN (read mismatch)\n");
    }

    serial::write_str("[INSTALLER] Installation complete\r\n");

    // ══════════════════════════════════════════════════════════════════════
    // Summary
    // ══════════════════════════════════════════════════════════════════════
    puts("\n");
    dim("  ------------------------------------------------\n");
    ok("  Installation complete!"); puts("\n");
    dim("  ------------------------------------------------\n\n");

    puts("  Written to /dev/"); hl(dev_name(disk)); puts(":\n\n");
    dim("    Sector 0  MBR boot record  (0x55AA)\n");
    dim("    Sector 1  AETERNA identity  (magic AETERNA  + 0xAE05)\n");
    dim("    Sector 2  GPT header stub  (EFI PART)\n\n");

    if verify_ok {
        ok("  [+]  Disk verified — AETERNA magic present.\n\n");
    } else {
        warn("  [!]  Could not verify disk readback. Check hardware.\n\n");
    }

    warn("  To make this disk fully bootable, write the ISO:\n\n");
    puts("    dd if=ospab-os-v2-N.iso"); puts(" of=/dev/"); puts(dev_name(disk));
    puts(" bs=4M\n\n");

    dim("  Full filesystem support (FAT32/ext4) planned for v3.0.\n\n");
    dim("  ------------------------------------------------\n");
    dim("  Press ENTER to return to terminal...\n");
    puts("  > ");
    wait_enter();
    framebuffer::clear(BG);
    framebuffer::set_cursor_pos(0, 0);
}
