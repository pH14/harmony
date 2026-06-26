// SPDX-License-Identifier: AGPL-3.0-or-later
//! The public plain-data surface: parsed headers, the address↔node map, the
//! decision seam ([`NetOracle`]/[`NetSend`]/[`NetAnswer`]), and the scheduled
//! delivery ([`NetDeliver`]).

use std::collections::{BTreeMap, BTreeSet};

use crate::{ConnId, NodeId, VTime};

/// The L2/L3/L4 header fields the switch needs for routing and fault targeting,
/// produced by [`parse`](crate::parse).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FrameHdr {
    /// Source MAC (Ethernet bytes `6..12`).
    pub src_mac: [u8; 6],
    /// Destination MAC (Ethernet bytes `0..6`).
    pub dst_mac: [u8; 6],
    /// `dst_mac` is the all-ones broadcast address; the frame fans out to every
    /// other node and `dst` is then a placeholder (it echoes `src`).
    pub broadcast: bool,
    /// Source node, resolved from `src_mac` (or the IPv4 source as a fallback).
    pub src: NodeId,
    /// Destination node, resolved from `dst_mac` (or the IPv4 destination as a
    /// fallback). Meaningless when `broadcast`.
    pub dst: NodeId,
    /// Connection identity for fault targeting (see [`ConnId`]); `0` when the
    /// frame is not IPv4/TCP-or-UDP.
    pub conn: ConnId,
    /// The whole-frame length in bytes (saturated to `u32::MAX`).
    pub len: u32,
}

/// MAC/IP ↔ [`NodeId`] resolution, fixed at config time.
///
/// Backed by `BTreeMap`/`BTreeSet` (never `HashMap`): resolution is a pure
/// lookup and the broadcast fan-out visits nodes in **sorted `NodeId` order**, so
/// no iteration order can reach a scheduling answer (conventions rule 4).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct NodeMap {
    by_mac: BTreeMap<[u8; 6], NodeId>,
    by_ip: BTreeMap<[u8; 4], NodeId>,
    nodes: BTreeSet<NodeId>,
}

impl NodeMap {
    /// An empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `mac -> node` (and `node` as a broadcast target).
    pub fn insert_mac(&mut self, mac: [u8; 6], node: NodeId) {
        self.by_mac.insert(mac, node);
        self.nodes.insert(node);
    }

    /// Register `ip -> node` (and `node` as a broadcast target). IPv4 resolution
    /// is the fallback when a frame's MAC is not registered.
    pub fn insert_ip(&mut self, ip: [u8; 4], node: NodeId) {
        self.by_ip.insert(ip, node);
        self.nodes.insert(node);
    }

    /// Resolve a MAC to its node, if registered.
    pub(crate) fn resolve_mac(&self, mac: &[u8; 6]) -> Option<NodeId> {
        self.by_mac.get(mac).copied()
    }

    /// Resolve an IPv4 address to its node, if registered.
    pub(crate) fn resolve_ip(&self, ip: &[u8; 4]) -> Option<NodeId> {
        self.by_ip.get(ip).copied()
    }

    /// Every registered node, in ascending `NodeId` order (the broadcast
    /// fan-out order).
    pub(crate) fn node_ids(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.nodes.iter().copied()
    }
}

/// What the switch asks the oracle about: one frame's `src`/`dst`/`conn`/`len`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct NetSend {
    /// Source node.
    pub src: NodeId,
    /// Destination node (for a broadcast frame, one fan-out target).
    pub dst: NodeId,
    /// Connection identity (see [`ConnId`]).
    pub conn: ConnId,
    /// Whole-frame length in bytes.
    pub len: u32,
}

/// The non-nominal answer vocabulary, each a deterministic operation on the
/// delivery schedule (for a frame sent at `T`, with base latency `L₀`).
///
/// There is deliberately **no `Partition` variant**: a partition is the
/// *standing* [`Switch::set_partition`](crate::Switch::set_partition) topology
/// policy consulted before the oracle, not a per-send answer — a per-send
/// "partition" would just be [`NetAnswer::Drop`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum NetAnswer {
    /// Nominal: one delivery at `T + L₀`.
    Deliver,
    /// No delivery event.
    Drop,
    /// One delivery at `T + L₀ + d` (saturating).
    Delay(VTime),
    /// Two delivery events (both at `T + L₀`).
    Dup,
    /// One delivery whose bytes differ from the input at exactly
    /// `offset % len`. The `offset` is reduced modulo the frame length before the
    /// XOR, so a recorded/mutated out-of-range `offset` is deterministic and
    /// never panics; an empty frame is a no-op `Deliver` (conventions rule 4).
    Corrupt {
        /// Byte index to flip, taken modulo the frame length.
        offset: u32,
        /// Value XORed into that byte.
        xor: u8,
    },
    /// Hold this frame and deliver it after the *next* frame on this link. If no
    /// later frame ever arrives, the held frame is flushed once at the bounded
    /// reorder horizon `T + L₀ + `[`REORDER_MAX`](crate::REORDER_MAX), so a
    /// last-frame reorder can never strand a Timeline.
    Reorder,
}

/// The decision seam (conventions rule 2). The switch asks for one answer per
/// send; the integrator binds task 24's `Environment` to this. In seeded mode an
/// implementation is a pure PRNG draw — no host round-trip.
pub trait NetOracle {
    /// Decide the fate of one send at V-time `now`.
    fn decide_send(&mut self, now: VTime, s: &NetSend) -> NetAnswer;
}

/// A scheduled delivery the frontier enacts when V-time reaches `at` (it copies
/// `frame` into `dst`'s RX ring and raises the pv-NIC IRQ).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct NetDeliver {
    /// Destination node.
    pub dst: NodeId,
    /// The frame bytes to deliver (already corrupted, if a fault asked for it).
    pub frame: Vec<u8>,
    /// The V-time at which this delivery is due.
    pub at: VTime,
}
