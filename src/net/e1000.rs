/*
 * Intel e1000 / e1000e NIC driver for AETERNA
 *
 * Supports:
 *   8086:100E  — Intel 82545EM (QEMU -device e1000, VMware default)
 *   8086:100F  — Intel 82545EM (server variant)
 *   8086:10D3  — Intel 82574L / e1000e (VMware, newer bare metal)
 *   8086:10EA  — Intel 82577LM (Nehalem-era laptops)
 *   8086:1502  — Intel 82579LM (Sandy Bridge laptops — very common)
 *   8086:1503  — Intel 82579V
 *   8086:150C  — Intel 82583V
 *   8086:1533  — Intel I210 (modern)
 *
 * Uses MMIO (BAR0). Descriptors in static memory, accessed via HHDM.
 * 8 TX descriptors, 8 RX descriptors, each with 2K data buffers.
 */
#![allow(dead_code)]


// ─── PCI IDs ───────────────────────────────────────────────────────────────
const E1000_IDS: &[(u16, u16)] = &[
    (0x8086, 0x100E), // 82545EM — QEMU, VMware
    (0x8086, 0x100F), // 82545EM server
    (0x8086, 0x10D3), // 82574L — VMware, bare metal
    (0x8086, 0x10EA), // 82577LM — older laptops
    (0x8086, 0x1502), // 82579LM — Sandy Bridge laptops
    (0x8086, 0x1503), // 82579V
    (0x8086, 0x150C), // 82583V
    (0x8086, 0x1533), // I210
    (0x8086, 0x15A3), // I218-V
];

// ─── Register offsets (relative to MMIO base) ─────────────────────────────
const REG_CTRL:   u32 = 0x0000;
const REG_STATUS: u32 = 0x0008;
const REG_EECD:   u32 = 0x0010;
const REG_EEPROM: u32 = 0x0014;
const REG_ICR:    u32 = 0x00C0;
const REG_IMS:    u32 = 0x00D0;
const REG_IMC:    u32 = 0x00D8;
const REG_RCTL:   u32 = 0x0100;
const REG_TCTL:   u32 = 0x0400;
const REG_TIPG:   u32 = 0x0410;
const REG_RDBAL:  u32 = 0x2800;
const REG_RDBAH:  u32 = 0x2804;
const REG_RDLEN:  u32 = 0x2808;
const REG_RDH:    u32 = 0x2810;
const REG_RDT:    u32 = 0x2818;
const REG_TDBAL:  u32 = 0x3800;
const REG_TDBAH:  u32 = 0x3804;
const REG_TDLEN:  u32 = 0x3808;
const REG_TDH:    u32 = 0x3810;
const REG_TDT:    u32 = 0x3818;
const REG_MTA:    u32 = 0x5200; // Multicast table (128 * 4 bytes)
const REG_RAL0:   u32 = 0x5400;
const REG_RAH0:   u32 = 0x5404;

// CTRL bits
const CTRL_SLU:    u32 = 1 << 6;  // Set Link Up
const CTRL_RST:    u32 = 1 << 26; // Device Reset

// RCTL bits
const RCTL_EN:     u32 = 1 << 1;
const RCTL_BAM:    u32 = 1 << 15; // Broadcast Accept Mode
const RCTL_SECRC:  u32 = 1 << 26; // Strip Ethernet CRC
const RCTL_BSIZE_2K: u32 = 0 << 16; // Buffer size = 2048 (BSIZE=0, BSEX=0)

// TCTL bits
const TCTL_EN:     u32 = 1 << 1;
const TCTL_PSP:    u32 = 1 << 3;  // Pad Short Packets

// TX descriptor command bits
const TDESC_CMD_EOP:  u8 = 1 << 0; // End Of Packet
const TDESC_CMD_IFCS: u8 = 1 << 1; // Insert FCS
const TDESC_CMD_RS:   u8 = 1 << 3; // Report Status

// TX descriptor status bits
const TDESC_STA_DD: u8 = 1 << 0; // Descriptor Done

// RX descriptor status bits
const RDESC_STS_DD:  u8 = 1 << 0; // Descriptor Done
const RDESC_STS_EOP: u8 = 1 << 1; // End of Packet

// ─── Descriptor types (packed, 16 bytes each) ─────────────────────────────
#[repr(C, packed)]
#[derive(Copy, Clone, Default)]
struct TxDesc {
    addr:    u64,
    length:  u16,
    cso:     u8,
    cmd:     u8,
    status:  u8,
    css:     u8,
    special: u16,
}

#[repr(C, packed)]
#[derive(Copy, Clone, Default)]
struct RxDesc {
    addr:     u64,
    length:   u16,
    checksum: u16,
    status:   u8,
    errors:   u8,
    special:  u16,
}

// ─── Ring sizes ────────────────────────────────────────────────────────────
const TX_RING: usize = 8;
const RX_RING: usize = 8;
const RX_BUF_SIZE: usize = 2048;

// ─── Static state ──────────────────────────────────────────────────────────
static mut MMIO_BASE: usize = 0;
static mut MAC: [u8; 6] = [0; 6];
static mut INITIALIZED: bool = false;

// Descriptor rings
#[repr(C, align(16))]
struct TxRing([TxDesc; TX_RING]);
#[repr(C, align(16))]
struct RxRing([RxDesc; RX_RING]);

static mut TX_RING_BUF: TxRing = TxRing([TxDesc {
    addr: 0, length: 0, cso: 0, cmd: 0, status: 0, css: 0, special: 0
}; TX_RING]);
static mut RX_RING_BUF: RxRing = RxRing([RxDesc {
    addr: 0, length: 0, checksum: 0, status: 0, errors: 0, special: 0
}; RX_RING]);

// TX data buffers (one per descriptor)
static mut TX_BUFS: [[u8; 2048]; TX_RING] = [[0u8; 2048]; TX_RING];
// RX data buffers (one per descriptor)
static mut RX_BUFS: [[u8; RX_BUF_SIZE]; RX_RING] = [[0u8; RX_BUF_SIZE]; RX_RING];

// Scratch buffer for returning received data
static mut PACKET_OUT: [u8; 2048] = [0u8; 2048];

static mut TX_TAIL: usize = 0;
static mut RX_TAIL: usize = 0;

// ─── MMIO helpers ──────────────────────────────────────────────────────────
#[inline(always)]
fn read_reg(reg: u32) -> u32 {
    let addr = unsafe { MMIO_BASE + reg as usize };
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}

#[inline(always)]
fn write_reg(reg: u32, val: u32) {
    let addr = unsafe { MMIO_BASE + reg as usize };
    unsafe { core::ptr::write_volatile(addr as *mut u32, val); }
}

fn flush_reg(reg: u32) {
    let _ = read_reg(reg);
}

// ─── PCI helpers ───────────────────────────────────────────────────────────
fn pci_read(bus: u8, dev: u8, func: u8, offset: u8) -> u32 {
    let addr: u32 = 0x80000000
        | ((bus as u32) << 16)
        | ((dev as u32) << 11)
        | ((func as u32) << 8)
        | ((offset as u32) & 0xFC);
    unsafe {
        let val: u32;
        core::arch::asm!("out dx, eax", in("dx") 0x0CF8u16, in("eax") addr, options(nomem, nostack));
        core::arch::asm!("in eax, dx",  in("dx") 0x0CFCu16, out("eax") val, options(nomem, nostack));
        val
    }
}

fn pci_write(bus: u8, dev: u8, func: u8, offset: u8, val: u32) {
    let addr: u32 = 0x80000000
        | ((bus as u32) << 16)
        | ((dev as u32) << 11)
        | ((func as u32) << 8)
        | ((offset as u32) & 0xFC);
    unsafe {
        core::arch::asm!("out dx, eax", in("dx") 0x0CF8u16, in("eax") addr, options(nomem, nostack));
        core::arch::asm!("out dx, eax", in("dx") 0x0CFCu16, in("eax") val, options(nomem, nostack));
    }
}

fn virt_to_phys(vaddr: usize) -> u64 {
    // Kernel virtual offset = KERNEL_VIRT - KERNEL_PHYS = 0xffffffff80000000
    (vaddr as u64).wrapping_sub(0xffff_ffff_8000_0000_u64)
}

fn phys_to_virt(phys: u64) -> usize {
    let hhdm = crate::arch::x86_64::boot::hhdm_offset().unwrap_or(0xFFFF_8000_0000_0000);
    (phys + hhdm) as usize
}

/// Busy-wait loop
fn delay(count: u32) {
    for _ in 0..count {
        unsafe { core::arch::asm!("pause"); }
    }
}

// ─── EEPROM (for MAC address on older e1000) ──────────────────────────────
fn eeprom_detect() -> bool {
    write_reg(REG_EEPROM, 0x01);
    for _ in 0..1000 {
        let v = read_reg(REG_EEPROM);
        if v & 0x10 != 0 { return true; }
    }
    false
}

fn eeprom_read(addr: u8) -> u16 {
    // Start read cycle
    write_reg(REG_EEPROM, 1 | ((addr as u32) << 8));
    // Wait for done bit
    let mut timeout = 100000u32;
    loop {
        let v = read_reg(REG_EEPROM);
        if v & (1 << 4) != 0 { return ((v >> 16) & 0xFFFF) as u16; }
        timeout -= 1;
        if timeout == 0 { return 0; }
        delay(1);
    }
}

// ─── Probe and initialize ──────────────────────────────────────────────────
pub fn probe_and_init() -> bool {
    for bus in 0u8..16 {
        for dev in 0u8..32 {
            let id = pci_read(bus, dev, 0, 0x00);
            let vendor = (id & 0xFFFF) as u16;
            let device = ((id >> 16) & 0xFFFF) as u16;
            let is_e1000 = E1000_IDS.iter().any(|&(v, d)| v == vendor && d == device);
            if is_e1000 {
                return init_device(bus, dev, 0, vendor, device);
            }
        }
    }
    false
}

fn init_device(bus: u8, dev: u8, func: u8, vendor: u16, device: u16) -> bool {
    // --- Enable Bus Master + MMIO in PCI command register ---
    let cmd = pci_read(bus, dev, func, 0x04);
    pci_write(bus, dev, func, 0x04, cmd | 0b110); // MMIO + Bus Master

    // --- Read BAR0 (MMIO, 32-bit or 64-bit) ---
    let bar0 = pci_read(bus, dev, func, 0x10);
    if bar0 & 1 != 0 {
        crate::arch::x86_64::serial::write_str("[e1000] BAR0 is I/O — unexpected, skipping\r\n");
        return false; // e1000 should always be MMIO
    }
    let bar_type = (bar0 >> 1) & 0x3;
    let mmio_phys: u64 = if bar_type == 2 {
        // 64-bit BAR
        let bar0_hi = pci_read(bus, dev, func, 0x14);
        ((bar0 & !0xF) as u64) | ((bar0_hi as u64) << 32)
    } else {
        (bar0 & !0xF) as u64
    };

    if mmio_phys == 0 {
        return false;
    }

    let irq = (pci_read(bus, dev, func, 0x3C) & 0xFF) as u8;

    unsafe {
        // Map physical MMIO through HHDM
        MMIO_BASE = phys_to_virt(mmio_phys);
    }

    crate::arch::x86_64::serial::write_str("[e1000] Found ");
    serial_hex16(vendor);
    crate::arch::x86_64::serial::write_byte(b':');
    serial_hex16(device);
    crate::arch::x86_64::serial::write_str(", MMIO phys=0x");
    serial_hex32(mmio_phys as u32);
    crate::arch::x86_64::serial::write_str(", IRQ=");
    serial_dec(irq as u64);
    crate::arch::x86_64::serial::write_str("\r\n");

    // --- Reset device ---
    let ctrl = read_reg(REG_CTRL);
    write_reg(REG_CTRL, ctrl | CTRL_RST);
    delay(10000);
    // Wait for reset to clear
    let mut timeout = 100000u32;
    while read_reg(REG_CTRL) & CTRL_RST != 0 && timeout > 0 {
        timeout -= 1;
        delay(1);
    }
    if timeout == 0 {
        crate::arch::x86_64::serial::write_str("[e1000] Reset timeout\r\n");
        return false;
    }
    delay(5000);

    // --- Disable interrupts initially ---
    write_reg(REG_IMC, 0xFFFFFFFF);
    let _ = read_reg(REG_ICR); // clear pending

    // --- Set link up ---
    write_reg(REG_CTRL, read_reg(REG_CTRL) | CTRL_SLU);

    // --- Read MAC address ---
    let has_eeprom = eeprom_detect();
    let mac_bytes = if has_eeprom {
        // Read from EEPROM
        let w0 = eeprom_read(0);
        let w1 = eeprom_read(1);
        let w2 = eeprom_read(2);
        [
            (w0 & 0xFF) as u8, ((w0 >> 8) & 0xFF) as u8,
            (w1 & 0xFF) as u8, ((w1 >> 8) & 0xFF) as u8,
            (w2 & 0xFF) as u8, ((w2 >> 8) & 0xFF) as u8,
        ]
    } else {
        // Read from RAL/RAH
        let ral = read_reg(REG_RAL0);
        let rah = read_reg(REG_RAH0);
        [
            (ral & 0xFF) as u8, ((ral >> 8) & 0xFF) as u8,
            ((ral >> 16) & 0xFF) as u8, ((ral >> 24) & 0xFF) as u8,
            (rah & 0xFF) as u8, ((rah >> 8) & 0xFF) as u8,
        ]
    };
    unsafe { MAC = mac_bytes; }

    crate::arch::x86_64::serial::write_str("[e1000] MAC: ");
    for i in 0..6 {
        serial_hex_byte(mac_bytes[i]);
        if i < 5 { crate::arch::x86_64::serial::write_byte(b':'); }
    }
    crate::arch::x86_64::serial::write_str("\r\n");

    // --- Clear multicast table ---
    for i in 0..(128u32) {
        write_reg(REG_MTA + i * 4, 0);
    }

    // --- Set up TX ring ---
    unsafe {
        // Zero all descriptors
        for i in 0..TX_RING {
            TX_RING_BUF.0[i] = TxDesc::default();
            // Pre-assign TX buffer physical addresses
            TX_RING_BUF.0[i].addr = virt_to_phys(TX_BUFS[i].as_ptr() as usize);
        }
        let tx_phys = virt_to_phys(TX_RING_BUF.0.as_ptr() as usize);
        write_reg(REG_TDBAL, tx_phys as u32);
        write_reg(REG_TDBAH, (tx_phys >> 32) as u32);
        write_reg(REG_TDLEN, (TX_RING * 16) as u32);
        write_reg(REG_TDH, 0);
        write_reg(REG_TDT, 0);
        TX_TAIL = 0;
    }

    // TCTL: enable TX, pad short packets, CT=15, COLD=64
    write_reg(REG_TCTL, TCTL_EN | TCTL_PSP | (0x0F << 4) | (0x40 << 12));
    // TIPG: recommended values for IFG
    write_reg(REG_TIPG, 0x0060200A);

    // --- Set up RX ring ---
    unsafe {
        for i in 0..RX_RING {
            RX_RING_BUF.0[i] = RxDesc::default();
            RX_RING_BUF.0[i].addr = virt_to_phys(RX_BUFS[i].as_ptr() as usize);
            RX_RING_BUF.0[i].status = 0; // mark as ready to receive
        }
        let rx_phys = virt_to_phys(RX_RING_BUF.0.as_ptr() as usize);
        write_reg(REG_RDBAL, rx_phys as u32);
        write_reg(REG_RDBAH, (rx_phys >> 32) as u32);
        write_reg(REG_RDLEN, (RX_RING * 16) as u32);
        write_reg(REG_RDH, 0);
        write_reg(REG_RDT, (RX_RING - 1) as u32); // tail points to last descriptor initially
        RX_TAIL = 0;
    }

    // RCTL: enable, accept broadcast, 2K buffer, strip CRC
    write_reg(REG_RCTL, RCTL_EN | RCTL_BAM | RCTL_BSIZE_2K | RCTL_SECRC);

    // --- Set receive address ---
    let ral = (mac_bytes[0] as u32)
        | ((mac_bytes[1] as u32) << 8)
        | ((mac_bytes[2] as u32) << 16)
        | ((mac_bytes[3] as u32) << 24);
    let rah = (mac_bytes[4] as u32)
        | ((mac_bytes[5] as u32) << 8)
        | (1u32 << 31); // AV (address valid)
    write_reg(REG_RAL0, ral);
    write_reg(REG_RAH0, rah);

    // --- Enable RX/TX interrupts ---
    write_reg(REG_IMS, 0x04 | 0x80); // RXDMT0 + RXT0

    // Enable IRQ in PIC
    if irq < 16 {
        crate::arch::x86_64::pic::enable_irq(irq);
    }

    unsafe { INITIALIZED = true; }
    crate::arch::x86_64::serial::write_str("[e1000] Initialized OK\r\n");
    true
}

// ─── Public API ───────────────────────────────────────────────────────────

pub fn mac_address() -> [u8; 6] {
    unsafe { MAC }
}

pub fn is_initialized() -> bool {
    unsafe { INITIALIZED }
}

/// Transmit a packet
pub fn send_packet(data: &[u8], len: usize) {
    if !is_initialized() || len == 0 || len > 1514 { return; }

    unsafe {
        let idx = TX_TAIL;
        let desc = &mut TX_RING_BUF.0[idx];

        // Copy data into TX buffer
        TX_BUFS[idx][..len].copy_from_slice(&data[..len]);

        // Fill descriptor
        desc.addr   = virt_to_phys(TX_BUFS[idx].as_ptr() as usize);
        desc.length  = len as u16;
        desc.cso    = 0;
        desc.cmd    = TDESC_CMD_EOP | TDESC_CMD_IFCS | TDESC_CMD_RS;
        desc.status = 0;
        desc.css    = 0;
        desc.special = 0;

        // Advance tail
        TX_TAIL = (TX_TAIL + 1) % TX_RING;
        write_reg(REG_TDT, TX_TAIL as u32);

        // Wait for descriptor done (TX_RING - 1 ahead)
        let mut timeout = 500000u32;
        while TX_RING_BUF.0[idx].status & TDESC_STA_DD == 0 && timeout > 0 {
            timeout -= 1;
            core::arch::asm!("pause");
        }
    }
}

/// Receive a packet (if available). Returns (data slice, length) or None.
pub fn receive_packet() -> Option<(&'static [u8], usize)> {
    if !is_initialized() { return None; }

    unsafe {
        let idx = RX_TAIL;
        let desc = &mut RX_RING_BUF.0[idx];

        // Check DD (Descriptor Done) bit
        if desc.status & RDESC_STS_DD == 0 {
            return None;
        }

        let len = desc.length as usize;
        if len < 14 || len > 2048 {
            // Bad packet — reset descriptor
            desc.status = 0;
            write_reg(REG_RDT, idx as u32);
            RX_TAIL = (RX_TAIL + 1) % RX_RING;
            return None;
        }

        // Copy to output buffer
        let copy_len = len.min(2048);
        PACKET_OUT[..copy_len].copy_from_slice(&RX_BUFS[idx][..copy_len]);

        // Reset descriptor for reuse
        desc.addr   = virt_to_phys(RX_BUFS[idx].as_ptr() as usize);
        desc.status = 0;
        desc.errors = 0;
        desc.length = 0;

        // Advance RDT to give hardware the descriptor back
        write_reg(REG_RDT, idx as u32);
        RX_TAIL = (RX_TAIL + 1) % RX_RING;

        Some((&PACKET_OUT[..copy_len], copy_len))
    }
}

/// Handle IRQ — clear interrupt cause
pub fn handle_irq() {
    if is_initialized() {
        let _ = read_reg(REG_ICR); // reading clears interrupts
    }
}

// ─── Serial debug helpers ─────────────────────────────────────────────────
fn serial_hex_byte(v: u8) {
    let h = b"0123456789abcdef";
    crate::arch::x86_64::serial::write_byte(h[(v >> 4) as usize]);
    crate::arch::x86_64::serial::write_byte(h[(v & 0xF) as usize]);
}
fn serial_hex16(v: u16) {
    serial_hex_byte((v >> 8) as u8);
    serial_hex_byte(v as u8);
}
fn serial_hex32(v: u32) {
    serial_hex_byte((v >> 24) as u8);
    serial_hex_byte((v >> 16) as u8);
    serial_hex_byte((v >> 8) as u8);
    serial_hex_byte(v as u8);
}
fn serial_dec(mut v: u64) {
    if v == 0 { crate::arch::x86_64::serial::write_byte(b'0'); return; }
    let mut buf = [0u8; 20];
    let mut i = 0;
    while v > 0 { buf[i] = b'0' + (v % 10) as u8; v /= 10; i += 1; }
    for j in (0..i).rev() { crate::arch::x86_64::serial::write_byte(buf[j]); }
}
