/*
 * AETERNA ACPI Subsystem
 *
 * Parses ACPI tables (RSDP → RSDT/XSDT → FADT) for:
 *   - Real hardware shutdown (S5 sleep state via PM1a_CNT)
 *   - Real hardware reboot (FADT reset register, or 0xCF9 fallback)
 *   - MCFG table for PCIe MMIO config space (future use)
 *
 * RSDP is obtained from the Limine bootloader.
 * All pointers are physical addresses accessed via HHDM.
 */

use core::arch::asm;
use crate::arch::x86_64::serial;
use crate::arch::x86_64::boot;

// ══════════════════════════════════════════════════════════════════════════════
// ACPI table structures
// ══════════════════════════════════════════════════════════════════════════════

/// RSDP v1 (ACPI 1.0) — 20 bytes
#[repr(C, packed)]
struct Rsdp {
    signature:    [u8; 8],   // "RSD PTR "
    checksum:     u8,
    oem_id:       [u8; 6],
    revision:     u8,        // 0 = ACPI 1.0, 2 = ACPI 2.0+
    rsdt_address: u32,       // physical address of RSDT
}

/// RSDP v2 (ACPI 2.0+) — 36 bytes
#[repr(C, packed)]
struct Rsdp2 {
    v1:             Rsdp,
    length:         u32,
    xsdt_address:   u64,     // physical address of XSDT (64-bit)
    extended_checksum: u8,
    reserved:       [u8; 3],
}

/// Standard ACPI table header (SDT header) — 36 bytes
#[repr(C, packed)]
pub struct SdtHeader {
    pub signature:    [u8; 4],
    pub length:       u32,
    pub revision:     u8,
    pub checksum:     u8,
    pub oem_id:       [u8; 6],
    pub oem_table_id: [u8; 8],
    pub oem_revision: u32,
    pub creator_id:   u32,
    pub creator_revision: u32,
}

/// Fixed ACPI Description Table (FADT / FACP) — partial, we only need specific fields
#[repr(C, packed)]
struct Fadt {
    header:         SdtHeader,
    firmware_ctrl:  u32,          // offset 36
    dsdt:           u32,          // offset 40
    _reserved1:     u8,           // offset 44
    preferred_pm_profile: u8,     // offset 45
    sci_interrupt:  u16,          // offset 46
    smi_command_port: u32,        // offset 48
    acpi_enable:    u8,           // offset 52
    acpi_disable:   u8,           // offset 53
    s4bios_req:     u8,           // offset 54
    pstate_control: u8,           // offset 55
    pm1a_event_block: u32,        // offset 56
    pm1b_event_block: u32,        // offset 60
    pm1a_control_block: u32,      // offset 64 ← PM1a_CNT_BLK (I/O port)
    pm1b_control_block: u32,      // offset 68
    pm2_control_block: u32,       // offset 72
    pm_timer_block: u32,          // offset 76
    gpe0_block:     u32,          // offset 80
    gpe1_block:     u32,          // offset 84
    pm1_event_length: u8,         // offset 88
    pm1_control_length: u8,       // offset 89
    pm2_control_length: u8,       // offset 90
    pm_timer_length: u8,          // offset 91
    gpe0_block_length: u8,        // offset 92
    gpe1_block_length: u8,        // offset 93
    gpe1_base:      u8,           // offset 94
    cstate_control: u8,           // offset 95
    worst_c2_latency: u16,        // offset 96
    worst_c3_latency: u16,        // offset 98
    flush_size:     u16,          // offset 100
    flush_stride:   u16,          // offset 102
    duty_offset:    u8,           // offset 104
    duty_width:     u8,           // offset 105
    day_alarm:      u8,           // offset 106
    month_alarm:    u8,           // offset 107
    century:        u8,           // offset 108
    boot_arch_flags: u16,         // offset 109 (ACPI 2.0+)
    _reserved2:     u8,           // offset 111
    flags:          u32,          // offset 112
    reset_register: GenericAddress, // offset 116 (ACPI 2.0+)
    reset_value:    u8,           // offset 128
    arm_boot_arch:  u16,          // offset 129
    fadt_minor_version: u8,       // offset 131
}

/// ACPI Generic Address Structure (GAS) — 12 bytes
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct GenericAddress {
    address_space: u8,   // 0=system memory, 1=system I/O, 2=PCI config
    bit_width:     u8,
    bit_offset:    u8,
    access_size:   u8,   // 0=undefined, 1=byte, 2=word, 3=dword, 4=qword
    address:       u64,
}

// ══════════════════════════════════════════════════════════════════════════════
// Parsed ACPI state
// ══════════════════════════════════════════════════════════════════════════════

/// Cached ACPI data extracted from tables
struct AcpiState {
    initialized:     bool,
    /// PM1a Control Block I/O port (from FADT)
    pm1a_cnt_blk:    u32,
    /// PM1b Control Block I/O port (0 if not present)
    pm1b_cnt_blk:    u32,
    /// SLP_TYPa value for S5 (shutdown) state
    slp_typa_s5:     u16,
    /// SLP_TYPb value for S5 (if PM1b exists)
    slp_typb_s5:     u16,
    /// FADT reset register (ACPI 2.0+)
    reset_reg:       GenericAddress,
    /// FADT reset value
    reset_val:       u8,
    /// Whether FADT has valid reset register (ACPI 2.0+ with RESET_REG_SUP flag)
    has_reset_reg:   bool,
    /// SCI interrupt number
    sci_irq:         u16,
}

static mut ACPI: AcpiState = AcpiState {
    initialized:   false,
    pm1a_cnt_blk:  0,
    pm1b_cnt_blk:  0,
    slp_typa_s5:   0,
    slp_typb_s5:   0,
    reset_reg:     GenericAddress {
        address_space: 0, bit_width: 0, bit_offset: 0, access_size: 0, address: 0,
    },
    reset_val:     0,
    has_reset_reg: false,
    sci_irq:       0,
};

// ══════════════════════════════════════════════════════════════════════════════
// HHDM helper: physical → virtual
// ══════════════════════════════════════════════════════════════════════════════

fn phys_to_virt(phys: u64) -> *const u8 {
    let offset = boot::hhdm_offset().unwrap_or(0);
    (phys + offset) as *const u8
}

// ══════════════════════════════════════════════════════════════════════════════
// ACPI initialization
// ══════════════════════════════════════════════════════════════════════════════

/// Initialize ACPI subsystem: parse RSDP → RSDT/XSDT → FADT → DSDT (for S5).
/// Call once during boot (after HHDM is set up).
pub fn init() -> bool {
    serial::write_str("[ACPI] Initializing...\r\n");

    // Get RSDP from Limine bootloader
    let rsdp_ptr = match boot::rsdp_address() {
        Some(p) => p,
        None => {
            // Fallback: scan EBDA and BIOS ROM area
            serial::write_str("[ACPI] Limine RSDP not available, scanning memory...\r\n");
            match scan_for_rsdp() {
                Some(p) => p,
                None => {
                    serial::write_str("[ACPI] RSDP not found!\r\n");
                    return false;
                }
            }
        }
    };

    serial::write_str("[ACPI] RSDP found at 0x");
    serial_hex(rsdp_ptr as u64);
    serial::write_str("\r\n");

    // Validate RSDP signature
    let rsdp = unsafe { &*(rsdp_ptr as *const Rsdp) };
    if &rsdp.signature != b"RSD PTR " {
        serial::write_str("[ACPI] Invalid RSDP signature\r\n");
        return false;
    }

    // Checksum validation (RSDP v1: first 20 bytes sum to 0 mod 256)
    let rsdp_bytes = unsafe { core::slice::from_raw_parts(rsdp_ptr, 20) };
    let sum: u8 = rsdp_bytes.iter().fold(0u8, |a, &b| a.wrapping_add(b));
    if sum != 0 {
        serial::write_str("[ACPI] RSDP checksum failed\r\n");
        return false;
    }

    // Determine ACPI version and get table root
    let acpi_v2 = rsdp.revision >= 2;
    serial::write_str("[ACPI] ACPI revision: ");
    if acpi_v2 { serial::write_str("2.0+\r\n"); } else { serial::write_str("1.0\r\n"); }

    // Parse RSDT or XSDT to find FADT
    let fadt_ptr = if acpi_v2 {
        let rsdp2 = unsafe { &*(rsdp_ptr as *const Rsdp2) };
        let xsdt_phys = rsdp2.xsdt_address;
        if xsdt_phys != 0 {
            serial::write_str("[ACPI] XSDT at 0x");
            serial_hex(xsdt_phys);
            serial::write_str("\r\n");
            find_table_xsdt(xsdt_phys, b"FACP")
        } else {
            // Fallback to RSDT
            let rsdt_phys = rsdp.rsdt_address as u64;
            find_table_rsdt(rsdt_phys, b"FACP")
        }
    } else {
        let rsdt_phys = rsdp.rsdt_address as u64;
        serial::write_str("[ACPI] RSDT at 0x");
        serial_hex(rsdt_phys);
        serial::write_str("\r\n");
        find_table_rsdt(rsdt_phys, b"FACP")
    };

    let fadt_virt = match fadt_ptr {
        Some(p) => p,
        None => {
            serial::write_str("[ACPI] FADT (FACP) not found!\r\n");
            return false;
        }
    };

    serial::write_str("[ACPI] FADT found\r\n");

    // Extract FADT fields
    let fadt = unsafe { &*(fadt_virt as *const Fadt) };
    unsafe {
        ACPI.pm1a_cnt_blk = fadt.pm1a_control_block;
        ACPI.pm1b_cnt_blk = fadt.pm1b_control_block;
        ACPI.sci_irq = fadt.sci_interrupt;

        serial::write_str("[ACPI] PM1a_CNT_BLK: 0x");
        serial_hex(ACPI.pm1a_cnt_blk as u64);
        serial::write_str("\r\n");

        if ACPI.pm1b_cnt_blk != 0 {
            serial::write_str("[ACPI] PM1b_CNT_BLK: 0x");
            serial_hex(ACPI.pm1b_cnt_blk as u64);
            serial::write_str("\r\n");
        }

        // Check for ACPI 2.0+ reset register
        // FADT flags bit 10 = RESET_REG_SUP
        if acpi_v2 && fadt.header.length >= 129 && (fadt.flags & (1 << 10)) != 0 {
            ACPI.reset_reg = fadt.reset_register;
            ACPI.reset_val = fadt.reset_value;
            ACPI.has_reset_reg = ACPI.reset_reg.address != 0;
            if ACPI.has_reset_reg {
                serial::write_str("[ACPI] Reset register: space=");
                serial_dec(ACPI.reset_reg.address_space as u64);
                serial::write_str(" addr=0x");
                serial_hex(ACPI.reset_reg.address);
                serial::write_str(" val=0x");
                serial_hex(ACPI.reset_val as u64);
                serial::write_str("\r\n");
            }
        }
    }

    // Parse DSDT to find \_S5 SLP_TYP values for shutdown
    let dsdt_phys = if acpi_v2 && fadt.header.length >= 148 {
        // X_DSDT (64-bit) is at offset 140 in FADT
        let x_dsdt_ptr = unsafe {
            let p = (fadt_virt as *const u8).add(140);
            *(p as *const u64)
        };
        if x_dsdt_ptr != 0 { x_dsdt_ptr } else { fadt.dsdt as u64 }
    } else {
        fadt.dsdt as u64
    };

    if dsdt_phys != 0 {
        let dsdt_virt = phys_to_virt(dsdt_phys);
        parse_s5_from_dsdt(dsdt_virt);
    } else {
        serial::write_str("[ACPI] No DSDT — using default S5 SLP_TYP=5\r\n");
        unsafe {
            ACPI.slp_typa_s5 = 5;
            ACPI.slp_typb_s5 = 5;
        }
    }

    unsafe { ACPI.initialized = true; }
    serial::write_str("[ACPI] Initialization complete\r\n");
    true
}

/// Check if ACPI is available and initialized
pub fn is_available() -> bool {
    unsafe { ACPI.initialized }
}

// ══════════════════════════════════════════════════════════════════════════════
// RSDP scanning fallback (if Limine doesn't provide it)
// ══════════════════════════════════════════════════════════════════════════════

fn scan_for_rsdp() -> Option<*const u8> {
    // Scan EBDA (Extended BIOS Data Area) — first KiB at segment from 0x040E
    let ebda_seg = unsafe { *(phys_to_virt(0x040E) as *const u16) } as u64;
    let ebda_base = ebda_seg << 4;
    if ebda_base != 0 {
        if let Some(p) = scan_region(ebda_base, 1024) {
            return Some(p);
        }
    }

    // Scan BIOS ROM area: 0xE0000 - 0xFFFFF
    scan_region(0xE0000, 0x20000)
}

fn scan_region(base: u64, length: usize) -> Option<*const u8> {
    let start = phys_to_virt(base);
    let mut offset = 0usize;
    while offset + 20 <= length {
        let ptr = unsafe { start.add(offset) };
        let sig = unsafe { core::slice::from_raw_parts(ptr, 8) };
        if sig == b"RSD PTR " {
            // Validate checksum
            let bytes = unsafe { core::slice::from_raw_parts(ptr, 20) };
            let sum: u8 = bytes.iter().fold(0u8, |a, &b| a.wrapping_add(b));
            if sum == 0 {
                return Some(ptr);
            }
        }
        offset += 16; // RSDP is always 16-byte aligned
    }
    None
}

// ══════════════════════════════════════════════════════════════════════════════
// Table lookup: RSDT (32-bit pointers) and XSDT (64-bit pointers)
// ══════════════════════════════════════════════════════════════════════════════

fn find_table_rsdt(rsdt_phys: u64, signature: &[u8; 4]) -> Option<*const u8> {
    let rsdt_virt = phys_to_virt(rsdt_phys);
    let header = unsafe { &*(rsdt_virt as *const SdtHeader) };

    if &header.signature != b"RSDT" {
        serial::write_str("[ACPI] Invalid RSDT signature\r\n");
        return None;
    }

    let entry_count = (header.length as usize - core::mem::size_of::<SdtHeader>()) / 4;
    let entries_ptr = unsafe { rsdt_virt.add(core::mem::size_of::<SdtHeader>()) as *const u32 };

    for i in 0..entry_count {
        let entry_phys = unsafe { *entries_ptr.add(i) } as u64;
        if entry_phys == 0 { continue; }
        let entry_virt = phys_to_virt(entry_phys);
        let entry_header = unsafe { &*(entry_virt as *const SdtHeader) };
        if &entry_header.signature == signature {
            return Some(entry_virt);
        }
    }
    None
}

fn find_table_xsdt(xsdt_phys: u64, signature: &[u8; 4]) -> Option<*const u8> {
    let xsdt_virt = phys_to_virt(xsdt_phys);
    let header = unsafe { &*(xsdt_virt as *const SdtHeader) };

    if &header.signature != b"XSDT" {
        serial::write_str("[ACPI] Invalid XSDT signature\r\n");
        return None;
    }

    let entry_count = (header.length as usize - core::mem::size_of::<SdtHeader>()) / 8;
    let entries_ptr = unsafe { xsdt_virt.add(core::mem::size_of::<SdtHeader>()) as *const u64 };

    for i in 0..entry_count {
        let entry_phys = unsafe { *entries_ptr.add(i) };
        if entry_phys == 0 { continue; }
        let entry_virt = phys_to_virt(entry_phys);
        let entry_header = unsafe { &*(entry_virt as *const SdtHeader) };
        if &entry_header.signature == signature {
            return Some(entry_virt);
        }
    }
    None
}

// ══════════════════════════════════════════════════════════════════════════════
// DSDT \_S5 object parsing
// ══════════════════════════════════════════════════════════════════════════════

/// Search the DSDT (or SSDT) AML bytecode for the \_S5_ object.
/// The S5 object contains SLP_TYPa and SLP_TYPb values needed for shutdown.
///
/// AML encoding of \_S5_ typically looks like:
///   08 5F53355F 12 ...   (NameOp, "_S5_", PackageOp, ...)
///   The package contains: NumElements, ByteConst(SLP_TYPa), ByteConst(SLP_TYPb), ...
fn parse_s5_from_dsdt(dsdt_virt: *const u8) {
    let header = unsafe { &*(dsdt_virt as *const SdtHeader) };
    let dsdt_len = header.length as usize;

    if dsdt_len < core::mem::size_of::<SdtHeader>() + 8 {
        serial::write_str("[ACPI] DSDT too small\r\n");
        unsafe {
            ACPI.slp_typa_s5 = 5;
            ACPI.slp_typb_s5 = 5;
        }
        return;
    }

    // Search for "_S5_" in AML bytecode after the header
    let aml_start = core::mem::size_of::<SdtHeader>();
    let aml = unsafe { core::slice::from_raw_parts(dsdt_virt.add(aml_start), dsdt_len - aml_start) };

    // Look for the byte sequence: 08 5F 53 35 5F (NameOp followed by "_S5_")
    // then 12 (PackageOp)
    let name_bytes = b"_S5_";

    for i in 0..aml.len().saturating_sub(20) {
        if aml[i] == 0x08 && i + 5 < aml.len() {
            // Check for "_S5_" name
            if &aml[i+1..i+5] == name_bytes {
                // Found NameOp "_S5_" — next should be PackageOp (0x12)
                let pkg_start = i + 5;
                if pkg_start >= aml.len() { continue; }

                if aml[pkg_start] == 0x12 {
                    // Parse package: skip PkgLength encoding
                    let (pkg_data_start, _pkg_len) = parse_pkg_length(aml, pkg_start + 1);
                    if pkg_data_start >= aml.len() { continue; }

                    // Skip NumElements byte
                    let elem_start = pkg_data_start + 1;
                    if elem_start >= aml.len() { continue; }

                    // Read SLP_TYPa (first element)
                    let (slp_typa, next) = read_aml_integer(aml, elem_start);
                    // Read SLP_TYPb (second element)
                    let (slp_typb, _) = read_aml_integer(aml, next);

                    unsafe {
                        ACPI.slp_typa_s5 = slp_typa as u16;
                        ACPI.slp_typb_s5 = slp_typb as u16;
                    }

                    serial::write_str("[ACPI] S5 SLP_TYPa=");
                    serial_dec(slp_typa);
                    serial::write_str(" SLP_TYPb=");
                    serial_dec(slp_typb);
                    serial::write_str("\r\n");
                    return;
                }
            }
        }
    }

    // \_S5_ not found — use common defaults
    serial::write_str("[ACPI] \\_S5_ not found in DSDT, using defaults (SLP_TYP=5)\r\n");
    unsafe {
        ACPI.slp_typa_s5 = 5;
        ACPI.slp_typb_s5 = 5;
    }
}

/// Parse AML PkgLength encoding (1-4 bytes).
/// Returns (offset_after_pkglen, total_package_length).
fn parse_pkg_length(aml: &[u8], offset: usize) -> (usize, usize) {
    if offset >= aml.len() { return (offset, 0); }
    let lead = aml[offset];
    let byte_count = ((lead >> 6) & 0x03) as usize;

    if byte_count == 0 {
        // Single byte: length = lead & 0x3F
        (offset + 1, (lead & 0x3F) as usize)
    } else {
        // Multi-byte: lead provides low 4 bits, following bytes provide rest
        let mut len = (lead & 0x0F) as usize;
        for i in 0..byte_count {
            if offset + 1 + i >= aml.len() { return (offset + 1 + byte_count, len); }
            len |= (aml[offset + 1 + i] as usize) << (4 + 8 * i);
        }
        (offset + 1 + byte_count, len)
    }
}

/// Read an AML integer constant (ByteConst, WordConst, DWordConst, or raw byte).
/// Returns (value, next_offset).
fn read_aml_integer(aml: &[u8], offset: usize) -> (u64, usize) {
    if offset >= aml.len() { return (0, offset); }
    match aml[offset] {
        0x0A => {
            // BytePrefix: next byte is the value
            if offset + 1 < aml.len() {
                (aml[offset + 1] as u64, offset + 2)
            } else {
                (0, offset + 1)
            }
        }
        0x0B => {
            // WordPrefix: next 2 bytes (LE)
            if offset + 2 < aml.len() {
                let val = u16::from_le_bytes([aml[offset + 1], aml[offset + 2]]);
                (val as u64, offset + 3)
            } else {
                (0, offset + 3)
            }
        }
        0x0C => {
            // DWordPrefix: next 4 bytes (LE)
            if offset + 4 < aml.len() {
                let val = u32::from_le_bytes([
                    aml[offset + 1], aml[offset + 2],
                    aml[offset + 3], aml[offset + 4],
                ]);
                (val as u64, offset + 5)
            } else {
                (0, offset + 5)
            }
        }
        0x00 => (0, offset + 1), // ZeroOp
        0x01 => (1, offset + 1), // OneOp
        0xFF => (u64::MAX, offset + 1), // OnesOp
        b => {
            // Direct byte value (0x02..0x09 are reserved but some BIOSes use raw bytes)
            (b as u64, offset + 1)
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// Shutdown (S5 sleep state)
// ══════════════════════════════════════════════════════════════════════════════

/// Perform ACPI shutdown (S5 sleep state).
/// Falls back to emulator-specific ports if ACPI is not available.
pub fn shutdown() {
    serial::write_str("[ACPI] Shutdown requested\r\n");

    unsafe {
        asm!("cli");

        if ACPI.initialized && ACPI.pm1a_cnt_blk != 0 {
            // Write SLP_TYPa | SLP_EN (bit 13) to PM1a_CNT
            let val = (ACPI.slp_typa_s5 as u16) << 10 | (1 << 13);
            serial::write_str("[ACPI] Writing PM1a_CNT: 0x");
            serial_hex(val as u64);
            serial::write_str(" to port 0x");
            serial_hex(ACPI.pm1a_cnt_blk as u64);
            serial::write_str("\r\n");

            asm!("out dx, ax",
                in("dx") ACPI.pm1a_cnt_blk as u16,
                in("ax") val,
                options(nomem, nostack));

            // If PM1b exists, write there too
            if ACPI.pm1b_cnt_blk != 0 {
                let val_b = (ACPI.slp_typb_s5 as u16) << 10 | (1 << 13);
                asm!("out dx, ax",
                    in("dx") ACPI.pm1b_cnt_blk as u16,
                    in("ax") val_b,
                    options(nomem, nostack));
            }

            // Wait a bit for the hardware to respond
            for _ in 0..1000000u32 { asm!("pause"); }
        }

        // Fallback: QEMU shutdown port
        serial::write_str("[ACPI] Trying QEMU shutdown port 0x604\r\n");
        asm!("out dx, ax", in("dx") 0x604u16, in("ax") 0x2000u16, options(nomem, nostack));

        // Fallback: Bochs/old QEMU shutdown port
        asm!("out dx, ax", in("dx") 0xB004u16, in("ax") 0x2000u16, options(nomem, nostack));

        // Fallback: VirtualBox shutdown port
        asm!("out dx, ax", in("dx") 0x4004u16, in("ax") 0x3400u16, options(nomem, nostack));
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// Reboot
// ══════════════════════════════════════════════════════════════════════════════

/// Perform ACPI reboot.
/// Tries: FADT reset register → PCI reset (0xCF9) → PS/2 controller → triple fault.
pub fn reboot() {
    serial::write_str("[ACPI] Reboot requested\r\n");

    unsafe {
        asm!("cli");

        // Method 1: FADT reset register (ACPI 2.0+)
        if ACPI.initialized && ACPI.has_reset_reg {
            serial::write_str("[ACPI] Using FADT reset register\r\n");
            match ACPI.reset_reg.address_space {
                1 => {
                    // System I/O space
                    let port = ACPI.reset_reg.address as u16;
                    let val = ACPI.reset_val;
                    asm!("out dx, al",
                        in("dx") port,
                        in("al") val,
                        options(nomem, nostack));
                }
                0 => {
                    // System memory space
                    let addr = phys_to_virt(ACPI.reset_reg.address) as *mut u8;
                    core::ptr::write_volatile(addr, ACPI.reset_val);
                }
                2 => {
                    // PCI Configuration Space
                    // address = bus<<16 | device<<11 | function<<8 | offset
                    let addr = ACPI.reset_reg.address as u32;
                    let pci_addr = 0x80000000 | addr;
                    asm!("out dx, eax",
                        in("dx") 0xCF8u16,
                        in("eax") pci_addr & 0xFFFFFFFC,
                        options(nomem, nostack));
                    let data_port = 0xCFC + (addr & 3);
                    asm!("out dx, al",
                        in("dx") data_port as u16,
                        in("al") ACPI.reset_val,
                        options(nomem, nostack));
                }
                _ => {}
            }
            for _ in 0..1000000u32 { asm!("pause"); }
        }

        // Method 2: PCI reset control register (0xCF9)
        serial::write_str("[ACPI] Trying PCI reset (0xCF9)\r\n");
        asm!("out dx, al", in("dx") 0xCF9u16, in("al") 0x00u8, options(nomem, nostack));
        asm!("out dx, al", in("dx") 0xCF9u16, in("al") 0x04u8, options(nomem, nostack)); // RST_CPU
        asm!("out dx, al", in("dx") 0xCF9u16, in("al") 0x0Eu8, options(nomem, nostack)); // RST_CPU | SYS_RST | FULL_RST
        for _ in 0..1000000u32 { asm!("pause"); }

        // Method 3: PS/2 keyboard controller reset
        serial::write_str("[ACPI] Trying PS/2 keyboard controller reset\r\n");
        let mut timeout = 100000u32;
        loop {
            let status: u8;
            asm!("in al, dx", in("dx") 0x64u16, out("al") status, options(nomem, nostack));
            if status & 0x02 == 0 || timeout == 0 { break; }
            timeout -= 1;
        }
        asm!("out dx, al", in("dx") 0x64u16, in("al") 0xFEu8, options(nomem, nostack));
        for _ in 0..1000000u32 { asm!("pause"); }

        // Method 4: Triple fault (last resort)
        serial::write_str("[ACPI] Triple fault reboot\r\n");
        let null_idt: [u8; 6] = [0; 6];
        asm!("lidt [{}]", in(reg) &null_idt, options(noreturn));
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// Info / diagnostics
// ══════════════════════════════════════════════════════════════════════════════

/// Get PM1a control block port (for diagnostics)
pub fn pm1a_cnt_blk() -> u32 {
    unsafe { ACPI.pm1a_cnt_blk }
}

/// Get SCI interrupt number
pub fn sci_irq() -> u16 {
    unsafe { ACPI.sci_irq }
}

/// Get S5 SLP_TYP values
pub fn s5_slp_typ() -> (u16, u16) {
    unsafe { (ACPI.slp_typa_s5, ACPI.slp_typb_s5) }
}

// ══════════════════════════════════════════════════════════════════════════════
// Serial helpers
// ══════════════════════════════════════════════════════════════════════════════

fn serial_hex(val: u64) {
    const HEX: [u8; 16] = *b"0123456789ABCDEF";
    let mut buf = [0u8; 16];
    let mut v = val;
    for i in (0..16).rev() {
        buf[i] = HEX[(v & 0xF) as usize];
        v >>= 4;
    }
    for b in buf {
        serial::write_byte(b);
    }
}

fn serial_dec(mut val: u64) {
    if val == 0 {
        serial::write_byte(b'0');
        return;
    }
    let mut buf = [0u8; 20];
    let mut i = 0;
    while val > 0 {
        buf[i] = b'0' + (val % 10) as u8;
        val /= 10;
        i += 1;
    }
    for j in (0..i).rev() {
        serial::write_byte(buf[j]);
    }
}
