// SPDX-License-Identifier: AGPL-3.0-or-later
//! The [`Switch`]: routing, the V-time delivery schedule, and the fault verbs.
//!
//! State is three deterministic structures plus a monotonic tie-break counter:
//!
//! * `pending` — a `BTreeMap<(VTime, seq), NetDeliver>` of scheduled deliveries,
//!   keyed by `(at, seq)` so [`Switch::due`] drains them in V-time order with
//!   ties broken by `seq` (never by map iteration order).
//! * `held` — the per-link [`NetAnswer::Reorder`] buffer: frames waiting for the
//!   next frame on their link, with a bounded flush horizon.
//! * standing `partitions`/`throttles` — topology faults consulted *before* the
//!   oracle.
//!
//! Every scheduled V-time is computed with saturating `u64` arithmetic, so a
//! hostile `Delay(u64::MAX)` or a `now` near `u64::MAX` clamps to
//! `VTime(u64::MAX)` rather than wrapping into the past or panicking.

use std::collections::{BTreeMap, BTreeSet};

use crate::codec;
use crate::error::NetError;
use crate::parse::parse;
use crate::types::{NetAnswer, NetDeliver, NetOracle, NetSend, NodeMap};
use crate::{NodeId, REORDER_MAX, VTime};

/// A directed link, `(src, dst)`. Used to key the reorder buffer and throttles.
pub(crate) type Link = (NodeId, NodeId);

/// One held-reorder frame: the bytes to deliver and the V-time past which it is
/// flushed even if no later frame arrives.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct HeldFrame {
    pub(crate) frame: Vec<u8>,
    pub(crate) horizon: VTime,
}

/// A standing rate-limit ("clog") on a directed link for a V-time window.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Throttle {
    /// Max frames admitted per `per` V-time, within the active window.
    pub(crate) max: u32,
    /// The fixed-window width in V-time (a `0` width disables the limit).
    pub(crate) per: VTime,
    /// Active half-open V-time window `[start, end)`.
    pub(crate) start: VTime,
    pub(crate) end: VTime,
    /// The fixed-window index currently being counted.
    pub(crate) cur_index: u64,
    /// Frames admitted so far in `cur_index`.
    pub(crate) count: u32,
}

/// The host-side L2 switch and V-time delivery scheduler.
///
/// See the [crate docs](crate) for the model. Construct with [`Switch::new`],
/// feed transmits through [`Switch::on_tx`], drain due deliveries with
/// [`Switch::due`], arm standing faults with
/// [`Switch::set_partition`]/[`Switch::set_throttle`], and snapshot the whole
/// thing with [`Switch::save_state`]/[`Switch::restore_state`].
pub struct Switch {
    pub(crate) nodes: NodeMap,
    pub(crate) l0: VTime,
    /// Standing partitions, each `(a, b, start, end)` with `a <= b` normalized;
    /// the `BTreeSet` keeps them canonical (sorted, deduped) for byte-identical
    /// snapshots.
    pub(crate) partitions: BTreeSet<(NodeId, NodeId, VTime, VTime)>,
    /// Standing throttles, keyed by directed link (re-arming replaces).
    pub(crate) throttles: BTreeMap<Link, Throttle>,
    /// The scheduled deliveries, keyed by `(at, seq)`.
    pub(crate) pending: BTreeMap<(VTime, u64), NetDeliver>,
    /// The per-link reorder hold buffer (FIFO within a link).
    pub(crate) held: BTreeMap<Link, Vec<HeldFrame>>,
    /// The next monotonic tie-break sequence number.
    pub(crate) next_seq: u64,
}

impl Switch {
    /// A switch over `nodes` with base one-way latency `l0` (in V-time units).
    pub fn new(nodes: NodeMap, l0: VTime) -> Self {
        Self {
            nodes,
            l0,
            partitions: BTreeSet::new(),
            throttles: BTreeMap::new(),
            pending: BTreeMap::new(),
            held: BTreeMap::new(),
            next_seq: 0,
        }
    }

    /// Handle one transmit at V-time `now`: parse, consult standing faults then
    /// the oracle (per destination), and enqueue `0..N` deliveries into the
    /// schedule. Returns the deliveries this call enqueued, in `(at, seq)` order.
    ///
    /// Deterministic given `(now, frame, oracle state, switch state)`. A frame
    /// whose [`parse`] fails (truncated/malformed guest bytes) is **dropped** —
    /// this returns an empty `Vec`, never panicking (it is the guest-controlled
    /// TX entry point; conventions rule 4). A broadcast frame fans out to every
    /// other node, each its own decision, visited in **sorted `NodeId` order** so
    /// the per-destination oracle draws happen in a fixed order.
    pub fn on_tx(
        &mut self,
        now: VTime,
        frame: Vec<u8>,
        oracle: &mut dyn NetOracle,
    ) -> Vec<NetDeliver> {
        let Some(hdr) = parse(&frame, &self.nodes) else {
            return Vec::new(); // malformed → drop
        };

        let mut keys: Vec<(VTime, u64)> = Vec::new();
        if hdr.broadcast {
            // Collect first so the NodeMap borrow ends before we mutate `self`.
            let dsts: Vec<NodeId> = self.nodes.node_ids().filter(|&n| n != hdr.src).collect();
            for dst in dsts {
                self.route_one(now, hdr.src, dst, hdr.conn, &frame, oracle, &mut keys);
            }
        } else {
            self.route_one(now, hdr.src, hdr.dst, hdr.conn, &frame, oracle, &mut keys);
        }

        // Return clones in schedule order (ties by seq), matching `due`.
        keys.sort_unstable();
        keys.iter()
            .filter_map(|k| self.pending.get(k).cloned())
            .collect()
    }

    /// Route one (src, dst) frame: apply the standing faults or the oracle, then
    /// release any frames that were held on this link before this send.
    #[allow(clippy::too_many_arguments)] // a private helper threading the per-send context
    fn route_one(
        &mut self,
        now: VTime,
        src: NodeId,
        dst: NodeId,
        conn: crate::ConnId,
        frame: &[u8],
        oracle: &mut dyn NetOracle,
        keys: &mut Vec<(VTime, u64)>,
    ) {
        let link: Link = (src, dst);
        // How many frames were held *before* this send — only these are released
        // by it (a current `Reorder` is held for a future frame to release).
        let held_before = self.held.get(&link).map_or(0, Vec::len);
        let at_nominal = sat_add(now, self.l0);

        // A released `Reorder` frame must deliver *after* this (the next) frame on
        // the link. `seq` only tie-breaks at an equal `at`, so the held frame is
        // anchored at this frame's **actual** scheduled time (the latest of its
        // own deliveries — `at_nominal`, or `at_nominal + d` if it was delayed),
        // not merely the nominal arrival. With no delivery (drop/reorder/standing
        // fault) there is nothing to follow, so it falls back to `at_nominal`.
        let mut release_at = at_nominal;

        // Standing faults take precedence and skip the oracle entirely.
        if self.is_partitioned(src, dst, now) || self.throttle_blocks(src, dst, now) {
            // Dropped — schedule nothing for the current frame.
        } else {
            let len = u32::try_from(frame.len()).unwrap_or(u32::MAX);
            match oracle.decide_send(
                now,
                &NetSend {
                    src,
                    dst,
                    conn,
                    len,
                },
            ) {
                NetAnswer::Deliver => self.schedule(dst, frame.to_vec(), at_nominal, keys),
                NetAnswer::Drop => {}
                NetAnswer::Delay(d) => {
                    let at = sat_add(at_nominal, d);
                    self.schedule(dst, frame.to_vec(), at, keys);
                    release_at = at;
                }
                NetAnswer::Dup => {
                    self.schedule(dst, frame.to_vec(), at_nominal, keys);
                    self.schedule(dst, frame.to_vec(), at_nominal, keys);
                }
                NetAnswer::Corrupt { offset, xor } => {
                    self.schedule(dst, corrupt(frame, offset, xor), at_nominal, keys);
                }
                NetAnswer::Reorder => {
                    let horizon = sat_add(at_nominal, REORDER_MAX);
                    self.held.entry(link).or_default().push(HeldFrame {
                        frame: frame.to_vec(),
                        horizon,
                    });
                }
            }
        }

        // Release the pre-existing held frames. A frame whose horizon has already
        // passed (`horizon <= now`, e.g. this next frame arrives after the bounded
        // reorder horizon and `due` was not called in between) is delivered at its
        // **horizon** — its bounded-horizon contract — not re-anchored after this
        // late next frame. A live frame delivers *after* this frame's own
        // deliveries (which already took the smaller seqs), anchored at this
        // frame's actual scheduled time `release_at`.
        if held_before > 0
            && let Some(buf) = self.held.get_mut(&link)
        {
            let released: Vec<HeldFrame> = buf.drain(0..held_before).collect();
            if buf.is_empty() {
                self.held.remove(&link);
            }
            for hf in released {
                let at = if hf.horizon <= now {
                    hf.horizon
                } else {
                    release_at
                };
                self.schedule(dst, hf.frame, at, keys);
            }
        }
    }

    /// Insert one delivery, assigning the next monotonic `seq`.
    fn schedule(&mut self, dst: NodeId, frame: Vec<u8>, at: VTime, keys: &mut Vec<(VTime, u64)>) {
        let seq = self.next_seq;
        // Unreachable wrap (2^64 sends); `wrapping_add` keeps the path panic-free.
        self.next_seq = self.next_seq.wrapping_add(1);
        let key = (at, seq);
        self.pending.insert(key, NetDeliver { dst, frame, at });
        keys.push(key);
    }

    /// Is `(src, dst)` partitioned at `now`? Partitions are undirected; the
    /// window is half-open `[start, end)`.
    fn is_partitioned(&self, src: NodeId, dst: NodeId, now: VTime) -> bool {
        let (a, b) = norm(src, dst);
        self.partitions
            .iter()
            .any(|&(pa, pb, start, end)| pa == a && pb == b && start <= now && now < end)
    }

    /// Is `(src, dst)` over its throttle budget at `now`? Consumes one slot when
    /// admitting (under budget); over-budget frames are clogged (dropped).
    fn throttle_blocks(&mut self, src: NodeId, dst: NodeId, now: VTime) -> bool {
        let Some(t) = self.throttles.get_mut(&(src, dst)) else {
            return false;
        };
        if now < t.start || now >= t.end || t.per.0 == 0 {
            return false; // inactive window or degenerate width → no limit
        }
        let idx = (now.0 - t.start.0) / t.per.0;
        if idx != t.cur_index {
            t.cur_index = idx;
            t.count = 0;
        }
        if t.count < t.max {
            t.count = t.count.saturating_add(1);
            false
        } else {
            true
        }
    }

    /// Pop every delivery due at or before `now`, in `(at, seq)` order (the
    /// frontier drains these into RX rings). Also flushes any held-reorder frame
    /// whose horizon has passed, so a last-frame reorder is never stranded.
    pub fn due(&mut self, now: VTime) -> Vec<NetDeliver> {
        self.flush_reorder_horizon(now);

        // Split out the due half: keys `< (now+1, 0)` i.e. `at <= now`.
        let due_map = match now.0.checked_add(1) {
            Some(next) => {
                let future = self.pending.split_off(&(VTime(next), 0));
                std::mem::replace(&mut self.pending, future)
            }
            None => std::mem::take(&mut self.pending), // now == u64::MAX → all due
        };
        due_map.into_values().collect()
    }

    /// Flush held-reorder frames whose horizon `<= now` into the pending map (so
    /// they drain in this `due` call). Links visited in sorted order and FIFO
    /// within a link, so the flush `seq`s are assigned deterministically.
    fn flush_reorder_horizon(&mut self, now: VTime) {
        let mut flush: Vec<(NodeId, Vec<u8>, VTime)> = Vec::new();
        for (&(_, dst), buf) in self.held.iter_mut() {
            let mut kept = Vec::with_capacity(buf.len());
            for hf in std::mem::take(buf) {
                if hf.horizon <= now {
                    flush.push((dst, hf.frame, hf.horizon));
                } else {
                    kept.push(hf);
                }
            }
            *buf = kept;
        }
        self.held.retain(|_, buf| !buf.is_empty());
        for (dst, frame, at) in flush {
            let seq = self.next_seq;
            self.next_seq = self.next_seq.wrapping_add(1);
            self.pending
                .insert((at, seq), NetDeliver { dst, frame, at });
        }
    }

    /// Arm a standing partition between nodes `a` and `b` for the half-open
    /// V-time window `[window.0, window.1)`. While active, matching sends (either
    /// direction) are dropped without consulting the oracle.
    ///
    /// Provenance/determinism: a partition's reproducer is the recorded standing
    /// schedule (task 24). On `branch` the frontier re-arms it identically by
    /// calling this; on `replay` the standing state is restored verbatim via
    /// [`Switch::restore_state`] — never armed out-of-band.
    pub fn set_partition(&mut self, a: NodeId, b: NodeId, window: (VTime, VTime)) {
        let (a, b) = norm(a, b);
        self.partitions.insert((a, b, window.0, window.1));
    }

    /// Arm a standing throttle ("clog") on the directed `link` for the half-open
    /// window `[window.0, window.1)`: admit at most `max_per.0` frames per
    /// `max_per.1` V-time, dropping the rest. Re-arming the same link replaces
    /// the prior throttle (and resets its counter).
    pub fn set_throttle(&mut self, link: Link, max_per: (u32, VTime), window: (VTime, VTime)) {
        self.throttles.insert(
            link,
            Throttle {
                max: max_per.0,
                per: max_per.1,
                start: window.0,
                end: window.1,
                cur_index: 0,
                count: 0,
            },
        );
    }

    /// Serialize the whole switch state — pending deliveries, standing
    /// faults/throttles, the held-reorder buffer, and the next monotonic `seq` —
    /// to a byte-deterministic blob. Equal state encodes to identical bytes.
    ///
    /// The [`NodeMap`] and `l0` are config, reconstructed by the integrator
    /// before restore, but `l0` is carried too so a restored switch schedules
    /// identically without relying on matching construction.
    pub fn save_state(&self) -> Vec<u8> {
        codec::encode(self)
    }

    /// Restore state produced by [`Switch::save_state`] onto this switch (its
    /// [`NodeMap`] is left intact). Strict and total: a malformed blob yields
    /// [`NetError::Malformed`] and never panics (conventions rule 4).
    pub fn restore_state(&mut self, b: &[u8]) -> Result<(), NetError> {
        codec::decode_into(self, b)
    }
}

/// Normalize an unordered node pair so a partition is direction-independent.
fn norm(a: NodeId, b: NodeId) -> (NodeId, NodeId) {
    if a <= b { (a, b) } else { (b, a) }
}

/// Saturating V-time addition.
pub(crate) fn sat_add(a: VTime, b: VTime) -> VTime {
    VTime(a.0.saturating_add(b.0))
}

/// Apply a `Corrupt` answer: XOR the byte at `offset % len`. An empty frame is a
/// no-op (`offset % 0` is undefined, so it is never computed) — conventions
/// rule 4.
fn corrupt(frame: &[u8], offset: u32, xor: u8) -> Vec<u8> {
    let mut out = frame.to_vec();
    if out.is_empty() {
        return out;
    }
    let idx = (offset as usize) % out.len();
    out[idx] ^= xor;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupt_empty_frame_is_a_noop() {
        // `offset % 0` is never computed; the operation is a safe no-op.
        assert!(corrupt(&[], 7, 0xFF).is_empty());
    }

    #[test]
    fn corrupt_reduces_offset_modulo_len() {
        let f = [0x00, 0x11, 0x22, 0x33];
        // In range.
        assert_eq!(corrupt(&f, 1, 0xFF), [0x00, 0xEE, 0x22, 0x33]);
        // Out of range wraps: offset 6 % 4 == 2.
        assert_eq!(corrupt(&f, 6, 0xFF), [0x00, 0x11, 0xDD, 0x33]);
        // u32::MAX % 4 == 3 — never an out-of-bounds index.
        assert_eq!(corrupt(&f, u32::MAX, 0x0F), [0x00, 0x11, 0x22, 0x3C]);
    }

    #[test]
    fn sat_add_clamps_at_u64_max() {
        assert_eq!(sat_add(VTime(10), VTime(5)), VTime(15));
        assert_eq!(sat_add(VTime(u64::MAX), VTime(1)), VTime(u64::MAX));
    }

    #[test]
    fn norm_orders_the_pair() {
        assert_eq!(norm(NodeId(3), NodeId(1)), (NodeId(1), NodeId(3)));
        assert_eq!(norm(NodeId(1), NodeId(3)), (NodeId(1), NodeId(3)));
    }
}
