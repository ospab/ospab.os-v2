/*
 * disk_tools — GPT-aware fdisk, NVMe lsblk extension, and mkfs.aeterna
 *
 * BSL 1.1 — Copyright (c) 2026 ospab
 *
 * run_fdisk(args)   — list all disks (AHCI + NVMe) with GPT partition tables
 * run_lsblk(args)   — list all block devices including NVMe
 * run_mkfs(args)    — format a partition with AeternaFS
 *
 * All output goes to the framebuffer via framebuffer::draw_string().
 */

#![allow(dead_code)]

extern crate alloc;

use crate::arch::x86_64::framebuffer;

const FG: u32     = 0x00FFFFFF;
const FG_OK: u32  = 0x0000FF00;
const FG_ERR: u32 = 0x00FF4444;
const FG_DIM: u32 = 0x00AAAAAA;
const FG_HL: u32  = 0x00FFCC00;
const FG_DIR: u32 = 0x005555FF;
const BG: u32     = 0x00000000;

fn puts(s: &str)  { framebuffer::draw_string(s, FG, BG); }
fn ok(s: &str)    { framebuffer::draw_string(s, FG_OK, BG); }
fn err(s: &str)   { framebuffer::draw_string(s, FG_ERR, BG); }
fn dim(s: &str)   { framebuffer::draw_string(s, FG_DIM, BG); }
fn hl(s: &str)    { framebuffer::draw_string(s, FG_HL, BG); }

fn put_u64(mut n: u64) {
    if n == 0 { puts("0"); return; }
    let mut buf = [0u8; 20]; let mut i = 0;
    while n > 0 { buf[i] = b'0' + (n % 10) as u8; n /= 10; i += 1; }
    for j in (0..i).rev() { framebuffer::draw_char(buf[j] as char, FG, BG); }
}

fn put_u64_col(mut n: u64, col: u32) {
    if n == 0 { framebuffer::draw_char('0', col, BG); return; }
    let mut buf = [0u8; 20]; let mut i = 0;
    while n > 0 { buf[i] = b'0' + (n % 10) as u8; n /= 10; i += 1; }
    for j in (0..i).rev() { framebuffer::draw_char(buf[j] as char, col, BG); }
}

fn put_size_mib(mib: u64) {
    if mib >= 1024 {
        let gib = mib / 1024;
        let frac = (mib % 1024) * 10 / 1024;
        put_u64(gib); puts("."); put_u64(frac); puts(" GiB");
    } else {
        put_u64(mib); puts(" MiB");
    }
}

// ─── fdisk ────────────────────────────────────────────────────────────────

/// `fdisk [-l]` — list all disks and GPT partition tables.
pub fn run_fdisk(args: &str) {
    if !args.is_empty() && args != "-l" {
        err("fdisk: unknown option\n");
        dim("Usage: fdisk -l\n");
        return;
    }

    let mut found_any = false;

    // ── NVMe disk ────────────────────────────────────────────────────────
    if crate::drivers::nvme::is_initialized() {
        found_any = true;
        let sectors = crate::drivers::nvme::sector_count();
        let ss = crate::drivers::nvme::sector_size() as u64;
        let bytes = sectors * ss;
        let mib = bytes / (1024 * 1024);

        hl("Disk /dev/nvme0n1: ");
        put_size_mib(mib);
        puts(", ");
        put_u64(bytes);
        puts(" bytes, ");
        put_u64(sectors);
        puts(" sectors\n");
        dim("Disk model: NVMe-SSD\n");
        dim("Units: sectors of ");
        put_u64(ss);
        puts(" bytes\n");

        // Parse and display GPT
        let part_count = crate::drivers::gpt::partition_count();
        if part_count == 0 {
            dim("Partition table type: none (unpartitioned)\n\n");
        } else {
            dim("Partition table type: GPT\n");
            puts("Device\t\t\tStart\t\tEnd\t\tSize\tType\n");
            dim("─────────────────────────────────────────────────────────────────\n");
            for i in 0..part_count {
                if let Some(part) = crate::drivers::gpt::get_partition(i) {
                    puts("/dev/nvme0n1p");
                    put_u64((i + 1) as u64);
                    puts("\t");
                    put_u64(part.start_lba);
                    puts("\t\t");
                    put_u64(part.end_lba);
                    puts("\t\t");
                    let size_sec = part.end_lba.saturating_sub(part.start_lba) + 1;
                    let size_mib = size_sec * ss / (1024 * 1024);
                    put_size_mib(size_mib);
                    puts("\t");
                    // Print GUID type short label
                    let type_label = guid_type_label(&part.type_guid);
                    puts(type_label);
                    if !part.name.is_empty() {
                        puts(" (");
                        puts(&part.name);
                        puts(")");
                    }
                    puts("\n");
                }
            }
            puts("\n");
        }
    }

    // ── AHCI/ATA disks ───────────────────────────────────────────────────
    let n = crate::drivers::disk_count();
    for i in 0..n {
        if let Some(d) = crate::drivers::disk_info(i) {
            found_any = true;
            let dev_name = match d.kind {
                crate::drivers::DiskKind::Ahci => {
                    let idx = crate::drivers::disk_info_count_before(
                        i, crate::drivers::DiskKind::Ahci);
                    match idx { 0 => "sda", 1 => "sdb", 2 => "sdc", _ => "sdX" }
                }
                crate::drivers::DiskKind::Ata => {
                    let idx = crate::drivers::disk_info_count_before(
                        i, crate::drivers::DiskKind::Ata);
                    match idx { 0 => "hda", 1 => "hdb", 2 => "hdc", _ => "hdX" }
                }
            };
            hl("Disk /dev/");
            hl(dev_name);
            puts(": ");
            put_size_mib(d.size_mb as u64);
            puts(", ");
            put_u64(d.size_mb as u64 * 1024 * 1024);
            puts(" bytes, ");
            put_u64(d.sectors);
            puts(" sectors\n");
            puts("Disk model: ");
            puts(crate::drivers::model_str(d));
            puts("\n");
            dim("Units: sectors of 512 bytes\n");

            // Try to read GPT from AHCI disk
            let mut ahci_dev = crate::drivers::block::AhciDisk { disk_idx: i };
            let ahci_parts = crate::drivers::gpt::parse(&mut ahci_dev);
            let pc = ahci_parts.unwrap_or(0);
            if pc == 0 {
                dim("Partition table type: none\n\n");
            } else {
                dim("Partition table type: GPT\n");
                puts("Device\t\t\tStart\t\tEnd\t\tSize\tType\n");
                dim("─────────────────────────────────────────────────────────\n");
                for j in 0..pc {
                    if let Some(part) = crate::drivers::gpt::get_partition(j) {
                        puts("/dev/");
                        puts(dev_name);
                        put_u64((j + 1) as u64);
                        puts("\t");
                        put_u64(part.start_lba);
                        puts("\t\t");
                        put_u64(part.end_lba);
                        puts("\t\t");
                        let size_sec = part.end_lba.saturating_sub(part.start_lba) + 1;
                        let size_mib = size_sec * 512 / (1024 * 1024);
                        put_size_mib(size_mib);
                        puts("\t");
                        puts(guid_type_label(&part.type_guid));
                        if !part.name.is_empty() {
                            puts(" ("); puts(&part.name); puts(")");
                        }
                        puts("\n");
                    }
                }
                puts("\n");
            }
        }
    }

    if !found_any {
        dim("No block devices found.\n");
        dim("Attach a disk: QEMU -drive file=disk.img,format=raw,if=none,id=d0\n");
        dim("               -device ahci,id=ahci0 -device ide-hd,drive=d0,bus=ahci0.0\n");
    }
}

// ─── lsblk ───────────────────────────────────────────────────────────────

/// Extended `lsblk` with NVMe support.
pub fn run_lsblk(_args: &str) {
    puts("NAME           TYPE    SIZE       RO  MODEL\n");
    dim("──────────────────────────────────────────────────────────\n");

    // NVMe
    if crate::drivers::nvme::is_initialized() {
        let sectors = crate::drivers::nvme::sector_count();
        let ss = crate::drivers::nvme::sector_size() as u64;
        let mib = sectors * ss / (1024 * 1024);
        puts("nvme0n1        NVMe    ");
        put_size_mib(mib);
        puts("   0   NVMe-SSD\n");

        // GPT partitions
        for i in 0..crate::drivers::gpt::partition_count() {
            if let Some(part) = crate::drivers::gpt::get_partition(i) {
                puts("  nvme0n1p");
                put_u64((i + 1) as u64);
                puts("      part    ");
                let psize = (part.end_lba.saturating_sub(part.start_lba) + 1) * ss;
                put_size_mib(psize / (1024 * 1024));
                puts("   0   ");
                if !part.name.is_empty() { puts(&part.name); }
                puts("\n");
            }
        }
    }

    // AHCI / ATA
    let n = crate::drivers::disk_count();
    for i in 0..n {
        if let Some(d) = crate::drivers::disk_info(i) {
            let base_name = match d.kind {
                crate::drivers::DiskKind::Ahci => {
                    let idx = crate::drivers::disk_info_count_before(i, crate::drivers::DiskKind::Ahci);
                    match idx { 0 => "sda", 1 => "sdb", 2 => "sdc", _ => "sdX" }
                }
                crate::drivers::DiskKind::Ata => {
                    let idx = crate::drivers::disk_info_count_before(i, crate::drivers::DiskKind::Ata);
                    match idx { 0 => "hda", 1 => "hdb", 2 => "hdc", _ => "hdX" }
                }
            };
            let kind_s = match d.kind {
                crate::drivers::DiskKind::Ahci => "SATA  ",
                crate::drivers::DiskKind::Ata  => "IDE   ",
            };
            puts(base_name);
            puts("             ");
            puts(kind_s);
            puts("  ");
            put_size_mib(d.size_mb as u64);
            puts("   0   ");
            puts(crate::drivers::model_str(d));
            puts("\n");
        }
    }
}

// ─── mkfs ─────────────────────────────────────────────────────────────────

/// `mkfs.aeterna <device> [label]`
/// Formats the given block device (partition) with AeternaFS.
///
/// Examples:
///   mkfs /dev/nvme0n1p1
///   mkfs /dev/nvme0n1p1 MYPART
///
pub fn run_mkfs(args: &str) {
    let mut tokens = args.splitn(3, ' ');
    let device = match tokens.next() {
        Some(d) if !d.is_empty() => d.trim(),
        _ => {
            err("mkfs: missing device\n");
            dim("Usage: mkfs <device> [label]\n");
            dim("       mkfs /dev/nvme0n1p1\n");
            dim("       mkfs /dev/nvme0n1p1 MYPART\n");
            return;
        }
    };
    let label = tokens.next().unwrap_or("AETERNA").trim();
    let label = if label.is_empty() { "AETERNA" } else { label };

    puts("mkfs.aeterna: formatting ");
    puts(device);
    puts(" with label '");
    puts(label);
    puts("'\n");

    // Parse device path → find partition info
    if device.starts_with("/dev/nvme0n1p") {
        let part_num_str = &device["/dev/nvme0n1p".len()..];
        let part_num: usize = match parse_usize(part_num_str) {
            Some(n) if n >= 1 => n,
            _ => { err("mkfs: invalid partition number\n"); return; }
        };
        let idx = part_num - 1;
        let part = match crate::drivers::gpt::get_partition(idx) {
            Some(p) => p,
            None => {
                err("mkfs: partition not found in GPT\n");
                dim("Tip: run `fdisk -l` to list available partitions\n");
                return;
            }
        };
        let ss = crate::drivers::nvme::sector_size();
        let fmt_ok = crate::fs::aeternafs::mkfs(
            crate::fs::aeternafs::DiskBacking::NvmePartition { part_start: part.start_lba },
            part.start_lba,
            part.end_lba.saturating_sub(part.start_lba) + 1,
            ss,
            label,
        );
        if fmt_ok {
            ok("[AeternaFS] Format successful\n");
            puts("  Label:     "); puts(label); puts("\n");
            puts("  Partition: "); puts(device); puts("\n");
            puts("  Start LBA: "); put_u64(part.start_lba); puts("\n");
            puts("  Sectors:   "); put_u64(part.end_lba.saturating_sub(part.start_lba) + 1); puts("\n");
            dim("Run `mount /dev/nvme0n1p");
            put_u64_col(part_num as u64, FG_DIM);
            dim(" /mnt/target` to mount (not yet implemented interactively),\n");
            dim("or use `install` to deploy AETERNA to this partition.\n");
        } else {
            err("mkfs: format failed\n");
        }
    } else if device.starts_with("/dev/") {
        // AHCI partition — try to find disk index from name
        let name = &device["/dev/".len()..];
        let (disk_name, part_str) = split_dev_name(name);
        let disk_idx = match ahci_name_to_idx(disk_name) {
            Some(i) => i,
            None => { err("mkfs: unknown device (only /dev/nvme0n1pN and /dev/sdaN supported)\n"); return; }
        };
        let part_num: usize = match parse_usize(part_str) {
            Some(n) if n >= 1 => n,
            _ => { err("mkfs: invalid partition number\n"); return; }
        };

        // Parse GPT on the AHCI disk to get partition LBAs
        let mut ahci_dev = crate::drivers::block::AhciDisk { disk_idx };
        let pc = crate::drivers::gpt::parse(&mut ahci_dev).unwrap_or(0);
        if pc == 0 {
            err("mkfs: no GPT found on disk\n");
            return;
        }
        let part = match crate::drivers::gpt::get_partition(part_num - 1) {
            Some(p) => p,
            None => { err("mkfs: partition index out of range\n"); return; }
        };
        let fmt_ok = crate::fs::aeternafs::mkfs(
            crate::fs::aeternafs::DiskBacking::AhciPartition {
                disk_idx,
                part_start: part.start_lba,
            },
            part.start_lba,
            part.end_lba.saturating_sub(part.start_lba) + 1,
            512,
            label,
        );
        if fmt_ok { ok("Format successful\n"); } else { err("mkfs: format failed\n"); }
    } else {
        err("mkfs: unrecognized device path\n");
        dim("Supported: /dev/nvme0n1pN  /dev/sdaN\n");
    }
}

// ─── mount helper (called after mkfs or at boot) ──────────────────────────

/// `mount <device> <mountpoint>`
pub fn run_mount(args: &str) {
    let mut toks = args.splitn(3, ' ');
    let device = match toks.next() { Some(d) if !d.is_empty() => d.trim(), _ => {
        err("mount: usage: mount <device> <mountpoint>\n"); return;
    }};
    let mountpoint = match toks.next() { Some(m) if !m.is_empty() => m.trim(), _ => {
        err("mount: missing mountpoint\n"); return;
    }};

    puts("Mounting ");
    puts(device);
    puts(" at ");
    puts(mountpoint);
    puts("...\n");

    let mounted = if device.starts_with("/dev/nvme0n1p") {
        let part_str = &device["/dev/nvme0n1p".len()..];
        let idx = parse_usize(part_str).unwrap_or(1).saturating_sub(1);
        if let Some(part) = crate::drivers::gpt::get_partition(idx) {
            let ss = crate::drivers::nvme::sector_size();
            let ok = crate::fs::aeternafs::mount_nvme_partition(part.start_lba, ss);
            if ok {
                crate::fs::mount(mountpoint, crate::fs::aeternafs::instance())
            } else {
                false
            }
        } else { false }
    } else {
        err("mount: only NVMe partitions supported interactively\n");
        false
    };

    if mounted { ok("Mounted successfully\n"); }
    else { err("mount: failed — check that the partition is formatted with mkfs first\n"); }
}

// ─── helpers ──────────────────────────────────────────────────────────────

fn parse_usize(s: &str) -> Option<usize> {
    let s = s.trim();
    if s.is_empty() { return None; }
    let mut n: usize = 0;
    for b in s.bytes() {
        if b < b'0' || b > b'9' { return None; }
        n = n.wrapping_mul(10).wrapping_add((b - b'0') as usize);
    }
    Some(n)
}

/// Split "sda1" → ("sda", "1")
fn split_dev_name(s: &str) -> (&str, &str) {
    let i = s.bytes().rposition(|b| b < b'0' || b > b'9').map(|i| i + 1).unwrap_or(s.len());
    (&s[..i.saturating_sub(0)], &s[i..])
}

fn ahci_name_to_idx(name: &str) -> Option<usize> {
    let base = name.trim_end_matches(|c: char| c.is_ascii_digit());
    let letter = base.chars().last()?;
    let idx = (letter as u8).wrapping_sub(b'a') as usize;
    let n = crate::drivers::disk_count();
    let mut ahci_found = 0usize;
    for i in 0..n {
        if let Some(d) = crate::drivers::disk_info(i) {
            if matches!(d.kind, crate::drivers::DiskKind::Ahci) {
                if ahci_found == idx { return Some(i); }
                ahci_found += 1;
            }
        }
    }
    None
}

fn guid_type_label(guid: &[u8; 16]) -> &'static str {
    const ESP:   [u8; 16] = [0x28,0x73,0x2A,0xC1,0x1F,0xF8,0xD2,0x11,0xBA,0x4B,0x00,0xA0,0xC9,0x3E,0xC9,0x3B];
    const LINUX: [u8; 16] = [0xAF,0x3D,0xC6,0x0F,0x83,0x84,0x72,0x47,0x8E,0x79,0x3D,0x69,0xD8,0x47,0x7D,0xE4];
    const MSDAT: [u8; 16] = [0xA2,0xA0,0xD0,0xEB,0xE5,0xB9,0x33,0x44,0x87,0xC0,0x68,0xB6,0xB7,0x26,0x99,0xC7];
    if guid == &ESP   { return "EFI System"; }
    if guid == &LINUX { return "Linux filesystem"; }
    if guid == &MSDAT { return "Microsoft basic data"; }
    // AeternaFS custom GUID: use a simple check on bytes 0-3
    if guid[0] == 0x53 && guid[1] == 0x46 && guid[2] == 0x54 && guid[3] == 0x45 {
        return "AeternaFS";
    }
    "Unknown"
}
