// SPDX-License-Identifier: AGPL-3.0-or-later
//! # pv-net — the host L2 switch + V-time network-fault scheduler
//!
//! In dissonance the "nodes" of a distributed system under test are
//! containers/processes inside **one** deterministic single-vCPU guest, so
//! inter-node traffic is guest-internal. We route every such frame through a
//! `net_tx` hypercall to this **host-side L2 switch**, which therefore sees every
//! frame and is the single point where network faults apply. Host-side
//! enforcement is determinism-clean: decide, enforce, and schedule all happen on
//! the host, in V-time.
//!
//! The load-bearing idea is that **delivery is scheduled in V-time (a
//! branch-count clock, the only deterministic clock) and every network fault is
//! an operation on that schedule.** A frame sent at V-time `T` is delivered at
//! `T + L₀`; `drop` removes the event, `delay(d)` moves it to `T + L₀ + d`,
//! `dup`/`corrupt`/`reorder` double/mutate/reassign it, and a standing
//! `partition` drops on a link for a window. The switch consults a
//! [`NetOracle`] per send to choose the answer; in seeded mode that is a pure
//! PRNG draw, so there is no host round-trip on the hot path.
//!
//! This crate is the switch and the schedule only. It does **not** own the
//! `Environment` (dissonance task 24): it takes a decider through the locally
//! defined [`NetOracle`] trait (conventions rule 2), which the integrator binds
//! to task 24's `Environment`. The `net_tx` hypercall exit handler, the RX ring,
//! the pv-NIC IRQ, and guest-memory frame copies are **frontier** (vmm-core),
//! built later against this crate.
//!
//! ## Determinism discipline
//!
//! Nothing here observes wall-clock time, host entropy, or `HashMap`/`HashSet`
//! iteration order: routing tables are `BTreeMap`/`BTreeSet`-backed, the delivery
//! schedule is a `BTreeMap<(VTime, seq), _>` whose ties are broken by a
//! monotonic `seq` (never by map order), and **all V-time arithmetic saturates**
//! (a mutated/hostile `Delay(u64::MAX)` clamps to [`VTime`]`(u64::MAX)` rather
//! than wrapping into the past or panicking). Library entry points never panic on
//! untrusted bytes ([`parse`] and [`Switch::on_tx`] drop malformed input;
//! [`Switch::restore_state`] returns [`NetError`]).
//!
//! ## Module layout
//!
//! [`mod@error`] (the [`NetError`] enum) · `types` (the public plain data:
//! [`FrameHdr`], [`NodeMap`], [`NetSend`], [`NetAnswer`], [`NetDeliver`],
//! [`NetOracle`]) · `parse` (the panic-free L2/L3/L4 [`parse`]) · `switch` (the
//! [`Switch`] state machine: [`Switch::on_tx`]/[`Switch::due`] and the standing
//! faults) · `codec` ([`Switch::save_state`]/[`Switch::restore_state`]).

mod codec;
mod error;
mod parse;
mod switch;
mod types;

pub use error::NetError;
pub use parse::parse;
pub use switch::Switch;
pub use types::{FrameHdr, NetAnswer, NetDeliver, NetOracle, NetSend, NodeMap};

/// V-time, a count of retired conditional branches — the project's only
/// deterministic clock. Mirrors the integration type (conventions rule 2); the
/// integrator unifies it with `vtime`'s clock. All scheduling arithmetic on it
/// saturates at `u64::MAX`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct VTime(pub u64);

/// An in-guest node (container/process), resolved from a frame's L2/L3 address
/// via the [`NodeMap`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct NodeId(pub u32);

/// A connection identity derived from the L3/L4 5-tuple, used only for fault
/// *targeting* (it is handed to the [`NetOracle`]); it never affects routing.
/// Direction-independent: both halves of a flow map to the same `ConnId`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct ConnId(pub u64);

/// The bounded reorder horizon, in V-time units. A [`NetAnswer::Reorder`] frame
/// with no later frame on its link is flushed exactly once at
/// `T + L₀ + REORDER_MAX` (saturating) by [`Switch::due`], so a last-frame
/// reorder can never strand or hang a Timeline. Fixed constant by design (it is
/// part of the deterministic schedule, not tunable per send); the integrator may
/// re-pick the magnitude, which only changes how long a stranded reorder waits.
pub const REORDER_MAX: VTime = VTime(1 << 20);
