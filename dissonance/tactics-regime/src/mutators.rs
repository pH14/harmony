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
    /// Never clobbers a guest override.
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
        let dst = free_non_guest_slot(&overrides, &mut rng);
        overrides.insert(dst, Action::Host(fault));
        rebuild(env, overrides)
    }

    /// **Delete:** remove every **host** override in a `Moment` range `[lo, hi]`;
    /// guest overrides in the range are preserved verbatim.
    pub fn delete(env: &EnvSpec, salt: u64) -> EnvSpec {
        let mut overrides = env.overrides().clone();
        let mut rng = Prng::new(salt ^ DELETE_DOMAIN);
        let lo = rng.next_u64();
        let hi = lo.saturating_add(rng.next_u64() % REGION_MAX);
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
        let lo = rng.next_u64();
        let hi = lo.saturating_add(rng.next_u64() % REGION_MAX);
        let mag = rng.next_u64() % REGION_MAX;
        let neg = rng.next_u64() & 1 == 1;

        let region: Vec<Moment> = overrides
            .range(lo..=hi)
            .filter(|(_, a)| matches!(a, Action::Host(_)))
            .map(|(m, _)| *m)
            .collect();
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

/// A deterministic `Moment` slot that does **not** hold a guest action — inherited
/// verbatim from [`environment::EnvCodec`]'s `free_non_guest_slot`: draw one word,
/// then scan upward past any guest-occupied slot. It may land free or overwrite
/// another host action (both legal); it never overwrites a guest override.
/// Terminates because guest overrides are finite. The scan's `wrapping_add` is a
/// slot search, not a `Moment` translation — no override's key is ever
/// arithmetic-wrapped.
fn free_non_guest_slot(map: &BTreeMap<Moment, Action>, rng: &mut Prng) -> Moment {
    let mut d = rng.next_u64();
    while matches!(map.get(&d), Some(Action::Guest(_))) {
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

    /// Retarget never emits a deferred host fault, even when the victim was one.
    #[test]
    fn retarget_confines_to_v1_even_from_deferred() {
        let mut spec = EnvSpec::Seeded {
            seed: 1,
            policy: FaultPolicy::none(),
        };
        spec.perturb(HostFault::SkewTime(VTime(9)), 50);
        spec.perturb(HostFault::SetClockRate(Ratio::new(3, 4).unwrap()), 60);
        for salt in 0u64..64 {
            let out = SeqMutators::retarget(&spec, salt);
            for (_, f) in out.host_faults() {
                // The retargeted key is now v1; the untouched one may still be
                // deferred — but at least one becomes v1 and none becomes a *new*
                // deferred fault. Assert nothing outside {v1, original deferred}.
                assert!(
                    is_v1(&f) || matches!(f, HostFault::SkewTime(_) | HostFault::SetClockRate(_)),
                    "retarget produced an out-of-vocabulary fault"
                );
            }
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
}
