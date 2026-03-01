/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab

Virtual Memory Manager (VMM) for AETERNA microkernel.
Manages x86_64 4-level page tables (PML4 → PDPT → PD → PT).

Design decisions:
 - Limine already enables paging and maps the kernel + HHDM.
   We do NOT replace Limine's page tables at boot.
 - Instead, we expose an API to create NEW page tables (for user processes)
   and to map/unmap/translate addresses in any address space.
 - For kernel mappings, we walk/modify the CURRENT CR3 page table.
 - All page table frames are allocated from the physical memory manager.
 - We access physical memory through the HHDM (Higher Half Direct Map)
   at offset 0xFFFF_8000_0000_0000 provided by Limine.
*/

use core::arch::asm;
use core::sync::atomic::{AtomicU64, AtomicBool, Ordering};

// ─── Constants ──────────────────────────────────────────────────────────────

/// Page size: 4 KiB
pub const PAGE_SIZE: u64 = 4096;

/// Number of entries per page table level
pub const ENTRIES_PER_TABLE: usize = 512;

/// HHDM offset — set once from Limine at init()
static HHDM: AtomicU64 = AtomicU64::new(0);

/// VMM initialized flag
static VMM_INIT: AtomicBool = AtomicBool::new(false);

// ─── Page table entry flags (x86_64) ────────────────────────────────────────

/// Page is present in memory
pub const FLAG_PRESENT:   u64 = 1 << 0;
/// Page is writable (read/write)
pub const FLAG_WRITABLE:  u64 = 1 << 1;
/// Page is accessible from user mode (ring 3)
pub const FLAG_USER:      u64 = 1 << 2;
/// Write-through caching
pub const FLAG_PWT:       u64 = 1 << 3;
/// Cache disabled
pub const FLAG_PCD:       u64 = 1 << 4;
/// Page has been accessed
pub const FLAG_ACCESSED:  u64 = 1 << 5;
/// Page has been written to (dirty)
pub const FLAG_DIRTY:     u64 = 1 << 6;
/// Huge page (2 MiB at PD level, 1 GiB at PDPT level)
pub const FLAG_HUGE:      u64 = 1 << 7;
/// Global — TLB entry not flushed on CR3 switch
pub const FLAG_GLOBAL:    u64 = 1 << 8;
/// No-execute: page cannot contain executable code (requires NXE in EFER)
pub const FLAG_NX:        u64 = 1 << 63;

/// Mask to extract the physical address from a page table entry
/// Bits 12..51 hold the 4K-aligned physical frame address
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

// ─── Page Table Structures ──────────────────────────────────────────────────

/// A single page table entry (PTE). Identical at all 4 levels.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct PageTableEntry(pub u64);

impl PageTableEntry {
    /// Create an empty (not present) entry
    #[inline]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Check if the entry is present
    #[inline]
    pub fn is_present(&self) -> bool {
        self.0 & FLAG_PRESENT != 0
    }

    /// Check if the entry maps a huge page
    #[inline]
    pub fn is_huge(&self) -> bool {
        self.0 & FLAG_HUGE != 0
    }

    /// Get the physical address stored in this entry
    #[inline]
    pub fn addr(&self) -> u64 {
        self.0 & ADDR_MASK
    }

    /// Get the flags stored in this entry
    #[inline]
    pub fn flags(&self) -> u64 {
        self.0 & !ADDR_MASK
    }

    /// Set the entry to point to a physical address with given flags
    #[inline]
    pub fn set(&mut self, phys_addr: u64, flags: u64) {
        self.0 = (phys_addr & ADDR_MASK) | flags;
    }

    /// Clear the entry (mark not present)
    #[inline]
    pub fn clear(&mut self) {
        self.0 = 0;
    }
}

/// A page table: 512 entries, page-aligned (4 KiB total).
/// Used at all 4 levels: PML4, PDPT, PD, PT.
#[repr(C, align(4096))]
pub struct PageTable {
    pub entries: [PageTableEntry; ENTRIES_PER_TABLE],
}

impl PageTable {
    /// Create a new empty page table (all entries not present)
    pub const fn new() -> Self {
        Self {
            entries: [PageTableEntry::empty(); ENTRIES_PER_TABLE],
        }
    }

    /// Zero out all entries
    pub fn zero(&mut self) {
        for e in self.entries.iter_mut() {
            e.clear();
        }
    }
}

// ─── Index extraction from virtual address ──────────────────────────────────

/// Extract PML4 index (bits 39..47) from a virtual address
#[inline]
pub fn pml4_index(virt: u64) -> usize {
    ((virt >> 39) & 0x1FF) as usize
}

/// Extract PDPT index (bits 30..38)
#[inline]
pub fn pdpt_index(virt: u64) -> usize {
    ((virt >> 30) & 0x1FF) as usize
}

/// Extract PD index (bits 21..29)
#[inline]
pub fn pd_index(virt: u64) -> usize {
    ((virt >> 21) & 0x1FF) as usize
}

/// Extract PT index (bits 12..20)
#[inline]
pub fn pt_index(virt: u64) -> usize {
    ((virt >> 12) & 0x1FF) as usize
}

/// Extract page offset (bits 0..11)
#[inline]
pub fn page_offset(virt: u64) -> u64 {
    virt & 0xFFF
}

// ─── HHDM helpers ───────────────────────────────────────────────────────────

/// Convert a physical address to a virtual address via HHDM
#[inline]
pub fn phys_to_virt(phys: u64) -> u64 {
    phys + HHDM.load(Ordering::Relaxed)
}

/// Convert a virtual HHDM address back to physical
#[inline]
pub fn virt_to_phys_hhdm(virt: u64) -> u64 {
    virt - HHDM.load(Ordering::Relaxed)
}

/// Get a mutable reference to a page table at a given physical address
/// by mapping it through the HHDM.
#[inline]
unsafe fn table_at_phys(phys: u64) -> &'static mut PageTable {
    let virt = phys_to_virt(phys);
    &mut *(virt as *mut PageTable)
}

// ─── CR3 operations ─────────────────────────────────────────────────────────

/// Read the current PML4 physical address from CR3
pub fn read_cr3() -> u64 {
    let cr3: u64;
    unsafe { asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack)); }
    cr3 & ADDR_MASK
}

/// Write a new PML4 physical address to CR3, switching address spaces.
/// This flushes the TLB for all non-global pages.
///
/// # Safety
/// The caller must ensure the new PML4 maps at least:
///  - The kernel code/data/bss (higher half)
///  - The HHDM
///  - The framebuffer
///  - The stack
/// Otherwise a triple fault will occur immediately.
pub unsafe fn write_cr3(pml4_phys: u64) {
    asm!("mov cr3, {}", in(reg) pml4_phys, options(nostack));
}

/// Flush a single TLB entry for the given virtual address
pub fn invlpg(virt: u64) {
    unsafe {
        asm!("invlpg [{}]", in(reg) virt, options(nostack));
    }
}

// ─── Initialization ─────────────────────────────────────────────────────────

/// Initialize the VMM. Call after physical memory manager and heap are ready.
/// Reads the HHDM offset from Limine boot data.
pub fn init() {
    let hhdm = crate::arch::x86_64::boot::hhdm_offset().unwrap_or(0xFFFF_8000_0000_0000);
    HHDM.store(hhdm, Ordering::Relaxed);
    VMM_INIT.store(true, Ordering::Relaxed);

    let cr3 = read_cr3();
    crate::arch::x86_64::serial::write_str("[VMM] Initialized. CR3=0x");
    crate::arch::x86_64::init::serial_hex(cr3);
    crate::arch::x86_64::serial::write_str(" HHDM=0x");
    crate::arch::x86_64::init::serial_hex(hhdm);
    crate::arch::x86_64::serial::write_str("\r\n");
}

/// Check if VMM is initialized
pub fn is_initialized() -> bool {
    VMM_INIT.load(Ordering::Relaxed)
}

// ─── Allocate a zeroed page table frame ─────────────────────────────────────

/// Allocate a new physical 4K frame for a page table, zero it, return phys addr.
/// Returns None if out of memory.
fn alloc_table_frame() -> Option<u64> {
    let frame = crate::mm::physical::alloc_frame()?;
    // Zero the frame via HHDM
    unsafe {
        let ptr = phys_to_virt(frame) as *mut u8;
        core::ptr::write_bytes(ptr, 0, PAGE_SIZE as usize);
    }
    Some(frame)
}

// ─── Core mapping functions ─────────────────────────────────────────────────

/// Map a single 4K virtual page to a physical frame in a given address space.
///
/// `pml4_phys` — physical address of the PML4 table.
/// `virt`     — virtual address to map (4K-aligned).
/// `phys`     — physical address to map to (4K-aligned).
/// `flags`    — page flags (FLAG_PRESENT is always set automatically).
///
/// Missing intermediate tables (PDPT, PD, PT) are allocated automatically.
/// Returns true on success, false if a table frame could not be allocated.
pub fn map_page(pml4_phys: u64, virt: u64, phys: u64, flags: u64) -> bool {
    let flags = flags | FLAG_PRESENT;

    let i4 = pml4_index(virt);
    let i3 = pdpt_index(virt);
    let i2 = pd_index(virt);
    let i1 = pt_index(virt);

    unsafe {
        // Level 4: PML4
        let pml4 = table_at_phys(pml4_phys);
        if !pml4.entries[i4].is_present() {
            match alloc_table_frame() {
                Some(f) => pml4.entries[i4].set(f, FLAG_PRESENT | FLAG_WRITABLE | FLAG_USER),
                None => return false,
            }
        }
        let pdpt_phys = pml4.entries[i4].addr();

        // Level 3: PDPT
        let pdpt = table_at_phys(pdpt_phys);
        if !pdpt.entries[i3].is_present() {
            match alloc_table_frame() {
                Some(f) => pdpt.entries[i3].set(f, FLAG_PRESENT | FLAG_WRITABLE | FLAG_USER),
                None => return false,
            }
        }
        // If this is a 1 GiB huge page, we cannot descend further
        if pdpt.entries[i3].is_huge() {
            return false;
        }
        let pd_phys = pdpt.entries[i3].addr();

        // Level 2: PD (Page Directory)
        let pd = table_at_phys(pd_phys);
        if !pd.entries[i2].is_present() {
            match alloc_table_frame() {
                Some(f) => pd.entries[i2].set(f, FLAG_PRESENT | FLAG_WRITABLE | FLAG_USER),
                None => return false,
            }
        }
        // If this is a 2 MiB huge page, we cannot descend further
        if pd.entries[i2].is_huge() {
            return false;
        }
        let pt_phys = pd.entries[i2].addr();

        // Level 1: PT (Page Table)
        let pt = table_at_phys(pt_phys);
        pt.entries[i1].set(phys, flags);
    }

    true
}

/// Map a single 4K page in the CURRENT address space (CR3).
pub fn map_page_current(virt: u64, phys: u64, flags: u64) -> bool {
    let pml4 = read_cr3();
    let ok = map_page(pml4, virt, phys, flags);
    if ok {
        invlpg(virt);
    }
    ok
}

/// Unmap a single 4K virtual page in the given address space.
/// Returns the physical address that was mapped there, or None if not mapped.
pub fn unmap_page(pml4_phys: u64, virt: u64) -> Option<u64> {
    let i4 = pml4_index(virt);
    let i3 = pdpt_index(virt);
    let i2 = pd_index(virt);
    let i1 = pt_index(virt);

    unsafe {
        let pml4 = table_at_phys(pml4_phys);
        if !pml4.entries[i4].is_present() { return None; }
        let pdpt_phys = pml4.entries[i4].addr();

        let pdpt = table_at_phys(pdpt_phys);
        if !pdpt.entries[i3].is_present() || pdpt.entries[i3].is_huge() { return None; }
        let pd_phys = pdpt.entries[i3].addr();

        let pd = table_at_phys(pd_phys);
        if !pd.entries[i2].is_present() || pd.entries[i2].is_huge() { return None; }
        let pt_phys = pd.entries[i2].addr();

        let pt = table_at_phys(pt_phys);
        if !pt.entries[i1].is_present() { return None; }

        let phys = pt.entries[i1].addr();
        pt.entries[i1].clear();
        Some(phys)
    }
}

/// Unmap a page from the current address space and flush TLB.
pub fn unmap_page_current(virt: u64) -> Option<u64> {
    let pml4 = read_cr3();
    let result = unmap_page(pml4, virt);
    if result.is_some() {
        invlpg(virt);
    }
    result
}

/// Translate a virtual address to a physical address by walking page tables.
/// Returns None if the address is not mapped.
///
/// Handles 4K pages, 2 MiB huge pages, and 1 GiB huge pages.
pub fn translate(pml4_phys: u64, virt: u64) -> Option<u64> {
    let i4 = pml4_index(virt);
    let i3 = pdpt_index(virt);
    let i2 = pd_index(virt);
    let i1 = pt_index(virt);

    unsafe {
        let pml4 = table_at_phys(pml4_phys);
        if !pml4.entries[i4].is_present() { return None; }
        let pdpt_phys = pml4.entries[i4].addr();

        let pdpt = table_at_phys(pdpt_phys);
        if !pdpt.entries[i3].is_present() { return None; }
        // 1 GiB huge page
        if pdpt.entries[i3].is_huge() {
            let base = pdpt.entries[i3].addr();
            let offset = virt & 0x3FFF_FFFF; // 30-bit offset within 1 GiB
            return Some(base + offset);
        }
        let pd_phys = pdpt.entries[i3].addr();

        let pd = table_at_phys(pd_phys);
        if !pd.entries[i2].is_present() { return None; }
        // 2 MiB huge page
        if pd.entries[i2].is_huge() {
            let base = pd.entries[i2].addr();
            let offset = virt & 0x1F_FFFF; // 21-bit offset within 2 MiB
            return Some(base + offset);
        }
        let pt_phys = pd.entries[i2].addr();

        let pt = table_at_phys(pt_phys);
        if !pt.entries[i1].is_present() { return None; }
        // 4 KiB page
        Some(pt.entries[i1].addr() + page_offset(virt))
    }
}

/// Translate using the current CR3
pub fn translate_current(virt: u64) -> Option<u64> {
    translate(read_cr3(), virt)
}

/// Map a contiguous range of virtual pages to a contiguous range of physical frames.
/// Both `virt_start` and `phys_start` must be 4K-aligned.
/// `page_count` — number of 4K pages to map.
pub fn map_range(pml4_phys: u64, virt_start: u64, phys_start: u64, page_count: u64, flags: u64) -> bool {
    for i in 0..page_count {
        let v = virt_start + i * PAGE_SIZE;
        let p = phys_start + i * PAGE_SIZE;
        if !map_page(pml4_phys, v, p, flags) {
            return false;
        }
    }
    true
}

/// Map a range in the current address space
pub fn map_range_current(virt_start: u64, phys_start: u64, page_count: u64, flags: u64) -> bool {
    let pml4 = read_cr3();
    for i in 0..page_count {
        let v = virt_start + i * PAGE_SIZE;
        let p = phys_start + i * PAGE_SIZE;
        if !map_page(pml4, v, p, flags) {
            return false;
        }
        invlpg(v);
    }
    true
}

// ─── New address space creation (for future processes) ──────────────────────

/// Create a new, empty PML4 page table.
/// Copies kernel-space entries (upper half: indices 256..511) from the current
/// CR3 so that kernel code/data/stack/HHDM remain accessible.
/// Returns the physical address of the new PML4, or None on OOM.
pub fn create_address_space() -> Option<u64> {
    let new_pml4_phys = alloc_table_frame()?;
    let current_pml4_phys = read_cr3();

    unsafe {
        let current = table_at_phys(current_pml4_phys);
        let new = table_at_phys(new_pml4_phys);

        // Copy kernel-half entries (indices 256..511)
        for i in 256..512 {
            new.entries[i] = current.entries[i];
        }
        // User-half (0..255) is already zeroed from alloc_table_frame
    }

    Some(new_pml4_phys)
}

/// Switch to a different address space by writing CR3.
/// Caller must ensure the target page table maps kernel space correctly.
pub unsafe fn switch_address_space(pml4_phys: u64) {
    write_cr3(pml4_phys);
}

// ─── Diagnostic: count mapped pages ─────────────────────────────────────────

/// Count how many 4K pages are mapped in the lower half (user space)
/// of the given address space.
pub fn count_user_pages(pml4_phys: u64) -> u64 {
    let mut count: u64 = 0;
    unsafe {
        let pml4 = table_at_phys(pml4_phys);
        for i4 in 0..256 {
            if !pml4.entries[i4].is_present() { continue; }
            let pdpt = table_at_phys(pml4.entries[i4].addr());
            for i3 in 0..512 {
                if !pdpt.entries[i3].is_present() { continue; }
                if pdpt.entries[i3].is_huge() { count += 512 * 512; continue; } // 1 GiB
                let pd = table_at_phys(pdpt.entries[i3].addr());
                for i2 in 0..512 {
                    if !pd.entries[i2].is_present() { continue; }
                    if pd.entries[i2].is_huge() { count += 512; continue; } // 2 MiB
                    let pt = table_at_phys(pd.entries[i2].addr());
                    for i1 in 0..512 {
                        if pt.entries[i1].is_present() { count += 1; }
                    }
                }
            }
        }
    }
    count
}
