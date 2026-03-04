/*
 * ping — ICMP echo utility for AETERNA
 *
 * Linux-compatible output and argument parsing.
 *
 * Usage:
 *   ping [options] <destination>
 *
 * Options:
 *   -c <count>      Number of echo requests to send (default: infinite)
 *   -i <interval>   Interval between packets in seconds (default: 1)
 *   -s <size>       Payload size in bytes (default: 56)
 *   -W <timeout>    Time to wait for a response in seconds (default: 3)
 *
 * RTT is measured with the TSC (Time Stamp Counter) at µs precision.
 */

extern crate alloc;
use alloc::format;

use crate::arch::x86_64::framebuffer;

const FG: u32     = 0x00FFFFFF;
const FG_ERR: u32 = 0x00FF4444;
const FG_DIM: u32 = 0x00AAAAAA;
const BG: u32     = 0x00000000;

fn puts(s: &str)  { framebuffer::draw_string(s, FG, BG); }
fn err(s: &str)   { framebuffer::draw_string(s, FG_ERR, BG); }
fn dim(s: &str)   { framebuffer::draw_string(s, FG_DIM, BG); }

// ─── Configuration ────────────────────────────────────────────────────────────

struct PingConfig<'a> {
    target:      &'a str,
    ip:          [u8; 4],
    count:       Option<usize>,   // None = infinite (until Ctrl+C)
    interval_us: u64,             // microseconds between packets
    payload:     usize,           // payload bytes (default 56)
    timeout_us:  u64,             // per-packet wait timeout in µs
}

// ─── Argument parser ──────────────────────────────────────────────────────────

fn parse_args<'a>(args: &'a str) -> Option<PingConfig<'a>> {
    let mut count: Option<usize> = None;
    let mut interval_us: u64 = 1_000_000; // 1 second
    let mut payload: usize = 56;
    let mut timeout_us: u64 = 3_000_000;  // 3 seconds
    let mut target: Option<&str> = None;

    let mut words = args.split_whitespace();
    while let Some(w) = words.next() {
        match w {
            "-c" => {
                let v = words.next().unwrap_or("0");
                count = match parse_usize(v) {
                    Some(n) if n > 0 => Some(n),
                    _ => { err("ping: bad value for -c\n"); return None; }
                };
            }
            "-i" => {
                let v = words.next().unwrap_or("1");
                interval_us = match parse_seconds_to_us(v) {
                    Some(us) if us > 0 => us,
                    _ => { err("ping: bad value for -i\n"); return None; }
                };
            }
            "-s" => {
                let v = words.next().unwrap_or("56");
                payload = match parse_usize(v) {
                    Some(n) if n <= 1458 => n,
                    _ => { err("ping: -s must be 0..1458\n"); return None; }
                };
            }
            "-W" => {
                let v = words.next().unwrap_or("3");
                timeout_us = match parse_seconds_to_us(v) {
                    Some(us) if us > 0 => us,
                    _ => { err("ping: bad value for -W\n"); return None; }
                };
            }
            _ if w.starts_with('-') => {
                err("ping: unknown option: ");
                err(w);
                err("\n");
                return None;
            }
            _ => {
                target = Some(w);
            }
        }
    }

    let target = match target {
        Some(t) => t,
        None => {
            err("ping: missing destination\n");
            dim("Usage: ping [-c count] [-i interval] [-s size] [-W timeout] <destination>\n");
            return None;
        }
    };

    // Parse IP address
    let ip = match parse_ip(target) {
        Some(ip) => ip,
        None => {
            err("ping: invalid address: ");
            err(target);
            err("\n");
            return None;
        }
    };

    Some(PingConfig { target, ip, count, interval_us, payload, timeout_us })
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn parse_usize(s: &str) -> Option<usize> {
    let mut val = 0usize;
    let mut has = false;
    for &b in s.as_bytes() {
        if b >= b'0' && b <= b'9' {
            val = val.checked_mul(10)?.checked_add((b - b'0') as usize)?;
            has = true;
        } else {
            break;
        }
    }
    if has { Some(val) } else { None }
}

/// Parse a seconds value (integer or decimal with up to 6 fractional digits) → µs.
fn parse_seconds_to_us(s: &str) -> Option<u64> {
    let bytes = s.as_bytes();
    let mut int_part: u64 = 0;
    let mut frac_part: u64 = 0;
    let mut frac_digits: u32 = 0;
    let mut in_frac = false;
    let mut has_digit = false;

    for &b in bytes {
        if b == b'.' && !in_frac {
            in_frac = true;
        } else if b >= b'0' && b <= b'9' {
            has_digit = true;
            if in_frac {
                if frac_digits < 6 {
                    frac_part = frac_part * 10 + (b - b'0') as u64;
                    frac_digits += 1;
                }
            } else {
                int_part = int_part * 10 + (b - b'0') as u64;
            }
        } else {
            break;
        }
    }
    if !has_digit { return None; }

    // Pad fractional part to 6 digits (microseconds)
    while frac_digits < 6 {
        frac_part *= 10;
        frac_digits += 1;
    }

    Some(int_part * 1_000_000 + frac_part)
}

fn parse_ip(s: &str) -> Option<[u8; 4]> {
    let bytes = s.as_bytes();
    let mut ip = [0u8; 4];
    let mut octet = 0u32;
    let mut idx = 0usize;
    let mut has = false;
    for &b in bytes {
        if b == b'.' {
            if !has || octet > 255 || idx >= 3 { return None; }
            ip[idx] = octet as u8;
            idx += 1;
            octet = 0;
            has = false;
        } else if b >= b'0' && b <= b'9' {
            octet = octet * 10 + (b - b'0') as u32;
            has = true;
        } else {
            return None;
        }
    }
    if !has || octet > 255 || idx != 3 { return None; }
    ip[3] = octet as u8;
    Some(ip)
}

/// Integer square root (for mdev calculation).
fn isqrt(n: u64) -> u64 {
    if n == 0 { return 0; }
    let mut x = n;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}

fn check_ctrl_c() -> bool {
    while let Some(ch) = crate::arch::x86_64::keyboard::try_read_key() {
        if ch == '\x03' { return true; }
    }
    false
}

// ─── Format helpers ───────────────────────────────────────────────────────────

/// Format RTT µs as "X.XX ms" or "0.XXX ms" into a static buffer and return slice.
fn fmt_rtt_us(rtt_us: u64, buf: &mut [u8; 32]) -> &str {
    let ms_int  = rtt_us / 1_000;
    let ms_frac = (rtt_us % 1_000) / 10; // 2 decimal places
    let len = fmt_u64(ms_int, buf);
    buf[len] = b'.';
    buf[len + 1] = b'0' + ((ms_frac / 10) % 10) as u8;
    buf[len + 2] = b'0' + (ms_frac % 10) as u8;
    let total = len + 3;
    // SAFETY: all bytes are ASCII digits or '.'
    unsafe { core::str::from_utf8_unchecked(&buf[..total]) }
}

fn fmt_u64(mut v: u64, buf: &mut [u8; 32]) -> usize {
    if v == 0 {
        buf[0] = b'0';
        return 1;
    }
    let mut tmp = [0u8; 20];
    let mut i = 0;
    while v > 0 {
        tmp[i] = b'0' + (v % 10) as u8;
        v /= 10;
        i += 1;
    }
    for j in 0..i {
        buf[j] = tmp[i - 1 - j];
    }
    i
}

// ─── Entry point ──────────────────────────────────────────────────────────────

/// Entry point: `ping [options] <destination>`
pub fn run(args: &str) {
    let args = args.trim();

    let cfg = match parse_args(args) {
        Some(c) => c,
        None => return,
    };

    if !crate::net::is_up() {
        err("ping: network is down\n");
        return;
    }

    // ARP warm-up: resolve gateway/target MAC before first ICMP packet
    if crate::net::arp::cache_lookup(cfg.ip).is_none() {
        crate::net::arp::send_request(cfg.ip);
        let arp_deadline = crate::arch::x86_64::tsc::tsc_stamp_us() + 500_000; // 500 ms
        while crate::arch::x86_64::tsc::tsc_stamp_us() < arp_deadline {
            crate::net::poll_rx();
            if crate::net::arp::cache_lookup(cfg.ip).is_some() { break; }
            crate::core::scheduler::sys_yield();
        }
    }

    // Header:  PING 10.0.2.2 (10.0.2.2) 56(84) bytes of data.
    let total_ip = 20 + 8 + cfg.payload; // IP + ICMP hdr + payload
    puts(&format!("PING {} ({}) {}({}) bytes of data.\n",
        cfg.target, cfg.target, cfg.payload, total_ip));

    let mut sent: u64     = 0;
    let mut received: u64 = 0;
    let mut seq: u16      = 1;
    let mut interrupted   = false;

    // RTT statistics (all in µs)
    let mut rtt_min: u64 = u64::MAX;
    let mut rtt_max: u64 = 0;
    let mut rtt_sum: u64 = 0;
    let mut rtt_sum_sq: u64 = 0; // for mdev

    loop {
        // Respect -c <count>: stop after N packets
        if let Some(max) = cfg.count {
            if sent as usize >= max { break; }
        }

        // Check Ctrl+C before sending
        if check_ctrl_c() { interrupted = true; break; }

        // Send ICMP echo request
        crate::net::icmp::send_ping_sized(cfg.ip, seq, cfg.payload);
        sent += 1;

        // Wait for reply (TSC-based timeout)
        let mut reply = None;
        let wait_start = crate::arch::x86_64::tsc::tsc_stamp_us();
        loop {
            if check_ctrl_c() { interrupted = true; break; }
            crate::net::poll_rx();
            if let Some(r) = crate::net::icmp::poll_reply() {
                reply = Some(r);
                break;
            }
            let elapsed = crate::arch::x86_64::tsc::tsc_stamp_us().saturating_sub(wait_start);
            if elapsed >= cfg.timeout_us {
                crate::net::icmp::cancel_wait();
                break;
            }
            crate::core::scheduler::sys_yield();
        }
        if interrupted { break; }

        // Display result
        match reply {
            Some(r) => {
                received += 1;
                let rtt = r.rtt_us;

                // Update statistics
                if rtt < rtt_min { rtt_min = rtt; }
                if rtt > rtt_max { rtt_max = rtt; }
                rtt_sum += rtt;
                rtt_sum_sq += rtt.saturating_mul(rtt);

                let mut buf = [0u8; 32];
                let ms_str = fmt_rtt_us(rtt, &mut buf);
                puts(&format!("{} bytes from {}: icmp_seq={} ttl={} time={} ms\n",
                    r.nbytes, cfg.target, seq, r.ttl, ms_str));
            }
            None => {
                err(&format!("Request timeout for icmp_seq {}\n", seq));
            }
        }

        seq = seq.wrapping_add(1);

        // Inter-packet delay (skip if this was the last packet in counted mode)
        if let Some(max) = cfg.count {
            if sent as usize >= max { break; }
        }
        if check_ctrl_c() { interrupted = true; break; }

        let delay_start = crate::arch::x86_64::tsc::tsc_stamp_us();
        while crate::arch::x86_64::tsc::tsc_stamp_us().saturating_sub(delay_start) < cfg.interval_us {
            if check_ctrl_c() { interrupted = true; break; }
            crate::net::poll_rx();
            crate::core::scheduler::sys_yield();
        }
        if interrupted { break; }
    }

    // ── Summary ──
    if interrupted {
        puts("\n");
    }

    let lost = sent.saturating_sub(received);
    let loss_pct = if sent > 0 { lost * 100 / sent } else { 0 };

    puts(&format!("\n--- {} ping statistics ---\n", cfg.target));
    puts(&format!("{} packets transmitted, {} received, {}% packet loss\n",
        sent, received, loss_pct));

    if received > 0 {
        let avg = rtt_sum / received;
        // mdev = sqrt(E[x²] - (E[x])²)
        let mean_sq = rtt_sum_sq / received;
        let sq_mean = avg.saturating_mul(avg);
        let variance = mean_sq.saturating_sub(sq_mean);
        let mdev = isqrt(variance);
        if rtt_min == u64::MAX { rtt_min = 0; }

        let mut buf_min  = [0u8; 32];
        let mut buf_avg  = [0u8; 32];
        let mut buf_max  = [0u8; 32];
        let mut buf_mdev = [0u8; 32];
        let s_min  = fmt_rtt_us(rtt_min, &mut buf_min);
        let s_avg  = fmt_rtt_us(avg, &mut buf_avg);
        let s_max  = fmt_rtt_us(rtt_max, &mut buf_max);
        let s_mdev = fmt_rtt_us(mdev, &mut buf_mdev);

        puts(&format!("rtt min/avg/max/mdev = {}/{}/{}/{} ms\n",
            s_min, s_avg, s_max, s_mdev));
    }
}
