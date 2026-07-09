// SPDX-License-Identifier: AGPL-3.0-or-later
//! The [`Server`] seam — the session client's view of a control-transport
//! server, plus the two task-80/81 reply views ([`RegsView`], [`ExecResult`]).
//!
//! `docs/RESOLUTION.md` rules that resolution "forks and reads *directly*
//! against the control-transport server (task 58) … never by tunneling through
//! explorer code": the same verb socket the explorer drives. This trait is that
//! socket, abstracted so the crate gates fully against an **in-crate mock**
//! ([`MockServer`](crate::MockServer)) while a real box connection is a second,
//! wire-speaking implementor handed to the foreman.
//!
//! ## Which verbs use which types
//!
//! The verbs `control-proto` already carries — `hello` / `snapshot` / `drop` /
//! `branch` / `replay` / `run` / `hash` — take and return its **real wire
//! types** ([`control_proto::Environment`], [`StopReason`], [`HashScope`], …),
//! so this client genuinely speaks that contract (`tests/wire.rs` pins the
//! request/reply values against `control-proto`'s codec byte-for-byte). The
//! three verbs tasks 80/81 add but that are unmerged on this branch — `read` /
//! `regs` / `exec` — use the **local** [`RegsView`] / [`ExecResult`] views,
//! shaped exactly as those specs fix them (conventions rule 2). See
//! [`SessionError`] for why their errors live here too.

use control_proto::{Caps, Environment, HashScope, SnapId, StopConditions, StopReason, VTime};
use environment::{EnvSpec, Moment};
use serde::{Deserialize, Serialize};

use crate::SessionError;

/// The result of a [`snapshot`](Server::snapshot): a fresh handle plus the
/// task-81 lineage taint flag. A snapshot captured from a timeline an `exec`
/// improvisation has tainted reports `tainted: true`, so a future
/// Archive/donation path (task 64+) can refuse admission without asking.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Snapshot {
    /// The pool-wide snapshot handle.
    pub id: SnapId,
    /// Whether the captured timeline is tainted (task 81).
    pub tainted: bool,
}

/// The control-transport server as the session client needs it. A single
/// request/reply per method; the two result categories are preserved — a
/// guest-observable outcome is a [`StopReason`] returned `Ok`, every failure is
/// a [`SessionError`].
///
/// Implementors: [`MockServer`](crate::MockServer) (the in-crate scripted
/// loopback, the whole laptop gate) and — post-80/81, foreman-side — a thin
/// adapter over a real `control-proto` socket.
pub trait Server {
    /// Negotiate the session. Must be the first call; returns the server's
    /// [`Caps`]. A mismatch is surfaced by [`Session::connect`](crate::Session::connect)
    /// as [`SessionError::Negotiation`].
    fn hello(&mut self, caps: Caps) -> Result<Caps, SessionError>;

    /// Capture state at the current quiescent point → a [`Snapshot`] handle.
    fn snapshot(&mut self) -> Result<Snapshot, SessionError>;

    /// Release a snapshot handle (corpus GC).
    fn drop_snap(&mut self, snap: SnapId) -> Result<(), SessionError>;

    /// Restore `snap` and reseed from `env` — the explore/materialize path. The
    /// new timeline runs under `env`.
    fn branch(&mut self, snap: SnapId, env: &Environment) -> Result<(), SessionError>;

    /// Restore `snap` verbatim — the reproduce/determinism-gate path.
    fn replay(&mut self, snap: SnapId) -> Result<(), SessionError>;

    /// Advance the VM under `until`. Returns the guest-observable
    /// [`StopReason`] (data, never an error).
    fn run(&mut self, until: StopConditions) -> Result<StopReason, SessionError>;

    /// Canonical state digest over `scope` — the determinism primitive.
    fn hash(&mut self, scope: HashScope) -> Result<[u8; 32], SessionError>;

    /// **Observation** (task 80): read `len` bytes of guest physical memory at
    /// `gpa`. Never mutates guest state, V-time, or any hash. Out-of-range or
    /// oversized → a loud [`SessionError`], never a truncated success.
    fn read(&mut self, gpa: u64, len: u32) -> Result<Vec<u8>, SessionError>;

    /// **Observation** (task 80): the versioned [`RegsView`] at the current
    /// [`Moment`]. Never mutates guest state or any hash.
    fn regs(&mut self) -> Result<RegsView, SessionError>;

    /// **Improvisation** (task 81): inject `cmd` on the guest serial input, run
    /// until a completion sentinel or `deadline`, capture output. Sets the
    /// timeline's taint bit. The server refuses nothing (a caller may
    /// deliberately sacrifice a timeline); the taint bit makes the consequence
    /// structural.
    fn exec(&mut self, cmd: &str, deadline: VTime) -> Result<ExecResult, SessionError>;

    /// Mint the genesis-complete reproducer ([`EnvSpec`]) for the current point
    /// — the task-81 taint guard's fail-loud site: a tainted timeline returns
    /// [`SessionError::Tainted`], never a lying `Environment`.
    fn recorded_env(&mut self) -> Result<EnvSpec, SessionError>;
}

/// A **versioned** register view (task 80): general-purpose registers, `rip`,
/// `rflags`, the segment selectors, the control registers, and the current
/// [`Moment`]/V-time. A *view*, not the save/restore format — additive
/// evolution, no round-trip obligation, so [`version`](RegsView::version) may
/// gain fields without breaking a reader that pins the older shape.
///
/// Serde-serializable so a transcript record captures the full view losslessly.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RegsView {
    /// The view schema version (task-80 additive-evolution contract).
    pub version: u16,
    /// The 16 general-purpose registers in canonical order:
    /// `rax rbx rcx rdx rsi rdi rbp rsp r8 r9 r10 r11 r12 r13 r14 r15`.
    pub gpr: [u64; 16],
    /// The instruction pointer.
    pub rip: u64,
    /// The flags register.
    pub rflags: u64,
    /// The segment selectors `cs ss ds es fs gs`.
    pub seg: [u16; 6],
    /// Control register `cr0`.
    pub cr0: u64,
    /// Control register `cr3` (the page-table base).
    pub cr3: u64,
    /// Control register `cr4`.
    pub cr4: u64,
    /// The current [`Moment`] (retired-instruction count) this view is of.
    pub moment: Moment,
    /// The current effective V-time.
    pub vtime: u64,
}

impl RegsView {
    /// The current [`RegsView`] schema version. Additive-only: a bump adds
    /// fields, never reshapes or drops one.
    pub const VERSION: u16 = 1;
}

/// The result of an [`exec`](Server::exec) improvisation (task 81): the captured
/// serial output, whether the command completed cleanly, and — displayed
/// prominently by the REPL — the timeline's taint state after the injection.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ExecResult {
    /// The serial output captured while the command ran.
    pub output: Vec<u8>,
    /// Whether the command reached its completion sentinel before the deadline.
    pub ok: bool,
    /// The timeline's taint bit after this `exec` — always `true` once any
    /// `exec` has run against the timeline (surfaced so the caller *sees* it).
    pub tainted: bool,
}
