/*
 * ATA PIO disk driver for AETERNA
 *
 * Supports:
 *   Primary IDE channel:   I/O 0x1F0–0x1F7, control 0x3F6
 *   Secondary IDE channel: I/O 0x170–0x177, control 0x376
 *
 * Works with:
 *   QEMU:         -drive file=disk.img,format=raw,if=ide  (or PIIX IDE default)
 *   VirtualBox:   IDE controller
 *   Bare metal:   Legacy IDE / SATA in IDE compatibility mode
 *   VMware:       IDE controller mode
 *
 * Implements:
 *   - IDENTIFY (detect drives, get model/size)
 *   - READ SECTORS (28-bit LBA PIO)
 *   - WRITE SECTORS (28-bit LBA PIO)
 *   - Max 2^28 = 128 GiB addressable; covers all typical VM disk sizes
 */#![allow(dead_code)]
use core::arch::asm;

// ─── IDE I/O Ports ─────────────────────────────────────────────────────────
// Primary channel
const ATA_PRIMARY_BASE:  u16 = 0x1F0;
const ATA_PRIMARY_CTRL:  u16 = 0x3F6;
// Secondary channel
const ATA_SECOND_BASE:   u16 = 0x170;
const ATA_SECOND_CTRL:   u16 = 0x376;

// Register offsets from base
const ATA_REG_DATA:      u16 = 0;   // Data port (16-bit)
const ATA_REG_ERROR:     u16 = 1;   // Error / Features
const ATA_REG_SECCOUNT:  u16 = 2;   // Sector count
const ATA_REG_LBA_LO:    u16 = 3;   // LBA bits 0-7
const ATA_REG_LBA_MID:   u16 = 4;   // LBA bits 8-15
const ATA_REG_LBA_HI:    u16 = 5;   // LBA bits 16-23
const ATA_REG_HEAD:      u16 = 6;   // Drive/Head: [1|1|LBA|DRV|LBA bits 24-27]
const ATA_REG_STATUS:    u16 = 7;   // Status (read) / Command (write)
const ATA_REG_CMD:       u16 = 7;

// Status register bits
const ATA_SR_ERR:  u8 = 1 << 0; // Error
const ATA_SR_DRQ:  u8 = 1 << 3; // Data Request Ready
const ATA_SR_SRV:  u8 = 1 << 4; // Overlapped Mode Service Request
const ATA_SR_DF:   u8 = 1 << 5; // Drive Fault Error
const ATA_SR_RDY:  u8 = 1 << 6; // Drive ready
const ATA_SR_BSY:  u8 = 1 << 7; // Drive busy

// Commands
const ATA_CMD_IDENTIFY:       u8 = 0xEC;
const ATA_CMD_READ_SECTORS:   u8 = 0x20;
const ATA_CMD_WRITE_SECTORS:  u8 = 0x30;
const ATA_CMD_CACHE_FLUSH:    u8 = 0xE7;

const SECTOR_SIZE: usize = 512;
const MAX_DRIVES: usize = 4; // Primary master/slave, secondary master/slave

// ─── Drive info ────────────────────────────────────────────────────────────
#[derive(Copy, Clone)]
pub struct DriveInfo {
    pub present:    bool,
    pub channel:    u8,       // 0 = primary, 1 = secondary
    pub drive:      u8,       // 0 = master, 1 = slave
    pub model:      [u8; 41], // Model string (null-terminated)
    pub sectors:    u32,      // Total LBA sectors (28-bit)
    pub size_mb:    u32,      // Size in MiB
}

static mut DRIVES: [DriveInfo; MAX_DRIVES] = [DriveInfo {
    present: false,
    channel: 0,
    drive: 0,
    model: [0u8; 41],
    sectors: 0,
    size_mb: 0,
}; MAX_DRIVES];

static mut DRIVE_COUNT: usize = 0;
static mut INITIALIZED: bool = false;

// ─── I/O helpers ──────────────────────────────────────────────────────────
fn inb(port: u16) -> u8 {
    let v: u8;
    unsafe { asm!("in al, dx", in("dx") port, out("al") v, options(nomem, nostack)); }
    v
}
fn inw(port: u16) -> u16 {
    let v: u16;
    unsafe { asm!("in ax, dx", in("dx") port, out("ax") v, options(nomem, nostack)); }
    v
}
fn outb(port: u16, v: u8) {
    unsafe { asm!("out dx, al", in("dx") port, in("al") v, options(nomem, nostack)); }
}

fn delay400ns(ctrl: u16) {
    // Read alt-status 4 times = ~400ns delay
    inb(ctrl); inb(ctrl); inb(ctrl); inb(ctrl);
}

// ─── Channel base address ──────────────────────────────────────────────────
fn channel_base(channel: u8) -> u16 {
    if channel == 0 { ATA_PRIMARY_BASE } else { ATA_SECOND_BASE }
}
fn channel_ctrl(channel: u8) -> u16 {
    if channel == 0 { ATA_PRIMARY_CTRL } else { ATA_SECOND_CTRL }
}

// ─── Wait for BSY to clear (poll alternate status to avoid clearing IRQ) ────
fn wait_not_busy(_base: u16, ctrl: u16) -> bool {
    delay400ns(ctrl);
    let mut timeout = 2_000_000u32;
    loop {
        // Poll alternate status (ctrl port) — no side effects on interrupt status
        let s = inb(ctrl);
        if s & ATA_SR_BSY == 0 { return true; }
        timeout -= 1;
        if timeout == 0 { return false; }
        unsafe { core::arch::asm!("pause"); }
    }
}

/// Wait for BSY=0 AND DRQ=1 (drive ready to transfer data)
fn wait_drq(_base: u16, ctrl: u16) -> bool {
    let mut timeout = 2_000_000u32;
    loop {
        // Poll alternate status — no side effects
        let s = inb(ctrl);
        if s & ATA_SR_ERR != 0 { return false; }
        if s & ATA_SR_DF  != 0 { return false; }
        if s & ATA_SR_BSY == 0 && s & ATA_SR_DRQ != 0 { return true; }
        timeout -= 1;
        if timeout == 0 { return false; }
        unsafe { core::arch::asm!("pause"); }
    }
}

// ─── Detect and initialize ────────────────────────────────────────────────
pub fn init() -> usize {
    unsafe {
        DRIVE_COUNT = 0;
        // Probe all 4 drive positions
        for channel in 0u8..2 {
            let base = channel_base(channel);
            let ctrl = channel_ctrl(channel);

            for drive in 0u8..2 {
                if probe_drive(channel, drive, base, ctrl) {
                    DRIVE_COUNT += 1;
                }
            }
        }
        INITIALIZED = true;
        DRIVE_COUNT
    }
}

fn probe_drive(channel: u8, drive: u8, base: u16, ctrl: u16) -> bool {
    // Select drive
    let head = 0xA0 | (drive << 4);
    outb(base + ATA_REG_HEAD, head);
    delay400ns(ctrl);

    // Write known values to sector count and LBA registers
    outb(base + ATA_REG_SECCOUNT, 0xAB);
    outb(base + ATA_REG_LBA_LO,   0xCD);

    // Read back — if matches, controller exists
    let sc = inb(base + ATA_REG_SECCOUNT);
    let lo = inb(base + ATA_REG_LBA_LO);

    if sc != 0xAB || lo != 0xCD {
        return false; // No drive / floating bus
    }

    // Send IDENTIFY command
    outb(base + ATA_REG_SECCOUNT, 0);
    outb(base + ATA_REG_LBA_LO,   0);
    outb(base + ATA_REG_LBA_MID,  0);
    outb(base + ATA_REG_LBA_HI,   0);
    outb(base + ATA_REG_CMD,       ATA_CMD_IDENTIFY);

    // Status 0 = no drive
    let status = inb(base + ATA_REG_STATUS);
    if status == 0 { return false; }

    // Wait for BSY to clear
    if !wait_not_busy(base, ctrl) { return false; }

    // Check if ATAPI (has non-zero LBA_MID/HI after IDENTIFY)
    let mid = inb(base + ATA_REG_LBA_MID);
    let hi  = inb(base + ATA_REG_LBA_HI);
    if mid != 0 || hi != 0 {
        // ATAPI device — skip
        return false;
    }

    // Wait for DRQ
    if !wait_drq(base, ctrl) { return false; }

    // Read 256 words of identify data
    let mut id_buf = [0u16; 256];
    for w in &mut id_buf {
        *w = inw(base + ATA_REG_DATA);
    }

    // Parse model string: words 27-46 = 40 bytes (pairs swapped)
    let slot = unsafe { DRIVE_COUNT };
    unsafe {
        DRIVES[slot].present = true;
        DRIVES[slot].channel = channel;
        DRIVES[slot].drive   = drive;

        // Model name: words 27-46, each word is 2 bytes, bytes swapped
        let mut model_idx = 0usize;
        for i in 27..47 {
            let word = id_buf[i];
            let b0 = (word >> 8) as u8;
            let b1 = (word & 0xFF) as u8;
            if b0 != 0 && model_idx < 40 { DRIVES[slot].model[model_idx] = b0; model_idx += 1; }
            if b1 != 0 && model_idx < 40 { DRIVES[slot].model[model_idx] = b1; model_idx += 1; }
        }
        // Trim trailing spaces
        while model_idx > 0 && DRIVES[slot].model[model_idx - 1] == b' ' {
            DRIVES[slot].model[model_idx - 1] = 0;
            model_idx -= 1;
        }
        DRIVES[slot].model[model_idx] = 0; // null term

        // LBA28 sector count: words 60-61
        let sects = (id_buf[60] as u32) | ((id_buf[61] as u32) << 16);
        DRIVES[slot].sectors  = sects;
        DRIVES[slot].size_mb  = sects / 2048; // 512-byte sectors → MiB
    }

    let d = unsafe { &DRIVES[slot] };
    crate::arch::x86_64::serial::write_str("[ATA] Drive ");
    crate::arch::x86_64::serial::write_byte(b'0' + channel);
    crate::arch::x86_64::serial::write_byte(b'/');
    crate::arch::x86_64::serial::write_byte(b'0' + drive);
    crate::arch::x86_64::serial::write_str(": ");
    let model_str = unsafe {
        let end = d.model.iter().position(|&b| b == 0).unwrap_or(40);
        core::str::from_utf8_unchecked(&d.model[..end])
    };
    crate::arch::x86_64::serial::write_str(model_str);
    crate::arch::x86_64::serial::write_str(", ");
    serial_dec(d.size_mb as u64);
    crate::arch::x86_64::serial::write_str(" MiB\r\n");

    true
}

pub fn is_initialized() -> bool { unsafe { INITIALIZED } }
pub fn drive_count() -> usize  { unsafe { DRIVE_COUNT } }

pub fn drive_info(idx: usize) -> Option<&'static DriveInfo> {
    unsafe {
        if idx < DRIVE_COUNT && DRIVES[idx].present {
            Some(&DRIVES[idx])
        } else {
            None
        }
    }
}

// ─── Select drive + set up LBA address ────────────────────────────────────
fn select_drive_lba28(_channel: u8, drive: u8, base: u16, ctrl: u16,
                       lba: u32, count: u8) {
    let head = 0xE0 | ((drive & 1) << 4) | ((lba >> 24) as u8 & 0x0F);
    outb(base + ATA_REG_HEAD,    head);
    delay400ns(ctrl);
    outb(base + ATA_REG_SECCOUNT, count);
    outb(base + ATA_REG_LBA_LO,  (lba & 0xFF) as u8);
    outb(base + ATA_REG_LBA_MID, ((lba >> 8) & 0xFF) as u8);
    outb(base + ATA_REG_LBA_HI,  ((lba >> 16) & 0xFF) as u8);
}

/// Read `count` sectors (max 255) from drive `drive_idx` starting at LBA `lba`.
/// Returns true on success. `buf` must be at least count * 512 bytes.
pub fn read_sectors(drive_idx: usize, lba: u32, count: u8, buf: &mut [u8]) -> bool {
    if count == 0 || buf.len() < (count as usize) * SECTOR_SIZE { return false; }

    let d = match drive_info(drive_idx) {
        Some(d) => *d,
        None => return false,
    };

    let base = channel_base(d.channel);
    let ctrl = channel_ctrl(d.channel);

    // Suppress IDE IRQs during PIO transfer (nIEN = bit 1)
    outb(ctrl, 0x02);

    if !wait_not_busy(base, ctrl) {
        crate::arch::x86_64::serial::write_str("[ATA] read: BSY timeout before select\r\n");
        outb(ctrl, 0x00);
        return false;
    }

    select_drive_lba28(d.channel, d.drive, base, ctrl, lba, count);
    outb(base + ATA_REG_CMD, ATA_CMD_READ_SECTORS);
    delay400ns(ctrl);  // give drive time to assert BSY

    for sector in 0..(count as usize) {
        if !wait_drq(base, ctrl) {
            crate::arch::x86_64::serial::write_str("[ATA] read: DRQ timeout\r\n");
            outb(ctrl, 0x00);
            return false;
        }

        let offset = sector * SECTOR_SIZE;
        for w in 0..(SECTOR_SIZE / 2) {
            let word = inw(base + ATA_REG_DATA);
            buf[offset + w * 2]     = (word & 0xFF) as u8;
            buf[offset + w * 2 + 1] = ((word >> 8) & 0xFF) as u8;
        }
    }

    // Clear nIEN, read status to acknowledge any pending interrupt
    outb(ctrl, 0x00);
    let _ = inb(base + ATA_REG_STATUS);
    true
}

/// Write `count` sectors (max 255) to drive `drive_idx` starting at LBA `lba`.
pub fn write_sectors(drive_idx: usize, lba: u32, count: u8, data: &[u8]) -> bool {
    if count == 0 || data.len() < (count as usize) * SECTOR_SIZE { return false; }

    let d = match drive_info(drive_idx) {
        Some(d) => *d,
        None => return false,
    };

    let base = channel_base(d.channel);
    let ctrl = channel_ctrl(d.channel);

    // Suppress IDE IRQs during PIO transfer (nIEN = bit 1 of device control)
    outb(ctrl, 0x02);

    if !wait_not_busy(base, ctrl) {
        crate::arch::x86_64::serial::write_str("[ATA] write: BSY timeout before select\r\n");
        outb(ctrl, 0x00);
        return false;
    }

    select_drive_lba28(d.channel, d.drive, base, ctrl, lba, count);
    outb(base + ATA_REG_CMD, ATA_CMD_WRITE_SECTORS);
    delay400ns(ctrl);  // give drive time to assert BSY after command

    for sector in 0..(count as usize) {
        // Wait for BSY=0 && DRQ=1 (drive ready for data)
        if !wait_drq(base, ctrl) {
            crate::arch::x86_64::serial::write_str("[ATA] write: DRQ timeout sector ");
            serial_dec(sector as u64);
            crate::arch::x86_64::serial::write_str("\r\n");
            outb(ctrl, 0x00);
            return false;
        }

        let offset = sector * SECTOR_SIZE;
        for w in 0..(SECTOR_SIZE / 2) {
            let lo = data[offset + w * 2] as u16;
            let hi = data[offset + w * 2 + 1] as u16;
            let word = lo | (hi << 8);
            unsafe {
                asm!("out dx, ax",
                    in("dx") base + ATA_REG_DATA,
                    in("ax") word,
                    options(nomem, nostack)
                );
            }
        }
        // Wait for drive to finish processing this sector
        if !wait_not_busy(base, ctrl) {
            crate::arch::x86_64::serial::write_str("[ATA] write: BSY timeout post-sector\r\n");
            outb(ctrl, 0x00);
            return false;
        }
    }

    // Flush write cache
    outb(base + ATA_REG_CMD, ATA_CMD_CACHE_FLUSH);
    if !wait_not_busy(base, ctrl) {
        crate::arch::x86_64::serial::write_str("[ATA] write: cache flush timeout\r\n");
    }

    // Clear nIEN, read status to acknowledge any pending interrupt
    outb(ctrl, 0x00);
    let _ = inb(base + ATA_REG_STATUS);

    true
}

fn serial_dec(mut v: u64) {
    if v == 0 { crate::arch::x86_64::serial::write_byte(b'0'); return; }
    let mut buf = [0u8; 20]; let mut i = 0;
    while v > 0 { buf[i] = b'0' + (v % 10) as u8; v /= 10; i += 1; }
    for j in (0..i).rev() { crate::arch::x86_64::serial::write_byte(buf[j]); }
}
