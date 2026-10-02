// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::admission::AdmissionError;
use consonance_client::session::SharedState;
use control_proto::{
    DecisionId, EventRef, Moment, Resolution, StopConditions, StopReason, class_bit,
};
use environment::{
    channel::{Answer, Question, RecordedEnv, ServiceHandler, ServiceResponse},
    sdk::{self, EventClass, ServiceOutcome},
};
use hypercall_proto::{
    MAX_PAYLOAD, ServiceId, Status,
    observation::{self, Descriptor},
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, ops::Range};

pub(crate) type Env = RecordedEnv<Box<dyn ServiceHandler>>;
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) enum Pending {
    Request([u32; 5]),
    Write([u32; 4]),
    Close(u32),
    Seek {
        fd: u32,
        offset: i64,
        whence: u32,
        output: u32,
    },
}
impl Pending {
    pub(crate) fn import(&self) -> (&'static str, &'static str) {
        match self {
            Self::Request(_) => ("harmony_v1", "request"),
            Self::Write(_) => ("wasi_snapshot_preview1", "fd_write"),
            Self::Close(_) => ("wasi_snapshot_preview1", "fd_close"),
            Self::Seek { .. } => ("wasi_snapshot_preview1", "fd_seek"),
        }
    }
}
#[derive(Clone, Debug)]
pub(crate) struct Host {
    pub(crate) env: Env,
    pub(crate) pending: Option<Pending>,
    pub(crate) observations: BTreeMap<u32, (u64, u32)>,
    pub(crate) thresholds: BTreeMap<u32, u64>,
    pub(crate) events: SharedState,
    pub(crate) console: SharedState,
    pub(crate) closed: [bool; 2],
    pub(crate) imports: u64,
}
impl Host {
    pub(crate) fn new(env: Env) -> Self {
        Self {
            env,
            pending: None,
            observations: BTreeMap::new(),
            thresholds: BTreeMap::new(),
            events: SharedState::from_bytes(Vec::new(), None),
            console: SharedState::from_bytes(Vec::new(), None),
            closed: [false; 2],
            imports: 0,
        }
    }
}
pub(crate) struct Completion {
    pub(crate) host: Host,
    pub(crate) writes: Vec<(usize, Vec<u8>)>,
    pub(crate) value: i32,
    pub(crate) request_bytes: usize,
    pub(crate) answer_bytes: usize,
    pub(crate) stop: Option<StopReason>,
}
pub(crate) enum Prepared {
    External(Question),
    Complete(Box<Completion>),
}
fn invalid(message: &str) -> AdmissionError {
    AdmissionError(message.into())
}
pub(crate) fn range(memory: &[u8], pointer: u64, len: u64) -> Result<Range<usize>, AdmissionError> {
    let end = pointer
        .checked_add(len)
        .ok_or_else(|| invalid("linear-memory range overflow"))?;
    if end > memory.len() as u64 {
        return Err(invalid("linear-memory range exceeds capacity"));
    }
    Ok(pointer as usize..end as usize)
}
fn read(memory: &[u8], pointer: u32, len: usize) -> Result<&[u8], AdmissionError> {
    Ok(&memory[range(memory, u64::from(pointer), len as u64)?])
}
fn decision_answer(
    env: &mut Env,
    question: &Question,
    moment: u64,
    resolve: Option<&Resolution>,
) -> Result<Option<Answer>, AdmissionError> {
    env.set_moment(moment);
    if let Some(resolve) = resolve {
        if resolve.vtime.0 != moment
            || resolve.service != question.service()
            || resolve.id.0 != question.request_id()
        {
            return Err(invalid("resolution does not match the pending decision"));
        }
        let answer = Answer::decode(&resolve.answer.0).map_err(|e| invalid(&e.to_string()))?;
        if sdk::answer_bytes(&answer).is_none() {
            return Err(invalid("resolution exceeds its encoded answer limit"));
        }
        env.record_question(moment, question, answer.clone());
        return Ok(Some(answer));
    }
    match env.decide(question).map_err(|e| invalid(&e.to_string()))? {
        ServiceResponse::Answered(answer) => Ok(Some(answer)),
        ServiceResponse::External => Ok(None),
    }
}
impl Host {
    pub(crate) fn prepare(
        &self,
        memory: &[u8],
        moment: u64,
        until: StopConditions,
        resolve: Option<&Resolution>,
    ) -> Result<Prepared, AdmissionError> {
        let pending = self
            .pending
            .as_ref()
            .ok_or_else(|| invalid("no pending request"))?;
        let mut completion = Completion {
            host: self.clone(),
            writes: Vec::new(),
            value: 0,
            request_bytes: 0,
            answer_bytes: 0,
            stop: None,
        };
        completion.host.pending = None;
        completion.host.imports = completion
            .host
            .imports
            .checked_add(1)
            .ok_or_else(|| invalid("import count exhausted"))?;
        if !matches!(pending, Pending::Request(_)) && resolve.is_some() {
            return Err(invalid("resolution supplied for a descriptor operation"));
        }
        match *pending {
            Pending::Request([operation, pointer, len, output, capacity]) => {
                if len as usize > MAX_PAYLOAD
                    || capacity as usize > MAX_PAYLOAD
                    || range(memory, u64::from(pointer), u64::from(len)).is_err()
                    || range(memory, u64::from(output), u64::from(capacity)).is_err()
                {
                    if resolve.is_some() {
                        return Err(invalid("resolution targets an invalid request range"));
                    }
                    completion.value = -(Status::OutOfRange as i32);
                    return Ok(Prepared::Complete(Box::new(completion)));
                }
                let request = read(memory, pointer, len as usize)?;
                completion.request_bytes = request.len();
                let service = operation >> 16;
                let opcode = operation & 65535;
                let mut answer = Vec::new();
                let mut status = Status::Ok;
                if resolve.is_some() && !(service == ServiceId::Sdk as u32 && opcode == 3) {
                    return Err(invalid("resolution supplied for a non-decision request"));
                }
                match (service, opcode) {
                    (s, 3) if s == ServiceId::Sdk as u32 => {
                        let question = match sdk::service_question(request) {
                            Ok(question) => question,
                            Err(status) if resolve.is_none() => {
                                completion.value = -(status as i32);
                                return Ok(Prepared::Complete(Box::new(completion)));
                            }
                            Err(_) => {
                                return Err(invalid("resolution targets a malformed SDK decision"));
                            }
                        };
                        if resolve.is_none() && until.on.armed(class_bit::BUGGIFY) {
                            return Ok(Prepared::External(question));
                        }
                        if let Some(resolve) = resolve {
                            let resolved = decision_answer(
                                &mut completion.host.env,
                                &question,
                                moment,
                                Some(resolve),
                            )?
                            .unwrap();
                            answer = sdk::answer_bytes(&resolved)
                                .ok_or_else(|| invalid("invalid encoded decision answer"))?;
                        } else {
                            match sdk::decide_service(&mut completion.host.env, moment, request) {
                                Ok(ServiceOutcome::Answered(bytes)) => answer = bytes,
                                Ok(ServiceOutcome::External(question)) => {
                                    return Ok(Prepared::External(question));
                                }
                                Err(value) => status = value,
                            }
                        }
                    }
                    (s, 1) if s == ServiceId::Entropy as u32 => {
                        let count = match <[u8; 4]>::try_from(request) {
                            Ok(bytes) => u32::from_le_bytes(bytes),
                            Err(_) => {
                                completion.value = -(Status::BadRequest as i32);
                                return Ok(Prepared::Complete(Box::new(completion)));
                            }
                        };
                        if count as usize > MAX_PAYLOAD {
                            status = Status::OutOfRange;
                        } else {
                            let question =
                                Question::entropy(count).map_err(|e| invalid(&e.to_string()))?;
                            let Some(Answer::Data(bytes)) =
                                decision_answer(&mut completion.host.env, &question, moment, None)?
                            else {
                                return Err(invalid("entropy supply returned a non-data answer"));
                            };
                            answer = bytes;
                        }
                    }
                    (s, 1) if s == ServiceId::Payload as u32 => {
                        match sdk::pull_payload(&mut completion.host.env, request) {
                            Ok(Some(bytes)) => answer = bytes,
                            Ok(None) => {
                                status = Status::OutOfRange;
                                completion.stop = Some(StopReason::Quiescent {
                                    vtime: Moment(moment),
                                });
                            }
                            Err(value) => status = value,
                        }
                    }
                    (s, 2) if s == ServiceId::Sdk as u32 => {
                        match sdk::coverage_request(request).and_then(
                            |(thread, observed, ready)| {
                                sdk::decide_coverage(
                                    &mut completion.host.env,
                                    &mut completion.host.thresholds,
                                    moment,
                                    thread,
                                    observed,
                                    ready,
                                )
                            },
                        ) {
                            Ok(coverage) => answer.extend(coverage.encode()),
                            Err(value) => status = value,
                        }
                    }
                    (s, 1) if s == ServiceId::Event as u32 => {
                        if request.len() < 4 {
                            status = Status::BadRequest;
                        } else {
                            let id = u32::from_le_bytes(request[..4].try_into().unwrap());
                            let data = &request[4..];
                            match sdk::classify_event(id, data) {
                                EventClass::Malformed => status = Status::BadRequest,
                                event => {
                                    if id == observation::EVENT_ID {
                                        let descriptor = match Descriptor::decode(data) {
                                            Ok(descriptor) => descriptor,
                                            Err(_) => {
                                                completion.value = -(Status::BadRequest as i32);
                                                return Ok(Prepared::Complete(Box::new(
                                                    completion,
                                                )));
                                            }
                                        };
                                        if descriptor.len == 0 {
                                            completion.host.observations.remove(&descriptor.handle);
                                        } else {
                                            if range(
                                                memory,
                                                descriptor.address,
                                                u64::from(descriptor.len),
                                            )
                                            .is_err()
                                            {
                                                completion.value = -(Status::OutOfRange as i32);
                                                return Ok(Prepared::Complete(Box::new(
                                                    completion,
                                                )));
                                            }
                                            if !completion
                                                .host
                                                .observations
                                                .contains_key(&descriptor.handle)
                                                && completion.host.observations.len()
                                                    >= observation::MAX_REGIONS as usize
                                            {
                                                completion.value = -(Status::OutOfRange as i32);
                                                return Ok(Prepared::Complete(Box::new(
                                                    completion,
                                                )));
                                            }
                                            completion.host.observations.insert(
                                                descriptor.handle,
                                                (descriptor.address, descriptor.len),
                                            );
                                        }
                                    }
                                    let mut event_bytes = moment.to_le_bytes().to_vec();
                                    event_bytes.extend(id.to_le_bytes());
                                    event_bytes.extend((data.len() as u32).to_le_bytes());
                                    event_bytes.extend(data);
                                    completion.host.events =
                                        completion.host.events.appended(&event_bytes);
                                    completion.stop = match event {
                                        EventClass::Violation { id, data } => {
                                            Some(StopReason::Assertion {
                                                vtime: Moment(moment),
                                                ev: EventRef { id, data },
                                            })
                                        }
                                        EventClass::SnapshotPoint
                                            if until.on.armed(class_bit::SNAPSHOT_POINT) =>
                                        {
                                            Some(StopReason::SnapshotPoint {
                                                vtime: Moment(moment),
                                            })
                                        }
                                        _ => None,
                                    };
                                }
                            }
                        }
                    }
                    (s, 1) if s == ServiceId::Console as u32 => {
                        completion.host.console = completion.host.console.appended(request)
                    }
                    (1 | 2 | 4 | 6 | 8, _) => status = Status::UnknownOpcode,
                    _ => status = Status::UnknownService,
                }
                if status == Status::Ok {
                    if answer.len() > capacity as usize {
                        return Err(invalid("answer exceeds the declared output capacity"));
                    }
                    completion.value = answer.len() as i32;
                    completion.answer_bytes = answer.len();
                    if !answer.is_empty() {
                        completion.writes.push((output as usize, answer));
                    }
                } else {
                    completion.value = -(status as i32);
                }
            }
            Pending::Write([fd, iovecs, count, written]) => {
                if !(1..=2).contains(&fd) || self.closed[(fd - 1) as usize] {
                    completion.value = 8;
                } else if count > 128 {
                    completion.value = 28;
                } else {
                    let result = (|| {
                        range(memory, u64::from(written), 4)?;
                        let vectors = read(memory, iovecs, count as usize * 8)?;
                        let mut bytes: Vec<u8> = Vec::new();
                        for vector in vectors.chunks_exact(8) {
                            let pointer = u32::from_le_bytes(vector[..4].try_into().unwrap());
                            let len = u32::from_le_bytes(vector[4..].try_into().unwrap()) as usize;
                            if len > MAX_PAYLOAD - bytes.len() {
                                return Err(invalid("console write exceeds its byte limit"));
                            }
                            bytes.extend(read(memory, pointer, len)?);
                        }
                        Ok::<_, AdmissionError>(bytes)
                    })();
                    match result {
                        Ok(bytes) => {
                            completion.request_bytes = bytes.len() + count as usize * 8;
                            completion.answer_bytes = 4;
                            completion.writes.push((
                                written as usize,
                                (bytes.len() as u32).to_le_bytes().to_vec(),
                            ));
                            completion.host.console = completion.host.console.appended(&bytes);
                        }
                        Err(_) => completion.value = 21,
                    }
                }
            }
            Pending::Close(fd) => {
                if !(1..=2).contains(&fd) || self.closed[(fd - 1) as usize] {
                    completion.value = 8;
                } else {
                    completion.host.closed[(fd - 1) as usize] = true;
                }
            }
            Pending::Seek { fd, .. } => {
                completion.value = if (1..=2).contains(&fd) && !self.closed[(fd - 1) as usize] {
                    70
                } else {
                    8
                }
            }
        }
        Ok(Prepared::Complete(Box::new(completion)))
    }
    pub(crate) fn decision_stop(question: &Question, moment: u64) -> StopReason {
        let mut ctx = question.service().to_le_bytes().to_vec();
        ctx.extend(question.request_id().to_le_bytes());
        ctx.extend(question.payload());
        StopReason::Decision {
            vtime: Moment(moment),
            id: DecisionId(question.request_id()),
            ctx,
        }
    }
}

impl Host {
    pub(crate) fn events(&self) -> Result<Vec<(u64, u32, Vec<u8>)>, AdmissionError> {
        let bytes = self.events.materialize();
        let mut remaining = bytes.as_slice();
        let mut events = Vec::new();
        while !remaining.is_empty() {
            let (header, tail) = remaining
                .split_first_chunk::<16>()
                .ok_or_else(|| invalid("truncated captured event"))?;
            let at = u64::from_le_bytes(header[..8].try_into().unwrap());
            let id = u32::from_le_bytes(header[8..12].try_into().unwrap());
            let len = u32::from_le_bytes(header[12..].try_into().unwrap()) as usize;
            if len > MAX_PAYLOAD - 4 {
                return Err(invalid("captured event exceeds transport bounds"));
            }
            let data = tail
                .get(..len)
                .ok_or_else(|| invalid("truncated captured event payload"))?;
            events.push((at, id, data.to_vec()));
            remaining = &tail[len..];
        }
        Ok(events)
    }
}
