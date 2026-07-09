// SPDX-License-Identifier: AGPL-3.0-or-later
//! The benchmark manifest: the seeded bugs, their distinct classes, per-bug
//! serial markers, and **tunable** trigger thresholds.
//!
//! The manifest is the shared fixture every later beats-baseline gate extends.
//! Task 69 plants three bugs of distinct classes; tasks 72/75 add (iv)
//! partition-duration, (v) depth-2 concurrency, and (vi) convergence/liveness.
//! The [`BugClass`] enum and [`TriggerParams`] are deliberately open (a
//! non-exhaustive class set, a per-class parameter variant) so those slot in
//! **without restructuring** — a new class is a new variant plus a new trigger
//! predicate in [`crate::trigger`], nothing else.
//!
//! Every threshold is a manifest parameter (window width, retry count, prefix
//! length) so a bug's expected naïve time-to-find dials into ~10²–10³ branches —
//! the campaigns must finish on the box (spec).

use serde::{Deserialize, Serialize};

/// A stable per-bug identifier. The serial marker and fingerprint attribute a
/// campaign find to exactly one `BugId`, so the correlation is measured per bug.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct BugId(pub u16);

/// The distinct bug classes in the fixture. **Non-exhaustive**: tasks 72/75 add
/// `PartitionDuration`, `Depth2Concurrency`, and `ConvergenceLiveness` without
/// disturbing the existing three.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub enum BugClass {
    /// (i) Task 60's planted bug, reused verbatim: a single-event `CorruptMemory`
    /// upset of a supervisor ledger word inside a narrow sensitive window.
    FaultTiming,
    /// (ii) An ordering assumption broken only when an `InjectInterrupt`-timing
    /// perturbation lands inside a vulnerable window (a handler that corrupts
    /// shared bookkeeping if preempted mid-update).
    OrderingInterrupt,
    /// (iii) A branch taken only on a rare seeded-entropy value (the task-42
    /// `gen_random_uuid()` prefix-match pattern) that then poisons state and
    /// crashes.
    RareEntropy,
}

/// How a triggered bug is observed as a crash. The guest maps the outcome to a
/// distinct guest terminal (see `guest/linux/campaign-init.sh`); the campaign
/// oracle keys on the terminal **class**, and the per-bug serial marker plus this
/// tag attribute the find. Mirrors `conductor::campaign`'s `CRASH_KIND_*`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[repr(u8)]
pub enum CrashKind {
    /// A guest panic → serial marker → terminal.
    Panic = 0,
    /// A triple fault (`reboot -f` on the container kernel) → `KVM_EXIT_SHUTDOWN`.
    TripleFault = 1,
    /// A clean shutdown request the backend maps to a crash terminal.
    Shutdown = 2,
}

/// The tunable trigger threshold for a bug, one variant per class. Every field is
/// a dial the manifest owns, so the expected time-to-find is set here — not baked
/// into the guest.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub enum TriggerParams {
    /// (i) The single-event upset: the exact ledger `gpa`, the guard-bit `mask`,
    /// and the half-open sensitive `window` `[lo, hi)` in Moments. Matches
    /// `conductor::planted::Trigger`.
    FaultTiming {
        /// Guest-physical address of the supervisor's ledger word.
        gpa: u64,
        /// The one-bit XOR mask that flips the ledger's guard bit.
        mask: u64,
        /// The half-open sensitive Moment window `[lo, hi)`.
        window: (u64, u64),
    },
    /// (ii) The vulnerable window: an `InjectInterrupt` of `vector` landing at a
    /// Moment in `[lo, hi)` preempts the handler mid-update. `window_width =
    /// hi − lo` is the dial on the expected time-to-find.
    OrderingInterrupt {
        /// The interrupt vector whose delivery is unsafe mid-update.
        vector: u8,
        /// The half-open vulnerable Moment window `[lo, hi)`.
        window: (u64, u64),
    },
    /// (iii) The rare-entropy branch: the run's seed-derived draw must match
    /// `prefix` in its top `prefix_bits` bits. The fire probability is
    /// `2^-prefix_bits`, so `prefix_bits` dials the expected time-to-find
    /// (`prefix_bits = 8` ⇒ ~256 branches).
    RareEntropy {
        /// The target value; only its top `prefix_bits` bits are compared.
        prefix: u64,
        /// How many leading bits must match. Fire probability `2^-prefix_bits`.
        prefix_bits: u32,
    },
}

/// One planted bug in the benchmark. Carries everything the campaign and the
/// report need: identity, class, the trigger threshold, the serial marker and
/// crash kind that attribute a find, and the documented naïve expectation.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct BugSpec {
    /// Stable identity.
    pub id: BugId,
    /// A short human name (report headings, IMPLEMENTATION notes).
    pub name: String,
    /// The bug's class.
    pub class: BugClass,
    /// The **distinct** per-bug serial marker the guest prints on trigger, so
    /// fingerprints attribute finds per-bug (spec gate 2).
    pub serial_marker: String,
    /// How the crash is observed.
    pub crash_kind: CrashKind,
    /// The tunable trigger threshold.
    pub trigger: TriggerParams,
    /// The documented expected time-to-find under **naïve** (blind seed) search,
    /// in branches — the baseline the correlation report compares the signal
    /// configuration against.
    pub expected_naive_ttf_branches: u64,
}

/// The benchmark: an ordered set of planted bugs. The report measures correlation
/// per bug and rules over the set.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Benchmark {
    /// The planted bugs, in `BugId` order.
    pub bugs: Vec<BugSpec>,
}

impl Benchmark {
    /// The Wave-5 fixture: the three task-69 bugs of distinct classes. Thresholds
    /// are dialled so each bug's naïve time-to-find sits in ~10²–10³ branches.
    ///
    /// The `BASE`/window constants mirror `conductor::planted` (bug 1 is task
    /// 60's, reused verbatim) so the toy path and the box path agree.
    pub fn wave5() -> Self {
        // Base V-time the supervised guest is quiescent at when snapshotted; the
        // sensitive windows are small offsets past it (see conductor::planted).
        const BASE: u64 = 1_000;
        Benchmark {
            bugs: vec![
                BugSpec {
                    id: BugId(1),
                    name: "fault-timing-crash".to_string(),
                    class: BugClass::FaultTiming,
                    // Task 60's marker (guest/linux/campaign-super.c).
                    serial_marker: "CAMPAIGN_BUG".to_string(),
                    crash_kind: CrashKind::Shutdown,
                    // Exactly conductor::planted::Trigger::toy(): gpa 0x3000, bit
                    // 31, one-slot window at offset 3.
                    trigger: TriggerParams::FaultTiming {
                        gpa: 0x3000,
                        mask: 1 << 31,
                        window: (BASE + 3, BASE + 4),
                    },
                    // The toy search space is 128 points → ~128 branches median.
                    expected_naive_ttf_branches: 128,
                },
                BugSpec {
                    id: BugId(2),
                    name: "ordering-interrupt-window".to_string(),
                    class: BugClass::OrderingInterrupt,
                    serial_marker: "ORDER_BUG".to_string(),
                    // The guest aborts via isa-debug-exit → non-zero exit → /init
                    // reboot → backend `Shutdown` → `Crash{Shutdown}` (identical
                    // terminal path to bug 1's campaign-super.c). The manifest is
                    // the attribution ground truth, so it names the *real* box
                    // terminal, not the aspirational triple-fault (round-7 P2).
                    crash_kind: CrashKind::Shutdown,
                    // A vulnerable window 4 Moments wide; the campaign must land
                    // an InjectInterrupt of vector 0x81 inside it.
                    trigger: TriggerParams::OrderingInterrupt {
                        vector: 0x81,
                        window: (BASE + 8, BASE + 12),
                    },
                    expected_naive_ttf_branches: 256,
                },
                BugSpec {
                    id: BugId(3),
                    name: "rare-entropy-prefix".to_string(),
                    class: BugClass::RareEntropy,
                    serial_marker: "UUID_BUG".to_string(),
                    // The rare branch prints UUID_BUG then dereferences a poisoned
                    // userspace pointer → SIGSEGV → non-zero exit → /init reboot →
                    // backend `Shutdown` → `Crash{Shutdown}` (isa-debug-exit is
                    // unreachable on the container kernel, exactly as for bugs 1/2).
                    // The manifest is the attribution ground truth, so it names the
                    // *real* box terminal, not the aspirational guest `Panic` (the
                    // same round-7 P2 correction already applied to bug 2).
                    crash_kind: CrashKind::Shutdown,
                    // 8-bit prefix ⇒ fire probability 1/256 ⇒ ~256 branches.
                    trigger: TriggerParams::RareEntropy {
                        prefix: 0xA5 << 56,
                        prefix_bits: 8,
                    },
                    expected_naive_ttf_branches: 256,
                },
            ],
        }
    }

    /// Look up a bug by id.
    pub fn get(&self, id: BugId) -> Option<&BugSpec> {
        self.bugs.iter().find(|b| b.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wave5_has_three_distinct_classes() {
        let b = Benchmark::wave5();
        assert_eq!(b.bugs.len(), 3);
        // Distinct classes.
        let mut classes: Vec<_> = b.bugs.iter().map(|x| format!("{:?}", x.class)).collect();
        classes.sort();
        classes.dedup();
        assert_eq!(classes.len(), 3);
        // Distinct serial markers (attribution gate).
        let mut marks: Vec<_> = b.bugs.iter().map(|x| x.serial_marker.clone()).collect();
        marks.sort();
        marks.dedup();
        assert_eq!(marks.len(), 3);
        // Distinct ids.
        assert!(b.get(BugId(1)).is_some());
        assert!(b.get(BugId(2)).is_some());
        assert!(b.get(BugId(3)).is_some());
        assert!(b.get(BugId(9)).is_none());
    }

    #[test]
    fn thresholds_target_findable_range() {
        for bug in Benchmark::wave5().bugs {
            assert!(
                (100..=1000).contains(&bug.expected_naive_ttf_branches),
                "{} naive TTF must sit in 10²–10³ branches",
                bug.name
            );
        }
    }

    #[test]
    fn manifest_roundtrips_json() {
        let b = Benchmark::wave5();
        let s = serde_json::to_string(&b).unwrap();
        let back: Benchmark = serde_json::from_str(&s).unwrap();
        assert_eq!(b, back);
    }
}
