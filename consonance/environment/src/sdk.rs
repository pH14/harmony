// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;

use hypercall_proto::{
    MAX_PAYLOAD, SDK_COVERAGE_QUANTUM, SDK_COVERAGE_REQUEST_LEN, SDK_COVERAGE_RESPONSE_LEN, Status,
};

use crate::Moment;
use crate::channel::{
    Answer, DEFAULT_COVERAGE_QUANTUM, Question, RecordedEnv, SERVICE_COVERAGE_QUANTUM,
    SERVICE_SCHEDULER, ServiceHandler, ServiceResponse,
};

pub const NAMESPACE_SHIFT: u32 = 24;
pub const LOCAL_MASK: u32 = (1 << NAMESPACE_SHIFT) - 1;
pub const NAMESPACE_ASSERT: u8 = 1;
pub const NAMESPACE_LIFECYCLE: u8 = 4;
const VIOLATION: u8 = 1;
const SERVICE_PREFIX: usize = 10;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EventClass {
    Violation { id: u32, data: Vec<u8> },
    SnapshotPoint,
    Recorded,
    Malformed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServiceOutcome {
    Answered(Vec<u8>),
    External(Question),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Coverage {
    pub thread: u32,
    pub observed: u64,
    pub ready: u32,
    pub selected: u32,
    pub next: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CoverageOutcome {
    Answered(Coverage),
    External(Question),
}

impl Coverage {
    #[must_use]
    pub fn encode(&self) -> [u8; SDK_COVERAGE_RESPONSE_LEN] {
        let mut answer = [0_u8; SDK_COVERAGE_RESPONSE_LEN];
        answer[0..8].copy_from_slice(&self.next.to_le_bytes());
        answer[8..12].copy_from_slice(&self.selected.to_le_bytes());
        answer
    }
}

#[must_use]
pub fn classify_event(id: u32, data: &[u8]) -> EventClass {
    let namespace = (id >> NAMESPACE_SHIFT) as u8;
    let local = id & LOCAL_MASK;
    match namespace {
        NAMESPACE_ASSERT if data.first() == Some(&VIOLATION) => {
            let Some(length) = data.get(1..3) else {
                return EventClass::Malformed;
            };
            let length = usize::from(u16::from_le_bytes([length[0], length[1]]));
            match data.get(3..) {
                Some(detail) if detail.len() == length => EventClass::Violation {
                    id: local,
                    data: detail.to_vec(),
                },
                _ => EventClass::Malformed,
            }
        }
        NAMESPACE_LIFECYCLE if local == 0 => {
            if data.is_empty() {
                EventClass::SnapshotPoint
            } else {
                EventClass::Malformed
            }
        }
        NAMESPACE_LIFECYCLE if local == 1 => {
            if data.len() == 8 {
                EventClass::SnapshotPoint
            } else {
                EventClass::Malformed
            }
        }
        _ => EventClass::Recorded,
    }
}

pub fn service_question(payload: &[u8]) -> Result<Question, Status> {
    let Some((prefix, body)) = payload.split_first_chunk::<SERVICE_PREFIX>() else {
        return Err(Status::BadRequest);
    };
    let service = u16::from_le_bytes([prefix[0], prefix[1]]);
    if service <= SERVICE_SCHEDULER {
        return Err(Status::BadRequest);
    }
    let mut request = [0_u8; 8];
    request.copy_from_slice(&prefix[2..]);
    Question::with_request_id(u64::from_le_bytes(request), service, body.to_vec())
        .map_err(|_| Status::BadRequest)
}

pub fn decide_service<H: ServiceHandler + Clone>(
    env: &mut RecordedEnv<H>,
    moment: Moment,
    payload: &[u8],
) -> Result<ServiceOutcome, Status> {
    let question = service_question(payload)?;
    let mut candidate = env.checked_clone().map_err(|_| Status::Internal)?;
    candidate.set_moment(moment);
    let outcome = match candidate
        .decide_recorded(&question)
        .map_err(|error| match error {
            crate::channel::ChannelError::RequestIdentityMismatch => Status::BadRequest,
            _ => Status::Internal,
        })? {
        ServiceResponse::Answered(answer) => {
            ServiceOutcome::Answered(answer_bytes(&answer).ok_or(Status::Internal)?)
        }
        ServiceResponse::External => ServiceOutcome::External(question),
    };
    *env = candidate;
    Ok(outcome)
}

#[must_use]
pub fn answer_bytes(answer: &Answer) -> Option<Vec<u8>> {
    match answer {
        Answer::Nominal => Some(vec![0]),
        Answer::Data(bytes) if bytes.len() < MAX_PAYLOAD => {
            let mut out = Vec::with_capacity(1 + bytes.len());
            out.push(1);
            out.extend(bytes);
            Some(out)
        }
        Answer::Data(_) => None,
    }
}

pub fn pull_payload<H: ServiceHandler>(
    env: &mut RecordedEnv<H>,
    payload: &[u8],
) -> Result<Option<Vec<u8>>, Status> {
    let bytes = <[u8; 4]>::try_from(payload).map_err(|_| Status::BadRequest)?;
    let bytes = u32::from_le_bytes(bytes) as usize;
    if bytes == 0 || bytes > MAX_PAYLOAD {
        return Err(Status::BadRequest);
    }
    env.pull_payload(bytes).map_err(|_| Status::BadRequest)
}

pub fn coverage_request(payload: &[u8]) -> Result<(u32, u64, u32), Status> {
    let payload: &[u8; SDK_COVERAGE_REQUEST_LEN] =
        payload.try_into().map_err(|_| Status::BadRequest)?;
    let thread = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
    let mut observed = [0_u8; 8];
    observed.copy_from_slice(&payload[4..12]);
    let ready = u32::from_le_bytes([payload[12], payload[13], payload[14], payload[15]]);
    Ok((thread, u64::from_le_bytes(observed), ready))
}

pub fn coverage_question(
    thresholds: &BTreeMap<u32, u64>,
    thread: u32,
    observed: u64,
    ready: u32,
) -> Result<Question, Status> {
    let expected = thresholds
        .get(&thread)
        .copied()
        .unwrap_or(SDK_COVERAGE_QUANTUM);
    if ready == 0 || (observed != SDK_COVERAGE_QUANTUM && observed != expected) {
        return Err(Status::BadRequest);
    }
    Question::with_request_id(
        u64::from(thread),
        SERVICE_COVERAGE_QUANTUM,
        observed.to_le_bytes().to_vec(),
    )
    .map_err(|_| Status::BadRequest)
}

pub fn decide_coverage<H: ServiceHandler + Clone>(
    env: &mut RecordedEnv<H>,
    thresholds: &mut BTreeMap<u32, u64>,
    moment: Moment,
    thread: u32,
    observed: u64,
    ready: u32,
) -> Result<CoverageOutcome, Status> {
    let quantum_question = coverage_question(thresholds, thread, observed, ready)?;
    let mut candidate = env.checked_clone().map_err(|_| Status::Internal)?;
    candidate.set_moment(moment);
    let question = Question::with_request_id(
        u64::from(thread),
        SERVICE_SCHEDULER,
        ready.to_le_bytes().to_vec(),
    )
    .map_err(|_| Status::BadRequest)?;
    let Ok(ServiceResponse::Answered(Answer::Data(bytes))) = candidate.decide(&question) else {
        return Err(Status::Internal);
    };
    let selected = <[u8; 4]>::try_from(bytes.as_slice())
        .map(u32::from_le_bytes)
        .map_err(|_| Status::Internal)?;
    if selected >= ready {
        return Err(Status::Internal);
    }
    let quantum_answer = match candidate.recorded_override(&quantum_question) {
        Ok(Some(answer)) => ServiceResponse::Answered(answer),
        Ok(None) => match candidate.coverage_schedule() {
            Some(schedule) => ServiceResponse::Answered(Answer::Data(
                schedule.quantum_at(moment).to_le_bytes().to_vec(),
            )),
            None => candidate
                .decide_recorded(&quantum_question)
                .map_err(|_| Status::Internal)?,
        },
        Err(_) => return Err(Status::Internal),
    };
    let quantum = match quantum_answer {
        ServiceResponse::Answered(Answer::Nominal) => DEFAULT_COVERAGE_QUANTUM,
        ServiceResponse::Answered(Answer::Data(bytes)) => <[u8; 8]>::try_from(bytes.as_slice())
            .map(u64::from_le_bytes)
            .map_err(|_| Status::Internal)?,
        ServiceResponse::External => return Ok(CoverageOutcome::External(quantum_question)),
    };
    if quantum == 0 {
        return Err(Status::BadRequest);
    }
    let next = observed.checked_add(quantum).ok_or(Status::OutOfRange)?;
    *env = candidate;
    thresholds.insert(thread, next);
    Ok(CoverageOutcome::Answered(Coverage {
        thread,
        observed,
        ready,
        selected,
        next,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{ChannelError, CoverageSchedule, NominalHandler, RecordedState};
    use std::sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    };

    #[derive(Clone)]
    struct UnrecordedHandler(&'static [u8]);

    impl ServiceHandler for UnrecordedHandler {
        fn identity(&self) -> &[u8] {
            self.0
        }

        fn configuration(&self) -> &[u8] {
            &[]
        }

        fn respond(&mut self, _: Moment, _: &Question) -> Result<ServiceResponse, ChannelError> {
            panic!("unrecorded handler must not choose guest input")
        }

        fn snapshot_state(&self) -> Result<Vec<u8>, ChannelError> {
            Ok(Vec::new())
        }

        fn restore_state(&mut self, state: &[u8]) -> Result<(), ChannelError> {
            if state.is_empty() {
                Ok(())
            } else {
                Err(ChannelError::Malformed)
            }
        }

        fn clone_box(&self) -> Box<dyn ServiceHandler> {
            Box::new(self.clone())
        }
    }

    struct MutatingClone(Arc<AtomicU8>);

    impl ServiceHandler for MutatingClone {
        fn identity(&self) -> &[u8] {
            b"mutating-clone"
        }

        fn configuration(&self) -> &[u8] {
            b""
        }

        fn respond(&mut self, _: Moment, _: &Question) -> Result<ServiceResponse, ChannelError> {
            panic!("an unrecorded custom service must wait for an explicit answer")
        }

        fn snapshot_state(&self) -> Result<Vec<u8>, ChannelError> {
            Ok(vec![self.0.load(Ordering::SeqCst)])
        }

        fn restore_state(&mut self, state: &[u8]) -> Result<(), ChannelError> {
            let [value] = state else {
                return Err(ChannelError::Malformed);
            };
            self.0.store(*value, Ordering::SeqCst);
            Ok(())
        }

        fn clone_box(&self) -> Box<dyn ServiceHandler> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::new(Self(Arc::clone(&self.0)))
        }
    }

    fn mutating_clone_environment() -> (RecordedEnv<Box<dyn ServiceHandler>>, Arc<AtomicU8>) {
        let state = Arc::new(AtomicU8::new(0));
        let env = RecordedEnv::new(
            7,
            Box::new(MutatingClone(Arc::clone(&state))) as Box<dyn ServiceHandler>,
        );
        (env, state)
    }

    struct DivergingClone(u8);

    impl ServiceHandler for DivergingClone {
        fn identity(&self) -> &[u8] {
            b"diverging-clone"
        }

        fn configuration(&self) -> &[u8] {
            b""
        }

        fn respond(&mut self, _: Moment, _: &Question) -> Result<ServiceResponse, ChannelError> {
            panic!("an unrecorded custom service must wait for an explicit answer")
        }

        fn snapshot_state(&self) -> Result<Vec<u8>, ChannelError> {
            Ok(vec![self.0])
        }

        fn restore_state(&mut self, state: &[u8]) -> Result<(), ChannelError> {
            let [value] = state else {
                return Err(ChannelError::Malformed);
            };
            self.0 = *value;
            Ok(())
        }

        fn clone_box(&self) -> Box<dyn ServiceHandler> {
            Box::new(Self(self.0 + 1))
        }
    }

    #[test]
    fn unrecorded_custom_service_waits_for_explicit_input() {
        let mut env = RecordedEnv::new(7, UnrecordedHandler(b"unrecorded-handler"));
        let mut request = 19_u16.to_le_bytes().to_vec();
        request.extend(11_u64.to_le_bytes());
        request.extend(b"choose");
        let question = service_question(&request).unwrap();
        assert_eq!(
            decide_service(&mut env, 4, &request),
            Ok(ServiceOutcome::External(question.clone()))
        );
        env.record_question(4, &question, Answer::Data(vec![0x42]));
        assert_eq!(
            decide_service(&mut env, 4, &request),
            Ok(ServiceOutcome::Answered(vec![1, 0x42]))
        );
        let mut changed = request;
        changed.push(0x55);
        assert_eq!(
            decide_service(&mut env, 4, &changed),
            Err(Status::BadRequest)
        );
    }

    #[test]
    fn external_service_request_poisoned_if_cloning_changes_live_handler() {
        let (mut env, state) = mutating_clone_environment();
        assert!(env.snapshot_state().is_ok());
        let mut request = 9_u16.to_le_bytes().to_vec();
        request.extend(7_u64.to_le_bytes());
        assert!(matches!(
            decide_service(&mut env, 4, &request),
            Err(Status::Internal)
        ));
        assert_eq!(state.load(Ordering::SeqCst), 1);
        assert_eq!(env.snapshot_state(), Err(ChannelError::Poisoned));
    }

    #[test]
    fn external_service_request_poisoned_if_the_clone_has_different_handler_state() {
        let mut env = RecordedEnv::new(7, Box::new(DivergingClone(0)) as Box<dyn ServiceHandler>);
        assert!(env.snapshot_state().is_ok());
        let mut request = 9_u16.to_le_bytes().to_vec();
        request.extend(7_u64.to_le_bytes());
        assert_eq!(decide_service(&mut env, 4, &request), Err(Status::Internal));
        assert_eq!(env.snapshot_state(), Err(ChannelError::Poisoned));
    }

    #[test]
    fn external_coverage_request_poisoned_if_cloning_changes_live_handler() {
        let (mut env, state) = mutating_clone_environment();
        let mut thresholds = BTreeMap::new();
        assert!(env.snapshot_state().is_ok());
        assert_eq!(
            decide_coverage(&mut env, &mut thresholds, 4, 11, 1, 2),
            Err(Status::Internal)
        );
        assert_eq!(state.load(Ordering::SeqCst), 1);
        assert!(thresholds.is_empty());
        assert_eq!(env.snapshot_state(), Err(ChannelError::Poisoned));
    }

    #[test]
    fn claimed_nominal_identity_cannot_bypass_the_decision_boundary() {
        let mut env = RecordedEnv::new(7, UnrecordedHandler(b"consonance.nominal-handler.v1"));
        let mut request = 19_u16.to_le_bytes().to_vec();
        request.extend(11_u64.to_le_bytes());
        let question = service_question(&request).unwrap();
        assert_eq!(
            decide_service(&mut env, 4, &request),
            Ok(ServiceOutcome::External(question))
        );
    }

    #[test]
    fn unrecorded_custom_coverage_waits_for_explicit_input() {
        let mut env = RecordedEnv::new(7, UnrecordedHandler(b"unrecorded-handler"));
        let mut thresholds = BTreeMap::new();
        let before = RecordedState::capture(&env).unwrap().encode();
        assert_eq!(
            decide_coverage(&mut env, &mut thresholds, 4, 11, 1, 2),
            Ok(CoverageOutcome::External(
                Question::with_request_id(
                    11,
                    SERVICE_COVERAGE_QUANTUM,
                    1_u64.to_le_bytes().to_vec(),
                )
                .unwrap()
            ))
        );
        assert!(thresholds.is_empty());
        assert_eq!(RecordedState::capture(&env).unwrap().encode(), before);
        assert_eq!(RecordedState::capture(&env).unwrap().answers().count(), 0);
    }

    #[test]
    fn lifecycle_events_distinguish_frame_complete_from_neighbors() {
        let id = |local| (u32::from(NAMESPACE_LIFECYCLE) << NAMESPACE_SHIFT) | local;
        assert_eq!(classify_event(id(1), &[0; 8]), EventClass::SnapshotPoint);
        assert_eq!(classify_event(id(2), &[0; 8]), EventClass::Recorded);
        assert_eq!(classify_event(id(1), &[]), EventClass::Malformed);
    }

    #[test]
    fn event_payload_matrix() {
        type C = EventClass;
        let assert_id = (u32::from(NAMESPACE_ASSERT) << NAMESPACE_SHIFT) | 20;
        let setup_id = u32::from(NAMESPACE_LIFECYCLE) << NAMESPACE_SHIFT;
        let frame_id = setup_id | 1;
        let state_id = (2u32 << NAMESPACE_SHIFT) | 3;

        assert_eq!(
            classify_event(assert_id, &[1, 0, 0]),
            C::Violation {
                id: 20,
                data: vec![]
            }
        );
        assert_eq!(
            classify_event(assert_id, &[1, 2, 0, 0xAB, 0xCD]),
            C::Violation {
                id: 20,
                data: vec![0xAB, 0xCD]
            }
        );
        assert_eq!(classify_event(assert_id, &[1, 2, 0]), C::Malformed);
        assert_eq!(classify_event(assert_id, &[1, 0, 0, 0x99]), C::Malformed);
        assert_eq!(classify_event(assert_id, &[1]), C::Malformed);
        assert_eq!(classify_event(assert_id, &[1, 0]), C::Malformed);
        assert_eq!(classify_event(assert_id, &[0, 0, 0]), C::Recorded);
        assert_eq!(classify_event(assert_id, &[9, 0, 0]), C::Recorded);

        assert_eq!(classify_event(setup_id, &[]), C::SnapshotPoint);
        assert_eq!(classify_event(setup_id, &[0xAB]), C::Malformed);
        assert_eq!(classify_event(setup_id, &[0; 4]), C::Malformed);

        assert_eq!(
            classify_event(frame_id, &17_u64.to_le_bytes()),
            C::SnapshotPoint
        );
        assert_eq!(classify_event(frame_id, &[]), C::Malformed);
        assert_eq!(classify_event(frame_id, &[0; 7]), C::Malformed);
        assert_eq!(classify_event(frame_id, &[0; 9]), C::Malformed);

        assert_eq!(classify_event(state_id, &[0, 1, 2, 3]), C::Recorded);
        assert_eq!(
            classify_event((9u32 << NAMESPACE_SHIFT) | 7, &[1, 2, 3]),
            C::Recorded
        );
    }

    #[test]
    fn generic_service_response_reserves_one_byte_for_its_tag() {
        let bytes = vec![0x5a; MAX_PAYLOAD - 1];
        let frame = answer_bytes(&Answer::Data(bytes.clone())).unwrap();
        assert_eq!(frame.len(), MAX_PAYLOAD);
        assert_eq!(frame[0], 1);
        assert_eq!(&frame[1..], bytes);
        assert_eq!(answer_bytes(&Answer::Data(vec![0; MAX_PAYLOAD])), None);
        assert_eq!(answer_bytes(&Answer::Nominal), Some(vec![0]));
    }

    #[test]
    fn service_questions_need_a_prefix_above_the_core_supplies() {
        assert_eq!(service_question(&[0; 9]), Err(Status::BadRequest));
        let mut payload = SERVICE_SCHEDULER.to_le_bytes().to_vec();
        payload.extend(7_u64.to_le_bytes());
        assert_eq!(service_question(&payload), Err(Status::BadRequest));
        let mut payload = 9_u16.to_le_bytes().to_vec();
        payload.extend(7_u64.to_le_bytes());
        payload.extend(b"ask");
        let question = service_question(&payload).unwrap();
        assert_eq!(
            (
                question.service(),
                question.request_id(),
                question.payload()
            ),
            (9, 7, &b"ask"[..])
        );
    }

    #[test]
    fn recorded_service_answer_binds_the_first_request_payload() {
        let mut env = RecordedEnv::nominal(7);
        env.record_service_request(4, 9, 7, Answer::Data(vec![42]));
        let request = |body: &[u8]| {
            let mut payload = 9_u16.to_le_bytes().to_vec();
            payload.extend(7_u64.to_le_bytes());
            payload.extend(body);
            payload
        };
        assert_eq!(
            decide_service(&mut env, 4, &request(b"first")),
            Ok(ServiceOutcome::Answered(vec![1, 42]))
        );
        assert_eq!(
            decide_service(&mut env, 4, &request(b"second")),
            Err(Status::BadRequest)
        );
        assert_eq!(
            decide_service(&mut env, 4, &request(b"first")),
            Ok(ServiceOutcome::Answered(vec![1, 42]))
        );
        let state = RecordedState::decode(&env.snapshot_state().unwrap().encode()).unwrap();
        let mut replay = RecordedEnv::nominal(7);
        state.restore_into(&mut replay).unwrap();
        assert_eq!(
            decide_service(&mut replay, 4, &request(b"second")),
            Err(Status::BadRequest)
        );
    }

    #[test]
    fn coverage_commits_only_a_valid_answer() {
        let mut env = RecordedEnv::new(0x51ced, NominalHandler);
        let mut thresholds = BTreeMap::new();
        assert_eq!(coverage_request(&[0; 15]), Err(Status::BadRequest));
        let CoverageOutcome::Answered(first) =
            decide_coverage(&mut env, &mut thresholds, 4, 1, 1, 2).unwrap()
        else {
            panic!("nominal coverage must answer");
        };
        assert_eq!(first.next, 1 + DEFAULT_COVERAGE_QUANTUM);
        assert!(first.selected < 2);
        assert_eq!(
            decide_coverage(&mut env, &mut thresholds, 5, 1, 3, 2),
            Err(Status::BadRequest)
        );
        let mut replay_env = env.clone();
        let mut replay_thresholds = thresholds.clone();
        let continuation = decide_coverage(&mut env, &mut thresholds, 5, 1, first.next, 2).unwrap();
        assert_eq!(
            decide_coverage(&mut replay_env, &mut replay_thresholds, 5, 1, first.next, 2).unwrap(),
            continuation
        );
        env.record_service_request(9, SERVICE_SCHEDULER, 7, Answer::Data(vec![1, 2, 3]));
        let before = thresholds.clone();
        assert_eq!(
            decide_coverage(&mut env, &mut thresholds, 9, 7, 1, 2),
            Err(Status::Internal)
        );
        assert_eq!(thresholds, before);
    }

    #[test]
    fn coverage_quantum_is_recorded_and_replayed_with_the_threshold() {
        let mut env = RecordedEnv::new(11, NominalHandler);
        let mut thresholds = BTreeMap::new();
        env.record_service_request(
            41,
            SERVICE_COVERAGE_QUANTUM,
            7,
            Answer::Data(64_u64.to_le_bytes().to_vec()),
        );
        let CoverageOutcome::Answered(first) =
            decide_coverage(&mut env, &mut thresholds, 41, 7, 1, 2).unwrap()
        else {
            panic!("recorded coverage must answer");
        };
        assert_eq!(first.next, 65);
        let mut replay_env = env.clone();
        let mut replay_thresholds = thresholds.clone();
        let next = decide_coverage(&mut env, &mut thresholds, 42, 7, 65, 2).unwrap();
        assert_eq!(
            decide_coverage(&mut replay_env, &mut replay_thresholds, 42, 7, 65, 2).unwrap(),
            next
        );
    }

    #[test]
    fn a_reused_thread_identity_restarts_at_the_first_threshold() {
        let mut env = RecordedEnv::new(11, NominalHandler);
        let mut thresholds = BTreeMap::new();
        let CoverageOutcome::Answered(advanced) =
            decide_coverage(&mut env, &mut thresholds, 0, 7, SDK_COVERAGE_QUANTUM, 1).unwrap()
        else {
            panic!("nominal coverage must answer");
        };
        assert_eq!(
            decide_coverage(&mut env, &mut thresholds, 1, 7, advanced.next + 1, 1),
            Err(Status::BadRequest)
        );
        let CoverageOutcome::Answered(restarted) =
            decide_coverage(&mut env, &mut thresholds, 2, 7, SDK_COVERAGE_QUANTUM, 1).unwrap()
        else {
            panic!("nominal coverage must answer");
        };
        assert_eq!(thresholds.get(&7), Some(&restarted.next));
    }

    #[test]
    fn coverage_exchanges_keep_policy_answers_compact() {
        let mut env = RecordedEnv::new(11, NominalHandler);
        let mut thresholds = BTreeMap::new();
        let mut threshold = SDK_COVERAGE_QUANTUM;
        for moment in 0..1024 {
            let CoverageOutcome::Answered(coverage) =
                decide_coverage(&mut env, &mut thresholds, moment, 7, threshold, 1).unwrap()
            else {
                panic!("nominal coverage must answer");
            };
            threshold = coverage.next;
        }
        assert_eq!(RecordedState::capture(&env).unwrap().answers().count(), 0);
        assert_eq!(thresholds.get(&7), Some(&threshold));
    }

    #[test]
    fn scheduled_coverage_survives_more_callbacks_than_the_answer_tape_limit() {
        let schedule = CoverageSchedule::new(1024, vec![(10, 130_010, 1)]).unwrap();
        let mut env = RecordedEnv::new(11, UnrecordedHandler(b"custom-coverage"));
        env.set_coverage_schedule(Some(schedule));
        let mut thresholds = BTreeMap::new();
        let mut threshold = SDK_COVERAGE_QUANTUM;
        for moment in 10..130_010 {
            let CoverageOutcome::Answered(coverage) =
                decide_coverage(&mut env, &mut thresholds, moment, 7, threshold, 1).unwrap()
            else {
                panic!("recorded coverage policy must answer");
            };
            threshold = coverage.next;
        }
        assert_eq!(threshold, SDK_COVERAGE_QUANTUM + 130_000);
        let state = RecordedState::capture(&env).unwrap();
        assert_eq!(state.answers().count(), 0);
        assert!(state.encode().len() < 1024);
        let mut generic_request = SERVICE_COVERAGE_QUANTUM.to_le_bytes().to_vec();
        generic_request.extend(7_u64.to_le_bytes());
        generic_request.extend(1_u64.to_le_bytes());
        assert!(matches!(
            decide_service(&mut env, 130_010, &generic_request),
            Ok(ServiceOutcome::External(_))
        ));
    }

    #[test]
    fn exact_coverage_answer_precedes_the_recorded_schedule() {
        let mut env = RecordedEnv::new(11, UnrecordedHandler(b"custom-coverage"));
        env.set_coverage_schedule(Some(CoverageSchedule::new(1, vec![]).unwrap()));
        let observed = SDK_COVERAGE_QUANTUM;
        let question =
            Question::with_request_id(7, SERVICE_COVERAGE_QUANTUM, observed.to_le_bytes().to_vec())
                .unwrap();
        env.record_question(10, &question, Answer::Data(7_u64.to_le_bytes().to_vec()));
        let mut thresholds = BTreeMap::new();
        let CoverageOutcome::Answered(first) =
            decide_coverage(&mut env, &mut thresholds, 10, 7, observed, 1).unwrap()
        else {
            panic!("recorded answer must win");
        };
        assert_eq!(first.next, observed + 7);
        let CoverageOutcome::Answered(next) =
            decide_coverage(&mut env, &mut thresholds, 11, 7, first.next, 1).unwrap()
        else {
            panic!("schedule must answer after the exact override");
        };
        assert_eq!(next.next, first.next + 1);
    }

    #[test]
    fn rejected_coverage_quantum_keeps_the_threshold_unchanged() {
        for (answer, status) in [
            (vec![1, 2, 3], Status::Internal),
            (0_u64.to_le_bytes().to_vec(), Status::BadRequest),
            (u64::MAX.to_le_bytes().to_vec(), Status::OutOfRange),
        ] {
            let mut env = RecordedEnv::new(11, NominalHandler);
            let mut thresholds = BTreeMap::new();
            env.record_service_request(41, SERVICE_COVERAGE_QUANTUM, 7, Answer::Data(answer));
            let answers = RecordedState::capture(&env).unwrap().answers().count();
            assert_eq!(
                decide_coverage(&mut env, &mut thresholds, 41, 7, 1, 2),
                Err(status)
            );
            assert!(thresholds.is_empty());
            assert_eq!(
                RecordedState::capture(&env).unwrap().answers().count(),
                answers
            );
        }
    }
}
