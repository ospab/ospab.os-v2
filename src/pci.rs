/*
 * AETERNA PCI Configuration Space Access
 *
 * Centralized PCI config space read/write using I/O ports 0xCF8/0xCFC.
 * Replaces the private copies scattered across drivers.
 *
 * Supports:
 *   - Type 0 config space (bus, device, function, offset)
 *   - Full PCI bus enumeration
 *   - BAR decoding (MMIO and I/O)
 *   - Capability list walking
 *   - MSI/MSI-X configuration
 */

use core::arch::asm;

// PCI Configuration Address Port
const PCI_CONFIG_ADDR: u16 = 0xCF8;
const PCI_CONFIG_DATA: u16 = 0xCFC;

// ══════════════════════════════════════════════════════════════════════════════
// Config space access
// ══════════════════════════════════════════════════════════════════════════════

/// Read a 32-bit value from PCI configuration space.
pub fn config_read32(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    let addr: u32 = 0x80000000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | ((offset as u32) & 0xFC);
    unsafe {
        asm!("out dx, eax", in("dx") PCI_CONFIG_ADDR, in("eax") addr, options(nomem, nostack));
        let val: u32;
        asm!("in eax, dx", in("dx") PCI_CONFIG_DATA, out("eax") val, options(nomem, nostack));
        val
    }
}

/// Read a 16-bit value from PCI configuration space.
pub fn config_read16(bus: u8, device: u8, function: u8, offset: u8) -> u16 {
    let dword = config_read32(bus, device, function, offset & 0xFC);
    ((dword >> ((offset & 2) * 8)) & 0xFFFF) as u16
}

/// Read an 8-bit value from PCI configuration space.
pub fn config_read8(bus: u8, device: u8, function: u8, offset: u8) -> u8 {
    let dword = config_read32(bus, device, function, offset & 0xFC);
    ((dword >> ((offset & 3) * 8)) & 0xFF) as u8
}

/// Write a 32-bit value to PCI configuration space.
pub fn config_write32(bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    let addr: u32 = 0x80000000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | ((offset as u32) & 0xFC);
    unsafe {
        asm!("out dx, eax", in("dx") PCI_CONFIG_ADDR, in("eax") addr, options(nomem, nostack));
        asm!("out dx, eax", in("dx") PCI_CONFIG_DATA, in("eax") value, options(nomem, nostack));
    }
}

/// Write a 16-bit value to PCI configuration space.
pub fn config_write16(bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    let dword = config_read32(bus, device, function, offset & 0xFC);
    let shift = (offset & 2) * 8;
    let mask = !(0xFFFF << shift);
    let new_val = (dword & mask) | ((value as u32) << shift);
    config_write32(bus, device, function, offset & 0xFC, new_val);
}

/// Write an 8-bit value to PCI configuration space.
pub fn config_write8(bus: u8, device: u8, function: u8, offset: u8, value: u8) {
    let dword = config_read32(bus, device, function, offset & 0xFC);
    let shift = (offset & 3) * 8;
    let mask = !(0xFF << shift);
    let new_val = (dword & mask) | ((value as u32) << shift);
    config_write32(bus, device, function, offset & 0xFC, new_val);
}

// ══════════════════════════════════════════════════════════════════════════════
// Standard PCI header fields
// ══════════════════════════════════════════════════════════════════════════════

/// Read vendor ID (0xFFFF = no device)
pub fn vendor_id(bus: u8, dev: u8, func: u8) -> u16 {
    config_read16(bus, dev, func, 0x00)
}

/// Read device ID
pub fn device_id(bus: u8, dev: u8, func: u8) -> u16 {
    config_read16(bus, dev, func, 0x02)
}

/// Read class code (24-bit: class:subclass:progif)
pub fn class_code(bus: u8, dev: u8, func: u8) -> (u8, u8, u8) {
    let reg = config_read32(bus, dev, func, 0x08);
    let class = ((reg >> 24) & 0xFF) as u8;
    let subclass = ((reg >> 16) & 0xFF) as u8;
    let progif = ((reg >> 8) & 0xFF) as u8;
    (class, subclass, progif)
}

/// Read header type
pub fn header_type(bus: u8, dev: u8, func: u8) -> u8 {
    config_read8(bus, dev, func, 0x0E)
}

/// Read interrupt line
pub fn interrupt_line(bus: u8, dev: u8, func: u8) -> u8 {
    config_read8(bus, dev, func, 0x3C)
}

/// Read interrupt pin (0=none, 1=INTA, 2=INTB, 3=INTC, 4=INTD)
pub fn interrupt_pin(bus: u8, dev: u8, func: u8) -> u8 {
    config_read8(bus, dev, func, 0x3D)
}

/// Read command register
pub fn command(bus: u8, dev: u8, func: u8) -> u16 {
    config_read16(bus, dev, func, 0x04)
}

/// Write command register
pub fn set_command(bus: u8, dev: u8, func: u8, cmd: u16) {
    config_write16(bus, dev, func, 0x04, cmd);
}

/// Enable bus mastering and memory space access
pub fn enable_bus_master(bus: u8, dev: u8, func: u8) {
    let cmd = command(bus, dev, func);
    // Bit 1 = Memory Space, Bit 2 = Bus Master, Bit 10 = Interrupt Disable (clear it)
    let new_cmd = (cmd | 0x06) & !0x0400;
    set_command(bus, dev, func, new_cmd);
}

// ══════════════════════════════════════════════════════════════════════════════
// BAR (Base Address Register) decoding
// ══════════════════════════════════════════════════════════════════════════════

/// BAR types
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BarType {
    /// Memory-mapped I/O (base address, size, is_64bit, is_prefetchable)
    Mmio { base: u64, size: u64, is_64bit: bool, prefetchable: bool },
    /// I/O port space
    Io { base: u32, size: u32 },
    /// BAR not present or invalid
    None,
}

/// Read and decode a BAR register.
/// `bar_index` is 0-5 for a Type 0 header.
pub fn read_bar(bus: u8, dev: u8, func: u8, bar_index: u8) -> BarType {
    if bar_index > 5 { return BarType::None; }
    let offset = 0x10 + bar_index * 4;

    let original = config_read32(bus, dev, func, offset);
    if original == 0 { return BarType::None; }

    if original & 1 != 0 {
        // I/O BAR
        config_write32(bus, dev, func, offset, 0xFFFFFFFF);
        let size_mask = config_read32(bus, dev, func, offset);
        config_write32(bus, dev, func, offset, original);
        let base = original & 0xFFFFFFFC;
        let size = !(size_mask & 0xFFFFFFFC).wrapping_add(1);
        BarType::Io { base, size }
    } else {
        // Memory BAR
        let bar_type = (original >> 1) & 3;
        let prefetchable = (original & 0x08) != 0;

        if bar_type == 2 {
            // 64-bit BAR
            if bar_index >= 5 { return BarType::None; }
            let offset_hi = offset + 4;
            let original_hi = config_read32(bus, dev, func, offset_hi);

            config_write32(bus, dev, func, offset, 0xFFFFFFFF);
            config_write32(bus, dev, func, offset_hi, 0xFFFFFFFF);
            let size_lo = config_read32(bus, dev, func, offset);
            let size_hi = config_read32(bus, dev, func, offset_hi);
            config_write32(bus, dev, func, offset, original);
            config_write32(bus, dev, func, offset_hi, original_hi);

            let base = ((original_hi as u64) << 32) | ((original & 0xFFFFFFF0) as u64);
            let size_mask = ((size_hi as u64) << 32) | ((size_lo & 0xFFFFFFF0) as u64);
            let size = (!size_mask).wrapping_add(1);

            BarType::Mmio { base, size, is_64bit: true, prefetchable }
        } else {
            // 32-bit BAR
            config_write32(bus, dev, func, offset, 0xFFFFFFFF);
            let size_mask = config_read32(bus, dev, func, offset);
            config_write32(bus, dev, func, offset, original);

            let base = (original & 0xFFFFFFF0) as u64;
            let size = (!(size_mask & 0xFFFFFFF0) as u64).wrapping_add(1) & 0xFFFFFFFF;

            BarType::Mmio { base, size, is_64bit: false, prefetchable }
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// PCI Capability list
// ══════════════════════════════════════════════════════════════════════════════

/// Standard PCI capability IDs
pub const CAP_MSI: u8 = 0x05;
pub const CAP_MSIX: u8 = 0x11;
pub const CAP_PCIE: u8 = 0x10;

/// Find a PCI capability by ID. Returns the offset of the capability header, or None.
pub fn find_capability(bus: u8, dev: u8, func: u8, cap_id: u8) -> Option<u8> {
    // Check if capabilities list is supported (Status register bit 4)
    let status = config_read16(bus, dev, func, 0x06);
    if status & 0x10 == 0 { return None; }

    // Capabilities pointer is at offset 0x34
    let mut cap_ptr = config_read8(bus, dev, func, 0x34) & 0xFC;

    // Walk the linked list (max 48 iterations to prevent infinite loops)
    for _ in 0..48 {
        if cap_ptr == 0 { return None; }
        let id = config_read8(bus, dev, func, cap_ptr);
        if id == cap_id {
            return Some(cap_ptr);
        }
        cap_ptr = config_read8(bus, dev, func, cap_ptr + 1) & 0xFC;
    }
    None
}

// ══════════════════════════════════════════════════════════════════════════════
// PCI bus enumeration
// ══════════════════════════════════════════════════════════════════════════════

/// A discovered PCI device
#[derive(Debug, Clone, Copy)]
pub struct PciDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class: u8,
    pub subclass: u8,
    pub progif: u8,
}

/// Maximum devices we track
const MAX_PCI_DEVICES: usize = 64;
static mut PCI_DEVICES: [PciDevice; MAX_PCI_DEVICES] = [PciDevice {
    bus: 0, device: 0, function: 0,
    vendor_id: 0, device_id: 0,
    class: 0, subclass: 0, progif: 0,
}; MAX_PCI_DEVICES];
static mut PCI_DEVICE_COUNT: usize = 0;

/// Enumerate all PCI devices on bus 0-255.
/// Call once during boot (before drivers that need PCI).
pub fn enumerate() -> usize {
    unsafe { PCI_DEVICE_COUNT = 0; }

    for bus in 0u16..256 {
        for dev in 0u8..32 {
            let vid = vendor_id(bus as u8, dev, 0);
            if vid == 0xFFFF { continue; }

            add_device(bus as u8, dev, 0);

            // Check multi-function
            let ht = header_type(bus as u8, dev, 0);
            if ht & 0x80 != 0 {
                for func in 1u8..8 {
                    let vid = vendor_id(bus as u8, dev, func);
                    if vid != 0xFFFF {
                        add_device(bus as u8, dev, func);
                    }
                }
            }
        }
    }

    let count = unsafe { PCI_DEVICE_COUNT };
    crate::arch::x86_64::serial::write_str("[PCI] Enumerated ");
    serial_dec(count as u64);
    crate::arch::x86_64::serial::write_str(" devices\r\n");
    count
}

fn add_device(bus: u8, dev: u8, func: u8) {
    unsafe {
        if PCI_DEVICE_COUNT >= MAX_PCI_DEVICES { return; }
        let (class, subclass, progif) = class_code(bus, dev, func);
        PCI_DEVICES[PCI_DEVICE_COUNT] = PciDevice {
            bus, device: dev, function: func,
            vendor_id: vendor_id(bus, dev, func),
            device_id: device_id(bus, dev, func),
            class, subclass, progif,
        };
        PCI_DEVICE_COUNT += 1;
    }
}

/// Get the number of enumerated PCI devices
pub fn device_count() -> usize {
    unsafe { PCI_DEVICE_COUNT }
}

/// Get a reference to a PCI device by index
pub fn get_device(index: usize) -> Option<&'static PciDevice> {
    unsafe {
        if index < PCI_DEVICE_COUNT {
            Some(&PCI_DEVICES[index])
        } else {
            None
        }
    }
}

/// Find a device by class/subclass/progif. Returns the first match.
pub fn find_by_class(class: u8, subclass: u8, progif: u8) -> Option<&'static PciDevice> {
    unsafe {
        for i in 0..PCI_DEVICE_COUNT {
            let d = &PCI_DEVICES[i];
            if d.class == class && d.subclass == subclass && d.progif == progif {
                return Some(d);
            }
        }
        None
    }
}

/// Find all devices matching a class/subclass. Returns count and fills `out`.
pub fn find_all_by_class(class: u8, subclass: u8, out: &mut [PciDevice]) -> usize {
    let mut count = 0;
    unsafe {
        for i in 0..PCI_DEVICE_COUNT {
            let d = &PCI_DEVICES[i];
            if d.class == class && d.subclass == subclass {
                if count < out.len() {
                    out[count] = *d;
                    count += 1;
                }
            }
        }
    }
    count
}

// ══════════════════════════════════════════════════════════════════════════════
// Helpers
// ══════════════════════════════════════════════════════════════════════════════

fn serial_dec(mut val: u64) {
    if val == 0 {
        crate::arch::x86_64::serial::write_byte(b'0');
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
        crate::arch::x86_64::serial::write_byte(buf[j]);
    }
}
