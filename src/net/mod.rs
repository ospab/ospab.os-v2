/*
 * ospab.os Network Stack
 * Minimal IPv4 networking: RTL8139 → Ethernet → ARP → IPv4 → ICMP/UDP
 * Enough for ping and NTP time sync.
 */

pub mod rtl8139;
pub mod e1000;
pub mod rtl8169;
pub mod ethernet;
pub mod arp;
pub mod ipv4;
pub mod icmp;
pub mod udp;
pub mod sntp;

use core::sync::atomic::{AtomicBool, Ordering};

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

pub fn is_up() -> bool {
    NET_UP.load(Ordering::Relaxed)
}

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

    // Step 2: ARP resolve gateway
    crate::arch::x86_64::serial::write_str("[NET] Sending ARP for gateway...\r\n");
    arp::send_request(unsafe { GATEWAY_IP });

    // Wait briefly for ARP reply (poll-based, up to ~0.5 seconds)
    for _ in 0..50 {
        poll_rx();
        if unsafe { GATEWAY_MAC[0] != 0xFF || GATEWAY_MAC[1] != 0xFF } {
            break;
        }
        for _ in 0..100_000u32 {
            unsafe { core::arch::asm!("pause"); }
        }
    }

    let gw_resolved = unsafe { GATEWAY_MAC[0] != 0xFF || GATEWAY_MAC[1] != 0xFF };
    if gw_resolved {
        crate::arch::x86_64::serial::write_str("[NET] Gateway MAC resolved\r\n");
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
                ethernet::handle_frame(&buf[..len]);
            }
        }
        1 => {
            while let Some((buf, len)) = e1000::receive_packet() {
                if len < 14 { continue; }
                ethernet::handle_frame(&buf[..len]);
            }
        }
        _ => {
            while let Some((buf, len)) = rtl8169::receive_packet() {
                if len < 14 { continue; }
                ethernet::handle_frame(&buf[..len]);
            }
        }
    }
}

/// Send a raw Ethernet frame
pub fn send_raw(buf: &[u8], len: usize) {
    match unsafe { ACTIVE_NIC } {
        0 => rtl8139::send_packet(buf, len),
        1 => e1000::send_packet(buf, len),
        _ => rtl8169::send_packet(buf, len),
    }
}

/// Handle network IRQ — dispatch to the active NIC driver
pub fn handle_net_irq() {
    match unsafe { ACTIVE_NIC } {
        0 => rtl8139::handle_irq(),
        1 => e1000::handle_irq(),
        _ => rtl8169::handle_irq(),
    }
}

/// Diagnostic registers for the active NIC (ISR, CMD, CBR, CAPR)
pub fn diag() -> (u16, u8, u16, u16) {
    match unsafe { ACTIVE_NIC } {
        0 => rtl8139::diag(),
        _ => (0, 0, 0, 0),  // e1000/rtl8169 don't have these registers
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
