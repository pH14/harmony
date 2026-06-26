// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared test helpers: a MAC/node scheme, frame builders, and the oracle
//! flavors the gate tests drive the switch with. Each `tests/*.rs` pulls in only
//! what it needs, so some helpers are unused per binary.
#![allow(dead_code)]

use proptest::prelude::ProptestConfig;
use pv_net::{NetAnswer, NetOracle, NetSend, NodeId, NodeMap, VTime};

/// The all-ones broadcast destination MAC.
pub const BROADCAST: [u8; 6] = [0xff; 6];

/// Node `n`'s MAC under the test scheme (locally-administered, unicast).
pub fn mac(n: u8) -> [u8; 6] {
    [0x02, 0, 0, 0, 0, n]
}

/// A [`NodeMap`] of nodes `0..n` keyed by [`mac`].
pub fn node_map(n: u32) -> NodeMap {
    let mut m = NodeMap::new();
    for i in 0..n {
        m.insert_mac(mac(i as u8), NodeId(i));
    }
    m
}

/// A minimal non-IPv4 Ethernet frame (so `conn == 0`) from `src` to `dst` with a
/// `fill`-byte payload of `payload_len` bytes.
pub fn eth_frame(dst: [u8; 6], src: [u8; 6], fill: u8, payload_len: usize) -> Vec<u8> {
    let mut f = Vec::with_capacity(14 + payload_len);
    f.extend_from_slice(&dst);
    f.extend_from_slice(&src);
    f.extend_from_slice(&[0x88, 0xb5]); // local-experimental ethertype (not IPv4)
    f.resize(14 + payload_len, fill);
    f
}

/// Unicast frame from node `s` to node `d`.
pub fn frame(s: u8, d: u8, fill: u8, payload_len: usize) -> Vec<u8> {
    eth_frame(mac(d), mac(s), fill, payload_len)
}

/// Broadcast frame from node `s`.
pub fn bcast(s: u8, fill: u8, payload_len: usize) -> Vec<u8> {
    eth_frame(BROADCAST, mac(s), fill, payload_len)
}

/// Always answers the same.
pub struct FixedOracle(pub NetAnswer);
impl NetOracle for FixedOracle {
    fn decide_send(&mut self, _now: VTime, _s: &NetSend) -> NetAnswer {
        self.0
    }
}

/// Replays a fixed answer script in order; records the sends it was asked about.
pub struct ScriptedOracle {
    pub answers: Vec<NetAnswer>,
    pub idx: usize,
    pub seen: Vec<(VTime, NetSend)>,
}
impl ScriptedOracle {
    pub fn new(answers: Vec<NetAnswer>) -> Self {
        Self {
            answers,
            idx: 0,
            seen: Vec::new(),
        }
    }
}
impl NetOracle for ScriptedOracle {
    fn decide_send(&mut self, now: VTime, s: &NetSend) -> NetAnswer {
        self.seen.push((now, *s));
        let a = self
            .answers
            .get(self.idx)
            .copied()
            .unwrap_or(NetAnswer::Deliver);
        self.idx += 1;
        a
    }
}

/// Records every send and returns a fixed answer (for draw-order assertions).
pub struct RecordingOracle {
    pub answer: NetAnswer,
    pub seen: Vec<(VTime, NetSend)>,
}
impl RecordingOracle {
    pub fn new(answer: NetAnswer) -> Self {
        Self {
            answer,
            seen: Vec::new(),
        }
    }
}
impl NetOracle for RecordingOracle {
    fn decide_send(&mut self, now: VTime, s: &NetSend) -> NetAnswer {
        self.seen.push((now, *s));
        self.answer
    }
}

/// A deterministic seeded oracle (a small LCG) spanning every [`NetAnswer`]
/// variant; records the answers it produced, in order, so a [`ScriptedOracle`]
/// can replay them.
pub struct SeededOracle {
    state: u64,
    pub recorded: Vec<NetAnswer>,
}
impl SeededOracle {
    pub fn new(seed: u64) -> Self {
        Self {
            state: seed ^ 0x9E37_79B9_7F4A_7C15,
            recorded: Vec::new(),
        }
    }
    fn next(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.state
    }
}
impl NetOracle for SeededOracle {
    fn decide_send(&mut self, _now: VTime, _s: &NetSend) -> NetAnswer {
        let r = self.next();
        let ans = match r % 6 {
            0 => NetAnswer::Deliver,
            1 => NetAnswer::Drop,
            2 => NetAnswer::Delay(VTime((r >> 32) % 5000)),
            3 => NetAnswer::Dup,
            4 => NetAnswer::Corrupt {
                offset: (r >> 16) as u32,
                xor: ((r >> 8) as u8) | 1, // non-zero, so it always flips a byte
            },
            _ => NetAnswer::Reorder,
        };
        self.recorded.push(ans);
        ans
    }
}

/// Proptest config: spec case count natively, cut hard under Miri (kept for
/// portability even though this crate has no `unsafe` to scrutinize).
pub fn config(cases: u32) -> ProptestConfig {
    let mut cfg = ProptestConfig::with_cases(if cfg!(miri) { 16 } else { cases });
    if cfg!(miri) {
        cfg.failure_persistence = None;
    }
    cfg
}
