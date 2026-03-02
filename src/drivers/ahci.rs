/*
 * AHCI (Advanced Host Controller Interface) SATA driver for AETERNA
 *
 * Supports:
 *   Any PCI device class=0x01 (Mass Storage), subclass=0x06 (SATA), prog-if=0x01 (AHCI 1.0)
 *
 * Works with:
 *   QEMU:      -device ich9-ahci  (or -device ahci)
 *   VMware:    SATA controller (AHCI mode)
 *   Bare metal: All modern SATA controllers (Intel AHCI, AMD AHCI)
 *
 * Implements:
 *   - Port detection
 *   - Read/Write using FIS (Frame Information Structure)
 *   - 28-bit and 48-bit LBA
 *   - One command slot per port (simple, no NCQ)
 */#![allow(dead_code)]
use core::ptr;

use crate::mm::r#virtual::{self as vmm, FLAG_NX, FLAG_PCD, FLAG_PWT, FLAG_WRITABLE};

// ─── PCI scan for AHCI ────────────────────────────────────────────────────
const PCI_CLASS_STORAGE: u8  = 0x01;
const PCI_SUBCLASS_SATA: u8  = 0x06;
const PCI_PROGIF_AHCI:   u8  = 0x01;

// ─── AHCI HBA registers (offset from ABAR) ────────────────────────────────
const HBA_CAP:     u32 = 0x00; // Host Capabilities
const HBA_GHC:     u32 = 0x04; // Global Host Control
const HBA_IS:      u32 = 0x08; // Interrupt Status
const HBA_PI:      u32 = 0x0C; // Ports Implemented bitmask
const HBA_VS:      u32 = 0x10; // Version

// Global Host Control bits
const GHC_AE: u32 = 1 << 31; // AHCI Enable
const GHC_HR: u32 = 1 << 0;  // HBA Reset

// Per-port registers (offset = HBA_PORT_BASE + port * 0x80)
const PORT_BASE_OFFSET: u32 = 0x100;
const PORT_SIZE:         u32 = 0x80;

const PORT_CLB:  u32 = 0x00; // Command List Base Address (low)
const PORT_CLBU: u32 = 0x04; // Command List Base Address (high)
const PORT_FB:   u32 = 0x08; // FIS Base Address (low)
const PORT_FBU:  u32 = 0x0C; // FIS Base Address (high)
const PORT_IS:   u32 = 0x10; // Interrupt Status
const PORT_IE:   u32 = 0x14; // Interrupt Enable
const PORT_CMD:  u32 = 0x18; // Command and Status
const PORT_TFD:  u32 = 0x20; // Task File Data
const PORT_SIG:  u32 = 0x24; // Signature
const PORT_SSTS: u32 = 0x28; // SATA Status
const PORT_SERR: u32 = 0x30; // SATA Error
const PORT_CI:   u32 = 0x38; // Command Issue

// PORT_CMD bits
const PORT_CMD_ST:   u32 = 1 << 0;  // Start
const PORT_CMD_FRE:  u32 = 1 << 4;  // FIS Receive Enable
const PORT_CMD_FR:   u32 = 1 << 14; // FIS Receive Running
const PORT_CMD_CR:   u32 = 1 << 15; // Command List Running
const PORT_CMD_ICC_ACTIVE: u32 = 1 << 28;

// PORT_SSTS: device detected & comms established
const SSTS_DET_PRESENT: u32 = 0x3;
const SSTS_IPM_ACTIVE:  u32 = 0x100;

// ATA command codes
const ATA_CMD_READ_DMA_EX:  u8 = 0x25; // 48-bit LBA read DMA
const ATA_CMD_WRITE_DMA_EX: u8 = 0x35; // 48-bit LBA write DMA
const ATA_CMD_IDENTIFY:     u8 = 0xEC;

// FIS types
const FIS_TYPE_REG_H2D: u8 = 0x27; // Host to Device Register FIS
const FIS_TYPE_REG_D2H: u8 = 0x34; // Device to Host Register FIS
const FIS_TYPE_DMA_ACT: u8 = 0x39;
const FIS_TYPE_DMA_SETUP: u8 = 0x41;
const FIS_TYPE_DATA: u8 = 0x46;
const FIS_TYPE_BIST: u8 = 0x58;
const FIS_TYPE_PIO_SETUP: u8 = 0x5F;
const FIS_TYPE_DEV_BITS: u8 = 0xA1;

const SECTOR_SIZE: usize = 512;
const MAX_PORTS: usize = 32;

// Map AHCI MMIO as uncacheable to avoid register writes being cached
const AHCI_UC_VIRT_BASE: u64 = 0xFFFF_FF10_0000_0000;
const AHCI_MMIO_PAGES: u64 = 2; // 8 KiB covers HBA + ports

// ─── FIS structures ────────────────────────────────────────────────────────
#[repr(C, packed)]
#[derive(Copy, Clone, Default)]
struct FisRegH2D {
    fis_type: u8,  // FIS_TYPE_REG_H2D
    flags:    u8,  // bit 7 = 1 for command, 0 for control
    command:  u8,
    feature_lo: u8,
    lba0: u8, lba1: u8, lba2: u8, device: u8,
    lba3: u8, lba4: u8, lba5: u8, feature_hi: u8,
    count_lo: u8, count_hi: u8,
    icc: u8, control: u8,
    _rsv: [u8; 4],
}

#[repr(C, packed)]
#[derive(Copy, Clone, Default)]
struct FisRegD2H {
    fis_type: u8,
    flags: u8,
    status: u8, error: u8,
    lba0: u8, lba1: u8, lba2: u8, device: u8,
    lba3: u8, lba4: u8, lba5: u8, _rsv0: u8,
    count_lo: u8, count_hi: u8,
    _rsv1: [u8; 6],
}

// ─── Command List Entry (CMDHDR) — 32 bytes ─────────────────────────────────
#[repr(C, packed)]
#[derive(Copy, Clone, Default)]
struct CmdHeader {
    flags:      u16, // bits [4:0]=CFL (cmd FIS length/4), bit 6=write
    prdtl:      u16, // Physical Region Descriptor Table Length (# entries)
    prdbc:      u32, // PRD byte count
    ctba_lo:    u32, // Command Table Base Address low
    ctba_hi:    u32, // Command Table Base Address high
    _rsv:       [u32; 4],
}

// ─── Physical Region Descriptor Entry — 16 bytes ─────────────────────────
#[repr(C, packed)]
#[derive(Copy, Clone, Default)]
struct PrdEntry {
    dba_lo: u32, // Data Base Address low
    dba_hi: u32, // Data Base Address high
    _rsv:   u32,
    dbc:    u32, // Byte count minus 1, bit 31 = interrupt on completion
}

// ─── Command Table ─────────────────────────────────────────────────────────
// cfis: 64 bytes (FIS), acmd: 16 bytes (ATAPI), _rsv: 48 bytes, prdt: N entries
#[repr(C, packed)]
#[derive(Copy, Clone)]
struct CmdTable {
    cfis:   [u8; 64],
    acmd:   [u8; 16],
    _rsv:   [u8; 48],
    prdt:   [PrdEntry; 1], // We use 1 PRD entry per command
}

// ─── FIS receive area (256 bytes minimum) ────────────────────────────────
#[repr(C, align(256))]
#[derive(Copy, Clone)]
struct FisArea([u8; 256]);

// ─── Per-port DMA memory ──────────────────────────────────────────────────
// Each port needs: 1 CmdHeader (1K aligned, 32 bytes) + 1 CmdTable (128-byte aligned)
// We allocate per-port data buffers too

#[repr(C, align(1024))]
#[derive(Copy, Clone)]
struct PortCmdList([CmdHeader; 32]); // 32 slots, 1K aligned

const MAX_ACTIVE_PORTS: usize = 4;

static mut CMD_LISTS: [PortCmdList; MAX_ACTIVE_PORTS] = [PortCmdList([CmdHeader {
    flags: 0, prdtl: 0, prdbc: 0, ctba_lo: 0, ctba_hi: 0, _rsv: [0; 4]
}; 32]); MAX_ACTIVE_PORTS];

static mut FIS_AREAS: [FisArea; MAX_ACTIVE_PORTS] = [FisArea([0u8; 256]); MAX_ACTIVE_PORTS];

static mut CMD_TABLES: [CmdTable; MAX_ACTIVE_PORTS] = [CmdTable {
    cfis: [0u8; 64], acmd: [0u8; 16], _rsv: [0u8; 48],
    prdt: [PrdEntry { dba_lo: 0, dba_hi: 0, _rsv: 0, dbc: 0 }]
}; MAX_ACTIVE_PORTS];

// Data transfer buffer: 128K (enough for 256 sectors at once)
const DATA_BUF_SIZE: usize = 131072;
static mut DATA_BUF: [u8; DATA_BUF_SIZE] = [0u8; DATA_BUF_SIZE];

// ─── Global state ─────────────────────────────────────────────────────────
#[derive(Copy, Clone)]
pub struct AhciDrive {
    pub present:   bool,
    pub port_idx:  u8,   // HBA port index (0-31)
    pub alloc_idx: u8,   // Our alloc index (0-3) into the static arrays above
    pub sectors:   u64,  // Total sectors (48-bit LBA)
    pub size_mb:   u64,
    pub model:     [u8; 41],
}

static mut DRIVES: [AhciDrive; MAX_ACTIVE_PORTS] = [AhciDrive {
    present: false, port_idx: 0, alloc_idx: 0, sectors: 0, size_mb: 0,
    model: [0u8; 41]
}; MAX_ACTIVE_PORTS];

static mut DRIVE_COUNT: usize = 0;
static mut HBA_VIRT: usize = 0; // Virtual address of ABAR (via HHDM)
static mut INITIALIZED: bool = false;

// ─── MMIO helpers ──────────────────────────────────────────────────────────
fn hba_read(off: u32) -> u32 {
    let addr = unsafe { HBA_VIRT + off as usize };
    unsafe { ptr::read_volatile(addr as *const u32) }
}
fn hba_write(off: u32, val: u32) {
    let addr = unsafe { HBA_VIRT + off as usize };
    unsafe { ptr::write_volatile(addr as *mut u32, val); }
}
fn port_read(port: u32, reg: u32) -> u32 {
    hba_read(PORT_BASE_OFFSET + port * PORT_SIZE + reg)
}
fn port_write(port: u32, reg: u32, val: u32) {
    hba_write(PORT_BASE_OFFSET + port * PORT_SIZE + reg, val)
}

// ─── Utility ──────────────────────────────────────────────────────────────
fn virt_to_phys(vaddr: usize) -> u64 {
    // Kernel virtual offset = KERNEL_VIRT - KERNEL_PHYS = 0xffffffff80000000
    (vaddr as u64).wrapping_sub(0xffff_ffff_8000_0000_u64)
}
fn phys_to_virt(phys: u64) -> usize {
    let hhdm = crate::arch::x86_64::boot::hhdm_offset().unwrap_or(0xFFFF_8000_0000_0000);
    (phys + hhdm) as usize
}

fn virt_to_phys_hhdm(vaddr: usize) -> u64 {
    // Convert a higher-half direct mapping (HHDM) pointer back to physical
    let hhdm = crate::arch::x86_64::boot::hhdm_offset().unwrap_or(0xFFFF_8000_0000_0000);
    (vaddr as u64).wrapping_sub(hhdm)
}

fn delay(n: u32) {
    for _ in 0..n { unsafe { core::arch::asm!("pause"); } }
}

// ─── PCI scan ─────────────────────────────────────────────────────────────
fn pci_read(bus: u8, dev: u8, func: u8, off: u8) -> u32 {
    let addr: u32 = 0x80000000 | ((bus as u32) << 16) | ((dev as u32) << 11)
        | ((func as u32) << 8) | ((off as u32) & 0xFC);
    unsafe {
        let v: u32;
        core::arch::asm!("out dx, eax", in("dx") 0x0CF8u16, in("eax") addr, options(nomem, nostack));
        core::arch::asm!("in eax, dx",  in("dx") 0x0CFCu16, out("eax") v, options(nomem, nostack));
        v
    }
}
fn pci_write(bus: u8, dev: u8, func: u8, off: u8, val: u32) {
    let addr: u32 = 0x80000000 | ((bus as u32) << 16) | ((dev as u32) << 11)
        | ((func as u32) << 8) | ((off as u32) & 0xFC);
    unsafe {
        core::arch::asm!("out dx, eax", in("dx") 0x0CF8u16, in("eax") addr, options(nomem, nostack));
        core::arch::asm!("out dx, eax", in("dx") 0x0CFCu16, in("eax") val, options(nomem, nostack));
    }
}

pub fn init() -> usize {
    for bus in 0u8..16 {
        for dev in 0u8..32 {
            let id = pci_read(bus, dev, 0, 0);
            if id == 0xFFFFFFFF { continue; }
            let class_info = pci_read(bus, dev, 0, 0x08);
            let class    = ((class_info >> 24) & 0xFF) as u8;
            let subclass = ((class_info >> 16) & 0xFF) as u8;
            let prog_if  = ((class_info >> 8)  & 0xFF) as u8;
            if class == PCI_CLASS_STORAGE && subclass == PCI_SUBCLASS_SATA && prog_if == PCI_PROGIF_AHCI {
                // Enable Bus Master + MMIO
                let cmd = pci_read(bus, dev, 0, 0x04);
                pci_write(bus, dev, 0, 0x04, cmd | 0x06); // MMIO + Bus Master

                // Read ABAR (BAR5 = AHCI base memory register)
                let bar5 = pci_read(bus, dev, 0, 0x24);
                let abar_phys = (bar5 & !0xF) as u64;
                if abar_phys == 0 { continue; }

                // Map ABAR as UC/WC-disabled to ensure register writes hit hardware
                let abar_page = abar_phys & !0xFFF;
                let mut mapped = true;
                for page in 0..AHCI_MMIO_PAGES {
                    let v = AHCI_UC_VIRT_BASE + page * 4096;
                    let p = abar_page + page * 4096;
                    if !vmm::map_page_current(v, p, FLAG_WRITABLE | FLAG_PCD | FLAG_PWT | FLAG_NX) {
                        mapped = false;
                        break;
                    }
                }
                if !mapped {
                    crate::arch::x86_64::serial::write_str("[AHCI] Failed to map ABAR UC\r\n");
                    continue;
                }
                unsafe { HBA_VIRT = (AHCI_UC_VIRT_BASE + (abar_phys & 0xFFF)) as usize; }

                crate::arch::x86_64::serial::write_str("[AHCI] Controller at bus=");
                serial_dec(bus as u64); crate::arch::x86_64::serial::write_byte(b',');
                crate::arch::x86_64::serial::write_str(" ABAR=0x");
                serial_hex32(abar_phys as u32);
                crate::arch::x86_64::serial::write_str("\r\n");

                if init_hba() {
                    unsafe { INITIALIZED = true; }
                    return unsafe { DRIVE_COUNT };
                }
            }
        }
    }
    0
}

fn init_hba() -> bool {
    // Enable AHCI
    let ghc = hba_read(HBA_GHC);
    hba_write(HBA_GHC, ghc | GHC_AE);

    // Reset HBA
    hba_write(HBA_GHC, hba_read(HBA_GHC) | GHC_HR);
    let mut timeout = 1000000u32;
    while hba_read(HBA_GHC) & GHC_HR != 0 && timeout > 0 {
        timeout -= 1; delay(1);
    }
    if timeout == 0 { return false; }

    // Re-enable AHCI
    hba_write(HBA_GHC, hba_read(HBA_GHC) | GHC_AE);

    let pi = hba_read(HBA_PI); // Ports implemented bitmask
    crate::arch::x86_64::serial::write_str("[AHCI] Ports implemented: 0x");
    serial_hex32(pi);
    crate::arch::x86_64::serial::write_str("\r\n");

    let mut alloc_idx = 0usize;
    for port in 0u32..32 {
        if pi & (1 << port) == 0 { continue; }
        if alloc_idx >= MAX_ACTIVE_PORTS { break; }

        // Check if device is present
        let ssts = port_read(port, PORT_SSTS);
        let det = ssts & 0xF;
        let ipm = (ssts >> 8) & 0xF;

        if det != SSTS_DET_PRESENT { continue; }
        if ipm != 1 { continue; } // 1 = active / spun up

        let sig = port_read(port, PORT_SIG);
        crate::arch::x86_64::serial::write_str("[AHCI] Port ");
        serial_dec(port as u64);
        crate::arch::x86_64::serial::write_str(": sig=0x");
        serial_hex32(sig);
        crate::arch::x86_64::serial::write_str("\r\n");

        // Skip ATAPI (0xEB140101), PM (0x96690101), SEMB
        if sig == 0xEB140101 || sig == 0xC33C0101 {
            crate::arch::x86_64::serial::write_str("[AHCI]   -> ATAPI/other, skipping\r\n");
            continue;
        }

        if init_port(port, alloc_idx) {
            if identify_port(port, alloc_idx) {
                unsafe { DRIVE_COUNT += 1; }
                alloc_idx += 1;
            }
        }
    }

    true
}

fn stop_port(port: u32) {
    let cmd = port_read(port, PORT_CMD);
    port_write(port, PORT_CMD, cmd & !(PORT_CMD_ST | PORT_CMD_FRE));
    // Wait for CR and FR to clear
    let mut timeout = 500_000u32;
    loop {
        let c = port_read(port, PORT_CMD);
        if c & (PORT_CMD_CR | PORT_CMD_FR) == 0 { break; }
        timeout -= 1;
        if timeout == 0 { break; }
        delay(1);
    }
}

fn start_port(port: u32) {
    // Wait until CR clear
    let mut timeout = 500_000u32;
    while port_read(port, PORT_CMD) & PORT_CMD_CR != 0 && timeout > 0 {
        timeout -= 1; delay(1);
    }
    let cmd = port_read(port, PORT_CMD);
    port_write(port, PORT_CMD, cmd | PORT_CMD_FRE | PORT_CMD_ST);
}

fn wait_ci_clear(port: u32, mask: u32, label: &str) -> bool {
    // 18.2 Hz → ~55 ms per tick; 1000 ms ≈ 19 ticks
    let start_ms = crate::arch::x86_64::idt::timer_ticks();

    loop {
        let ci = port_read(port, PORT_CI);
        if ci & mask == 0 { return true; }

        let is = port_read(port, PORT_IS);
        if is & (1 << 30) != 0 {
            crate::arch::x86_64::serial::write_str("[AHCI] Task File Error while waiting for CI (" );
            crate::arch::x86_64::serial::write_str(label);
            crate::arch::x86_64::serial::write_str(")\r\n");
            return false;
        }

        if crate::arch::x86_64::idt::timer_ticks().wrapping_sub(start_ms) >= 19 {
            let serr = port_read(port, PORT_SERR);
            crate::arch::x86_64::serial::write_str("[AHCI] CI stuck after 1000ms (" );
            crate::arch::x86_64::serial::write_str(label);
            crate::arch::x86_64::serial::write_str(") IS=0x");
            serial_hex32(is);
            crate::arch::x86_64::serial::write_str(" SERR=0x");
            serial_hex32(serr);
            crate::arch::x86_64::serial::write_str("\r\n");
            return false;
        }

        delay(200); // small pause to avoid hammering the bus
    }
}

fn init_port(port: u32, ai: usize) -> bool {
    stop_port(port);

    // Set Command List Base
    let cl_phys = virt_to_phys(unsafe { CMD_LISTS[ai].0.as_ptr() as usize });
    port_write(port, PORT_CLB,  cl_phys as u32);
    port_write(port, PORT_CLBU, (cl_phys >> 32) as u32);

    // Set FIS Receive Base
    let fb_phys = virt_to_phys(unsafe { FIS_AREAS[ai].0.as_ptr() as usize });
    port_write(port, PORT_FB,  fb_phys as u32);
    port_write(port, PORT_FBU, (fb_phys >> 32) as u32);

    // Clear error
    port_write(port, PORT_SERR, 0xFFFFFFFF);

    // Set up the first command header with our command table
    let ct_phys = virt_to_phys(unsafe { CMD_TABLES[ai].cfis.as_ptr() as usize });
    unsafe {
        CMD_LISTS[ai].0[0].ctba_lo = ct_phys as u32;
        CMD_LISTS[ai].0[0].ctba_hi = (ct_phys >> 32) as u32;
    }

    start_port(port);
    true
}

fn identify_port(port: u32, ai: usize) -> bool {
    // Use a 512-byte identify buffer in DATA_BUF
    let result = run_pio_command(port, ai, ATA_CMD_IDENTIFY, 0, 0, 1, true);
    if !result { return false; }

    // Parse identify data from DATA_BUF
    unsafe {
        let id = core::slice::from_raw_parts(DATA_BUF.as_ptr() as *const u16, 256);

        // 48-bit LBA sector count: words 100-103
        let sects = (id[100] as u64)
            | ((id[101] as u64) << 16)
            | ((id[102] as u64) << 32)
            | ((id[103] as u64) << 48);

        let drive_idx = DRIVE_COUNT;
        DRIVES[drive_idx].present   = true;
        DRIVES[drive_idx].port_idx  = port as u8;
        DRIVES[drive_idx].alloc_idx = ai as u8;
        DRIVES[drive_idx].sectors   = sects;
        DRIVES[drive_idx].size_mb   = sects / 2048;

        // Model name: words 27-46, byte-swapped
        let mut mi = 0usize;
        for i in 27..47usize {
            let w = id[i];
            let b0 = (w >> 8) as u8;
            let b1 = (w & 0xFF) as u8;
            if b0 != 0 && mi < 40 { DRIVES[drive_idx].model[mi] = b0; mi += 1; }
            if b1 != 0 && mi < 40 { DRIVES[drive_idx].model[mi] = b1; mi += 1; }
        }
        while mi > 0 && DRIVES[drive_idx].model[mi - 1] == b' ' {
            DRIVES[drive_idx].model[mi - 1] = 0; mi -= 1;
        }
        DRIVES[drive_idx].model[mi] = 0;

        let model_end = DRIVES[drive_idx].model.iter().position(|&b| b == 0).unwrap_or(40);
        let model_str = core::str::from_utf8_unchecked(&DRIVES[drive_idx].model[..model_end]);
        crate::arch::x86_64::serial::write_str("[AHCI]   Model: ");
        crate::arch::x86_64::serial::write_str(model_str);
        crate::arch::x86_64::serial::write_str(", ");
        serial_dec(DRIVES[drive_idx].size_mb);
        crate::arch::x86_64::serial::write_str(" MiB\r\n");
    }
    true
}

fn run_pio_command(port: u32, ai: usize, cmd: u8, lba: u64, count: u32,
                   sector_count: u32, is_read: bool) -> bool {
    unsafe {
        let ct = &mut CMD_TABLES[ai];
        // Zero FIS area
        for b in ct.cfis.iter_mut() { *b = 0; }

        // Build H2D FIS
        ct.cfis[0] = FIS_TYPE_REG_H2D;
        ct.cfis[1] = 1 << 7; // C bit = 1 (command)
        ct.cfis[2] = cmd;
        ct.cfis[3] = 0; // features
        ct.cfis[4] = (lba & 0xFF) as u8;
        ct.cfis[5] = ((lba >> 8) & 0xFF) as u8;
        ct.cfis[6] = ((lba >> 16) & 0xFF) as u8;
        ct.cfis[7] = 0x40; // LBA mode = bit 6 set in device register
        ct.cfis[8] = ((lba >> 24) & 0xFF) as u8;
        ct.cfis[9] = ((lba >> 32) & 0xFF) as u8;
        ct.cfis[10] = ((lba >> 40) & 0xFF) as u8;
        ct.cfis[11] = 0;
        ct.cfis[12] = (count & 0xFF) as u8;
        ct.cfis[13] = ((count >> 8) & 0xFF) as u8;

        // Set up PRD entry
        let dat_phys = virt_to_phys(DATA_BUF.as_ptr() as usize);
        ct.prdt[0].dba_lo = dat_phys as u32;
        ct.prdt[0].dba_hi = (dat_phys >> 32) as u32;
        ct.prdt[0]._rsv   = 0;
        let byte_count = (sector_count as usize * SECTOR_SIZE - 1) as u32;
        ct.prdt[0].dbc    = byte_count | (1 << 31); // interrupt on completion

        // Set command header
        let cfis_len_dw = 5u16; // 20 bytes / 4 = 5 DWORDs
        let write_bit = if !is_read { 1u16 << 6 } else { 0u16 };
        CMD_LISTS[ai].0[0].flags  = cfis_len_dw | write_bit;
        CMD_LISTS[ai].0[0].prdtl  = 1;
        CMD_LISTS[ai].0[0].prdbc  = 0;
    }

    // Clear interrupt status
    port_write(port, PORT_IS, 0xFFFFFFFF);

    // Issue command (slot 0)
    port_write(port, PORT_CI, 1);

    // Wait for completion (1s deadline with debug dump)
    if !wait_ci_clear(port, 1, "PIO") {
        return false;
    }

    true
}

// ─── Public API ───────────────────────────────────────────────────────────

pub fn is_initialized() -> bool { unsafe { INITIALIZED } }
pub fn drive_count() -> usize   { unsafe { DRIVE_COUNT } }

pub fn drive_info(idx: usize) -> Option<&'static AhciDrive> {
    unsafe {
        if idx < DRIVE_COUNT && DRIVES[idx].present { Some(&DRIVES[idx]) } else { None }
    }
}

/// Read `count` sectors from AHCI drive `drive_idx` at LBA `lba`.
/// Data written into `buf` (must be count * 512 bytes).
pub fn read_sectors(drive_idx: usize, lba: u64, count: u32, buf: &mut [u8]) -> bool {
    if buf.len() < count as usize * SECTOR_SIZE { return false; }

    let d = match drive_info(drive_idx) { Some(d) => *d, None => return false };
    let port = d.port_idx as u32;
    let ai   = d.alloc_idx as usize;

    if !run_pio_command(port, ai, ATA_CMD_READ_DMA_EX, lba, count, count, true) {
        return false;
    }

    let total = (count as usize * SECTOR_SIZE).min(DATA_BUF_SIZE);
    buf[..total].copy_from_slice(unsafe { &DATA_BUF[..total] });
    true
}

/// Write `count` sectors to AHCI drive `drive_idx` at LBA `lba`.
pub fn write_sectors(drive_idx: usize, lba: u64, count: u32, data: &[u8]) -> bool {
    if data.len() < count as usize * SECTOR_SIZE { return false; }

    let d = match drive_info(drive_idx) { Some(d) => *d, None => return false };
    let port = d.port_idx as u32;
    let ai   = d.alloc_idx as usize;

    let total = (count as usize * SECTOR_SIZE).min(DATA_BUF_SIZE);
    unsafe { DATA_BUF[..total].copy_from_slice(&data[..total]); }

    run_pio_command(port, ai, ATA_CMD_WRITE_DMA_EX, lba, count, count, false)
}

/// Zero-copy DMA write using a physical buffer allocated by the PMM.
/// `buffer_phys` must point to a physically contiguous region of at least
/// `count * 512` bytes. Returns true on success.
pub fn dma_write(drive_idx: usize, lba: u64, count: u32, buffer_phys: u64) -> bool {
    if count == 0 { return true; }

    let d = match drive_info(drive_idx) { Some(d) => *d, None => return false };
    let port = d.port_idx as u32;
    let ai   = d.alloc_idx as usize;

    unsafe {
        let ct = &mut CMD_TABLES[ai];
        for b in ct.cfis.iter_mut() { *b = 0; }

        // Build H2D FIS for WRITE DMA EXT
        ct.cfis[0] = FIS_TYPE_REG_H2D;
        ct.cfis[1] = 1 << 7; // Command
        ct.cfis[2] = ATA_CMD_WRITE_DMA_EX;
        ct.cfis[3] = 0;
        ct.cfis[4] = (lba & 0xFF) as u8;
        ct.cfis[5] = ((lba >> 8) & 0xFF) as u8;
        ct.cfis[6] = ((lba >> 16) & 0xFF) as u8;
        ct.cfis[7] = 0x40; // LBA mode
        ct.cfis[8] = ((lba >> 24) & 0xFF) as u8;
        ct.cfis[9] = ((lba >> 32) & 0xFF) as u8;
        ct.cfis[10] = ((lba >> 40) & 0xFF) as u8;
        ct.cfis[11] = 0;
        ct.cfis[12] = (count & 0xFF) as u8;
        ct.cfis[13] = ((count >> 8) & 0xFF) as u8;

        // PRD: point directly to caller's physical buffer
        ct.prdt[0].dba_lo = buffer_phys as u32;
        ct.prdt[0].dba_hi = (buffer_phys >> 32) as u32;
        ct.prdt[0]._rsv   = 0;
        let byte_count = (count as usize * SECTOR_SIZE - 1) as u32;
        ct.prdt[0].dbc    = byte_count | (1 << 31); // IOC

        // Command header
        let cfis_len_dw = 5u16;
        CMD_LISTS[ai].0[0].flags  = cfis_len_dw | (1u16 << 6); // write bit
        CMD_LISTS[ai].0[0].prdtl  = 1;
        CMD_LISTS[ai].0[0].prdbc  = 0;

        port_write(port, PORT_IS, 0xFFFFFFFF);
        port_write(port, PORT_CI, 1);

        // Poll for completion (1s deadline with IS/SERR dump)
        if !wait_ci_clear(port, 1, "DMA") {
            return false;
        }
    }

    true
}

fn serial_hex32(v: u32) {
    let h = b"0123456789abcdef";
    for i in (0..8).rev() {
        crate::arch::x86_64::serial::write_byte(h[((v >> (i * 4)) & 0xF) as usize]);
    }
}
fn serial_dec(mut v: u64) {
    if v == 0 { crate::arch::x86_64::serial::write_byte(b'0'); return; }
    let mut buf = [0u8; 20]; let mut i = 0;
    while v > 0 { buf[i] = b'0' + (v % 10) as u8; v /= 10; i += 1; }
    for j in (0..i).rev() { crate::arch::x86_64::serial::write_byte(buf[j]); }
}
