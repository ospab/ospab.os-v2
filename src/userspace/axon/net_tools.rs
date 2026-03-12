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

fn check_ctrl_c() -> bool {
    while let Some(ch) = crate::arch::x86_64::keyboard::try_read_key() {
        if ch == '\x03' { return true; }
    }
    false
}

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

    if net::link_up() {
        ok("Up");
    } else {
        err("Down");
    }
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
        dim("  Example: ping 8.8.8.8\n");
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
            let raw = crate::fs::disk_sync::last_snapshot_bytes();
            let used_kb = if raw > 0 { raw / 1024 } else { 0 };
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

// ─── traceroute ───────────────────────────────────────────────────────────────
//
// ICMP-based traceroute: sends Echo Requests with incrementing IP TTL values.
// Each router that drops the packet due to TTL=0 replies with ICMP Time
// Exceeded (type 11), revealing the route.
//
// In QEMU user-mode networking intermediate hops silently drop the expired
// packets (the virtual NAT does not forward Time Exceeded replies back), so
// those hops will show "* * *" — the same behaviour as any NATted environment.

pub fn cmd_traceroute(args: &str) {
    let args = args.trim();
    if args.is_empty() {
        err("traceroute: missing host\n");
        dim("Usage: traceroute [-m max_hops] [-w secs] <host|ip>\n");
        dim("  Example: traceroute 8.8.8.8\n");
        return;
    }

    // ── Argument parsing ─────────────────────────────────────────────────────
    let mut max_hops: u8  = 30;
    let mut timeout_us    = 3_000_000u64; // 3 s per hop
    let mut target        = "";
    let mut words         = args.split_whitespace().peekable();

    while let Some(w) = words.next() {
        match w {
            "-m" => {
                if let Some(v) = words.next() {
                    max_hops = v.parse::<u8>().unwrap_or(30).max(1);
                }
            }
            "-w" => {
                if let Some(v) = words.next() {
                    if let Ok(secs) = v.parse::<u32>() {
                        timeout_us = secs as u64 * 1_000_000;
                    }
                }
            }
            _ if w.starts_with('-') => {
                err("traceroute: unknown option: "); err(w); err("\n");
                return;
            }
            _ => { target = w; }
        }
    }
    if target.is_empty() {
        err("traceroute: missing destination\n");
        return;
    }

    // ── Resolve destination ───────────────────────────────────────────────────
    let dst_ip = match crate::net::resolver::parse_ipv4(target) {
        Some(ip) => ip,
        None => match crate::net::resolver::resolve_host(target) {
            Ok(ip) => {
                ok("Resolved "); puts(target); ok(" -> ");
                puts(&format!("{}.{}.{}.{}\n", ip[0], ip[1], ip[2], ip[3]));
                ip
            }
            Err(e) => {
                err("traceroute: cannot resolve "); err(target);
                err(": "); err(e.as_str()); err("\n");
                return;
            }
        },
    };

    if !crate::net::is_up() {
        err("traceroute: network is down\n");
        return;
    }

    // ── Warm up ARP for the destination / gateway ─────────────────────────────
    crate::net::arp::send_request(dst_ip);
    let arp_dl = crate::arch::x86_64::idt::timer_ticks() + 30;
    while crate::arch::x86_64::idt::timer_ticks() < arp_dl {
        crate::net::poll_rx();
        crate::core::scheduler::sys_yield();
        if crate::net::arp::cache_lookup(dst_ip).is_some() { break; }
    }

    // ── Header ────────────────────────────────────────────────────────────────
    hl("traceroute to ");
    puts(target);
    hl(&format!(" ({}.{}.{}.{})", dst_ip[0], dst_ip[1], dst_ip[2], dst_ip[3]));
    puts(&format!(", {} hops max\n", max_hops));

    // ── Probe loop ────────────────────────────────────────────────────────────
    for ttl in 1u8..=max_hops {
        // Print hop number (right-aligned, 2 chars)
        if ttl < 10 { puts(" "); }
        put_usize(ttl as usize);
        puts("  ");

        // Send 3 probes per hop (classic traceroute behaviour)
        let mut any_reply = false;
        let mut reached   = false;
        let mut last_src  = [0u8; 4];

        for probe in 0u16..3 {
            let seq = (ttl as u16) * 10 + probe;
            crate::net::icmp::send_ping_ttl(dst_ip, seq, ttl);

            // Wait for reply with TSC-based timeout
            let start = crate::arch::x86_64::tsc::tsc_stamp_us();
            let mut reply = None;
            loop {
                reply = crate::net::icmp::poll_reply();
                if reply.is_some() { break; }
                let elapsed = crate::arch::x86_64::tsc::tsc_stamp_us()
                    .saturating_sub(start);
                if elapsed >= timeout_us {
                    crate::net::icmp::cancel_wait();
                    break;
                }
                crate::core::scheduler::sys_yield();
            }

            match reply {
                Some(r) => {
                    any_reply = true;
                    last_src  = r.src_ip;
                    let rtt_ms = if r.rtt_us < 1000 { 1 } else { r.rtt_us / 1000 };
                    puts(&format!("{} ms  ", rtt_ms));
                    if !r.is_ttl_exceeded {
                        // Echo Reply from destination: we've arrived
                        reached = true;
                    }
                }
                None => {
                    puts("*  ");
                }
            }
        }

        // Print replier's IP (nothing for pure timeouts)
        if any_reply {
            let ip_s = format!("{}.{}.{}.{}", last_src[0], last_src[1], last_src[2], last_src[3]);
            ok(&format!(" {}\n", ip_s));
        } else {
            puts("(no reply)\n");
        }

        // Destination reached — stop probing
        if reached { break; }

        // Ctrl+C bail-out
        if check_ctrl_c() {
            puts("^C\n");
            break;
        }
    }
}

// ─── ip addr / ip link / ip route ────────────────────────────────────────────

fn put_ip(ip: [u8; 4]) {
    puts(&format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]));
}

fn subnet_prefix_len(mask: [u8; 4]) -> u8 {
    mask.iter().map(|b| b.count_ones() as u8).sum()
}

fn ip_broadcast(ip: [u8; 4], mask: [u8; 4]) -> [u8; 4] {
    [ip[0] | !mask[0], ip[1] | !mask[1], ip[2] | !mask[2], ip[3] | !mask[3]]
}

fn ip_network(ip: [u8; 4], mask: [u8; 4]) -> [u8; 4] {
    [ip[0] & mask[0], ip[1] & mask[1], ip[2] & mask[2], ip[3] & mask[3]]
}

pub fn cmd_ip_show(args: &str) {
    use crate::net;

    if !net::is_up() {
        err("ip: network not available\n");
        return;
    }
    let sub = args.split_whitespace().next().unwrap_or("");
    match sub {
        "" | "a" | "addr" | "address" => cmd_ip_show_addr(),
        "l" | "link" => cmd_ip_show_link(),
        "r" | "route" => cmd_ip_show_route(),
        _ => {
            err("ip: unknown subcommand\n");
            dim("Usage: ip {a[ddr] | l[ink] | r[oute]}\n");
        }
    }
}

fn cmd_ip_show_addr() {
    use crate::net;
    let ip   = unsafe { net::OUR_IP };
    let mask = unsafe { net::SUBNET_MASK };
    let mac  = unsafe { net::OUR_MAC };
    let nic  = net::nic_name();
    let up   = net::link_up();
    let plen = subnet_prefix_len(mask);
    let brd  = ip_broadcast(ip, mask);
    let rx_p = net::rx_packets();
    let tx_p = net::tx_packets();

    // Loopback
    dim("1: lo: <LOOPBACK,UP,LOWER_UP> mtu 65536 state UNKNOWN\n");
    dim("    link/loopback 00:00:00:00:00:00 brd 00:00:00:00:00:00\n");
    dim("    inet 127.0.0.1/8 scope host lo\n\n");

    // NIC
    puts("2: ");
    hl(nic);
    puts(": <BROADCAST,MULTICAST,");
    if up { ok("UP,LOWER_UP"); } else { err("NO-CARRIER"); }
    puts("> mtu 1500 state ");
    if up { ok("UP"); } else { err("DOWN"); }
    puts("\n");
    puts("    link/ether ");
    draw_mac(&mac);
    puts(" brd ff:ff:ff:ff:ff:ff\n");
    puts("    inet ");
    put_ip(ip);
    puts(&format!("/{} brd ", plen));
    put_ip(brd);
    puts(" scope global ");
    puts(nic);
    puts("\n");
    puts("    RX: ");
    put_usize(rx_p as usize);
    puts(" pkts  TX: ");
    put_usize(tx_p as usize);
    puts(" pkts\n");
}

fn cmd_ip_show_link() {
    use crate::net;
    let mac = unsafe { net::OUR_MAC };
    let nic = net::nic_name();
    let up  = net::link_up();
    let rx  = net::rx_packets();
    let tx  = net::tx_packets();
    let rxb = net::rx_bytes();
    let txb = net::tx_bytes();

    dim("1: lo: <LOOPBACK,UP,LOWER_UP> mtu 65536 state UNKNOWN\n");
    dim("    link/loopback 00:00:00:00:00:00 brd 00:00:00:00:00:00\n\n");

    puts("2: ");
    hl(nic);
    puts(": <BROADCAST,MULTICAST,");
    if up { ok("UP,LOWER_UP"); } else { err("NO-CARRIER"); }
    puts("> mtu 1500 state ");
    if up { ok("UP"); } else { err("DOWN"); }
    puts("\n");
    puts("    link/ether ");
    draw_mac(&mac);
    puts(" brd ff:ff:ff:ff:ff:ff\n");
    puts(&format!("    RX: packets={} bytes={}\n", rx, rxb));
    puts(&format!("    TX: packets={} bytes={}\n", tx, txb));
}

fn cmd_ip_show_route() {
    use crate::net;
    let ip   = unsafe { net::OUR_IP };
    let gw   = unsafe { net::GATEWAY_IP };
    let mask = unsafe { net::SUBNET_MASK };
    let nic  = net::nic_name();
    let plen = subnet_prefix_len(mask);
    let net_addr = ip_network(ip, mask);

    puts("default via ");
    put_ip(gw);
    puts(" dev ");
    puts(nic);
    puts("\n");
    put_ip(net_addr);
    puts(&format!("/{} dev ", plen));
    puts(nic);
    puts(" proto kernel scope link src ");
    put_ip(ip);
    puts("\n");
    puts("127.0.0.0/8 dev lo scope host\n");
}

// ─── nslookup ─────────────────────────────────────────────────────────────────

pub fn cmd_nslookup(args: &str) {
    let args = args.trim();
    if args.is_empty() {
        err("nslookup: missing hostname\n");
        dim("Usage: nslookup <hostname|ip>\n");
        return;
    }

    if !crate::net::is_up() {
        err("nslookup: network is down\n");
        return;
    }

    let dns_ip = unsafe { crate::net::DNS_IP };
    let dns_str = format!("{}.{}.{}.{}", dns_ip[0], dns_ip[1], dns_ip[2], dns_ip[3]);

    dim(&format!("Server:\t\t{}\n", dns_str));
    dim(&format!("Address:\t{}#53\n\n", dns_str));

    match crate::net::resolver::resolve_host(args) {
        Ok(ip) => {
            dim("Non-authoritative answer:\n");
            puts(&format!("Name:\t{}\n", args));
            ok(&format!("Address: {}.{}.{}.{}\n", ip[0], ip[1], ip[2], ip[3]));
        }
        Err(e) => {
            err(&format!("nslookup: can't resolve '{}': {}\n", args, e.as_str()));
        }
    }
}

// ─── curl — HTTP/1.x over real TCP ───────────────────────────────────────────

/// Decode HTTP/1.1 chunked transfer encoding.
fn decode_chunked(data: &[u8]) -> alloc::vec::Vec<u8> {
    let mut out = alloc::vec::Vec::new();
    let mut pos = 0usize;
    while pos < data.len() {
        // find CRLF terminating the chunk-size line
        let mut lf = pos;
        while lf + 1 < data.len() && !(data[lf] == b'\r' && data[lf + 1] == b'\n') {
            lf += 1;
        }
        if lf + 1 >= data.len() { break; }
        let hex_s = core::str::from_utf8(&data[pos..lf]).unwrap_or("0");
        let hex_s = hex_s.split(';').next().unwrap_or("0").trim();
        let chunk_size = usize::from_str_radix(hex_s, 16).unwrap_or(0);
        pos = lf + 2; // skip chunk-size CRLF
        if chunk_size == 0 { break; } // terminal chunk
        let take = chunk_size.min(data.len().saturating_sub(pos));
        out.extend_from_slice(&data[pos..pos + take]);
        pos += take + 2; // skip data + trailing CRLF
    }
    out
}

/// Write response bytes to the framebuffer (printable + common whitespace).
fn print_http_bytes(data: &[u8], color: u32) {
    for &b in data {
        if b >= 0x20 || b == b'\n' || b == b'\r' || b == b'\t' {
            framebuffer::draw_char(b as char, color, BG);
        }
    }
}

pub fn cmd_curl(args: &str) {
    use alloc::string::String;
    use alloc::vec::Vec;

    let args = args.trim();
    if args.is_empty() {
        err("curl: missing URL\n");
        dim("Usage: curl [-IiLvs] [-H hdr] [-X method] [-d data] <URL>\n");
        dim("  -I / --head      HEAD request — print headers only\n");
        dim("  -i / --include   Include response headers in output\n");
        dim("  -v / --verbose   Verbose (show request + response)\n");
        dim("  -L / --location  Follow redirects\n");
        dim("  -H hdr           Add custom request header\n");
        dim("  -X method        Override HTTP method\n");
        dim("  -d data          POST data\n");
        return;
    }

    if !crate::net::is_up() { err("curl: network is down\n"); return; }

    // ── Parse flags ───────────────────────────────────────────────────────────
    let mut head_only   = false; // -I / --head
    let mut inc_hdrs    = false; // -i / --include
    let mut verbose     = false; // -v / --verbose
    let mut silent      = false; // -s / --silent
    let mut follow      = false; // -L / --location
    let mut method_ovrd = "";
    let mut post_data   = "";
    let mut url_raw     = "";
    let mut extra: Vec<&str> = Vec::new();
    {
        let mut it = args.split_ascii_whitespace();
        while let Some(tok) = it.next() {
            match tok {
                "-I" | "--head"      => head_only   = true,
                "-i" | "--include"   => inc_hdrs    = true,
                "-v" | "--verbose"   => verbose     = true,
                "-s" | "--silent"    => silent      = true,
                "-L" | "--location"  => follow      = true,
                "-H" | "--header"    => { extra.push(it.next().unwrap_or("")); }
                "-X" | "--request"   => { method_ovrd = it.next().unwrap_or(""); }
                "-d" | "--data"      => { post_data  = it.next().unwrap_or(""); }
                "-o" | "--output"    => { let _ = it.next(); }
                _ if !tok.starts_with('-') => url_raw = tok,
                _ => {}
            }
        }
    }
    if url_raw.is_empty() { err("curl: missing URL\n"); return; }

    let http_method = if !method_ovrd.is_empty() { method_ovrd }
        else if head_only { "HEAD" }
        else if !post_data.is_empty() { "POST" }
        else { "GET" };

    // ── Redirect loop ─────────────────────────────────────────────────────────
    let mut cur_url = String::from(url_raw);
    'redir: for _depth in 0..=10usize {

        // ── Parse URL into owned components ────────────────────────────────
        let (use_tls, host, port, path): (bool, String, u16, String) = {
            let s: &str = &cur_url;
            let (tls, rest) = if s.starts_with("https://") {
                (true,  &s[8..])
            } else if s.starts_with("http://") {
                (false, &s[7..])
            } else {
                (false, s)
            };
            let dflt = if tls { 443u16 } else { 80u16 };
            let (hp, p) = match rest.find('/') {
                Some(i) => (&rest[..i], &rest[i..]),
                None    => (rest, "/"),
            };
            let (h, po) = match hp.rfind(':') {
                Some(c) => (&hp[..c], hp[c+1..].parse::<u16>().unwrap_or(dflt)),
                None    => (hp, dflt),
            };
            (tls, String::from(h), po, String::from(p))
        };

        // ── Resolve ──────────────────────────────────────────────────────────
        let ip = match crate::net::resolver::resolve_host(&host) {
            Ok(ip) => ip,
            Err(e) => {
                err("curl: cannot resolve '"); err(&host);
                err("': "); err(e.as_str()); err("\n");
                return;
            }
        };

        if verbose {
            dim(&format!("* Trying {}.{}.{}.{}:{} ...\n",
                ip[0], ip[1], ip[2], ip[3], port));
        }

        // ── Build HTTP/1.1 request ────────────────────────────────────────────
        let mut req = String::with_capacity(256);
        req.push_str(http_method); req.push(' ');
        req.push_str(&path); req.push_str(" HTTP/1.1\r\n");
        req.push_str("Host: "); req.push_str(&host); req.push_str("\r\n");
        req.push_str("User-Agent: curl/8.0.0 (AETERNA)\r\n");
        req.push_str("Accept: */*\r\n");
        req.push_str("Connection: close\r\n");
        for h in &extra {
            if !h.is_empty() { req.push_str(h); req.push_str("\r\n"); }
        }
        if !post_data.is_empty() {
            req.push_str(&format!(
                "Content-Length: {}\r\nContent-Type: application/x-www-form-urlencoded\r\n",
                post_data.len()));
        }
        req.push_str("\r\n");
        if !post_data.is_empty() { req.push_str(post_data); }

        if verbose {
            for line in req.lines() { dim("> "); puts(line); puts("\n"); }
            dim(">\n");
        }

        // ── Connect, send, and receive the full response ─────────────────────
        let raw: Vec<u8> = 'conn: {
            if use_tls {
                let mut tls = match crate::net::tls::connect(ip, port, &host) {
                    Ok(c)  => c,
                    Err(e) => { err("curl: TLS failed: "); err(e); err("\n"); return; }
                };
                if verbose { dim("* SSL connection established\n"); }
                if tls.send(req.as_bytes()).is_err() {
                    err("curl: TLS send failed\n"); tls.close(); return;
                }
                let mut acc: Vec<u8> = Vec::with_capacity(8192);
                let mut buf = [0u8; 4096];
                loop {
                    if check_ctrl_c() { dim("\n[interrupted]\n"); tls.close(); return; }
                    match tls.recv(&mut buf, 500) {
                        Ok(0)  => break,
                        Ok(n)  => acc.extend_from_slice(&buf[..n]),
                        Err(_) => break,
                    }
                    if acc.len() > 512 * 1024 { break; } // cap at 512 KB
                }
                tls.close();
                break 'conn acc;
            } else {
                let conn = match crate::net::tcp::tcp_connect(ip, port) {
                    Ok(c)  => c,
                    Err(e) => { err("curl: connect failed: "); err(e.as_str()); err("\n"); return; }
                };
                if crate::net::tcp::tcp_send(conn, req.as_bytes()).is_err() {
                    err("curl: send failed\n");
                    crate::net::tcp::tcp_close(conn);
                    return;
                }
                let mut acc: Vec<u8> = Vec::with_capacity(8192);
                let mut buf = [0u8; 2048];
                loop {
                    if check_ctrl_c() {
                        dim("\n[interrupted]\n");
                        crate::net::tcp::tcp_close(conn);
                        return;
                    }
                    match crate::net::tcp::tcp_recv(conn, &mut buf, 400) {
                        Ok(0) => break,
                        Ok(n) => acc.extend_from_slice(&buf[..n]),
                        Err(crate::net::tcp::TcpError::TimedOut)
                        | Err(crate::net::tcp::TcpError::WouldBlock) => break,
                        Err(e) => { err("\ncurl: recv: "); err(e.as_str()); err("\n"); break; }
                    }
                    if acc.len() > 512 * 1024 { break; }
                }
                crate::net::tcp::tcp_close(conn);
                break 'conn acc;
            }
        };

        if raw.is_empty() {
            if !silent { err("curl: empty response\n"); }
            return;
        }

        // ── Split headers / body at the first \r\n\r\n ───────────────────────
        let sep_pos = match raw.windows(4).position(|w| w == b"\r\n\r\n") {
            Some(i) => i,
            None    => { print_http_bytes(&raw, FG); return; }
        };
        let hdr_bytes = &raw[..sep_pos];
        let body_raw  = &raw[sep_pos + 4..];

        // ── Parse status line + key headers ──────────────────────────────────
        let hdr_str = core::str::from_utf8(hdr_bytes).unwrap_or("");
        let mut status_code: u16 = 0;
        let mut status_text      = "";
        let mut is_chunked       = false;
        let mut redirect_url: Option<String> = None;

        for (idx, raw_line) in hdr_str.split('\n').enumerate() {
            let line = raw_line.trim_end_matches('\r');
            if idx == 0 {
                status_text = line;
                let mut parts = line.splitn(3, ' ');
                let _ = parts.next(); // "HTTP/x.y"
                if let Some(c) = parts.next() {
                    status_code = c.trim().parse().unwrap_or(0);
                }
            } else if let Some(col) = line.find(':') {
                let key = line[..col].trim();
                let val = line[col + 1..].trim();
                if key.eq_ignore_ascii_case("transfer-encoding")
                    && val.eq_ignore_ascii_case("chunked")
                {
                    is_chunked = true;
                } else if key.eq_ignore_ascii_case("location") {
                    redirect_url = Some(String::from(val));
                }
            }
        }

        if verbose {
            for raw_line in hdr_str.split('\n') {
                let line = raw_line.trim_end_matches('\r');
                dim("< "); puts(line); puts("\n");
            }
            dim("<\n");
        }

        // ── Follow redirect (3xx) ─────────────────────────────────────────────
        if follow && (300..=399).contains(&status_code) {
            if let Some(loc) = redirect_url {
                if !silent {
                    dim(&format!("* Following {} redirect to {}\n", status_code, &loc));
                }
                cur_url = if loc.starts_with("http://") || loc.starts_with("https://") {
                    loc
                } else {
                    let scheme = if use_tls { "https" } else { "http" };
                    format!("{}://{}{}", scheme, &host, &loc)
                };
                continue 'redir;
            }
        }

        // ── Decode body ───────────────────────────────────────────────────────
        let body = if is_chunked {
            decode_chunked(body_raw)
        } else {
            body_raw.to_vec()
        };

        // ── Display based on mode ─────────────────────────────────────────────
        if head_only {
            // -I: status line + headers, no body
            puts(status_text); puts("\n");
            for raw_line in hdr_str.split('\n').skip(1) {
                let line = raw_line.trim_end_matches('\r');
                if line.is_empty() { break; }
                puts(line); puts("\n");
            }
        } else if inc_hdrs {
            // -i: headers + body
            print_http_bytes(hdr_bytes, FG_DIM);
            puts("\r\n\r\n");
            print_http_bytes(&body, FG);
        } else if verbose {
            // -v: request+response headers already shown, just print body
            print_http_bytes(&body, FG);
        } else {
            // default: body only (like real curl)
            print_http_bytes(&body, FG);
        }

        if !silent { puts("\n"); }
        break 'redir;
    }
}
