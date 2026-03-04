/*
 * SNTP — Simple Network Time Protocol (RFC 4330)
 *
 * Protocol:
 *   - Client sends 48-byte request with byte[0] = 0x23 (LI=0, VN=4, Mode=3=client)
 *   - Server replies with:
 *       byte[0]    : LI/VN/Mode  — Mode must be 4 (server) or 5 (broadcast)
 *       byte[1]    : Stratum     — 0 = KoD (reject), 1..15 = valid
 *       bytes[40..43]: Transmit Timestamp seconds (u32 big-endian, NTP epoch 1900)
 *       bytes[44..47]: Transmit Timestamp fractions (u32 big-endian, 1/2^32 sec)
 *   - Unix time = NTP seconds − 2_208_988_800
 *
 * Timezone:
 *   /etc/timezone holds: "UTC"  "UTC+3"  "UTC-5"  "UTC+5:30"
 *   Only UTC±H[:MM] offsets are supported.
 *
 * API:
 *   sync_system_time()          — try NTP_FALLBACKS list, update global state
 *   sync_time([ip;4])           — sync against specific server
 *   unix_time()                 — current Unix timestamp (0 = never synced)
 *   local_time()                — unix_time() + timezone offset
 *   format_datetime(ts, buf)    — "YYYY-MM-DD HH:MM:SS UTC"
 *   format_datetime_with_tz(…)  — includes timezone suffix
 */

/// NTP epoch offset: seconds between 1900-01-01 and 1970-01-01.
const NTP_EPOCH_OFFSET: u64 = 2_208_988_800;

// ─── Global state ────────────────────────────────────────────────────────────

static mut SYNCED_UNIX_TIME: u64  = 0;
static mut SYNCED_AT_TICK:   u64  = 0;
static mut TIME_SYNCED:      bool = false;
/// UTC offset in seconds for display.  Populated lazily from /etc/timezone.
static mut TZ_OFFSET_SECS:   i64  = 0;
static mut TZ_LOADED:        bool = false;

// ─── Error type ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SntpError {
    NetworkDown,
    SendFailed,
    /// No reply before timeout.
    Timeout,
    /// Packet too short, or Mode/Stratum invalid.
    InvalidResponse,
    /// Encoded timestamp is before the Unix epoch (1970).
    InvalidTimestamp,
}

impl SntpError {
    pub fn as_str(self) -> &'static str {
        match self {
            SntpError::NetworkDown      => "network not available",
            SntpError::SendFailed       => "UDP send failed",
            SntpError::Timeout          => "server timed out",
            SntpError::InvalidResponse  => "invalid NTP response",
            SntpError::InvalidTimestamp => "timestamp out of valid range",
        }
    }
}

// ─── Public accessors ────────────────────────────────────────────────────────

pub fn is_synced() -> bool { unsafe { TIME_SYNCED } }

/// Current estimated Unix timestamp; advances by elapsed ticks since last sync.
/// Returns 0 if `sync_time` / `sync_system_time` has never succeeded.
pub fn unix_time() -> u64 {
    unsafe {
        if !TIME_SYNCED { return 0; }
        let elapsed = crate::arch::x86_64::idt::timer_ticks()
            .wrapping_sub(SYNCED_AT_TICK) / 100;      // 100 Hz → seconds
        SYNCED_UNIX_TIME.saturating_add(elapsed)
    }
}

/// `unix_time()` with the timezone offset from /etc/timezone applied.
pub fn local_time() -> i64 {
    (unix_time() as i64).saturating_add(read_timezone_offset())
}

// ─── Timezone ────────────────────────────────────────────────────────────────

/// Read, cache, and return the UTC offset in seconds from `/etc/timezone`.
pub fn read_timezone_offset() -> i64 {
    unsafe {
        if TZ_LOADED { return TZ_OFFSET_SECS; }
    }
    let off = match crate::fs::read_file("/etc/timezone") {
        Some(d) => parse_timezone_offset(&d),
        None    => 0,
    };
    unsafe { TZ_OFFSET_SECS = off; TZ_LOADED = true; }
    off
}

/// Force a re-read of /etc/timezone on the next `read_timezone_offset()` call.
pub fn invalidate_tz_cache() { unsafe { TZ_LOADED = false; } }

/// Parse a timezone string and return the UTC offset in **seconds**.
///
/// Recognised formats: `"UTC"`, `"UTC+3"`, `"UTC-5"`, `"UTC+5:30"`.
/// Anything else (e.g. IANA zone names) returns `0`.
pub fn parse_timezone_offset(data: &[u8]) -> i64 {
    // Trim trailing whitespace / newlines.
    let mut end = data.len();
    while end > 0 && matches!(data[end - 1], b'\n' | b'\r' | b' ' | b'\t') { end -= 1; }
    let s = &data[..end];

    if s == b"UTC" || s.is_empty() { return 0; }

    if s.len() < 4 || &s[..3] != b"UTC" { return 0; }
    let sign: i64 = match s[3] { b'+' => 1, b'-' => -1, _ => return 0 };

    let rest = &s[4..];
    let (hp, mp) = match rest.iter().position(|&b| b == b':') {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None    => (rest, &b""[..]),
    };
    let h = dec_bytes(hp) as i64;
    let m = if mp.is_empty() { 0i64 } else { dec_bytes(mp) as i64 };
    if h > 14 || m > 59 { return 0; }
    sign * (h * 3600 + m * 60)
}

fn dec_bytes(b: &[u8]) -> u32 {
    let mut v = 0u32;
    for &c in b { if c.is_ascii_digit() { v = v * 10 + (c - b'0') as u32; } }
    v
}

// ─── NTP packet parsing — pure, no I/O (testable) ───────────────────────────

/// Parse an NTP/SNTP server response (`>= 48` bytes) and return a Unix timestamp.
///
/// Validates:
/// * Minimum length (48 bytes)
/// * Mode field == 4 (server) or 5 (broadcast)
/// * Stratum != 0 (stratum 0 = Kiss-o'-Death)
/// * Transmit Timestamp > NTP epoch offset (sanity check)
pub fn parse_ntp_response(response: &[u8]) -> Result<u64, SntpError> {
    if response.len() < 48 { return Err(SntpError::InvalidResponse); }

    let mode    = response[0] & 0x07;
    let stratum = response[1];

    if mode != 4 && mode != 5    { return Err(SntpError::InvalidResponse); }
    if stratum == 0               { return Err(SntpError::InvalidResponse); }

    let ntp_sec = u32::from_be_bytes([
        response[40], response[41], response[42], response[43],
    ]) as u64;

    if ntp_sec < NTP_EPOCH_OFFSET { return Err(SntpError::InvalidTimestamp); }

    Ok(ntp_sec - NTP_EPOCH_OFFSET)
}

// ─── Server list ─────────────────────────────────────────────────────────────

/// Static NTP server addresses tried in order by `sync_system_time`.
pub const NTP_FALLBACKS: &[[u8; 4]] = &[
    [10,  0,   2,   2],  // QEMU SLIRP gateway (typically responds to NTP)
    [216, 239, 35,  0],  // time1.google.com
    [216, 239, 35,  4],  // time4.google.com
    [162, 159, 200, 1],  // time.cloudflare.com
    [129, 6,   15,  28], // time-a-wwv.nist.gov
];

// ─── Sync functions ──────────────────────────────────────────────────────────

/// Send an SNTP request to `server_ip:123` and update the global clock on success.
/// Blocks up to ~3 s (300 ticks at 100 Hz).
pub fn sync_time(server_ip: [u8; 4]) -> Result<u64, SntpError> {
    // 64-byte aligned NTP packet (friendly to cache lines and DMA descriptors).
    #[repr(C, align(64))]
    struct NtpPkt([u8; 48]);
    let mut pkt = NtpPkt([0u8; 48]);
    pkt.0[0] = 0x23; // LI=0, VN=4, Mode=3 (client)   → 0b00_100_011

    super::udp::send_udp(server_ip, 12345, 123, &pkt.0);

    let resp = super::udp::wait_rx(300).ok_or(SntpError::Timeout)?;
    let ts   = parse_ntp_response(resp)?;

    unsafe {
        SYNCED_UNIX_TIME = ts;
        SYNCED_AT_TICK   = crate::arch::x86_64::idt::timer_ticks();
        TIME_SYNCED      = true;
        TZ_LOADED        = false; // re-read timezone on next query
    }
    let _ = read_timezone_offset();
    Ok(ts)
}

/// Try each server in `NTP_FALLBACKS` in order.
/// Returns the first successful Unix timestamp, or the last `SntpError`.
pub fn sync_system_time() -> Result<u64, SntpError> {
    let mut last = SntpError::Timeout;
    for &ip in NTP_FALLBACKS {
        match sync_time(ip) {
            Ok(ts) => return Ok(ts),
            Err(e) => {
                last = e;
                // Inter-server pause: ~50 ms
                let t0 = crate::arch::x86_64::idt::timer_ticks();
                while crate::arch::x86_64::idt::timer_ticks().wrapping_sub(t0) < 5 {
                    unsafe { core::arch::asm!("pause"); }
                }
            }
        }
    }
    Err(last)
}

// ─── Formatting ──────────────────────────────────────────────────────────────

/// Format a Unix timestamp as `"YYYY-MM-DD HH:MM:SS UTC"`.
/// Writes into `buf` (32 bytes), returns bytes written.
pub fn format_datetime(unix_ts: u64, buf: &mut [u8; 32]) -> usize {
    format_datetime_with_tz(unix_ts, 0, buf)
}

/// Format a Unix timestamp with a UTC offset applied for display.
/// Output example: `"2026-03-04 15:43:01 UTC+3"`
pub fn format_datetime_with_tz(unix_ts: u64, tz_secs: i64, buf: &mut [u8; 32]) -> usize {
    let adj  = (unix_ts as i64).saturating_add(tz_secs).max(0) as u64;
    let mut d = (adj / 86_400) as u32;
    let t     = (adj % 86_400) as u32;
    let (h, m, s) = (t / 3600, (t % 3600) / 60, t % 60);

    let mut yr = 1970u32;
    loop {
        let yd = if is_leap(yr) { 366 } else { 365 };
        if d < yd { break; }
        d -= yd;
        yr += 1;
    }
    let mdays: [u32; 12] = [
        31, if is_leap(yr) { 29 } else { 28 }, 31, 30, 31, 30,
        31, 31, 30, 31, 30, 31,
    ];
    let mut mo = 11u32;
    for i in 0..12usize {
        if d < mdays[i] { mo = i as u32; break; }
        d -= mdays[i];
    }
    let day = d + 1;

    let mut p = 0usize;

    // YYYY-MM-DD HH:MM:SS
    for div in [1000u32, 100, 10, 1] { wc(buf, &mut p, b'0' + ((yr / div) % 10) as u8); }
    wc(buf, &mut p, b'-'); w2(buf, &mut p, mo + 1);
    wc(buf, &mut p, b'-'); w2(buf, &mut p, day);
    wc(buf, &mut p, b' '); w2(buf, &mut p, h);
    wc(buf, &mut p, b':'); w2(buf, &mut p, m);
    wc(buf, &mut p, b':'); w2(buf, &mut p, s);
    wc(buf, &mut p, b' ');

    // "UTC" [+/-H[:MM]]
    wc(buf, &mut p, b'U'); wc(buf, &mut p, b'T'); wc(buf, &mut p, b'C');
    if tz_secs != 0 {
        let (sgn, abs) = if tz_secs > 0 { (b'+', tz_secs as u32) } else { (b'-', (-tz_secs) as u32) };
        wc(buf, &mut p, sgn);
        let (hh, mm) = (abs / 3600, (abs % 3600) / 60);
        if hh >= 10 { wc(buf, &mut p, b'0' + (hh / 10) as u8); }
        wc(buf, &mut p, b'0' + (hh % 10) as u8);
        if mm > 0 { wc(buf, &mut p, b':'); w2(buf, &mut p, mm); }
    }
    p
}

#[inline(always)] fn wc(b: &mut [u8; 32], p: &mut usize, c: u8) { b[*p] = c; *p += 1; }
#[inline(always)] fn w2(b: &mut [u8; 32], p: &mut usize, v: u32) {
    wc(b, p, b'0' + ((v / 10) % 10) as u8);
    wc(b, p, b'0' + (v % 10) as u8);
}
fn is_leap(y: u32) -> bool { (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 }

// ─── Unit tests (pure, no I/O — run on host) ─────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn server_pkt(ntp_sec: u32) -> [u8; 48] {
        let mut p = [0u8; 48];
        p[0] = 0x24; p[1] = 1; // Mode=4 (server), Stratum=1
        let ts = ntp_sec.to_be_bytes();
        p[40] = ts[0]; p[41] = ts[1]; p[42] = ts[2]; p[43] = ts[3];
        p
    }

    // ── parse_ntp_response ──────────────────────────────────────────────────

    #[test] fn ntp_ok_1000s() {
        assert_eq!(parse_ntp_response(&server_pkt(2_208_989_800)), Ok(1000u64));
    }
    #[test] fn ntp_exact_epoch() {
        assert_eq!(parse_ntp_response(&server_pkt(NTP_EPOCH_OFFSET as u32)), Ok(0u64));
    }
    #[test] fn ntp_short_packet() {
        assert_eq!(parse_ntp_response(&[0u8; 10]), Err(SntpError::InvalidResponse));
    }
    #[test] fn ntp_client_mode_rejected() {
        let mut p = server_pkt(2_208_989_800); p[0] = 0x23; // client mode
        assert_eq!(parse_ntp_response(&p), Err(SntpError::InvalidResponse));
    }
    #[test] fn ntp_zero_stratum_rejected() {
        let mut p = server_pkt(2_208_989_800); p[1] = 0;
        assert_eq!(parse_ntp_response(&p), Err(SntpError::InvalidResponse));
    }
    #[test] fn ntp_pre_epoch_rejected() {
        assert_eq!(parse_ntp_response(&server_pkt(100)), Err(SntpError::InvalidTimestamp));
    }

    // ── parse_timezone_offset ───────────────────────────────────────────────

    #[test] fn tz_utc()       { assert_eq!(parse_timezone_offset(b"UTC"),      0); }
    #[test] fn tz_plus3()     { assert_eq!(parse_timezone_offset(b"UTC+3"),    3*3600); }
    #[test] fn tz_minus5()    { assert_eq!(parse_timezone_offset(b"UTC-5"),   -5*3600); }
    #[test] fn tz_plus5_30()  { assert_eq!(parse_timezone_offset(b"UTC+5:30"), 5*3600+30*60); }
    #[test] fn tz_minus3_30() { assert_eq!(parse_timezone_offset(b"UTC-3:30"), -(3*3600+30*60)); }
    #[test] fn tz_newline()   { assert_eq!(parse_timezone_offset(b"UTC+3\n"), 3*3600); }
    #[test] fn tz_iana_name() { assert_eq!(parse_timezone_offset(b"America/New_York"), 0); }

    // ── format_datetime ─────────────────────────────────────────────────────

    #[test] fn fmt_epoch() {
        let mut b = [0u8; 32];
        let n = format_datetime(0, &mut b);
        assert_eq!(core::str::from_utf8(&b[..n]).unwrap(), "1970-01-01 00:00:00 UTC");
    }
    #[test] fn fmt_tz_plus3() {
        let mut b = [0u8; 32];
        let n = format_datetime_with_tz(0, 3*3600, &mut b);
        assert_eq!(core::str::from_utf8(&b[..n]).unwrap(), "1970-01-01 03:00:00 UTC+3");
    }
    #[test] fn fmt_tz_india() {
        let mut b = [0u8; 32];
        // UTC+5:30 applied to epoch → 1970-01-01 05:30:00 UTC+5:30
        let n = format_datetime_with_tz(0, 5*3600+30*60, &mut b);
        assert_eq!(core::str::from_utf8(&b[..n]).unwrap(), "1970-01-01 05:30:00 UTC+5:30");
    }
}
