/*
 * ospab.os Network Stack
 * Minimal IPv4 networking: RTL8139 → Ethernet → ARP → IPv4 → ICMP/UDP
 * Enough for ping and NTP time sync.
 */

pub mod rtl8139;
pub mod e1000;
pub mod rtl8169;
pub mod ethernet;
pub mod diag;
pub mod arp;
pub mod ipv4;
pub mod icmp;
pub mod udp;
pub mod sntp;
pub mod resolver;

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

// ─── Universal NIC trait (owned by this module) ───
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkState { Down, Up }

pub trait NetDriver {
    fn name(&self) -> &'static str;
    fn probe_and_init(&self) -> bool;
    fn mac_address(&self) -> [u8; 6];
    fn send_packet(&self, buf: &[u8], len: usize);
    fn receive_packet(&self) -> Option<(&'static [u8], usize)>;
    fn handle_irq(&self);
    fn link_state(&self) -> LinkState;
}

// ─── Network configuration (QEMU user-mode defaults) ───
pub static mut OUR_IP: [u8; 4]       = [10, 0, 2, 15];
pub static mut GATEWAY_IP: [u8; 4]   = [10, 0, 2, 2];
pub static mut SUBNET_MASK: [u8; 4]  = [255, 255, 255, 0];
pub static mut DNS_IP: [u8; 4]       = [10, 0, 2, 3];
pub static mut OUR_MAC: [u8; 6]      = [0; 6];
pub static mut GATEWAY_MAC: [u8; 6]  = [0xFF; 6]; // broadcast until ARP resolve

static NET_UP: AtomicBool = AtomicBool::new(false);
// 0=RTL8139  1=e1000  2=RTL8169
static mut ACTIVE_NIC: u8 = 0;
// Packet counters
static RX_PKTS: AtomicU64 = AtomicU64::new(0);
static TX_PKTS: AtomicU64 = AtomicU64::new(0);

pub fn is_up() -> bool { NET_UP.load(Ordering::Relaxed) }
pub fn rx_packets() -> u64 { RX_PKTS.load(Ordering::Relaxed) }
pub fn tx_packets() -> u64 { TX_PKTS.load(Ordering::Relaxed) }

/// Initialize the network stack. Call after PCI scan.
pub fn init() -> bool {
    crate::arch::x86_64::serial::write_str("[NET] Initializing network stack...\r\n");

    // Step 1: Find and init NIC — try RTL8139, e1000, RTL8169 in order
    enum NicKind { Rtl8139, E1000, Rtl8169 }
    let nic: NicKind;
    if rtl8139::probe_and_init() {
        crate::arch::x86_64::serial::write_str("[NET] RTL8139 found\r\n");
        nic = NicKind::Rtl8139;
    } else if e1000::probe_and_init() {
        crate::arch::x86_64::serial::write_str("[NET] Intel e1000 found\r\n");
        nic = NicKind::E1000;
    } else if rtl8169::probe_and_init() {
        crate::arch::x86_64::serial::write_str("[NET] RTL8169/8111 found\r\n");
        nic = NicKind::Rtl8169;
    } else {
        crate::arch::x86_64::serial::write_str("[NET] No supported NIC found\r\n");
        return false;
    }

    // Store which NIC is active so poll_rx / send_raw know which to call
    unsafe { ACTIVE_NIC = nic as u8; }

    // Copy MAC from whichever NIC was found
    let mac = match unsafe { ACTIVE_NIC } {
        0 => rtl8139::mac_address(),
        1 => e1000::mac_address(),
        _ => rtl8169::mac_address(),
    };
    unsafe {
        OUR_MAC = mac;
    }

    crate::arch::x86_64::serial::write_str("[NET] NIC initialized, MAC: ");
    for i in 0..6 {
        serial_hex_byte(mac[i]);
        if i < 5 { crate::arch::x86_64::serial::write_byte(b':'); }
    }
    crate::arch::x86_64::serial::write_str("\r\n");

    // Step 2: ARP resolve gateway — send 3 requests over 2 seconds
    crate::arch::x86_64::serial::write_str("[NET] Sending ARP for gateway...\r\n");
    arp::send_request(unsafe { GATEWAY_IP });

    // Poll for ARP reply using PIT ticks (200 ticks = 2s at 100Hz)
    let start = crate::arch::x86_64::idt::timer_ticks();
    let mut arp_sends = 1u32;
    loop {
        poll_rx();
        if unsafe { GATEWAY_MAC[0] != 0xFF || GATEWAY_MAC[1] != 0xFF } {
            break;
        }
        let elapsed = crate::arch::x86_64::idt::timer_ticks().wrapping_sub(start);
        if elapsed >= 200 { break; } // 2 second timeout
        // Re-send ARP every 0.5s (50 ticks)
        if elapsed >= arp_sends as u64 * 50 {
            arp::send_request(unsafe { GATEWAY_IP });
            arp_sends += 1;
        }
        unsafe { core::arch::asm!("hlt"); } // sleep until next IRQ (timer or NIC)
    }

    let gw_resolved = unsafe { GATEWAY_MAC[0] != 0xFF || GATEWAY_MAC[1] != 0xFF };
    if gw_resolved {
        crate::arch::x86_64::serial::write_str("[NET] Gateway MAC resolved\r\n");
        // Store resolved gateway in ARP cache
        let gw_ip  = unsafe { GATEWAY_IP };
        let gw_mac = unsafe { GATEWAY_MAC };
        arp::cache_update(gw_ip, gw_mac);
    } else {
        // QEMU SLIRP doesn't answer ARP normally — set gateway to broadcast
        // so packets still reach the virtual gateway
        unsafe { GATEWAY_MAC = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]; }
        crate::arch::x86_64::serial::write_str("[NET] Gateway ARP timeout (using broadcast)\r\n");
    }

    NET_UP.store(true, Ordering::Relaxed);
    crate::arch::x86_64::serial::write_str("[NET] Network stack ready\r\n");
    true
}

/// Poll for received packets — dispatches to the active NIC
pub fn poll_rx() {
    match unsafe { ACTIVE_NIC } {
        0 => {
            while let Some((buf, len)) = rtl8139::receive_packet() {
                if len < 14 { continue; }
                RX_PKTS.fetch_add(1, Ordering::Relaxed);
                ethernet::handle_frame(&buf[..len]);
            }
        }
        1 => {
            while let Some((buf, len)) = e1000::receive_packet() {
                if len < 14 { continue; }
                RX_PKTS.fetch_add(1, Ordering::Relaxed);
                ethernet::handle_frame(&buf[..len]);
            }
        }
        _ => {
            while let Some((buf, len)) = rtl8169::receive_packet() {
                if len < 14 { continue; }
                RX_PKTS.fetch_add(1, Ordering::Relaxed);
                ethernet::handle_frame(&buf[..len]);
            }
        }
    }
}

/// Send a raw Ethernet frame
pub fn send_raw(buf: &[u8], len: usize) {
    TX_PKTS.fetch_add(1, Ordering::Relaxed);
    match unsafe { ACTIVE_NIC } {
        0 => rtl8139::send_packet(buf, len),
        1 => e1000::send_packet(buf, len),
        _ => rtl8169::send_packet(buf, len),
    }
}

/// Handle network IRQ — called from IDT IRQ 9/10/11 dispatcher (inside ISR).
///
/// Interrupt-driven path:
///   1. Acknowledge the NIC's interrupt register (clears IRQ line).
///   2. **Immediately drain all available receive descriptors** — every packet
///      that arrived since the last drain is handled right now, inside the ISR,
///      without waiting for the next 10 ms PIT tick.
///   3. The NIC's hardware interrupt line is de-asserted after step 1, so no
///      spurious re-delivery occurs.
///
/// This is Task 2 of the latency-elimination plan: ping RTT is now bounded by
/// NIC-to-ISR delivery time (typically <200 µs on VMware/QEMU), not by the
/// 10 ms PIT period.
pub fn handle_net_irq() {
    // Step 1: ack NIC interrupt register
    match unsafe { ACTIVE_NIC } {
        0 => rtl8139::handle_irq(),
        1 => e1000::handle_irq(),
        _ => rtl8169::handle_irq(),
    }
    // Step 2: drain all pending RX frames immediately (interrupt-driven poll)
    poll_rx();
}

/// Diagnostic registers for the active NIC
/// For e1000: (STATUS, ICR, RDH, RDT)
/// For RTL8139: (ISR as u32, CMD as u32, CBR as u32, CAPR as u32)
pub fn diag() -> (u32, u32, u32, u32) {
    match unsafe { ACTIVE_NIC } {
        0 => {
            let (isr, cmd, cbr, capr) = rtl8139::diag();
            (isr as u32, cmd as u32, cbr as u32, capr as u32)
        }
        1 => e1000::diag_regs(),
        _ => (0, 0, 0, 0),
    }
}

/// Report which NIC is active (for terminal ifconfig)
pub fn nic_name() -> &'static str {
    match unsafe { ACTIVE_NIC } {
        0 => "RTL8139",
        1 => "Intel e1000",
        _ => "RTL8169/8111",
    }
}

/// Format IP for display: writes "x.x.x.x" into buf, returns len
pub fn format_ip(ip: [u8; 4], buf: &mut [u8; 16]) -> usize {
    let mut pos = 0;
    for i in 0..4 {
        let mut n = ip[i];
        if n >= 100 {
            buf[pos] = b'0' + n / 100;
            pos += 1;
            n %= 100;
            buf[pos] = b'0' + n / 10;
            pos += 1;
            n %= 10;
        } else if n >= 10 {
            buf[pos] = b'0' + n / 10;
            pos += 1;
            n %= 10;
        }
        buf[pos] = b'0' + n;
        pos += 1;
        if i < 3 {
            buf[pos] = b'.';
            pos += 1;
        }
    }
    pos
}

pub fn format_mac(mac: [u8; 6], buf: &mut [u8; 18]) -> usize {
    let hex = b"0123456789abcdef";
    let mut pos = 0;
    for i in 0..6 {
        buf[pos] = hex[(mac[i] >> 4) as usize];
        pos += 1;
        buf[pos] = hex[(mac[i] & 0xF) as usize];
        pos += 1;
        if i < 5 {
            buf[pos] = b':';
            pos += 1;
        }
    }
    pos
}

fn serial_hex_byte(v: u8) {
    let hex = b"0123456789abcdef";
    crate::arch::x86_64::serial::write_byte(hex[(v >> 4) as usize]);
    crate::arch::x86_64::serial::write_byte(hex[(v & 0xF) as usize]);
}
