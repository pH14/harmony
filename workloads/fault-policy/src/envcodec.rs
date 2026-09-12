// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;

use crate::Span;
use crate::error::EnvError;
use crate::host::{Action, BitMask, HostFault, Moment, Ratio};
use crate::policy::FaultPolicy;
use crate::prng::Prng;
use crate::recorded::{EnvSpec, StandingFault};

const MUTATE_DOMAIN: u64 = 0x4D75_7461_7465_2121;

#[derive(Clone, Copy, Debug, Default)]
pub struct EnvCodec;

impl EnvCodec {
    pub fn seeded(seed: u64, policy: FaultPolicy) -> EnvSpec {
        EnvSpec::Seeded { seed, policy }
    }

    pub fn mutate(env: &EnvSpec, salt: u64) -> EnvSpec {
        let mut overrides = env.overrides().clone();
        let standing = match env {
            EnvSpec::Recorded { standing, .. } => standing.clone(),
            EnvSpec::Seeded { .. } => Vec::new(),
        };
        let reseeds = env.reseeds().clone();
        let payloads = env.payloads().map(<[Vec<u8>]>::to_vec);
        let mut rng = Prng::new(salt ^ MUTATE_DOMAIN);

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
                let k = host_keys[(rng.next_u64() % host_keys.len() as u64) as usize];
                overrides.remove(&k);
            }
            2 => {
                let k = host_keys[(rng.next_u64() % host_keys.len() as u64) as usize];
                if let Some(action) = overrides.remove(&k) {
                    let dst = free_non_guest_slot(&overrides, &mut rng);
                    overrides.insert(dst, action);
                }
            }
            _ => {
                let dst = free_non_guest_slot(&overrides, &mut rng);
                overrides.insert(dst, Action::Host(host_fault_from(&mut rng)));
            }
        }

        EnvSpec::Recorded {
            seed: env.seed(),
            policy: env.policy().clone(),
            overrides,
            standing,
            reseeds,
            payloads,
        }
    }

    pub fn compose(base: &EnvSpec, tail: &EnvSpec, at: Moment) -> Result<EnvSpec, EnvError> {
        if !standing_of(base).is_empty()
            || !standing_of(tail).is_empty()
            || base.payloads().is_some()
            || tail.payloads().is_some()
        {
            return Err(EnvError::UnsupportedComposition);
        }
        if matches!(base, EnvSpec::Seeded { .. }) || matches!(tail, EnvSpec::Seeded { .. }) {
            return Err(EnvError::UnsupportedComposition);
        }
        if tail.seed() != base.seed() || tail.policy() != base.policy() {
            return Err(EnvError::UnsupportedComposition);
        }

        let mut overrides: BTreeMap<Moment, Action> = base
            .overrides()
            .iter()
            .filter(|(m, _)| **m < at)
            .map(|(m, a)| (*m, a.clone()))
            .collect();
        for (m, a) in tail.overrides() {
            overrides.insert(rekey_moment(*m, at)?, a.clone());
        }

        let mut reseeds: BTreeMap<Moment, u64> = base
            .reseeds()
            .iter()
            .filter(|(m, _)| **m < at)
            .map(|(m, s)| (*m, *s))
            .collect();
        for (m, s) in tail.reseeds() {
            reseeds.insert(rekey_moment(*m, at)?, *s);
        }

        Ok(EnvSpec::Recorded {
            seed: base.seed(),
            policy: base.policy().clone(),
            overrides,
            standing: Vec::new(),
            reseeds,
            payloads: None,
        })
    }
}

fn standing_of(spec: &EnvSpec) -> &[StandingFault] {
    match spec {
        EnvSpec::Recorded { standing, .. } => standing,
        EnvSpec::Seeded { .. } => &[],
    }
}

fn rekey_moment(m: Moment, at: Moment) -> Result<Moment, EnvError> {
    m.checked_add(at).ok_or(EnvError::Overflow)
}

#[cfg(kani)]
#[path = "envcodec_proofs.rs"]
mod proofs;

fn free_non_guest_slot(map: &BTreeMap<Moment, Action>, rng: &mut Prng) -> Moment {
    let mut d = rng.next_u64();
    while matches!(map.get(&d), Some(Action::Guest(_))) {
        d = d.wrapping_add(1);
    }
    d
}

const MUTATE_INTID_MASK: u64 = 0xFF;

fn host_fault_from(rng: &mut Prng) -> HostFault {
    match rng.next_u64() % 4 {
        0 => HostFault::SkewTime(Span(rng.next_u64())),
        1 => {
            let num = rng.next_u64();
            let den = (rng.next_u64() % (1u64 << 32)) + 1;
            HostFault::SetClockRate(Ratio::new(num, den).expect("den >= 1 by construction"))
        }
        2 => HostFault::CorruptMemory {
            gpa: rng.next_u64(),
            mask: BitMask(rng.next_u64()),
        },
        _ => HostFault::InjectInterrupt {
            vector: (rng.next_u64() & MUTATE_INTID_MASK) as u32,
        },
    }
}

#[cfg(test)]
mod tests {

    use std::collections::BTreeMap;

    use super::{EnvCodec, MUTATE_DOMAIN, MUTATE_INTID_MASK, free_non_guest_slot, host_fault_from};
    use crate::Span;
    use crate::catalog::Answer;
    use crate::host::{Action, BitMask, HostFault, Moment, Ratio};
    use crate::policy::FaultPolicy;
    use crate::prng::Prng;
    use crate::recorded::EnvSpec;

    fn seed_for_arm(arm: u64) -> u64 {
        (0u64..10_000)
            .find(|&s| Prng::new(s).next_u64() % 4 == arm)
            .expect("an arm-selecting seed exists in range")
    }

    fn salt_for_op(op: u64) -> u64 {
        (0u64..10_000)
            .find(|&s| Prng::new(s ^ MUTATE_DOMAIN).next_u64() % 3 == op)
            .expect("an op-selecting salt exists in range")
    }

    #[test]
    fn host_fault_from_arm0_is_exact_skewtime() {
        let seed = seed_for_arm(0);
        let got = host_fault_from(&mut Prng::new(seed));
        let mut e = Prng::new(seed);
        let _arm = e.next_u64();
        let expected = HostFault::SkewTime(Span(e.next_u64()));
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
        let seed = (0u64..10_000)
            .find(|&s| {
                let mut p = Prng::new(s);
                if p.next_u64() % 4 != 3 {
                    return false;
                }
                let v = p.next_u64() & MUTATE_INTID_MASK;
                v != MUTATE_INTID_MASK && v != 0
            })
            .expect("a non-trivial arm-3 seed exists in range");
        let got = host_fault_from(&mut Prng::new(seed));
        let mut e = Prng::new(seed);
        let _arm = e.next_u64();
        let vector = (e.next_u64() & MUTATE_INTID_MASK) as u32;
        assert!(
            u64::from(vector) != MUTATE_INTID_MASK && vector != 0,
            "chosen seed has a discriminating vector byte"
        );
        assert_eq!(
            got,
            HostFault::InjectInterrupt { vector },
            "arm 3 must map to InjectInterrupt with the exact masked low byte"
        );
    }

    #[test]
    fn generated_interrupt_identities_stay_inside_the_admissible_range() {
        for seed in 0u64..2_000 {
            if let HostFault::InjectInterrupt { vector } = host_fault_from(&mut Prng::new(seed)) {
                assert!(
                    u64::from(vector) <= MUTATE_INTID_MASK,
                    "seed {seed} minted interrupt identity {vector}, outside the range the \
                     machine under test can accept — a wasted mutation, not a fault"
                );
            }
        }
    }

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

    fn one_host_spec(k: Moment, action: Action) -> EnvSpec {
        EnvSpec::Recorded {
            seed: 0,
            policy: FaultPolicy::none(),
            overrides: BTreeMap::from([(k, action)]),
            standing: vec![],
            reseeds: std::collections::BTreeMap::new(),
            payloads: None,
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
