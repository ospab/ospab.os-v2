/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab

Disk persistence for AETERNA's RamFS.

Binary format ("Flat Image Persistence"):
  Superblock:
    SUPER_MAGIC:    u64 LE  (0x0041455445524E41 = "AETERNA\0")
    sector_count:   u32 LE  (number of 512-byte sectors used — stored here so
                             boot can read exactly this many, no guessing)
    MAGIC:          10 bytes b"AETERNA_FS"
    VERSION:        u32 LE  = 1
    COUNT:          u32 LE  = number of entries
  Entries (variable length per entry):
    path_len: u16 LE
    path:     path_len bytes (UTF-8, absolute, e.g. "/etc/hostname")
    type:     u8  (0 = File, 1 = Dir)
    [if File:]
      data_len: u32 LE
      data:     data_len bytes

Storage layout on disk:
  LBA 2048 … LBA 2048 + sector_count

Usage:
  sync_filesystem()           — serialize RamFS → disk
  read_from_disk()            → Some(raw_bytes) reads exactly sector_count sectors
  deserialize_ramfs(raw)      → Some(BTreeMap<path, RamNode>)
  (then call ramfs::restore_from_tree(tree) to restore on boot)
*/

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::convert::TryInto;

use super::ramfs::RamNode;

// ─── Constants ───────────────────────────────────────────────────────────────

/// Superblock magic at LBA_BASE (8 bytes): 0x41455445524E41 = "AETERNA"
pub const SUPER_MAGIC: u64 = 0x41455445524E41;

/// Magic signature that follows the superblock
const MAGIC: &[u8; 10] = b"AETERNA_FS";

/// Current format version
const VERSION: u32 = 1;

/// Node type tags
const TAG_FILE: u8 = 0;
const TAG_DIR:  u8 = 1;

/// Starting LBA for persistence data (well past MBR/GPT/ISO area)
const LBA_BASE: u64 = 2048;

/// Maximum bytes we write (limits how many sectors we use: 8 MiB)
const MAX_FS_BYTES: usize = 8 * 1024 * 1024;

/// Choose the primary persistence disk: prefer AHCI, else first disk.
fn select_disk() -> Option<(usize, crate::drivers::DiskKind, usize)> {
    let total = crate::drivers::disk_count();
    // Prefer first AHCI disk
    for i in 0..total {
        if let Some(info) = crate::drivers::disk_info(i) {
            if info.kind == crate::drivers::DiskKind::Ahci {
                let ahci_idx = crate::drivers::disk_info_count_before(i, crate::drivers::DiskKind::Ahci);
                return Some((i, info.kind, ahci_idx));
            }
        }
    }
    // Fallback: first disk of any kind
    if let Some(info) = crate::drivers::disk_info(0) {
        let kind_idx = match info.kind {
            crate::drivers::DiskKind::Ahci => crate::drivers::disk_info_count_before(0, crate::drivers::DiskKind::Ahci),
            crate::drivers::DiskKind::Ata => crate::drivers::disk_info_count_before(0, crate::drivers::DiskKind::Ata),
        };
        return Some((0, info.kind, kind_idx));
    }
    None
}

/// Expose the chosen disk index for boot-time inspection
pub fn primary_disk_index() -> Option<usize> {
    select_disk().map(|(g, _, _)| g)
}

// ─── Serialization ───────────────────────────────────────────────────────────

/// Compute the required serialized size (unpadded) for a RamFS tree.
fn required_size(tree: &BTreeMap<String, RamNode>) -> usize {
    let mut total = 0usize;
    // Superblock header
    total += 8;                      // SUPER_MAGIC
    total += 4;                      // sector_count placeholder
    total += MAGIC.len();            // MAGIC
    total += 4;                      // VERSION
    total += 4;                      // COUNT

    for (path, node) in tree {
        total += 2 + path.len() + 1; // path_len + path + tag
        if let RamNode::File(data) = node {
            total += 4 + data.len();
        }
    }
    total
}

struct RawWriter {
    ptr: *mut u8,
    cap: usize,
    len: usize,
}

impl RawWriter {
    fn new(ptr: *mut u8, cap: usize) -> Self { Self { ptr, cap, len: 0 } }
    fn remaining(&self) -> usize { self.cap.saturating_sub(self.len) }
    fn write(&mut self, data: &[u8]) -> bool {
        if data.len() > self.remaining() { return false; }
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), self.ptr.add(self.len), data.len()); }
        self.len += data.len();
        true
    }
    fn write_u32(&mut self, v: u32) -> bool { self.write(&v.to_le_bytes()) }
    fn write_u64(&mut self, v: u64) -> bool { self.write(&v.to_le_bytes()) }
}

/// Serialize a RamFS tree snapshot directly into a raw buffer.
/// `total_sectors` is patched into offset 8 after serialization.
/// Returns bytes written on success.
fn serialize_ramfs_into(tree: &BTreeMap<String, RamNode>, dest: *mut u8, cap: usize, total_sectors: u32) -> Option<usize> {
    let mut w = RawWriter::new(dest, cap);

    // Superblock magic (8 bytes)
    if !w.write_u64(SUPER_MAGIC) { return None; }
    // sector_count (4 bytes) — how many 512-byte sectors are used
    if !w.write_u32(total_sectors) { return None; }
    // Secondary magic + header
    if !w.write(MAGIC) { return None; }
    if !w.write_u32(VERSION) { return None; }

    // Placeholder for node count
    let count_offset = w.len;
    if !w.write_u32(0) { return None; }

    let mut count: u32 = 0;
    for (path, node) in tree {
        let path_bytes = path.as_bytes();
        if path_bytes.len() > u16::MAX as usize { continue; }

        if !w.write(&(path_bytes.len() as u16).to_le_bytes()) { return None; }
        if !w.write(path_bytes) { return None; }

        match node {
            RamNode::Dir => {
                if !w.write(&[TAG_DIR]) { return None; }
            }
            RamNode::File(data) => {
                let data_len = data.len().min(MAX_FS_BYTES / 2) as u32;
                if !w.write(&[TAG_FILE]) { return None; }
                if !w.write_u32(data_len) { return None; }
                if !w.write(&data[..data_len as usize]) { return None; }
            }
        }
        count += 1;
    }

    // Patch count
    unsafe {
        core::ptr::copy_nonoverlapping(count.to_le_bytes().as_ptr(), dest.add(count_offset), 4);
    }

    Some(w.len)
}

// ─── Deserialization ──────────────────────────────────────────────────────────

/// Deserialize a flat byte blob back into a RamFS tree.
/// Returns None if the magic or version is invalid.
pub fn deserialize_ramfs(buf: &[u8]) -> Option<BTreeMap<String, RamNode>> {
    // Header: SUPER_MAGIC(8) + sector_count(4) + MAGIC(10) + VERSION(4) + COUNT(4)
    let hdr_min = 8 + 4 + MAGIC.len() + 4 + 4;
    if buf.len() < hdr_min { return None; }

    // Superblock magic
    if u64::from_le_bytes(buf[0..8].try_into().ok()?) != SUPER_MAGIC { return None; }
    // Skip sector_count (4 bytes)
    let mut cursor = 8 + 4;
    // MAGIC
    if &buf[cursor..cursor + MAGIC.len()] != MAGIC { return None; }
    cursor += MAGIC.len();

    // Version
    let version = u32::from_le_bytes(buf[cursor..cursor + 4].try_into().ok()?);
    cursor += 4;
    if version != VERSION { return None; }

    // Entry count
    let count = u32::from_le_bytes(buf[cursor..cursor + 4].try_into().ok()?);
    cursor += 4;

    let mut tree: BTreeMap<String, RamNode> = BTreeMap::new();

    for _ in 0..count {
        if cursor + 2 > buf.len() { break; }

        // path_len
        let path_len = u16::from_le_bytes(buf[cursor..cursor + 2].try_into().ok()?) as usize;
        cursor += 2;

        if cursor + path_len > buf.len() { break; }

        // path
        let path = match core::str::from_utf8(&buf[cursor..cursor + path_len]) {
            Ok(s) => String::from(s),
            Err(_) => break,
        };
        cursor += path_len;

        if cursor >= buf.len() { break; }

        // node type
        let tag = buf[cursor];
        cursor += 1;

        match tag {
            TAG_DIR => {
                tree.insert(path, RamNode::Dir);
            }
            TAG_FILE => {
                if cursor + 4 > buf.len() { break; }
                let data_len = u32::from_le_bytes(buf[cursor..cursor + 4].try_into().ok()?) as usize;
                cursor += 4;
                if cursor + data_len > buf.len() { break; }
                let data = buf[cursor..cursor + data_len].to_vec();
                cursor += data_len;
                tree.insert(path, RamNode::File(data));
            }
            _ => break, // unknown tag → corrupt data
        }
    }

    Some(tree)
}

// ─── Disk I/O ────────────────────────────────────────────────────────────────

/// Write a serialized blob to disk starting at LBA_BASE.
/// Data is padded to a whole number of 512-byte sectors.
/// Returns true on success.
pub fn write_to_disk(data: *const u8, total_bytes: usize, sector_count: u32, buffer_phys: u64) -> bool {
    let (global_idx, kind, per_kind_idx) = match select_disk() { Some(t) => t, None => return false };

    match kind {
        crate::drivers::DiskKind::Ahci => {
            return crate::drivers::ahci::dma_write(per_kind_idx, LBA_BASE, sector_count, buffer_phys);
        }
        crate::drivers::DiskKind::Ata => {
            // Fallback: copy through existing ATA path via unified driver
            let slice = unsafe { core::slice::from_raw_parts(data, total_bytes) };
            return crate::drivers::write(global_idx, LBA_BASE, sector_count, slice);
        }
    }
}

/// Read persisted RamFS blob from disk.
/// Strategy:
///   1. Read exactly 1 sector to validate SUPER_MAGIC and extract sector_count.
///   2. Allocate buffer for sector_count sectors.
///   3. Read remaining sectors in 128-sector batches (ATA-safe).
pub fn read_from_disk() -> Option<Vec<u8>> {
    let (global_idx, _kind, _per_kind_idx) = select_disk()?;

    // Step 1: read header sector
    let mut hdr = [0u8; 512];
    if !crate::drivers::read(global_idx, LBA_BASE, 1, &mut hdr) {
        crate::arch::x86_64::serial::write_str("[FS] read header sector failed\r\n");
        return None;
    }

    // Validate SUPER_MAGIC (bytes 0-7)
    if u64::from_le_bytes(hdr[0..8].try_into().ok()?) != SUPER_MAGIC {
        crate::arch::x86_64::serial::write_str("[FS] superblock magic mismatch\r\n");
        return None;
    }

    // Read stored sector count (bytes 8-11)
    let sector_count = u32::from_le_bytes(hdr[8..12].try_into().ok()?);
    if sector_count == 0 || sector_count as usize > MAX_FS_BYTES / 512 {
        crate::arch::x86_64::serial::write_str("[FS] invalid sector_count in superblock\r\n");
        return None;
    }

    crate::arch::x86_64::serial::write_str("[FS] reading ");
    serial_dec(sector_count as u64);
    crate::arch::x86_64::serial::write_str(" sectors from LBA 2048\r\n");

    // Step 2: allocate full buffer
    let total = sector_count as usize * 512;
    let mut buf: Vec<u8> = alloc::vec![0u8; total];

    // Step 3: batched read (128 sectors = 64 KiB per batch, safe for ATA)
    const BATCH: u32 = 128;
    let mut done = 0u32;
    while done < sector_count {
        let batch = (sector_count - done).min(BATCH);
        let off = done as usize * 512;
        let end = off + batch as usize * 512;
        if !crate::drivers::read(global_idx, LBA_BASE + done as u64, batch, &mut buf[off..end]) {
            crate::arch::x86_64::serial::write_str("[FS] batch read failed at sector ");
            serial_dec(done as u64);
            crate::arch::x86_64::serial::write_str("\r\n");
            return None;
        }
        done += batch;
    }

    Some(buf)
}

fn serial_dec(mut v: u64) {
    if v == 0 { crate::arch::x86_64::serial::write_byte(b'0'); return; }
    let mut buf = [0u8; 20]; let mut i = 0;
    while v > 0 { buf[i] = b'0' + (v % 10) as u8; v /= 10; i += 1; }
    for j in (0..i).rev() { crate::arch::x86_64::serial::write_byte(buf[j]); }
}

// ─── High-level sync ──────────────────────────────────────────────────────────

/// Deterministic Memory Fabric token for persistence buffer
#[derive(Copy, Clone)]
struct MedToken {
    phys: u64,
    virt: *mut u8,
    capacity: usize,
}

/// Snapshot the current RamFS to disk.
/// Returns true if written successfully.
pub fn sync_filesystem() -> bool {
    let tree = match super::get_tree_copy() {
        Some(t) => t,
        None => return false,
    };

    if tree.is_empty() { return false; }

    crate::arch::x86_64::serial::write_str("[FS] sync begin\r\n");

    let required = required_size(&tree);
    let sector_size = 512usize;
    let sectors_needed = ((required + sector_size - 1) / sector_size).max(1);
    let total_bytes = sectors_needed * sector_size;
    let frames_needed = ((total_bytes + 4095) / 4096) as u64;

    // Deterministic Memory Fabric: allocate/retain a dedicated MED token for serialization
    static mut MED_BUF: Option<MedToken> = None;
    let med = unsafe {
        match &MED_BUF {
            Some(tok) if tok.capacity >= total_bytes => MedToken { phys: tok.phys, virt: tok.virt, capacity: tok.capacity },
            _ => {
                if let Some(p) = crate::mm::physical::alloc_frames(frames_needed) {
                    let cap = (frames_needed as usize) * 4096;
                    let hhdm = crate::arch::x86_64::boot::hhdm_offset().unwrap_or(0xFFFF_8000_0000_0000);
                    let virt = (p + hhdm) as *mut u8;
                    let tok = MedToken { phys: p, virt, capacity: cap };
                    MED_BUF = Some(tok);
                    tok
                } else {
                    return false;
                }
            }
        }
    };

    // Zero buffer then serialize directly
    unsafe { core::ptr::write_bytes(med.virt, 0, med.capacity); }
    let written = match serialize_ramfs_into(&tree, med.virt, med.capacity, sectors_needed as u32) {
        Some(n) => n,
        None => return false,
    };
    if written == 0 { return false; }

    // Patch sector_count into the buffer at offset 8 now that we know sectors_needed
    unsafe {
        core::ptr::copy_nonoverlapping(
            (sectors_needed as u32).to_le_bytes().as_ptr(),
            med.virt.add(8),
            4,
        );
    }

    crate::arch::x86_64::serial::write_str("[FS] writing to disk\r\n");
    if !write_to_disk(med.virt, total_bytes, sectors_needed as u32, med.phys) {
        crate::arch::x86_64::serial::write_str("[FS] write_to_disk failed\r\n");
        return false;
    }

    // Read-back verification: AHCI read of superblock must match magic
    let mut verify = [0u8; 512];
    let (global_idx, kind, per_kind_idx) = match select_disk() { Some(t) => t, None => return false };
    let read_ok = match kind {
        crate::drivers::DiskKind::Ahci => crate::drivers::ahci::read_sectors(per_kind_idx, LBA_BASE, 1, &mut verify),
        crate::drivers::DiskKind::Ata => crate::drivers::read(global_idx, LBA_BASE, 1, &mut verify),
    };
    if !read_ok || verify.len() < 8 || u64::from_le_bytes(verify[0..8].try_into().unwrap_or_default()) != SUPER_MAGIC {
        panic!("DISK_WRITE_VERIFICATION_FAILED");
    }

    crate::arch::x86_64::serial::write_str("[FS] sync ok\r\n");

    true
}
