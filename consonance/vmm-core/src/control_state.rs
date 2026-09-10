// SPDX-License-Identifier: AGPL-3.0-or-later
//! Snapshot state owned by the control plane, not the guest CPU or service.

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
pub(crate) struct ControlState {
    pub recorded: InputSpec,
    /// Only effects and reseeds are populated; this is not a replacement SDK environment.
    pub pending: InputSpec,
    pub poisoned: Option<ScheduleFailure>,
    pub exec_nonce: u64,
    /// Canonical retained command parser; guest serial input remains VM-owned.
    pub exec: Option<Vec<u8>>,
}

impl ControlState {
    pub fn encode(&self) -> Vec<u8> {
        let recorded = self.recorded.encode();
        let pending = self.pending.encode();
        let mut out = if self.exec.is_some() {
            b"HCSTATE2"
        } else {
            b"HCSTATE1"
        }
        .to_vec();
        out.extend_from_slice(&(recorded.len() as u64).to_le_bytes());
        out.extend_from_slice(&(pending.len() as u64).to_le_bytes());
        out.extend_from_slice(&self.exec_nonce.to_le_bytes());
        out.push(u8::from(self.poisoned.is_some()));
        if let Some(failure) = self.poisoned {
            out.extend_from_slice(&failure.moment.to_le_bytes());
            out.extend_from_slice(&failure.vtime.to_le_bytes());
        }
        out.extend_from_slice(&recorded);
        out.extend_from_slice(&pending);
        if let Some(exec) = &self.exec {
            out.extend_from_slice(&(exec.len() as u64).to_le_bytes());
            out.extend_from_slice(exec);
        }
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Option<Self>, &'static str> {
        if bytes.is_empty() {
            return Ok(None); // legacy portable artifacts did not carry control state
        }
        let mut input = bytes;
        let has_exec = match take(&mut input, 8)? {
            b"HCSTATE1" => false,
            b"HCSTATE2" => true,
            _ => return Err("control state magic"),
        };
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
        let recorded = InputSpec::decode_snapshot(take(&mut input, recorded_len)?)
            .map_err(|_| "recorded control inputs")?;
        let pending = InputSpec::decode_snapshot(take(&mut input, pending_len)?)
            .map_err(|_| "pending control inputs")?;
        let exec = if has_exec {
            let len = usize::try_from(number(&mut input)?).map_err(|_| "exec length")?;
            let bytes = take(&mut input, len)?;
            let command = crate::exec::retained::RetainedExec::decode(bytes)?;
            if command.view(control_proto::Moment(0)).id.checked_add(1) != Some(exec_nonce) {
                return Err("command nonce disagrees with retained command");
            }
            Some(bytes.to_vec())
        } else {
            None
        };
        if !input.is_empty() {
            return Err("trailing control state");
        }
        if pending.seed() != 0
            || pending.config() != &ServiceConfig::default()
            || pending.payloads().is_some()
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
        Ok(Some(Self {
            recorded,
            pending,
            poisoned,
            exec_nonce,
            exec,
        }))
    }

    pub fn decode_for_policy(
        bytes: &[u8],
        policy: &ServiceConfig,
    ) -> Result<Option<Self>, &'static str> {
        let state = Self::decode(bytes)?;
        if state
            .as_ref()
            .is_some_and(|state| state.recorded.config() != policy)
        {
            return Err("recorded control policy differs from the snapshot policy");
        }
        Ok(state)
    }

    pub fn decode_for_snapshot(
        bytes: &[u8],
        policy: &ServiceConfig,
        tainted: bool,
    ) -> Result<Option<Self>, &'static str> {
        let state = Self::decode_for_policy(bytes, policy)?;
        if !tainted && state.as_ref().is_some_and(|state| state.exec.is_some()) {
            return Err("retained command requires a tainted snapshot");
        }
        Ok(state)
    }

    /// Hash the complete control state. The consumed prefix also matters:
    /// it governs duplicate-input rejection and the recorded reproducer reply.
    pub fn append_hash(&self, suffix: &mut Vec<u8>) {
        let state = self.encode();
        suffix.extend_from_slice(b"CPLN");
        suffix.extend_from_slice(&(state.len() as u64).to_le_bytes());
        suffix.extend_from_slice(&state);
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
    use environment::channel::{Effect, MAX_CHANNEL_BYTES};

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
            exec: None,
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
    fn control_round_trip_and_legacy_absence() {
        let state = state();
        assert_eq!(
            ControlState::decode(&state.encode()).unwrap(),
            Some(state.clone())
        );
        assert_eq!(ControlState::decode(&[]).unwrap(), None);
        let mut runnable = state;
        runnable.poisoned = None;
        assert_eq!(
            ControlState::decode(&runnable.encode()).unwrap(),
            Some(runnable)
        );
    }

    #[test]
    fn retained_command_uses_versioned_record_and_hash_coverage() {
        let legacy = state();
        assert!(legacy.encode().starts_with(b"HCSTATE1"));
        let command = crate::exec::retained::RetainedExec::new("echo once", 16, 0);
        let mut retained = legacy.clone();
        retained.exec = Some(command.encode());
        let encoded = retained.encode();
        assert_eq!(
            ControlState::decode_for_snapshot(&encoded, retained.recorded.config(), false),
            Err("retained command requires a tainted snapshot")
        );
        assert_eq!(
            ControlState::decode_for_snapshot(&encoded, retained.recorded.config(), true).unwrap(),
            Some(retained.clone())
        );
        assert!(encoded.starts_with(b"HCSTATE2"));
        assert_eq!(
            ControlState::decode(&encoded).unwrap(),
            Some(retained.clone())
        );
        let mut before = Vec::new();
        let mut after = Vec::new();
        legacy.append_hash(&mut before);
        retained.append_hash(&mut after);
        assert_ne!(before, after);
        retained.exec_nonce += 1;
        assert_eq!(
            ControlState::decode(&retained.encode()),
            Err("command nonce disagrees with retained command")
        );
        for len in 1..encoded.len() {
            assert!(ControlState::decode(&encoded[..len]).is_err());
        }
        let mut trailing = encoded;
        trailing.push(0);
        assert!(ControlState::decode(&trailing).is_err());
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
                exec: None,
            };
            assert_eq!(
                (state.recorded.encode().len() > MAX_CHANNEL_BYTES),
                large_recorded
            );
            assert_eq!(
                (state.pending.encode().len() > MAX_CHANNEL_BYTES),
                !large_recorded
            );
            assert_eq!(ControlState::decode(&state.encode()).unwrap(), Some(state));
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
            .record_effect(20, Effect::InjectInterrupt { vector: 32 });
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
        baseline.append_hash(&mut suffix);
        for altered in [
            ControlState {
                exec_nonce: 18,
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
            altered.append_hash(&mut other);
            assert_ne!(other, suffix);
        }
        let mut history = baseline;
        history.recorded = InputSpec::seeded(10);
        let mut same = Vec::new();
        history.append_hash(&mut same);
        assert_ne!(same, suffix);
        let empty = ControlState {
            recorded: history.recorded,
            pending: InputSpec::seeded(0),
            poisoned: None,
            exec_nonce: 0,
            exec: None,
        };
        let mut unchanged = b"existing-state".to_vec();
        empty.append_hash(&mut unchanged);
        assert!(unchanged.starts_with(b"existing-stateCPLN"));
    }
}
