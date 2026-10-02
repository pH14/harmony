// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;

use hypercall_proto::{
    MAX_PAYLOAD, SDK_COVERAGE_QUANTUM, SDK_COVERAGE_REQUEST_LEN, SDK_COVERAGE_RESPONSE_LEN, Status,
};

use crate::Moment;
use crate::channel::{
    Answer, Question, RecordedEnv, SERVICE_SCHEDULER, ServiceHandler, ServiceResponse,
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
    let mut candidate = env.clone();
    candidate.set_moment(moment);
    let outcome = match candidate.decide(&question).map_err(|_| Status::Internal)? {
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

pub fn decide_coverage<H: ServiceHandler + Clone>(
    env: &mut RecordedEnv<H>,
    thresholds: &mut BTreeMap<u32, u64>,
    moment: Moment,
    thread: u32,
    observed: u64,
    ready: u32,
) -> Result<Coverage, Status> {
    let expected = thresholds
        .get(&thread)
        .copied()
        .unwrap_or(SDK_COVERAGE_QUANTUM);
    if ready == 0 || observed != expected {
        return Err(Status::BadRequest);
    }
    let next = observed
        .checked_add(SDK_COVERAGE_QUANTUM)
        .ok_or(Status::OutOfRange)?;
    let mut candidate = env.clone();
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
    *env = candidate;
    thresholds.insert(thread, next);
    Ok(Coverage {
        thread,
        observed,
        ready,
        selected,
        next,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::NominalHandler;

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
    fn coverage_commits_only_a_valid_answer() {
        let mut env = RecordedEnv::new(0x51ced, NominalHandler);
        let mut thresholds = BTreeMap::new();
        assert_eq!(coverage_request(&[0; 15]), Err(Status::BadRequest));
        let first = decide_coverage(&mut env, &mut thresholds, 4, 1, 1, 2).unwrap();
        assert_eq!(first.next, 2);
        assert!(first.selected < 2);
        assert_eq!(
            decide_coverage(&mut env, &mut thresholds, 5, 1, 3, 2),
            Err(Status::BadRequest)
        );
        let mut replay_env = env.clone();
        let mut replay_thresholds = thresholds.clone();
        let continuation = decide_coverage(&mut env, &mut thresholds, 5, 1, 2, 2).unwrap();
        assert_eq!(
            decide_coverage(&mut replay_env, &mut replay_thresholds, 5, 1, 2, 2).unwrap(),
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
}
