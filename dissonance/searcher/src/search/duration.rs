// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::VecDeque,
    num::{NonZeroU64, NonZeroUsize},
};

use serde::{Deserialize, Serialize};

use crate::search::rand::RomuDuoJrRand;

pub const DURATION_POLICY_IDENTIFIER: &str = "recent_useful_work_log_duration_v1";
const RECENT_OBSERVATIONS: usize = 128;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Observation {
    duration: NonZeroU64,
    useful: bool,
    execution_cost: NonZeroU64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DurationCheckpoint {
    policy: String,
    #[serde(deserialize_with = "deserialize_recent")]
    recent: Vec<Observation>,
}

fn deserialize_recent<'de, D>(deserializer: D) -> Result<Vec<Observation>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct RecentVisitor;

    impl<'de> serde::de::Visitor<'de> for RecentVisitor {
        type Value = Vec<Observation>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                formatter,
                "at most {RECENT_OBSERVATIONS} duration observations"
            )
        }

        fn visit_seq<S>(self, mut sequence: S) -> Result<Self::Value, S::Error>
        where
            S: serde::de::SeqAccess<'de>,
        {
            if sequence
                .size_hint()
                .is_some_and(|size| size > RECENT_OBSERVATIONS)
            {
                return Err(serde::de::Error::custom(
                    "duration history exceeds its capacity",
                ));
            }
            let mut recent = Vec::with_capacity(RECENT_OBSERVATIONS);
            while let Some(observation) = sequence.next_element()? {
                if recent.len() == RECENT_OBSERVATIONS {
                    return Err(serde::de::Error::custom(
                        "duration history exceeds its capacity",
                    ));
                }
                recent.push(observation);
            }
            Ok(recent)
        }
    }

    deserializer.deserialize_seq(RecentVisitor)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DurationPolicy {
    recent: VecDeque<Observation>,
}

impl Default for DurationPolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl DurationPolicy {
    #[must_use]
    pub fn new() -> Self {
        Self {
            recent: VecDeque::with_capacity(RECENT_OBSERVATIONS),
        }
    }

    #[must_use]
    pub fn draw(&self, rand: &mut RomuDuoJrRand, max_duration: NonZeroU64) -> u64 {
        let scales = (u64::BITS - max_duration.get().leading_zeros()) as usize;
        let scale_bound = NonZeroUsize::new(scales).expect("positive duration has a scale");
        if rand.next_u64() & 1 == 0 {
            return 1_u64 << rand.below(scale_bound);
        }
        let mut successes = [0_u128; u64::BITS as usize];
        let mut costs = [0_u128; u64::BITS as usize];
        for observation in &self.recent {
            let scale = observation.duration.get().trailing_zeros() as usize;
            successes[scale] += u128::from(observation.useful);
            costs[scale] += u128::from(observation.execution_cost.get());
        }
        let mut best = [0_usize; u64::BITS as usize];
        let mut count = 0;
        for scale in 0..scales {
            if successes[scale] == 0 {
                continue;
            }
            let order = if count == 0 {
                std::cmp::Ordering::Greater
            } else {
                (successes[scale] * costs[best[0]]).cmp(&(successes[best[0]] * costs[scale]))
            };
            if order.is_gt() {
                count = 0;
            }
            if !order.is_lt() {
                best[count] = scale;
                count += 1;
            }
        }
        let scale = match NonZeroUsize::new(count) {
            Some(count) => best[rand.below(count)],
            None => rand.below(scale_bound),
        };
        1_u64 << scale
    }

    pub fn observe(
        &mut self,
        duration: NonZeroU64,
        useful: bool,
        execution_cost: NonZeroU64,
    ) -> Result<(), &'static str> {
        if !duration.get().is_power_of_two() {
            return Err("duration observation is not a logarithmic choice");
        }
        if self.recent.len() == RECENT_OBSERVATIONS {
            self.recent.pop_front();
        }
        self.recent.push_back(Observation {
            duration,
            useful,
            execution_cost,
        });
        Ok(())
    }

    #[must_use]
    pub fn checkpoint(&self) -> DurationCheckpoint {
        DurationCheckpoint {
            policy: DURATION_POLICY_IDENTIFIER.to_owned(),
            recent: self.recent.iter().copied().collect(),
        }
    }

    pub fn from_checkpoint(checkpoint: DurationCheckpoint) -> Result<Self, &'static str> {
        if checkpoint.policy != DURATION_POLICY_IDENTIFIER {
            return Err("unrecognized duration policy");
        }
        if checkpoint.recent.len() > RECENT_OBSERVATIONS {
            return Err("duration history exceeds its capacity");
        }
        if checkpoint
            .recent
            .iter()
            .any(|entry| !entry.duration.get().is_power_of_two())
        {
            return Err("duration history contains a nonlogarithmic choice");
        }
        let mut policy = Self::new();
        policy.recent.extend(checkpoint.recent);
        Ok(policy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn positive(value: u64) -> NonZeroU64 {
        NonZeroU64::new(value).expect("positive")
    }

    #[test]
    fn feasible_draws_include_the_full_integer_range() {
        let policy = DurationPolicy::new();
        let mut rand = RomuDuoJrRand::with_seed(17);
        let mut seen = 0_u64;
        for _ in 0..16_384 {
            let duration = policy.draw(&mut rand, positive(u64::MAX));
            assert!(duration.is_power_of_two());
            seen |= duration;
        }
        assert_eq!(seen, u64::MAX);
        for maximum in [1, 3, 17, u64::MAX] {
            for _ in 0..128 {
                assert!(policy.draw(&mut rand, positive(maximum)) <= maximum);
            }
        }
    }

    #[test]
    fn history_is_bounded_and_extreme_costs_do_not_overflow() {
        let mut policy = DurationPolicy::new();
        for _ in 0..1_024 {
            policy
                .observe(positive(1), true, positive(u64::MAX))
                .expect("observe");
            policy
                .observe(positive(2), true, positive(u64::MAX))
                .expect("observe");
        }
        assert_eq!(policy.checkpoint().recent.len(), RECENT_OBSERVATIONS);
        let mut rand = RomuDuoJrRand::with_seed(9);
        for _ in 0..128 {
            assert!(policy.draw(&mut rand, positive(2)) <= 2);
        }
    }

    #[test]
    fn corrupt_checkpoints_are_rejected() {
        let mut checkpoint = DurationPolicy::new().checkpoint();
        checkpoint.policy = "unknown".to_owned();
        assert!(DurationPolicy::from_checkpoint(checkpoint).is_err());
        let entry = Observation {
            duration: positive(1),
            useful: true,
            execution_cost: positive(1),
        };
        let mut checkpoint = DurationPolicy::new().checkpoint();
        checkpoint.recent = vec![entry; RECENT_OBSERVATIONS + 1];
        assert!(DurationPolicy::from_checkpoint(checkpoint).is_err());
        let mut checkpoint = DurationPolicy::new().checkpoint();
        checkpoint.recent.push(Observation {
            duration: positive(3),
            ..entry
        });
        assert!(DurationPolicy::from_checkpoint(checkpoint).is_err());
    }

    #[test]
    fn invalid_observation_does_not_change_learning_state() {
        let mut policy = DurationPolicy::new();
        policy
            .observe(positive(2), true, positive(7))
            .expect("observe");
        let before = policy.checkpoint();
        assert!(policy.observe(positive(3), true, positive(1)).is_err());
        assert_eq!(policy.checkpoint(), before);
    }

    #[test]
    fn history_limit_is_enforced_during_checkpoint_decoding() {
        let mut checkpoint = DurationPolicy::new().checkpoint();
        checkpoint.recent = vec![
            Observation {
                duration: positive(1),
                useful: false,
                execution_cost: positive(1),
            };
            RECENT_OBSERVATIONS + 1
        ];
        let json = serde_json::to_vec(&checkpoint).expect("encode");
        assert!(serde_json::from_slice::<DurationCheckpoint>(&json).is_err());
        let postcard = postcard::to_stdvec(&checkpoint).expect("encode");
        assert!(postcard::from_bytes::<DurationCheckpoint>(&postcard).is_err());
        checkpoint.recent.pop();
        let postcard = postcard::to_stdvec(&checkpoint).expect("encode");
        assert_eq!(
            postcard::from_bytes::<DurationCheckpoint>(&postcard).expect("decode"),
            checkpoint
        );
    }

    #[test]
    fn preference_uses_execution_cost_instead_of_the_duration_label() {
        let mut policy = DurationPolicy::new();
        for _ in 0..32 {
            policy
                .observe(positive(1), true, positive(100))
                .expect("observe");
            policy
                .observe(positive(2), true, positive(1))
                .expect("observe");
        }
        let mut rand = RomuDuoJrRand::with_seed(91);
        let preferred = (0..1_024)
            .filter(|_| policy.draw(&mut rand, positive(2)) == 2)
            .count();
        assert!(preferred > 640);
        for _ in 0..128 {
            assert_eq!(policy.draw(&mut rand, positive(1)), 1);
        }
    }
}
