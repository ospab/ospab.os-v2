/*
 * IPv4 layer
 * Parse incoming IPv4 packets, build outgoing ones.
 * Minimal: no fragmentation, no options, TTL=64.
 */

const PROTO_ICMP: u8 = 1;
const PROTO_UDP: u8  = 17;

/// Handle an incoming IPv4 packet (Ethernet payload)
pub fn handle_ipv4(data: &[u8]) {
    if data.len() < 20 { return; }

    let version = data[0] >> 4;
    let ihl = (data[0] & 0x0F) as usize;
    if version != 4 || ihl < 5 { return; }

    let header_len = ihl * 4;
    let total_len = u16::from_be_bytes([data[2], data[3]]) as usize;
    if data.len() < total_len || total_len < header_len { return; }

    let protocol = data[9];
    let ttl      = data[8];
    let _src_ip = [data[12], data[13], data[14], data[15]];
    let dst_ip  = [data[16], data[17], data[18], data[19]];

    // Check if this packet is for us
    let our_ip = unsafe { super::OUR_IP };
    if dst_ip != our_ip && dst_ip != [255, 255, 255, 255] { return; }

    let payload = &data[header_len..total_len];

    match protocol {
        PROTO_ICMP => super::icmp::handle_icmp(payload, _src_ip, ttl),
        PROTO_UDP  => super::udp::handle_udp(payload, _src_ip),
        _ => {}
    }
}

/// Build and send an IPv4 packet
/// protocol: IP protocol number, dst_ip: destination, payload: data above IP
pub fn send_ipv4(protocol: u8, dst_ip: [u8; 4], payload: &[u8]) {
    let total_len = 20 + payload.len();
    if total_len > 1480 { return; }

    let mut pkt = [0u8; 1500];

    // Version (4) + IHL (5) = 0x45
    pkt[0] = 0x45;
    // DSCP + ECN = 0
    pkt[1] = 0x00;
    // Total length
    let tl = (total_len as u16).to_be_bytes();
    pkt[2] = tl[0];
    pkt[3] = tl[1];
    // Identification (simple counter)
    static mut IP_ID: u16 = 1;
    unsafe {
        let id = IP_ID.to_be_bytes();
        pkt[4] = id[0];
        pkt[5] = id[1];
        IP_ID = IP_ID.wrapping_add(1);
    }
    // Flags + Fragment offset: Don't Fragment
    pkt[6] = 0x40;
    pkt[7] = 0x00;
    // TTL
    pkt[8] = 64;
    // Protocol
    pkt[9] = protocol;
    // Checksum (0 for now, will compute)
    pkt[10] = 0;
    pkt[11] = 0;
    // Source IP
    let src = unsafe { super::OUR_IP };
    pkt[12..16].copy_from_slice(&src);
    // Destination IP
    pkt[16..20].copy_from_slice(&dst_ip);

    // Compute header checksum
    let cksum = checksum(&pkt[..20]);
    pkt[10] = (cksum >> 8) as u8;
    pkt[11] = (cksum & 0xFF) as u8;

    // Payload
    pkt[20..20 + payload.len()].copy_from_slice(payload);

    // Determine destination MAC
    let dst_mac = resolve_mac(dst_ip);

    // Send via Ethernet
    super::ethernet::send_frame(dst_mac, super::ethernet::ETHERTYPE_IPV4, &pkt[..total_len]);
}

/// Resolve MAC for IP: check ARP cache first, then fall back to gateway/broadcast
fn resolve_mac(dst_ip: [u8; 4]) -> [u8; 6] {
    let our_ip = unsafe { super::OUR_IP };
    let mask   = unsafe { super::SUBNET_MASK };

    // Direct cache look-up — covers both local and remote (via gateway ARP)
    if let Some(mac) = super::arp::cache_lookup(dst_ip) {
        return mac;
    }

    // Check if destination is on our subnet
    let same_subnet = (dst_ip[0] & mask[0]) == (our_ip[0] & mask[0])
        && (dst_ip[1] & mask[1]) == (our_ip[1] & mask[1])
        && (dst_ip[2] & mask[2]) == (our_ip[2] & mask[2])
        && (dst_ip[3] & mask[3]) == (our_ip[3] & mask[3]);

    if same_subnet {
        // Send ARP request and wait briefly for reply
        super::arp::send_request(dst_ip);
        for _ in 0..100 {
            super::poll_rx();
            if let Some(mac) = super::arp::cache_lookup(dst_ip) {
                return mac;
            }
            for _ in 0..50_000u32 { unsafe { core::arch::asm!("pause"); } }
        }
        [0xFF; 6] // fallback: broadcast
    } else {
        // Route via gateway — use cached gateway MAC or broadcast
        let gw_ip = unsafe { super::GATEWAY_IP };
        if let Some(mac) = super::arp::cache_lookup(gw_ip) {
            return mac;
        }
        unsafe { super::GATEWAY_MAC }
    }
}

/// Compute IP checksum (one's complement of one's complement sum of 16-bit words)
pub fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += u16::from_be_bytes([data[i], data[i + 1]]) as u32;
        i += 2;
    }
    if i < data.len() {
        sum += (data[i] as u32) << 8;
    }
    while sum > 0xFFFF {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !sum as u16
}
