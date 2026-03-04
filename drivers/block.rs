/*
 * AETERNA BlockDevice abstraction + DevFS
 *
 * BSL 1.1 — Copyright (c) 2026 ospab
 *
 * BlockDevice: unified trait covering NVMe, AHCI, ATA.
 *   device.read(lba, buf)  / device.write(lba, buf)
 *   device.sector_size()   device.sector_count()
 *   device.model()         device.name()
 *
 * DevFS: /dev/ filesystem nodes.
 *   /dev/nvme0n1     — whole NVMe disk
 *   /dev/nvme0n1p1   — partition 1 (offset + size enforced)
 *   /dev/sda, /dev/hda — AHCI / ATA
 *   /dev/null, /dev/zero — classic char devices
 *
 * Devices are registered after drivers init() and GPT parse().
 * VFS mount for /dev/ is a lightweight CharDevice layer on top of VFS mod.
 */

extern crate alloc;
use alloc::string::String;

// ─── BlockDevice trait ────────────────────────────────────────────────────

/// Unified read/write interface for any block storage device.
pub trait BlockDevice: Send {
    fn name(&self) -> &str;
    fn model(&self) -> &str;
    fn sector_size(&self) -> usize;
    fn sector_count(&self) -> u64;

    /// Read `buf.len() / sector_size()` sectors from `lba` into `buf`.
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> bool;

    /// Write `buf.len() / sector_size()` sectors from `buf` to `lba`.
    fn write(&mut self, lba: u64, buf: &[u8]) -> bool;

    fn size_mib(&self) -> u64 {
        self.sector_count() * self.sector_size() as u64 / (1024 * 1024)
    }
}

// ─── NvmeDisk: BlockDevice over the first NVMe namespace ─────────────────

pub struct NvmeDisk {
    model: [u8; 41],
}

impl NvmeDisk {
    pub fn new() -> Self {
        // Pull model string from Identify data (already stored in nvme module)
        NvmeDisk { model: [0u8; 41] }
    }
}

impl BlockDevice for NvmeDisk {
    fn name(&self) -> &str { "nvme0n1" }
    fn model(&self) -> &str { "NVMe SSD" }
    fn sector_size(&self) -> usize { crate::drivers::nvme::sector_size() }
    fn sector_count(&self) -> u64  { crate::drivers::nvme::sector_count() }

    fn read(&mut self, lba: u64, buf: &mut [u8]) -> bool {
        let ss = self.sector_size();
        if ss == 0 || buf.len() % ss != 0 { return false; }
        let count = (buf.len() / ss) as u32;
        crate::drivers::nvme::read_sectors(lba, count, buf)
    }

    fn write(&mut self, lba: u64, buf: &[u8]) -> bool {
        let ss = self.sector_size();
        if ss == 0 || buf.len() % ss != 0 { return false; }
        let count = (buf.len() / ss) as u32;
        crate::drivers::nvme::write_sectors(lba, count, buf)
    }
}

// ─── NvmeDisk as BlockReadSectors for GPT parser ──────────────────────────

impl crate::drivers::gpt::BlockReadSectors for NvmeDisk {
    fn read_sectors(&mut self, lba: u64, count: u32, buf: &mut [u8]) -> bool {
        crate::drivers::nvme::read_sectors(lba, count, buf)
    }
    fn sector_size(&self) -> usize { crate::drivers::nvme::sector_size() }
    fn total_sectors(&self) -> u64 { crate::drivers::nvme::sector_count() }
}

// ─── PartitionDevice: BlockDevice scoped to a single GPT partition ────────

pub struct PartitionDevice {
    pub disk_name: String,    // e.g. "nvme0n1"
    pub part_num:  usize,     // 1-based
    pub start_lba: u64,
    pub size_lba:  u64,
    pub ss:        usize,
}

impl BlockDevice for PartitionDevice {
    fn name(&self) -> &str { "nvme_part" }
    fn model(&self) -> &str { "" }
    fn sector_size(&self) -> usize { self.ss }
    fn sector_count(&self) -> u64  { self.size_lba }

    fn read(&mut self, relative_lba: u64, buf: &mut [u8]) -> bool {
        if relative_lba + (buf.len() / self.ss) as u64 > self.size_lba { return false; }
        let abs_lba = self.start_lba + relative_lba;
        let count = (buf.len() / self.ss) as u32;
        crate::drivers::nvme::read_sectors(abs_lba, count, buf)
    }

    fn write(&mut self, relative_lba: u64, buf: &[u8]) -> bool {
        if relative_lba + (buf.len() / self.ss) as u64 > self.size_lba { return false; }
        let abs_lba = self.start_lba + relative_lba;
        let count = (buf.len() / self.ss) as u32;
        crate::drivers::nvme::write_sectors(abs_lba, count, buf)
    }
}

// ─── AhciDisk: BlockDevice wrapper over AHCI driver ──────────────────────

pub struct AhciDisk {
    pub disk_idx: usize,
}

impl BlockDevice for AhciDisk {
    fn name(&self) -> &str { "ahci_disk" }
    fn model(&self) -> &str { "" }
    fn sector_size(&self) -> usize { 512 }
    fn sector_count(&self) -> u64 {
        crate::drivers::disk_info(self.disk_idx)
            .map(|d| d.sectors)
            .unwrap_or(0)
    }
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> bool {
        let count = (buf.len() / 512) as u32;
        crate::drivers::read(self.disk_idx, lba, count, buf)
    }
    fn write(&mut self, lba: u64, buf: &[u8]) -> bool {
        let count = (buf.len() / 512) as u32;
        crate::drivers::write(self.disk_idx, lba, count, buf)
    }
}

impl crate::drivers::gpt::BlockReadSectors for AhciDisk {
    fn read_sectors(&mut self, lba: u64, count: u32, buf: &mut [u8]) -> bool {
        crate::drivers::read(self.disk_idx, lba, count, buf)
    }
    fn sector_size(&self) -> usize { 512 }
    fn total_sectors(&self) -> u64 {
        crate::drivers::disk_info(self.disk_idx)
            .map(|d| d.sectors)
            .unwrap_or(0)
    }
}

// ─── DevFS: /dev/ nodes registered into VFS ───────────────────────────────

/// Register all known block + char devices into the VFS /dev/ tree.
/// Call after nvme::probe_and_init() and drivers::init().
pub fn register_devices() {
    let s = crate::arch::x86_64::serial::write_str;
    s("[DevFS] Registering /dev/ nodes\r\n");

    // Ensure /dev directory exists
    let _ = crate::fs::mkdir("/dev");

    // /dev/null — always empty reads, discards writes
    register_chardev("/dev/null", b"");

    // /dev/zero — zero-byte source (create as a named file with metadata)
    register_chardev("/dev/zero", b"\x00");

    // NVMe disk node
    if crate::drivers::nvme::is_initialized() {
        let ss   = crate::drivers::nvme::sector_size();
        let secs = crate::drivers::nvme::sector_count();
        let mib  = secs * ss as u64 / (1024 * 1024);

        // Write metadata descriptor to the node
        let mut meta = [0u8; 64];
        let s_nvme = b"nvme:sectors=";
        meta[..s_nvme.len()].copy_from_slice(s_nvme);
        let mib_str = u64_to_dec_bytes(mib);
        let ml = mib_str.len();
        meta[s_nvme.len()..s_nvme.len() + ml].copy_from_slice(&mib_str);

        let _ = crate::fs::write_file("/dev/nvme0n1", &meta[..s_nvme.len() + ml]);
        s("[DevFS] /dev/nvme0n1 registered (");
        serial_dec(mib);
        s(" MiB)\r\n");

        // Register partition nodes
        for i in 0..crate::drivers::gpt::partition_count() {
            if let Some(part) = crate::drivers::gpt::get_partition(i) {
                let part_mib = part.size_mib(ss);
                let mut path = [0u8; 24];
                let prefix = b"/dev/nvme0n1p";
                path[..prefix.len()].copy_from_slice(prefix);
                let num = u64_to_dec_bytes(i as u64 + 1);
                path[prefix.len()..prefix.len() + num.len()].copy_from_slice(&num);
                let path_len = prefix.len() + num.len();
                let path_str = core::str::from_utf8(&path[..path_len]).unwrap_or("/dev/nvme0n1pX");

                let mut pmeta = [0u8; 64];
                let s_part = b"nvmep:start=";
                pmeta[..s_part.len()].copy_from_slice(s_part);
                let _ = crate::fs::write_file(path_str, &pmeta[..s_part.len()]);

                s("[DevFS] ");
                s(path_str);
                s(" (");
                serial_dec(part_mib);
                s(" MiB) \"");
                s(part.name.as_str());
                s("\"\r\n");
            }
        }
    }

    // ATA/AHCI disks
    let disk_count = crate::drivers::disk_count();
    for i in 0..disk_count {
        if let Some(d) = crate::drivers::disk_info(i) {
            let name = crate::drivers::dev_name_for_index(i);
            let mut path = [0u8; 16];
            let prefix = b"/dev/";
            path[..prefix.len()].copy_from_slice(prefix);
            let nb = name.as_bytes();
            let plen = prefix.len() + nb.len().min(10);
            path[prefix.len()..plen].copy_from_slice(&nb[..plen - prefix.len()]);
            let path_str = core::str::from_utf8(&path[..plen]).unwrap_or("/dev/sdX");

            let meta = b"block:ata";
            let _ = crate::fs::write_file(path_str, meta);
            s("[DevFS] ");
            s(path_str);
            s(" registered\r\n");
        }
    }

    s("[DevFS] Done\r\n");
}

fn register_chardev(path: &str, content: &[u8]) {
    let _ = crate::fs::write_file(path, content);
}

// ─── Helper: u64 to decimal byte array ───────────────────────────────────

fn u64_to_dec_bytes(mut v: u64) -> alloc::vec::Vec<u8> {
    if v == 0 { return alloc::vec![b'0']; }
    let mut buf = [0u8; 20]; let mut i = 0;
    while v > 0 { buf[i] = b'0' + (v % 10) as u8; v /= 10; i += 1; }
    buf[..i].reverse();
    buf[..i].to_vec()
}

fn serial_dec(mut v: u64) {
    if v == 0 { crate::arch::x86_64::serial::write_byte(b'0'); return; }
    let mut buf = [0u8; 20]; let mut i = 0;
    while v > 0 { buf[i] = b'0' + (v % 10) as u8; v /= 10; i += 1; }
    for j in (0..i).rev() { crate::arch::x86_64::serial::write_byte(buf[j]); }
}
