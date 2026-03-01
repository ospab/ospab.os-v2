/*
 * Ethernet frame handling
 * Parse incoming frames and dispatch by EtherType
 * Build outgoing frames with proper headers
 */

// EtherType constants
pub const ETHERTYPE_IPV4: u16 = 0x0800;
pub const ETHERTYPE_ARP: u16  = 0x0806;

/// Parse an incoming Ethernet frame and dispatch to protocol handlers
pub fn handle_frame(data: &[u8]) {
    if data.len() < 14 { return; }

    // Ethernet header: dst[6] + src[6] + ethertype[2]
    let ethertype = u16::from_be_bytes([data[12], data[13]]);
    let payload = &data[14..];

    match ethertype {
        ETHERTYPE_ARP  => super::arp::handle_arp(payload),
        ETHERTYPE_IPV4 => super::ipv4::handle_ipv4(payload),
        _ => {} // Ignore unknown protocol
    }
}

/// Build and send an Ethernet frame
/// dst_mac: destination MAC, ethertype: protocol, payload: data above Ethernet
pub fn send_frame(dst_mac: [u8; 6], ethertype: u16, payload: &[u8]) {
    let total = 14 + payload.len();
    if total > 1514 { return; }

    let mut frame = [0u8; 1514];

    // Destination MAC
    frame[0..6].copy_from_slice(&dst_mac);

    // Source MAC
    let our_mac = unsafe { super::OUR_MAC };
    frame[6..12].copy_from_slice(&our_mac);

    // EtherType
    let et = ethertype.to_be_bytes();
    frame[12] = et[0];
    frame[13] = et[1];

    // Payload
    frame[14..14 + payload.len()].copy_from_slice(payload);

    // Minimum Ethernet frame is 60 bytes (excl FCS)
    let send_len = if total < 60 { 60 } else { total };

    super::send_raw(&frame, send_len);
}
