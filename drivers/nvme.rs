/*
 * AETERNA NVMe Driver — Non-Volatile Memory Express over PCIe
 *
 * BSL 1.1 — Copyright (c) 2026 ospab
 *
 * Implements the NVMe 1.4 specification:
 *   - PCI BAR0 MMIO mapping (Class 01h / Sub 08h / ProgIF 02h)
 *   - Controller reset + configuration (CC, CSTS, AQA, ASQ, ACQ)
 *   - Admin queues (Identify Controller, Create I/O SQ+CQ)
 *   - 32-entry I/O submission + completion queues
 *   - Physical Region Pages (PRP1 / PRP2) for up to 8 KiB DMA
 *   - Identify Namespace → sector count + block size
 *   - NVM Read (0x02) and Write (0x01) commands
 *   - Per-command phase bit tracking for completion polling
 *
 * Memory layout (all static, kernel .bss):
 *   ADMIN_SQ  — 64 × 16 Admin Submission entries (1 KiB)
 *   ADMIN_CQ  — 64 × 16 Admin Completion entries (1 KiB)
 *   IO_SQ     — 32 × 64 I/O Submission entries (2 KiB)
 *   IO_CQ     — 32 × 16 I/O Completion entries (512 B)
 *   IDENTIFY  — 4 KiB Identify buffer
 *   DMA_BUF   — 8 KiB data transfer buffer (up to 16 sectors)
 */

use core::sync::atomic::{AtomicBool, Ordering};

// ─── MMIO register offsets ────────────────────────────────────────────────

const REG_CAP:    usize = 0x00; // 64-bit
const REG_VS:     usize = 0x08; // 32-bit version
const REG_CC:     usize = 0x14; // 32-bit controller config
const REG_CSTS:   usize = 0x1C; // 32-bit controller status
const REG_AQA:    usize = 0x24; // 32-bit admin queue attributes
const REG_ASQ:    usize = 0x28; // 64-bit admin submission queue base
const REG_ACQ:    usize = 0x30; // 64-bit admin completion queue base

// CC bits
const CC_EN:      u32 = 1 << 0;
const CC_CSS_NVM: u32 = 0 << 4; // NVM command set
const CC_MPS_4K:  u32 = 0 << 7; // 4 KiB host pages (MPS=0 → 2^(12+0)=4096)
const CC_AMS_RR:  u32 = 0 << 11; // round-robin arbitration
const CC_SHN_NONE:u32 = 0 << 14;
const CC_IOSQES:  u32 = 6 << 16; // I/O SQ entry size = 2^6 = 64 bytes
const CC_IOCQES:  u32 = 4 << 20; // I/O CQ entry size = 2^4 = 16 bytes

// CSTS bits
const CSTS_RDY:   u32 = 1 << 0;
const CSTS_FATAL: u32 = 1 << 1;

// Admin command opcodes
const ADM_IDENTIFY:       u8 = 0x06;
const ADM_CREATE_IO_SQ:   u8 = 0x01;
const ADM_CREATE_IO_CQ:   u8 = 0x05;
const ADM_DELETE_IO_SQ:   u8 = 0x00;
const ADM_DELETE_IO_CQ:   u8 = 0x04;

// I/O command opcodes
const IO_READ:  u8 = 0x02;
const IO_WRITE: u8 = 0x01;

// Identify CNS values
const CNS_CONTROLLER: u32 = 0x01;
const CNS_NAMESPACE:  u32 = 0x00;

// NVMe namespace ID for the first namespace
const NSID: u32 = 1;

// HHDM base for MMIO access
const HHDM: u64 = 0xFFFF800000000000;

// Queue depths
const ADMIN_Q_DEPTH: usize = 16; // 0..15  (AQA stores depth-1)
const IO_Q_DEPTH:    usize = 32;

// ─── DMA address translation ──────────────────────────────────────────────

#[inline]
fn virt_to_phys(virt: usize) -> u64 {
    // Uses actual kernel load address from Limine (handles KASLR/relocation)
    let offset = crate::arch::x86_64::boot::kernel_virt_offset();
    (virt as u64).wrapping_sub(offset)
}

#[inline]
fn phys_to_virt(phys: u64) -> usize {
    (phys + HHDM) as usize
}

// ─── Submission queue entry (64 bytes, NVMe 1.4 §4.6) ────────────────────

#[repr(C)]
#[derive(Clone, Copy)]
struct SqEntry {
    cdw0:    u32, // Opcode[7:0] | Fused[9:8] | PSDT[15:14] | CID[31:16]
    nsid:    u32,
    cdw2:    u32,
    cdw3:    u32,
    mptr_lo: u32,
    mptr_hi: u32,
    prp1_lo: u32,
    prp1_hi: u32,
    prp2_lo: u32,
    prp2_hi: u32,
    cdw10:   u32,
    cdw11:   u32,
    cdw12:   u32,
    cdw13:   u32,
    cdw14:   u32,
    cdw15:   u32,
}

impl SqEntry {
    const fn zero() -> Self {
        Self { cdw0:0,nsid:0,cdw2:0,cdw3:0,mptr_lo:0,mptr_hi:0,
               prp1_lo:0,prp1_hi:0,prp2_lo:0,prp2_hi:0,
               cdw10:0,cdw11:0,cdw12:0,cdw13:0,cdw14:0,cdw15:0 }
    }

    fn set_opcode_cid(&mut self, op: u8, cid: u16) {
        self.cdw0 = (op as u32) | ((cid as u32) << 16);
    }

    fn set_prp1(&mut self, phys: u64) {
        self.prp1_lo = phys as u32;
        self.prp1_hi = (phys >> 32) as u32;
    }

    fn set_prp2(&mut self, phys: u64) {
        self.prp2_lo = phys as u32;
        self.prp2_hi = (phys >> 32) as u32;
    }
}

// ─── Completion queue entry (16 bytes, NVMe 1.4 §4.6) ────────────────────

#[repr(C)]
#[derive(Clone, Copy)]
struct CqEntry {
    dw0: u32, // command-specific result
    dw1: u32, // reserved
    dw2: u32, // SQ head ptr[15:0] | SQ ID[31:16]
    dw3: u32, // CID[15:0] | Phase bit[16] | Status[31:17]
}

impl CqEntry {
    const fn zero() -> Self { Self { dw0: 0, dw1: 0, dw2: 0, dw3: 0 } }

    fn phase(&self) -> u32 {
        unsafe { core::ptr::read_volatile(&self.dw3) >> 16 & 1 }
    }

    fn status_code(&self) -> u16 {
        // Status field [31:17] — Generic Command Status in bits [14:8]
        let dw3 = unsafe { core::ptr::read_volatile(&self.dw3) };
        ((dw3 >> 17) & 0x7FFF) as u16
    }
}

// ─── Static DMA-capable buffers ───────────────────────────────────────────

#[repr(C, align(4096))]
struct AdminSqBuf([SqEntry; ADMIN_Q_DEPTH]);

#[repr(C, align(4096))]
struct AdminCqBuf([CqEntry; ADMIN_Q_DEPTH]);

#[repr(C, align(4096))]
struct IoSqBuf([SqEntry; IO_Q_DEPTH]);

#[repr(C, align(4096))]
struct IoCqBuf([CqEntry; IO_Q_DEPTH]);

#[repr(C, align(4096))]
struct IdentifyBuf([u8; 4096]);

/// 8 KiB data buffer for up to 16 sectors per command
#[repr(C, align(4096))]
struct DmaBuf([u8; 8192]);

static mut ADMIN_SQ:  AdminSqBuf   = AdminSqBuf([SqEntry::zero(); ADMIN_Q_DEPTH]);
static mut ADMIN_CQ:  AdminCqBuf   = AdminCqBuf([CqEntry::zero(); ADMIN_Q_DEPTH]);
static mut IO_SQ:     IoSqBuf      = IoSqBuf([SqEntry::zero(); IO_Q_DEPTH]);
static mut IO_CQ:     IoCqBuf      = IoCqBuf([CqEntry::zero(); IO_Q_DEPTH]);
static mut IDENTIFY:  IdentifyBuf  = IdentifyBuf([0u8; 4096]);
static mut DMA_BUF:   DmaBuf       = DmaBuf([0u8; 8192]);

// ─── Driver state ─────────────────────────────────────────────────────────

static INITIALIZED: AtomicBool = AtomicBool::new(false);

static mut MMIO_BASE: u64    = 0;  // virtual address of NVMe BAR0
static mut ADMIN_SQ_TAIL: usize = 0;
static mut ADMIN_CQ_HEAD: usize = 0;
static mut IO_SQ_TAIL:    usize = 0;
static mut IO_CQ_HEAD:    usize = 0;
static mut ADMIN_PHASE:   u32  = 1; // expected phase bit for admin CQ
static mut IO_PHASE:      u32  = 1; // expected phase bit for I/O CQ
static mut CMD_ID_SEQ:    u16  = 1; // monotonic command ID counter
static mut DOORBELLS_BASE: u64 = 0; // address of first doorbell register
static mut DSTRD:         u32  = 0; // doorbell stride (from CAP[13:12])

/// Sector size reported by Identify Namespace
static mut LBA_SHIFT: u32 = 9; // default 512 bytes
/// Total sectors on the first namespace
pub static mut SECTOR_COUNT: u64 = 0;
/// Bus/device/function of the found NVMe device
static mut PCI_BUS: u8 = 0;
static mut PCI_DEV: u8 = 0;
static mut PCI_FUN: u8 = 0;

// ─── MMIO helpers ─────────────────────────────────────────────────────────

#[inline(always)]
unsafe fn mmio_r32(off: usize) -> u32 {
    core::ptr::read_volatile((MMIO_BASE as usize + off) as *const u32)
}

#[inline(always)]
unsafe fn mmio_w32(off: usize, val: u32) {
    core::ptr::write_volatile((MMIO_BASE as usize + off) as *mut u32, val);
}

#[inline(always)]
unsafe fn mmio_r64(off: usize) -> u64 {
    let lo = mmio_r32(off) as u64;
    let hi = mmio_r32(off + 4) as u64;
    lo | (hi << 32)
}

#[inline(always)]
unsafe fn mmio_w64(off: usize, val: u64) {
    mmio_w32(off, val as u32);
    mmio_w32(off + 4, (val >> 32) as u32);
}

// ─── Doorbell helpers ─────────────────────────────────────────────────────

// Doorbell register index:
//   Admin SQ = 0, Admin CQ = 1, I/O SQ (QID 1) = 2, I/O CQ (QID 1) = 3
#[inline(always)]
unsafe fn write_doorbell(queue_idx: usize, val: u32) {
    let stride = 4 << DSTRD; // bytes per doorbell register
    let addr = (DOORBELLS_BASE as usize) + queue_idx * stride;
    core::ptr::write_volatile(addr as *mut u32, val);
}

// ─── Delay helper (uses PIT timer ticks) ────────────────────────────────

fn delay_ms(ms: u64) {
    let start = crate::arch::x86_64::idt::timer_ticks();
    // At 100 Hz: 1 tick = 10 ms
    let ticks_needed = (ms + 9) / 10; // round up
    loop {
        if crate::arch::x86_64::idt::timer_ticks().wrapping_sub(start) >= ticks_needed {
            break;
        }
        unsafe { core::arch::asm!("pause"); }
    }
}

// ─── Admin command submission and polling ────────────────────────────────

/// Submit one admin command and wait for completion.
/// Returns Ok(dw0) on success, Err(status) on NVMe error.
unsafe fn admin_submit_poll(cmd: &SqEntry) -> Result<u32, u16> {
    let tail = ADMIN_SQ_TAIL;

    // Copy command into admin SQ
    core::ptr::copy_nonoverlapping(
        cmd as *const SqEntry,
        &mut ADMIN_SQ.0[tail] as *mut SqEntry,
        1,
    );

    // Advance SQ tail
    ADMIN_SQ_TAIL = (tail + 1) % ADMIN_Q_DEPTH;

    // Ring Admin SQ doorbell (index 0)
    write_doorbell(0, ADMIN_SQ_TAIL as u32);

    // Poll CQ for matching completion (phase-bit based)
    let deadline_ticks = crate::arch::x86_64::idt::timer_ticks() + 500; // 5 second timeout
    loop {
        let entry = &ADMIN_CQ.0[ADMIN_CQ_HEAD];
        let phase = entry.phase();
        if phase == ADMIN_PHASE {
            let status = entry.status_code();
            let dw0 = core::ptr::read_volatile(&entry.dw0);

            // Advance CQ head
            ADMIN_CQ_HEAD = (ADMIN_CQ_HEAD + 1) % ADMIN_Q_DEPTH;
            if ADMIN_CQ_HEAD == 0 {
                ADMIN_PHASE ^= 1; // toggle expected phase on wraparound
            }
            // Ring Admin CQ doorbell (index 1)
            write_doorbell(1, ADMIN_CQ_HEAD as u32);

            if status == 0 {
                return Ok(dw0);
            } else {
                return Err(status);
            }
        }
        if crate::arch::x86_64::idt::timer_ticks() >= deadline_ticks {
            crate::arch::x86_64::serial::write_str("[NVMe] Admin command timed out\r\n");
            return Err(0xFFFF);
        }
        core::arch::asm!("pause");
    }
}

/// Submit one I/O command and wait for completion.
unsafe fn io_submit_poll(cmd: &SqEntry) -> Result<u32, u16> {
    let tail = IO_SQ_TAIL;

    core::ptr::copy_nonoverlapping(
        cmd as *const SqEntry,
        &mut IO_SQ.0[tail] as *mut SqEntry,
        1,
    );

    IO_SQ_TAIL = (tail + 1) % IO_Q_DEPTH;
    // I/O SQ doorbell: index 2
    write_doorbell(2, IO_SQ_TAIL as u32);

    let deadline_ticks = crate::arch::x86_64::idt::timer_ticks() + 300; // 3 second
    loop {
        let entry = &IO_CQ.0[IO_CQ_HEAD];
        let phase = entry.phase();
        if phase == IO_PHASE {
            let status = entry.status_code();
            let dw0 = core::ptr::read_volatile(&entry.dw0);

            IO_CQ_HEAD = (IO_CQ_HEAD + 1) % IO_Q_DEPTH;
            if IO_CQ_HEAD == 0 {
                IO_PHASE ^= 1;
            }
            // I/O CQ doorbell: index 3
            write_doorbell(3, IO_CQ_HEAD as u32);

            if status == 0 {
                return Ok(dw0);
            } else {
                return Err(status);
            }
        }
        if crate::arch::x86_64::idt::timer_ticks() >= deadline_ticks {
            crate::arch::x86_64::serial::write_str("[NVMe] I/O command timed out\r\n");
            return Err(0xFFFF);
        }
        core::arch::asm!("pause");
    }
}

// ─── PCI scan for NVMe device ────────────────────────────────────────────

/// Probe PCI bus for NVMe controller (Class 01h, Sub 08h, ProgIF 02h).
/// Returns Some((bus, dev, fun)) if found.
fn pci_find_nvme() -> Option<(u8, u8, u8)> {
    // Fast path: check kernel PCI device table
    for i in 0..64 {
        if let Some(d) = crate::pci::get_device(i) {
            // NVMe: class=01 sub=08 progif=02
            if d.class == 0x01 && d.subclass == 0x08 && d.progif == 0x02 {
                return Some((d.bus, d.device, d.function));
            }
        } else {
            break;
        }
    }
    // Fallback: manual scan — check ALL 8 functions per device
    // (VMware and some PCIe controllers place NVMe on function > 0)
    for bus in 0u8..=255 {
        for dev in 0u8..32 {
            for fun in 0u8..8 {
                let vendor = crate::pci::config_read16(bus, dev, fun, 0x00);
                if vendor == 0xFFFF {
                    // If function 0 doesn't exist, skip the whole device
                    if fun == 0 { break; }
                    continue;
                }
                let class_word = crate::pci::config_read32(bus, dev, fun, 0x08);
                let class  = (class_word >> 24) as u8;
                let sub    = (class_word >> 16) as u8;
                let progif = (class_word >>  8) as u8;
                if class == 0x01 && sub == 0x08 && progif == 0x02 {
                    crate::arch::x86_64::serial::write_str("[NVMe] Found via fallback scan\r\n");
                    return Some((bus, dev, fun));
                }
                // If device is not multi-function, skip functions 1-7
                if fun == 0 {
                    let header_type = (crate::pci::config_read32(bus, dev, 0, 0x0C) >> 16) as u8;
                    if header_type & 0x80 == 0 { break; }
                }
            }
        }
    }
    None
}

// ─── Public init ─────────────────────────────────────────────────────────

/// Initialize the NVMe controller. Call after PCI enumeration.
/// Returns true if an NVMe drive was found and initialized.
pub fn probe_and_init() -> bool {
    let s = crate::arch::x86_64::serial::write_str;
    s("[NVMe] Scanning for NVMe controller...\r\n");

    let (bus, dev, fun) = match pci_find_nvme() {
        Some(x) => x,
        None => {
            s("[NVMe] No NVMe controller found\r\n");
            return false;
        }
    };

    s("[NVMe] Found at PCI ");
    serial_hex_byte(bus); s(":"); serial_hex_byte(dev); s("."); serial_hex_byte(fun);
    s("\r\n");

    unsafe {
        PCI_BUS = bus; PCI_DEV = dev; PCI_FUN = fun;
    }

    // Enable PCI Bus Master + MMIO
    let cmd = crate::pci::config_read16(bus, dev, fun, 0x04);
    crate::pci::config_write16(bus, dev, fun, 0x04, cmd | 0x0006);

    // Read BAR0 (64-bit: BAR0 + BAR1)
    let bar0_lo = crate::pci::config_read32(bus, dev, fun, 0x10);
    let bar0_hi = crate::pci::config_read32(bus, dev, fun, 0x14);
    let phys_bar0 = ((bar0_hi as u64) << 32) | ((bar0_lo & !0xF) as u64);

    if phys_bar0 == 0 {
        s("[NVMe] BAR0 is zero — no MMIO assigned\r\n");
        return false;
    }

    s("[NVMe] BAR0 phys=0x");
    serial_hex64(phys_bar0);
    s("\r\n");

    // Map BAR0 via HHDM
    let mmio_virt = phys_bar0 + HHDM;

    unsafe {
        MMIO_BASE = mmio_virt;

        // Read CAP register to get DSTRD and MPSMIN
        let cap = mmio_r64(REG_CAP);
        DSTRD = ((cap >> 32) & 0xF) as u32; // bits[35:32]
        let mpsmin = (cap >> 48) & 0xF;     // bits[51:48]
        let to_500ms = (cap >> 24) & 0xFF;  // TO: units of 500ms

        s("[NVMe] CAP=0x"); serial_hex64(cap);
        s(" DSTRD="); serial_dec(DSTRD as u64);
        s(" MPSMIN="); serial_dec(mpsmin);
        s(" TO="); serial_dec(to_500ms);
        s("×500ms\r\n");

        DOORBELLS_BASE = mmio_virt + 0x1000;

        // Step 1: Disable controller (CC.EN = 0)
        let cc = mmio_r32(REG_CC);
        if cc & CC_EN != 0 {
            mmio_w32(REG_CC, cc & !CC_EN);
            // Wait for CSTS.RDY = 0
            let deadline = crate::arch::x86_64::idt::timer_ticks()
                + (to_500ms as u64 + 1) * 50; // TO×500ms converted to 10ms ticks
            loop {
                let csts = mmio_r32(REG_CSTS);
                if csts & CSTS_RDY == 0 { break; }
                if crate::arch::x86_64::idt::timer_ticks() >= deadline {
                    s("[NVMe] Controller disable timeout\r\n");
                    return false;
                }
                core::arch::asm!("pause");
            }
        }

        // Step 2: Configure admin queues
        let asq_phys = virt_to_phys(ADMIN_SQ.0.as_ptr() as usize);
        let acq_phys = virt_to_phys(ADMIN_CQ.0.as_ptr() as usize);

        // AQA: Admin SQ size[11:0] | Admin CQ size[27:16] — stored as (depth - 1)
        let aqa = ((ADMIN_Q_DEPTH as u32 - 1) << 16) | (ADMIN_Q_DEPTH as u32 - 1);
        mmio_w32(REG_AQA, aqa);
        mmio_w64(REG_ASQ, asq_phys);
        mmio_w64(REG_ACQ, acq_phys);

        s("[NVMe] ASQ phys=0x"); serial_hex64(asq_phys);
        s(" ACQ phys=0x"); serial_hex64(acq_phys);
        s("\r\n");

        // Step 3: Set CC: MPS=0 (4KiB), CSS=NVM, SQ/CQ entry sizes, then EN=1
        let cc_new = CC_EN | CC_CSS_NVM | CC_MPS_4K | CC_AMS_RR | CC_SHN_NONE
                   | CC_IOSQES | CC_IOCQES;
        mmio_w32(REG_CC, cc_new);

        // Step 4: Wait CSTS.RDY = 1
        let deadline = crate::arch::x86_64::idt::timer_ticks()
            + (to_500ms as u64 + 2) * 50;
        loop {
            let csts = mmio_r32(REG_CSTS);
            if csts & CSTS_FATAL != 0 {
                s("[NVMe] Controller fatal error during init\r\n");
                return false;
            }
            if csts & CSTS_RDY != 0 { break; }
            if crate::arch::x86_64::idt::timer_ticks() >= deadline {
                s("[NVMe] Controller enable timeout\r\n");
                return false;
            }
            core::arch::asm!("pause");
        }
        s("[NVMe] Controller enabled\r\n");

        // Step 5: Identify Controller
        if !identify_controller() { return false; }

        // Step 6: Create I/O queues
        if !create_io_queues() { return false; }

        // Step 7: Identify Namespace 1 → get sector count
        if !identify_namespace() { return false; }

        INITIALIZED.store(true, Ordering::Relaxed);
        s("[NVMe] Init complete — "); serial_dec(SECTOR_COUNT);
        s(" sectors ("); serial_dec(SECTOR_COUNT >> 11); s(" MiB)\r\n");
        true
    }
}

/// Send Identify Controller command, log model number.
unsafe fn identify_controller() -> bool {
    let s = crate::arch::x86_64::serial::write_str;
    let buf_phys = virt_to_phys(IDENTIFY.0.as_ptr() as usize);

    // Zero out identify buffer
    for b in IDENTIFY.0.iter_mut() { *b = 0; }

    let mut cmd = SqEntry::zero();
    cmd.set_opcode_cid(ADM_IDENTIFY, next_cid());
    cmd.nsid = 0;
    cmd.set_prp1(buf_phys);
    cmd.cdw10 = CNS_CONTROLLER;

    match admin_submit_poll(&cmd) {
        Ok(_) => {}
        Err(e) => {
            s("[NVMe] Identify Controller failed, status=0x");
            serial_hex16(e);
            s("\r\n");
            return false;
        }
    }

    // Model number is at bytes 24..63 in identify data (ASCII, space-padded)
    let model = &IDENTIFY.0[24..64];
    s("[NVMe] Controller model: ");
    for &b in model {
        if b >= 0x20 && b < 0x7F { crate::arch::x86_64::serial::write_byte(b); }
    }
    s("\r\n");

    // FW revision at bytes 64..71
    let fw = &IDENTIFY.0[64..72];
    s("[NVMe] FW revision: ");
    for &b in fw {
        if b >= 0x20 && b < 0x7F { crate::arch::x86_64::serial::write_byte(b); }
    }
    s("\r\n");

    true
}

/// Create I/O Completion Queue (QID 1) and I/O Submission Queue (QID 1).
unsafe fn create_io_queues() -> bool {
    let s = crate::arch::x86_64::serial::write_str;

    let io_cq_phys = virt_to_phys(IO_CQ.0.as_ptr() as usize);
    let io_sq_phys = virt_to_phys(IO_SQ.0.as_ptr() as usize);

    // Create I/O CQ first
    {
        let mut cmd = SqEntry::zero();
        cmd.set_opcode_cid(ADM_CREATE_IO_CQ, next_cid());
        cmd.set_prp1(io_cq_phys);
        // CDW10: Queue Size[31:16] | QID[15:0]  (size stored as N-1)
        cmd.cdw10 = ((IO_Q_DEPTH as u32 - 1) << 16) | 1;
        // CDW11: IEN[1] | PC[0]  — interrupts disabled, physically contiguous
        cmd.cdw11 = 0x01; // PC=1 (physically contiguous)

        match admin_submit_poll(&cmd) {
            Ok(_) => s("[NVMe] I/O CQ created\r\n"),
            Err(e) => {
                s("[NVMe] Create I/O CQ failed, status=0x");
                serial_hex16(e);
                s("\r\n");
                return false;
            }
        }
    }

    // Create I/O SQ
    {
        let mut cmd = SqEntry::zero();
        cmd.set_opcode_cid(ADM_CREATE_IO_SQ, next_cid());
        cmd.set_prp1(io_sq_phys);
        // CDW10: QSize[31:16] | QID[15:0]
        cmd.cdw10 = ((IO_Q_DEPTH as u32 - 1) << 16) | 1;
        // CDW11: CQID[31:16] | Priority[2:1] | PC[0]
        cmd.cdw11 = (1 << 16) | 0x01; // CQID=1, PC=1

        match admin_submit_poll(&cmd) {
            Ok(_) => s("[NVMe] I/O SQ created\r\n"),
            Err(e) => {
                s("[NVMe] Create I/O SQ failed, status=0x");
                serial_hex16(e);
                s("\r\n");
                return false;
            }
        }
    }

    true
}

/// Identify Namespace 1 → extract NSZE (total sectors) and LBAF (sector size).
unsafe fn identify_namespace() -> bool {
    let s = crate::arch::x86_64::serial::write_str;

    for b in IDENTIFY.0.iter_mut() { *b = 0; }
    let buf_phys = virt_to_phys(IDENTIFY.0.as_ptr() as usize);

    let mut cmd = SqEntry::zero();
    cmd.set_opcode_cid(ADM_IDENTIFY, next_cid());
    cmd.nsid = NSID;
    cmd.set_prp1(buf_phys);
    cmd.cdw10 = CNS_NAMESPACE;

    match admin_submit_poll(&cmd) {
        Ok(_) => {}
        Err(e) => {
            s("[NVMe] Identify Namespace failed, status=0x");
            serial_hex16(e);
            s("\r\n");
            return false;
        }
    }

    // NSZE (Namespace Size) at bytes 0..7
    let nsze = u64::from_le_bytes(IDENTIFY.0[0..8].try_into().unwrap_or([0u8; 8]));
    // FLBAS at byte 26: bits[3:0] = currently active LBA format index
    let flbas = IDENTIFY.0[26];
    let lba_fmt_idx = (flbas & 0x0F) as usize;
    // LBA Format descriptors start at byte 128, each 4 bytes
    let lbaf_off = 128 + lba_fmt_idx * 4;
    // Byte 0-1: Metadata Size (MS); Byte 2: LBADS (log2 sector size); Byte 3: RP
    let lbads = if lbaf_off + 3 < 4096 { IDENTIFY.0[lbaf_off + 2] } else { 9 };
    // LBADS is log2 of sector size (9 = 512B, 12 = 4096B)
    LBA_SHIFT = lbads as u32;
    SECTOR_COUNT = nsze;

    s("[NVMe] NS1: ");
    serial_dec(nsze);
    s(" sectors × ");
    serial_dec(1u64 << lbads);
    s(" bytes/sector\r\n");

    true
}

// ─── Utility ──────────────────────────────────────────────────────────────

unsafe fn next_cid() -> u16 {
    let id = CMD_ID_SEQ;
    CMD_ID_SEQ = CMD_ID_SEQ.wrapping_add(1).max(1);
    id
}

// ─── Public I/O ───────────────────────────────────────────────────────────

pub fn is_initialized() -> bool {
    INITIALIZED.load(Ordering::Relaxed)
}

pub fn sector_count() -> u64 {
    unsafe { SECTOR_COUNT }
}

pub fn sector_size() -> usize {
    unsafe { 1 << LBA_SHIFT }
}

/// Read `count` sectors starting at `lba` into `buf`.
/// buf must be at least count × sector_size() bytes.
/// Returns true on success.
pub fn read_sectors(lba: u64, count: u32, buf: &mut [u8]) -> bool {
    if !is_initialized() { return false; }
    let ss = sector_size();
    let bytes = count as usize * ss;
    if buf.len() < bytes { return false; }

    // Maximum per-command: limited by DMA_BUF size (8 KiB = up to 8 sectors for 1K or 4 for 4K)
    let max_per_cmd = 8192 / ss;
    let mut done = 0usize;
    let mut cur_lba = lba;

    while done < count as usize {
        let batch = (count as usize - done).min(max_per_cmd);
        let batch_bytes = batch * ss;

        if !unsafe { read_one_batch(cur_lba, batch as u32, &mut buf[done * ss..(done + batch) * ss]) } {
            return false;
        }

        done += batch;
        cur_lba += batch as u64;
    }
    true
}

/// Send an NVMe Flush command (I/O opcode 0x00) to commit any volatile write
/// cache to non-volatile storage.  Call after bulk writes to ensure UEFI can
/// read back consistent data on the next boot.
/// Returns true on success or if the controller is not yet initialised.
pub fn flush() -> bool {
    if !is_initialized() { return true; }
    unsafe {
        let mut cmd = SqEntry::zero();
        // NVMe 1.4 §6.8 — Flush command, I/O opcode 0x00, NSID 1
        // No PRPs required — the controller ignores them for Flush.
        cmd.set_opcode_cid(0x00, next_cid());
        cmd.nsid = NSID;
        match io_submit_poll(&cmd) {
            Ok(_) => {
                crate::arch::x86_64::serial::write_str("[NVMe] Flush OK\r\n");
                true
            }
            Err(e) => {
                crate::arch::x86_64::serial::write_str("[NVMe] Flush error status=0x");
                serial_hex16(e);
                crate::arch::x86_64::serial::write_str("\r\n");
                false   // non-fatal — data was written, may not be persisted
            }
        }
    }
}

/// Write `count` sectors starting at `lba` from `buf`.
pub fn write_sectors(lba: u64, count: u32, buf: &[u8]) -> bool {
    if !is_initialized() { return false; }
    let ss = sector_size();
    let bytes = count as usize * ss;
    if buf.len() < bytes { return false; }

    let max_per_cmd = 8192 / ss;
    let mut done = 0usize;
    let mut cur_lba = lba;

    while done < count as usize {
        let batch = (count as usize - done).min(max_per_cmd);
        let batch_bytes = batch * ss;

        if !unsafe { write_one_batch(cur_lba, batch as u32, &buf[done * ss..(done + batch) * ss]) } {
            return false;
        }

        done += batch;
        cur_lba += batch as u64;
    }
    true
}

unsafe fn read_one_batch(lba: u64, count: u32, out: &mut [u8]) -> bool {
    let ss = sector_size();
    let bytes = count as usize * ss;

    let dma_phys = virt_to_phys(DMA_BUF.0.as_ptr() as usize);
    let dma_phys2 = dma_phys + 4096; // PRP2 for second 4KiB page

    let mut cmd = SqEntry::zero();
    cmd.set_opcode_cid(IO_READ, next_cid());
    cmd.nsid = NSID;
    cmd.set_prp1(dma_phys);
    if bytes > 4096 { cmd.set_prp2(dma_phys2); }
    // CDW10: Starting LBA low 32 bits
    cmd.cdw10 = lba as u32;
    // CDW11: Starting LBA high 32 bits
    cmd.cdw11 = (lba >> 32) as u32;
    // CDW12: NLB[15:0] = 0-based number of logical blocks (count - 1)
    cmd.cdw12 = count - 1;

    match io_submit_poll(&cmd) {
        Ok(_) => {
            out.copy_from_slice(&DMA_BUF.0[..bytes]);
            true
        }
        Err(e) => {
            crate::arch::x86_64::serial::write_str("[NVMe] Read error status=0x");
            serial_hex16(e);
            crate::arch::x86_64::serial::write_str("\r\n");
            false
        }
    }
}

unsafe fn write_one_batch(lba: u64, count: u32, data: &[u8]) -> bool {
    let ss = sector_size();
    let bytes = count as usize * ss;

    let dma_phys = virt_to_phys(DMA_BUF.0.as_ptr() as usize);
    let dma_phys2 = dma_phys + 4096;

    DMA_BUF.0[..bytes].copy_from_slice(data);

    let mut cmd = SqEntry::zero();
    cmd.set_opcode_cid(IO_WRITE, next_cid());
    cmd.nsid = NSID;
    cmd.set_prp1(dma_phys);
    if bytes > 4096 { cmd.set_prp2(dma_phys2); }
    cmd.cdw10 = lba as u32;
    cmd.cdw11 = (lba >> 32) as u32;
    cmd.cdw12 = count - 1;

    match io_submit_poll(&cmd) {
        Ok(_) => true,
        Err(e) => {
            crate::arch::x86_64::serial::write_str("[NVMe] Write error status=0x");
            serial_hex16(e);
            crate::arch::x86_64::serial::write_str("\r\n");
            false
        }
    }
}

// ─── Serial debug helpers ─────────────────────────────────────────────────

fn serial_hex_byte(v: u8) {
    let h = b"0123456789abcdef";
    crate::arch::x86_64::serial::write_byte(h[(v >> 4) as usize]);
    crate::arch::x86_64::serial::write_byte(h[(v & 0xF) as usize]);
}
fn serial_hex16(v: u16) {
    serial_hex_byte((v >> 8) as u8); serial_hex_byte(v as u8);
}
fn serial_hex64(v: u64) {
    for i in (0..8).rev() { serial_hex_byte((v >> (i * 8)) as u8); }
}
fn serial_dec(mut v: u64) {
    if v == 0 { crate::arch::x86_64::serial::write_byte(b'0'); return; }
    let mut buf = [0u8; 20]; let mut i = 0;
    while v > 0 { buf[i] = b'0' + (v % 10) as u8; v /= 10; i += 1; }
    for j in (0..i).rev() { crate::arch::x86_64::serial::write_byte(buf[j]); }
}
