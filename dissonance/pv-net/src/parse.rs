// SPDX-License-Identifier: AGPL-3.0-or-later
//! Panic-free L2 (and best-effort L3/L4) frame parsing.
//!
//! The switch needs only enough of a frame to route it (source/destination
//! node) and to target faults (a connection identity). We parse the Ethernet
//! header for addressing and, when the frame is IPv4/TCP-or-UDP, derive a
//! direction-independent [`ConnId`] from the 5-tuple. Everything else is left
//! alone — there is **no** ARP/bridge state machine (task non-goal).
//!
//! Every byte access goes through bounds-checked slicing, so `parse` never panics
//! and never reads out of bounds on arbitrary/truncated/mutated input
//! (conventions rule 4): malformed framing yields `None`, and a frame that is
//! addressable at L2 but unparseable at L3/L4 simply gets `conn = 0`.

use crate::types::{FrameHdr, NodeMap};
use crate::{ConnId, NodeId};

/// Minimum Ethernet II header: `dst(6) + src(6) + ethertype(2)`.
const ETH_HDR_LEN: usize = 14;
const ETHERTYPE_IPV4: u16 = 0x0800;
const IPPROTO_TCP: u8 = 6;
const IPPROTO_UDP: u8 = 17;
const BROADCAST_MAC: [u8; 6] = [0xff; 6];

/// Parse a raw L2 frame against the configured [`NodeMap`].
///
/// Returns `None` for input that cannot be routed: shorter than an Ethernet
/// header, or whose source/destination cannot be resolved to a node (an unknown
/// sender or an unknown unicast destination). A broadcast frame needs only a
/// resolvable source. **Never panics** on any byte string.
pub fn parse(frame: &[u8], nodes: &NodeMap) -> Option<FrameHdr> {
    let dst_mac: [u8; 6] = frame.get(0..6)?.try_into().ok()?;
    let src_mac: [u8; 6] = frame.get(6..12)?.try_into().ok()?;
    let ethertype = u16::from_be_bytes([*frame.get(12)?, *frame.get(13)?]);
    // Anything past the L2 header is the L3 payload; absent for a bare header.
    let l3 = frame.get(ETH_HDR_LEN..).unwrap_or(&[]);

    let broadcast = dst_mac == BROADCAST_MAC;

    // Best-effort IPv4 dissection: addresses (for the node fallback) and the
    // connection identity. Any shortfall leaves these `None`/`0`.
    let ipv4 = if ethertype == ETHERTYPE_IPV4 {
        parse_ipv4(l3)
    } else {
        None
    };
    let (src_ip, dst_ip) = match &ipv4 {
        Some(p) => (Some(p.src_ip), Some(p.dst_ip)),
        None => (None, None),
    };
    let conn = ipv4.as_ref().map_or(ConnId(0), Ipv4Dissection::conn);

    // Resolve nodes by MAC, falling back to IPv4 address (the "MAC/IP ↔ NodeId"
    // contract): a sender we cannot attribute, or a unicast destination we
    // cannot route to, makes the frame unroutable.
    let src = resolve(nodes, &src_mac, src_ip.as_ref())?;
    let dst = if broadcast {
        src // placeholder; broadcast routing ignores it
    } else {
        resolve(nodes, &dst_mac, dst_ip.as_ref())?
    };

    let len = u32::try_from(frame.len()).unwrap_or(u32::MAX);

    Some(FrameHdr {
        src_mac,
        dst_mac,
        broadcast,
        src,
        dst,
        conn,
        len,
    })
}

/// MAC first, then IPv4 address — the [`NodeMap`] resolution order.
fn resolve(nodes: &NodeMap, mac: &[u8; 6], ip: Option<&[u8; 4]>) -> Option<NodeId> {
    nodes
        .resolve_mac(mac)
        .or_else(|| ip.and_then(|ip| nodes.resolve_ip(ip)))
}

/// The IPv4 fields we extract: endpoints, protocol, and the L4 ports — `Some`
/// only for a *complete* TCP/UDP header, `None` otherwise (non-TCP/UDP, or a
/// TCP/UDP packet truncated before its ports).
struct Ipv4Dissection {
    src_ip: [u8; 4],
    dst_ip: [u8; 4],
    proto: u8,
    ports: Option<(u16, u16)>,
}

impl Ipv4Dissection {
    /// A direction-independent connection identity: the two `(ip, port)`
    /// endpoints are sorted before hashing, so both halves of a flow collapse to
    /// one [`ConnId`]. A frame with no complete TCP/UDP L4 header — non-TCP/UDP,
    /// or a TCP/UDP packet truncated before its ports — has **no** connection
    /// identity by contract: it returns [`ConnId`]`(0)`, like a non-IPv4 frame,
    /// even though its addresses are still parsed (for the node-resolution
    /// fallback).
    fn conn(&self) -> ConnId {
        let Some((src_port, dst_port)) = self.ports else {
            return ConnId(0);
        };
        let a = endpoint_bytes(self.src_ip, src_port);
        let b = endpoint_bytes(self.dst_ip, dst_port);
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let mut buf = [0u8; 13];
        buf[0] = self.proto;
        buf[1..7].copy_from_slice(&lo);
        buf[7..13].copy_from_slice(&hi);
        ConnId(fnv1a64(&buf))
    }
}

/// Pack an `(ip, port)` endpoint into 6 comparable big-endian bytes.
fn endpoint_bytes(ip: [u8; 4], port: u16) -> [u8; 6] {
    let p = port.to_be_bytes();
    [ip[0], ip[1], ip[2], ip[3], p[0], p[1]]
}

/// Parse an IPv4 header (and its TCP/UDP ports) out of the L3 payload. Returns
/// `None` if it is not a well-formed IPv4 packet; bounds-checked throughout.
fn parse_ipv4(l3: &[u8]) -> Option<Ipv4Dissection> {
    let version_ihl = *l3.first()?;
    if version_ihl >> 4 != 4 {
        return None;
    }
    // IHL is the header length in 32-bit words; minimum 5 (20 bytes).
    let ihl_words = (version_ihl & 0x0f) as usize;
    if ihl_words < 5 {
        return None;
    }
    let ihl_bytes = ihl_words * 4;
    // Total IP packet length (header + data) the IPv4 header declares (bytes
    // 2..4). The L4 ports must fall within *this*, not merely within the captured
    // bytes — a frame can carry trailing padding past the IP packet.
    let total_length = u16::from_be_bytes([*l3.get(2)?, *l3.get(3)?]) as usize;
    let proto = *l3.get(9)?;
    let src_ip: [u8; 4] = l3.get(12..16)?.try_into().ok()?;
    let dst_ip: [u8; 4] = l3.get(16..20)?.try_into().ok()?;

    // Flags + fragment offset (IPv4 header bytes 6..8); the low 13 bits are the
    // offset. A non-first fragment (offset != 0) carries no L4 header at its
    // payload start, so it has no usable ports.
    let fragment_offset = u16::from_be_bytes([*l3.get(6)?, *l3.get(7)?]) & 0x1fff;

    // The 4-byte port pair sits at `[ihl_bytes, l4_end)` (honoring IHL). It is
    // `Some` only when the frame is TCP/UDP, is *not* a non-first fragment, the
    // **declared** IP `total_length` actually reaches the ports, *and* the
    // captured bytes do too. Anything else (non-TCP/UDP, a non-first fragment, an
    // IHL/total_length that leaves no room for L4, or a packet truncated before
    // its ports) yields `None` (and a `conn` of 0). Bounds-checked throughout, so
    // a short or mis-declared header never reads past the captured bytes.
    let l4_end = ihl_bytes + 4;
    let ports = if (proto == IPPROTO_TCP || proto == IPPROTO_UDP)
        && fragment_offset == 0
        && total_length >= l4_end
    {
        l3.get(ihl_bytes..l4_end).map(|l4| {
            (
                u16::from_be_bytes([l4[0], l4[1]]),
                u16::from_be_bytes([l4[2], l4[3]]),
            )
        })
    } else {
        None
    };

    Some(Ipv4Dissection {
        src_ip,
        dst_ip,
        proto,
        ports,
    })
}

/// FNV-1a 64-bit — a fixed, deterministic, dependency-free hash for the
/// connection identity (no host entropy, no `HashMap` involved).
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
