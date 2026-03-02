/*
 * Realtek RTL8169/8111/8168 Gigabit Ethernet driver for AETERNA
 *
 * Supports:
 *   10EC:8169 — RTL8169 (rare, usually in desktop)
 *   10EC:8168 — RTL8111/8168 (extremely common on laptops & desktops)
 *   10EC:8162 — RTL8162
 *   10EC:8167 — RTL8110SC/8169SC
 *   10EC:8136 — RTL8101E / RTL8102E (100Mbps, some laptops)
 *   10EC:8161 — RTL8111E
 *
 * Descriptor-based DMA: 4 TX + 4 RX descriptors, 2K buffers each.
 * Uses I/O port (BAR0) for simplicity and broad compatibility.
 */
#![allow(dead_code)]

// ─── PCI IDs ───────────────────────────────────────────────────────────────
const RTL_IDS: &[(u16, u16)] = &[
    (0x10EC, 0x8169), // RTL8169 GbE
    (0x10EC, 0x8168), // RTL8111/8168 GbE — most common on laptops
    (0x10EC, 0x8162), // RTL8162
    (0x10EC, 0x8167), // RTL8110SC
    (0x10EC, 0x8136), // RTL8101E/8102E Fast Ethernet
    (0x10EC, 0x8161), // RTL8111E
    (0x10EC, 0x8125), // RTL8125 2.5GbE
];

// ─── Register offsets ─────────────────────────────────────────────────────
const REG_IDR0:        u16 = 0x00; // MAC bytes 0-3
const REG_IDR4:        u16 = 0x04; // MAC bytes 4-5
const REG_MAR0:        u16 = 0x08; // Multicast filter
const REG_TX_DESC_LO:  u16 = 0x20; // TX descriptor ring address low
const REG_TX_DESC_HI:  u16 = 0x24; // TX descriptor ring address high
const REG_CMD:         u16 = 0x37; // Command register
const REG_TPPOLL:      u16 = 0x38; // TX polling (write 0x40 to trigger)
const REG_IMR:         u16 = 0x3C; // Interrupt mask
const REG_ISR:         u16 = 0x3E; // Interrupt status
const REG_TX_CONFIG:   u16 = 0x40; // TX config
const REG_RX_CONFIG:   u16 = 0x44; // RX config
const REG_9346CR:      u16 = 0x50; // 9346/EEPROM command register
const REG_CONFIG1:     u16 = 0x52;
const REG_PHYAR:       u16 = 0x60; // PHY access
const REG_RX_DESC_LO:  u16 = 0xE4; // RX descriptor ring address low
const REG_RX_DESC_HI:  u16 = 0xE8; // RX descriptor ring address high
const REG_TX_THRESH:   u16 = 0xEC; // TX threshold

// CMD register bits
const CMD_TX_ENABLE:   u8 = 1 << 2;
const CMD_RX_ENABLE:   u8 = 1 << 3;
const CMD_RESET:       u8 = 1 << 4;

// 9346CR bits
const CR9346_UNLOCK:   u8 = 0xC0; // Enable config register write
const CR9346_LOCK:     u8 = 0x00; // Lock config registers

// ISR/IMR bits
const INT_ROK:  u16 = 1 << 0;  // Receive OK
const INT_TOK:  u16 = 1 << 2;  // Transmit OK
const INT_RER:  u16 = 1 << 1;  // Receive Error
const INT_TER:  u16 = 1 << 3;  // Transmit Error

// TX polling
const TPPOLL_NPQ: u8 = 0x40; // Normal Priority Queue poll

// RX config: accept broadcast, multicast, unicast + all physical
const RX_CONFIG_DEFAULT: u32 = 0x0000E70F;
// TX config: DMA burst 1024, IFG = normal
const TX_CONFIG_DEFAULT: u32 = 0x03000700;

// ─── Descriptor format ────────────────────────────────────────────────────
// RTL8169 uses 16-byte descriptors:
//   Bits: [flags:16 | frame_length:15 | OWN:1] [VLAN:32] [addr_lo:32] [addr_hi:32]
const DESC_OWN:   u32 = 1 << 31; // Set = owned by NIC
const DESC_EOR:   u32 = 1 << 30; // End of ring
const DESC_FS:    u32 = 1 << 29; // First segment
const DESC_LS:    u32 = 1 << 28; // Last segment
const DESC_LEN_MASK: u32 = 0x3FFF;

#[repr(C, packed)]
#[derive(Copy, Clone)]
struct Desc {
    flags:   u32, // OWN | EOR | FS | LS | length
    vlan:    u32, // VLAN tag (0 for untagged)
    addr_lo: u32,
    addr_hi: u32,
}

// ─── Ring sizes ────────────────────────────────────────────────────────────
const TX_RING: usize = 4;
const RX_RING: usize = 4;
const BUF_SIZE: usize = 2048;

// ─── Static state ──────────────────────────────────────────────────────────
static mut IO_BASE:    u16 = 0;
static mut MAC:        [u8; 6] = [0; 6];
static mut INITIALIZED: bool = false;

#[repr(C, align(256))]
struct DescRing<const N: usize>([Desc; N]);

static mut TX_DESCS: DescRing<TX_RING> = DescRing([Desc {
    flags: 0, vlan: 0, addr_lo: 0, addr_hi: 0
}; TX_RING]);
static mut RX_DESCS: DescRing<RX_RING> = DescRing([Desc {
    flags: 0, vlan: 0, addr_lo: 0, addr_hi: 0
}; RX_RING]);

static mut TX_BUFS: [[u8; BUF_SIZE]; TX_RING] = [[0u8; BUF_SIZE]; TX_RING];
static mut RX_BUFS: [[u8; BUF_SIZE]; RX_RING] = [[0u8; BUF_SIZE]; RX_RING];
static mut PACKET_OUT: [u8; BUF_SIZE] = [0u8; BUF_SIZE];

static mut TX_CUR: usize = 0;
static mut RX_CUR: usize = 0;

// ─── I/O helpers ──────────────────────────────────────────────────────────
fn inb(port: u16) -> u8 {
    let v: u8;
    unsafe { core::arch::asm!("in al, dx", in("dx") port, out("al") v, options(nomem, nostack)); }
    v
}
fn inw(port: u16) -> u16 {
    let v: u16;
    unsafe { core::arch::asm!("in ax, dx", in("dx") port, out("ax") v, options(nomem, nostack)); }
    v
}
fn inl(port: u16) -> u32 {
    let v: u32;
    unsafe { core::arch::asm!("in eax, dx", in("dx") port, out("eax") v, options(nomem, nostack)); }
    v
}
fn outb(port: u16, v: u8) {
    unsafe { core::arch::asm!("out dx, al", in("dx") port, in("al") v, options(nomem, nostack)); }
}
fn outw(port: u16, v: u16) {
    unsafe { core::arch::asm!("out dx, ax", in("dx") port, in("ax") v, options(nomem, nostack)); }
}
fn outl(port: u16, v: u32) {
    unsafe { core::arch::asm!("out dx, eax", in("dx") port, in("eax") v, options(nomem, nostack)); }
}

fn delay(n: u32) {
    for _ in 0..n {
        unsafe { core::arch::asm!("pause"); }
    }
}

fn virt_to_phys(vaddr: usize) -> u64 {
    // Kernel virtual offset = KERNEL_VIRT - KERNEL_PHYS = 0xffffffff80000000
    (vaddr as u64).wrapping_sub(0xffff_ffff_8000_0000_u64)
}

// ─── PCI helpers ──────────────────────────────────────────────────────────
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
fn pci_write16(bus: u8, dev: u8, func: u8, off: u8, val: u16) {
    let addr: u32 = 0x80000000 | ((bus as u32) << 16) | ((dev as u32) << 11)
        | ((func as u32) << 8) | ((off as u32) & 0xFC);
    unsafe {
        let old: u32;
        core::arch::asm!("out dx, eax", in("dx") 0x0CF8u16, in("eax") addr, options(nomem, nostack));
        core::arch::asm!("in eax, dx",  in("dx") 0x0CFCu16, out("eax") old, options(nomem, nostack));
        let shift = ((off & 2) as u32) * 8;
        let new = (old & !(0xFFFFu32 << shift)) | ((val as u32) << shift);
        core::arch::asm!("out dx, eax", in("dx") 0x0CF8u16, in("eax") addr, options(nomem, nostack));
        core::arch::asm!("out dx, eax", in("dx") 0x0CFCu16, in("eax") new, options(nomem, nostack));
    }
}

// ─── Probe and init ───────────────────────────────────────────────────────
pub fn probe_and_init() -> bool {
    for bus in 0u8..16 {
        for dev in 0u8..32 {
            let id = pci_read(bus, dev, 0, 0);
            let vendor = (id & 0xFFFF) as u16;
            let device = ((id >> 16) & 0xFFFF) as u16;
            if RTL_IDS.iter().any(|&(v, d)| v == vendor && d == device) {
                return init_device(bus, dev, 0);
            }
        }
    }
    false
}

fn init_device(bus: u8, dev: u8, func: u8) -> bool {
    // Read BAR0 (I/O port for RTL8169)
    let bar0 = pci_read(bus, dev, func, 0x10);
    if bar0 & 1 == 0 {
        // MMIO BAR — not I/O, skip for now
        crate::arch::x86_64::serial::write_str("[RTL8169] BAR0 not I/O, trying MMIO workaround\r\n");
        // For RTL8168 on modern systems, BAR2 might be I/O
        let bar2 = pci_read(bus, dev, func, 0x18);
        if bar2 & 1 == 0 { return false; }
        unsafe { IO_BASE = (bar2 & 0xFFFC) as u16; }
    } else {
        unsafe { IO_BASE = (bar0 & 0xFFFC) as u16; }
    }

    // Enable Bus Master + I/O in PCI command
    let cmd = (pci_read(bus, dev, func, 0x04) & 0xFFFF) as u16;
    pci_write16(bus, dev, func, 0x04, cmd | 0x05);

    let irq = (pci_read(bus, dev, func, 0x3C) & 0xFF) as u8;
    let io = unsafe { IO_BASE };

    crate::arch::x86_64::serial::write_str("[RTL8169] IO=0x");
    serial_hex16(io);
    crate::arch::x86_64::serial::write_str(", IRQ=");
    serial_dec(irq as u64);
    crate::arch::x86_64::serial::write_str("\r\n");

    // Unlock config registers
    outb(io + REG_9346CR, CR9346_UNLOCK);
    delay(1000);

    // Software reset
    outb(io + REG_CMD, CMD_RESET);
    let mut timeout = 100000u32;
    while inb(io + REG_CMD) & CMD_RESET != 0 && timeout > 0 {
        timeout -= 1;
        delay(1);
    }
    if timeout == 0 {
        crate::arch::x86_64::serial::write_str("[RTL8169] Reset timeout\r\n");
        return false;
    }

    // Re-unlock after reset
    outb(io + REG_9346CR, CR9346_UNLOCK);

    // Disable all interrupts temporarily
    outw(io + REG_IMR, 0x0000);
    outw(io + REG_ISR, 0xFFFF); // Clear all pending

    // Read MAC
    let mac = [
        inb(io + REG_IDR0 + 0),
        inb(io + REG_IDR0 + 1),
        inb(io + REG_IDR0 + 2),
        inb(io + REG_IDR0 + 3),
        inb(io + REG_IDR4 + 0),
        inb(io + REG_IDR4 + 1),
    ];
    unsafe { MAC = mac; }

    crate::arch::x86_64::serial::write_str("[RTL8169] MAC: ");
    for i in 0..6 {
        serial_hex_byte(mac[i]);
        if i < 5 { crate::arch::x86_64::serial::write_byte(b':'); }
    }
    crate::arch::x86_64::serial::write_str("\r\n");

    // Set multicast mask to accept all
    outl(io + REG_MAR0,    0xFFFFFFFF);
    outl(io + REG_MAR0 + 4, 0xFFFFFFFF);

    // --- Initialize TX descriptors ---
    unsafe {
        for i in 0..TX_RING {
            let is_last = i == TX_RING - 1;
            let phys = virt_to_phys(TX_BUFS[i].as_ptr() as usize);
            TX_DESCS.0[i].flags   = if is_last { DESC_EOR } else { 0 };
            TX_DESCS.0[i].vlan    = 0;
            TX_DESCS.0[i].addr_lo = phys as u32;
            TX_DESCS.0[i].addr_hi = (phys >> 32) as u32;
        }
        let tx_phys = virt_to_phys(TX_DESCS.0.as_ptr() as usize);
        outl(io + REG_TX_DESC_LO, tx_phys as u32);
        outl(io + REG_TX_DESC_HI, (tx_phys >> 32) as u32);
        TX_CUR = 0;
    }

    // --- Initialize RX descriptors ---
    unsafe {
        for i in 0..RX_RING {
            let is_last = i == RX_RING - 1;
            let phys = virt_to_phys(RX_BUFS[i].as_ptr() as usize);
            // OWN=1 so NIC can fill them; EOR on last; length = BUF_SIZE
            let eor = if is_last { DESC_EOR } else { 0 };
            RX_DESCS.0[i].flags   = DESC_OWN | eor | (BUF_SIZE as u32 & DESC_LEN_MASK);
            RX_DESCS.0[i].vlan    = 0;
            RX_DESCS.0[i].addr_lo = phys as u32;
            RX_DESCS.0[i].addr_hi = (phys >> 32) as u32;
        }
        let rx_phys = virt_to_phys(RX_DESCS.0.as_ptr() as usize);
        outl(io + REG_RX_DESC_LO, rx_phys as u32);
        outl(io + REG_RX_DESC_HI, (rx_phys >> 32) as u32);
        RX_CUR = 0;
    }

    // TX config: normal IFG, DMA burst 1K
    outl(io + REG_TX_CONFIG, TX_CONFIG_DEFAULT);
    // RX config: accept unicast/multicast/broadcast/promiscuous
    outl(io + REG_RX_CONFIG, RX_CONFIG_DEFAULT);
    // TX threshold
    outb(io + REG_TX_THRESH, 0x3B);

    // Enable TX + RX
    outb(io + REG_CMD, CMD_TX_ENABLE | CMD_RX_ENABLE);

    // Lock config registers
    outb(io + REG_9346CR, CR9346_LOCK);

    // Enable TX/RX OK + error interrupts
    outw(io + REG_IMR, INT_ROK | INT_TOK | INT_RER | INT_TER);

    // Enable IRQ in PIC
    if irq < 16 {
        crate::arch::x86_64::pic::enable_irq(irq);
    }

    unsafe { INITIALIZED = true; }
    crate::arch::x86_64::serial::write_str("[RTL8169] Initialized OK\r\n");
    true
}

// ─── Public API ───────────────────────────────────────────────────────────

pub fn mac_address() -> [u8; 6] { unsafe { MAC } }
pub fn is_initialized() -> bool { unsafe { INITIALIZED } }

/// Send a packet
pub fn send_packet(data: &[u8], len: usize) {
    if !is_initialized() || len == 0 || len > 1514 { return; }

    unsafe {
        let io = IO_BASE;
        let idx = TX_CUR;
        let d = &mut TX_DESCS.0[idx];

        // Wait until NIC releases this descriptor
        let mut timeout = 500000u32;
        while d.flags & DESC_OWN != 0 && timeout > 0 {
            timeout -= 1;
            core::arch::asm!("pause");
        }
        if timeout == 0 { return; }

        // Copy data
        TX_BUFS[idx][..len].copy_from_slice(&data[..len]);

        let is_last = idx == TX_RING - 1;
        let eor = if is_last { DESC_EOR } else { 0 };
        let phys = virt_to_phys(TX_BUFS[idx].as_ptr() as usize);
        d.addr_lo = phys as u32;
        d.addr_hi = (phys >> 32) as u32;
        d.vlan    = 0;

        // Give to NIC: OWN + FS + LS + EOR (if last) + length
        let flags = DESC_OWN | DESC_FS | DESC_LS | eor | (len as u32 & DESC_LEN_MASK);
        // Write flags last (after addr/len are set)
        core::ptr::write_volatile(core::ptr::addr_of_mut!(d.flags), flags);

        // Trigger TX
        outb(io + REG_TPPOLL, TPPOLL_NPQ);

        TX_CUR = (TX_CUR + 1) % TX_RING;
    }
}

/// Receive a packet (poll, non-blocking)
pub fn receive_packet() -> Option<(&'static [u8], usize)> {
    if !is_initialized() { return None; }

    unsafe {
        let idx = RX_CUR;
        let d = &mut RX_DESCS.0[idx];

        // If OWN bit set, NIC still owns this descriptor
        if d.flags & DESC_OWN != 0 { return None; }

        let len = (d.flags & DESC_LEN_MASK) as usize;
        if len < 14 || len > BUF_SIZE {
            // Bad — give back to NIC
            let is_last = idx == RX_RING - 1;
            let eor = if is_last { DESC_EOR } else { 0 };
            let phys = virt_to_phys(RX_BUFS[idx].as_ptr() as usize);
            d.addr_lo = phys as u32;
            d.addr_hi = (phys >> 32) as u32;
            d.vlan    = 0;
            core::ptr::write_volatile(core::ptr::addr_of_mut!(d.flags),
                DESC_OWN | eor | (BUF_SIZE as u32 & DESC_LEN_MASK));
            RX_CUR = (RX_CUR + 1) % RX_RING;
            return None;
        }

        // Copy data
        let copy_len = len.min(BUF_SIZE);
        PACKET_OUT[..copy_len].copy_from_slice(&RX_BUFS[idx][..copy_len]);

        // Give descriptor back to NIC
        let is_last = idx == RX_RING - 1;
        let eor = if is_last { DESC_EOR } else { 0 };
        let phys = virt_to_phys(RX_BUFS[idx].as_ptr() as usize);
        d.addr_lo = phys as u32;
        d.addr_hi = (phys >> 32) as u32;
        d.vlan    = 0;
        core::ptr::write_volatile(core::ptr::addr_of_mut!(d.flags),
            DESC_OWN | eor | (BUF_SIZE as u32 & DESC_LEN_MASK));

        RX_CUR = (RX_CUR + 1) % RX_RING;
        Some((&PACKET_OUT[..copy_len], copy_len))
    }
}

/// IRQ handler — acknowledge interrupt
pub fn handle_irq() {
    if is_initialized() {
        let io = unsafe { IO_BASE };
        let isr = inw(io + REG_ISR);
        outw(io + REG_ISR, isr); // clear all
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
fn serial_dec(mut v: u64) {
    if v == 0 { crate::arch::x86_64::serial::write_byte(b'0'); return; }
    let mut buf = [0u8; 20]; let mut i = 0;
    while v > 0 { buf[i] = b'0' + (v % 10) as u8; v /= 10; i += 1; }
    for j in (0..i).rev() { crate::arch::x86_64::serial::write_byte(buf[j]); }
}
