// SPDX-License-Identifier: AGPL-3.0-or-later
//! The harmony in-guest fault agent — the portable brain.
//!
//! This library is the target-agnostic half of the fault agent: the workload
//! description it reads at start ([`bundle`]), the decode of a standing-poll
//! answer into per-node faults ([`faults`]), the reconciliation of one answer
//! against the previous one into signals and hook launches ([`supervisor`]),
//! the hook stdout directive protocol ([`directive`]), and the IJON state
//! registers it publishes ([`regs`]). All of it is pure logic with unit tests
//! that run on the macOS dev host; the binary (`src/main.rs`) supplies the
//! Linux glue: the `/dev/harmony` doorbell transport, process spawning with
//! per-node process groups, and process-group signalling.
//!
//! Determinism discipline (conventions rule 4): the agent's behaviour is a
//! function of the host's standing-fault answers and the exit status of the
//! processes it supervises. Nothing here reads a clock or an unseeded RNG, no
//! `HashMap`/`HashSet` exists in the crate, and every collection the agent
//! iterates is kept in a canonical order so two same-seed runs issue the same
//! signals in the same order.

pub mod bundle;
pub mod directive;
pub mod faults;
pub mod recovery;
pub mod regs;
pub mod supervisor;

pub use bundle::{Bundle, BundleError, HookSpec, NodeSpec};
pub use directive::{Directive, DirectiveError, LineReader};
pub use faults::{ActiveFaults, NodeFaults, Park};
pub use recovery::{ReadyHook, RecoveryGate};
pub use regs::{RegisterSnapshot, Registers};
pub use supervisor::{Action, Counters, Supervisor};

/// The sleep the poll loop requests between standing-fault polls, in
/// nanoseconds.
///
/// It is a request, not a period: the guest timer rounds it up to its own
/// granularity (measured at roughly 18.4 ms under `harmony_pvclock`), which is
/// therefore the granularity at which a fault window's edge is observed.
/// Nothing in the agent derives a rate or a duration from a tick count.
pub const TICK_NANOS: u64 = 10_000_000;

/// The source that paces the poll loop.
///
/// A trait rather than a direct sleep call so the poll loop is exercised on the
/// macOS dev host, and so the tick source can be replaced without touching the
/// supervision logic. A wait is allowed to return after longer than it was
/// asked for; the agent treats every wakeup as one tick and never as a
/// duration.
pub trait Clock {
    /// The failure a wait can report.
    type Error;

    /// Block until at least one tick period has elapsed.
    fn wait(&mut self) -> Result<(), Self::Error>;
}
