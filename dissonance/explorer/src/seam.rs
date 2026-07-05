// SPDX-License-Identifier: AGPL-3.0-or-later
//! The driver and minting seams the engine codes against (conventions rule 2).
//!
//! [`Machine`]/[`MachineFactory`] is how the explorer drives a deterministic
//! guest as a black box — `branch`/`replay`/`run`/`snapshot`/`drop`/`hash`/
//! coverage — exactly the control-plane verb set of `docs/DISSONANCE.md`. In
//! production a thin R2-socket adapter over `control-proto` implements it; in
//! tests an in-crate toy machine does, so the engine and the determinism gate run
//! both sides unchanged. [`EnvCodec`] is how the schema-blind explorer mints and
//! mutates *valid* [`Environment`] blobs without ever parsing task 24's structure.

use crate::error::MachineError;
use crate::{Answer, Environment, SnapId, StopConditions, StopReason};

/// The control-plane driver the explorer treats as a black box. Every method is
/// fallible with a [`MachineError`] (a transport/backend failure), kept strictly
/// distinct from a guest-observable [`StopReason`].
///
/// The two restore verbs are deliberately split so the reproduce-vs-diverge
/// choice is explicit at every call site (`docs/DISSONANCE.md`, "no bare
/// restore"): [`branch`](Machine::branch) reseeds from an [`Environment`] to
/// explore a new future; [`replay`](Machine::replay) restores verbatim to
/// reproduce.
pub trait Machine {
    /// Restore `snap` and reseed the environment from `env` — explore a new
    /// future. `env`'s overrides are keyed by decision index *since this
    /// branch*.
    fn branch(&mut self, snap: SnapId, env: &Environment) -> Result<(), MachineError>;

    /// Restore `snap` verbatim — reproduce the exact run that was snapshotted
    /// (the determinism / repro path).
    fn replay(&mut self, snap: SnapId) -> Result<(), MachineError>;

    /// Advance until a [`StopReason`]. `resolve` answers the
    /// [`Decision`](StopReason::Decision) the *prior* `run` surfaced (the
    /// suspended hypercall is re-entered with the staged answer); pass `None` to
    /// start a run or to continue past a non-decision stop.
    fn run(
        &mut self,
        until: &StopConditions,
        resolve: Option<&Answer>,
    ) -> Result<StopReason, MachineError>;

    /// Capture state at the current (quiescent) point and return a fresh
    /// [`SnapId`]. Errors with [`MachineError::NotQuiescent`] off a quiescent
    /// point.
    fn snapshot(&mut self) -> Result<SnapId, MachineError>;

    /// Release `snap` (corpus GC). Using a dropped handle afterward is a
    /// [`MachineError::UnknownSnapshot`].
    fn drop_snap(&mut self, snap: SnapId) -> Result<(), MachineError>;

    /// The canonical 32-byte digest of the current state — the determinism
    /// primitive. Equal runs hash equal.
    fn hash(&mut self) -> Result<[u8; 32], MachineError>;

    /// The coverage map for the most recent run (AFL-style edge counts). In
    /// production a view of the shmem region; in the toy machine synthetic. The
    /// explorer reads it for novelty scoring but never interprets its layout.
    fn coverage(&self) -> &[u8];

    /// The reproducer [`Environment`] accumulated over the current Modulation: the
    /// base seed/policy plus the answers resolved since the last
    /// `branch`/`replay`, keyed by decision index since that branch. The machine
    /// owns the blob backing (it mediates every `run(resolve)`), so it — not the
    /// schema-blind explorer — emits the recorded blob; the explorer ferries it
    /// into a [`RunOutcome`](crate::RunOutcome)/[`Frontier`](crate::Frontier)
    /// without parsing it.
    fn recorded_env(&self) -> Result<Environment, MachineError>;
}

/// Spawns fresh [`Machine`]s at their quiescent boot point. The R2 adapter spawns
/// a VM; the toy machine returns a fresh state machine. A higher-level driver
/// uses this to start each campaign; [`Explorer::new`](crate::Explorer::new)
/// itself takes one already-spawned machine.
pub trait MachineFactory {
    /// The machine type produced.
    type M: Machine;
    /// Spawn a fresh machine, quiescent at boot.
    fn spawn(&self) -> Self::M;
}

/// Mints and mutates **valid** [`Environment`] blobs so the explorer stays
/// schema-blind (dissonance task 24 owns the structure). Bound at integration to
/// `EnvSpec`'s codec; the toy machine provides a trivial impl. Without it a
/// production strategy could only emit raw bytes the backend rejects as
/// `BadEnvVersion`/`MalformedEnvironment` and exploration would never leave the
/// toy machine. The strategy *decides* (seed / which override to mutate — that is
/// policy); the codec *encodes* (task 24's structure).
pub trait EnvCodec {
    /// A fresh pure-seeded environment (no overrides) — the explore step's
    /// draw and the empty-frontier / genesis base. Genesis-complete (decision
    /// index zero).
    fn seeded(&self, seed: u64) -> Environment;

    /// A coverage-guided mutation of `base`: decode, tweak the seed or one
    /// override, re-encode — always a *valid* blob the backend accepts, never a
    /// raw byte-flip. `salt` makes the choice deterministic (no wall-clock /
    /// host-RNG).
    fn mutate(&self, base: &Environment, salt: u64) -> Environment;

    /// Compose a genesis-complete `base` with a **branch-local** delta (a
    /// [`Machine::recorded_env`] from a run branched off `base`'s snapshot) into
    /// one genesis-complete [`Environment`], by re-indexing the delta's decision
    /// IDs onto the end of `base`. This is how a [`Bug`](crate::Bug) found below a
    /// non-genesis corpus snapshot still yields a portable, genesis-replayable
    /// reproducer. Deterministic.
    ///
    /// **Contract:** the delta must be [`compose`](Self::compose)-compatible with
    /// `base` — same seed and fault *policy* — or a schema-aware codec rejects it
    /// loudly rather than mint a reproducer that does not replay
    /// ([`SpecEnvCodec`](crate::SpecEnvCodec) panics on a seed/policy mismatch).
    /// [`quiesce`](Self::quiesce) (the probe *branch* env) and
    /// [`rebase_probe_delta`](Self::rebase_probe_delta) (the probe *reproducer*
    /// delta) exist so a forward probe honors this.
    fn compose(&self, base: &Environment, branch_local: &Environment) -> Environment;

    /// A **quiesced** view of `base`: the same *seed* (so a branch-local delta
    /// recorded after branching with it stays [`compose`](Self::compose)-
    /// compatible), but with fault injection **fully stopped** — the fault policy
    /// set to none *and* the concrete schedule (per-`Moment` overrides, standing
    /// faults) stripped — so a run reseeded from it injects no faults and answers
    /// nominally. This is what a directed liveness probe branches with: a copied
    /// policy would keep sampling fresh faults under `StopMask::NONE`, so a probe
    /// could report non-convergence caused by new faults rather than the terminal
    /// state. Genesis-frame, deterministic.
    fn quiesce(&self, base: &Environment) -> Environment;

    /// Re-key the **nominal probe delta** onto `original`'s fault regime so a probe
    /// reproducer [`compose`](Self::compose)s cleanly AND **preserves the original
    /// policy**. The delta was recorded from a policy-none [`quiesce`](Self::quiesce)
    /// branch, so it carries no policy; the reproducer, however, must replay the
    /// *original* faults to reach the terminal state the finding is about — and
    /// those faults are frequently **policy-sampled** (declined decisions, seed +
    /// policy answer), not concrete overrides, so dropping the policy would make
    /// the reproducer never reach the terminal. This gives the delta `original`'s
    /// fault policy (its own recorded schedule untouched) so
    /// `compose(original, rebase_probe_delta(delta, original))` keeps `original`'s
    /// policy in the result. The **branch** env stays [`quiesce`](Self::quiesce)d
    /// (faults stopped) — the two paths differ: quiesce → none, reproducer →
    /// preserve. The default is the identity (a codec whose `compose` does not key
    /// on the policy, e.g. seed-only); a policy-driven codec overrides it.
    fn rebase_probe_delta(&self, delta: &Environment, original: &Environment) -> Environment {
        let _ = original;
        delta.clone()
    }
}
