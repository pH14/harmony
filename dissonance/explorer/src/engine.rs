// SPDX-License-Identifier: AGPL-3.0-or-later
//! The engine: [`Explorer`] and the two loops, plus the [`RunOutcome`] of one
//! Timeline and the [`Bug`] one Multiverse step can surface.
//!
//! [`Explorer::timeline`] drives one run to a terminal stop, answering each
//! surfaced decision through the [`Strategy`] and accumulating the reproducer
//! [`Environment`]. [`Explorer::multiverse_step`] picks/mutates an environment,
//! branches, runs one Timeline, scores novelty, admits the snapshot if novel
//! (issuing `drop_snap` for evictions — corpus GC), and rebases any [`Bug`] to
//! genesis before reporting. [`Explorer::explore`] runs the Multiverse for a
//! bounded number of steps.

use std::collections::BTreeSet;

use sha2::{Digest, Sha256};

use crate::corpus::{Corpus, CovScore};
use crate::error::MachineError;
use crate::seam::{EnvCodec, Machine};
use crate::strategy::Strategy;
use crate::{Answer, Environment, SnapId, StopConditions, StopMask, StopReason};

/// The result of one Timeline: where it stopped, the genesis-or-branch-local
/// reproducer [`Environment`] accumulated over it, and the coverage novelty it
/// scored against the corpus at that moment.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RunOutcome {
    /// The terminal [`StopReason`] the Timeline ended at.
    pub stop: StopReason,
    /// The reproducer accumulated over the run ([`Machine::recorded_env`]).
    pub env: Environment,
    /// The coverage novelty scored against the corpus when the run ended.
    pub coverage_novelty: CovScore,
}

/// A snapshot captured mid-run at a [`StopReason::SnapshotPoint`], paired with the
/// **prefix** reproducer and coverage *as of that point* — not the whole run.
/// Admitting this triple keeps a corpus entry's env the genesis-complete
/// reproducer that produced *its snapshot* (so a child branched off it, or a
/// [`Bug`] rebased through it via [`EnvCodec::compose`], keys correctly), and its
/// score the novelty of the path *to* the snapshot.
struct PendingSnapshot {
    snap: SnapId,
    /// `Machine::recorded_env` captured at the SnapshotPoint — the prefix that
    /// produced `snap`, keyed from this Timeline's branch origin.
    env: Environment,
    /// `Machine::coverage` captured at the SnapshotPoint — the edges hit on the
    /// path to `snap`.
    coverage: Vec<u8>,
}

/// A reproducer for a found bug. `env` is **genesis-complete**: `branch(genesis,
/// env)` + re-run reproduces `stop` bit-for-bit. A bug found below a non-genesis
/// corpus snapshot is rebased to genesis on report (the explorer composes the
/// corpus base env with the branch-local delta), because overrides are keyed by
/// decision index *since the branch* and a non-genesis base would mis-key them.
/// The `fingerprint` is a `sha2` digest of the stop reason, used to dedup
/// repeated discoveries of the same bug.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Bug {
    /// A stable digest of the stop reason (for dedup).
    pub fingerprint: [u8; 32],
    /// The genesis-complete reproducer.
    pub env: Environment,
    /// The bug's stop reason (a [`StopReason::Crash`] or
    /// [`StopReason::Assertion`]).
    pub stop: StopReason,
}

/// The exploration engine: a [`Machine`] driven by a [`Strategy`] over a
/// [`Corpus`], minting environments through an [`EnvCodec`]. Owns the genesis
/// snapshot every first-generation Timeline branches from.
pub struct Explorer<M: Machine, S: Strategy> {
    machine: M,
    strategy: S,
    env: Box<dyn EnvCodec>,
    corpus: Corpus,
    genesis: SnapId,
    until: StopConditions,
    /// Snapshots captured mid-run at [`StopReason::SnapshotPoint`]s this Timeline,
    /// each with its prefix env + coverage, awaiting admission by the enclosing
    /// [`multiverse_step`](Explorer::multiverse_step). A `Vec` (not a single slot)
    /// so a Timeline that forks more than once admits/drops *every* snapshot and
    /// never leaks a backend handle.
    pending_snapshots: Vec<PendingSnapshot>,
}

impl<M: Machine, S: Strategy> Explorer<M, S> {
    /// Snapshot the freshly-spawned machine at its quiescent boot point → the
    /// **genesis [`SnapId`]**, the base every first-generation Timeline branches
    /// from (the corpus starts empty, so step 1 has no admitted entry to branch
    /// from). Returns [`Err`] if that initial snapshot fails (e.g. not
    /// quiescent) — never panics or fabricates a base.
    ///
    /// The default [`StopConditions`] surface every decision class and the
    /// snapshot point ([`StopMask::ALL`], no deadline) — the coverage-guided
    /// default. A pure seed-driven campaign sets [`StopMask::NONE`] via
    /// [`set_stop_conditions`](Explorer::set_stop_conditions).
    pub fn new(machine: M, strategy: S, env: Box<dyn EnvCodec>) -> Result<Self, MachineError> {
        let mut machine = machine;
        let genesis = machine.snapshot()?;
        Ok(Self {
            machine,
            strategy,
            env,
            corpus: Corpus::new(),
            genesis,
            until: StopConditions {
                deadline: None,
                on: StopMask::ALL,
            },
            pending_snapshots: Vec::new(),
        })
    }

    /// The genesis snapshot every first-generation Timeline branches from.
    pub fn genesis(&self) -> SnapId {
        self.genesis
    }

    /// The corpus, for inspection.
    pub fn corpus(&self) -> &Corpus {
        &self.corpus
    }

    /// Re-capacitate the corpus — a tuning knob normally applied before
    /// exploration begins. Any currently-kept entries are discarded, so their
    /// snapshots are `drop_snap`'d first (never silently forgotten, which would
    /// leak backend handles). The corpus-GC gate forces eviction at a small
    /// capacity through this.
    pub fn set_corpus_capacity(&mut self, capacity: usize) -> Result<(), MachineError> {
        // Collect snaps first (ends the corpus borrow), then release each.
        let snaps: Vec<SnapId> = (0..self.corpus.len())
            .filter_map(|i| self.corpus.entry(i).map(|(snap, _, _)| snap))
            .collect();
        for snap in snaps {
            self.machine.drop_snap(snap)?;
        }
        self.corpus = Corpus::with_capacity(capacity);
        Ok(())
    }

    /// The [`StopConditions`] used by [`multiverse_step`](Explorer::multiverse_step)
    /// and [`explore`](Explorer::explore).
    pub fn stop_conditions(&self) -> &StopConditions {
        &self.until
    }

    /// Set the [`StopConditions`] the Multiverse drives each Timeline with — e.g.
    /// [`StopMask::NONE`] for a pure seed-driven campaign, or a deadline.
    pub fn set_stop_conditions(&mut self, until: StopConditions) {
        self.until = until;
    }

    /// Direct access to the driven machine, for tests that branch/replay/hash it
    /// outside the loop (e.g. the Timeline-replay gate).
    pub fn machine_mut(&mut self) -> &mut M {
        &mut self.machine
    }

    /// **Inner loop.** Drive one run from `base` to a terminal stop, answering
    /// each surfaced [`StopReason::Decision`] via the strategy and snapshotting
    /// at any [`StopReason::SnapshotPoint`] (stored for the enclosing Multiverse
    /// step to admit). Returns the terminal stop, the accumulated reproducer, and
    /// its coverage novelty.
    pub fn timeline(
        &mut self,
        base: SnapId,
        env: &Environment,
        until: &StopConditions,
    ) -> Result<RunOutcome, MachineError> {
        // Drop any snapshots left pending by a prior *direct* `timeline` call
        // (only `multiverse_step` admits/drains them) so a repeated or aborted
        // direct run never leaks a backend handle — rather than a bare `clear()`
        // that would forget the `SnapId` without `drop_snap`.
        for pending in std::mem::take(&mut self.pending_snapshots) {
            self.machine.drop_snap(pending.snap)?;
        }
        self.machine.branch(base, env)?;
        let mut resolve: Option<Answer> = None;
        loop {
            let stop = self.machine.run(until, resolve.as_ref())?;
            match stop {
                StopReason::Decision { ref ctx, .. } => {
                    // Answer the surfaced decision and feed it back on the next
                    // `run`. Disjoint field borrows: strategy (mut), machine
                    // (shared, for the live coverage map).
                    let answer = self.strategy.choose(ctx, self.machine.coverage());
                    resolve = Some(answer);
                }
                StopReason::SnapshotPoint { .. } => {
                    // Fork point: capture a branchable base for the corpus and
                    // continue the run past it. The env/coverage are captured *now*
                    // (the prefix as of this snapshot), not at the terminal stop —
                    // admitting the whole-run env would mis-key a later branch's
                    // overrides against the snapshot's decision-index origin.
                    let snap = self.machine.snapshot()?;
                    // If capturing the prefix env fails after the snapshot already
                    // succeeded, the handle would leak — release it (best effort,
                    // preserving the original error) before propagating.
                    let prefix_env = match self.machine.recorded_env() {
                        Ok(env) => env,
                        Err(e) => {
                            let _ = self.machine.drop_snap(snap);
                            return Err(e);
                        }
                    };
                    let prefix_coverage = self.machine.coverage().to_vec();
                    self.pending_snapshots.push(PendingSnapshot {
                        snap,
                        env: prefix_env,
                        coverage: prefix_coverage,
                    });
                    resolve = None;
                }
                terminal => {
                    let env = self.machine.recorded_env()?;
                    let coverage_novelty = self.corpus.novelty(self.machine.coverage());
                    return Ok(RunOutcome {
                        stop: terminal,
                        env,
                        coverage_novelty,
                    });
                }
            }
        }
    }

    /// **Outer loop.** One Multiverse step: pick/mutate an environment, branch,
    /// run one Timeline, admit the snapshot if novel (issuing `drop_snap` for
    /// evictions), and return a rebased-to-genesis [`Bug`] if the run crashed or
    /// violated an assertion. A [`MachineError`] aborts the step loudly and is
    /// never reported as a bug.
    pub fn multiverse_step(&mut self) -> Result<Option<Bug>, MachineError> {
        let until = self.until.clone();
        // Disjoint field borrows: strategy (mut), corpus (shared), env (shared).
        let (base_snap, branch_env) =
            self.strategy
                .next_env(&self.corpus, self.genesis, self.env.as_ref());

        let outcome = self.timeline(base_snap, &branch_env, &until)?;

        let bug = if outcome.stop.is_bug() {
            Some(self.report(base_snap, &outcome))
        } else {
            None
        };

        // A snapshot forked below a non-genesis base is captured branch-local to
        // *this* Timeline's `base_snap`; to keep the corpus invariant that every
        // entry's env is genesis-complete, rebase it through that base's
        // (genesis-complete) env exactly as `report` rebases a bug. Capture the
        // base env *before* the admit loop, which may evict it.
        let base_genesis: Option<Environment> = if base_snap == self.genesis {
            None
        } else {
            self.corpus.base_env(base_snap).cloned()
        };

        // Every snapshot this run forked is offered to the corpus with the prefix
        // env/coverage captured at its fork point; a non-novel one is dropped
        // immediately, and any entry evicted by an admission is dropped after (GC).
        // Draining the whole `Vec` means no forked handle is ever leaked.
        for pending in std::mem::take(&mut self.pending_snapshots) {
            let env = if base_snap == self.genesis {
                pending.env
            } else if let Some(base) = &base_genesis {
                self.env.compose(base, &pending.env)
            } else {
                // The base was evicted before we could rebase — we cannot form a
                // genesis-complete entry, so drop the snapshot rather than admit a
                // branch-local one (which would break genesis replay downstream).
                self.machine.drop_snap(pending.snap)?;
                continue;
            };
            let novel = self.corpus.admit(pending.snap, env, &pending.coverage);
            if !novel {
                self.machine.drop_snap(pending.snap)?;
            }
        }
        for evicted in self.corpus.drain_evicted() {
            self.machine.drop_snap(evicted)?;
        }

        Ok(bug)
    }

    /// Run the Multiverse for `steps` steps; return the distinct bugs found
    /// (deduplicated by fingerprint). Any [`MachineError`] aborts the whole
    /// campaign loudly (propagated), exactly as the two-result-categories rule
    /// requires.
    pub fn explore(&mut self, steps: u64) -> Result<Vec<Bug>, MachineError> {
        let mut bugs = Vec::new();
        let mut seen: BTreeSet<[u8; 32]> = BTreeSet::new();
        for _ in 0..steps {
            if let Some(bug) = self.multiverse_step()?
                && seen.insert(bug.fingerprint)
            {
                bugs.push(bug);
            }
        }
        Ok(bugs)
    }

    /// Build a [`Bug`] from a terminal bug stop, rebasing its env to genesis.
    /// A genesis-rooted run's reproducer is already genesis-complete; a run
    /// branched off a corpus snapshot is composed with that snapshot's
    /// genesis-complete base env (re-keying the branch-local overrides).
    fn report(&self, base_snap: SnapId, outcome: &RunOutcome) -> Bug {
        let env = if base_snap == self.genesis {
            outcome.env.clone()
        } else if let Some(base) = self.corpus.base_env(base_snap) {
            self.env.compose(base, &outcome.env)
        } else {
            // The base was evicted before this child reported. Its branch-local
            // env still reproduces from that snapshot, but not from genesis;
            // surfaced as-is rather than dropping a real bug. (Tests size the
            // corpus so live bases are never evicted — see IMPLEMENTATION.md.)
            outcome.env.clone()
        };
        Bug {
            fingerprint: fingerprint(&outcome.stop),
            env,
            stop: outcome.stop.clone(),
        }
    }
}

/// A stable 32-byte digest of a bug stop, so the same crash/assertion dedups
/// across the many environments that reach it. Domain-separated by a leading tag;
/// only the bug-bearing variants are expected, but every variant hashes totally.
fn fingerprint(stop: &StopReason) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"dissonance.explorer.bug.v1");
    match stop {
        StopReason::Crash { vtime, info } => {
            h.update([0xC1]);
            h.update(vtime.0.to_le_bytes());
            h.update(info);
        }
        StopReason::Assertion { vtime, id, data } => {
            h.update([0xA1]);
            h.update(vtime.0.to_le_bytes());
            h.update(id.to_le_bytes());
            h.update(data);
        }
        // Non-bug stops are never fingerprinted in practice; hash their tag so
        // the function stays total.
        StopReason::Deadline { vtime } => {
            h.update([0xD1]);
            h.update(vtime.0.to_le_bytes());
        }
        StopReason::Quiescent { vtime } => {
            h.update([0x01]);
            h.update(vtime.0.to_le_bytes());
        }
        StopReason::Decision { vtime, id, ctx } => {
            h.update([0xDE]);
            h.update(vtime.0.to_le_bytes());
            h.update(id.to_le_bytes());
            h.update(ctx);
        }
        StopReason::SnapshotPoint { vtime } => {
            h.update([0x5A]);
            h.update(vtime.0.to_le_bytes());
        }
    }
    h.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::fingerprint;
    use crate::{StopReason, VTime};

    /// The bug fingerprint is the pinned `sha2` digest of the stop reason — locks
    /// the domain tag + field layout and kills the `[0;32]`/`[1;32]` mutants.
    #[test]
    fn fingerprint_is_a_pinned_digest() {
        let crash = StopReason::Crash {
            vtime: VTime(80),
            info: vec![2, 4],
        };
        let golden: [u8; 32] = [
            0x87, 0x98, 0x12, 0xff, 0x07, 0x03, 0x95, 0x3f, 0x5d, 0x41, 0x10, 0xd9, 0xb7, 0xc9,
            0x06, 0xcc, 0xfc, 0xf9, 0xc2, 0xeb, 0x81, 0x71, 0x5e, 0xd6, 0xaf, 0x1b, 0x5c, 0x21,
            0x5c, 0x23, 0x6e, 0x16,
        ];
        assert_eq!(fingerprint(&crash), golden);
        assert_ne!(fingerprint(&crash), [0u8; 32]);
        assert_ne!(fingerprint(&crash), [1u8; 32]);
    }

    /// Two different stops fingerprint differently (so dedup keeps distinct bugs).
    #[test]
    fn fingerprint_distinguishes_stops() {
        let crash = StopReason::Crash {
            vtime: VTime(80),
            info: vec![2, 4],
        };
        let assertion = StopReason::Assertion {
            vtime: VTime(80),
            id: 5,
            data: vec![3],
        };
        // Different kind, different vtime, and different info each shift the digest.
        assert_ne!(fingerprint(&crash), fingerprint(&assertion));
        assert_ne!(
            fingerprint(&crash),
            fingerprint(&StopReason::Crash {
                vtime: VTime(81),
                info: vec![2, 4],
            })
        );
        assert_ne!(
            fingerprint(&crash),
            fingerprint(&StopReason::Crash {
                vtime: VTime(80),
                info: vec![2, 5],
            })
        );
    }
}
