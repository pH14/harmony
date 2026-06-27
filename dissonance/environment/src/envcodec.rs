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

    /// **Compose** two environments on the single [`Moment`] axis: keep `base` as
    /// the genesis prefix `[0, at)` and splice `tail` in at `at`, re-keying every
    /// `tail` override's `Moment` by `+ at`. Because `Moment` is one axis for
    /// *both* planes, this re-keying is plain integer arithmetic — not a
    /// cross-plane merge (the task-93 simplification).
    ///
    /// The result is genesis-complete and collision-free: `base` contributes only
    /// `m < at`, `tail` only `m + at ≥ at`. The seed and policy come from `base`
    /// (the run starts there). **`tail`'s standing faults are carried too** —
    /// their V-time windows shift by `+ at` consistently with the overrides
    /// (V-time being a derived view of the same retired-count axis), so a bug
    /// caused by a branch-local standing fault (e.g. a partition) still replays
    /// from the composed genesis. `tail`'s seed and policy are not composed.
    ///
    /// Returns [`EnvError::Overflow`] if any re-keying (`m + at`, or a tail
    /// standing window bound `+ at`) would exceed [`u64::MAX`]; rejecting is
    /// mandatory because saturating would silently collapse distinct overrides
    /// onto one key, breaking the collision-free replay contract.
    pub fn compose(base: &EnvSpec, tail: &EnvSpec, at: Moment) -> Result<EnvSpec, EnvError> {
        let mut overrides: BTreeMap<Moment, Action> = base
            .overrides()
            .iter()
            .filter(|(m, _)| **m < at)
            .map(|(m, a)| (*m, a.clone()))
            .collect();
        for (m, a) in tail.overrides() {
            let key = m.checked_add(at).ok_or(EnvError::Overflow)?;
            overrides.insert(key, a.clone());
        }

        // base's standing faults are kept whole; tail's are appended, their
        // V-time windows shifted by +at (overflow rejects, never saturates).
        let mut standing: Vec<StandingFault> = match base {
            EnvSpec::Recorded { standing, .. } => standing.clone(),
            EnvSpec::Seeded { .. } => Vec::new(),
        };
        if let EnvSpec::Recorded {
            standing: tail_standing,
            ..
        } = tail
        {
            for s in tail_standing {
                let w0 = s.window.0.0.checked_add(at).ok_or(EnvError::Overflow)?;
                let w1 = s.window.1.0.checked_add(at).ok_or(EnvError::Overflow)?;
                standing.push(StandingFault {
                    class: s.class,
                    target: s.target.clone(),
                    window: (VTime(w0), VTime(w1)),
                });
            }
        }

        Ok(EnvSpec::Recorded {
            seed: base.seed(),
            policy: base.policy().clone(),
            overrides,
            standing,
        })
    }
}

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
