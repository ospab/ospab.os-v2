/*
 * AETERNA GPT Parser — GUID Partition Table reader
 *
 * BSL 1.1 — Copyright (c) 2026 ospab
 *
 * Parses GPT from any block device that implements BlockRead:
 *   LBA 0  — Protective MBR (checks 0xEE at partition type offset)
 *   LBA 1  — Primary GPT Header (signature "EFI PART", CRC32 verified)
 *   LBA 2+ — Partition entries (128 bytes each), UTF-16LE names → UTF-8
 *
 * Registers partitions in the global partition table.
 * The caller (drivers/mod.rs) maps these to /dev/nvme0n1pX entries.
 *
 * Backup GPT header at the last LBA is also checked if primary fails CRC.
 */

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;

// ─── GPT constants ────────────────────────────────────────────────────────

const GPT_SIGNATURE:    u64 = 0x5452415020494645; // "EFI PART" little-endian
const GPT_REVISION_1_0: u32 = 0x00010000;
const GPT_HEADER_SIZE:  u32 = 92;

// Well-known partition type GUIDs (mixed-endian as stored on disk)
pub const GUID_EMPTY:      [u8; 16] = [0u8; 16];

/// EFI System Partition: C12A7328-F81F-11D2-BA4B-00A0C93EC93B
pub const GUID_ESP: [u8; 16] = [
    0x28, 0x73, 0x2A, 0xC1,  0x1F, 0xF8,  0xD2, 0x11,
    0xBA, 0x4B,  0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B,
];

/// Linux filesystem data: 0FC63DAF-8483-4772-8E79-3D69D8477DE4
pub const GUID_LINUX_DATA: [u8; 16] = [
    0xAF, 0x3D, 0xC6, 0x0F,  0x83, 0x84,  0x72, 0x47,
    0x8E, 0x79,  0x3D, 0x69, 0xD8, 0x47, 0x7D, 0xE4,
];

/// Microsoft Basic Data: EBD0A0A2-B9E5-4433-87C0-68B6B72699C7
pub const GUID_MSDATA: [u8; 16] = [
    0xA2, 0xA0, 0xD0, 0xEB,  0xE5, 0xB9,  0x33, 0x44,
    0x87, 0xC0,  0x68, 0xB6, 0xB7, 0x26, 0x99, 0xC7,
];

// ─── Structs ──────────────────────────────────────────────────────────────

/// A parsed GPT partition entry (human-friendly, UTF-8 name)
#[derive(Clone)]
pub struct GptPartition {
    pub index:      usize,
    pub type_guid:  [u8; 16],
    pub part_guid:  [u8; 16],
    pub start_lba:  u64,
    pub end_lba:    u64,
    pub attributes: u64,
    pub name:       String,
}

impl GptPartition {
    pub fn size_sectors(&self) -> u64 {
        if self.end_lba >= self.start_lba {
            self.end_lba - self.start_lba + 1
        } else {
            0
        }
    }

    pub fn size_mib(&self, sector_bytes: usize) -> u64 {
        self.size_sectors() * sector_bytes as u64 / (1024 * 1024)
    }

    pub fn is_esp(&self) -> bool { self.type_guid == GUID_ESP }
    pub fn is_linux_data(&self) -> bool { self.type_guid == GUID_LINUX_DATA }
    pub fn is_msdata(&self) -> bool { self.type_guid == GUID_MSDATA }
}

// Global partition table (max 128 partitions as per GPT spec)
static mut PARTS: [Option<GptPartition>; 128] = [const { None }; 128];
static mut PART_COUNT: usize = 0;
static mut DISK_GUID: [u8; 16] = [0u8; 16];
static mut TOTAL_SECTORS: u64 = 0;

// ─── CRC32 (standard Ethernet polynomial, same as in installer.rs) ───────

fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFFFFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            if crc & 1 != 0 { crc = (crc >> 1) ^ 0xEDB88320; }
            else             { crc >>= 1; }
        }
    }
    !crc
}

// ─── UTF-16LE → UTF-8 helper ──────────────────────────────────────────────

fn utf16le_to_string(raw: &[u8]) -> String {
    let mut s = String::new();
    let mut i = 0;
    while i + 1 < raw.len() {
        let code = u16::from_le_bytes([raw[i], raw[i + 1]]);
        i += 2;
        if code == 0 { break; }
        // Basic BMP code point to UTF-8
        if code < 0x80 {
            s.push(code as u8 as char);
        } else if code < 0x800 {
            s.push(char::from_u32(code as u32).unwrap_or('?'));
        } else {
            s.push(char::from_u32(code as u32).unwrap_or('?'));
        }
    }
    s
}

// ─── GUID formatting helper ───────────────────────────────────────────────

fn guid_matches(a: &[u8; 16], b: &[u8; 16]) -> bool {
    *a == *b
}

fn guid_is_empty(g: &[u8; 16]) -> bool {
    g.iter().all(|&b| b == 0)
}

// ─── Block I/O abstraction ────────────────────────────────────────────────

/// Trait for reading sectors from a block device.
/// Implemented differently for NVMe and ATA/AHCI.
pub trait BlockReadSectors {
    fn read_sectors(&mut self, lba: u64, count: u32, buf: &mut [u8]) -> bool;
    fn sector_size(&self) -> usize { 512 }
    fn total_sectors(&self) -> u64;
}

// ─── GPT parsing ─────────────────────────────────────────────────────────

/// Parse GPT from the given block device. Populates global partition table.
/// Returns the number of valid partitions found, or None on fatal error.
pub fn parse<B: BlockReadSectors>(dev: &mut B) -> Option<usize> {
    let s = crate::arch::x86_64::serial::write_str;
    let ss = dev.sector_size();

    // Allocate a sector buffer on stack (max 4096 for NVMe with large sectors)
    let mut sector = [0u8; 4096];
    let sector_buf = &mut sector[..ss];

    // ── LBA 0: Protective MBR check ──────────────────────────────────────
    if !dev.read_sectors(0, 1, &mut sector[..ss]) {
        s("[GPT] LBA 0 read failed\r\n");
        return None;
    }
    // Boot signature at offset 510..512
    if sector[510] != 0x55 || sector[511] != 0xAA {
        s("[GPT] No MBR boot signature\r\n");
        // Not fatal — EFI may still have GPT
    }
    // Check first partition entry type = 0xEE (protective MBR for GPT)
    let pmbr_type = sector[0x1C2];
    if pmbr_type != 0xEE {
        s("[GPT] No protective MBR (type=0x");
        serial_hex_byte(pmbr_type);
        s(") — not a GPT disk\r\n");
        return None;
    }
    s("[GPT] Protective MBR found\r\n");

    // ── LBA 1: Primary GPT Header ─────────────────────────────────────────
    let mut header_buf = [0u8; 4096];
    if !dev.read_sectors(1, 1, &mut header_buf[..ss]) {
        s("[GPT] LBA 1 read failed\r\n");
        return None;
    }

    if let Some(hdr) = parse_gpt_header(&header_buf[..92], 1) {
        let count = parse_from_header(dev, &hdr, ss);
        if count.is_some() { return count; }
    }

    // ── Backup GPT header (last LBA on disk) ──────────────────────────────
    let total = dev.total_sectors();
    if total > 34 {
        s("[GPT] Primary header bad — trying backup at LBA ");
        serial_dec(total - 1);
        s("\r\n");
        let mut backup_buf = [0u8; 4096];
        if dev.read_sectors(total - 1, 1, &mut backup_buf[..ss]) {
            if let Some(hdr) = parse_gpt_header(&backup_buf[..92], total - 1) {
                return parse_from_header(dev, &hdr, ss);
            }
        }
    }

    None
}

struct GptHeader {
    my_lba:              u64,
    alternate_lba:       u64,
    first_usable_lba:    u64,
    last_usable_lba:     u64,
    disk_guid:           [u8; 16],
    part_entry_lba:      u64,
    num_part_entries:    u32,
    part_entry_size:     u32,
    part_entry_crc:      u32,
}

fn parse_gpt_header(buf: &[u8], expected_lba: u64) -> Option<GptHeader> {
    if buf.len() < 92 { return None; }
    let s = crate::arch::x86_64::serial::write_str;

    // Signature at offset 0 (8 bytes)
    let sig = u64::from_le_bytes(buf[0..8].try_into().ok()?);
    if sig != GPT_SIGNATURE {
        s("[GPT] Bad header signature\r\n");
        return None;
    }

    // Revision at 8
    let rev = u32::from_le_bytes(buf[8..12].try_into().ok()?);
    if rev != GPT_REVISION_1_0 {
        s("[GPT] Unsupported GPT revision\r\n");
        // Not fatal — continue anyway
    }

    // Header size at 12
    let hdr_size = u32::from_le_bytes(buf[12..16].try_into().ok()?);
    if hdr_size < 92 {
        s("[GPT] Header size too small\r\n");
        return None;
    }

    // CRC32 of header at 16 (zeroed during calculation)
    let stored_crc = u32::from_le_bytes(buf[16..20].try_into().ok()?);
    let mut hdr_copy = [0u8; 512];
    let copy_len = hdr_size.min(512) as usize;
    hdr_copy[..copy_len].copy_from_slice(&buf[..copy_len]);
    // Zero the CRC field
    hdr_copy[16] = 0; hdr_copy[17] = 0; hdr_copy[18] = 0; hdr_copy[19] = 0;
    let calc_crc = crc32(&hdr_copy[..copy_len]);
    if calc_crc != stored_crc {
        s("[GPT] Header CRC mismatch (stored=0x");
        serial_hex32(stored_crc);
        s(" calc=0x");
        serial_hex32(calc_crc);
        s(")\r\n");
        return None;
    }

    let my_lba           = u64::from_le_bytes(buf[24..32].try_into().ok()?);
    let alternate_lba    = u64::from_le_bytes(buf[32..40].try_into().ok()?);
    let first_usable_lba = u64::from_le_bytes(buf[40..48].try_into().ok()?);
    let last_usable_lba  = u64::from_le_bytes(buf[48..56].try_into().ok()?);

    let mut disk_guid = [0u8; 16];
    disk_guid.copy_from_slice(&buf[56..72]);

    let part_entry_lba  = u64::from_le_bytes(buf[72..80].try_into().ok()?);
    let num_entries     = u32::from_le_bytes(buf[80..84].try_into().ok()?);
    let entry_size      = u32::from_le_bytes(buf[84..88].try_into().ok()?);
    let part_crc        = u32::from_le_bytes(buf[88..92].try_into().ok()?);

    if my_lba != expected_lba {
        s("[GPT] MyLBA mismatch\r\n");
        // Still attempt parse
    }

    s("[GPT] Header OK: ");
    serial_dec(num_entries as u64);
    s(" entries × ");
    serial_dec(entry_size as u64);
    s(" bytes starting at LBA ");
    serial_dec(part_entry_lba);
    s("\r\n");

    Some(GptHeader {
        my_lba,
        alternate_lba,
        first_usable_lba,
        last_usable_lba,
        disk_guid,
        part_entry_lba,
        num_part_entries: num_entries,
        part_entry_size:  entry_size,
        part_entry_crc:   part_crc,
    })
}

fn parse_from_header<B: BlockReadSectors>(
    dev: &mut B,
    hdr: &GptHeader,
    ss: usize,
) -> Option<usize> {
    let s = crate::arch::x86_64::serial::write_str;
    let entry_size = hdr.part_entry_size as usize;
    if entry_size < 128 || entry_size > 512 { return None; }

    let entries_per_sector = ss / entry_size;
    let total_entries = hdr.num_part_entries as usize;
    // Clamp to reasonable limit
    let total_entries = total_entries.min(128);

    // Calculate how many sectors to read for all entries
    let sectors_needed = (total_entries * entry_size + ss - 1) / ss;
    let sectors_needed = sectors_needed.min(33);

    // We need an entry buffer; limit to 33 sectors × 4096 bytes = 135 KiB
    // Use a static buffer to avoid stack overflow in no_std
    static mut ENTRY_BUF: [u8; 33 * 512] = [0u8; 33 * 512];
    let read_bytes = sectors_needed * ss;
    let buf = unsafe {
        let b = &mut ENTRY_BUF[..read_bytes];
        if !dev.read_sectors(hdr.part_entry_lba, sectors_needed as u32, b) {
            s("[GPT] Partition entry read failed\r\n");
            return None;
        }
        b
    };

    // Verify partition entry CRC
    let calc_crc = crc32(&buf[..total_entries * entry_size]);
    if calc_crc != hdr.part_entry_crc {
        s("[GPT] Partition CRC mismatch (stored=0x");
        serial_hex32(hdr.part_entry_crc);
        s(" calc=0x");
        serial_hex32(calc_crc);
        s(") — treating as non-fatal\r\n");
        // Non-fatal: proceed
    }

    // Save disk GUID
    unsafe {
        DISK_GUID = hdr.disk_guid;
        TOTAL_SECTORS = dev.total_sectors();
        PART_COUNT = 0;
    }

    let mut found = 0usize;
    for i in 0..total_entries {
        let off = i * entry_size;
        if off + entry_size > buf.len() { break; }
        let entry = &buf[off..off + entry_size];

        let mut type_guid = [0u8; 16];
        type_guid.copy_from_slice(&entry[0..16]);
        if guid_is_empty(&type_guid) { continue; } // empty slot

        let mut part_guid = [0u8; 16];
        part_guid.copy_from_slice(&entry[16..32]);

        let start_lba  = u64::from_le_bytes(entry[32..40].try_into().unwrap_or([0u8; 8]));
        let end_lba    = u64::from_le_bytes(entry[40..48].try_into().unwrap_or([0u8; 8]));
        let attributes = u64::from_le_bytes(entry[48..56].try_into().unwrap_or([0u8; 8]));

        // Name: UTF-16 LE in bytes 56..128 (72 bytes = 36 code units)
        let name_raw = &entry[56..entry_size.min(128)];
        let name = utf16le_to_string(name_raw);

        let part = GptPartition {
            index: found,
            type_guid,
            part_guid,
            start_lba,
            end_lba,
            attributes,
            name,
        };

        let part_type = if part.is_esp() { "ESP" }
            else if part.is_linux_data() { "Linux" }
            else if part.is_msdata() { "MSDATA" }
            else { "other" };

        s("[GPT] Part ");
        serial_dec(found as u64 + 1);
        s(" ["); s(part_type); s("] LBA ");
        serial_dec(start_lba); s(".."); serial_dec(end_lba);
        s(" ("); serial_dec((end_lba - start_lba + 1) * ss as u64 / (1024 * 1024));
        s(" MiB) \"");
        s(part.name.as_str());
        s("\"\r\n");

        unsafe {
            if PART_COUNT < 128 {
                PARTS[PART_COUNT] = Some(part);
                PART_COUNT += 1;
            }
        }
        found += 1;
    }

    s("[GPT] Found ");
    serial_dec(found as u64);
    s(" partitions\r\n");
    Some(found)
}

// ─── Public accessors ─────────────────────────────────────────────────────

pub fn partition_count() -> usize { unsafe { PART_COUNT } }
pub fn disk_guid() -> [u8; 16] { unsafe { DISK_GUID } }

pub fn get_partition(idx: usize) -> Option<GptPartition> {
    if idx >= unsafe { PART_COUNT } { return None; }
    unsafe { PARTS[idx].clone() }
}

pub fn find_esp() -> Option<GptPartition> {
    for i in 0..unsafe { PART_COUNT } {
        if let Some(ref p) = unsafe { &PARTS[i] } {
            if p.is_esp() { return Some(p.clone()); }
        }
    }
    None
}

pub fn find_by_type(type_guid: &[u8; 16]) -> Vec<GptPartition> {
    let mut result = Vec::new();
    for i in 0..unsafe { PART_COUNT } {
        if let Some(ref p) = unsafe { &PARTS[i] } {
            if &p.type_guid == type_guid { result.push(p.clone()); }
        }
    }
    result
}

pub fn find_by_label(label: &str) -> Option<GptPartition> {
    for i in 0..unsafe { PART_COUNT } {
        if let Some(ref p) = unsafe { &PARTS[i] } {
            if p.name.as_str() == label { return Some(p.clone()); }
        }
    }
    None
}

// ─── Serial debug helpers ─────────────────────────────────────────────────

fn serial_hex_byte(v: u8) {
    let h = b"0123456789abcdef";
    crate::arch::x86_64::serial::write_byte(h[(v >> 4) as usize]);
    crate::arch::x86_64::serial::write_byte(h[(v & 0xF) as usize]);
}
fn serial_hex32(v: u32) {
    for i in (0..4).rev() { serial_hex_byte((v >> (i * 8)) as u8); }
}
fn serial_dec(mut v: u64) {
    if v == 0 { crate::arch::x86_64::serial::write_byte(b'0'); return; }
    let mut buf = [0u8; 20]; let mut i = 0;
    while v > 0 { buf[i] = b'0' + (v % 10) as u8; v /= 10; i += 1; }
    for j in (0..i).rev() { crate::arch::x86_64::serial::write_byte(buf[j]); }
}
