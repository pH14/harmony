// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`SeqMutators`] — AFLNet-style region operators over a recorded env's
//! `Moment → Action` schedule. Each is a pure deterministic `(env, salt) ->
//! EnvSpec` matching [`environment::EnvCodec::mutate`]'s shape, and each honors
//! the schedule-safety rules restated in the crate docs: guest overrides
//! verbatim, no standing faults introduced, `Moment` arithmetic that rejects
//! overflow (never wraps), and inserted host faults confined to the enforced v1
//! vocabulary ([`CorruptMemory`](HostFault::CorruptMemory) /
//! [`InjectInterrupt`](HostFault::InjectInterrupt)).

use std::collections::BTreeMap;

use environment::{Action, BitMask, EnvSpec, HostFault, Moment, StandingFault};
use explorer::Prng;

/// Domain separators, so each operator (and the dispatcher) draws a distinct
/// stream from the same salt, and none coincides with the base seed or
/// `environment`'s own `MUTATE_DOMAIN`.
const INSERT_DOMAIN: u64 = 0x494E_5345_5254_2121; // "INSERT!!"
const DELETE_DOMAIN: u64 = 0x4445_4C45_5445_2121; // "DELETE!!"
const RETARGET_DOMAIN: u64 = 0x5245_5441_5247_2121; // "RETARG!!"
const SHIFT_DOMAIN: u64 = 0x5348_4946_5421_2121; // "SHIFT!!!"
const DISPATCH_DOMAIN: u64 = 0x5345_5147_4D55_5421; // "SEQGMUT!"

/// The maximum span a `delete`/`shift` region covers and the maximum magnitude a
/// `shift` translates by. A bound keeps region draws in a workable range without
/// affecting determinism (the whole map is still reachable across salts).
const REGION_MAX: u64 = 1 << 20;

/// The AFLNet-style sequence mutators. A unit type: every operation is a pure
/// function of `(env, salt)`, holding no state — the mutation-axis analogue of
/// [`environment::EnvCodec`]'s host-plane `mutate`, but region-scoped and
/// confined to the enforced v1 host vocabulary.
#[derive(Clone, Copy, Debug, Default)]
pub struct SeqMutators;

impl SeqMutators {
    /// Dispatch to one operator selected by `salt` — a convenience for a campaign
    /// that wants "one region mutation" without choosing the operator itself.
    pub fn mutate(env: &EnvSpec, salt: u64) -> EnvSpec {
        let mut rng = Prng::new(salt ^ DISPATCH_DOMAIN);
        match rng.next_u64() % 4 {
            0 => Self::insert(env, salt),
            1 => Self::delete(env, salt),
            2 => Self::retarget(env, salt),
            _ => Self::shift(env, salt),
        }
    }

    /// **Insert:** add a fresh v1 host fault at a free `Moment`, or copy an
    /// existing host region (one host override, sanitized to v1) to a free slot.
    /// The destination is always a **genuinely free** `Moment`, so `insert` never
    /// clobbers any existing override (guest or host) — it adds exactly one.
    pub fn insert(env: &EnvSpec, salt: u64) -> EnvSpec {
        let mut overrides = env.overrides().clone();
        let mut rng = Prng::new(salt ^ INSERT_DOMAIN);
        let host_keys = host_keys(&overrides);

        // Copy an existing region when one exists and the coin says so; else
        // insert a fresh v1 host fault.
        let copy = !host_keys.is_empty() && (rng.next_u64() & 1 == 0);
        let fault = if copy {
            let k = host_keys[(rng.next_u64() % host_keys.len() as u64) as usize];
            match overrides.get(&k) {
                Some(Action::Host(f)) => sanitize_v1(*f, &mut rng),
                // Unreachable: host_keys only names Host entries.
                _ => v1_host_fault_from(&mut rng),
            }
        } else {
            v1_host_fault_from(&mut rng)
        };
        let dst = free_slot(&overrides, &mut rng);
        overrides.insert(dst, Action::Host(fault));
        rebuild(env, overrides)
    }

    /// **Delete:** remove every **host** override in a `Moment` region anchored on
    /// an existing host override (see [`anchored_region`]); guest overrides in the
    /// region are preserved verbatim. A no-op when there is no host override.
    pub fn delete(env: &EnvSpec, salt: u64) -> EnvSpec {
        let mut overrides = env.overrides().clone();
        let mut rng = Prng::new(salt ^ DELETE_DOMAIN);
        let keys = host_keys(&overrides);
        if keys.is_empty() {
            return rebuild(env, overrides);
        }
        let (lo, hi) = anchored_region(&keys, &mut rng);
        let victims: Vec<Moment> = overrides
            .range(lo..=hi)
            .filter(|(_, a)| matches!(a, Action::Host(_)))
            .map(|(m, _)| *m)
            .collect();
        for m in victims {
            overrides.remove(&m);
        }
        rebuild(env, overrides)
    }

    /// **Retarget:** rewrite one host override's payload to a fresh v1 host fault,
    /// in place (its `Moment` is unchanged). A no-op when there is no host
    /// override to retarget.
    pub fn retarget(env: &EnvSpec, salt: u64) -> EnvSpec {
        let mut overrides = env.overrides().clone();
        let mut rng = Prng::new(salt ^ RETARGET_DOMAIN);
        let host_keys = host_keys(&overrides);
        if !host_keys.is_empty() {
            let k = host_keys[(rng.next_u64() % host_keys.len() as u64) as usize];
            overrides.insert(k, Action::Host(v1_host_fault_from(&mut rng)));
        }
        rebuild(env, overrides)
    }

    /// **Shift:** translate a region's **host** overrides by a signed `Moment`
    /// delta, order-preserving. Fails **closed** (the env is returned unchanged)
    /// if any translated key would overflow `u64` or collide with a retained
    /// override — so a guest override is never clobbered and no two overrides
    /// ever collapse onto one `Moment`.
    pub fn shift(env: &EnvSpec, salt: u64) -> EnvSpec {
        let overrides = env.overrides().clone();
        let mut rng = Prng::new(salt ^ SHIFT_DOMAIN);
        let keys = host_keys(&overrides);
        if keys.is_empty() {
            return rebuild(env, overrides);
        }
        let (lo, hi) = anchored_region(&keys, &mut rng);
        let mag = rng.next_u64() % REGION_MAX;
        let neg = rng.next_u64() & 1 == 1;

        let region: Vec<Moment> = overrides
            .range(lo..=hi)
            .filter(|(_, a)| matches!(a, Action::Host(_)))
            .map(|(m, _)| *m)
            .collect();
        // The anchor is a host key inside `[lo, hi]`, so the region is non-empty;
        // the guard stays defensive.
        if region.is_empty() {
            return rebuild(env, overrides);
        }

        // Compute translated keys with checked arithmetic (never wraps).
        let mut moved: Vec<(Moment, Action)> = Vec::with_capacity(region.len());
        for &k in &region {
            let nk = if neg {
                k.checked_sub(mag)
            } else {
                k.checked_add(mag)
            };
            match (nk, overrides.get(&k)) {
                (Some(v), Some(action)) => moved.push((v, action.clone())),
                // Overflow: fail closed.
                _ => return rebuild(env, overrides),
            }
        }

        // Build the result: retained overrides minus the region, then the moved
        // host actions — but only if no moved key collides with a retained one.
        let mut result = overrides.clone();
        for &k in &region {
            result.remove(&k);
        }
        for (nk, _) in &moved {
            if result.contains_key(nk) {
                // Collision with a retained (guest or out-of-region host)
                // override: fail closed.
                return rebuild(env, overrides);
            }
        }
        for (nk, action) in moved {
            result.insert(nk, action);
        }
        rebuild(env, result)
    }
}

/// A `[lo, hi]` region **anchored on the schedule**: pick an existing key
/// uniformly, then jitter the bounds `± REGION_MAX` around it. Real recorded
/// schedules cluster `Moment`s at magnitudes far below `2^64`, so a region drawn
/// from a uniform `u64` low bound would intersect an override with probability
/// `~REGION_MAX/2^64 ≈ 0` — making the region operators no-ops on any realistic
/// schedule. Anchoring guarantees the region contains its anchor, so `delete`
/// always removes it and `shift` always has a non-empty region. `keys` must be
/// non-empty (the callers check). Uses `saturating_{sub,add}` for the bound
/// jitter only — a clamp on the *region window*, never a `Moment` translation, so
/// no override's key is wrapped or saturated.
fn anchored_region(keys: &[Moment], rng: &mut Prng) -> (Moment, Moment) {
    let anchor = keys[(rng.next_u64() % keys.len() as u64) as usize];
    let lo = anchor.saturating_sub(rng.next_u64() % REGION_MAX);
    let hi = anchor.saturating_add(rng.next_u64() % REGION_MAX);
    (lo, hi)
}

/// The host-keyed `Moment`s of an override map, in ascending order.
fn host_keys(map: &BTreeMap<Moment, Action>) -> Vec<Moment> {
    map.iter()
        .filter(|(_, a)| matches!(a, Action::Host(_)))
        .map(|(m, _)| *m)
        .collect()
}

/// Rebuild an [`EnvSpec::Recorded`] from a base's seed/policy/standing and a new
/// override map. Standing faults are carried through **verbatim** — never added
/// to, so no [`StandingFault`] is ever introduced.
fn rebuild(env: &EnvSpec, overrides: BTreeMap<Moment, Action>) -> EnvSpec {
    EnvSpec::Recorded {
        seed: env.seed(),
        policy: env.policy().clone(),
        overrides,
        standing: standing_of(env),
    }
}

/// A spec's standing faults (`Seeded` has none).
fn standing_of(env: &EnvSpec) -> Vec<StandingFault> {
    match env {
        EnvSpec::Recorded { standing, .. } => standing.clone(),
        EnvSpec::Seeded { .. } => Vec::new(),
    }
}

/// A deterministic **genuinely free** `Moment` slot — one holding *no* override
/// of either plane. Draw one word, then scan upward past **any** occupant (guest
/// or host). `insert`'s contract is "add a fresh fault at a free `Moment`", so it
/// must never land on an occupied slot and silently drop the incumbent — a
/// same-`Moment` override replacement is exactly the class the task-59 ruling-B
/// outlawed. (This is stricter than `environment::EnvCodec`'s own
/// `free_non_guest_slot`, which tolerates overwriting a host action; the
/// region-scoped mutators here do not.) Terminates because overrides are finite.
/// The scan's `wrapping_add` is a slot search, not a `Moment` translation — no
/// override's key is ever arithmetic-wrapped.
fn free_slot(map: &BTreeMap<Moment, Action>, rng: &mut Prng) -> Moment {
    let mut d = rng.next_u64();
    while map.contains_key(&d) {
        d = d.wrapping_add(1);
    }
    d
}

/// Draw one **enforced v1** host fault: only
/// [`CorruptMemory`](HostFault::CorruptMemory) or
/// [`InjectInterrupt`](HostFault::InjectInterrupt). The task-59-deferred
/// `SkewTime`/`SetClockRate` are never produced.
fn v1_host_fault_from(rng: &mut Prng) -> HostFault {
    if rng.next_u64() & 1 == 0 {
        HostFault::CorruptMemory {
            gpa: rng.next_u64(),
            mask: BitMask(rng.next_u64()),
        }
    } else {
        HostFault::InjectInterrupt {
            vector: (rng.next_u64() & 0xFF) as u8,
        }
    }
}

/// Keep a host fault only if it is already in the v1 vocabulary; otherwise
/// replace it with a fresh v1 fault. So copying a region can never *insert* a
/// deferred `SkewTime`/`SetClockRate`.
fn sanitize_v1(f: HostFault, rng: &mut Prng) -> HostFault {
    match f {
        HostFault::CorruptMemory { .. } | HostFault::InjectInterrupt { .. } => f,
        HostFault::SkewTime(_) | HostFault::SetClockRate(_) => v1_host_fault_from(rng),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use environment::{Answer, FaultPolicy, Ratio, VTime};

    /// A base spec with one guest and one host override at fixed moments.
    fn base() -> EnvSpec {
        let mut spec = EnvSpec::Seeded {
            seed: 7,
            policy: FaultPolicy::none(),
        };
        spec.record(100, Action::Guest(Answer::Nominal));
        spec.perturb(HostFault::InjectInterrupt { vector: 3 }, 200);
        spec
    }

    fn is_v1(f: &HostFault) -> bool {
        matches!(
            f,
            HostFault::CorruptMemory { .. } | HostFault::InjectInterrupt { .. }
        )
    }

    /// `free_slot` skips **any** occupant — host or guest — so `insert` can never
    /// land on an occupied `Moment` and silently drop the incumbent (the ruling-B
    /// regression). Unit-tested directly: seed a `Prng`, pre-occupy its exact
    /// first draw, and assert the returned slot moved past it. (Tested at the
    /// helper because `insert`'s fresh branch consumes a variable number of words
    /// in `v1_host_fault_from` before `free_slot` draws, so the collision `Moment`
    /// is not simply the word after the copy coin.)
    #[test]
    fn free_slot_skips_any_occupant() {
        let seed = 0xC0FF_EE00_1234_5678u64;
        let first = Prng::new(seed).next_u64();

        // A HOST occupant at the first draw: the pre-fix guest-only skip would
        // have returned `first` and clobbered it; now we skip to `first + 1`.
        let host_map = BTreeMap::from([(
            first,
            Action::Host(HostFault::InjectInterrupt { vector: 1 }),
        )]);
        let got = free_slot(&host_map, &mut Prng::new(seed));
        assert_eq!(got, first.wrapping_add(1), "must skip a host-occupied slot");
        assert!(!host_map.contains_key(&got));

        // A GUEST occupant is likewise skipped.
        let guest_map = BTreeMap::from([(first, Action::Guest(Answer::Nominal))]);
        assert_eq!(
            free_slot(&guest_map, &mut Prng::new(seed)),
            first.wrapping_add(1),
            "must skip a guest-occupied slot"
        );

        // A free first draw is returned as-is.
        assert_eq!(free_slot(&BTreeMap::new(), &mut Prng::new(seed)), first);
    }

    /// Every operator is a pure function of `(env, salt)`.
    #[test]
    fn operators_are_deterministic() {
        let e = base();
        for salt in 0u64..32 {
            assert_eq!(SeqMutators::insert(&e, salt), SeqMutators::insert(&e, salt));
            assert_eq!(SeqMutators::delete(&e, salt), SeqMutators::delete(&e, salt));
            assert_eq!(
                SeqMutators::retarget(&e, salt),
                SeqMutators::retarget(&e, salt)
            );
            assert_eq!(SeqMutators::shift(&e, salt), SeqMutators::shift(&e, salt));
            assert_eq!(SeqMutators::mutate(&e, salt), SeqMutators::mutate(&e, salt));
        }
    }

    /// Whether a host fault is task-59-deferred (not v1).
    fn is_deferred(f: &HostFault) -> bool {
        matches!(f, HostFault::SkewTime(_) | HostFault::SetClockRate(_))
    }

    /// Retarget converts exactly one deferred fault to v1: the retargeted slot
    /// becomes v1, and the deferred-fault count drops by one (it never introduces
    /// a NEW deferred fault). Asserts the real property — not the earlier
    /// tautology (`is_v1 || is_deferred` is true of all four variants).
    #[test]
    fn retarget_converts_one_deferred_to_v1() {
        let mut spec = EnvSpec::Seeded {
            seed: 1,
            policy: FaultPolicy::none(),
        };
        spec.perturb(HostFault::SkewTime(VTime(9)), 50);
        spec.perturb(HostFault::SetClockRate(Ratio::new(3, 4).unwrap()), 60);
        let deferred_count = |s: &EnvSpec| s.host_faults().filter(|(_, f)| is_deferred(f)).count();
        assert_eq!(deferred_count(&spec), 2, "both host faults start deferred");

        for salt in 0u64..128 {
            // The victim key is `host_keys[word0 % len]` with the two sorted keys.
            let mut rng = Prng::new(salt ^ RETARGET_DOMAIN);
            let keys = [50u64, 60u64];
            let chosen = keys[(rng.next_u64() % keys.len() as u64) as usize];

            let out = SeqMutators::retarget(&spec, salt);
            match out.overrides().get(&chosen) {
                Some(Action::Host(f)) => {
                    assert!(is_v1(f), "the retargeted slot must become v1")
                }
                other => panic!("expected a host override at {chosen}, got {other:?}"),
            }
            assert_eq!(
                deferred_count(&out),
                1,
                "retarget converts exactly one deferred fault and introduces none"
            );
        }
    }

    /// Copy sanitizes: inserting from a deferred-only base yields only v1 faults
    /// for the *newly inserted* override.
    #[test]
    fn insert_copy_sanitizes_deferred_source() {
        let mut spec = EnvSpec::Seeded {
            seed: 1,
            policy: FaultPolicy::none(),
        };
        spec.perturb(HostFault::SkewTime(VTime(9)), 50);
        // Salts where insert chooses the copy branch will sanitize the SkewTime.
        for salt in 0u64..64 {
            let out = SeqMutators::insert(&spec, salt);
            let new_keys: Vec<_> = out
                .host_faults()
                .filter(|(m, _)| *m != 50)
                .map(|(_, f)| f)
                .collect();
            for f in new_keys {
                assert!(is_v1(&f), "an inserted fault must be v1");
            }
        }
    }

    /// Guest overrides survive every operator verbatim.
    #[test]
    fn guest_overrides_are_verbatim() {
        let e = base();
        let guest_before: BTreeMap<_, _> = e
            .overrides()
            .iter()
            .filter(|(_, a)| matches!(a, Action::Guest(_)))
            .map(|(m, a)| (*m, a.clone()))
            .collect();
        for salt in 0u64..64 {
            for out in [
                SeqMutators::insert(&e, salt),
                SeqMutators::delete(&e, salt),
                SeqMutators::retarget(&e, salt),
                SeqMutators::shift(&e, salt),
            ] {
                let guest_after: BTreeMap<_, _> = out
                    .overrides()
                    .iter()
                    .filter(|(_, a)| matches!(a, Action::Guest(_)))
                    .map(|(m, a)| (*m, a.clone()))
                    .collect();
                assert_eq!(
                    guest_before, guest_after,
                    "guest overrides must be verbatim"
                );
            }
        }
    }

    /// The region operators **anchor on the schedule**, so they act on a
    /// realistic recorded env whose `Moment`s cluster far below `2^64` — a
    /// uniform-`u64` low bound would make them no-ops. On a schedule near `1e9`,
    /// `delete` removes at least one host override for *every* salt (the region
    /// always contains its host anchor) and `shift` relocates for at least one.
    #[test]
    fn region_ops_hit_a_realistic_schedule() {
        let mut spec = EnvSpec::Seeded {
            seed: 0,
            policy: FaultPolicy::none(),
        };
        for i in 0..8u64 {
            spec.perturb(
                HostFault::InjectInterrupt { vector: i as u8 },
                1_000_000_000 + i * 50,
            );
        }
        let host_count = |s: &EnvSpec| s.host_faults().count();
        let host_moments = |s: &EnvSpec| {
            let mut v: Vec<Moment> = s.host_faults().map(|(m, _)| m).collect();
            v.sort_unstable();
            v
        };
        let base_hosts = host_count(&spec);
        let base_moments = host_moments(&spec);

        let mut shift_relocated = false;
        for salt in 0u64..64 {
            let deleted = SeqMutators::delete(&spec, salt);
            assert!(
                host_count(&deleted) < base_hosts,
                "salt {salt}: anchored delete must remove >=1 host override from a clustered schedule"
            );
            if host_moments(&SeqMutators::shift(&spec, salt)) != base_moments {
                shift_relocated = true;
            }
        }
        assert!(
            shift_relocated,
            "anchored shift must relocate a host override on the clustered schedule"
        );
    }
}
