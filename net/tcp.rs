/*
 * TCP — Transmission Control Protocol (RFC 793)
 *
 * Minimal but real implementation:
 *   - 3-way handshake (SYN → SYN-ACK → ACK)
 *   - Data transfer with sequence/acknowledgment tracking
 *   - Connection teardown (FIN → FIN-ACK → ACK)
 *   - Retransmit timer (simple fixed timeout)
 *   - Receive window
 *   - RST handling
 *
 * Connection table: up to MAX_CONNS simultaneous connections.
 * Each connection has a small rx ring buffer and tx state.
 *
 * API:
 *   tcp_connect(dst_ip, dst_port) -> Option<usize>     (returns conn id)
 *   tcp_listen(port) -> Option<usize>                   (passive open)
 *   tcp_accept(listener_id) -> Option<usize>            (wait for SYN)
 *   tcp_send(conn, data) -> Result<usize, TcpError>
 *   tcp_recv(conn, buf) -> Result<usize, TcpError>
 *   tcp_close(conn)
 *   handle_tcp(payload, src_ip)                          (called from IPv4)
 */

extern crate alloc;

use core::sync::atomic::{AtomicU32, Ordering};

// ─── TCP header constants ─────────────────────────────────────────────────
const TCP_FIN: u8 = 0x01;
const TCP_SYN: u8 = 0x02;
const TCP_RST: u8 = 0x04;
const TCP_PSH: u8 = 0x08;
const TCP_ACK: u8 = 0x10;

// ─── Connection limits ────────────────────────────────────────────────────
const MAX_CONNS: usize = 16;
const RX_BUF_SIZE: usize = 8192;
const TX_BUF_SIZE: usize = 4096;
const RETRANSMIT_TICKS: u64 = 300; // 3 seconds at 100Hz PIT
const CONNECT_TIMEOUT_TICKS: u64 = 500; // 5 seconds
const DEFAULT_WINDOW: u16 = 8192;

// ─── Connection state (RFC 793) ───────────────────────────────────────────
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcpState {
    Closed,
    Listen,
    SynSent,
    SynReceived,
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    LastAck,
    TimeWait,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcpError {
    ConnectionRefused,
    ConnectionReset,
    TimedOut,
    NotConnected,
    InvalidConn,
    NoFreeSlots,
    WouldBlock,
}

impl TcpError {
    pub fn as_str(&self) -> &'static str {
        match self {
            TcpError::ConnectionRefused => "Connection refused",
            TcpError::ConnectionReset   => "Connection reset by peer",
            TcpError::TimedOut          => "Connection timed out",
            TcpError::NotConnected      => "Not connected",
            TcpError::InvalidConn       => "Invalid connection",
            TcpError::NoFreeSlots       => "No free connection slots",
            TcpError::WouldBlock        => "Would block",
        }
    }
}

// ─── Ring buffer for received data ────────────────────────────────────────
struct RxRing {
    buf: [u8; RX_BUF_SIZE],
    head: usize, // write position
    tail: usize, // read position
    len:  usize,
}

impl RxRing {
    const fn new() -> Self {
        RxRing { buf: [0; RX_BUF_SIZE], head: 0, tail: 0, len: 0 }
    }

    fn write(&mut self, data: &[u8]) -> usize {
        let avail = RX_BUF_SIZE - self.len;
        let n = data.len().min(avail);
        for i in 0..n {
            self.buf[self.head] = data[i];
            self.head = (self.head + 1) % RX_BUF_SIZE;
        }
        self.len += n;
        n
    }

    fn read(&mut self, out: &mut [u8]) -> usize {
        let n = out.len().min(self.len);
        for i in 0..n {
            out[i] = self.buf[self.tail];
            self.tail = (self.tail + 1) % RX_BUF_SIZE;
        }
        self.len -= n;
        n
    }

    fn available(&self) -> usize { self.len }
    fn free_space(&self) -> usize { RX_BUF_SIZE - self.len }
}

// ─── TCP Connection Block ─────────────────────────────────────────────────
struct TcpConn {
    state:       TcpState,
    local_port:  u16,
    remote_port: u16,
    remote_ip:   [u8; 4],
    // Sequence numbers
    snd_una:     u32, // oldest unacknowledged
    snd_nxt:     u32, // next seq to send
    snd_iss:     u32, // initial send sequence
    rcv_nxt:     u32, // next expected receive seq
    rcv_irs:     u32, // initial receive sequence
    // Receive window
    rcv_wnd:     u16,
    snd_wnd:     u16, // peer's receive window
    // Buffers
    rx_ring:     RxRing,
    // Retransmit state
    last_tx_tick: u64,
    retransmit_data: [u8; TX_BUF_SIZE],
    retransmit_len:  usize,
    // Listener: accepted connection slot (for Listen state)
    accept_ready: bool,
    accept_ip:    [u8; 4],
    accept_port:  u16,
    accept_irs:   u32,
}

impl TcpConn {
    const fn new() -> Self {
        TcpConn {
            state: TcpState::Closed,
            local_port: 0,
            remote_port: 0,
            remote_ip: [0; 4],
            snd_una: 0,
            snd_nxt: 0,
            snd_iss: 0,
            rcv_nxt: 0,
            rcv_irs: 0,
            rcv_wnd: DEFAULT_WINDOW,
            snd_wnd: DEFAULT_WINDOW,
            rx_ring: RxRing::new(),
            last_tx_tick: 0,
            retransmit_data: [0; TX_BUF_SIZE],
            retransmit_len: 0,
            accept_ready: false,
            accept_ip: [0; 4],
            accept_port: 0,
            accept_irs: 0,
        }
    }

    fn reset(&mut self) {
        *self = TcpConn::new();
    }
}

// ─── Global connection table ──────────────────────────────────────────────
// Single-core kernel: no lock needed, just unsafe static.
static mut CONNS: [TcpConn; MAX_CONNS] = {
    const INIT: TcpConn = TcpConn::new();
    [INIT; MAX_CONNS]
};

// Monotonic ISS (Initial Send Sequence) counter
static ISS_COUNTER: AtomicU32 = AtomicU32::new(0x10000);

// Ephemeral port counter
static EPHEMERAL_PORT: AtomicU32 = AtomicU32::new(49152);

fn next_iss() -> u32 {
    // Mix tick count into ISS for weak randomization
    let tick = crate::arch::x86_64::idt::timer_ticks() as u32;
    ISS_COUNTER.fetch_add(64000u32.wrapping_add(tick), Ordering::Relaxed)
}

fn next_ephemeral_port() -> u16 {
    let p = EPHEMERAL_PORT.fetch_add(1, Ordering::Relaxed);
    // Wrap within ephemeral range 49152-65535
    (49152 + (p % 16384)) as u16
}

fn now_ticks() -> u64 {
    crate::arch::x86_64::idt::timer_ticks()
}

fn alloc_conn() -> Option<usize> {
    unsafe {
        for i in 0..MAX_CONNS {
            if CONNS[i].state == TcpState::Closed {
                return Some(i);
            }
        }
    }
    None
}

fn find_conn(local_port: u16, remote_ip: [u8; 4], remote_port: u16) -> Option<usize> {
    unsafe {
        for i in 0..MAX_CONNS {
            let c = &CONNS[i];
            if c.state != TcpState::Closed
                && c.local_port == local_port
                && c.remote_ip == remote_ip
                && c.remote_port == remote_port
            {
                return Some(i);
            }
        }
    }
    None
}

fn find_listener(local_port: u16) -> Option<usize> {
    unsafe {
        for i in 0..MAX_CONNS {
            if CONNS[i].state == TcpState::Listen && CONNS[i].local_port == local_port {
                return Some(i);
            }
        }
    }
    None
}

// ─── TCP segment builder ──────────────────────────────────────────────────

/// Build and send a TCP segment.
/// flags: combination of TCP_SYN, TCP_ACK, TCP_FIN, TCP_RST, TCP_PSH
fn send_tcp_segment(
    dst_ip:     [u8; 4],
    src_port:   u16,
    dst_port:   u16,
    seq:        u32,
    ack:        u32,
    flags:      u8,
    window:     u16,
    payload:    &[u8],
) {
    let data_offset: u8 = 5; // 20-byte header, no options
    let header_len = (data_offset as usize) * 4;
    let total_len = header_len + payload.len();
    if total_len > 1460 { return; } // MSS guard

    let mut seg = [0u8; 1480];

    // Source port
    seg[0..2].copy_from_slice(&src_port.to_be_bytes());
    // Destination port
    seg[2..4].copy_from_slice(&dst_port.to_be_bytes());
    // Sequence number
    seg[4..8].copy_from_slice(&seq.to_be_bytes());
    // Acknowledgment number
    seg[8..12].copy_from_slice(&ack.to_be_bytes());
    // Data offset (4 bits) + reserved (4 bits)
    seg[12] = data_offset << 4;
    // Flags
    seg[13] = flags;
    // Window
    seg[14..16].copy_from_slice(&window.to_be_bytes());
    // Checksum (0 for now)
    seg[16] = 0;
    seg[17] = 0;
    // Urgent pointer
    seg[18] = 0;
    seg[19] = 0;

    // Payload
    if !payload.is_empty() {
        seg[header_len..header_len + payload.len()].copy_from_slice(payload);
    }

    // Compute TCP checksum (with pseudo-header)
    let src_ip = unsafe { super::OUR_IP };
    let cksum = tcp_checksum(&src_ip, &dst_ip, &seg[..total_len]);
    seg[16] = (cksum >> 8) as u8;
    seg[17] = (cksum & 0xFF) as u8;

    // Send via IPv4 (protocol 6 = TCP)
    super::ipv4::send_ipv4(6, dst_ip, &seg[..total_len]);
}

/// TCP checksum with pseudo-header
fn tcp_checksum(src: &[u8; 4], dst: &[u8; 4], segment: &[u8]) -> u16 {
    let mut sum: u32 = 0;

    // Pseudo-header: src IP
    sum += u16::from_be_bytes([src[0], src[1]]) as u32;
    sum += u16::from_be_bytes([src[2], src[3]]) as u32;
    // Pseudo-header: dst IP
    sum += u16::from_be_bytes([dst[0], dst[1]]) as u32;
    sum += u16::from_be_bytes([dst[2], dst[3]]) as u32;
    // Pseudo-header: protocol (6) + TCP length
    sum += 6u32; // protocol
    sum += segment.len() as u32;

    // TCP segment
    let mut i = 0;
    while i + 1 < segment.len() {
        sum += u16::from_be_bytes([segment[i], segment[i + 1]]) as u32;
        i += 2;
    }
    if i < segment.len() {
        sum += (segment[i] as u32) << 8;
    }

    while sum > 0xFFFF {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !sum as u16
}

// ─── Incoming packet handler (called from IPv4 dispatcher) ────────────────

/// Handle an incoming TCP segment. Called from ipv4::handle_ipv4().
pub fn handle_tcp(data: &[u8], src_ip: [u8; 4]) {
    if data.len() < 20 { return; }

    let src_port = u16::from_be_bytes([data[0], data[1]]);
    let dst_port = u16::from_be_bytes([data[2], data[3]]);
    let seq_num  = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    let ack_num  = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);
    let data_off = ((data[12] >> 4) as usize) * 4;
    let flags    = data[13];
    let window   = u16::from_be_bytes([data[14], data[15]]);

    if data_off > data.len() { return; }
    let payload = &data[data_off..];

    let is_syn = flags & TCP_SYN != 0;
    let is_ack = flags & TCP_ACK != 0;
    let is_fin = flags & TCP_FIN != 0;
    let is_rst = flags & TCP_RST != 0;
    let is_psh = flags & TCP_PSH != 0;
    let _ = is_psh; // suppress unused warning

    // 1. Try to find an existing connection
    if let Some(idx) = find_conn(dst_port, src_ip, src_port) {
        unsafe { handle_conn_segment(idx, seq_num, ack_num, flags, window, payload); }
        return;
    }

    // 2. SYN to a listener? (passive open)
    if is_syn && !is_ack {
        if let Some(lis) = find_listener(dst_port) {
            unsafe {
                let l = &mut CONNS[lis];
                // Store incoming SYN info for accept()
                l.accept_ready = true;
                l.accept_ip    = src_ip;
                l.accept_port  = src_port;
                l.accept_irs   = seq_num;
            }
            return;
        }
    }

    // 3. No matching connection or listener — send RST
    if !is_rst {
        let (rst_seq, rst_ack, rst_flags) = if is_ack {
            (ack_num, 0u32, TCP_RST)
        } else {
            let seg_len = payload.len() as u32
                + if is_syn { 1 } else { 0 }
                + if is_fin { 1 } else { 0 };
            (0u32, seq_num.wrapping_add(seg_len), TCP_RST | TCP_ACK)
        };
        send_tcp_segment(src_ip, dst_port, src_port, rst_seq, rst_ack, rst_flags, 0, &[]);
    }
}

/// Process a segment for an existing connection.
unsafe fn handle_conn_segment(
    idx: usize,
    seq: u32,
    ack: u32,
    flags: u8,
    window: u16,
    payload: &[u8],
) {
    let c = &mut CONNS[idx];

    let is_syn = flags & TCP_SYN != 0;
    let is_ack = flags & TCP_ACK != 0;
    let is_fin = flags & TCP_FIN != 0;
    let is_rst = flags & TCP_RST != 0;

    // RST handling — any state
    if is_rst {
        c.state = TcpState::Closed;
        return;
    }

    match c.state {
        TcpState::SynSent => {
            // Expecting SYN-ACK
            if is_syn && is_ack {
                if ack == c.snd_nxt {
                    c.rcv_irs = seq;
                    c.rcv_nxt = seq.wrapping_add(1);
                    c.snd_una = ack;
                    c.snd_wnd = window;
                    c.state = TcpState::Established;
                    // Send ACK to complete 3-way handshake
                    send_tcp_segment(
                        c.remote_ip, c.local_port, c.remote_port,
                        c.snd_nxt, c.rcv_nxt, TCP_ACK,
                        c.rcv_wnd, &[],
                    );
                } else {
                    // Bad ack — send RST
                    send_tcp_segment(
                        c.remote_ip, c.local_port, c.remote_port,
                        ack, 0, TCP_RST, 0, &[],
                    );
                    c.state = TcpState::Closed;
                }
            }
        }

        TcpState::SynReceived => {
            if is_ack && ack == c.snd_nxt {
                c.snd_una = ack;
                c.snd_wnd = window;
                c.state = TcpState::Established;
            }
        }

        TcpState::Established => {
            // Update send window
            if is_ack {
                if ack_in_range(c.snd_una, ack, c.snd_nxt) {
                    c.snd_una = ack;
                    c.retransmit_len = 0; // data acknowledged
                }
                c.snd_wnd = window;
            }

            // Accept data
            if !payload.is_empty() && seq == c.rcv_nxt {
                let written = c.rx_ring.write(payload);
                c.rcv_nxt = c.rcv_nxt.wrapping_add(written as u32);
                // Send ACK for received data
                send_tcp_segment(
                    c.remote_ip, c.local_port, c.remote_port,
                    c.snd_nxt, c.rcv_nxt, TCP_ACK,
                    c.rx_ring.free_space().min(u16::MAX as usize) as u16,
                    &[],
                );
            }

            // FIN from peer — transition to CloseWait
            if is_fin {
                c.rcv_nxt = c.rcv_nxt.wrapping_add(1);
                c.state = TcpState::CloseWait;
                send_tcp_segment(
                    c.remote_ip, c.local_port, c.remote_port,
                    c.snd_nxt, c.rcv_nxt, TCP_ACK,
                    c.rcv_wnd, &[],
                );
            }
        }

        TcpState::FinWait1 => {
            if is_ack && ack == c.snd_nxt {
                c.snd_una = ack;
                if is_fin {
                    // Simultaneous close — FIN+ACK received
                    c.rcv_nxt = c.rcv_nxt.wrapping_add(1);
                    c.state = TcpState::TimeWait;
                    send_tcp_segment(
                        c.remote_ip, c.local_port, c.remote_port,
                        c.snd_nxt, c.rcv_nxt, TCP_ACK,
                        c.rcv_wnd, &[],
                    );
                } else {
                    c.state = TcpState::FinWait2;
                }
            } else if is_fin {
                c.rcv_nxt = c.rcv_nxt.wrapping_add(1);
                c.state = TcpState::TimeWait;
                send_tcp_segment(
                    c.remote_ip, c.local_port, c.remote_port,
                    c.snd_nxt, c.rcv_nxt, TCP_ACK,
                    c.rcv_wnd, &[],
                );
            }
        }

        TcpState::FinWait2 => {
            if is_fin {
                c.rcv_nxt = c.rcv_nxt.wrapping_add(1);
                c.state = TcpState::TimeWait;
                send_tcp_segment(
                    c.remote_ip, c.local_port, c.remote_port,
                    c.snd_nxt, c.rcv_nxt, TCP_ACK,
                    c.rcv_wnd, &[],
                );
            }
        }

        TcpState::LastAck => {
            if is_ack && ack == c.snd_nxt {
                c.state = TcpState::Closed;
            }
        }

        TcpState::TimeWait => {
            // Stay in TimeWait; handled by timeout (simplified: immediate close)
            c.state = TcpState::Closed;
        }

        TcpState::CloseWait => {
            // Waiting for local close — just accept ACKs
            if is_ack {
                c.snd_una = ack;
            }
        }

        _ => {}
    }
}

/// Check if ack is in the valid range [una, nxt]
fn ack_in_range(una: u32, ack: u32, nxt: u32) -> bool {
    if nxt >= una {
        ack >= una && ack <= nxt
    } else {
        // Wrapped
        ack >= una || ack <= nxt
    }
}

// ─── Public API ───────────────────────────────────────────────────────────

/// Active open: connect to dst_ip:dst_port. Returns connection ID.
/// Blocks until established or timeout.
pub fn tcp_connect(dst_ip: [u8; 4], dst_port: u16) -> Result<usize, TcpError> {
    let idx = alloc_conn().ok_or(TcpError::NoFreeSlots)?;
    let local_port = next_ephemeral_port();
    let iss = next_iss();

    unsafe {
        let c = &mut CONNS[idx];
        c.reset();
        c.state = TcpState::SynSent;
        c.local_port = local_port;
        c.remote_port = dst_port;
        c.remote_ip = dst_ip;
        c.snd_iss = iss;
        c.snd_una = iss;
        c.snd_nxt = iss.wrapping_add(1); // SYN consumes one seq
        c.rcv_wnd = DEFAULT_WINDOW;
    }

    // Send SYN
    send_tcp_segment(dst_ip, local_port, dst_port, iss, 0, TCP_SYN, DEFAULT_WINDOW, &[]);
    unsafe { CONNS[idx].last_tx_tick = now_ticks(); }

    // Wait for connection establishment
    let deadline = now_ticks() + CONNECT_TIMEOUT_TICKS;
    let mut retransmit_deadline = now_ticks() + RETRANSMIT_TICKS;

    loop {
        super::poll_rx();
        let state = unsafe { CONNS[idx].state };

        match state {
            TcpState::Established => return Ok(idx),
            TcpState::Closed => return Err(TcpError::ConnectionRefused),
            _ => {}
        }

        let t = now_ticks();
        if t >= deadline {
            unsafe { CONNS[idx].reset(); }
            return Err(TcpError::TimedOut);
        }

        // Retransmit SYN if no response
        if t >= retransmit_deadline {
            let iss_val = unsafe { CONNS[idx].snd_iss };
            send_tcp_segment(dst_ip, local_port, dst_port, iss_val, 0, TCP_SYN, DEFAULT_WINDOW, &[]);
            retransmit_deadline = t + RETRANSMIT_TICKS;
        }

        unsafe { core::arch::asm!("hlt"); }
    }
}

/// Passive open: listen on a port. Returns listener connection ID.
pub fn tcp_listen(port: u16) -> Result<usize, TcpError> {
    let idx = alloc_conn().ok_or(TcpError::NoFreeSlots)?;
    unsafe {
        let c = &mut CONNS[idx];
        c.reset();
        c.state = TcpState::Listen;
        c.local_port = port;
    }
    Ok(idx)
}

/// Accept an incoming connection on a listener. Blocks until SYN arrives.
/// Returns a NEW connection ID for the accepted connection (listener stays open).
pub fn tcp_accept(listener: usize, timeout_ticks: u64) -> Result<usize, TcpError> {
    if listener >= MAX_CONNS { return Err(TcpError::InvalidConn); }
    unsafe {
        if CONNS[listener].state != TcpState::Listen {
            return Err(TcpError::InvalidConn);
        }
    }

    let deadline = now_ticks() + timeout_ticks;
    loop {
        super::poll_rx();

        let ready = unsafe { CONNS[listener].accept_ready };
        if ready {
            // Allocate a new connection for the accepted client
            let idx = alloc_conn().ok_or(TcpError::NoFreeSlots)?;
            let iss = next_iss();

            unsafe {
                let l = &mut CONNS[listener];
                let client_ip   = l.accept_ip;
                let client_port = l.accept_port;
                let client_irs  = l.accept_irs;
                let listen_port = l.local_port;
                l.accept_ready = false;

                let c = &mut CONNS[idx];
                c.reset();
                c.state = TcpState::SynReceived;
                c.local_port = listen_port;
                c.remote_port = client_port;
                c.remote_ip = client_ip;
                c.snd_iss = iss;
                c.snd_una = iss;
                c.snd_nxt = iss.wrapping_add(1);
                c.rcv_irs = client_irs;
                c.rcv_nxt = client_irs.wrapping_add(1);
                c.rcv_wnd = DEFAULT_WINDOW;

                // Send SYN-ACK
                send_tcp_segment(
                    client_ip, listen_port, client_port,
                    iss, c.rcv_nxt, TCP_SYN | TCP_ACK,
                    DEFAULT_WINDOW, &[],
                );
                c.last_tx_tick = now_ticks();
            }

            // Wait for ACK to complete handshake
            let hs_deadline = now_ticks() + CONNECT_TIMEOUT_TICKS;
            loop {
                super::poll_rx();
                let state = unsafe { CONNS[idx].state };
                match state {
                    TcpState::Established => return Ok(idx),
                    TcpState::Closed => return Err(TcpError::ConnectionReset),
                    _ => {}
                }
                if now_ticks() >= hs_deadline {
                    unsafe { CONNS[idx].reset(); }
                    return Err(TcpError::TimedOut);
                }
                unsafe { core::arch::asm!("hlt"); }
            }
        }

        if now_ticks() >= deadline {
            return Err(TcpError::TimedOut);
        }
        unsafe { core::arch::asm!("hlt"); }
    }
}

/// Send data on an established connection. Returns bytes sent.
pub fn tcp_send(conn: usize, data: &[u8]) -> Result<usize, TcpError> {
    if conn >= MAX_CONNS { return Err(TcpError::InvalidConn); }

    unsafe {
        let c = &mut CONNS[conn];
        match c.state {
            TcpState::Established | TcpState::CloseWait => {}
            TcpState::Closed => return Err(TcpError::NotConnected),
            _ => return Err(TcpError::NotConnected),
        }

        // Send in MSS-sized chunks (1460 - 20 TCP header = 1440 max payload per segment)
        let mss = 1440usize.min(c.snd_wnd as usize);
        if mss == 0 { return Err(TcpError::WouldBlock); }

        let mut sent = 0;
        while sent < data.len() {
            let chunk = (data.len() - sent).min(mss);
            let payload = &data[sent..sent + chunk];

            // Send segment with PSH+ACK
            send_tcp_segment(
                c.remote_ip, c.local_port, c.remote_port,
                c.snd_nxt, c.rcv_nxt, TCP_PSH | TCP_ACK,
                c.rx_ring.free_space().min(u16::MAX as usize) as u16,
                payload,
            );

            // Store for potential retransmit (last chunk only — simplified)
            let rlen = chunk.min(TX_BUF_SIZE);
            c.retransmit_data[..rlen].copy_from_slice(&payload[..rlen]);
            c.retransmit_len = rlen;
            c.last_tx_tick = now_ticks();

            c.snd_nxt = c.snd_nxt.wrapping_add(chunk as u32);
            sent += chunk;
        }

        Ok(sent)
    }
}

/// Receive data from an established connection. Returns bytes read.
/// If no data available and connection is still open, blocks briefly.
pub fn tcp_recv(conn: usize, buf: &mut [u8], timeout_ticks: u64) -> Result<usize, TcpError> {
    if conn >= MAX_CONNS { return Err(TcpError::InvalidConn); }

    let deadline = now_ticks() + timeout_ticks;

    loop {
        super::poll_rx();

        unsafe {
            let c = &mut CONNS[conn];

            // Read any available data
            let n = c.rx_ring.read(buf);
            if n > 0 {
                return Ok(n);
            }

            // Check connection state
            match c.state {
                TcpState::Established | TcpState::SynReceived => {
                    // Still connected but no data yet
                }
                TcpState::CloseWait | TcpState::LastAck | TcpState::TimeWait | TcpState::Closed => {
                    // Peer has closed
                    return Ok(0);
                }
                _ => return Err(TcpError::NotConnected),
            }

            // Retransmit check
            if c.retransmit_len > 0 && now_ticks() - c.last_tx_tick > RETRANSMIT_TICKS {
                send_tcp_segment(
                    c.remote_ip, c.local_port, c.remote_port,
                    c.snd_una, c.rcv_nxt, TCP_PSH | TCP_ACK,
                    c.rx_ring.free_space().min(u16::MAX as usize) as u16,
                    &c.retransmit_data[..c.retransmit_len],
                );
                c.last_tx_tick = now_ticks();
            }
        }

        if now_ticks() >= deadline { return Err(TcpError::TimedOut); }
        unsafe { core::arch::asm!("hlt"); }
    }
}

/// Non-blocking receive — returns WouldBlock if no data.
pub fn tcp_recv_nb(conn: usize, buf: &mut [u8]) -> Result<usize, TcpError> {
    if conn >= MAX_CONNS { return Err(TcpError::InvalidConn); }

    super::poll_rx();
    unsafe {
        let c = &mut CONNS[conn];
        let n = c.rx_ring.read(buf);
        if n > 0 { return Ok(n); }

        match c.state {
            TcpState::Established => Err(TcpError::WouldBlock),
            TcpState::CloseWait | TcpState::Closed => Ok(0),
            _ => Err(TcpError::NotConnected),
        }
    }
}

/// Close a connection gracefully (sends FIN).
pub fn tcp_close(conn: usize) {
    if conn >= MAX_CONNS { return; }

    unsafe {
        let c = &mut CONNS[conn];
        match c.state {
            TcpState::Established => {
                // Send FIN+ACK
                send_tcp_segment(
                    c.remote_ip, c.local_port, c.remote_port,
                    c.snd_nxt, c.rcv_nxt, TCP_FIN | TCP_ACK,
                    c.rcv_wnd, &[],
                );
                c.snd_nxt = c.snd_nxt.wrapping_add(1); // FIN occupies one seq
                c.state = TcpState::FinWait1;
                c.last_tx_tick = now_ticks();

                // Wait briefly for FIN-ACK (up to 3s)
                let deadline = now_ticks() + 300;
                while now_ticks() < deadline {
                    super::poll_rx();
                    if c.state == TcpState::Closed || c.state == TcpState::TimeWait {
                        break;
                    }
                    core::arch::asm!("hlt");
                }
                c.reset();
            }
            TcpState::CloseWait => {
                // We received FIN already; send our FIN
                send_tcp_segment(
                    c.remote_ip, c.local_port, c.remote_port,
                    c.snd_nxt, c.rcv_nxt, TCP_FIN | TCP_ACK,
                    c.rcv_wnd, &[],
                );
                c.snd_nxt = c.snd_nxt.wrapping_add(1);
                c.state = TcpState::LastAck;
                c.last_tx_tick = now_ticks();

                let deadline = now_ticks() + 300;
                while now_ticks() < deadline {
                    super::poll_rx();
                    if c.state == TcpState::Closed { break; }
                    core::arch::asm!("hlt");
                }
                c.reset();
            }
            TcpState::Listen | TcpState::SynSent | TcpState::SynReceived => {
                c.reset();
            }
            _ => {
                c.reset();
            }
        }
    }
}

/// Get the state of a connection (for display/diagnostics).
pub fn conn_state(conn: usize) -> TcpState {
    if conn >= MAX_CONNS { return TcpState::Closed; }
    unsafe { CONNS[conn].state }
}

/// Get info about a connection for netstat-like display.
pub fn conn_info(conn: usize) -> Option<(TcpState, u16, [u8; 4], u16)> {
    if conn >= MAX_CONNS { return None; }
    unsafe {
        let c = &CONNS[conn];
        if c.state == TcpState::Closed { return None; }
        Some((c.state, c.local_port, c.remote_ip, c.remote_port))
    }
}

/// Number of bytes available to read on a connection.
pub fn available(conn: usize) -> usize {
    if conn >= MAX_CONNS { return 0; }
    unsafe { CONNS[conn].rx_ring.available() }
}

/// Return a snapshot of all active connections for display.
/// Fills `out` with (state, local_port, remote_ip, remote_port), returns count.
pub fn active_connections(out: &mut [(TcpState, u16, [u8; 4], u16)]) -> usize {
    let mut n = 0;
    unsafe {
        for i in 0..MAX_CONNS {
            if n >= out.len() { break; }
            if CONNS[i].state != TcpState::Closed {
                out[n] = (CONNS[i].state, CONNS[i].local_port, CONNS[i].remote_ip, CONNS[i].remote_port);
                n += 1;
            }
        }
    }
    n
}
