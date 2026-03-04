/*
 * AeternaFS — Native AETERNA Filesystem
 *
 * BSL 1.1 — Copyright (c) 2026 ospab
 *
 * A simple but fully functional on-disk filesystem for ospab.os:
 *
 *   Layout per partition (all offsets relative to partition start):
 *   ┌─────────────────────────────────────────────────────────┐
 *   │ LBA 0      │ Superblock (512 B)                         │
 *   │ LBA 1      │ Inode bitmap (512 B = 4096 inode slots)    │
 *   │ LBA 2-9    │ Block bitmap (8 × 512 B = 32768 data blks) │
 *   │ LBA 10-73  │ Inode table (64 × 512 B = 512 inodes)      │
 *   │ LBA 74+    │ Data blocks (1 sector each = ss bytes)     │
 *   └─────────────────────────────────────────────────────────┘
 *
 *   Inode 0 = reserved, Inode 1 = root directory "/"
 *
 *   Files and directories are addressed by inode number.
 *   Directories store 32-byte entries: { inode: u32, name: [u8; 28] }
 *   Files store raw byte data, up to 8 direct block pointers per inode.
 *   Maximum file size: 8 blocks × sector_size bytes
 *   (e.g., 8 × 512 = 4096 B or 8 × 4096 = 32768 B for NVMe)
 *
 *   Write-through: every write immediately goes to disk via NVMe/AHCI.
 *   Interior mutability via spin::Mutex<AeternaFsState>.
 *
 *   On-disk encoding is always little-endian.
 */

#![allow(dead_code)]

extern crate alloc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use spin::Mutex;

use crate::fs::{FileSystem, DirEntry, NodeType};

// ─── On-disk magic ────────────────────────────────────────────────────────

pub const AETERNA_MAGIC:   u64 = 0x5346_5445_4145_0100; // "AETEFS\x01\x00"
pub const AETERNA_VERSION: u32 = 1;
pub const LABEL_AETERNA:   &[u8] = b"AETERNA_SYSTEM";

// ─── Layout constants (all in sectors / LBA units, relative to part_start) ─

const INODE_BMAP_LBA:  u64 = 1;
const BLOCK_BMAP_LBA:  u64 = 2;  // 8 sectors
const INODE_TBL_LBA:   u64 = 10; // 64 sectors
pub const DATA_START_LBA: u64 = 74;

const MAX_INODES:  usize = 512;  // 64 sectors × 8 inodes/sector
const MAX_BLOCKS:  usize = 32768; // 8 sectors × 512 B × 8 bits/byte

const INODE_ROOT:  u32   = 1; // inode 1 = root directory

// ─── Inode modes ─────────────────────────────────────────────────────────

const MODE_FREE: u16 = 0x0000;
const MODE_REG:  u16 = 0x8000;
const MODE_DIR:  u16 = 0x4000;

// ─── On-disk structures (all little-endian) ───────────────────────────────

/// Superblock — fits in first 160 bytes of LBA 0
#[repr(C, packed)]
struct Superblock {
    magic:           u64,     // 0
    version:         u32,     // 8
    flags:           u32,     // 12
    total_lba:       u64,     // 16
    inode_bmap_lba:  u64,     // 24  = 1
    block_bmap_lba:  u64,     // 32  = 2
    inode_tbl_lba:   u64,     // 40  = 10
    data_start_lba:  u64,     // 48  = 74
    total_inodes:    u32,     // 56  = 512
    free_inodes:     u32,     // 60
    total_blocks:    u32,     // 64
    free_blocks:     u32,     // 68
    label:           [u8; 64],// 72
    crc:             u32,     // 136
}

/// Inode — 64 bytes each, 8 per sector
#[repr(C, packed)]
struct Inode {
    mode:     u16,        // 0:  MODE_FREE / MODE_REG / MODE_DIR
    nlinks:   u16,        // 2
    size:     u64,        // 4:  bytes for files, byte-size of dirent area for dirs
    mtime:    u64,        // 12
    direct:   [u32; 8],   // 20: up to 8 direct block indices (0 = unused)
    crc:      u32,        // 52
    _pad:     [u8; 8],    // 56..64
}

/// Directory entry — 32 bytes each
#[repr(C, packed)]
struct Dirent {
    inode:    u32,        // 0: inode number (0 = free slot)
    name:     [u8; 28],   // 4: null-terminated filename
}

// ─── AeternaFS state (mutable, behind Mutex) ────────────────────────────

/// Which physical device backs this filesystem instance
#[derive(Clone, Copy, PartialEq)]
pub enum DiskBacking {
    NvmePartition { part_start: u64 },  // NVMe absolute LBA offset
    AhciPartition { disk_idx: usize, part_start: u64 },
}

struct AeternaFsState {
    backing:      DiskBacking,
    sector_size:  usize,
    ready:        bool,
    total_blocks: u32,
    free_blocks:  u32,
    total_inodes: u32,
    free_inodes:  u32,
}

impl AeternaFsState {
    const fn new() -> Self {
        AeternaFsState {
            backing:      DiskBacking::NvmePartition { part_start: 0 },
            sector_size:  512,
            ready:        false,
            total_blocks: 0,
            free_blocks:  0,
            total_inodes: MAX_INODES as u32,
            free_inodes:  MAX_INODES as u32 - 2,
        }
    }
}

/// The AeternaFS instance — a static singleton per partition
pub struct AeternaFs {
    state: Mutex<AeternaFsState>,
}

impl AeternaFs {
    pub const fn new() -> Self {
        AeternaFs { state: Mutex::new(AeternaFsState::new()) }
    }

    /// Mount: verify superblock or format if magic missing.
    pub fn mount(&self, backing: DiskBacking, sector_size: usize) -> bool {
        let mut st = self.state.lock();
        st.backing = backing;
        st.sector_size = sector_size;

        let mut sb_sec = vec![0u8; sector_size];
        if !read_lba(&st, 0, &mut sb_sec) {
            return false;
        }

        let magic = u64::from_le_bytes(sb_sec[0..8].try_into().unwrap_or([0u8; 8]));
        if magic == AETERNA_MAGIC {
            // Load superblock metadata
            st.total_inodes = u32::from_le_bytes(sb_sec[56..60].try_into().unwrap_or([0u8; 4]));
            st.free_inodes  = u32::from_le_bytes(sb_sec[60..64].try_into().unwrap_or([0u8; 4]));
            st.total_blocks = u32::from_le_bytes(sb_sec[64..68].try_into().unwrap_or([0u8; 4]));
            st.free_blocks  = u32::from_le_bytes(sb_sec[68..72].try_into().unwrap_or([0u8; 4]));
            st.ready = true;
            crate::arch::x86_64::serial::write_str("[AeternaFS] Superblock valid, mounted\r\n");
            true
        } else {
            crate::arch::x86_64::serial::write_str("[AeternaFS] No valid superblock found\r\n");
            false
        }
    }

    pub fn is_ready(&self) -> bool {
        self.state.lock().ready
    }
}

// ─── Low-level disk I/O ───────────────────────────────────────────────────

/// Read one sector at `relative_lba` from the partition (adds part_start).
fn read_lba(st: &AeternaFsState, relative_lba: u64, buf: &mut [u8]) -> bool {
    let ss = st.sector_size;
    if buf.len() < ss { return false; }
    let buf = &mut buf[..ss];
    match st.backing {
        DiskBacking::NvmePartition { part_start } => {
            crate::drivers::nvme::read_sectors(part_start + relative_lba, 1, buf)
        }
        DiskBacking::AhciPartition { disk_idx, part_start } => {
            crate::drivers::read(disk_idx, part_start + relative_lba, 1, buf)
        }
    }
}

/// Write one sector at `relative_lba`.
fn write_lba(st: &AeternaFsState, relative_lba: u64, buf: &[u8]) -> bool {
    let ss = st.sector_size;
    if buf.len() < ss { return false; }
    let buf = &buf[..ss];
    match st.backing {
        DiskBacking::NvmePartition { part_start } => {
            crate::drivers::nvme::write_sectors(part_start + relative_lba, 1, buf)
        }
        DiskBacking::AhciPartition { disk_idx, part_start } => {
            crate::drivers::write(disk_idx, part_start + relative_lba, 1, buf)
        }
    }
}

// ─── Bitmap operations ────────────────────────────────────────────────────

/// Read inode bitmap (1 sector).
fn read_inode_bmap(st: &AeternaFsState) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; st.sector_size];
    if read_lba(st, INODE_BMAP_LBA, &mut buf) { Some(buf) } else { None }
}

fn write_inode_bmap(st: &AeternaFsState, bmap: &[u8]) -> bool {
    if bmap.len() < st.sector_size {
        let mut padded = vec![0u8; st.sector_size];
        padded[..bmap.len()].copy_from_slice(bmap);
        write_lba(st, INODE_BMAP_LBA, &padded)
    } else {
        write_lba(st, INODE_BMAP_LBA, &bmap[..st.sector_size])
    }
}

fn bitmap_get(bmap: &[u8], bit: usize) -> bool {
    let byte = bit / 8;
    let mask = 1u8 << (bit % 8);
    byte < bmap.len() && (bmap[byte] & mask) != 0
}

fn bitmap_set(bmap: &mut [u8], bit: usize, val: bool) {
    let byte = bit / 8;
    let mask = 1u8 << (bit % 8);
    if byte < bmap.len() {
        if val { bmap[byte] |= mask; } else { bmap[byte] &= !mask; }
    }
}

/// Allocate an inode. Returns inode number (1-based) or 0 on failure.
fn alloc_inode(st: &AeternaFsState) -> u32 {
    let mut bmap = match read_inode_bmap(st) { Some(b) => b, None => return 0 };
    // Inode 0 = reserved, start from 2 (1 = root, always allocated)
    for i in 2..MAX_INODES {
        if !bitmap_get(&bmap, i) {
            bitmap_set(&mut bmap, i, true);
            write_inode_bmap(st, &bmap);
            return i as u32;
        }
    }
    0 // no free inodes
}

fn free_inode(st: &AeternaFsState, ino: u32) {
    if let Some(mut bmap) = read_inode_bmap(st) {
        bitmap_set(&mut bmap, ino as usize, false);
        write_inode_bmap(st, &bmap);
    }
}

/// Read block bitmap (8 sectors).
fn read_block_bmap(st: &AeternaFsState) -> Option<Vec<u8>> {
    let ss = st.sector_size;
    let mut buf = vec![0u8; ss * 8];
    for i in 0..8 {
        if !read_lba(st, BLOCK_BMAP_LBA + i as u64, &mut buf[i * ss..(i + 1) * ss]) {
            return None;
        }
    }
    Some(buf)
}

fn write_block_bmap(st: &AeternaFsState, bmap: &[u8]) -> bool {
    let ss = st.sector_size;
    for i in 0..8 {
        let off = i * ss;
        let end = off + ss;
        if end <= bmap.len() {
            if !write_lba(st, BLOCK_BMAP_LBA + i as u64, &bmap[off..end]) {
                return false;
            }
        }
    }
    true
}

/// Allocate a data block. Returns block index (0-based) or u32::MAX on failure.
fn alloc_block(st: &AeternaFsState) -> u32 {
    let mut bmap = match read_block_bmap(st) { Some(b) => b, None => return u32::MAX };
    for i in 0..st.total_blocks as usize {
        if !bitmap_get(&bmap, i) {
            bitmap_set(&mut bmap, i, true);
            write_block_bmap(st, &bmap);
            // Zero out the new block
            let zero = vec![0u8; st.sector_size];
            write_lba(st, DATA_START_LBA + i as u64, &zero);
            return i as u32;
        }
    }
    u32::MAX
}

fn free_block(st: &AeternaFsState, blk: u32) {
    if let Some(mut bmap) = read_block_bmap(st) {
        bitmap_set(&mut bmap, blk as usize, false);
        write_block_bmap(st, &bmap);
    }
}

// ─── Inode I/O ───────────────────────────────────────────────────────────

/// Read a packed inode from the inode table.
fn read_inode_raw(st: &AeternaFsState, ino: u32) -> Option<Inode> {
    if ino == 0 || ino as usize >= MAX_INODES { return None; }
    let ss = st.sector_size;
    let inodes_per_sector = ss / 64;
    let sector_off = (ino as usize) / inodes_per_sector;
    let slot_off   = (ino as usize) % inodes_per_sector;

    let lba = INODE_TBL_LBA + sector_off as u64;
    let mut buf = vec![0u8; ss];
    if !read_lba(st, lba, &mut buf) { return None; }

    let base = slot_off * 64;
    if base + 64 > buf.len() { return None; }

    // Deserialize inode from bytes — manually because repr(packed) + bytecast
    let raw = &buf[base..base + 64];
    let inode = Inode {
        mode:   u16::from_le_bytes([raw[0], raw[1]]),
        nlinks: u16::from_le_bytes([raw[2], raw[3]]),
        size:   u64::from_le_bytes(raw[4..12].try_into().unwrap_or([0u8; 8])),
        mtime:  u64::from_le_bytes(raw[12..20].try_into().unwrap_or([0u8; 8])),
        direct: {
            let mut d = [0u32; 8];
            for k in 0..8 {
                d[k] = u32::from_le_bytes(raw[20 + k*4..24 + k*4].try_into().unwrap_or([0u8; 4]));
            }
            d
        },
        crc:    u32::from_le_bytes(raw[52..56].try_into().unwrap_or([0u8; 4])),
        _pad:   [0u8; 8],
    };
    Some(inode)
}

fn write_inode_raw(st: &AeternaFsState, ino: u32, inode: &Inode) -> bool {
    if ino == 0 || ino as usize >= MAX_INODES { return false; }
    let ss = st.sector_size;
    let inodes_per_sector = ss / 64;
    let sector_off = (ino as usize) / inodes_per_sector;
    let slot_off   = (ino as usize) % inodes_per_sector;

    let lba = INODE_TBL_LBA + sector_off as u64;
    let mut buf = vec![0u8; ss];
    if !read_lba(st, lba, &mut buf) { return false; }

    let base = slot_off * 64;
    if base + 64 > buf.len() { return false; }

    let raw = &mut buf[base..base + 64];
    raw[0..2].copy_from_slice(&inode.mode.to_le_bytes());
    raw[2..4].copy_from_slice(&inode.nlinks.to_le_bytes());
    raw[4..12].copy_from_slice(&inode.size.to_le_bytes());
    raw[12..20].copy_from_slice(&inode.mtime.to_le_bytes());
    for k in 0..8 {
        raw[20 + k*4..24 + k*4].copy_from_slice(&inode.direct[k].to_le_bytes());
    }
    raw[52..56].copy_from_slice(&inode.crc.to_le_bytes());

    write_lba(st, lba, &buf)
}

// ─── Block data I/O ───────────────────────────────────────────────────────

fn read_block(st: &AeternaFsState, blk: u32) -> Option<Vec<u8>> {
    let ss = st.sector_size;
    let mut buf = vec![0u8; ss];
    if read_lba(st, DATA_START_LBA + blk as u64, &mut buf) { Some(buf) } else { None }
}

fn write_block(st: &AeternaFsState, blk: u32, data: &[u8]) -> bool {
    let ss = st.sector_size;
    let mut buf = vec![0u8; ss];
    let len = data.len().min(ss);
    buf[..len].copy_from_slice(&data[..len]);
    write_lba(st, DATA_START_LBA + blk as u64, &buf)
}

// ─── Path resolution helpers ──────────────────────────────────────────────

/// Walk the directory tree to find the inode for `path`.
/// Returns (parent_inode, component_name, inode) for the target.
/// If path = "/" returns (0, "", 1).
fn lookup_path(st: &AeternaFsState, path: &str) -> Option<u32> {
    let path = path.trim_matches('/');
    if path.is_empty() { return Some(INODE_ROOT); }

    let mut current_ino = INODE_ROOT;
    for component in path.split('/') {
        if component.is_empty() { continue; }
        let found = dir_lookup(st, current_ino, component)?;
        current_ino = found;
    }
    Some(current_ino)
}

/// Look up a name in a directory inode. Returns child inode or None.
fn dir_lookup(st: &AeternaFsState, dir_ino: u32, name: &str) -> Option<u32> {
    let inode = read_inode_raw(st, dir_ino)?;
    if inode.mode & 0xF000 != MODE_DIR { return None; }
    let ss = st.sector_size;
    let dirents_per_block = ss / 32;

    for blk_idx in 0..8 {
        let blk = inode.direct[blk_idx];
        if blk == 0 { break; }
        let block_data = read_block(st, blk)?;
        for slot in 0..dirents_per_block {
            let off = slot * 32;
            if off + 32 > block_data.len() { break; }
            let ino = u32::from_le_bytes(block_data[off..off+4].try_into().ok()?);
            if ino == 0 { continue; }
            let raw_name = &block_data[off + 4..off + 32];
            let end = raw_name.iter().position(|&b| b == 0).unwrap_or(28);
            let entry_name = core::str::from_utf8(&raw_name[..end]).unwrap_or("");
            if entry_name == name { return Some(ino); }
        }
    }
    None
}

/// Add a directory entry to a directory inode.
fn dir_add_entry(st: &mut AeternaFsState, dir_ino: u32, name: &str, child_ino: u32) -> bool {
    let mut inode = match read_inode_raw(st, dir_ino) { Some(i) => i, None => return false };
    if inode.mode & 0xF000 != MODE_DIR { return false; }
    if name.len() > 27 { return false; }
    let ss = st.sector_size;
    let dirents_per_block = ss / 32;

    // Find a free slot in existing blocks
    for blk_idx in 0..8 {
        let blk = inode.direct[blk_idx];
        if blk == 0 { break; }
        let mut block_data = match read_block(st, blk) { Some(b) => b, None => continue };
        for slot in 0..dirents_per_block {
            let off = slot * 32;
            if off + 32 > block_data.len() { break; }
            let ino = u32::from_le_bytes(block_data[off..off+4].try_into().unwrap_or([0u8;4]));
            if ino == 0 {
                // Free slot — write entry
                block_data[off..off+4].copy_from_slice(&child_ino.to_le_bytes());
                let nb = name.as_bytes();
                let nlen = nb.len().min(27);
                block_data[off+4..off+4+nlen].copy_from_slice(&nb[..nlen]);
                block_data[off+4+nlen] = 0;
                return write_block(st, blk, &block_data);
            }
        }
    }

    // No free slot — allocate a new block
    for blk_idx in 0..8 {
        if inode.direct[blk_idx] == 0 {
            let new_blk = alloc_block(st);
            if new_blk == u32::MAX { return false; }
            inode.direct[blk_idx] = new_blk;
            inode.size += ss as u64;
            write_inode_raw(st, dir_ino, &inode);

            let mut block_data = vec![0u8; ss];
            block_data[0..4].copy_from_slice(&child_ino.to_le_bytes());
            let nb = name.as_bytes();
            let nlen = nb.len().min(27);
            block_data[4..4+nlen].copy_from_slice(&nb[..nlen]);
            return write_block(st, new_blk, &block_data);
        }
    }
    false // no block pointers left
}

/// Remove a directory entry by name.
fn dir_remove_entry(st: &AeternaFsState, dir_ino: u32, name: &str) -> bool {
    let inode = match read_inode_raw(st, dir_ino) { Some(i) => i, None => return false };
    if inode.mode & 0xF000 != MODE_DIR { return false; }
    let ss = st.sector_size;
    let dirents_per_block = ss / 32;

    for blk_idx in 0..8 {
        let blk = inode.direct[blk_idx];
        if blk == 0 { break; }
        let mut block_data = match read_block(st, blk) { Some(b) => b, None => continue };
        for slot in 0..dirents_per_block {
            let off = slot * 32;
            if off + 32 > block_data.len() { break; }
            let ino = u32::from_le_bytes(block_data[off..off+4].try_into().unwrap_or([0u8;4]));
            if ino == 0 { continue; }
            let raw_name = &block_data[off+4..off+32];
            let end = raw_name.iter().position(|&b| b == 0).unwrap_or(28);
            let entry_name = core::str::from_utf8(&raw_name[..end]).unwrap_or("");
            if entry_name == name {
                // Zero out the entry
                for b in &mut block_data[off..off+32] { *b = 0; }
                return write_block(st, blk, &block_data);
            }
        }
    }
    false
}

// ─── FileSystem trait implementation ─────────────────────────────────────

impl FileSystem for AeternaFs {
    fn name(&self) -> &str { "aeternafs" }

    fn read_file(&self, path: &str) -> Option<Vec<u8>> {
        let st = self.state.lock();
        if !st.ready { return None; }

        let ino = lookup_path(&st, path)?;
        let inode = read_inode_raw(&st, ino)?;
        if inode.mode & 0xF000 != MODE_REG { return None; }

        let ss = st.sector_size;
        let size = inode.size as usize;
        let mut result = Vec::with_capacity(size);

        for blk_idx in 0..8 {
            let blk = inode.direct[blk_idx];
            if blk == 0 { break; }
            let block_data = read_block(&st, blk)?;
            let remaining = size.saturating_sub(result.len());
            let take = remaining.min(ss);
            result.extend_from_slice(&block_data[..take]);
            if result.len() >= size { break; }
        }
        Some(result)
    }

    fn write_file(&self, path: &str, data: &[u8]) -> bool {
        let mut st = self.state.lock();
        if !st.ready { return false; }

        // Resolve parent dir and filename
        let (parent_path, filename) = split_path(path);

        // Lookup or create inode for this file
        let ino = match lookup_path(&st, path) {
            Some(ino) => {
                // File exists — free its old blocks
                if let Some(mut inode) = read_inode_raw(&st, ino) {
                    if inode.mode & 0xF000 == MODE_REG {
                        for k in 0..8 {
                            if inode.direct[k] != 0 {
                                free_block(&st, inode.direct[k]);
                                inode.direct[k] = 0;
                            }
                        }
                        inode.size = 0;
                        write_inode_raw(&st, ino, &inode);
                    }
                }
                ino
            }
            None => {
                // File doesn't exist — create it
                let parent_ino = match lookup_path(&st, parent_path) {
                    Some(p) => p,
                    None => return false,
                };
                let new_ino = alloc_inode(&st);
                if new_ino == 0 { return false; }
                let new_inode = Inode {
                    mode: MODE_REG, nlinks: 1, size: 0, mtime: 0,
                    direct: [0u32; 8], crc: 0, _pad: [0u8; 8],
                };
                if !write_inode_raw(&st, new_ino, &new_inode) { return false; }
                if !dir_add_entry(&mut st, parent_ino, filename, new_ino) { return false; }
                new_ino
            }
        };

        // Write data in block-sized chunks
        let ss = st.sector_size;
        let mut written = 0usize;
        let mut inode = match read_inode_raw(&st, ino) { Some(i) => i, None => return false };

        for blk_idx in 0..8 {
            if written >= data.len() { break; }
            let new_blk = alloc_block(&st);
            if new_blk == u32::MAX { return false; }
            inode.direct[blk_idx] = new_blk;
            let chunk_end = (written + ss).min(data.len());
            write_block(&st, new_blk, &data[written..chunk_end]);
            written = chunk_end;
        }
        inode.size = data.len() as u64;
        write_inode_raw(&st, ino, &inode);

        // Update superblock free counts
        flush_superblock(&st);
        true
    }

    fn append_file(&self, path: &str, data: &[u8]) -> bool {
        // Read existing content, append, rewrite
        let existing = self.read_file(path).unwrap_or_default();
        let mut new_data = existing;
        new_data.extend_from_slice(data);
        self.write_file(path, &new_data)
    }

    fn readdir(&self, path: &str) -> Option<Vec<DirEntry>> {
        let st = self.state.lock();
        if !st.ready { return None; }

        let ino = lookup_path(&st, path)?;
        let inode = read_inode_raw(&st, ino)?;
        if inode.mode & 0xF000 != MODE_DIR { return None; }

        let ss = st.sector_size;
        let dirents_per_block = ss / 32;
        let mut entries = Vec::new();

        for blk_idx in 0..8 {
            let blk = inode.direct[blk_idx];
            if blk == 0 { break; }
            let block_data = read_block(&st, blk)?;
            for slot in 0..dirents_per_block {
                let off = slot * 32;
                if off + 32 > block_data.len() { break; }
                let child_ino = u32::from_le_bytes(block_data[off..off+4].try_into().ok()?);
                if child_ino == 0 { continue; }
                let raw_name = &block_data[off+4..off+32];
                let end = raw_name.iter().position(|&b| b == 0).unwrap_or(28);
                let entry_name = core::str::from_utf8(&raw_name[..end]).unwrap_or("?");

                let (node_type, size) = if let Some(child_inode) = read_inode_raw(&st, child_ino) {
                    let nt = if child_inode.mode & 0xF000 == MODE_DIR {
                        NodeType::Directory
                    } else {
                        NodeType::File
                    };
                    (nt, child_inode.size as usize)
                } else {
                    (NodeType::File, 0)
                };

                entries.push(DirEntry {
                    name: String::from(entry_name),
                    node_type,
                    size,
                });
            }
        }
        Some(entries)
    }

    fn mkdir(&self, path: &str) -> bool {
        let mut st = self.state.lock();
        if !st.ready { return false; }

        // Recursively ensure parent exists
        let (parent_path, dirname) = split_path(path);
        if dirname.is_empty() { return true; } // already root

        // Ensure parent directory exists
        let parent_ino = match lookup_path(&st, parent_path) {
            Some(p) => p,
            None => {
                // Drop lock, recurse, re-acquire — avoid deadlock
                drop(st);
                if !self.mkdir(parent_path) { return false; }
                st = self.state.lock();
                match lookup_path(&st, parent_path) {
                    Some(p) => p,
                    None => return false,
                }
            }
        };

        // Check if directory already exists
        if lookup_path(&st, path).is_some() { return true; }

        let new_ino = alloc_inode(&st);
        if new_ino == 0 { return false; }

        // Create directory inode with "." and ".." entries
        let mut new_inode = Inode {
            mode: MODE_DIR, nlinks: 2, size: 0, mtime: 0,
            direct: [0u32; 8], crc: 0, _pad: [0u8; 8],
        };
        if !write_inode_raw(&st, new_ino, &new_inode) { return false; }

        // Add "." and ".." entries
        dir_add_entry(&mut st, new_ino, ".", new_ino);
        dir_add_entry(&mut st, new_ino, "..", parent_ino);

        // Add entry in parent
        dir_add_entry(&mut st, parent_ino, dirname, new_ino);

        flush_superblock(&st);
        true
    }

    fn touch(&self, path: &str) -> bool {
        if self.exists(path) { return true; }
        self.write_file(path, &[])
    }

    fn exists(&self, path: &str) -> bool {
        let st = self.state.lock();
        if !st.ready { return false; }
        lookup_path(&st, path).is_some()
    }

    fn stat(&self, path: &str) -> Option<DirEntry> {
        let st = self.state.lock();
        if !st.ready { return None; }

        let ino = lookup_path(&st, path)?;
        let inode = read_inode_raw(&st, ino)?;
        let name = path.rsplit('/').next().unwrap_or(path);
        Some(DirEntry {
            name: String::from(name),
            node_type: if inode.mode & 0xF000 == MODE_DIR {
                NodeType::Directory
            } else {
                NodeType::File
            },
            size: inode.size as usize,
        })
    }

    fn remove(&self, path: &str) -> bool {
        let mut st = self.state.lock();
        if !st.ready { return false; }

        let ino = match lookup_path(&st, path) { Some(i) => i, None => return false };
        let inode = match read_inode_raw(&st, ino) { Some(i) => i, None => return false };

        // Free data blocks
        let mut inode_mut = inode;
        for k in 0..8 {
            if inode_mut.direct[k] != 0 {
                free_block(&st, inode_mut.direct[k]);
                inode_mut.direct[k] = 0;
            }
        }
        inode_mut.mode = MODE_FREE;
        write_inode_raw(&st, ino, &inode_mut);
        free_inode(&st, ino);

        // Remove from parent directory
        let (parent_path, filename) = split_path(path);
        if let Some(parent_ino) = lookup_path(&st, parent_path) {
            dir_remove_entry(&st, parent_ino, filename);
        }

        flush_superblock(&st);
        true
    }
}

// ─── mkfs: Initialize AeternaFS on a partition ───────────────────────────

/// Format a partition with AeternaFS.
/// `backing` specifies the disk, `part_start` / `part_sectors` the extent.
/// `label` is the partition label (max 64 bytes).
pub fn mkfs(backing: DiskBacking, part_start: u64, part_sectors: u64,
            sector_size: usize, label: &str) -> bool
{
    let s = crate::arch::x86_64::serial::write_str;
    s("[AeternaFS] Formatting... LBA ");
    serial_dec(part_start);
    s("+");
    serial_dec(part_sectors);
    s("\r\n");

    // Build a temporary state for direct I/O
    let mut st = AeternaFsState::new();
    st.backing     = backing;
    st.sector_size = sector_size;
    let data_blocks = (part_sectors.saturating_sub(DATA_START_LBA)) as u32;
    st.total_blocks = data_blocks.min(MAX_BLOCKS as u32);
    st.free_blocks  = st.total_blocks;
    st.total_inodes = MAX_INODES as u32;
    st.free_inodes  = MAX_INODES as u32 - 2; // 0=reserved, 1=root

    // ── 1. Write Superblock ─────────────────────────────────────────────
    let mut sb_sec = vec![0u8; sector_size];
    sb_sec[0..8].copy_from_slice(&AETERNA_MAGIC.to_le_bytes());
    sb_sec[8..12].copy_from_slice(&AETERNA_VERSION.to_le_bytes());
    sb_sec[12..16].copy_from_slice(&0u32.to_le_bytes()); // flags
    sb_sec[16..24].copy_from_slice(&part_sectors.to_le_bytes());
    sb_sec[24..32].copy_from_slice(&INODE_BMAP_LBA.to_le_bytes());
    sb_sec[32..40].copy_from_slice(&BLOCK_BMAP_LBA.to_le_bytes());
    sb_sec[40..48].copy_from_slice(&INODE_TBL_LBA.to_le_bytes());
    sb_sec[48..56].copy_from_slice(&DATA_START_LBA.to_le_bytes());
    sb_sec[56..60].copy_from_slice(&(MAX_INODES as u32).to_le_bytes());
    sb_sec[60..64].copy_from_slice(&st.free_inodes.to_le_bytes());
    sb_sec[64..68].copy_from_slice(&st.total_blocks.to_le_bytes());
    sb_sec[68..72].copy_from_slice(&st.free_blocks.to_le_bytes());
    let lb = label.as_bytes();
    let ll = lb.len().min(63);
    sb_sec[72..72+ll].copy_from_slice(&lb[..ll]);
    if !write_lba(&st, 0, &sb_sec) {
        s("[AeternaFS] Superblock write failed\r\n");
        return false;
    }
    s("[AeternaFS] Superblock written\r\n");

    // ── 2. Zero inode bitmap ─────────────────────────────────────────────
    {
        let zero = vec![0u8; sector_size];
        // Reserve inode 0 and 1
        let mut bmap = vec![0u8; sector_size];
        bitmap_set(&mut bmap, 0, true); // reserved
        bitmap_set(&mut bmap, 1, true); // root
        if !write_lba(&st, INODE_BMAP_LBA, &bmap) {
            s("[AeternaFS] Inode bitmap write failed\r\n");
            return false;
        }
    }

    // ── 3. Zero block bitmap ─────────────────────────────────────────────
    {
        let zero = vec![0u8; sector_size];
        for i in 0..8 {
            if !write_lba(&st, BLOCK_BMAP_LBA + i as u64, &zero) {
                s("[AeternaFS] Block bitmap write failed\r\n");
                return false;
            }
        }
    }

    // ── 4. Zero inode table ──────────────────────────────────────────────
    {
        let zero = vec![0u8; sector_size];
        for i in 0..64 {
            let _ = write_lba(&st, INODE_TBL_LBA + i as u64, &zero);
        }
    }

    // ── 5. Create root directory inode (inode 1) ──────────────────────────
    let root_blk = alloc_block(&st);
    if root_blk == u32::MAX {
        s("[AeternaFS] Cannot allocate root dir block\r\n");
        return false;
    }
    let ss = sector_size;
    let mut root_inode = Inode {
        mode: MODE_DIR, nlinks: 2, size: ss as u64, mtime: 0,
        direct: [0u32; 8], crc: 0, _pad: [0u8; 8],
    };
    root_inode.direct[0] = root_blk;
    if !write_inode_raw(&st, INODE_ROOT, &root_inode) {
        s("[AeternaFS] Root inode write failed\r\n");
        return false;
    }

    // ── 6. Write "." and ".." into root dir block ─────────────────────────
    {
        let mut block_data = vec![0u8; ss];
        // Entry 0: "."  → inode 1
        block_data[0..4].copy_from_slice(&INODE_ROOT.to_le_bytes());
        block_data[4] = b'.';
        // Entry 1: ".." → inode 1 (root has no parent)
        block_data[32..36].copy_from_slice(&INODE_ROOT.to_le_bytes());
        block_data[36] = b'.';
        block_data[37] = b'.';
        if !write_block(&st, root_blk, &block_data) {
            s("[AeternaFS] Root dir block write failed\r\n");
            return false;
        }
    }

    s("[AeternaFS] Format complete — AeternaFS ready\r\n");
    true
}

// ─── Superblock flush helper ──────────────────────────────────────────────

fn flush_superblock(st: &AeternaFsState) {
    let ss = st.sector_size;
    let mut sb_sec = match read_lba_vec(st, 0, ss) {
        Some(b) => b,
        None => return,
    };
    // Update mutable fields
    sb_sec[60..64].copy_from_slice(&st.free_inodes.to_le_bytes());
    sb_sec[68..72].copy_from_slice(&st.free_blocks.to_le_bytes());
    let _ = write_lba(st, 0, &sb_sec);
}

fn read_lba_vec(st: &AeternaFsState, lba: u64, ss: usize) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; ss];
    if read_lba(st, lba, &mut buf) { Some(buf) } else { None }
}

// ─── Path helpers ─────────────────────────────────────────────────────────

/// Split "/foo/bar/baz" into ("/foo/bar", "baz")
fn split_path(path: &str) -> (&str, &str) {
    let path = path.trim_end_matches('/');
    if path.is_empty() || path == "/" { return ("/", ""); }
    match path.rfind('/') {
        Some(0) => ("/", &path[1..]),
        Some(i) => (&path[..i], &path[i+1..]),
        None    => ("/", path),
    }
}

// ─── Static instance ──────────────────────────────────────────────────────

static INSTANCE: AeternaFs = AeternaFs::new();

pub fn instance() -> &'static AeternaFs {
    &INSTANCE
}

/// Mount AeternaFS from a known NVMe partition.
pub fn mount_nvme_partition(part_start: u64, sector_size: usize) -> bool {
    INSTANCE.mount(
        DiskBacking::NvmePartition { part_start },
        sector_size,
    )
}

/// Mount AeternaFS from an AHCI partition.
pub fn mount_ahci_partition(disk_idx: usize, part_start: u64, sector_size: usize) -> bool {
    INSTANCE.mount(
        DiskBacking::AhciPartition { disk_idx, part_start },
        sector_size,
    )
}

// ─── Serial helper ────────────────────────────────────────────────────────

fn serial_dec(mut v: u64) {
    if v == 0 { crate::arch::x86_64::serial::write_byte(b'0'); return; }
    let mut buf = [0u8; 20]; let mut i = 0;
    while v > 0 { buf[i] = b'0' + (v % 10) as u8; v /= 10; i += 1; }
    for j in (0..i).rev() { crate::arch::x86_64::serial::write_byte(buf[j]); }
}
