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
use crate::host::{Action, BitMask, HostFault, Moment, Ratio};
use crate::policy::FaultPolicy;
use crate::prng::Prng;
use crate::recorded::EnvSpec;

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
    /// mutation is always *legal* — it inserts, moves, or removes a host-plane
    /// [`Action::Host`] override, which carries no admissibility constraint (a
    /// host fault needs no [`DecisionPoint`](crate::DecisionPoint)). Guest-plane
    /// mutation requires the live decision context the explorer supplies at
    /// `decide` time, not this offline codec, so it is out of scope here.
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

        // With an empty map only "insert" is possible; otherwise pick among
        // insert (0), remove (1), move (2).
        let op = if overrides.is_empty() {
            0
        } else {
            rng.next_u64() % 3
        };
        match op {
            1 => {
                let key = nth_key(&overrides, rng.next_u64());
                if let Some(k) = key {
                    overrides.remove(&k);
                }
            }
            2 => {
                let key = nth_key(&overrides, rng.next_u64());
                if let Some(k) = key
                    && let Some(action) = overrides.remove(&k)
                {
                    overrides.insert(rng.next_u64(), action);
                }
            }
            _ => {
                let at = rng.next_u64();
                overrides.insert(at, Action::Host(host_fault_from(&mut rng)));
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
    /// `tail` override's `Moment` by `+ at` (saturating at [`u64::MAX`]). Because
    /// `Moment` is one axis for *both* planes, this re-keying is plain integer
    /// arithmetic — not a cross-plane merge (the task-93 simplification).
    ///
    /// The result is genesis-complete and collision-free: `base` contributes only
    /// `m < at`, `tail` only `m + at ≥ at`. The seed, policy, and standing faults
    /// come from `base` (the run starts there); `tail`'s seed/policy/standing are
    /// not composed (standing faults live on the separate V-time axis — see
    /// [`StandingFault`](crate::StandingFault)).
    pub fn compose(base: &EnvSpec, tail: &EnvSpec, at: Moment) -> EnvSpec {
        let mut overrides: BTreeMap<Moment, Action> = base
            .overrides()
            .iter()
            .filter(|(m, _)| **m < at)
            .map(|(m, a)| (*m, a.clone()))
            .collect();
        for (m, a) in tail.overrides() {
            overrides.insert(m.saturating_add(at), a.clone());
        }
        let standing = match base {
            EnvSpec::Recorded { standing, .. } => standing.clone(),
            EnvSpec::Seeded { .. } => Vec::new(),
        };
        EnvSpec::Recorded {
            seed: base.seed(),
            policy: base.policy().clone(),
            overrides,
            standing,
        }
    }
}

/// The key at position `idx % len` of a non-empty map (used by `mutate` to pick a
/// victim entry deterministically). `None` only when the map is empty.
fn nth_key(map: &BTreeMap<Moment, Action>, idx: u64) -> Option<Moment> {
    let len = map.len();
    if len == 0 {
        return None;
    }
    map.keys().nth((idx % len as u64) as usize).copied()
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
