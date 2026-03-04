/*
 * netstat — network interface and ARP cache status for AETERNA
 *
 * Standalone logical unit.  No scheduler dependencies.
 * Usage: netstat
 */

extern crate alloc;
use alloc::format;

use crate::arch::x86_64::framebuffer;

const FG: u32     = 0x00FFFFFF;
const FG_OK: u32  = 0x0000FF00;
const FG_ERR: u32 = 0x00FF4444;
const FG_DIM: u32 = 0x00AAAAAA;
const FG_HL: u32  = 0x00FFCC00;
const BG: u32     = 0x00000000;

fn puts(s: &str) { framebuffer::draw_string(s, FG, BG); }
fn ok(s: &str)   { framebuffer::draw_string(s, FG_OK, BG); }
fn err(s: &str)  { framebuffer::draw_string(s, FG_ERR, BG); }
fn dim(s: &str)  { framebuffer::draw_string(s, FG_DIM, BG); }
fn hl(s: &str)   { framebuffer::draw_string(s, FG_HL, BG); }

fn draw_mac(mac: &[u8; 6], color: u32) {
    let hex = b"0123456789abcdef";
    for i in 0..6 {
        framebuffer::draw_char(hex[(mac[i] >> 4) as usize] as char, color, BG);
        framebuffer::draw_char(hex[(mac[i] & 0xF) as usize] as char, color, BG);
        if i < 5 { puts(":"); }
    }
}

/// Entry point: `netstat`
pub fn run(_args: &str) {
    use crate::net;

    if !net::is_up() {
        err("netstat: no NIC up\n");
        return;
    }

    let mac = unsafe { net::OUR_MAC };
    let ip  = unsafe { net::OUR_IP };
    let rx  = net::rx_packets();
    let tx  = net::tx_packets();
    let nic = net::nic_name();

    // ── Interface table ────────────────────────────────────────────────
    hl("  Interface    MAC                  IP              Status    RX        TX\n");
    dim("  -------------------------------------------------------------------------\n");

    puts("  ");
    puts(nic);
    for _ in 0..(13usize.saturating_sub(nic.len())) { puts(" "); }

    draw_mac(&mac, FG);
    puts("  ");

    let ip_s = format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);
    puts(&ip_s);
    for _ in 0..(16usize.saturating_sub(ip_s.len())) { puts(" "); }

    ok("Up");
    puts("      ");
    let rx_s = format!("{}", rx);
    puts(&rx_s);
    for _ in 0..(10usize.saturating_sub(rx_s.len())) { puts(" "); }
    puts(&format!("{}", tx));
    puts("\n\n");

    // ── ARP cache ─────────────────────────────────────────────────────
    hl("  ARP Cache:\n");
    dim("  IP              MAC\n");
    dim("  ---------------------------------\n");

    let mut cache = [([0u8; 4], [0u8; 6]); 16];
    let n = net::arp::cache_entries(&mut cache);
    if n == 0 {
        let gw = unsafe { net::GATEWAY_IP };
        let gw_mac = unsafe { net::GATEWAY_MAC };
        puts(&format!("  {}.{}.{}.{}  ", gw[0], gw[1], gw[2], gw[3]));
        draw_mac(&gw_mac, FG_DIM);
        puts("  (gateway, fallback)\n");
    } else {
        for i in 0..n {
            let (cip, cmac) = cache[i];
            puts(&format!("  {}.{}.{}.{}  ", cip[0], cip[1], cip[2], cip[3]));
            draw_mac(&cmac, FG_DIM);
            puts("\n");
        }
    }
}
