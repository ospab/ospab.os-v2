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
 * 32 TX descriptors, 32 RX descriptors, each with 2K data buffers.
 *
 * DMA address translation:
 *   Kernel statics live in .bss at high virtual addresses.
 *   Limine may relocate the kernel (KASLR), so the physical base is
 *   obtained at runtime from the Executable Address Request.
 *   phys = virt - (virtual_base - physical_base).
 *   This is validated at init time by cross-checking HHDM.
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
const CTRL_ASDE:   u32 = 1 << 5;  // Auto-Speed Detection Enable

// STATUS bits
const STATUS_LU:   u32 = 1 << 1;  // Link Up indication

// RCTL bits
const RCTL_EN:     u32 = 1 << 1;
const RCTL_UPE:    u32 = 1 << 3;  // Unicast Promiscuous Enable
const RCTL_MPE:    u32 = 1 << 4;  // Multicast Promiscuous Enable
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
const TX_RING: usize = 32;
const RX_RING: usize = 32;
const RX_BUF_SIZE: usize = 2048;

// ─── Static state ──────────────────────────────────────────────────────────
static mut MMIO_BASE: usize = 0;
static mut MAC: [u8; 6] = [0; 6];
static mut INITIALIZED: bool = false;

// Descriptor rings — must be 16-byte aligned, which #[repr(C, align(128))]
// guarantees (128 is a multiple of 16, also helps cache-line alignment).
#[repr(C, align(128))]
struct TxRing([TxDesc; TX_RING]);
#[repr(C, align(128))]
struct RxRing([RxDesc; RX_RING]);

static mut TX_RING_BUF: TxRing = TxRing([TxDesc {
    addr: 0, length: 0, cso: 0, cmd: 0, status: 0, css: 0, special: 0
}; TX_RING]);
static mut RX_RING_BUF: RxRing = RxRing([RxDesc {
    addr: 0, length: 0, checksum: 0, status: 0, errors: 0, special: 0
}; RX_RING]);

// TX data buffers (one per descriptor, 16-byte aligned)
#[repr(C, align(16))]
struct Buf2K([u8; 2048]);
static mut TX_BUFS: [Buf2K; TX_RING] = {
    const ZERO: Buf2K = Buf2K([0u8; 2048]);
    [ZERO; TX_RING]
};
// RX data buffers (one per descriptor, 16-byte aligned)
static mut RX_BUFS: [Buf2K; RX_RING] = {
    const ZERO: Buf2K = Buf2K([0u8; 2048]);
    [ZERO; RX_RING]
};

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

/// Translate kernel-section virtual address to physical.
/// Uses the actual kernel load address from Limine (handles KASLR/relocation).
fn virt_to_phys(vaddr: usize) -> u64 {
    let offset = crate::arch::x86_64::boot::kernel_virt_offset();
    (vaddr as u64).wrapping_sub(offset)
}

fn phys_to_virt(phys: u64) -> usize {
    let hhdm = crate::arch::x86_64::boot::hhdm_offset().unwrap_or(0xFFFF_8000_0000_0000);
    (phys + hhdm) as usize
}

/// Busy-wait delay using PIT ticks (each tick = 10ms at 100Hz).
/// `ms` is approximate milliseconds.
fn delay_ms(ms: u32) {
    let ticks = (ms / 10).max(1) as u64;
    let start = crate::arch::x86_64::idt::timer_ticks();
    while crate::arch::x86_64::idt::timer_ticks().wrapping_sub(start) < ticks {
        unsafe { core::arch::asm!("pause"); }
    }
}

// ─── EEPROM (for MAC address on older e1000) ──────────────────────────────
fn eeprom_detect() -> bool {
    write_reg(REG_EEPROM, 0x01);
    for _ in 0..10000 {
        let v = read_reg(REG_EEPROM);
        if v & 0x10 != 0 { return true; }
        unsafe { core::arch::asm!("pause"); }
    }
    false
}

fn eeprom_read(addr: u8) -> u16 {
    write_reg(REG_EEPROM, 1 | ((addr as u32) << 8));
    let mut timeout = 1000000u32;
    loop {
        let v = read_reg(REG_EEPROM);
        if v & (1 << 4) != 0 { return ((v >> 16) & 0xFFFF) as u16; }
        timeout -= 1;
        if timeout == 0 { return 0; }
        unsafe { core::arch::asm!("pause"); }
    }
}

// ─── Probe and initialize ──────────────────────────────────────────────────
pub fn probe_and_init() -> bool {
    // Use the kernel PCI table first (fast path)
    for &(vid, did) in E1000_IDS {
        if let Some(dev) = crate::pci::find_by_vendor_device(vid, did) {
            return init_device(dev.bus, dev.device, dev.function, vid, did);
        }
    }
    // Fallback: manual scan of first 16 buses
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
    let s = crate::arch::x86_64::serial::write_str;

    // --- Enable Bus Master + MMIO in PCI command register ---
    let cmd = pci_read(bus, dev, func, 0x04);
    pci_write(bus, dev, func, 0x04, cmd | 0x07); // I/O + MMIO + Bus Master

    // --- Read BAR0 (MMIO, 32-bit or 64-bit) ---
    let bar0 = pci_read(bus, dev, func, 0x10);
    if bar0 & 1 != 0 {
        s("[e1000] BAR0 is I/O — unexpected, skipping\r\n");
        return false;
    }
    let bar_type = (bar0 >> 1) & 0x3;
    let mmio_phys: u64 = if bar_type == 2 {
        let bar0_hi = pci_read(bus, dev, func, 0x14);
        ((bar0 & !0xF) as u64) | ((bar0_hi as u64) << 32)
    } else {
        (bar0 & !0xF) as u64
    };

    if mmio_phys == 0 {
        s("[e1000] BAR0 is zero\r\n");
        return false;
    }

    let irq = (pci_read(bus, dev, func, 0x3C) & 0xFF) as u8;

    unsafe {
        MMIO_BASE = phys_to_virt(mmio_phys);
    }

    s("[e1000] Found ");
    serial_hex16(vendor);
    crate::arch::x86_64::serial::write_byte(b':');
    serial_hex16(device);
    s(", MMIO phys=0x");
    serial_hex32(mmio_phys as u32);
    s(", IRQ=");
    serial_dec(irq as u64);
    s("\r\n");

    // --- Reset device ---
    let ctrl = read_reg(REG_CTRL);
    write_reg(REG_CTRL, ctrl | CTRL_RST);
    // MUST wait at least 1ms for reset completion (Intel SDM 14.4)
    delay_ms(20);
    // Also poll for RST bit to clear
    let mut timeout = 1000u32;
    while read_reg(REG_CTRL) & CTRL_RST != 0 && timeout > 0 {
        timeout -= 1;
        delay_ms(1);
    }
    if timeout == 0 {
        s("[e1000] Reset timeout\r\n");
        return false;
    }
    delay_ms(20); // extra stabilization after reset

    // --- Disable ALL interrupts while configuring ---
    write_reg(REG_IMC, 0xFFFFFFFF);
    flush_reg(REG_STATUS);
    let _ = read_reg(REG_ICR); // clear any pending

    // --- Set link up (SLU + ASDE) ---
    let ctrl_val = read_reg(REG_CTRL);
    write_reg(REG_CTRL, (ctrl_val | CTRL_SLU | CTRL_ASDE) & !CTRL_RST);

    // --- Wait for link up (up to 3 seconds) ---
    s("[e1000] Waiting for link...\r\n");
    let mut link_up = false;
    for _ in 0..300 {
        let status = read_reg(REG_STATUS);
        if status & STATUS_LU != 0 {
            link_up = true;
            break;
        }
        delay_ms(10);
    }
    if link_up {
        s("[e1000] Link UP\r\n");
    } else {
        s("[e1000] Link DOWN — continuing anyway (VMware may come up later)\r\n");
    }

    // --- Read MAC address ---
    let has_eeprom = eeprom_detect();
    let mac_bytes = if has_eeprom {
        let w0 = eeprom_read(0);
        let w1 = eeprom_read(1);
        let w2 = eeprom_read(2);
        [
            (w0 & 0xFF) as u8, ((w0 >> 8) & 0xFF) as u8,
            (w1 & 0xFF) as u8, ((w1 >> 8) & 0xFF) as u8,
            (w2 & 0xFF) as u8, ((w2 >> 8) & 0xFF) as u8,
        ]
    } else {
        let ral = read_reg(REG_RAL0);
        let rah = read_reg(REG_RAH0);
        [
            (ral & 0xFF) as u8, ((ral >> 8) & 0xFF) as u8,
            ((ral >> 16) & 0xFF) as u8, ((ral >> 24) & 0xFF) as u8,
            (rah & 0xFF) as u8, ((rah >> 8) & 0xFF) as u8,
        ]
    };
    unsafe { MAC = mac_bytes; }

    s("[e1000] MAC: ");
    for i in 0..6 {
        serial_hex_byte(mac_bytes[i]);
        if i < 5 { crate::arch::x86_64::serial::write_byte(b':'); }
    }
    s("\r\n");

    // --- Clear multicast table ---
    for i in 0..(128u32) {
        write_reg(REG_MTA + i * 4, 0);
    }

    // --- Disable TX and RX while programming descriptors ---
    write_reg(REG_TCTL, 0);
    write_reg(REG_RCTL, 0);

    // =============================================================
    // TX RING SETUP
    // =============================================================
    unsafe {
        for i in 0..TX_RING {
            TX_RING_BUF.0[i] = TxDesc::default();
            TX_RING_BUF.0[i].addr = virt_to_phys(TX_BUFS[i].0.as_ptr() as usize);
            TX_RING_BUF.0[i].status = TDESC_STA_DD; // mark as "done" (available)
        }
        let tx_phys = virt_to_phys(TX_RING_BUF.0.as_ptr() as usize);
        write_reg(REG_TDBAL, tx_phys as u32);
        write_reg(REG_TDBAH, (tx_phys >> 32) as u32);
        write_reg(REG_TDLEN, (TX_RING * 16) as u32);
        write_reg(REG_TDH, 0);
        write_reg(REG_TDT, 0);
        TX_TAIL = 0;

        // Log TX ring addresses for debug
        s("[e1000] TX ring phys=0x");
        serial_hex64(tx_phys);
        s(" virt=0x");
        serial_hex64(TX_RING_BUF.0.as_ptr() as u64);
        s(" len=");
        serial_dec((TX_RING * 16) as u64);
        s("\r\n");
        s("[e1000] TX buf[0] phys=0x");
        serial_hex64(TX_RING_BUF.0[0].addr);
        s("\r\n");
    }

    // Enable TX: EN + Pad Short Packets, CT=0x0F, COLD=0x040 (full duplex)
    write_reg(REG_TCTL, TCTL_EN | TCTL_PSP | (0x0F << 4) | (0x040 << 12));
    // TIPG: recommended values
    write_reg(REG_TIPG, 0x0060200A);

    // =============================================================
    // RX RING SETUP
    // =============================================================
    unsafe {
        for i in 0..RX_RING {
            RX_RING_BUF.0[i] = RxDesc::default();
            RX_RING_BUF.0[i].addr = virt_to_phys(RX_BUFS[i].0.as_ptr() as usize);
            RX_RING_BUF.0[i].status = 0; // ready to receive
        }
        let rx_phys = virt_to_phys(RX_RING_BUF.0.as_ptr() as usize);
        write_reg(REG_RDBAL, rx_phys as u32);
        write_reg(REG_RDBAH, (rx_phys >> 32) as u32);
        write_reg(REG_RDLEN, (RX_RING * 16) as u32);
        write_reg(REG_RDH, 0);
        // RDT = RX_RING - 1: tell hardware it may use all descriptors
        write_reg(REG_RDT, (RX_RING - 1) as u32);
        RX_TAIL = 0;

        s("[e1000] RX ring phys=0x");
        serial_hex64(rx_phys);
        s(" virt=0x");
        serial_hex64(RX_RING_BUF.0.as_ptr() as u64);
        s(" len=");
        serial_dec((RX_RING * 16) as u64);
        s("\r\n");
        s("[e1000] RX buf[0] phys=0x");
        serial_hex64(RX_RING_BUF.0[0].addr);
        s("\r\n");
    }

    // --- Write receive address (RAL/RAH) ---
    let ral = (mac_bytes[0] as u32)
        | ((mac_bytes[1] as u32) << 8)
        | ((mac_bytes[2] as u32) << 16)
        | ((mac_bytes[3] as u32) << 24);
    let rah = (mac_bytes[4] as u32)
        | ((mac_bytes[5] as u32) << 8)
        | (1u32 << 31); // AV (address valid)
    write_reg(REG_RAL0, ral);
    write_reg(REG_RAH0, rah);

    // Enable RX: EN + Broadcast Accept + 2K buffers + Strip CRC + Unicast Promiscuous
    // UPE ensures we see ALL unicast frames (not just those matching RAL).
    // This is critical for VMware where MAC matching can be inconsistent.
    write_reg(REG_RCTL, RCTL_EN | RCTL_BAM | RCTL_UPE | RCTL_BSIZE_2K | RCTL_SECRC);

    // --- Enable useful interrupts ---
    // LSC (link), RXT0 (rx timer), RXO (rx overrun), RXDMT0 (rx desc low), TXDW (tx done)
    write_reg(REG_IMS, 0x01 | 0x04 | 0x10 | 0x40 | 0x80);
    let _ = read_reg(REG_ICR); // clear any pending

    if irq < 16 {
        crate::arch::x86_64::pic::enable_irq(irq);
        s("[e1000] PIC IRQ ");
        serial_dec(irq as u64);
        s(" enabled\r\n");
    } else {
        s("[e1000] No legacy IRQ (");
        serial_dec(irq as u64);
        s(") — using poll mode\r\n");
    }

    // --- Verification: read back critical registers ---
    let status = read_reg(REG_STATUS);
    let ctrl_final = read_reg(REG_CTRL);
    let rctl_val = read_reg(REG_RCTL);
    let tctl_val = read_reg(REG_TCTL);
    s("[e1000] STATUS=0x");
    serial_hex32(status);
    s(" CTRL=0x");
    serial_hex32(ctrl_final);
    s(" RCTL=0x");
    serial_hex32(rctl_val);
    s(" TCTL=0x");
    serial_hex32(tctl_val);
    s("\r\n");

    // Check that link is up
    if status & STATUS_LU != 0 {
        s("[e1000] Link confirmed UP, speed=");
        let speed = (status >> 6) & 0x3;
        match speed {
            0 => s("10Mbps"),
            1 => s("100Mbps"),
            2 | 3 => s("1000Mbps"),
            _ => s("?"),
        }
        s("\r\n");
    }

    unsafe { INITIALIZED = true; }
    s("[e1000] Initialized OK\r\n");
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

    // ─── TX Trace ───────────────────────────────────────────────────────────
    {
        let s = crate::arch::x86_64::serial::write_str;
        s("[NET] TX: size=");
        serial_dec(len as u64);
        if len >= 14 {
            let et = u16::from_be_bytes([data[12], data[13]]);
            if et == 0x0800 && len >= 34 {
                let proto = data[23];
                let dst   = [data[30], data[31], data[32], data[33]];
                s(", proto=");
                match proto {
                    1  => s("ICMP"),
                    6  => s("TCP"),
                    17 => s("UDP"),
                    _  => { s("0x"); serial_hex_byte(proto); }
                }
                s(", dest=");
                serial_ip(dst);
            } else if et == 0x0806 {
                s(", proto=ARP");
                if len >= 42 {
                    let target_ip = [data[38], data[39], data[40], data[41]];
                    s(", target=");
                    serial_ip(target_ip);
                }
            } else {
                s(", et=0x"); serial_hex16(et);
            }
        }
        s("\r\n");
    }

    unsafe {
        let idx = TX_TAIL;
        let desc = &mut TX_RING_BUF.0[idx];

        // Copy data into the TX buffer
        TX_BUFS[idx].0[..len].copy_from_slice(&data[..len]);

        // Fill descriptor — re-compute address each time to be safe
        desc.addr    = virt_to_phys(TX_BUFS[idx].0.as_ptr() as usize);
        desc.length  = len as u16;
        desc.cso     = 0;
        desc.cmd     = TDESC_CMD_EOP | TDESC_CMD_IFCS | TDESC_CMD_RS;
        desc.status  = 0;
        desc.css     = 0;
        desc.special = 0;

        // Advance tail — this kicks the NIC to transmit
        TX_TAIL = (TX_TAIL + 1) % TX_RING;
        write_reg(REG_TDT, TX_TAIL as u32);

        // Wait for descriptor done (with proper timeout)
        let start = crate::arch::x86_64::idt::timer_ticks();
        loop {
            let st = core::ptr::read_volatile(&desc.status as *const u8);
            if st & TDESC_STA_DD != 0 { break; }
            if crate::arch::x86_64::idt::timer_ticks().wrapping_sub(start) > 50 {
                crate::arch::x86_64::serial::write_str("[e1000] TX timeout! TDH=");
                serial_dec(read_reg(REG_TDH) as u64);
                crate::arch::x86_64::serial::write_str(" TDT=");
                serial_dec(read_reg(REG_TDT) as u64);
                crate::arch::x86_64::serial::write_str("\r\n");
                break;
            }
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

        // Use volatile read for the status — hardware DMA writes this
        let status = core::ptr::read_volatile(&desc.status as *const u8);

        // Check DD (Descriptor Done) bit
        if status & RDESC_STS_DD == 0 {
            return None;
        }

        let len = core::ptr::read_unaligned(core::ptr::addr_of!(desc.length)) as usize;
        if len < 14 || len > 2048 {
            // Bad packet — reset descriptor
            core::ptr::write_volatile(&mut desc.status as *mut u8, 0);
            let old_tail = idx;
            RX_TAIL = (RX_TAIL + 1) % RX_RING;
            write_reg(REG_RDT, old_tail as u32);
            return None;
        }

        // Copy to output buffer
        let copy_len = len.min(2048);
        core::ptr::copy_nonoverlapping(
            RX_BUFS[idx].0.as_ptr(),
            PACKET_OUT.as_mut_ptr(),
            copy_len,
        );

        // ─── RX Trace ─────────────────────────────────────────────────────
        {
            let s = crate::arch::x86_64::serial::write_str;
            let et = if copy_len >= 14 {
                u16::from_be_bytes([PACKET_OUT[12], PACKET_OUT[13]])
            } else { 0 };
            s("[NET] RX: Type=0x");
            serial_hex16(et);
            s(", len=");
            serial_dec(copy_len as u64);
            match et {
                0x0800 => {
                    s(" (IPv4)");
                    if copy_len >= 34 {
                        let proto = PACKET_OUT[23];
                        let src = [PACKET_OUT[26], PACKET_OUT[27], PACKET_OUT[28], PACKET_OUT[29]];
                        s(", proto=");
                        match proto {
                            1  => s("ICMP"),
                            6  => s("TCP"),
                            17 => s("UDP"),
                            _  => { s("0x"); serial_hex_byte(proto); }
                        }
                        s(", from=");
                        serial_ip(src);
                    }
                }
                0x0806 => s(" (ARP)"),
                0x86DD => s(" (IPv6)"),
                _      => {}
            }
            s("\r\n");
        }

        // Reset descriptor for reuse
        desc.addr   = virt_to_phys(RX_BUFS[idx].0.as_ptr() as usize);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(desc.status), 0);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(desc.errors), 0);
        core::ptr::write_unaligned(core::ptr::addr_of_mut!(desc.length), 0u16);

        // Give this descriptor back to hardware and advance
        let old_tail = idx;
        RX_TAIL = (RX_TAIL + 1) % RX_RING;
        write_reg(REG_RDT, old_tail as u32);

        Some((&PACKET_OUT[..copy_len], copy_len))
    }
}

/// Handle IRQ — clear interrupt cause and log it
pub fn handle_irq() {
    if is_initialized() {
        let icr = read_reg(REG_ICR); // reading clears interrupts
        if icr == 0 { return; } // spurious
        let s = crate::arch::x86_64::serial::write_str;
        s("[e1000] IRQ ICR=0x");
        serial_hex32(icr);
        if icr & 0x01 != 0 { s(" TXDW"); }
        if icr & 0x02 != 0 { s(" TXQE"); }
        if icr & 0x04 != 0 { s(" LSC"); }
        if icr & 0x10 != 0 { s(" RXDMT0"); }
        if icr & 0x40 != 0 { s(" RXO"); }
        if icr & 0x80 != 0 { s(" RXT0"); }
        s("\r\n");
    }
}

/// Diagnostic: return (STATUS, ICR, RDH, RDT) for e1000
pub fn diag_regs() -> (u32, u32, u32, u32) {
    if !is_initialized() { return (0, 0, 0, 0); }
    (
        read_reg(REG_STATUS),
        read_reg(REG_ICR),
        read_reg(REG_RDH),
        read_reg(REG_RDT),
    )
}

/// Check hardware link state.
/// e1000 STATUS register bit 1 (LU) = Link Up.
pub fn link_up() -> bool {
    if !is_initialized() { return false; }
    (read_reg(REG_STATUS) & STATUS_LU) != 0
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
fn serial_hex64(v: u64) {
    serial_hex32((v >> 32) as u32);
    serial_hex32(v as u32);
}
fn serial_dec(mut v: u64) {
    if v == 0 { crate::arch::x86_64::serial::write_byte(b'0'); return; }
    let mut buf = [0u8; 20];
    let mut i = 0;
    while v > 0 { buf[i] = b'0' + (v % 10) as u8; v /= 10; i += 1; }
    for j in (0..i).rev() { crate::arch::x86_64::serial::write_byte(buf[j]); }
}
fn serial_ip(ip: [u8; 4]) {
    for i in 0..4 {
        let mut v = ip[i];
        if v >= 100 { crate::arch::x86_64::serial::write_byte(b'0' + v/100); v %= 100;
                      crate::arch::x86_64::serial::write_byte(b'0' + v/10);  v %= 10; }
        else if v >= 10 { crate::arch::x86_64::serial::write_byte(b'0' + v/10); v %= 10; }
        crate::arch::x86_64::serial::write_byte(b'0' + v);
        if i < 3 { crate::arch::x86_64::serial::write_byte(b'.'); }
    }
}
