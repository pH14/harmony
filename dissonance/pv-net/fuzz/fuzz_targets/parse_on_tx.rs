// SPDX-License-Identifier: AGPL-3.0-or-later
//! Acceptance gate 1 — `parse` and `on_tx` (the guest-controlled TX entry point)
//! never panic and never read out of bounds on arbitrary/truncated/mutated
//! bytes. We fuzz **`on_tx` itself**, not only `parse`: a malformed frame must be
//! dropped (empty result), and every reachable scheduling path must stay total.
//! `restore_state` is fuzzed too, since it is the other untrusted-bytes entry.
//!
//! Run (needs the pinned nightly + cargo-fuzz, per the crate IMPLEMENTATION.md):
//!   cargo +nightly-2026-06-16 fuzz run parse_on_tx

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use pv_net::{NetAnswer, NetOracle, NetSend, NodeId, NodeMap, Switch, VTime, parse};

/// A structured fuzz input: enough to drive routing/broadcast deliberately, plus
/// a raw byte string that goes straight into the untrusted entry points.
#[derive(Arbitrary, Debug)]
struct Input {
    now: u64,
    seed: u64,
    src: u8,
    dst: u8,
    broadcast: bool,
    tail: Vec<u8>,
    raw: Vec<u8>,
}

/// Node `n`'s MAC under a fixed 8-node scheme.
fn mac(n: u8) -> [u8; 6] {
    [0x02, 0, 0, 0, 0, n]
}

fn node_map() -> NodeMap {
    let mut m = NodeMap::new();
    for i in 0..8 {
        m.insert_mac(mac(i), NodeId(u32::from(i)));
    }
    m
}

/// A deterministic oracle (LCG) that exercises every `NetAnswer`, seeded from the
/// fuzz input so corruption offsets, delays, reorders, etc. all get hit.
struct LcgOracle(u64);
impl NetOracle for LcgOracle {
    fn decide_send(&mut self, _now: VTime, _s: &NetSend) -> NetAnswer {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        match self.0 % 6 {
            0 => NetAnswer::Deliver,
            1 => NetAnswer::Drop,
            2 => NetAnswer::Delay(VTime(self.0 >> 16)),
            3 => NetAnswer::Dup,
            4 => NetAnswer::Corrupt {
                offset: (self.0 >> 8) as u32,
                xor: self.0 as u8,
            },
            _ => NetAnswer::Reorder,
        }
    }
}

fuzz_target!(|input: Input| {
    let nodes = node_map();

    // 1. parse must tolerate any byte string.
    let _ = parse(&input.raw, &nodes);

    // 2. on_tx on totally arbitrary bytes: a parse failure must drop the frame.
    let mut s = Switch::new(node_map(), VTime(100));
    let mut oracle = LcgOracle(input.seed);
    let out = s.on_tx(VTime(input.now), input.raw.clone(), &mut oracle);
    if parse(&input.raw, &nodes).is_none() {
        assert!(out.is_empty(), "a frame that fails parse must be dropped");
    }

    // 3. on_tx on a deliberately-routable frame, to reach the scheduling paths
    //    (unicast and broadcast, corrupt/delay/dup/reorder via the oracle).
    let dst_mac = if input.broadcast {
        [0xff; 6]
    } else {
        mac(input.dst)
    };
    let mut frame = Vec::with_capacity(12 + input.tail.len());
    frame.extend_from_slice(&dst_mac);
    frame.extend_from_slice(&mac(input.src));
    frame.extend_from_slice(&input.tail);
    let _ = s.on_tx(VTime(input.now), frame, &mut oracle);

    // 4. due drains (including any held-reorder horizon flush) without panicking.
    let _ = s.due(VTime(input.now));
    let _ = s.due(VTime(u64::MAX));

    // 5. snapshot round-trips, and restore tolerates arbitrary bytes.
    let blob = s.save_state();
    let mut restored = Switch::new(node_map(), VTime(100));
    restored.restore_state(&blob).expect("our own blob restores");
    let mut victim = Switch::new(node_map(), VTime(100));
    let _ = victim.restore_state(&input.raw);
});
