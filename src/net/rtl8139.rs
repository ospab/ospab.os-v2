/*
 * RTL8139 NIC driver for AETERNA
 * Minimal driver: PCI probe, init, TX (4 descriptors), RX (ring buffer 8K+16+1500)
 * Works with QEMU -device rtl8139
 */

use core::arch::asm;

// RTL8139 PCI IDs
const RTL8139_VENDOR: u16 = 0x10EC;
const RTL8139_DEVICE: u16 = 0x8139;

// RTL8139 register offsets
const REG_MAC0: u16       = 0x00;   // MAC address bytes 0-3
const REG_MAC4: u16       = 0x04;   // MAC address bytes 4-5
const REG_RBSTART: u16    = 0x30;   // RX buffer start address (physical)
const REG_CMD: u16        = 0x37;   // Command register
const REG_CAPR: u16       = 0x38;   // Current address of packet read
const REG_CBR: u16        = 0x3A;   // Current buffer address
const REG_IMR: u16        = 0x3C;   // Interrupt mask
const REG_ISR: u16        = 0x3E;   // Interrupt status
const REG_TCR: u16        = 0x40;   // Transmit configuration
const REG_RCR: u16        = 0x44;   // Receive configuration
const REG_CONFIG1: u16    = 0x52;   // Configuration register 1
const REG_TSAD0: u16      = 0x20;   // TX start address descriptor 0
const REG_TSD0: u16       = 0x10;   // TX status descriptor 0

// RX buffer: 8K + 16 bytes header + 1500 tail margin
const RX_BUF_SIZE: usize = 8192 + 16 + 1500;
// Packet buffer for returning data
const PACKET_BUF_SIZE: usize = 1600;

// Static buffers — MUST be DWORD-aligned for RTL8139 DMA.
// We use repr(C,align(4096)) wrappers to guarantee alignment.
#[repr(C, align(4096))]
struct AlignedRxBuf([u8; RX_BUF_SIZE]);
#[repr(C, align(32))]
struct AlignedTxBuf([u8; PACKET_BUF_SIZE]);
#[repr(C, align(32))]
struct AlignedPktBuf([u8; PACKET_BUF_SIZE]);

static mut RX_BUFFER: AlignedRxBuf = AlignedRxBuf([0; RX_BUF_SIZE]);
static mut TX_BUFFERS: [AlignedTxBuf; 4] = [
    AlignedTxBuf([0; PACKET_BUF_SIZE]),
    AlignedTxBuf([0; PACKET_BUF_SIZE]),
    AlignedTxBuf([0; PACKET_BUF_SIZE]),
    AlignedTxBuf([0; PACKET_BUF_SIZE]),
];
static mut PACKET_BUF: AlignedPktBuf = AlignedPktBuf([0; PACKET_BUF_SIZE]);

static mut IO_BASE: u16 = 0;
static mut MAC: [u8; 6] = [0; 6];
static mut TX_CUR: u8 = 0;
static mut RX_OFFSET: usize = 0;
static mut INITIALIZED: bool = false;

/// Read a PCI config u32
fn pci_read(bus: u8, dev: u8, func: u8, offset: u8) -> u32 {
    let addr: u32 = 0x80000000
        | ((bus as u32) << 16)
        | ((dev as u32) << 11)
        | ((func as u32) << 8)
        | ((offset as u32) & 0xFC);
    unsafe {
        asm!("out dx, eax", in("dx") 0x0CF8u16, in("eax") addr, options(nomem, nostack));
        let val: u32;
        asm!("in eax, dx", in("dx") 0x0CFCu16, out("eax") val, options(nomem, nostack));
        val
    }
}

/// Write PCI config u16
fn pci_write16(bus: u8, dev: u8, func: u8, offset: u8, val: u16) {
    let addr: u32 = 0x80000000
        | ((bus as u32) << 16)
        | ((dev as u32) << 11)
        | ((func as u32) << 8)
        | ((offset as u32) & 0xFC);
    unsafe {
        asm!("out dx, eax", in("dx") 0x0CF8u16, in("eax") addr, options(nomem, nostack));
        // Read-modify-write for 16-bit access
        let old: u32;
        asm!("in eax, dx", in("dx") 0x0CFCu16, out("eax") old, options(nomem, nostack));
        let shift = ((offset & 2) as u32) * 8;
        let mask = !(0xFFFF << shift);
        let new = (old & mask) | ((val as u32) << shift);
        asm!("out dx, eax", in("dx") 0x0CFCu16, in("eax") new, options(nomem, nostack));
    }
}

fn outb(port: u16, val: u8) {
    unsafe { asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack)); }
}
fn outw(port: u16, val: u16) {
    unsafe { asm!("out dx, ax", in("dx") port, in("ax") val, options(nomem, nostack)); }
}
fn outl(port: u16, val: u32) {
    unsafe { asm!("out dx, eax", in("dx") port, in("eax") val, options(nomem, nostack)); }
}
fn inb(port: u16) -> u8 {
    let val: u8;
    unsafe { asm!("in al, dx", in("dx") port, out("al") val, options(nomem, nostack)); }
    val
}
fn inw(port: u16) -> u16 {
    let val: u16;
    unsafe { asm!("in ax, dx", in("dx") port, out("ax") val, options(nomem, nostack)); }
    val
}
fn inl(port: u16) -> u32 {
    let val: u32;
    unsafe { asm!("in eax, dx", in("dx") port, out("eax") val, options(nomem, nostack)); }
    val
}

/// Convert a kernel static virtual address to physical.
/// Kernel is linked at KERNEL_VIRT=0xffffffff80200000, physical base KERNEL_PHYS=0x200000.
/// Kernel virtual offset = KERNEL_VIRT - KERNEL_PHYS = 0xffffffff80000000.
fn virt_to_phys(vaddr: usize) -> u32 {
    (vaddr as u64).wrapping_sub(0xffff_ffff_8000_0000_u64) as u32
}

/// Probe PCI bus for RTL8139 and initialize it
pub fn probe_and_init() -> bool {
    for bus in 0u8..8 {
        for dev in 0u8..32 {
            let id = pci_read(bus, dev, 0, 0);
            let vendor = (id & 0xFFFF) as u16;
            let device = ((id >> 16) & 0xFFFF) as u16;
            if vendor == RTL8139_VENDOR && device == RTL8139_DEVICE {
                return init_device(bus, dev, 0);
            }
        }
    }
    false
}

fn init_device(bus: u8, dev: u8, func: u8) -> bool {
    // Read BAR0 (I/O base)
    let bar0 = pci_read(bus, dev, func, 0x10);
    if bar0 & 1 == 0 {
        // Memory-mapped BAR, not I/O — we need I/O
        crate::arch::x86_64::serial::write_str("[RTL8139] BAR0 is MMIO, not supported\r\n");
        return false;
    }
    let io = (bar0 & 0xFFFC) as u16;

    // Enable bus mastering + I/O space in PCI command register
    let cmd = pci_read(bus, dev, func, 0x04) as u16;
    pci_write16(bus, dev, func, 0x04, cmd | 0x05); // IO Space + Bus Master

    // Read IRQ line
    let irq_line = (pci_read(bus, dev, func, 0x3C) & 0xFF) as u8;

    unsafe {
        IO_BASE = io;

        crate::arch::x86_64::serial::write_str("[RTL8139] IO base: 0x");
        serial_hex16(io);
        crate::arch::x86_64::serial::write_str(", IRQ: ");
        serial_dec(irq_line as u64);
        crate::arch::x86_64::serial::write_str("\r\n");

        // 1. Power on
        outb(io + REG_CONFIG1, 0x00);

        // 2. Software reset
        outb(io + REG_CMD, 0x10);
        let mut timeout = 100000u32;
        while inb(io + REG_CMD) & 0x10 != 0 && timeout > 0 {
            timeout -= 1;
        }
        if timeout == 0 {
            crate::arch::x86_64::serial::write_str("[RTL8139] Reset timeout\r\n");
            return false;
        }

        // 3. Read MAC address
        for i in 0..4u16 {
            MAC[i as usize] = inb(io + REG_MAC0 + i);
        }
        for i in 0..2u16 {
            MAC[(4 + i) as usize] = inb(io + REG_MAC4 + i);
        }

        // 4. Set RX buffer (physical address, must be <4GB for RTL8139)
        let rx_phys = virt_to_phys(RX_BUFFER.0.as_ptr() as usize);
        outl(io + REG_RBSTART, rx_phys);

        crate::arch::x86_64::serial::write_str("[RTL8139] RX buffer phys: 0x");
        serial_hex32(rx_phys);
        crate::arch::x86_64::serial::write_str("\r\n");

        // 5. Configure interrupts: ROK (bit 0) + TOK (bit 2) + RxOverflow (bit4) + RxFIFO (bit5) + LinkChg (bit5)
        outw(io + REG_IMR, 0x003F);

        // 6. RX config: accept all packets, proper DMA burst, 8K+16 buffer
        //    Bits 0-3: AAP|APM|AM|AB = accept all
        //    Bit 7: WRAP = 1 (ring buffer wraps)
        //    Bits 10:8: MXDMA = 111 (unlimited DMA burst)
        //    Bits 12:11: RBLEN = 00 (8K+16 buffer)
        //    Bits 15:13: RXFTH = 111 (no threshold, whole packet)
        outl(io + REG_RCR, 0x0000_E78F);

        // 7. TX config: IFG=96bit, MXDMA=2048 (bits 10:8 = 110)
        outl(io + REG_TCR, 0x0300_0600);

        // 8. Enable RX and TX
        outb(io + REG_CMD, 0x0C); // RE + TE

        // 9. Enable IRQ on the PIC
        if irq_line < 16 {
            crate::arch::x86_64::pic::enable_irq(irq_line);
        }

        RX_OFFSET = 0;
        TX_CUR = 0;
        INITIALIZED = true;

        // Verify register write-backs
        let rbstart_readback = inl(io + REG_RBSTART);
        let rcr_readback = inl(io + REG_RCR);
        let cmd_readback = inb(io + REG_CMD);
        let imr_readback = inw(io + REG_IMR);
        crate::arch::x86_64::serial::write_str("[RTL8139] Verify: RBSTART=0x");
        serial_hex32(rbstart_readback);
        crate::arch::x86_64::serial::write_str(" RCR=0x");
        serial_hex32(rcr_readback);
        crate::arch::x86_64::serial::write_str(" CMD=0x");
        serial_hex32(cmd_readback as u32);
        crate::arch::x86_64::serial::write_str(" IMR=0x");
        serial_hex32(imr_readback as u32);
        crate::arch::x86_64::serial::write_str("\r\n");
    }

    true
}

pub fn mac_address() -> [u8; 6] {
    unsafe { MAC }
}

pub fn is_initialized() -> bool {
    unsafe { INITIALIZED }
}

/// Send a packet (copies into TX descriptor buffer)
pub fn send_packet(data: &[u8], len: usize) {
    if !is_initialized() || len > 1500 || len < 14 { return; }

    unsafe {
        let desc = TX_CUR as usize;

        // Copy data to TX buffer
        TX_BUFFERS[desc].0[..len].copy_from_slice(&data[..len]);

        // Set TX start address (physical)
        let tx_phys = virt_to_phys(TX_BUFFERS[desc].0.as_ptr() as usize);
        outl(IO_BASE + REG_TSAD0 + (desc as u16) * 4, tx_phys);

        // Set TX status: length in bits 0-12, clear OWN bit (bit 13)
        outl(IO_BASE + REG_TSD0 + (desc as u16) * 4, len as u32 & 0x1FFF);

        // Wait for TX to complete (OWN bit set by hardware)
        let mut timeout = 200000u32;
        let mut final_status = inl(IO_BASE + REG_TSD0 + (desc as u16) * 4);
        loop {
            if final_status & 0x8000 != 0 { break; } // TOK
            if final_status & 0x4000_0000 != 0 { break; } // Abort
            timeout -= 1;
            if timeout == 0 { break; }
            final_status = inl(IO_BASE + REG_TSD0 + (desc as u16) * 4);
        }

        // Debug: log TX result
        crate::arch::x86_64::serial::write_str("[RTL8139] TX desc=");
        serial_dec(desc as u64);
        crate::arch::x86_64::serial::write_str(" len=");
        serial_dec(len as u64);
        crate::arch::x86_64::serial::write_str(" status=0x");
        serial_hex32(final_status);
        if final_status & 0x8000 != 0 {
            crate::arch::x86_64::serial::write_str(" TOK");
        } else if timeout == 0 {
            crate::arch::x86_64::serial::write_str(" TIMEOUT");
        }
        crate::arch::x86_64::serial::write_str("\r\n");

        TX_CUR = ((TX_CUR + 1) % 4) as u8;
    }
}

/// Receive a packet from the RX ring buffer. Returns (buffer_ref, length) or None.
pub fn receive_packet() -> Option<(&'static [u8], usize)> {
    unsafe {
        if !INITIALIZED { return None; }

        // Check if buffer is empty
        let cmd = inb(IO_BASE + REG_CMD);
        if cmd & 0x01 != 0 { // BUFE (buffer empty)
            return None;
        }

        // Read packet header at current offset
        let offset = RX_OFFSET;
        let header = u32::from_le_bytes([
            RX_BUFFER.0[offset % RX_BUF_SIZE],
            RX_BUFFER.0[(offset + 1) % RX_BUF_SIZE],
            RX_BUFFER.0[(offset + 2) % RX_BUF_SIZE],
            RX_BUFFER.0[(offset + 3) % RX_BUF_SIZE],
        ]);

        let status = (header & 0xFFFF) as u16;
        let pkt_len = ((header >> 16) & 0xFFFF) as usize;

        // Debug: log received packet
        crate::arch::x86_64::serial::write_str("[RTL8139] RX: status=0x");
        serial_hex16(status);
        crate::arch::x86_64::serial::write_str(" len=");
        serial_dec(pkt_len as u64);
        crate::arch::x86_64::serial::write_str(" offset=");
        serial_dec(offset as u64);
        crate::arch::x86_64::serial::write_str("\r\n");

        // Check ROK (bit 0 of status)
        if status & 0x01 == 0 {
            // Bad packet — skip and reset receiver
            crate::arch::x86_64::serial::write_str("[RTL8139] RX: bad status, resetting\r\n");
            outb(IO_BASE + REG_CMD, 0x04); // TE only
            core::arch::asm!("pause");
            outb(IO_BASE + REG_CMD, 0x0C); // TE + RE
            RX_OFFSET = 0;
            return None;
        }

        // pkt_len from RTL8139 includes the 4-byte ethernet CRC at the end
        // Actual usable Ethernet frame = pkt_len - 4
        let eth_len = if pkt_len >= 4 { pkt_len - 4 } else { pkt_len };

        if eth_len < 14 || pkt_len > 1518 {
            // Advance past this packet
            let advance = ((pkt_len + 4) + 3) & !3;
            RX_OFFSET = (offset + advance) % RX_BUF_SIZE;
            outw(IO_BASE + REG_CAPR, (RX_OFFSET as u16).wrapping_sub(16));
            return None;
        }

        // Copy Ethernet frame (skip 4-byte RTL header, strip 4-byte CRC)
        let data_start = (offset + 4) % RX_BUF_SIZE;
        let copy_len = eth_len.min(PACKET_BUF_SIZE);
        for i in 0..copy_len {
            PACKET_BUF.0[i] = RX_BUFFER.0[(data_start + i) % RX_BUF_SIZE];
        }

        // Advance offset: aligned(4-byte header + pkt_len)
        let advance = ((pkt_len + 4) + 3) & !3;
        RX_OFFSET = (offset + advance) % RX_BUF_SIZE;

        // Update CAPR: write (new_offset - 16) per RTL8139 spec to avoid overrun
        outw(IO_BASE + REG_CAPR, (RX_OFFSET as u16).wrapping_sub(16));

        Some((&PACKET_BUF.0[..copy_len], copy_len))
    }
}

/// Handle IRQ from RTL8139 - acknowledge interrupt
pub fn handle_irq() {
    unsafe {
        if !INITIALIZED { return; }
        let isr = inw(IO_BASE + REG_ISR);
        if isr != 0 {
            crate::arch::x86_64::serial::write_str("[RTL8139] IRQ: ISR=0x");
            serial_hex16(isr);
            crate::arch::x86_64::serial::write_str("\r\n");
        }
        // Acknowledge all bits
        outw(IO_BASE + REG_ISR, isr);
    }
}

/// Read NIC status registers for diagnostics (ISR, CMD, CBR, CAPR)
pub fn diag() -> (u16, u8, u16, u16) {
    unsafe {
        if !INITIALIZED { return (0, 0, 0, 0); }
        let isr  = inw(IO_BASE + REG_ISR);
        let cmd  = inb(IO_BASE + REG_CMD);
        let cbr  = inw(IO_BASE + REG_CBR);
        let capr = inw(IO_BASE + REG_CAPR);
        (isr, cmd, cbr, capr)
    }
}

fn serial_hex16(v: u16) {
    let hex = b"0123456789ABCDEF";
    crate::arch::x86_64::serial::write_byte(hex[((v >> 12) & 0xF) as usize]);
    crate::arch::x86_64::serial::write_byte(hex[((v >> 8) & 0xF) as usize]);
    crate::arch::x86_64::serial::write_byte(hex[((v >> 4) & 0xF) as usize]);
    crate::arch::x86_64::serial::write_byte(hex[(v & 0xF) as usize]);
}

fn serial_dec(mut val: u64) {
    if val == 0 { crate::arch::x86_64::serial::write_byte(b'0'); return; }
    let mut buf = [0u8; 20];
    let mut i = 0;
    while val > 0 { buf[i] = b'0' + (val % 10) as u8; val /= 10; i += 1; }
    for j in (0..i).rev() { crate::arch::x86_64::serial::write_byte(buf[j]); }
}

fn serial_hex32(v: u32) {
    let hex = b"0123456789ABCDEF";
    for i in (0..8).rev() {
        crate::arch::x86_64::serial::write_byte(hex[((v >> (i * 4)) & 0xF) as usize]);
    }
}
