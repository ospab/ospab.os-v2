/*
 * SNTP — Simple Network Time Protocol (RFC 4330)
 * Sends NTP request to a server and parses the response.
 * Uses UDP port 123.
 */

/// NTP timestamp epoch: Jan 1, 1900 → Unix epoch (Jan 1, 1970) = 2208988800 seconds
const NTP_EPOCH_OFFSET: u64 = 2_208_988_800;

/// Current synchronized time (Unix timestamp), 0 if not synced
static mut SYNCED_UNIX_TIME: u64 = 0;
static mut SYNCED_AT_TICK: u64 = 0;
static mut TIME_SYNCED: bool = false;

pub fn is_synced() -> bool {
    unsafe { TIME_SYNCED }
}

/// Get current Unix timestamp (adjusted for uptime since sync)
pub fn unix_time() -> u64 {
    unsafe {
        if !TIME_SYNCED { return 0; }
        let now = crate::arch::x86_64::idt::timer_ticks();
        let elapsed_secs = (now - SYNCED_AT_TICK) / 18;
        SYNCED_UNIX_TIME + elapsed_secs
    }
}

/// Perform NTP time synchronization
/// dst_ip: NTP server IP (e.g. 10.0.2.2 for QEMU SLIRP gateway, or pool.ntp.org via DNS)
/// Returns Some(unix_timestamp) on success
pub fn sync_time(server_ip: [u8; 4]) -> Option<u64> {
    // Build NTP request (48 bytes)
    let mut pkt = [0u8; 48];

    // LI=0, VN=4, Mode=3 (client) → 0b00_100_011 = 0x23
    pkt[0] = 0x23;
    // Stratum, Poll, Precision — leave as 0 for client request

    // Send via UDP, src port 12345, dst port 123 (NTP)
    super::udp::send_udp(server_ip, 12345, 123, &pkt);

    // Wait for response (up to ~3 seconds = ~54 ticks)
    let response = super::udp::wait_rx(54)?;

    if response.len() < 48 { return None; }

    // Transmit Timestamp is at bytes 40-43 (seconds), 44-47 (fraction)
    let ntp_seconds = u32::from_be_bytes([response[40], response[41], response[42], response[43]]) as u64;

    if ntp_seconds < NTP_EPOCH_OFFSET {
        return None; // Invalid timestamp
    }

    let unix_ts = ntp_seconds - NTP_EPOCH_OFFSET;

    unsafe {
        SYNCED_UNIX_TIME = unix_ts;
        SYNCED_AT_TICK = crate::arch::x86_64::idt::timer_ticks();
        TIME_SYNCED = true;
    }

    Some(unix_ts)
}

/// Format a Unix timestamp to "YYYY-MM-DD HH:MM:SS UTC"
/// Output to buffer, returns number of bytes written
pub fn format_datetime(unix_ts: u64, buf: &mut [u8; 32]) -> usize {
    // Simple date calculation (no leap second, basic leap year)
    let mut days = (unix_ts / 86400) as u32;
    let daytime = (unix_ts % 86400) as u32;
    let hours = daytime / 3600;
    let mins = (daytime % 3600) / 60;
    let secs = daytime % 60;

    // Year calculation from 1970
    let mut year = 1970u32;
    loop {
        let yday = if is_leap(year) { 366 } else { 365 };
        if days < yday { break; }
        days -= yday;
        year += 1;
    }

    // Month calculation
    let leap = is_leap(year);
    let mdays: [u32; 12] = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month = 0u32;
    for m in 0..12 {
        if days < mdays[m] { month = m as u32; break; }
        days -= mdays[m];
        if m == 11 { month = 11; }
    }
    let day = days + 1;

    // Format: "YYYY-MM-DD HH:MM:SS UTC"
    let mut pos = 0;

    // Year
    buf[pos] = b'0' + ((year / 1000) % 10) as u8; pos += 1;
    buf[pos] = b'0' + ((year / 100) % 10) as u8; pos += 1;
    buf[pos] = b'0' + ((year / 10) % 10) as u8; pos += 1;
    buf[pos] = b'0' + (year % 10) as u8; pos += 1;
    buf[pos] = b'-'; pos += 1;

    // Month (1-based)
    let m = month + 1;
    buf[pos] = b'0' + ((m / 10) % 10) as u8; pos += 1;
    buf[pos] = b'0' + (m % 10) as u8; pos += 1;
    buf[pos] = b'-'; pos += 1;

    // Day
    buf[pos] = b'0' + ((day / 10) % 10) as u8; pos += 1;
    buf[pos] = b'0' + (day % 10) as u8; pos += 1;
    buf[pos] = b' '; pos += 1;

    // Hours
    buf[pos] = b'0' + ((hours / 10) % 10) as u8; pos += 1;
    buf[pos] = b'0' + (hours % 10) as u8; pos += 1;
    buf[pos] = b':'; pos += 1;

    // Minutes
    buf[pos] = b'0' + ((mins / 10) % 10) as u8; pos += 1;
    buf[pos] = b'0' + (mins % 10) as u8; pos += 1;
    buf[pos] = b':'; pos += 1;

    // Seconds
    buf[pos] = b'0' + ((secs / 10) % 10) as u8; pos += 1;
    buf[pos] = b'0' + (secs % 10) as u8; pos += 1;
    buf[pos] = b' '; pos += 1;
    buf[pos] = b'U'; pos += 1;
    buf[pos] = b'T'; pos += 1;
    buf[pos] = b'C'; pos += 1;

    pos
}

fn is_leap(year: u32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}
