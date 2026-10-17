// SPDX-License-Identifier: AGPL-3.0-or-later

use std::borrow::Borrow;

use control_proto::ControlError;
use environment::input_spec::{InputSpec, ServiceConfig};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ScheduleFailure {
    pub moment: u64,
    pub vtime: u64,
}

impl ScheduleFailure {
    pub fn reply(self) -> ControlError {
        ControlError::ScheduleUnsatisfiable {
            moment: self.moment,
            vtime: self.vtime,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ControlState<R = InputSpec> {
    pub recorded: R,
    pub pending: InputSpec,
    pub poisoned: Option<ScheduleFailure>,
    pub exec_nonce: u64,
    pub reproducer_tainted: bool,
}

impl<R: Borrow<InputSpec>> ControlState<R> {
    pub fn encode(&self) -> Vec<u8> {
        let recorded = self.recorded.borrow().encode();
        let pending = self.pending.encode();
        let mut out = b"HCSTATE2".to_vec();
        out.extend_from_slice(&(recorded.len() as u64).to_le_bytes());
        out.extend_from_slice(&(pending.len() as u64).to_le_bytes());
        out.extend_from_slice(&self.exec_nonce.to_le_bytes());
        out.push(u8::from(self.poisoned.is_some()));
        if let Some(failure) = self.poisoned {
            out.extend_from_slice(&failure.moment.to_le_bytes());
            out.extend_from_slice(&failure.vtime.to_le_bytes());
        }
        out.push(u8::from(self.reproducer_tainted));
        out.extend_from_slice(&recorded);
        out.extend_from_slice(&pending);
        out
    }

    pub fn encode_and_append_hash(&self, suffix: &mut Vec<u8>) -> Vec<u8> {
        let state = self.encode();
        suffix.extend_from_slice(b"CPLN");
        suffix.extend_from_slice(&(state.len() as u64).to_le_bytes());
        suffix.extend_from_slice(&state);
        state
    }
}

impl ControlState {
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        let mut input = bytes;
        if take(&mut input, 8)? != b"HCSTATE2" {
            return Err("control state magic");
        }
        let recorded_len = usize::try_from(number(&mut input)?).map_err(|_| "control length")?;
        let pending_len = usize::try_from(number(&mut input)?).map_err(|_| "control length")?;
        let exec_nonce = number(&mut input)?;
        let poisoned = match take(&mut input, 1)?[0] {
            0 => None,
            1 => Some(ScheduleFailure {
                moment: number(&mut input)?,
                vtime: number(&mut input)?,
            }),
            _ => return Err("control failure tag"),
        };
        let reproducer_tainted = match take(&mut input, 1)?[0] {
            0 => false,
            1 => true,
            _ => return Err("control reproducer taint tag"),
        };
        let recorded = InputSpec::decode_snapshot(take(&mut input, recorded_len)?)
            .map_err(|_| "recorded control inputs")?;
        let pending = InputSpec::decode_snapshot(take(&mut input, pending_len)?)
            .map_err(|_| "pending control inputs")?;
        if !input.is_empty() {
            return Err("trailing control state");
        }
        if pending.seed() != 0
            || pending.config() != &ServiceConfig::default()
            || pending.payloads().is_some()
            || pending.coverage_schedule().is_some()
            || !pending.answers().is_empty()
        {
            return Err("pending control plan contains SDK inputs");
        }
        if pending
            .effects()
            .keys()
            .any(|at| recorded.effects().contains_key(at))
            || pending
                .reseeds()
                .keys()
                .any(|at| recorded.reseeds().contains_key(at))
        {
            return Err("control input is both pending and consumed");
        }
        if let Some(failure) = poisoned
            && !pending.effects().contains_key(&failure.moment)
            && !pending.reseeds().contains_key(&failure.moment)
        {
            return Err("failed control moment is absent from the pending plan");
        }
        Ok(Self {
            recorded,
            pending,
            poisoned,
            exec_nonce,
            reproducer_tainted,
        })
    }

    pub fn decode_for_policy(bytes: &[u8], policy: &ServiceConfig) -> Result<Self, &'static str> {
        let state = Self::decode(bytes)?;
        if state.recorded.config() != policy {
            return Err("recorded control policy differs from the snapshot policy");
        }
        Ok(state)
    }
}

fn take<'a>(input: &mut &'a [u8], length: usize) -> Result<&'a [u8], &'static str> {
    if length > input.len() {
        return Err("truncated control state");
    }
    let (head, tail) = input.split_at(length);
    *input = tail;
    Ok(head)
}

fn number(input: &mut &[u8]) -> Result<u64, &'static str> {
    Ok(u64::from_le_bytes(
        take(input, 8)?.try_into().map_err(|_| "control integer")?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use environment::channel::{CoverageSchedule, Effect, MAX_CHANNEL_BYTES};

    fn state() -> ControlState {
        let mut recorded = InputSpec::seeded(9);
        recorded.record_effect(
            1,
            Effect::WriteMemory {
                gpa: 32,
                bytes: vec![7],
            },
        );
        let mut pending = InputSpec::seeded(0);
        pending.record_effect(
            20,
            Effect::XorMemory {
                gpa: 33,
                bytes: vec![8],
            },
        );
        pending.record_reseed(21, 99);
        ControlState {
            recorded,
            pending,
            poisoned: Some(ScheduleFailure {
                moment: 20,
                vtime: 25,
            }),
            exec_nonce: 17,
            reproducer_tainted: true,
        }
    }

    fn large_input_spec(seed: u64) -> InputSpec {
        const EFFECT_BYTES: usize = 400_000;
        let mut spec = InputSpec::seeded(seed);
        for (at, fill) in [(1, 0x11), (2, 0x22), (3, 0x33)] {
            spec.record_effect(
                at,
                Effect::write_memory(at, vec![fill; EFFECT_BYTES]).unwrap(),
            );
        }
        spec
    }

    #[test]
    fn control_round_trip_rejects_absence() {
        let state = state();
        assert_eq!(
            ControlState::decode(&state.encode()).unwrap(),
            state.clone()
        );
        assert_eq!(ControlState::decode(&[]), Err("truncated control state"));
        let mut runnable = state;
        runnable.poisoned = None;
        assert_eq!(ControlState::decode(&runnable.encode()).unwrap(), runnable);
    }

    #[test]
    fn control_round_trip_accepts_large_recorded_and_pending_inputs() {
        for (recorded, pending, large_recorded) in [
            (large_input_spec(9), InputSpec::seeded(0), true),
            (InputSpec::seeded(9), large_input_spec(0), false),
        ] {
            let state = ControlState {
                recorded,
                pending,
                poisoned: None,
                exec_nonce: 17,
                reproducer_tainted: false,
            };
            assert_eq!(
                (state.recorded.encode().len() > MAX_CHANNEL_BYTES),
                large_recorded
            );
            assert_eq!(
                (state.pending.encode().len() > MAX_CHANNEL_BYTES),
                !large_recorded
            );
            assert_eq!(ControlState::decode(&state.encode()).unwrap(), state);
        }
    }

    #[test]
    fn control_decode_rejects_malformed_and_noncanonical_plans() {
        let bytes = state().encode();
        for length in 1..bytes.len() {
            assert!(ControlState::decode(&bytes[..length]).is_err());
        }
        for (offset, value) in [(0, 0), (8, 255), (32, 2)] {
            let mut bad = bytes.clone();
            bad[offset] = value;
            assert!(ControlState::decode(&bad).is_err());
        }
        let mut bad_taint = bytes.clone();
        bad_taint[49] = 2;
        assert_eq!(
            ControlState::decode(&bad_taint),
            Err("control reproducer taint tag")
        );
        let mut trailing = bytes;
        trailing.push(0);
        assert!(ControlState::decode(&trailing).is_err());
        let mut bad = state();
        bad.poisoned.as_mut().unwrap().moment = 19;
        assert!(ControlState::decode(&bad.encode()).is_err());
        let mut overlap = state();
        overlap.recorded.record_reseed(21, 99);
        assert!(ControlState::decode(&overlap.encode()).is_err());
        let mut overlap = state();
        overlap
            .recorded
            .record_effect(20, Effect::xor_memory(0x20, vec![32]).unwrap());
        assert!(ControlState::decode(&overlap.encode()).is_err());
        bad.poisoned = None;
        bad.pending.set_payloads(Some(vec![]));
        assert!(ControlState::decode(&bad.encode()).is_err());
    }

    #[test]
    fn pending_plan_rejects_each_sdk_input_independently() {
        use environment::channel::Answer as ServiceAnswer;
        let mut plans = Vec::new();
        plans.push(InputSpec::seeded(1));
        let mut configured = InputSpec::seeded(0);
        let mut policy = ServiceConfig::default();
        policy.configuration.push(1);
        configured.set_config(policy);
        plans.push(configured);
        let mut payload = InputSpec::seeded(0);
        payload.set_payloads(Some(vec![vec![2]]));
        plans.push(payload);
        let mut answered = InputSpec::seeded(0);
        answered
            .record_answer(1, 19, 7, ServiceAnswer::Nominal)
            .unwrap();
        plans.push(answered);
        let mut coverage = InputSpec::seeded(0);
        coverage.set_coverage_schedule(Some(CoverageSchedule::new(1024, vec![]).unwrap()));
        plans.push(coverage);
        for pending in plans {
            let invalid = ControlState {
                pending,
                poisoned: None,
                ..state()
            };
            assert_eq!(
                ControlState::decode(&invalid.encode()),
                Err("pending control plan contains SDK inputs")
            );
        }
    }

    #[test]
    fn hash_observes_pending_work_and_consumed_history() {
        let baseline = state();
        let mut suffix = Vec::new();
        let encoded = baseline.encode_and_append_hash(&mut suffix);
        assert_eq!(encoded, baseline.encode());
        assert_eq!(&suffix[..4], b"CPLN");
        assert_eq!(&suffix[4..12], &(encoded.len() as u64).to_le_bytes());
        assert_eq!(&suffix[12..], encoded.as_slice());
        assert_eq!(ControlState::decode(&encoded).unwrap(), baseline.clone());
        for altered in [
            ControlState {
                exec_nonce: 18,
                ..baseline.clone()
            },
            ControlState {
                reproducer_tainted: false,
                ..baseline.clone()
            },
            ControlState {
                poisoned: None,
                ..baseline.clone()
            },
            ControlState {
                pending: InputSpec::seeded(0),
                poisoned: None,
                ..baseline.clone()
            },
        ] {
            let mut other = Vec::new();
            altered.encode_and_append_hash(&mut other);
            assert_ne!(other, suffix);
        }
        let mut history = baseline;
        history.recorded = InputSpec::seeded(10);
        let mut same = Vec::new();
        history.encode_and_append_hash(&mut same);
        assert_ne!(same, suffix);
        let empty = ControlState {
            recorded: history.recorded,
            pending: InputSpec::seeded(0),
            poisoned: None,
            exec_nonce: 0,
            reproducer_tainted: false,
        };
        let mut unchanged = b"existing-state".to_vec();
        empty.encode_and_append_hash(&mut unchanged);
        assert!(unchanged.starts_with(b"existing-stateCPLN"));
    }
}
