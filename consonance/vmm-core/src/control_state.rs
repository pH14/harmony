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
}

impl ControlState {
    pub fn encode(&self) -> Vec<u8> {
        let recorded = self.recorded.encode();
        let pending = self.pending.encode();
        let mut out = b"HCSTATE1".to_vec();
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
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Option<Self>, &'static str> {
        if bytes.is_empty() {
            return Ok(None); // legacy portable artifacts did not carry control state
        }
        let mut input = bytes;
        if take(&mut input, 8)? != b"HCSTATE1" {
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
        let recorded = InputSpec::decode(take(&mut input, recorded_len)?)
            .map_err(|_| "recorded control inputs")?;
        let pending = InputSpec::decode(take(&mut input, pending_len)?)
            .map_err(|_| "pending control inputs")?;
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
    use environment::channel::Effect;

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
        }
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
        };
        let mut unchanged = b"existing-state".to_vec();
        empty.append_hash(&mut unchanged);
        assert!(unchanged.starts_with(b"existing-stateCPLN"));
    }
}
