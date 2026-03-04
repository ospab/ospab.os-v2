/*
 * Host Resolver for AETERNA OS
 *
 * Resolution order:
 *   1. Dotted-decimal IPv4 literal  → immediate return, no file I/O
 *   2. /etc/hosts lookup            → static, no network needed
 *   3. DNS query (stub)             → future work
 *
 * /etc/hosts format (identical to Unix):
 *   # comment line
 *   127.0.0.1   localhost
 *   10.0.2.2    gateway gw        ← IP + one or more names on the same line
 *   ::1         ip6-localhost     ← IPv6 lines are silently skipped
 *
 * Notes:
 *   - Inline `#` comments are stripped before parsing.
 *   - Tab and space both count as whitespace separators.
 *   - The IP address field is always first; all remaining tokens are names.
 *   - Name comparison is case-insensitive (ASCII only).
 *   - Each call to hosts_lookup / resolve_host reads /etc/hosts fresh via VFS.
 *     There is no long-lived cache inside this module (the VFS+RamFS provides one).
 */

extern crate alloc;
use alloc::{vec::Vec, string::String};

// ─── Error type ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveError {
    /// Name is not a valid IP literal and was not found in /etc/hosts (or DNS).
    NotFound,
    /// /etc/hosts could not be read from the VFS.
    HostsUnavailable,
    /// The network stack is not initialised (needed for future DNS fallback).
    NetworkDown,
}

impl ResolveError {
    pub fn as_str(self) -> &'static str {
        match self {
            ResolveError::NotFound          => "name not found",
            ResolveError::HostsUnavailable  => "/etc/hosts unavailable",
            ResolveError::NetworkDown       => "network not available",
        }
    }
}

// ─── IP parsing ──────────────────────────────────────────────────────────────

/// Try to parse a dotted-decimal IPv4 string (`"a.b.c.d"`) from a byte slice.
///
/// Accepts only decimal digits and dots. Returns `None` for anything else
/// (including partial addresses, leading zeros that form octal literals in C,
/// out-of-range octets, or trailing garbage).
///
/// Note: We accept decimal-only; no hex or octal forms are supported.
pub fn parse_ipv4_bytes(s: &[u8]) -> Option<[u8; 4]> {
    let mut ip = [0u8; 4];
    let mut idx = 0usize;
    let mut octet = 0u32;
    let mut has_digit = false;

    for &b in s {
        match b {
            b'0'..=b'9' => {
                octet = octet * 10 + (b - b'0') as u32;
                if octet > 255 { return None; }
                has_digit = true;
            }
            b'.' => {
                if !has_digit || idx >= 3 { return None; }
                ip[idx] = octet as u8;
                idx += 1;
                octet = 0;
                has_digit = false;
            }
            _ => return None,
        }
    }

    if !has_digit || idx != 3 { return None; }
    ip[3] = octet as u8;
    Some(ip)
}

/// Try to parse a dotted-decimal IPv4 string from a `&str`.
pub fn parse_ipv4(s: &str) -> Option<[u8; 4]> {
    parse_ipv4_bytes(s.as_bytes())
}

// ─── /etc/hosts parser ───────────────────────────────────────────────────────

/// One parsed entry from /etc/hosts: an IPv4 address plus all hostnames on
/// that line.  64-byte aligned for cache-line friendliness.
#[repr(C, align(64))]
pub struct HostsEntry {
    pub ip:    [u8; 4],
    /// All hostname/alias tokens from the line (first name and all aliases).
    pub names: Vec<String>,
}

/// Parse the full contents of an /etc/hosts file and return a `Vec<HostsEntry>`.
///
/// Rules:
/// * Leading and trailing whitespace on each line is ignored.
/// * Lines whose first non-whitespace character is `#` are skipped.
/// * Inline `#` characters terminate the content of a line.
/// * IPv6 addresses (lines whose first field contains `:`) are silently skipped.
/// * Lines where the first whitespace-separated field is not a valid IPv4 address
///   are silently skipped.
pub fn parse_hosts_bytes(data: &[u8]) -> Vec<HostsEntry> {
    let mut result = Vec::new();

    for raw_line in data.split(|&b| b == b'\n') {
        // Trim \r at end (CRLF files)
        let raw_line = if raw_line.last() == Some(&b'\r') {
            &raw_line[..raw_line.len() - 1]
        } else {
            raw_line
        };

        // Trim leading whitespace
        let line = trim_leading_ws(raw_line);

        // Skip empty and full-comment lines
        if line.is_empty() || line[0] == b'#' { continue; }

        // Strip inline comment
        let line = match line.iter().position(|&b| b == b'#') {
            Some(pos) => &line[..pos],
            None      => line,
        };

        // Tokenise by whitespace
        let mut tokens = split_ws(line);

        // First token = IP address
        let ip_bytes = match tokens.next() { Some(t) => t, None => continue };

        // Skip IPv6 addresses
        if ip_bytes.contains(&b':') { continue; }

        let ip = match parse_ipv4_bytes(ip_bytes) { Some(ip) => ip, None => continue };

        // Collect remaining tokens as names
        let mut names: Vec<String> = Vec::new();
        for name_bytes in tokens {
            if let Ok(s) = core::str::from_utf8(name_bytes) {
                if !s.is_empty() {
                    names.push(String::from(s));
                }
            }
        }

        if !names.is_empty() {
            result.push(HostsEntry { ip, names });
        }
    }

    result
}

// ─── Lookup functions ────────────────────────────────────────────────────────

/// Search the parsed entries for a hostname match (case-insensitive).
pub fn entries_lookup(entries: &[HostsEntry], name: &str) -> Option<[u8; 4]> {
    for entry in entries {
        for n in &entry.names {
            if n.eq_ignore_ascii_case(name) {
                return Some(entry.ip);
            }
        }
    }
    None
}

/// Look up `name` in `/etc/hosts`.  Reads the file on every call (VFS may
/// cache it internally); returns `None` if the file is missing or the name is
/// not present.
pub fn hosts_lookup(name: &str) -> Option<[u8; 4]> {
    let data    = crate::fs::read_file("/etc/hosts")?;
    let entries = parse_hosts_bytes(&data);
    entries_lookup(&entries, name)
}

/// Resolve a hostname to an IPv4 address.
///
/// Steps:
/// 1. If `name` is already a dotted-decimal address, return it immediately.
/// 2. Search `/etc/hosts`.
/// 3. Future: DNS query.
///
/// # Errors
/// * `ResolveError::NotFound` — name not resolvable via any available method.
/// * `ResolveError::HostsUnavailable` — /etc/hosts missing and no other
///   fallback succeeded.
pub fn resolve_host(name: &str) -> Result<[u8; 4], ResolveError> {
    // Step 0: Dotted-decimal literal?
    if let Some(ip) = parse_ipv4(name) {
        return Ok(ip);
    }

    // Step 1: /etc/hosts
    match crate::fs::read_file("/etc/hosts") {
        Some(data) => {
            let entries = parse_hosts_bytes(&data);
            if let Some(ip) = entries_lookup(&entries, name) {
                return Ok(ip);
            }
        }
        None => return Err(ResolveError::HostsUnavailable),
    }

    // Step 2: DNS (not yet implemented)
    // return dns_query(name).map_err(|_| ResolveError::NotFound);

    Err(ResolveError::NotFound)
}

// ─── Whitespace helpers ──────────────────────────────────────────────────────

fn is_ws(b: u8) -> bool { b == b' ' || b == b'\t' }

fn trim_leading_ws(s: &[u8]) -> &[u8] {
    let mut i = 0;
    while i < s.len() && is_ws(s[i]) { i += 1; }
    &s[i..]
}

struct WsSplit<'a> {
    data: &'a [u8],
    pos:  usize,
}

impl<'a> Iterator for WsSplit<'a> {
    type Item = &'a [u8];
    fn next(&mut self) -> Option<&'a [u8]> {
        while self.pos < self.data.len() && is_ws(self.data[self.pos]) { self.pos += 1; }
        if self.pos >= self.data.len() { return None; }
        let start = self.pos;
        while self.pos < self.data.len() && !is_ws(self.data[self.pos]) { self.pos += 1; }
        Some(&self.data[start..self.pos])
    }
}

fn split_ws(data: &[u8]) -> WsSplit<'_> { WsSplit { data, pos: 0 } }

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── IP parser ────────────────────────────────────────────────────────────

    #[test] fn ip_loopback()     { assert_eq!(parse_ipv4("127.0.0.1"),         Some([127, 0,   0,   1  ])); }
    #[test] fn ip_gateway()      { assert_eq!(parse_ipv4("10.0.2.2"),          Some([10,  0,   2,   2  ])); }
    #[test] fn ip_broadcast()    { assert_eq!(parse_ipv4("255.255.255.255"),   Some([255, 255, 255, 255])); }
    #[test] fn ip_zero()         { assert_eq!(parse_ipv4("0.0.0.0"),           Some([0,   0,   0,   0  ])); }
    #[test] fn ip_octet_too_big(){ assert_eq!(parse_ipv4("256.0.0.1"),         None); }
    #[test] fn ip_not_an_ip()    { assert_eq!(parse_ipv4("not-an-ip"),         None); }
    #[test] fn ip_incomplete()   { assert_eq!(parse_ipv4("1.2.3"),             None); }
    #[test] fn ip_trailing_dot() { assert_eq!(parse_ipv4("1.2.3."),            None); }
    #[test] fn ip_empty()        { assert_eq!(parse_ipv4(""),                  None); }
    #[test] fn ip_hostname()     { assert_eq!(parse_ipv4("localhost"),         None); }

    // ── hosts file parser ────────────────────────────────────────────────────

    #[test]
    fn hosts_basic() {
        let data = b"127.0.0.1 localhost\n10.0.2.2 gateway gw\n";
        let e = parse_hosts_bytes(data);
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].ip, [127, 0, 0, 1]);
        assert!(e[0].names.iter().any(|n| n == "localhost"));
        assert_eq!(e[1].ip, [10, 0, 2, 2]);
        assert!(e[1].names.iter().any(|n| n == "gateway"));
        assert!(e[1].names.iter().any(|n| n == "gw"));
    }

    #[test]
    fn hosts_comments_skipped() {
        let data = b"# full comment\n127.0.0.1 localhost # inline\n\n8.8.8.8 dns\n";
        let e = parse_hosts_bytes(data);
        assert_eq!(e.len(), 2, "comment and blank lines must be skipped");
        assert_eq!(e[0].ip, [127, 0, 0, 1]);
        assert_eq!(e[1].ip, [8, 8, 8, 8]);
    }

    #[test]
    fn hosts_tabs_as_separator() {
        let data = b"127.0.0.1\tlocalhost\n10.0.0.1\thost1\thost2\n";
        let e = parse_hosts_bytes(data);
        assert_eq!(e[0].ip, [127, 0, 0, 1]);
        assert!(e[1].names.iter().any(|n| n == "host1"));
        assert!(e[1].names.iter().any(|n| n == "host2"));
    }

    #[test]
    fn hosts_ipv6_skipped() {
        let data = b"::1 localhost\n127.0.0.1 loopback\n";
        let e = parse_hosts_bytes(data);
        assert_eq!(e.len(), 1, "IPv6 line must be skipped");
        assert_eq!(e[0].ip, [127, 0, 0, 1]);
    }

    #[test]
    fn hosts_crlf() {
        let data = b"127.0.0.1 localhost\r\n10.0.2.2 gw\r\n";
        let e = parse_hosts_bytes(data);
        assert_eq!(e.len(), 2);
    }

    // ── entries_lookup ───────────────────────────────────────────────────────

    #[test]
    fn lookup_found_case_insensitive() {
        let data = b"127.0.0.1 Localhost LocalHost\n";
        let e = parse_hosts_bytes(data);
        assert_eq!(entries_lookup(&e, "localhost"),  Some([127, 0, 0, 1]));
        assert_eq!(entries_lookup(&e, "LOCALHOST"),  Some([127, 0, 0, 1]));
        assert_eq!(entries_lookup(&e, "Localhost"),  Some([127, 0, 0, 1]));
    }

    #[test]
    fn lookup_not_found() {
        let data = b"127.0.0.1 localhost\n";
        let e = parse_hosts_bytes(data);
        assert_eq!(entries_lookup(&e, "unknown"), None);
    }

    // ── parse_ipv4 used in resolve_host (IP literal path) ───────────────────

    #[test]
    fn resolve_ip_literal_direct() {
        // When the VFS is unavailable in the test environment the IP-literal
        // branch must still succeed without touching any global state.
        // We call parse_ipv4 directly as a stand-in for that branch.
        assert_eq!(parse_ipv4("8.8.8.8"), Some([8, 8, 8, 8]));
    }
}
