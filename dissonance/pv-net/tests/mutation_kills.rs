// SPDX-License-Identifier: AGPL-3.0-or-later
//! Task 35 — exact-value tests that kill the mutants the first full-tree
//! `cargo mutants` run left surviving in this crate. The production logic is
//! correct; these tests *pin* the precise boundary / byte / hash that a mutated
//! operator would change, so the mutation now fails a test instead of slipping
//! through. Grouped by source location.
//!
//! Note on `switch.rs:201` (`held_before > 0` → `held_before >= 0`): that mutant
//! is **equivalent** and so is intentionally not targeted here. `held_before` is
//! `self.held.get(&link).map_or(0, Vec::len)`, and the codec rejects empty held
//! buffers (`nframes >= 1`) while every internal path prunes them, so
//! `held_before == 0` holds **iff** the link has no held buffer. At that boundary
//! the extra `>=` iteration is a no-op: either `get_mut` returns `None`, or (when
//! the current send is a `Reorder` that just pushed) `drain(0..0)` removes nothing
//! and the non-empty buffer is kept. The `> ` → `==` / `<` siblings *are* killed,
//! by `golden.rs::reorder_delivered_after_the_next_frame_on_the_link` (they break
//! the `held_before > 0` release path). See IMPLEMENTATION.md.

mod common;

use common::{FixedOracle, frame, mac, node_map};
use pv_net::{ConnId, NetAnswer, NetError, NodeId, REORDER_MAX, Switch, VTime, parse};

const L0: u64 = 100;

// ===========================================================================
// lib.rs:82 — `REORDER_MAX = VTime(1 << 20)` (`<<` → `>>` would make it 0).
// ===========================================================================

#[test]
fn reorder_max_is_one_left_shifted_twenty() {
    // The literal `1 << 20` here is in test code (never mutated); a `<<`→`>>`
    // mutation of the source constant makes it `1 >> 20 == 0`.
    assert_eq!(REORDER_MAX, VTime(1 << 20));
    assert_eq!(REORDER_MAX.0, 1_048_576);
}

#[test]
fn reorder_horizon_is_exactly_t_plus_l0_plus_one_megabyte() {
    // Behavioral pin of the same constant: a held last-frame reorder flushes at
    // exactly `T + L0 + (1 << 20)` — not at `T + L0` (which `>>` would yield).
    let mut s = Switch::new(node_map(2), VTime(L0));
    let now = 1_000u64;
    let mut hold = FixedOracle(NetAnswer::Reorder);
    s.on_tx(VTime(now), frame(0, 1, 0xAA, 4), &mut hold);

    let horizon = now + L0 + (1 << 20);
    assert!(
        s.due(VTime(horizon - 1)).is_empty(),
        "nothing is due one tick before the 1 MiB horizon"
    );
    let flushed = s.due(VTime(horizon));
    assert_eq!(flushed.len(), 1, "flushed exactly at T + L0 + (1 << 20)");
    assert_eq!(flushed[0].at, VTime(horizon));
}

// ===========================================================================
// switch.rs:247 — throttle fixed-window index `(now - start) / per`
// (`-` → `+` would shift which window a send falls in).
// ===========================================================================

#[test]
fn throttle_window_index_subtracts_the_start() {
    // With a non-zero window start that is NOT a multiple of `per`, `(now-start)`
    // and `(now+start)` land in *different* fixed windows, so the `-`→`+` mutant
    // resets the per-window counter at the wrong time. start=30, per=100, max=1:
    //   now=50  → (50-30)/100 = 0   ;  (50+30)/100 = 0
    //   now=100 → (100-30)/100 = 0  ;  (100+30)/100 = 1
    // So under `-` both sends share window 0 (second clogged); under `+` they are
    // in windows 0 and 1 (second admitted). Existing tests use start=0, where
    // `now-0 == now+0`, so they cannot see this.
    let mut s = Switch::new(node_map(2), VTime(L0));
    s.set_throttle(
        (NodeId(0), NodeId(1)),
        (1, VTime(100)),          // max 1 per 100 V-time
        (VTime(30), VTime(5000)), // window starts at 30 (not a multiple of 100)
    );
    let mut deliver = FixedOracle(NetAnswer::Deliver);

    assert_eq!(
        s.on_tx(VTime(50), frame(0, 1, 1, 4), &mut deliver).len(),
        1,
        "first send in window 0 is admitted"
    );
    assert_eq!(
        s.on_tx(VTime(100), frame(0, 1, 2, 4), &mut deliver).len(),
        0,
        "second send is still in window 0 (now-start) and is clogged; \
         the `+` mutant would put it in window 1 and admit it"
    );
}

// ===========================================================================
// parse.rs — the IPv4 connection identity (sort, endpoint packing, FNV) and
// the IHL bound.
// ===========================================================================

/// An independent FNV-1a/64 reference (the published constants), so the conn
/// golden is checked against the algorithm spec, not a copy of the crate's
/// internal state. A `^=`→`|=`/`&=` mutation of the crate's mixing step makes its
/// output diverge from this.
fn ref_fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The expected `ConnId` per the documented contract: a 13-byte buffer
/// `[proto, min(endpoint_a, endpoint_b), max(...)]`, each endpoint packed
/// big-endian as `[ip0..ip3, port_hi, port_lo]`, hashed with FNV-1a/64.
fn expected_conn(proto: u8, src_ip: [u8; 4], src_port: u16, dst_ip: [u8; 4], dst_port: u16) -> u64 {
    let endpoint = |ip: [u8; 4], port: u16| {
        let p = port.to_be_bytes();
        [ip[0], ip[1], ip[2], ip[3], p[0], p[1]]
    };
    let a = endpoint(src_ip, src_port);
    let b = endpoint(dst_ip, dst_port);
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    let mut buf = [0u8; 13];
    buf[0] = proto;
    buf[1..7].copy_from_slice(&lo);
    buf[7..13].copy_from_slice(&hi);
    ref_fnv1a64(&buf)
}

/// Build an Ethernet/IPv4 frame (IHL 5) carrying a 4-byte L4 port pair for
/// `proto` (6 = TCP, 17 = UDP).
#[allow(clippy::too_many_arguments)]
fn ipv4_l4(
    dst_mac: [u8; 6],
    src_mac: [u8; 6],
    proto: u8,
    src_ip: [u8; 4],
    dst_ip: [u8; 4],
    src_port: u16,
    dst_port: u16,
) -> Vec<u8> {
    let mut f = Vec::new();
    f.extend_from_slice(&dst_mac);
    f.extend_from_slice(&src_mac);
    f.extend_from_slice(&[0x08, 0x00]); // ethertype IPv4
    f.push(0x45); // version 4, IHL 5
    f.push(0x00);
    f.extend_from_slice(&[0x00, 0x1c]); // total length 28 = 20 IP + 8 L4
    f.extend_from_slice(&[0x00, 0x00]); // id
    f.extend_from_slice(&[0x00, 0x00]); // flags/frag (offset 0)
    f.push(64); // ttl
    f.push(proto);
    f.extend_from_slice(&[0x00, 0x00]); // checksum
    f.extend_from_slice(&src_ip);
    f.extend_from_slice(&dst_ip);
    f.extend_from_slice(&src_port.to_be_bytes());
    f.extend_from_slice(&dst_port.to_be_bytes());
    f.extend_from_slice(&[0x00, 0x08]); // L4 length-ish padding
    f.extend_from_slice(&[0x00, 0x00]);
    f
}

#[test]
fn ipv4_conn_is_the_exact_fnv_of_the_sorted_endpoints() {
    // Kills, in one shot: parse.rs:107 (the `a <= b` endpoint sort — a `>` mutant
    // swaps lo/hi), parse.rs:118 (`endpoint_bytes` replaced by a constant), and
    // parse.rs:181/183 (`fnv1a64` replaced by `1`, or its `^=` mixing → `|=`).
    let nodes = node_map(4);
    let src_ip = [10, 0, 0, 1];
    let dst_ip = [10, 0, 0, 2];
    let (sp, dp) = (1111u16, 2222u16);

    let f = ipv4_l4(mac(2), mac(1), 17, src_ip, dst_ip, sp, dp);
    let conn = parse(&f, &nodes).expect("routable").conn;

    let want = expected_conn(17, src_ip, sp, dst_ip, dp);
    assert_eq!(conn, ConnId(want), "conn is the FNV of [proto, min, max]");
    assert_ne!(conn, ConnId(0));
    assert_ne!(conn, ConnId(1), "not the constant-return mutant value");

    // Direction-independence is preserved at the exact value, too.
    let rev = ipv4_l4(mac(1), mac(2), 17, dst_ip, src_ip, dp, sp);
    assert_eq!(parse(&rev, &nodes).expect("routable").conn, ConnId(want));
}

#[test]
fn distinct_flows_get_distinct_conns() {
    // A second, independent guard against the constant-return mutants
    // (`endpoint_bytes`→`[0;6]`/`[1;6]`, `fnv1a64`→`1`): those collapse *every*
    // flow to one value, so two genuinely different flows would alias.
    let nodes = node_map(4);
    let c1 = parse(
        &ipv4_l4(mac(2), mac(1), 6, [10, 0, 0, 1], [10, 0, 0, 2], 1000, 2000),
        &nodes,
    )
    .unwrap()
    .conn;
    let c2 = parse(
        &ipv4_l4(mac(2), mac(1), 6, [10, 0, 0, 3], [10, 0, 0, 4], 3000, 4000),
        &nodes,
    )
    .unwrap()
    .conn;
    assert_ne!(c1, c2, "different 5-tuples must hash to different conns");
    assert_ne!(c1, ConnId(0));
    assert_ne!(c2, ConnId(0));
}

#[test]
fn ipv4_with_ihl_below_five_has_zero_conn() {
    // parse.rs:131 — `if ihl_words < 5 { return None }` rejects a sub-minimum IHL.
    // A `<`→`>` mutant would accept IHL 4 and read "ports" out of the address
    // bytes, yielding a non-zero conn. The frame stays L2-routable (MAC resolves),
    // so the only tell is `conn == 0`.
    let nodes = node_map(4);
    let mut f = Vec::new();
    f.extend_from_slice(&mac(2)); // dst → node 2
    f.extend_from_slice(&mac(1)); // src → node 1
    f.extend_from_slice(&[0x08, 0x00]); // ethertype IPv4
    // 20 bytes of L3 with IHL = 4 (invalid; minimum is 5).
    f.push(0x44); // version 4, IHL 4
    f.push(0x00);
    f.extend_from_slice(&[0x00, 0x14]); // total length 20
    f.extend_from_slice(&[0x00, 0x00]); // id
    f.extend_from_slice(&[0x00, 0x00]); // flags/frag
    f.push(64);
    f.push(6); // TCP
    f.extend_from_slice(&[0x00, 0x00]); // checksum
    f.extend_from_slice(&[10, 0, 0, 1]); // src ip
    f.extend_from_slice(&[10, 0, 0, 2]); // dst ip

    let hdr = parse(&f, &nodes).expect("L2-routable");
    assert_eq!(hdr.src, NodeId(1));
    assert_eq!(hdr.dst, NodeId(2));
    assert_eq!(
        hdr.conn,
        ConnId(0),
        "a sub-minimum IHL is not a parseable IPv4 header → no conn"
    );
}

// ===========================================================================
// codec.rs — the strict, total snapshot decoder (`decode_into`). Driven through
// the public `save_state`/`restore_state`.
// ===========================================================================

/// Restore `blob` into a fresh switch with the same node map; returns the result.
fn restore(nodes: u32, blob: &[u8]) -> Result<(), NetError> {
    let mut s = Switch::new(node_map(nodes), VTime(L0));
    s.restore_state(blob)
}

#[test]
fn codec104_partition_with_equal_endpoints_round_trips() {
    // `if a > b { Malformed }` — `a == b` is a valid (self-)partition. A `>`→`==`
    // or `>`→`>=` mutant would reject the equal-endpoint case.
    let mut s = Switch::new(node_map(4), VTime(L0));
    s.set_partition(NodeId(1), NodeId(1), (VTime(10), VTime(50))); // a == b == 1
    let blob = s.save_state();
    assert!(restore(4, &blob).is_ok(), "an a==b partition is admissible");

    // And re-saving from the restored switch reproduces the bytes exactly.
    let mut s2 = Switch::new(node_map(4), VTime(L0));
    s2.restore_state(&blob).unwrap();
    assert_eq!(s2.save_state(), blob);
}

#[test]
fn codec104_partition_with_a_greater_than_b_is_rejected() {
    // The other side of the `>` boundary: a hand-built blob with a > b must be
    // rejected. (`save_state` normalizes a<=b, so this can only come from crafted
    // / hostile bytes.) A `>`→`==` mutant would wrongly accept it.
    let mut s = Switch::new(node_map(4), VTime(L0));
    s.set_partition(NodeId(0), NodeId(1), (VTime(10), VTime(50)));
    let mut blob = s.save_state();
    assert!(restore(4, &blob).is_ok(), "the canonical blob is valid");

    // Partition record starts after header(6)+l0(8)+next_seq(8)+count(4) = 26:
    // a:u32 @ 26..30, b:u32 @ 30..34. Swap so a=1 > b=0.
    assert_eq!(&blob[26..30], &0u32.to_le_bytes(), "a field located");
    assert_eq!(&blob[30..34], &1u32.to_le_bytes(), "b field located");
    blob[26..30].copy_from_slice(&1u32.to_le_bytes());
    blob[30..34].copy_from_slice(&0u32.to_le_bytes());
    assert_eq!(
        restore(4, &blob),
        Err(NetError::Malformed),
        "a > b rejected"
    );
}

#[test]
fn codec108_two_ascending_partitions_round_trip() {
    // The partition ordering guard `if tuple <= prev { Malformed }` only fires
    // with >= 2 partitions. Two strictly-ascending partitions must round-trip; a
    // `<=`→`>` mutant would reject the ascending (valid) pair.
    let mut s = Switch::new(node_map(4), VTime(L0));
    s.set_partition(NodeId(0), NodeId(1), (VTime(10), VTime(50)));
    s.set_partition(NodeId(2), NodeId(3), (VTime(10), VTime(50)));
    let blob = s.save_state();
    assert!(
        restore(4, &blob).is_ok(),
        "two ascending partitions are canonical"
    );
    let mut s2 = Switch::new(node_map(4), VTime(L0));
    s2.restore_state(&blob).unwrap();
    assert_eq!(s2.save_state(), blob);
}

#[test]
fn codec130_two_ascending_throttles_round_trip() {
    // The throttle link-ordering guard `if link <= prev { ... }` — two
    // strictly-ascending links must round-trip; a `<=`→`>` mutant rejects them.
    let mut s = Switch::new(node_map(4), VTime(L0));
    s.set_throttle(
        (NodeId(0), NodeId(1)),
        (2, VTime(100)),
        (VTime(0), VTime(9999)),
    );
    s.set_throttle(
        (NodeId(0), NodeId(2)),
        (2, VTime(100)),
        (VTime(0), VTime(9999)),
    );
    let blob = s.save_state();
    assert!(
        restore(4, &blob).is_ok(),
        "two ascending throttle links are canonical"
    );
    let mut s2 = Switch::new(node_map(4), VTime(L0));
    s2.restore_state(&blob).unwrap();
    assert_eq!(s2.save_state(), blob);
}

#[test]
fn codec130_throttle_count_equal_to_max_round_trips() {
    // `... || count > max` rejects an over-budget count. `count == max` is the
    // admitted-to-the-limit case and is valid; a `>`→`>=` mutant rejects it.
    let mut s = Switch::new(node_map(2), VTime(L0));
    s.set_throttle(
        (NodeId(0), NodeId(1)),
        (2, VTime(1000)),
        (VTime(0), VTime(9999)),
    );
    let mut deliver = FixedOracle(NetAnswer::Deliver);
    // Two admitted sends drive `count` up to `max` (2).
    s.on_tx(VTime(0), frame(0, 1, 1, 4), &mut deliver);
    s.on_tx(VTime(0), frame(0, 1, 2, 4), &mut deliver);
    let blob = s.save_state();
    assert!(
        restore(2, &blob).is_ok(),
        "a throttle at exactly count == max is valid"
    );
    let mut s2 = Switch::new(node_map(2), VTime(L0));
    s2.restore_state(&blob).unwrap();
    assert_eq!(s2.save_state(), blob);
}

#[test]
fn codec156_pending_seq_equal_to_next_seq_is_rejected() {
    // Pending guard: `if key <= prev || seq >= next_seq { Malformed }`. With a
    // single pending (so `key <= prev` is false), a `seq == next_seq` blob is
    // rejected only by the *second* clause — exactly what `||`→`&&` would skip.
    let mut s = Switch::new(node_map(2), VTime(L0));
    let mut deliver = FixedOracle(NetAnswer::Deliver);
    s.on_tx(VTime(0), frame(0, 1, 0x07, 4), &mut deliver); // one pending, seq=0, next_seq=1
    let mut blob = s.save_state();
    assert!(restore(2, &blob).is_ok(), "the canonical blob is valid");

    // Layout (no partitions/throttles): next_seq:u64 @ 14..22; pending[0].seq:u64
    // @ 42..50 (after counts @ 22/26/30 and pending[0].at:u64 @ 34..42).
    let next_seq = u64::from_le_bytes(blob[14..22].try_into().unwrap());
    assert_eq!(next_seq, 1, "next_seq is 1 after one delivery");
    assert_eq!(
        u64::from_le_bytes(blob[42..50].try_into().unwrap()),
        0,
        "the pending seq starts at 0"
    );
    blob[42..50].copy_from_slice(&next_seq.to_le_bytes()); // seq == next_seq → invalid
    assert_eq!(
        restore(2, &blob),
        Err(NetError::Malformed),
        "a pending seq == next_seq must be rejected"
    );
}

#[test]
fn codec169_two_ascending_held_links_round_trip() {
    // Held-buffer link-ordering guard `... || prev_held.is_some_and(|p| link <= p)`
    // — two ascending held links must round-trip; a `<=`→`>` mutant rejects them.
    let mut s = Switch::new(node_map(4), VTime(L0));
    let mut hold = FixedOracle(NetAnswer::Reorder);
    s.on_tx(VTime(0), frame(0, 1, 0xAA, 4), &mut hold); // held on link (0,1)
    s.on_tx(VTime(0), frame(0, 2, 0xBB, 4), &mut hold); // held on link (0,2)
    let blob = s.save_state();
    assert!(
        restore(4, &blob).is_ok(),
        "two ascending held links are canonical"
    );
    let mut s2 = Switch::new(node_map(4), VTime(L0));
    s2.restore_state(&blob).unwrap();
    assert_eq!(s2.save_state(), blob);
}

#[test]
fn codec169_held_link_claiming_zero_frames_is_rejected() {
    // The other half of the same guard: `if nframes == 0 || ... { Malformed }`.
    // A blob whose (correctly-ordered) held link claims nframes == 0 is rejected
    // only by the *first* clause — what `||`→`&&` would skip.
    let mut s = Switch::new(node_map(2), VTime(L0));
    let mut hold = FixedOracle(NetAnswer::Reorder);
    s.on_tx(VTime(0), frame(0, 1, 0xAA, 4), &mut hold); // one held frame on (0,1)
    let mut blob = s.save_state();
    assert!(restore(2, &blob).is_ok(), "the canonical blob is valid");

    // Layout (no partitions/throttles/pending): held count:u32 @ 34..38; held[0]
    // link.0 @ 38..42, link.1 @ 42..46, nframes:u32 @ 46..50, then the frame.
    assert_eq!(&blob[34..38], &1u32.to_le_bytes(), "one held link");
    assert_eq!(&blob[46..50], &1u32.to_le_bytes(), "nframes starts at 1");
    blob[46..50].copy_from_slice(&0u32.to_le_bytes()); // claim nframes == 0
    blob.truncate(50); // drop the now-orphaned frame bytes so the cursor ends clean
    assert_eq!(
        restore(2, &blob),
        Err(NetError::Malformed),
        "a held link with nframes == 0 must be rejected"
    );
}
