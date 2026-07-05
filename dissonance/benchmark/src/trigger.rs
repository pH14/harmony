// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **toy trigger predicates** — a pure, deterministic model of each bug's
//! fire condition over an opaque `(seed, fault schedule)` [`Scenario`].
//!
//! This is the portable stand-in for the guest payloads (spec gate 1: *trigger
//! logic unit-tested against the mock/toy path — the trigger schedule fires
//! 100%, a nominal scenario never*). It mirrors `dissonance/conductor`'s
//! `ToyPlantedMachine` for bug 1, and models bugs 2 and 3 in the same shape, so
//! the ground-truth predicate the correlation report is validated against is the
//! exact contract the guest C payloads implement.
//!
//! None of this is randomness: the rare-entropy draw is the `SeededEntropy`
//! xorshift64* stream (the exact function the guest's RDRAND returns — see
//! [`entropy_draw`]), so a fixed scenario fires identically every time and the
//! guest and this model agree on which seeds fire.

use crate::manifest::{BugSpec, TriggerParams};
use serde::{Deserialize, Serialize};

/// One perturbation on the fault schedule the campaign searches. The two kinds
/// map to task-59 host-fault vocabulary (`CorruptMemory`, `InjectInterrupt`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum FaultKind {
    /// A single-event memory upset: flip `mask` at guest-physical `gpa`.
    CorruptMemory {
        /// Guest-physical address of the corrupted word.
        gpa: u64,
        /// The XOR bit pattern applied.
        mask: u64,
    },
    /// An injected interrupt of the given vector.
    InjectInterrupt {
        /// The interrupt vector delivered.
        vector: u8,
    },
}

/// A perturbation staged at a `Moment` (retired-branch count).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Perturbation {
    /// The Moment the perturbation is applied at (exact-arrival, task 59).
    pub at: u64,
    /// What is applied.
    pub kind: FaultKind,
}

/// A campaign branch's input: the run seed and its staged fault schedule. The
/// campaign searches the space of these; the finder never sees the trigger.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Scenario {
    /// The run seed (drives the guest's entropy draws).
    pub seed: u64,
    /// The staged fault schedule, in `Moment` order.
    pub faults: Vec<Perturbation>,
}

/// The deterministic entropy draw a run derives from its seed — the toy model of
/// the guest's `gen_random_uuid()` (task 42).
///
/// **This MUST be the identical function the guest sees.** The guest reads its
/// entropy from the VMM's `RDRAND` intercept, which returns the first word of
/// `SeededEntropy::new(EnvSpec.seed)` — the reference **xorshift64\*** stream in
/// `consonance/hypercall-proto` (`ENTROPY_MUL = 0x2545_F491_4F6C_DD1D`, zero-seed
/// fallback `0x9E37_79B9_7F4A_7C15`). This function replicates exactly that first
/// word, so `guest RDRAND == entropy_draw(seed)` and the guest and the model agree
/// on which seeds fire bug 3 (the round-3 review's stream-matching fix — an
/// earlier `splitmix64` model disagreed with the guest's stream). The pinned
/// known-answer test guards the replication against drift.
pub fn entropy_draw(seed: u64) -> u64 {
    // xorshift64* over the normalized seed — byte-for-byte the hypercall-proto
    // `SeededEntropy::next()` first word (the value the guest's RDRAND returns).
    let mut state = if seed == 0 {
        0x9E37_79B9_7F4A_7C15
    } else {
        seed
    };
    state ^= state >> 12;
    state ^= state << 25;
    state ^= state >> 27;
    state.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

/// Whether `scenario` fires `spec`'s planted bug. Pure, total, panic-free.
///
/// * `FaultTiming` — a `CorruptMemory` of the exact `(gpa, mask)` at a Moment in
///   the sensitive window (identical to `conductor::planted::Trigger::fires`).
/// * `OrderingInterrupt` — an `InjectInterrupt` of the exact `vector` at a Moment
///   in the vulnerable window.
/// * `RareEntropy` — the seed-derived draw matches `prefix` in its top
///   `prefix_bits` bits.
pub fn fires(spec: &BugSpec, scenario: &Scenario) -> bool {
    match spec.trigger {
        TriggerParams::FaultTiming { gpa, mask, window } => scenario.faults.iter().any(|p| {
            matches!(p.kind, FaultKind::CorruptMemory { gpa: g, mask: m } if g == gpa && m == mask)
                && window.0 <= p.at
                && p.at < window.1
        }),
        TriggerParams::OrderingInterrupt { vector, window } => scenario.faults.iter().any(|p| {
            matches!(p.kind, FaultKind::InjectInterrupt { vector: v } if v == vector)
                && window.0 <= p.at
                && p.at < window.1
        }),
        TriggerParams::RareEntropy {
            prefix,
            prefix_bits,
        } => prefix_matches(entropy_draw(scenario.seed), prefix, prefix_bits),
    }
}

/// Whether `draw` matches `prefix` in its top `bits` bits. `bits == 0` matches
/// everything (an empty prefix); `bits >= 64` compares the whole word. Panic-free
/// (the `>= 64` shift is guarded).
fn prefix_matches(draw: u64, prefix: u64, bits: u32) -> bool {
    if bits == 0 {
        return true;
    }
    if bits >= 64 {
        return draw == prefix;
    }
    let shift = 64 - bits;
    (draw >> shift) == (prefix >> shift)
}

/// Build a scenario that is **guaranteed** to fire `spec` — the ground-truth
/// positive the 100%-fire gate checks, and what the campaign toy replays to
/// confirm N/N reproduction. For `RareEntropy` this searches deterministically
/// for a hitting seed (always terminates for the manifest's `prefix_bits`).
pub fn triggering_scenario(spec: &BugSpec) -> Scenario {
    match spec.trigger {
        TriggerParams::FaultTiming { gpa, mask, window } => Scenario {
            seed: 0,
            faults: vec![Perturbation {
                at: window.0,
                kind: FaultKind::CorruptMemory { gpa, mask },
            }],
        },
        TriggerParams::OrderingInterrupt { vector, window } => Scenario {
            seed: 0,
            faults: vec![Perturbation {
                at: window.0,
                kind: FaultKind::InjectInterrupt { vector },
            }],
        },
        TriggerParams::RareEntropy {
            prefix,
            prefix_bits,
        } => Scenario {
            seed: seed_hitting_prefix(prefix, prefix_bits),
            faults: Vec::new(),
        },
    }
}

/// The first `seed` (scanning upward from 0) whose entropy draw matches `prefix`
/// in its top `bits` bits. Deterministic; terminates for any `bits` the manifest
/// uses (~`2^bits` iterations expected).
pub fn seed_hitting_prefix(prefix: u64, bits: u32) -> u64 {
    (0u64..)
        .find(|&s| prefix_matches(entropy_draw(s), prefix, bits))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Benchmark;

    /// Gate 1: the trigger schedule fires 100%, and a nominal (empty) scenario
    /// never fires — for every bug in the fixture.
    #[test]
    fn triggering_scenario_fires_and_nominal_never() {
        for bug in Benchmark::wave5().bugs {
            let hit = triggering_scenario(&bug);
            // The crafted scenario fires deterministically, 25× (N/N intent).
            for _ in 0..25 {
                assert!(fires(&bug, &hit), "{} must fire on its trigger", bug.name);
            }
            // A nominal scenario (no faults, seed 0) never fires — except the
            // rare-entropy bug whose trigger is a seed value, so nominal there is
            // "a non-hitting seed". Use seed 0's non-membership where applicable.
            let nominal = Scenario {
                seed: nonhitting_seed(&bug),
                faults: Vec::new(),
            };
            assert!(
                !fires(&bug, &nominal),
                "{} must not fire on a nominal scenario",
                bug.name
            );
        }
    }

    /// A seed known NOT to hit the rare-entropy prefix (and irrelevant to the
    /// fault-timing bugs, which need a fault schedule to fire at all).
    fn nonhitting_seed(bug: &crate::manifest::BugSpec) -> u64 {
        match bug.trigger {
            TriggerParams::RareEntropy {
                prefix,
                prefix_bits,
            } => (0u64..)
                .find(|&s| !prefix_matches(entropy_draw(s), prefix, prefix_bits))
                .unwrap(),
            _ => 0,
        }
    }

    /// Pinned known-answer: `entropy_draw` reproduces the `hypercall-proto`
    /// `SeededEntropy::new(seed).next()` first word exactly (guest RDRAND ==
    /// model). Values computed from that reference xorshift64* stream; a drift in
    /// either constant or shift changes them.
    #[test]
    fn entropy_draw_matches_seeded_entropy_reference() {
        assert_eq!(entropy_draw(0), 0x0d83_b3e2_9a21_487a); // zero-seed fallback
        assert_eq!(entropy_draw(1), 0x47e4_ce4b_896c_dd1d);
        assert_eq!(entropy_draw(2), 0x8fc9_9c97_12d9_ba3a);
        assert_eq!(entropy_draw(42), 0x56ce_4ab7_719b_a3a0);
    }

    /// Near-misses on the fault-timing bug are inert: wrong gpa, wrong bit, and
    /// outside the window all fail to fire (mirrors conductor::planted).
    #[test]
    fn fault_timing_near_misses_do_not_fire() {
        let bench = Benchmark::wave5();
        let bug = bench.get(crate::manifest::BugId(1)).unwrap();
        let TriggerParams::FaultTiming { gpa, mask, window } = bug.trigger else {
            panic!("bug 1 is fault-timing");
        };
        let s = |gpa, mask, at| Scenario {
            seed: 0,
            faults: vec![Perturbation {
                at,
                kind: FaultKind::CorruptMemory { gpa, mask },
            }],
        };
        assert!(fires(bug, &s(gpa, mask, window.0)));
        assert!(!fires(bug, &s(gpa + 0x1000, mask, window.0)), "wrong gpa");
        assert!(!fires(bug, &s(gpa, mask >> 1, window.0)), "wrong bit");
        assert!(
            !fires(bug, &s(gpa, mask, window.1)),
            "past window (hi excl)"
        );
        assert!(!fires(bug, &s(gpa, mask, window.0 - 1)), "before window");
    }

    /// Near-misses on the ordering bug: wrong vector, and outside the window.
    #[test]
    fn ordering_near_misses_do_not_fire() {
        let bench = Benchmark::wave5();
        let bug = bench.get(crate::manifest::BugId(2)).unwrap();
        let TriggerParams::OrderingInterrupt { vector, window } = bug.trigger else {
            panic!("bug 2 is ordering");
        };
        let s = |vector, at| Scenario {
            seed: 0,
            faults: vec![Perturbation {
                at,
                kind: FaultKind::InjectInterrupt { vector },
            }],
        };
        assert!(fires(bug, &s(vector, window.0)));
        assert!(
            fires(bug, &s(vector, window.1 - 1)),
            "last in-window Moment"
        );
        assert!(
            !fires(bug, &s(vector.wrapping_add(1), window.0)),
            "wrong vector"
        );
        assert!(!fires(bug, &s(vector, window.1)), "past window (hi excl)");
    }

    /// The rare-entropy fire rate is ~2^-prefix_bits over a naïve seed sweep — the
    /// property that makes its expected time-to-find dial to ~256 branches.
    #[test]
    fn rare_entropy_fire_rate_matches_prefix() {
        let bench = Benchmark::wave5();
        let bug = bench.get(crate::manifest::BugId(3)).unwrap();
        let TriggerParams::RareEntropy { prefix_bits, .. } = bug.trigger else {
            panic!("bug 3 is rare-entropy");
        };
        // 8-bit prefix ⇒ ~1/256. Over 25_600 seeds expect ~100 hits; allow slack.
        let hits = (0u64..25_600)
            .filter(|&s| {
                fires(
                    bug,
                    &Scenario {
                        seed: s,
                        faults: Vec::new(),
                    },
                )
            })
            .count();
        assert_eq!(prefix_bits, 8);
        assert!(
            (50..=180).contains(&hits),
            "8-bit prefix should hit ~100/25600, got {hits}"
        );
    }
}
