// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`EnvCodec`] — the vocabulary-aware **proposal seam**. The Theme (the outer
//! search loop) is structurally blind to fault semantics: it cannot *invent* a
//! legal [`HostFault`]/[`Answer`], so it asks the codec to `seeded` a fresh
//! environment, `mutate` an existing one, or `compose` two on the single
//! [`Moment`] axis. All three operate over the merged host+guest override map,
//! and all three are deterministic — `mutate`/`compose` take an explicit
//! salt/offset, so the search stays replayable.

use std::collections::BTreeMap;

use crate::VTime;
use crate::error::EnvError;
use crate::host::{Action, BitMask, HostFault, Moment, Ratio};
use crate::policy::FaultPolicy;
use crate::prng::Prng;
use crate::recorded::{EnvSpec, StandingFault};

/// Domain separation for `mutate`'s PRNG, so a mutation salt and a base seed that
/// happen to coincide do not draw the same stream.
const MUTATE_DOMAIN: u64 = 0x4D75_7461_7465_2121; // "Mutate!!"

/// The proposal seam the Theme calls. A unit type: every operation is a pure
/// function of its inputs, holding no state of its own.
///
/// This is one of the three opaque seams that make the Theme
/// *agnostic-by-interface* (navigation, scoring, **proposal**): vocabulary
/// knowledge lives here, not in the search policy, so adding a fault type grows
/// the codec and never the Theme (the dissonance D-invariant).
#[derive(Clone, Copy, Debug, Default)]
pub struct EnvCodec;

impl EnvCodec {
    /// Propose a pure **seeded** environment: a seed and a [`FaultPolicy`] answer
    /// every decision locally, with no overrides (FoundationDB `BUGGIFY` style).
    pub fn seeded(seed: u64, policy: FaultPolicy) -> EnvSpec {
        EnvSpec::Seeded { seed, policy }
    }

    /// Deterministically **mutate** an environment: one tweak to the merged
    /// override map, selected by `salt` (same `(env, salt)` ⇒ same result). The
    /// mutation operates **only on the host plane** — it inserts, moves, or
    /// removes an [`Action::Host`] override, which carries no admissibility
    /// constraint (a host fault needs no [`DecisionPoint`](crate::DecisionPoint)).
    /// Guest-plane mutation requires the live decision context the explorer
    /// supplies at `decide` time, not this offline codec; therefore **every
    /// [`Action::Guest`] override is preserved verbatim** — `mutate` never
    /// removes, relocates, or overwrites one, so it can never fabricate an
    /// out-of-context guest answer.
    ///
    /// Always returns a [`Recorded`](EnvSpec::Recorded) spec (inserting a host
    /// fault into a [`Seeded`](EnvSpec::Seeded) base promotes it); the base's
    /// seed, policy, and standing faults are preserved.
    pub fn mutate(env: &EnvSpec, salt: u64) -> EnvSpec {
        let mut overrides = env.overrides().clone();
        let standing = match env {
            EnvSpec::Recorded { standing, .. } => standing.clone(),
            EnvSpec::Seeded { .. } => Vec::new(),
        };
        let mut rng = Prng::new(salt ^ MUTATE_DOMAIN);

        // Only host actions are legal move/remove victims. With none present, the
        // only available op is "insert"; otherwise pick insert (0)/remove
        // (1)/move (2).
        let host_keys: Vec<Moment> = overrides
            .iter()
            .filter(|(_, a)| matches!(a, Action::Host(_)))
            .map(|(m, _)| *m)
            .collect();
        let op = if host_keys.is_empty() {
            0
        } else {
            rng.next_u64() % 3
        };
        match op {
            1 => {
                // Remove a host victim (guest actions are never removed).
                let k = host_keys[(rng.next_u64() % host_keys.len() as u64) as usize];
                overrides.remove(&k);
            }
            2 => {
                // Move a host victim to a fresh slot that does not clobber a guest
                // action (it may overwrite another host action, or land free).
                let k = host_keys[(rng.next_u64() % host_keys.len() as u64) as usize];
                if let Some(action) = overrides.remove(&k) {
                    let dst = free_non_guest_slot(&overrides, &mut rng);
                    overrides.insert(dst, action);
                }
            }
            _ => {
                // Insert a fresh host fault, again never clobbering a guest action.
                let dst = free_non_guest_slot(&overrides, &mut rng);
                overrides.insert(dst, Action::Host(host_fault_from(&mut rng)));
            }
        }

        EnvSpec::Recorded {
            seed: env.seed(),
            policy: env.policy().clone(),
            overrides,
            standing,
        }
    }

    /// **Compose** a `base` prefix with a `tail` continuation on the single
    /// [`Moment`] axis — the task-45 acceptance gate: *one-axis `Moment` override
    /// re-keying*. It keeps `base`'s genesis prefix `[0, at)` and splices `tail` in
    /// at `at`, re-keying every `tail` override's `Moment` by `+ at`. Because
    /// `Moment` is one axis for both planes, this is plain integer arithmetic; the
    /// result is genesis-complete and collision-free (`base` contributes only
    /// `m < at`, `tail` only `m + at ≥ at`) and carries `base`'s seed/policy (which
    /// equal `tail`'s). This succeeds at **any** `at`, genesis or not — the
    /// explorer rebases a branch-local delta onto a base below a snapshot this way.
    ///
    /// It **fails closed** ([`EnvError::UnsupportedComposition`]) for the cases
    /// outside this one-axis scope, which belong to **task 93** (the compose-model
    /// revisit — "see task 93"):
    ///
    /// - **Either input carries a [`StandingFault`].** Its window is in [`VTime`]
    ///   (retired *branches*) — a *different axis* than the `Moment` (retired
    ///   *instructions*) offset; a correct re-key needs a runtime `Moment → VTime`
    ///   map `compose` lacks.
    /// - **Either input is a pure [`Seeded`](EnvSpec::Seeded) environment.** Every
    ///   one of its decisions is seed-serviced, so splicing it at `at > 0` would
    ///   desync the tail's fresh PRNG stream (the composed prefix advances the
    ///   shared seed before the tail starts). Seeded/PRNG-state composition needs
    ///   the snapshot's captured PRNG state — task 93.
    /// - **`tail`'s seed or policy differs from `base`'s.** A single `EnvSpec`
    ///   carries one seed/policy, so it cannot hold a piecewise stream.
    ///
    /// Returns [`EnvError::Overflow`] if a tail `m + at` would exceed [`u64::MAX`]
    /// (rejected, never saturated — a wrap would collapse distinct overrides).
    ///
    /// **Scope note.** This is one-axis `Moment` *override* re-keying only. A
    /// `Recorded` input is treated as override-driven; if the composed run draws
    /// the seed for an unoverridden decision across a non-genesis splice, that is
    /// the seeded composition deferred to task 93 — the caller composes
    /// override-covered reproducers. `compose` re-keys the override map (the gate)
    /// and rejects the statically-detectable seeded inputs (the `Seeded` variant).
    pub fn compose(base: &EnvSpec, tail: &EnvSpec, at: Moment) -> Result<EnvSpec, EnvError> {
        // Fail closed on the multi-axis / seeded cases deferred to task 93.
        if !standing_of(base).is_empty() || !standing_of(tail).is_empty() {
            return Err(EnvError::UnsupportedComposition);
        }
        if matches!(base, EnvSpec::Seeded { .. }) || matches!(tail, EnvSpec::Seeded { .. }) {
            return Err(EnvError::UnsupportedComposition);
        }
        if tail.seed() != base.seed() || tail.policy() != base.policy() {
            return Err(EnvError::UnsupportedComposition);
        }

        // One-axis Moment override re-key (the spec gate): base keeps its prefix
        // [0, at), tail shifts into [at, ∞). Collision-free; overflow rejects.
        let mut overrides: BTreeMap<Moment, Action> = base
            .overrides()
            .iter()
            .filter(|(m, _)| **m < at)
            .map(|(m, a)| (*m, a.clone()))
            .collect();
        for (m, a) in tail.overrides() {
            overrides.insert(rekey_moment(*m, at)?, a.clone());
        }

        Ok(EnvSpec::Recorded {
            seed: base.seed(),
            policy: base.policy().clone(),
            overrides,
            standing: Vec::new(),
        })
    }
}

/// The standing faults of a spec (`Seeded` has none).
fn standing_of(spec: &EnvSpec) -> &[StandingFault] {
    match spec {
        EnvSpec::Recorded { standing, .. } => standing,
        EnvSpec::Seeded { .. } => &[],
    }
}

/// Re-key a tail `Moment` onto the composed genesis timeline by `+ at`, rejecting
/// overflow with [`EnvError::Overflow`] rather than wrapping (a wrap would collapse
/// two distinct overrides onto one key, breaking collision-free replay). Factored
/// out so the Kani harnesses can prove it injective and overflow-safe.
fn rekey_moment(m: Moment, at: Moment) -> Result<Moment, EnvError> {
    m.checked_add(at).ok_or(EnvError::Overflow)
}

/// Kani proof harnesses for the bounded integer invariants `compose` rests on
/// (`Ratio`'s no-divide-by-zero guard; `rekey_moment` injectivity + overflow
/// safety). `#[cfg(kani)]` + a separate file so they are verified by the `kani`
/// job, not compiled into the normal/test build or seen by the mutation oracle. A
/// child of `envcodec`, so `use super::*` reaches the private `rekey_moment` and
/// the imported `Ratio`.
#[cfg(kani)]
#[path = "envcodec_proofs.rs"]
mod proofs;

/// A deterministic [`Moment`] slot that does **not** hold a guest action — used
/// by `mutate` to place a host action without ever clobbering a guest override.
/// Draws one PRNG word, then scans upward (wrapping) past any guest-occupied
/// slot; it may land on a free slot or overwrite another host action, both legal.
/// Terminates because guest actions are finite.
fn free_non_guest_slot(map: &BTreeMap<Moment, Action>, rng: &mut Prng) -> Moment {
    let mut d = rng.next_u64();
    while matches!(map.get(&d), Some(Action::Guest(_))) {
        d = d.wrapping_add(1);
    }
    d
}

/// Draw one legal [`HostFault`] from `rng`. Every host fault is unconditionally
/// legal (no service point, no admissibility), so any draw is a valid proposal;
/// `SetClockRate`'s denominator is forced `≥ 1` so the `Ratio` always constructs.
fn host_fault_from(rng: &mut Prng) -> HostFault {
    match rng.next_u64() % 4 {
        0 => HostFault::SkewTime(VTime(rng.next_u64())),
        1 => {
            let num = rng.next_u64();
            // den in 1..=2^32, never zero.
            let den = (rng.next_u64() % (1u64 << 32)) + 1;
            // den != 0 by construction, so `new` is infallible here.
            HostFault::SetClockRate(Ratio::new(num, den).expect("den >= 1 by construction"))
        }
        2 => HostFault::CorruptMemory {
            gpa: rng.next_u64(),
            mask: BitMask(rng.next_u64()),
        },
        _ => HostFault::InjectInterrupt {
            vector: (rng.next_u64() & 0xFF) as u8,
        },
    }
}

#[cfg(test)]
mod tests {
    //! Exact-value unit tests that pin the private helpers and per-branch effects
    //! against mutation (the PR #16 round-2 `cargo mutants` survivors). They reach
    //! the private `host_fault_from` / `free_non_guest_slot` and the private
    //! `Prng` / `MUTATE_DOMAIN`, which integration tests in `tests/` cannot, so
    //! each mutant has a test that fails with an *exact* value, not a property it
    //! could slip past.

    use std::collections::BTreeMap;

    use super::{EnvCodec, MUTATE_DOMAIN, free_non_guest_slot, host_fault_from};
    use crate::VTime;
    use crate::catalog::Answer;
    use crate::host::{Action, BitMask, HostFault, Moment, Ratio};
    use crate::policy::FaultPolicy;
    use crate::prng::Prng;
    use crate::recorded::EnvSpec;

    /// First seed whose initial PRNG word selects `host_fault_from` arm `arm`
    /// (`word % 4 == arm`). Computed from the bare `Prng`, independent of any
    /// mutant in `host_fault_from` itself.
    fn seed_for_arm(arm: u64) -> u64 {
        (0u64..10_000)
            .find(|&s| Prng::new(s).next_u64() % 4 == arm)
            .expect("an arm-selecting seed exists in range")
    }

    /// First salt whose `mutate` op selector (`(salt ^ MUTATE_DOMAIN) word % 3`)
    /// equals `op`. Independent of any mutant in `mutate`'s arms.
    fn salt_for_op(op: u64) -> u64 {
        (0u64..10_000)
            .find(|&s| Prng::new(s ^ MUTATE_DOMAIN).next_u64() % 3 == op)
            .expect("an op-selecting salt exists in range")
    }

    // ---- host_fault_from arms (kills delete-arm-0 / delete-arm-1) -----------

    #[test]
    fn host_fault_from_arm0_is_exact_skewtime() {
        let seed = seed_for_arm(0);
        let got = host_fault_from(&mut Prng::new(seed));
        // Independent restatement: word0 selects the arm, word1 is the VTime.
        let mut e = Prng::new(seed);
        let _arm = e.next_u64();
        let expected = HostFault::SkewTime(VTime(e.next_u64()));
        assert_eq!(
            got, expected,
            "arm 0 must map to exactly SkewTime(word1) (deleting it yields InjectInterrupt)"
        );
    }

    #[test]
    fn host_fault_from_arm1_is_exact_setclockrate() {
        let seed = seed_for_arm(1);
        let got = host_fault_from(&mut Prng::new(seed));
        let mut e = Prng::new(seed);
        let _arm = e.next_u64();
        let num = e.next_u64();
        let den = (e.next_u64() % (1u64 << 32)) + 1;
        let expected = HostFault::SetClockRate(Ratio::new(num, den).unwrap());
        assert_eq!(
            got, expected,
            "arm 1 must map to exactly SetClockRate(num/den) (deleting it yields InjectInterrupt)"
        );
    }

    #[test]
    fn host_fault_from_arm2_is_exact_corruptmemory() {
        let seed = seed_for_arm(2);
        let got = host_fault_from(&mut Prng::new(seed));
        let mut e = Prng::new(seed);
        let _arm = e.next_u64();
        let gpa = e.next_u64();
        let mask = BitMask(e.next_u64());
        assert_eq!(
            got,
            HostFault::CorruptMemory { gpa, mask },
            "arm 2 must map to exactly CorruptMemory{{gpa, mask}} (deleting it yields InjectInterrupt)"
        );
    }

    #[test]
    fn host_fault_from_arm3_is_exact_inject_interrupt() {
        // Pick an arm-3 seed whose vector byte is neither 0xFF nor 0x00, so the
        // exact assertion distinguishes `& 0xFF` from both `| 0xFF` (→ always
        // 0xFF) and `^ 0xFF` (→ byte ^ 0xFF).
        let seed = (0u64..10_000)
            .find(|&s| {
                let mut p = Prng::new(s);
                if p.next_u64() % 4 != 3 {
                    return false;
                }
                let v = (p.next_u64() & 0xFF) as u8;
                v != 0xFF && v != 0x00
            })
            .expect("a non-trivial arm-3 seed exists in range");
        let got = host_fault_from(&mut Prng::new(seed));
        let mut e = Prng::new(seed);
        let _arm = e.next_u64();
        let vector = (e.next_u64() & 0xFF) as u8;
        assert!(
            vector != 0xFF && vector != 0x00,
            "chosen seed has a discriminating vector byte"
        );
        assert_eq!(
            got,
            HostFault::InjectInterrupt { vector },
            "arm 3 must map to InjectInterrupt with the exact low byte (kills & -> |/^ on 0xFF)"
        );
    }

    // ---- free_non_guest_slot (kills body -> Default::default()) -------------

    #[test]
    fn free_non_guest_slot_returns_the_drawn_word_not_default() {
        let map: BTreeMap<Moment, Action> = BTreeMap::new();
        let seed = 0xABCD_1234_5678_9AB1;
        let got = free_non_guest_slot(&map, &mut Prng::new(seed));
        let expected = Prng::new(seed).next_u64();
        assert_eq!(got, expected, "returns the drawn PRNG word");
        assert_ne!(
            got,
            Moment::default(),
            "must not be Moment::default() (0) — the mutant's return"
        );
    }

    #[test]
    fn free_non_guest_slot_skips_a_guest_occupied_slot_exactly() {
        let seed = 0x55u64;
        let first = Prng::new(seed).next_u64();
        // Park a guest action exactly where the first draw lands.
        let map = BTreeMap::from([(first, Action::Guest(Answer::Nominal))]);
        let got = free_non_guest_slot(&map, &mut Prng::new(seed));
        assert_eq!(
            got,
            first.wrapping_add(1),
            "skips the guest-occupied slot to the next Moment"
        );
        assert!(!matches!(map.get(&got), Some(Action::Guest(_))));
        assert_ne!(got, Moment::default());
    }

    // ---- mutate arms (kills delete-arm-1 remove / delete-arm-2 move) --------

    fn one_host_spec(k: Moment, action: Action) -> EnvSpec {
        EnvSpec::Recorded {
            seed: 0,
            policy: FaultPolicy::none(),
            overrides: BTreeMap::from([(k, action)]),
            standing: vec![],
        }
    }

    #[test]
    fn mutate_remove_branch_deletes_the_sole_host_override() {
        let k = 100u64;
        let action = Action::Host(HostFault::InjectInterrupt { vector: 42 });
        let spec = one_host_spec(k, action);
        let out = EnvCodec::mutate(&spec, salt_for_op(1));
        assert!(
            out.overrides().is_empty(),
            "the remove branch empties the map (len 0); deleting it falls to insert (len >= 1)"
        );
    }

    #[test]
    fn mutate_move_branch_relocates_preserving_count_and_action() {
        let k = 100u64;
        let action = Action::Host(HostFault::InjectInterrupt { vector: 42 });
        let spec = one_host_spec(k, action.clone());
        let out = EnvCodec::mutate(&spec, salt_for_op(2));
        assert_eq!(
            out.overrides().len(),
            1,
            "the move branch keeps exactly one override; deleting it falls to insert (len 2)"
        );
        let (_m, a) = out.overrides().iter().next().unwrap();
        assert_eq!(
            a, &action,
            "the move branch preserves the exact host action; insert would fabricate a fresh one"
        );
    }

    #[test]
    fn mutate_insert_branch_adds_a_second_host_override() {
        // The default arm (op 0): a fresh host action is added, count grows by one.
        let k = 100u64;
        let action = Action::Host(HostFault::InjectInterrupt { vector: 42 });
        let spec = one_host_spec(k, action.clone());
        let out = EnvCodec::mutate(&spec, salt_for_op(0));
        assert_eq!(out.overrides().len(), 2, "insert adds a second override");
        assert_eq!(
            out.overrides().get(&k),
            Some(&action),
            "the original survives"
        );
    }
}
