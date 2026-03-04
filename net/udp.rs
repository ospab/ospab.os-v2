/*
 * UDP — User Datagram Protocol
 * Minimal implementation for SNTP client.
 */

use core::sync::atomic::{AtomicBool, Ordering};

// UDP receive buffer for the SNTP response
static mut UDP_RX_BUF: [u8; 512] = [0; 512];
static mut UDP_RX_LEN: usize = 0;
static mut UDP_RX_PORT: u16 = 0;
static UDP_RX_READY: AtomicBool = AtomicBool::new(false);

/// Handle incoming UDP packet (IPv4 payload)
pub fn handle_udp(data: &[u8], _src_ip: [u8; 4]) {
    if data.len() < 8 { return; }

    let _src_port = u16::from_be_bytes([data[0], data[1]]);
    let dst_port  = u16::from_be_bytes([data[2], data[3]]);
    let length    = u16::from_be_bytes([data[4], data[5]]) as usize;

    if length < 8 || data.len() < length { return; }

    let payload = &data[8..length];

    // Store in RX buffer if we're listening on that port
    unsafe {
        if dst_port == UDP_RX_PORT || UDP_RX_PORT == 0 {
            let copy_len = payload.len().min(512);
            UDP_RX_BUF[..copy_len].copy_from_slice(&payload[..copy_len]);
            UDP_RX_LEN = copy_len;
            UDP_RX_PORT = dst_port;
            UDP_RX_READY.store(true, Ordering::Relaxed);
        }
    }
}

/// Send a UDP packet
pub fn send_udp(dst_ip: [u8; 4], src_port: u16, dst_port: u16, payload: &[u8]) {
    let udp_len = 8 + payload.len();
    if udp_len > 1460 { return; }

    let mut pkt = [0u8; 1468];

    // Source port
    let sp = src_port.to_be_bytes();
    pkt[0] = sp[0];
    pkt[1] = sp[1];

    // Destination port
    let dp = dst_port.to_be_bytes();
    pkt[2] = dp[0];
    pkt[3] = dp[1];

    // Length
    let len = (udp_len as u16).to_be_bytes();
    pkt[4] = len[0];
    pkt[5] = len[1];

    // Checksum (0 = disabled for UDP)
    pkt[6] = 0;
    pkt[7] = 0;

    // Payload
    pkt[8..8 + payload.len()].copy_from_slice(payload);

    // Listen for reply on src_port
    unsafe { UDP_RX_PORT = src_port; }
    UDP_RX_READY.store(false, Ordering::Relaxed);

    super::ipv4::send_ipv4(17, dst_ip, &pkt[..udp_len]);
}

/// Wait for a UDP packet on the listening port. Returns payload or None.
pub fn wait_rx(timeout_ticks: u64) -> Option<&'static [u8]> {
    let start = crate::arch::x86_64::idt::timer_ticks();
    loop {
        super::poll_rx();

        if UDP_RX_READY.load(Ordering::Relaxed) {
            UDP_RX_READY.store(false, Ordering::Relaxed);
            let len = unsafe { UDP_RX_LEN };
            return Some(unsafe { &UDP_RX_BUF[..len] });
        }

        let now = crate::arch::x86_64::idt::timer_ticks();
        if now.saturating_sub(start) >= timeout_ticks {
            return None;
        }

        unsafe { core::arch::asm!("hlt"); }
    }
}
