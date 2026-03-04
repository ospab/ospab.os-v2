/*
 * axon/net_tools — Network & disk commands: netstat, ping, df
 *
 * Self-contained logical unit.  Contacts the kernel net stack through
 * crate::net::* and the filesystem through crate::fs::*.
 *
 * All timer-based deadlines are calibrated for PIT at 100 Hz (10 ms/tick).
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

fn puts(s: &str)  { framebuffer::draw_string(s, FG, BG); }
fn ok(s: &str)    { framebuffer::draw_string(s, FG_OK, BG); }
fn err(s: &str)   { framebuffer::draw_string(s, FG_ERR, BG); }
fn dim(s: &str)   { framebuffer::draw_string(s, FG_DIM, BG); }
fn hl(s: &str)    { framebuffer::draw_string(s, FG_HL, BG); }

fn put_usize(mut n: usize) {
    if n == 0 { puts("0"); return; }
    let mut buf = [0u8; 20];
    let mut i = 0;
    while n > 0 { buf[i] = b'0' + (n % 10) as u8; n /= 10; i += 1; }
    for k in (0..i).rev() { framebuffer::draw_char(buf[k] as char, FG, BG); }
}

/// Inline hex nibble table
const HEX: [u8; 16] = *b"0123456789abcdef";

fn draw_mac(mac: &[u8; 6]) {
    for i in 0..6 {
        framebuffer::draw_char(HEX[(mac[i] >> 4) as usize] as char, FG, BG);
        framebuffer::draw_char(HEX[(mac[i] & 0xF) as usize] as char, FG, BG);
        if i < 5 { puts(":"); }
    }
}

fn draw_mac_dim(mac: &[u8; 6]) {
    for i in 0..6 {
        framebuffer::draw_char(HEX[(mac[i] >> 4) as usize] as char, FG_DIM, BG);
        framebuffer::draw_char(HEX[(mac[i] & 0xF) as usize] as char, FG_DIM, BG);
        if i < 5 { puts(":"); }
    }
}

// ─── netstat ─────────────────────────────────────────────────────────────────

pub fn cmd_netstat(_args: &str) {
    use crate::net;

    if !net::is_up() {
        err("netstat: no network interface is up\n");
        return;
    }

    // Interface table
    hl("  Interface    MAC                  IP              Status    RX        TX\n");
    dim("  -------------------------------------------------------------------------\n");

    let mac = unsafe { net::OUR_MAC };
    let ip  = unsafe { net::OUR_IP };
    let rx  = net::rx_packets();
    let tx  = net::tx_packets();
    let nic = net::nic_name();

    puts("  ");
    puts(nic);
    for _ in 0..(13usize.saturating_sub(nic.len())) { puts(" "); }

    draw_mac(&mac);
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
    puts("\n");

    // ARP cache
    let gw      = unsafe { net::GATEWAY_IP };
    let gw_mac  = unsafe { net::GATEWAY_MAC };
    puts("\n");
    hl("  ARP Cache:\n");
    dim("  IP              MAC\n");
    dim("  -------------------------------------\n");

    let mut cache = [([0u8; 4], [0u8; 6]); 16];
    let n = net::arp::cache_entries(&mut cache);
    for i in 0..n {
        let (cip, cmac) = cache[i];
        let ip_s = format!("  {}.{}.{}.{}", cip[0], cip[1], cip[2], cip[3]);
        puts(&ip_s);
        for _ in 0..(18usize.saturating_sub(ip_s.len())) { puts(" "); }
        draw_mac_dim(&cmac);
        puts("\n");
    }
    if n == 0 {
        puts("  Gateway ");
        puts(&format!("{}.{}.{}.{}", gw[0], gw[1], gw[2], gw[3]));
        puts("  ");
        draw_mac_dim(&gw_mac);
        puts("\n");
    }
}

// ─── ping ────────────────────────────────────────────────────────────────────
//
// A fully self-contained ICMP ping inside axon, separate from the terminal
// built-in.  All timeouts calibrated for PIT at 100 Hz (10 ms/tick):
//   3 s reply window  = 300 ticks
//   1 s inter-packet  = 100 ticks

pub fn cmd_ping(args: &str) {
    let args = args.trim();
    if args.is_empty() {
        err("ping: missing host\n");
        dim("Usage: ping [-c N] [-i secs] <host|ip>\n");
        dim("  Example: ping 10.0.2.2\n");
        dim("  Example: ping -c 3 localhost\n");
        return;
    }

    // Parse args: [-c N] [-i secs] <host|ip>
    let mut count      = 4usize;
    let mut target     = "";
    let mut iticks     = 100u64; // 1 s at 100 Hz
    let mut words_iter = args.split_whitespace().peekable();
    while let Some(w) = words_iter.next() {
        match w {
            "-c" => {
                if let Some(v) = words_iter.next() {
                    count = v.parse::<usize>().unwrap_or(4).max(1);
                }
            }
            "-i" => {
                if let Some(v) = words_iter.next() {
                    if let Ok(f) = v.parse::<f32>() {
                        iticks = (f * 100.0) as u64;
                    }
                }
            }
            _ if w.starts_with('-') => {
                err("ping: unknown option: "); err(w); err("\n");
                return;
            }
            _ => { target = w; }
        }
    }
    if target.is_empty() {
        err("ping: missing destination\n");
        return;
    }

    // Resolve: IPv4 literal first, then /etc/hosts
    let ip = match crate::net::resolver::parse_ipv4(target) {
        Some(ip) => ip,
        None => match crate::net::resolver::resolve_host(target) {
            Ok(ip) => {
                ok("Resolved "); puts(target); ok(" -> ");
                puts(&alloc::format!("{}.{}.{}.{}\n", ip[0], ip[1], ip[2], ip[3]));
                ip
            }
            Err(e) => {
                err("ping: cannot resolve "); err(target);
                err(": "); err(e.as_str()); err("\n");
                return;
            }
        },
    };

    if !crate::net::is_up() {
        err("ping: network is down\n");
        return;
    }

    ok("PING "); puts(target); ok(" 56(84) bytes of data\n");

    // Ensure ARP is warm for the target (send a gratuitous ARP first)
    crate::net::arp::send_request(ip);
    let arp_deadline = crate::arch::x86_64::idt::timer_ticks() + 30; // 300 ms
    while crate::arch::x86_64::idt::timer_ticks() < arp_deadline {
        crate::net::poll_rx();
        if crate::net::arp::cache_lookup(ip).is_some() { break; }
        crate::core::scheduler::sys_yield();
    }

    let mut received = 0usize;
    for seq in 1..=count {
        crate::net::icmp::send_ping(ip, seq as u16);

        // Wait up to 3 s (300 ticks @ 100 Hz) for ICMP echo reply
        let mut reply = None;
        let deadline = crate::arch::x86_64::idt::timer_ticks() + 300;
        while crate::arch::x86_64::idt::timer_ticks() < deadline {
            reply = crate::net::icmp::poll_reply();
            if reply.is_some() { break; }
            unsafe { core::arch::asm!("hlt"); }
        }
        if reply.is_none() {
            crate::net::icmp::cancel_wait();
        }

        match reply {
            Some(r) => {
                received += 1;
                let rtt_us = r.rtt_us;
                let display_ms = if rtt_us < 1000 { 1 } else { rtt_us / 1000 };
                ok("64 bytes from "); puts(target);
                puts(&format!(": icmp_seq={} ttl={} time={}ms\n", seq, r.ttl, display_ms));
            }
            None => {
                err("Request timeout for icmp_seq=");
                err(&format!("{}\n", seq));
            }
        }

        // inter-ping delay (default 1 s = 100 ticks @ 100 Hz; overridden by -i)
        if seq < count {
            let wait = crate::arch::x86_64::idt::timer_ticks() + iticks;
            while crate::arch::x86_64::idt::timer_ticks() < wait {
                crate::net::poll_rx();
                unsafe { core::arch::asm!("hlt"); }
            }
        }
    }

    puts("\n");
    dim(&format!("--- {} ping statistics ---\n", target));
    let lost = count - received;
    let loss_pct = if count > 0 { lost * 100 / count } else { 0 };
    dim(&format!("{} packets transmitted, {} received, {}% packet loss\n",
        count, received, loss_pct));
}

// ─── df ──────────────────────────────────────────────────────────────────────

pub fn cmd_df(_args: &str) {
    hl("  Filesystem      1K-blocks   Used  Available  Use%  Mounted on\n");
    dim("  ---------------------------------------------------------------\n");

    // RamFS usage
    let node_count = crate::fs::ramfs::node_count();
    let ramfs_used_kb = (node_count * 256) / 1024;
    let heap_total_kb = if crate::mm::heap::is_initialized() {
        let (used, free) = crate::mm::heap::stats();
        (used + free) / 1024
    } else { 131072 }; // 128 MiB default

    puts("  ramfs           ");
    put_usize(heap_total_kb); puts("  ");
    put_usize(ramfs_used_kb); puts("  ");
    put_usize(heap_total_kb.saturating_sub(ramfs_used_kb));
    puts("  ");
    if heap_total_kb > 0 {
        put_usize(ramfs_used_kb * 100 / heap_total_kb);
    } else { puts("0"); }
    puts("%  /\n");

    // Physical disk(s)
    let disk_count = crate::drivers::disk_count();
    for i in 0..disk_count {
        if let Some(info) = crate::drivers::disk_info(i) {
            let disk_kb = info.size_mb as usize * 1024;
            let used_kb = 8 * 1024; // persistence region: ~8 MiB
            puts("  disk");
            put_usize(i);
            puts("          ");
            put_usize(disk_kb); puts("  ");
            put_usize(used_kb); puts("  ");
            put_usize(disk_kb.saturating_sub(used_kb));
            puts("  ");
            if disk_kb > 0 {
                put_usize(used_kb * 100 / disk_kb);
            } else { puts("0"); }
            puts("%  /dev/disk");
            put_usize(i);
            puts("\n");
        }
    }
}
