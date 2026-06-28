// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`ToxiproxyEngine`] — the engine we ship, with toxiproxy-shaped toxic
//! semantics. It consults the decider **once per flow, on
//! [`Open`](FlowEvent::Open)**, then enforces that flow's [`FlowPolicy`] on every
//! chunk:
//!
//! - [`Nominal`](FlowPolicy::Nominal): deliver verbatim at the chunk's `at`.
//! - [`Latency(d)`](FlowPolicy::Latency): deliver at `at + d` (saturating).
//! - [`Throttle{bps}`](FlowPolicy::Throttle): pace bytes at `bps` per V-time unit,
//!   per direction.
//! - [`Loss`](FlowPolicy::Loss): drop each chunk with probability `num/den` from a
//!   per-connection seeded PRNG.
//! - [`Reset`](FlowPolicy::Reset): schedule one teardown and drop the rest.
//!
//! All per-flow state lives in plain maps in guest RAM, so consonance snapshots
//! and branches it for free (there is no `save_state`). Determinism comes from
//! the shared `(VTime, seq)` [`Scheduler`] and the seeded PRNG; nothing here reads
//! a wall-clock, a hash-ordered map into an action, or a float.

use std::collections::BTreeMap;

use crate::engine::{FlowDecider, FlowEngine};
use crate::prng::Prng;
use crate::sched::Scheduler;
use crate::{ConnId, Dir, FlowAction, FlowEvent, FlowPolicy, VTime};

/// The live enforcement state of one flow: its decided policy, the latest
/// delivery already scheduled for it (so a close tears down *after* pending
/// data), and whether the flow has already been torn down (a [`Reset`] policy
/// fired, or a close was seen).
#[derive(Clone, Debug)]
struct ConnState {
    /// The per-flow policy with any policy-local state (PRNG, throttle cursors).
    policy: PolicyState,
    /// The latest V-time any delivery has been scheduled for on this flow; a
    /// close's teardown is ordered at or after it.
    last_deliver: VTime,
    /// Once `true`, the flow is torn down: a teardown reset is queued and every
    /// further event on the flow is ignored.
    torn: bool,
}

/// The enforcing form of a [`FlowPolicy`], carrying any state the policy needs to
/// stay deterministic across chunks (a seeded PRNG for loss, per-direction
/// transmit cursors for throttle).
#[derive(Clone, Debug)]
enum PolicyState {
    /// Deliver verbatim at the chunk's arrival V-time.
    Nominal,
    /// Add `d` of (saturating) delay to each chunk's delivery.
    Latency(u64),
    /// Drop each chunk with probability `num/den` from `prng`.
    Loss {
        /// Per-connection drop PRNG, seeded from the decision.
        prng: Prng,
        /// Drop-fraction numerator.
        num: u16,
        /// Drop-fraction denominator (`0` ⇒ no loss).
        den: u16,
    },
    /// Pace bytes at `bps` per V-time unit, one transmit cursor per direction.
    Throttle {
        /// Bytes per V-time unit (`0` ⇒ deliveries clamp to `u64::MAX`).
        bps: u32,
        /// Per-direction "next free" transmit V-time, indexed by [`Dir::idx`].
        cursor: [u64; 2],
    },
    /// Tear the flow down at the first event carrying a V-time; drop the rest.
    Reset,
}

impl PolicyState {
    /// Build the enforcing state for a freshly decided `policy`.
    fn from_policy(policy: FlowPolicy) -> Self {
        match policy {
            FlowPolicy::Nominal => PolicyState::Nominal,
            FlowPolicy::Latency(d) => PolicyState::Latency(d.0),
            FlowPolicy::Loss { seed, num, den } => PolicyState::Loss {
                prng: Prng::new(seed),
                num,
                den,
            },
            FlowPolicy::Throttle { bps } => PolicyState::Throttle {
                bps,
                cursor: [0, 0],
            },
            FlowPolicy::Reset => PolicyState::Reset,
        }
    }
}

/// The engine we ship: toxiproxy toxic semantics over the shared deterministic
/// scheduler. Consult the decider once per flow on [`Open`](FlowEvent::Open) and
/// enforce the decided [`FlowPolicy`] on every subsequent chunk.
#[derive(Clone, Debug)]
pub struct ToxiproxyEngine {
    /// Per-flow enforcement state, keyed by [`ConnId`]. A `BTreeMap` (not a
    /// `HashMap`): the key order is deterministic, though only the `(VTime, seq)`
    /// scheduler order ever reaches an emitted action.
    conns: BTreeMap<u64, ConnState>,
    /// The V-time-ordered queue every action drains through.
    sched: Scheduler,
}

impl ToxiproxyEngine {
    /// A fresh engine with no live flows and an empty action queue.
    pub fn new() -> Self {
        Self {
            conns: BTreeMap::new(),
            sched: Scheduler::new(),
        }
    }

    /// Handle a chunk under an already-decided flow. Splits out of `on_event` so
    /// the per-policy logic reads top-to-bottom.
    fn on_chunk(&mut self, conn: ConnId, dir: Dir, at: VTime, bytes: Vec<u8>) {
        let Some(state) = self.conns.get_mut(&conn.0) else {
            // Stray chunk for an unknown flow: ignore deterministically.
            return;
        };
        if state.torn {
            // Flow already torn down: drop everything that follows.
            return;
        }
        match &mut state.policy {
            PolicyState::Nominal => {
                Self::deliver(
                    &mut self.sched,
                    &mut state.last_deliver,
                    conn,
                    dir,
                    bytes,
                    at,
                );
            }
            PolicyState::Latency(d) => {
                let when = VTime(at.0.saturating_add(*d));
                Self::deliver(
                    &mut self.sched,
                    &mut state.last_deliver,
                    conn,
                    dir,
                    bytes,
                    when,
                );
            }
            PolicyState::Loss { prng, num, den } => {
                if !loss_drops(prng, *num, *den) {
                    Self::deliver(
                        &mut self.sched,
                        &mut state.last_deliver,
                        conn,
                        dir,
                        bytes,
                        at,
                    );
                }
                // A drop schedules nothing — the chunk simply never arrives.
            }
            PolicyState::Throttle { bps, cursor } => {
                let when = throttle_at(&mut cursor[dir.idx()], at.0, bytes.len(), *bps);
                Self::deliver(
                    &mut self.sched,
                    &mut state.last_deliver,
                    conn,
                    dir,
                    bytes,
                    VTime(when),
                );
            }
            PolicyState::Reset => {
                // First event with a V-time on a Reset flow: tear down here, drop
                // this chunk and everything after it.
                self.sched.schedule(FlowAction::Reset { conn, at });
                state.torn = true;
            }
        }
    }

    /// Schedule a delivery and advance the flow's `last_deliver` watermark.
    fn deliver(
        sched: &mut Scheduler,
        last_deliver: &mut VTime,
        conn: ConnId,
        dir: Dir,
        bytes: Vec<u8>,
        at: VTime,
    ) {
        if at > *last_deliver {
            *last_deliver = at;
        }
        sched.schedule(FlowAction::Deliver {
            conn,
            dir,
            bytes,
            at,
        });
    }
}

impl Default for ToxiproxyEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl FlowEngine for ToxiproxyEngine {
    fn on_event(&mut self, ev: FlowEvent, decider: &mut dyn FlowDecider) {
        match ev {
            FlowEvent::Open { conn, src, dst } => {
                // Exactly once per flow: a duplicate Open for a known flow never
                // re-consults the decider (acceptance gate 5).
                if self.conns.contains_key(&conn.0) {
                    return;
                }
                let policy = decider.decide_flow(conn, src, dst);
                self.conns.insert(
                    conn.0,
                    ConnState {
                        policy: PolicyState::from_policy(policy),
                        last_deliver: VTime(0),
                        torn: false,
                    },
                );
            }
            FlowEvent::Chunk {
                conn,
                dir,
                at,
                bytes,
            } => self.on_chunk(conn, dir, at, bytes),
            FlowEvent::Close { conn, at } => {
                let Some(state) = self.conns.get_mut(&conn.0) else {
                    // Stray close for an unknown flow: ignore.
                    return;
                };
                if state.torn {
                    // Already torn down (Reset policy, or a prior close): nothing
                    // more to do.
                    return;
                }
                // Tear down after any still-pending delivery so the reset never
                // precedes delivered data for this flow.
                let when = VTime(at.0.max(state.last_deliver.0));
                self.sched.schedule(FlowAction::Reset { conn, at: when });
                state.torn = true;
            }
        }
    }

    fn due(&mut self, now: VTime) -> Vec<FlowAction> {
        self.sched.due(now)
    }
}

/// Roll the per-connection PRNG once and report whether this chunk is dropped.
/// Drops with probability `num/den`; `num >= den` always drops (`1/1` is a full
/// drop); `den == 0` is treated as no loss. Exactly one draw is consumed per
/// chunk regardless, so the stream stays aligned to the chunk count.
fn loss_drops(prng: &mut Prng, num: u16, den: u16) -> bool {
    let roll = prng.next_u64();
    if den == 0 {
        // Malformed fraction: deliver (matches environment's den==0 = no-op).
        return false;
    }
    // `roll % den < den <= u16::MAX`, so the cast back to u16 is lossless.
    let r = (roll % u64::from(den)) as u16;
    r < num
}

/// The delivery V-time for a throttled chunk: the later of its arrival and the
/// direction's transmit cursor, advanced by the chunk's transmit cost
/// (`ceil(len / bps)` V-time units). `bps == 0` yields a `u64::MAX` cost (the
/// flow is fully stalled) rather than dividing by zero. The cursor is advanced to
/// the returned time so the next chunk on this direction queues behind it.
fn throttle_at(cursor: &mut u64, at: u64, len: usize, bps: u32) -> u64 {
    let start = at.max(*cursor);
    let cost = if bps == 0 {
        u64::MAX
    } else {
        (len as u64).div_ceil(u64::from(bps))
    };
    let finish = start.saturating_add(cost);
    *cursor = finish;
    finish
}
