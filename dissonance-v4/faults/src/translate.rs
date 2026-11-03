// SPDX-License-Identifier: AGPL-3.0-or-later

use std::num::{NonZeroU16, NonZeroU64};

use engine::target::{Action, Vocabulary};
use faults_workload::{
    archive::{alternative_action, alternatives},
    bundle::FaultVocabulary,
    target::FaultAction,
};
use searcher::search::{duration::DurationPolicy, rand::RomuDuoJrRand};

pub const MAX_TICKS: u64 = 1_024;

#[derive(Clone, Debug)]
pub struct Translator {
    pub vocabulary: FaultVocabulary,
    pub alternatives: Vec<u8>,
}

impl Translator {
    pub fn new(vocabulary: FaultVocabulary) -> Self {
        let alternatives = alternatives(&vocabulary, true);
        Self {
            vocabulary,
            alternatives,
        }
    }

    pub fn vocabulary(&self, base_steps: u64) -> Vocabulary {
        Vocabulary {
            inputs: 2 * self.alternatives.len() as u32,
            holds: false,
            base_steps,
        }
    }

    pub fn action(&self, action: &Action, event_ready: u64) -> Result<FaultAction, String> {
        let mut rand = RomuDuoJrRand::with_seed(action.arg);
        let ticks = DurationPolicy::draw_without_history(
            &mut rand,
            NonZeroU64::new(MAX_TICKS).ok_or("empty tick range")?,
        );
        let ticks = NonZeroU16::new(u16::try_from(ticks).map_err(|e| e.to_string())?)
            .ok_or("zero ticks")?;
        let alternative = (action.input as usize)
            .checked_sub(self.alternatives.len())
            .and_then(|kind| self.alternatives.get(kind).copied())
            .unwrap_or(0);
        alternative_action(
            &mut rand,
            &self.vocabulary,
            alternative,
            event_ready,
            ticks,
            None,
        )
        .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faults_workload::target::FaultOperation;

    fn translator() -> Translator {
        Translator::new(
            serde_json::from_str(
                r#"{"nodes":3,"hooks":[1],"disks":[],"checksummed":[],"instrumented_events":true,"clock_faults":true,"excluded":[]}"#,
            )
            .expect("vocabulary"),
        )
    }

    #[test]
    fn half_the_inputs_wait_and_each_other_input_is_one_kind() {
        let t = translator();
        let vocab = t.vocabulary(64);
        assert_eq!(vocab.inputs as usize, 2 * t.alternatives.len());
        assert!(!vocab.holds);
        let kind = |input: u32, arg: u64, ready: u64| {
            t.action(
                &Action {
                    input,
                    arg,
                    hold: 1,
                },
                ready,
            )
            .expect("translate")
            .operation
        };
        for arg in 0..100 {
            for input in 0..t.alternatives.len() as u32 {
                assert!(matches!(kind(input, arg, 0b111), FaultOperation::Wait(_)));
            }
            let k = t.alternatives.len() as u32;
            assert!(matches!(kind(k, arg, 0), FaultOperation::Kill(..)));
            assert!(matches!(kind(k + 3, arg, 0), FaultOperation::Hook(1, _)));
            let event = k + t.alternatives.iter().position(|a| *a == 5).unwrap() as u32;
            assert!(matches!(kind(event, arg, 0), FaultOperation::Wait(_)));
            assert!(matches!(
                kind(event, arg, 0b001),
                FaultOperation::EventKill { node: 0, .. }
            ));
            let a = Action {
                input: k + 1,
                arg,
                hold: 1,
            };
            assert_eq!(t.action(&a, 0).unwrap(), t.action(&a, 0).unwrap());
            assert!(t.action(&a, 0).unwrap().ticks() <= MAX_TICKS);
        }
    }
}
