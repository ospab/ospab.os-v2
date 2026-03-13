/*
 * DNS Resolver — RFC 1035 stub resolver
 *
 * Sends standard A-record queries over UDP port 53 to the DNS server
 * configured via DHCP (stored in super::DNS_IP).
 *
 * Flow:
 *   1. Build DNS query (QR=0, OPCODE=0, QDCOUNT=1, QTYPE=A, QCLASS=IN)
 *   2. Send UDP to DNS_IP:53
 *   3. Wait for reply, parse answer section for A record
 *   4. Simple 16-entry cache with 60 s TTL
 *
 * No QEMU / SLIRP assumptions — pure RFC 1035 over real UDP.
 */

use core::sync::atomic::{AtomicBool, AtomicU16, Ordering};

// ─── DNS RX buffer — written by UDP handler, read by dns code ────────────
static mut DNS_RX_BUF: [u8; 512] = [0; 512];
static mut DNS_RX_LEN: usize = 0;
static DNS_RX_READY: AtomicBool = AtomicBool::new(false);

/// Called from UDP dispatch when dst_port matches our ephemeral DNS source port.
pub fn handle_dns_udp(data: &[u8]) {
    let copy = data.len().min(512);
    unsafe {
        DNS_RX_BUF[..copy].copy_from_slice(&data[..copy]);
        DNS_RX_LEN = copy;
    }
    DNS_RX_READY.store(true, Ordering::Release);
}

// ─── Cache ───────────────────────────────────────────────────────────────
const CACHE_SIZE: usize = 16;
const CACHE_TTL_TICKS: u64 = 6000; // 60 seconds at 100 Hz

struct CacheEntry {
    name: [u8; 64],
    name_len: u8,
    ip: [u8; 4],
    stamp: u64,
    valid: bool,
}

impl CacheEntry {
    const fn empty() -> Self {
        CacheEntry { name: [0; 64], name_len: 0, ip: [0; 4], stamp: 0, valid: false }
    }
}

static mut DNS_CACHE: [CacheEntry; CACHE_SIZE] = {
    const EMPTY: CacheEntry = CacheEntry::empty();
    [EMPTY; CACHE_SIZE]
};
static mut CACHE_NEXT: usize = 0;

fn cache_lookup(name: &str) -> Option<[u8; 4]> {
    let now = crate::arch::x86_64::idt::timer_ticks();
    let nb = name.as_bytes();
    unsafe {
        for e in DNS_CACHE.iter_mut() {
            if e.valid && e.name_len as usize == nb.len()
                && &e.name[..e.name_len as usize] == nb
            {
                if now.wrapping_sub(e.stamp) < CACHE_TTL_TICKS {
                    return Some(e.ip);
                } else {
                    e.valid = false;
                }
            }
        }
    }
    None
}

fn cache_insert(name: &str, ip: [u8; 4]) {
    let now = crate::arch::x86_64::idt::timer_ticks();
    let nb = name.as_bytes();
    if nb.len() > 64 { return; }
    unsafe {
        let slot = CACHE_NEXT % CACHE_SIZE;
        DNS_CACHE[slot].name[..nb.len()].copy_from_slice(nb);
        DNS_CACHE[slot].name_len = nb.len() as u8;
        DNS_CACHE[slot].ip = ip;
        DNS_CACHE[slot].stamp = now;
        DNS_CACHE[slot].valid = true;
        CACHE_NEXT += 1;
    }
}

// ─── Query ID counter ────────────────────────────────────────────────────
static DNS_QUERY_ID: AtomicU16 = AtomicU16::new(1);
/// Ephemeral source port for DNS — so UDP handler knows to route here.
static DNS_SRC_PORT: AtomicU16 = AtomicU16::new(0);

pub fn current_src_port() -> u16 {
    DNS_SRC_PORT.load(Ordering::Relaxed)
}

// ─── Encode DNS name (e.g. "google.com" → "\x06google\x03com\x00") ──────
fn encode_name(name: &str, buf: &mut [u8]) -> usize {
    let mut pos = 0;
    for label in name.split('.') {
        let lb = label.as_bytes();
        if lb.is_empty() || lb.len() > 63 { continue; }
        if pos + 1 + lb.len() >= buf.len() { return 0; }
        buf[pos] = lb.len() as u8;
        pos += 1;
        buf[pos..pos + lb.len()].copy_from_slice(lb);
        pos += lb.len();
    }
    if pos >= buf.len() { return 0; }
    buf[pos] = 0; // terminator
    pos += 1;
    pos
}

// ─── Build DNS query ─────────────────────────────────────────────────────
fn build_query(name: &str, query_id: u16, buf: &mut [u8; 512]) -> usize {
    // Header (12 bytes)
    let id = query_id.to_be_bytes();
    buf[0] = id[0]; buf[1] = id[1];
    buf[2] = 0x01; buf[3] = 0x00;  // flags: RD=1 (recursion desired)
    buf[4] = 0x00; buf[5] = 0x01;  // QDCOUNT = 1
    buf[6] = 0; buf[7] = 0;        // ANCOUNT
    buf[8] = 0; buf[9] = 0;        // NSCOUNT
    buf[10] = 0; buf[11] = 0;      // ARCOUNT

    let mut pos = 12;

    // Question section: QNAME + QTYPE(A=1) + QCLASS(IN=1)
    let name_len = encode_name(name, &mut buf[pos..]);
    if name_len == 0 { return 0; }
    pos += name_len;

    // QTYPE = A (1)
    buf[pos] = 0; buf[pos+1] = 1;
    pos += 2;
    // QCLASS = IN (1)
    buf[pos] = 0; buf[pos+1] = 1;
    pos += 2;

    pos
}

// ─── Skip a DNS name (handles compression pointers) ──────────────────────
fn skip_name(data: &[u8], mut pos: usize) -> usize {
    loop {
        if pos >= data.len() { return data.len(); }
        let b = data[pos];
        if b == 0 { return pos + 1; }               // End of name
        if b & 0xC0 == 0xC0 { return pos + 2; }     // Compression pointer
        pos += 1 + b as usize;
    }
}

// ─── Parse DNS response — find first A record ───────────────────────────
fn parse_response(data: &[u8], expected_id: u16) -> Option<[u8; 4]> {
    if data.len() < 12 { return None; }

    let id = u16::from_be_bytes([data[0], data[1]]);
    if id != expected_id { return None; }

    let flags = u16::from_be_bytes([data[2], data[3]]);
    // Check QR=1 (response)
    if flags & 0x8000 == 0 { return None; }
    // Check RCODE = 0 (no error)
    if flags & 0x000F != 0 { return None; }

    let ancount = u16::from_be_bytes([data[6], data[7]]);
    let qdcount = u16::from_be_bytes([data[4], data[5]]);

    // Skip question section
    let mut pos = 12;
    for _ in 0..qdcount {
        pos = skip_name(data, pos);
        pos += 4; // QTYPE + QCLASS
        if pos > data.len() { return None; }
    }

    // Parse answer section — look for type A (1), class IN (1)
    for _ in 0..ancount {
        if pos >= data.len() { return None; }
        pos = skip_name(data, pos); // NAME
        if pos + 10 > data.len() { return None; }
        let rtype  = u16::from_be_bytes([data[pos], data[pos+1]]);
        let rclass = u16::from_be_bytes([data[pos+2], data[pos+3]]);
        // skip TTL (4 bytes)
        let rdlen  = u16::from_be_bytes([data[pos+8], data[pos+9]]) as usize;
        pos += 10;
        if pos + rdlen > data.len() { return None; }

        if rtype == 1 && rclass == 1 && rdlen == 4 {
            return Some([data[pos], data[pos+1], data[pos+2], data[pos+3]]);
        }
        pos += rdlen;
    }

    None
}

fn parse_nameserver_line(line: &[u8]) -> Option<[u8; 4]> {
    let mut i = 0usize;
    while i < line.len() && (line[i] == b' ' || line[i] == b'\t') { i += 1; }
    if i >= line.len() { return None; }
    if line[i] == b'#' { return None; }

    const KEY: &[u8] = b"nameserver";
    if i + KEY.len() > line.len() { return None; }
    if !line[i..i + KEY.len()].eq_ignore_ascii_case(KEY) { return None; }
    i += KEY.len();

    while i < line.len() && (line[i] == b' ' || line[i] == b'\t') { i += 1; }
    if i >= line.len() { return None; }

    let start = i;
    while i < line.len() && line[i] != b' ' && line[i] != b'\t' && line[i] != b'#' && line[i] != b'\r' {
        i += 1;
    }
    if i <= start { return None; }

    let ip_str = core::str::from_utf8(&line[start..i]).ok()?;
    crate::net::resolver::parse_ipv4(ip_str)
}

fn collect_dns_servers(out: &mut [[u8; 4]; 6]) -> usize {
    let mut count = 0usize;
    let mut push_unique = |ip: [u8; 4], out: &mut [[u8; 4]; 6], count: &mut usize| {
        if *count >= out.len() || ip == [0, 0, 0, 0] { return; }
        for i in 0..*count {
            if out[i] == ip { return; }
        }
        out[*count] = ip;
        *count += 1;
    };

    // 1) Linux-style resolver file
    if let Some(data) = crate::fs::read_file("/etc/resolv.conf")
        .or_else(|| crate::fs::read_file("/etc/resolve"))
    {
        for line in data.split(|&b| b == b'\n') {
            if let Some(ip) = parse_nameserver_line(line) {
                push_unique(ip, out, &mut count);
            }
        }
    }

    // 2) DHCP-provided DNS as fallback
    let dhcp_dns = unsafe { super::DNS_IP };
    push_unique(dhcp_dns, out, &mut count);

    // 3) Hard fallback defaults
    push_unique([1, 1, 1, 1], out, &mut count);
    push_unique([1, 0, 0, 1], out, &mut count);
    push_unique([8, 8, 8, 8], out, &mut count);
    push_unique([8, 8, 4, 4], out, &mut count);

    count
}

// ─── Public API ──────────────────────────────────────────────────────────

/// Resolve a hostname to an IPv4 address via DNS.
///
/// Steps:
///   1. Check local cache
///   2. Send UDP query to DNS_IP:53
///   3. Wait up to 3 seconds for response
///   4. Retry up to 2 times
///
/// Returns None if DNS is not configured or the name cannot be resolved.
pub fn resolve(name: &str) -> Option<[u8; 4]> {
    // Check cache first
    if let Some(ip) = cache_lookup(name) {
        return Some(ip);
    }

    let mut servers = [[0u8; 4]; 6];
    let server_count = collect_dns_servers(&mut servers);
    if server_count == 0 { return None; }

    let query_id = DNS_QUERY_ID.fetch_add(1, Ordering::Relaxed);

    // Ephemeral port: 49152 + (query_id mod 16384)
    let src_port = 49152 + (query_id % 16384);
    DNS_SRC_PORT.store(src_port, Ordering::Relaxed);

    let mut qbuf = [0u8; 512];
    let qlen = build_query(name, query_id, &mut qbuf);
    if qlen == 0 { return None; }

    for si in 0..server_count {
        let dns_ip = servers[si];
        for attempt in 0u8..2 {
            let timeout = 300u64 * (1 + attempt as u64); // 3s, 6s

            DNS_RX_READY.store(false, Ordering::Release);
            super::udp::send_udp(dns_ip, src_port, 53, &qbuf[..qlen]);

            // Wait for response
            let start = crate::arch::x86_64::idt::timer_ticks();
            loop {
                super::poll_rx();
                if DNS_RX_READY.load(Ordering::Acquire) {
                    DNS_RX_READY.store(false, Ordering::Release);
                    let len = unsafe { DNS_RX_LEN };
                    let data = unsafe { &DNS_RX_BUF[..len] };
                    if let Some(ip) = parse_response(data, query_id) {
                        cache_insert(name, ip);
                        DNS_SRC_PORT.store(0, Ordering::Relaxed);
                        return Some(ip);
                    }
                }
                let now = crate::arch::x86_64::idt::timer_ticks();
                if now.saturating_sub(start) >= timeout { break; }
                unsafe { core::arch::asm!("hlt"); }
            }
        }
    }

    DNS_SRC_PORT.store(0, Ordering::Relaxed);
    None
}
