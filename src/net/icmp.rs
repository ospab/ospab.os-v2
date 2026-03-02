/*
 * ICMP — Internet Control Message Protocol
 * Handles Echo Reply (for receiving pong) and sends Echo Request (ping).
 */

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const ICMP_ECHO_REPLY: u8   = 0;
const ICMP_ECHO_REQUEST: u8 = 8;

// Ping state: shared between sender and receiver
static PING_WAITING: AtomicBool = AtomicBool::new(false);
static PING_RECEIVED: AtomicBool = AtomicBool::new(false);
static PING_SEQ: AtomicU64 = AtomicU64::new(0);
static PING_RTT_TICKS: AtomicU64 = AtomicU64::new(0);
static mut PING_SEND_TICK: u64 = 0;
#[allow(dead_code)]
static mut PING_TTL: u8 = 0;

/// Handle incoming ICMP packet
pub fn handle_icmp(data: &[u8], src_ip: [u8; 4]) {
    if data.len() < 8 { return; }

    let icmp_type = data[0];
    let _code = data[1];

    match icmp_type {
        ICMP_ECHO_REPLY => {
            crate::arch::x86_64::serial::write_str("[ICMP] Echo Reply received\r\n");
            if PING_WAITING.load(Ordering::Relaxed) {
                let seq = u16::from_be_bytes([data[6], data[7]]);
                let now = crate::arch::x86_64::idt::timer_ticks();
                let sent = unsafe { PING_SEND_TICK };
                let rtt = now.saturating_sub(sent);
                PING_RTT_TICKS.store(rtt, Ordering::Relaxed);
                PING_SEQ.store(seq as u64, Ordering::Relaxed);
                PING_RECEIVED.store(true, Ordering::Relaxed);
                PING_WAITING.store(false, Ordering::Relaxed);
            }
        }
        ICMP_ECHO_REQUEST => {
            // Reply to ping (we are being pinged)
            send_echo_reply(src_ip, data);
        }
        _ => {}
    }
}

/// Send ICMP echo request (ping)
pub fn send_ping(dst_ip: [u8; 4], seq: u16) {
    let mut pkt = [0u8; 64];

    // ICMP type: Echo Request
    pkt[0] = ICMP_ECHO_REQUEST;
    // Code: 0
    pkt[1] = 0;
    // Checksum (filled later)
    pkt[2] = 0;
    pkt[3] = 0;
    // Identifier
    pkt[4] = 0xAE;
    pkt[5] = 0x01;
    // Sequence number
    let sq = seq.to_be_bytes();
    pkt[6] = sq[0];
    pkt[7] = sq[1];

    // Payload: fill with pattern
    for i in 8..64 {
        pkt[i] = (i as u8) & 0xFF;
    }

    // Checksum
    let cksum = super::ipv4::checksum(&pkt[..64]);
    pkt[2] = (cksum >> 8) as u8;
    pkt[3] = (cksum & 0xFF) as u8;

    // Mark waiting
    PING_RECEIVED.store(false, Ordering::Relaxed);
    PING_WAITING.store(true, Ordering::Relaxed);
    unsafe { PING_SEND_TICK = crate::arch::x86_64::idt::timer_ticks(); }

    // Send via IPv4
    super::ipv4::send_ipv4(1, dst_ip, &pkt[..64]);
}

/// Non-blocking check for ping reply. Call from a loop together with keyboard polling.
/// Returns Some((seq, rtt_ms)) if a reply arrived, None if not yet.
pub fn poll_reply() -> Option<(u16, u64)> {
    super::poll_rx();
    if PING_RECEIVED.load(Ordering::Relaxed) {
        let seq = PING_SEQ.load(Ordering::Relaxed) as u16;
        let rtt_ticks = PING_RTT_TICKS.load(Ordering::Relaxed);
        // 18.2 ticks/sec → 1 tick ≈ 55 ms
        let rtt_ms = rtt_ticks * 55;
        return Some((seq, rtt_ms));
    }
    None
}

/// Cancel an in-progress wait (called on Ctrl+C)
pub fn cancel_wait() {
    PING_WAITING.store(false, Ordering::Relaxed);
    PING_RECEIVED.store(false, Ordering::Relaxed);
}

/// Wait for ping reply. Returns Some((seq, rtt_ms)) or None on timeout.
pub fn wait_reply(timeout_ticks: u64) -> Option<(u16, u64)> {
    let start = crate::arch::x86_64::idt::timer_ticks();
    loop {
        if let Some(r) = poll_reply() { return Some(r); }
        let now = crate::arch::x86_64::idt::timer_ticks();
        if now.saturating_sub(start) >= timeout_ticks {
            PING_WAITING.store(false, Ordering::Relaxed);
            return None;
        }
        unsafe { core::arch::asm!("hlt"); }
    }
}

/// Reply to an Echo Request
fn send_echo_reply(dst_ip: [u8; 4], request: &[u8]) {
    let len = request.len().min(1480);
    let mut pkt = [0u8; 1480];
    pkt[..len].copy_from_slice(&request[..len]);

    // Change type to Echo Reply
    pkt[0] = ICMP_ECHO_REPLY;
    // Recompute checksum
    pkt[2] = 0;
    pkt[3] = 0;
    let cksum = super::ipv4::checksum(&pkt[..len]);
    pkt[2] = (cksum >> 8) as u8;
    pkt[3] = (cksum & 0xFF) as u8;

    super::ipv4::send_ipv4(1, dst_ip, &pkt[..len]);
}
