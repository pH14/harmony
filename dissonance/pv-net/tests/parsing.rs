// SPDX-License-Identifier: AGPL-3.0-or-later
//! Frame parsing: L2 addressing/routing resolution, broadcast detection, the
//! IPv4 connection identity, and the IPv4 node-resolution fallback.

mod common;

use common::{BROADCAST, eth_frame, mac, node_map};
use pv_net::{ConnId, NodeId, NodeMap, parse};

/// An Ethernet/IPv4/UDP frame, for exercising L3/L4 dissection.
#[allow(clippy::too_many_arguments)]
fn ipv4_udp(
    dst_mac: [u8; 6],
    src_mac: [u8; 6],
    src_ip: [u8; 4],
    dst_ip: [u8; 4],
    src_port: u16,
    dst_port: u16,
) -> Vec<u8> {
    let mut f = Vec::new();
    f.extend_from_slice(&dst_mac);
    f.extend_from_slice(&src_mac);
    f.extend_from_slice(&[0x08, 0x00]); // ethertype IPv4
    // IPv4 header (IHL = 5).
    f.push(0x45);
    f.push(0x00);
    f.extend_from_slice(&[0x00, 0x1c]); // total length (unchecked by parse)
    f.extend_from_slice(&[0x00, 0x00]); // id
    f.extend_from_slice(&[0x00, 0x00]); // flags/frag
    f.push(64); // ttl
    f.push(17); // protocol UDP
    f.extend_from_slice(&[0x00, 0x00]); // checksum
    f.extend_from_slice(&src_ip);
    f.extend_from_slice(&dst_ip);
    // UDP header.
    f.extend_from_slice(&src_port.to_be_bytes());
    f.extend_from_slice(&dst_port.to_be_bytes());
    f.extend_from_slice(&[0x00, 0x08]); // udp length
    f.extend_from_slice(&[0x00, 0x00]); // udp checksum
    f
}

#[test]
fn parses_a_unicast_frame() {
    let nodes = node_map(4);
    let f = eth_frame(mac(2), mac(1), 0xAB, 10);
    let hdr = parse(&f, &nodes).expect("resolvable");
    assert_eq!(hdr.src, NodeId(1));
    assert_eq!(hdr.dst, NodeId(2));
    assert!(!hdr.broadcast);
    assert_eq!(hdr.src_mac, mac(1));
    assert_eq!(hdr.dst_mac, mac(2));
    assert_eq!(hdr.len, f.len() as u32);
    assert_eq!(
        hdr.conn,
        ConnId(0),
        "non-IPv4 frame has no connection identity"
    );
}

#[test]
fn detects_broadcast() {
    let nodes = node_map(4);
    let f = eth_frame(BROADCAST, mac(1), 0xAB, 4);
    let hdr = parse(&f, &nodes).expect("resolvable source");
    assert!(hdr.broadcast);
    assert_eq!(hdr.src, NodeId(1));
}

#[test]
fn too_short_is_none() {
    let nodes = node_map(4);
    for len in 0..14 {
        assert!(
            parse(&vec![0u8; len], &nodes).is_none(),
            "len {len} < L2 header"
        );
    }
}

#[test]
fn unknown_endpoints_are_unroutable() {
    let nodes = node_map(2); // only nodes 0 and 1 registered
    // Unknown source.
    assert!(parse(&eth_frame(mac(0), mac(9), 0, 4), &nodes).is_none());
    // Unknown unicast destination.
    assert!(parse(&eth_frame(mac(9), mac(0), 0, 4), &nodes).is_none());
}

#[test]
fn ipv4_conn_is_direction_independent_and_nonzero() {
    let nodes = node_map(4);
    let a = [10, 0, 0, 1];
    let b = [10, 0, 0, 2];
    let fwd = ipv4_udp(mac(2), mac(1), a, b, 1111, 2222);
    let rev = ipv4_udp(mac(1), mac(2), b, a, 2222, 1111);

    let cf = parse(&fwd, &nodes).unwrap().conn;
    let cr = parse(&rev, &nodes).unwrap().conn;
    assert_eq!(cf, cr, "both halves of a flow map to one ConnId");
    assert_ne!(cf, ConnId(0), "a real IPv4/UDP flow has a non-zero conn");
}

#[test]
fn ipv4_address_resolves_a_node_when_the_mac_is_unknown() {
    let mut nodes = NodeMap::new();
    nodes.insert_ip([10, 0, 0, 1], NodeId(5));
    nodes.insert_ip([10, 0, 0, 2], NodeId(6));
    // MACs deliberately not registered → resolution falls back to the IPv4 src/dst.
    let f = ipv4_udp(mac(0xAA), mac(0xBB), [10, 0, 0, 1], [10, 0, 0, 2], 80, 443);
    let hdr = parse(&f, &nodes).expect("resolvable by IP");
    assert_eq!(hdr.src, NodeId(5));
    assert_eq!(hdr.dst, NodeId(6));
}

#[test]
fn ipv4_non_tcp_udp_has_zero_conn() {
    let nodes = node_map(4);
    // Build an IPv4 frame, then flip the protocol byte (IPv4 header offset 9 ==
    // frame offset 23) to ICMP (1): routable at L2/L3, but no connection identity.
    let mut f = ipv4_udp(mac(2), mac(1), [10, 0, 0, 1], [10, 0, 0, 2], 1111, 2222);
    f[23] = 1; // IPPROTO_ICMP
    let hdr = parse(&f, &nodes).expect("still routable");
    assert_eq!(hdr.src, NodeId(1));
    assert_eq!(hdr.dst, NodeId(2));
    assert_eq!(
        hdr.conn,
        ConnId(0),
        "conn is 0 unless the frame is IPv4/TCP-or-UDP"
    );
}

#[test]
fn ipv4_non_first_fragment_has_zero_conn() {
    let nodes = node_map(4);
    // The IPv4 flags/fragment-offset field is at frame offset 20..22 (IPv4 header
    // bytes 6..8): top 3 bits flags (incl. MF = 0x2000), low 13 bits the offset.
    let mut f = ipv4_udp(mac(2), mac(1), [10, 0, 0, 1], [10, 0, 0, 2], 1111, 2222);

    // Non-first fragment: MF set + offset 100. Its payload start is not the L4
    // header, so the "ports" there are not ports → conn must be 0.
    f[20] = 0x20; // MF
    f[21] = 0x64; // fragment offset 100
    let frag = parse(&f, &nodes).expect("still L2/L3 routable");
    assert_eq!(frag.src, NodeId(1));
    assert_eq!(frag.dst, NodeId(2));
    assert_eq!(
        frag.conn,
        ConnId(0),
        "a non-first IPv4 fragment has no connection identity"
    );

    // Guard the inverse: a FIRST fragment (offset 0, MF set) DOES carry the L4
    // header, so it keeps a real conn — the gate is on offset, not on MF.
    f[20] = 0x20; // MF
    f[21] = 0x00; // offset 0
    let first = parse(&f, &nodes).expect("routable");
    assert_ne!(
        first.conn,
        ConnId(0),
        "the first fragment carries the L4 header, so it has a conn"
    );
}

#[test]
fn ipv4_total_length_excluding_l4_has_zero_conn() {
    let nodes = node_map(4);
    // Captured frame fully carries the UDP header, but the IPv4 total_length
    // (frame offset 16..18) is set to 20 — the IP header only, no room for the L4
    // ports. The bytes at the L4 offset are then trailing padding, not ports, so
    // conn must be 0 even though they are present in the capture.
    let mut f = ipv4_udp(mac(2), mac(1), [10, 0, 0, 1], [10, 0, 0, 2], 1111, 2222);
    f[16] = 0x00;
    f[17] = 0x14; // total_length = 20 (IP header only)
    let hdr = parse(&f, &nodes).expect("L2/L3 routable");
    assert_eq!(hdr.src, NodeId(1));
    assert_eq!(hdr.dst, NodeId(2));
    assert_eq!(
        hdr.conn,
        ConnId(0),
        "total_length leaving no room for L4 ports → conn 0"
    );

    // Sanity: with total_length covering the ports (28 = 20 IP + 8 UDP), the same
    // frame has a real conn — the gate is on the declared length, not the proto.
    f[16] = 0x00;
    f[17] = 0x1c;
    assert_ne!(parse(&f, &nodes).unwrap().conn, ConnId(0));
}

#[test]
fn truncated_l4_tcp_has_zero_conn_no_oob() {
    let nodes = node_map(4);
    // A complete 20-byte IPv4 header declaring TCP, but ZERO L4 bytes after it:
    // the ports can't be read, so conn must be 0 (and parsing must not read past
    // the captured bytes — bounds-checked).
    let mut f = Vec::new();
    f.extend_from_slice(&mac(2)); // dst
    f.extend_from_slice(&mac(1)); // src
    f.extend_from_slice(&[0x08, 0x00]); // ethertype IPv4
    f.push(0x45); // version 4, IHL 5
    f.push(0x00);
    f.extend_from_slice(&[0x00, 0x14]); // total length
    f.extend_from_slice(&[0x00, 0x00]); // id
    f.extend_from_slice(&[0x00, 0x00]); // flags/frag
    f.push(64); // ttl
    f.push(6); // protocol TCP
    f.extend_from_slice(&[0x00, 0x00]); // checksum
    f.extend_from_slice(&[10, 0, 0, 1]); // src ip
    f.extend_from_slice(&[10, 0, 0, 2]); // dst ip
    // no L4 segment at all
    assert_eq!(f.len(), 14 + 20);

    let hdr = parse(&f, &nodes).expect("L2/L3 routable");
    assert_eq!(hdr.src, NodeId(1));
    assert_eq!(hdr.dst, NodeId(2));
    assert_eq!(
        hdr.conn,
        ConnId(0),
        "a TCP frame truncated before its ports has no connection identity"
    );
}

#[test]
fn truncated_ipv4_keeps_l2_routing_with_zero_conn() {
    let nodes = node_map(4);
    // IPv4 ethertype but only a couple of L3 bytes: routable at L2, conn falls
    // back to 0, and crucially no panic.
    let mut f = eth_frame(mac(2), mac(1), 0, 0);
    f[12] = 0x08;
    f[13] = 0x00; // claim IPv4
    f.extend_from_slice(&[0x45, 0x00]); // a stub, far short of a full header
    let hdr = parse(&f, &nodes).expect("still L2-routable");
    assert_eq!(hdr.src, NodeId(1));
    assert_eq!(hdr.dst, NodeId(2));
    assert_eq!(hdr.conn, ConnId(0));
}
