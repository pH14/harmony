// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU8, Ordering};

use environment::channel::{
    Answer, ChannelError, CoverageSchedule, HandlerFingerprint, HandlerSnapshot, NominalHandler,
    Question, RecordedEnv, ServiceHandler,
};
use environment::sdk::{self, EventClass, ServiceOutcome};
#[cfg(target_os = "linux")]
use environment::{
    channel::RecordedState,
    input_spec::{ServiceConfig, ServiceFactory},
};
use hypercall_proto::{
    MAX_FRAME, MAX_PAYLOAD, SeededEntropy, Service, ServiceId, Status, decode, encode_error,
    encode_response, observation,
};
use sha2::{Digest, Sha256};

pub(crate) const STAMP_LEN: usize = 8;
#[cfg(target_os = "linux")]
const SERVICES_MAGIC: &[u8; 8] = b"HUMLSVC1";

#[cfg(target_os = "linux")]
const SOCKET_TYPE: libc::c_int = libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC;
#[cfg(not(target_os = "linux"))]
const SOCKET_TYPE: libc::c_int = libc::SOCK_SEQPACKET;

#[cfg(target_os = "linux")]
const SEND_FLAGS: libc::c_int = libc::MSG_NOSIGNAL;
#[cfg(not(target_os = "linux"))]
const SEND_FLAGS: libc::c_int = 0;

pub(crate) const RUNNING: u8 = 0;
pub(crate) const CUT: u8 = 1;
pub(crate) const FAILED: u8 = 2;
pub(crate) const CLOSED: u8 = 3;
#[cfg(test)]
pub(crate) const TEST_DELAY_CUT_SEED: u64 = 0x6855_4d4c_4355_5431;
#[cfg(test)]
pub(crate) static TEST_HOLD_CUT: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
pub(crate) static TEST_CUT_READY: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bridge {
    pub seed: u64,
    pub cut: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub moment: u64,
    pub id: u32,
    pub data: Arc<[u8]>,
}

#[derive(Debug)]
pub(crate) struct BridgeLog {
    pub events: Vec<Event>,
    pub failure: Option<String>,
    pub socket: OwnedFd,
}

impl Bridge {
    pub fn new(seed: u64) -> Self {
        Self { seed, cut: None }
    }

    pub fn boot_seed(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(b"harmony-uml-boot-seed");
        digest.update(self.seed.to_le_bytes());
        hex(&digest.finalize())
    }
}

pub fn event_hash(events: &[Event]) -> String {
    let mut digest = Sha256::new();
    for event in events {
        digest.update(event.moment.to_le_bytes());
        digest.update(event.id.to_le_bytes());
        digest.update((event.data.len() as u64).to_le_bytes());
        digest.update(&event.data);
    }
    hex(&digest.finalize())
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn socket_pair() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [-1; 2];
    // SAFETY: `fds` is a live, writable array of two c_ints for the call.
    if unsafe { libc::socketpair(libc::AF_UNIX, SOCKET_TYPE, 0, fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: socketpair(2) succeeded, so both descriptors are open and owned
    // by nothing else.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

pub(crate) fn receive(socket: &OwnedFd, buffer: &mut [u8]) -> io::Result<usize> {
    loop {
        // SAFETY: `buffer` is live and writable for `buffer.len()` bytes;
        let received = unsafe {
            libc::recv(
                socket.as_raw_fd(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                libc::MSG_TRUNC,
            )
        };
        if received >= 0 {
            return Ok(received.unsigned_abs());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

pub(crate) fn send(socket: &OwnedFd, frame: &[u8]) -> io::Result<()> {
    loop {
        // SAFETY: `frame` is live and readable for `frame.len()` bytes.
        let sent = unsafe {
            libc::send(
                socket.as_raw_fd(),
                frame.as_ptr().cast(),
                frame.len(),
                SEND_FLAGS,
            )
        };
        if sent >= 0 {
            return if sent.unsigned_abs() == frame.len() {
                Ok(())
            } else {
                Err(io::Error::other("short response frame"))
            };
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Signal {
    Violation { id: u32, data: Vec<u8> },
    SnapshotPoint,
    Exhausted,
    Decision(Question),
}

#[derive(Debug)]
pub(crate) struct Reply {
    pub length: usize,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub signal: Option<Signal>,
}

#[derive(Clone, Debug)]
pub(crate) struct Services {
    entropy: SeededEntropy,
    env: RecordedEnv<Box<dyn ServiceHandler>>,
    coverage: BTreeMap<u32, u64>,
    pub(crate) events: Vec<Event>,
    pub(crate) latest: u64,
}

impl Services {
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn handler_fingerprint(&self) -> Result<HandlerFingerprint, ChannelError> {
        self.env.handler_fingerprint()
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn verify_handler_unchanged(
        &self,
        before: &HandlerFingerprint,
    ) -> Result<(), ChannelError> {
        self.env.verify_handler_fingerprint_unchanged(before)
    }

    pub(crate) fn new(seed: u64) -> Self {
        Self {
            entropy: SeededEntropy::new(seed),
            env: RecordedEnv::new(seed, Box::new(NominalHandler)),
            coverage: BTreeMap::new(),
            events: Vec::new(),
            latest: 0,
        }
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn branch(
        &mut self,
        seed: u64,
        handler: Box<dyn ServiceHandler>,
        payloads: Vec<Vec<u8>>,
        coverage_schedule: Option<CoverageSchedule>,
    ) -> Result<(), String> {
        let identity = handler.identity().to_vec();
        let configuration = handler.configuration().to_vec();
        let current_before = HandlerSnapshot::capture(self.env.handler().as_ref())
            .map_err(|error| format!("current service handler: {error}"))?;
        let candidate_before = HandlerSnapshot::capture(handler.as_ref())
            .map_err(|error| format!("service handler: {error}"))?;
        let decode_env = RecordedEnv::new(0, handler.clone());
        self.env
            .verify_handler_unchanged(&current_before)
            .map_err(|error| format!("current service handler: {error}"))?;
        if HandlerSnapshot::capture(handler.as_ref()).as_ref() != Ok(&candidate_before) {
            return Err("service handler changed while checking its reseed".to_owned());
        }
        if decode_env.handler().identity() != identity
            || decode_env.handler().configuration() != configuration
        {
            return Err("service handler identity or configuration changed on reseed".to_owned());
        }
        let mut env = RecordedEnv::new(seed, handler);
        self.env
            .verify_handler_unchanged(&current_before)
            .map_err(|error| format!("current service handler: {error}"))?;
        if env.handler().identity() != identity || env.handler().configuration() != configuration {
            return Err("service handler identity or configuration changed on reseed".to_owned());
        }
        env.set_payloads(Some(payloads))
            .map_err(|error| format!("payload tape: {error}"))?;
        env.set_coverage_schedule(coverage_schedule);
        self.env = env;
        self.entropy = SeededEntropy::new(seed);
        Ok(())
    }

    pub(crate) fn exchange(
        &mut self,
        request: &[u8],
        response: &mut [u8],
    ) -> Result<Reply, String> {
        let Some((stamp, frame)) = request.split_first_chunk::<STAMP_LEN>() else {
            return Err(format!("request of {} bytes", request.len()));
        };
        let moment = u64::from_le_bytes(*stamp);
        if moment < self.latest {
            return Err(format!(
                "virtual time went from {} to {moment} ns",
                self.latest
            ));
        }
        self.latest = moment;
        self.answer(moment, frame, response)
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn prepare_resolution(
        &self,
        request: &[u8],
        moment: u64,
        service: u16,
        id: u64,
        answer: &[u8],
    ) -> Result<(Self, Vec<u8>), String> {
        if answer.len() > MAX_PAYLOAD {
            return Err("service resolution exceeds the bridge frame".into());
        }
        let mut candidate = self.clone();
        let mut response = vec![0; MAX_FRAME];
        let probe = candidate.exchange(request, &mut response)?;
        let Some(Signal::Decision(question)) = probe.signal else {
            return Err("the held request is not a service decision".into());
        };
        let Some((stamp, _)) = request.split_first_chunk::<STAMP_LEN>() else {
            return Err("held request has no virtual-time stamp".into());
        };
        if u64::from_le_bytes(*stamp) != moment
            || question.service() != service
            || question.request_id() != id
        {
            return Err("service resolution does not match the held question".into());
        }
        let answer = Answer::decode(answer).map_err(|error| format!("service answer: {error}"))?;
        candidate.env.record_question(moment, &question, answer);
        candidate
            .env
            .snapshot_state_with_binding_reserve()
            .map_err(|error| format!("recorded service answer: {error}"))?;
        let reply = candidate.exchange(request, &mut response)?;
        if reply.signal.is_some() {
            return Err("recorded service answer left the guest waiting".into());
        }
        let (header, _) = decode(&response[..reply.length])
            .map_err(|error| format!("service response: {error}"))?;
        if header.status != Status::Ok as u16 {
            return Err(format!("service answer was rejected: {}", header.status));
        }
        response.truncate(reply.length);
        Ok((candidate, response))
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn exchange_guarded(
        &mut self,
        check: impl Fn() -> Result<(), String>,
        request: &[u8],
        response: &mut [u8],
    ) -> Result<Reply, String> {
        check()?;
        let result = self.exchange(request, response);
        check()?;
        result
    }

    fn answer(
        &mut self,
        moment: u64,
        request: &[u8],
        response: &mut [u8],
    ) -> Result<Reply, String> {
        let reply = |length| Reply {
            length,
            signal: None,
        };
        let Ok((header, payload)) = decode(request) else {
            return Ok(reply(encode_error(
                ServiceId::Event as u16,
                1,
                0,
                Status::BadRequest,
                response,
            )));
        };
        let refuse = |status, response: &mut [u8]| {
            encode_error(header.service, header.opcode, header.seq, status, response)
        };
        if !header.is_request() {
            return Ok(reply(refuse(Status::BadRequest, response)));
        }
        let respond = |service, status, body: &[u8], response: &mut [u8]| {
            encode_response(service, header.opcode, header.seq, status, body, response).unwrap_or(0)
        };
        match (header.service, header.opcode) {
            (service, opcode) if service == ServiceId::Entropy as u16 => {
                let mut body = [0_u8; MAX_PAYLOAD];
                let (status, length) = self.entropy.handle(opcode, payload, &mut body);
                Ok(reply(respond(
                    ServiceId::Entropy,
                    status,
                    &body[..length],
                    response,
                )))
            }
            (service, 1) if service == ServiceId::Event as u16 => {
                Ok(self.event(moment, payload, response, refuse))
            }
            (service, 3) if service == ServiceId::Sdk as u16 => {
                match sdk::decide_service(&mut self.env, moment, payload) {
                    Ok(ServiceOutcome::Answered(body)) => {
                        Ok(reply(respond(ServiceId::Sdk, Status::Ok, &body, response)))
                    }
                    Ok(ServiceOutcome::External(question)) => Ok(Reply {
                        length: 0,
                        signal: Some(Signal::Decision(question)),
                    }),
                    Err(status) => Ok(reply(refuse(status, response))),
                }
            }
            (service, 2) if service == ServiceId::Sdk as u16 => {
                let decided =
                    sdk::coverage_request(payload).and_then(|(thread, observed, ready)| {
                        sdk::decide_coverage(
                            &mut self.env,
                            &mut self.coverage,
                            moment,
                            thread,
                            observed,
                            ready,
                        )
                    });
                Ok(reply(match decided {
                    Ok(sdk::CoverageOutcome::Answered(coverage)) => {
                        respond(ServiceId::Sdk, Status::Ok, &coverage.encode(), response)
                    }
                    Ok(sdk::CoverageOutcome::External(question)) => {
                        return Ok(Reply {
                            length: 0,
                            signal: Some(Signal::Decision(question)),
                        });
                    }
                    Err(status) => respond(ServiceId::Sdk, status, &[], response),
                }))
            }
            (service, 1)
                if service == ServiceId::Payload as u16 && self.env.payload_configured() =>
            {
                Ok(match sdk::pull_payload(&mut self.env, payload) {
                    Ok(Some(entry)) => {
                        reply(respond(ServiceId::Payload, Status::Ok, &entry, response))
                    }
                    Ok(None) => Reply {
                        length: respond(ServiceId::Payload, Status::OutOfRange, &[], response),
                        signal: Some(Signal::Exhausted),
                    },
                    Err(status) => reply(respond(ServiceId::Payload, status, &[], response)),
                })
            }
            (service, _)
                if service == ServiceId::Event as u16
                    || service == ServiceId::Sdk as u16
                    || (service == ServiceId::Payload as u16 && self.env.payload_configured()) =>
            {
                Ok(reply(refuse(Status::UnknownOpcode, response)))
            }
            _ => Ok(reply(refuse(Status::UnknownService, response))),
        }
    }

    fn event(
        &mut self,
        moment: u64,
        payload: &[u8],
        response: &mut [u8],
        refuse: impl Fn(Status, &mut [u8]) -> usize,
    ) -> Reply {
        let reply = |length, signal| Reply { length, signal };
        let Some((id, data)) = payload.split_first_chunk::<4>() else {
            return reply(refuse(Status::BadRequest, response), None);
        };
        let id = u32::from_le_bytes(*id);
        if id == observation::EVENT_ID
            && !observation::Descriptor::decode(data).is_ok_and(|region| region.len == 0)
        {
            return reply(refuse(Status::BadRequest, response), None);
        }
        let signal = match sdk::classify_event(id, data) {
            EventClass::Malformed => return reply(refuse(Status::BadRequest, response), None),
            EventClass::Violation { id, data } => Some(Signal::Violation { id, data }),
            EventClass::SnapshotPoint => Some(Signal::SnapshotPoint),
            EventClass::Recorded => None,
        };
        self.events.push(Event {
            moment,
            id,
            data: data.into(),
        });
        reply(refuse(Status::Ok, response), signal)
    }
}

#[cfg(target_os = "linux")]
impl Services {
    pub(crate) fn encode(&self) -> Result<Vec<u8>, String> {
        let recorded = self
            .env
            .snapshot_state()
            .map_err(|error| format!("service state: {error}"))?
            .encode();
        let mut out = SERVICES_MAGIC.to_vec();
        out.extend(self.latest.to_le_bytes());
        put_bytes(&mut out, &self.entropy.save_state());
        put_bytes(&mut out, &recorded);
        out.extend(len32(self.coverage.len()).to_le_bytes());
        for (thread, threshold) in &self.coverage {
            out.extend(thread.to_le_bytes());
            out.extend(threshold.to_le_bytes());
        }
        out.extend(len32(self.events.len()).to_le_bytes());
        for event in &self.events {
            out.extend(event.moment.to_le_bytes());
            out.extend(event.id.to_le_bytes());
            put_bytes(&mut out, &event.data);
        }
        Ok(out)
    }

    pub(crate) fn decode(bytes: &[u8], factory: &ServiceFactory) -> Result<Self, String> {
        let mut reader = Reader(bytes);
        if reader.take(SERVICES_MAGIC.len())? != SERVICES_MAGIC {
            return Err("not a bridge state".to_owned());
        }
        let latest = reader.u64()?;
        let mut entropy = SeededEntropy::new(0);
        entropy
            .restore_state(reader.bytes()?)
            .map_err(|error| format!("entropy state: {error:?}"))?;
        let recorded = RecordedState::decode(reader.bytes()?)
            .map_err(|error| format!("service state: {error}"))?;
        let config = ServiceConfig {
            identity: recorded.handler().identity().to_vec(),
            configuration: recorded.handler().configuration().to_vec(),
        };
        let handler = factory(&config).map_err(|error| format!("service handler: {error}"))?;
        let mut env = RecordedEnv::new(0, handler);
        recorded
            .restore_into(&mut env)
            .map_err(|error| format!("service state: {error}"))?;
        let mut coverage = BTreeMap::new();
        for _ in 0..reader.u32()? {
            let thread = reader.u32()?;
            coverage.insert(thread, reader.u64()?);
        }
        let count = reader.u32()?;
        let mut events = Vec::new();
        for _ in 0..count {
            events.push(Event {
                moment: reader.u64()?,
                id: reader.u32()?,
                data: reader.bytes()?.into(),
            });
        }
        if !reader.0.is_empty() {
            return Err("trailing bytes after the bridge state".to_owned());
        }
        Ok(Self {
            entropy,
            env,
            coverage,
            events,
            latest,
        })
    }
}

#[cfg(target_os = "linux")]
fn len32(length: usize) -> u32 {
    u32::try_from(length).unwrap_or(u32::MAX)
}

#[cfg(target_os = "linux")]
fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend(len32(bytes.len()).to_le_bytes());
    out.extend(bytes);
}

#[cfg(target_os = "linux")]
pub(crate) struct Reader<'a>(pub(crate) &'a [u8]);

#[cfg(target_os = "linux")]
impl<'a> Reader<'a> {
    pub(crate) fn take(&mut self, length: usize) -> Result<&'a [u8], String> {
        if length > self.0.len() {
            return Err("truncated bridge state".to_owned());
        }
        let (head, tail) = self.0.split_at(length);
        self.0 = tail;
        Ok(head)
    }

    fn u32(&mut self) -> Result<u32, String> {
        let mut bytes = [0_u8; 4];
        bytes.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(bytes))
    }

    pub(crate) fn u64(&mut self) -> Result<u64, String> {
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(bytes))
    }

    fn bytes(&mut self) -> Result<&'a [u8], String> {
        let length = self.u32()? as usize;
        self.take(length)
    }
}

pub(crate) fn serve(socket: OwnedFd, bridge: Bridge, state: Arc<AtomicU8>) -> BridgeLog {
    if bridge.cut == Some(0) {
        state.store(CUT, Ordering::Release);
        return BridgeLog {
            events: Vec::new(),
            failure: None,
            socket,
        };
    }
    let mut services = Services::new(bridge.seed);
    let mut request = vec![0_u8; STAMP_LEN + MAX_FRAME];
    let mut response = vec![0_u8; MAX_FRAME];
    let failure = loop {
        let length = match receive(&socket, &mut request) {
            Ok(0) => {
                state.store(CLOSED, Ordering::Release);
                break None;
            }
            Ok(length) if (STAMP_LEN..=request.len()).contains(&length) => length,
            Ok(length) => break Some(format!("request of {length} bytes")),
            Err(error) => break Some(format!("receive failed: {error}")),
        };
        let recorded = services.events.len();
        let answer = match services.exchange(&request[..length], &mut response) {
            Ok(Reply {
                signal: Some(Signal::Decision(question)),
                ..
            }) => {
                break Some(format!(
                    "unresolved service decision {}",
                    question.service()
                ));
            }
            Ok(answer) => answer.length,
            Err(failure) => break Some(failure),
        };
        if services.events.len() > recorded && bridge.cut == Some(services.events.len()) {
            #[cfg(test)]
            if bridge.seed == TEST_DELAY_CUT_SEED {
                TEST_CUT_READY.store(true, Ordering::Release);
                while TEST_HOLD_CUT.load(Ordering::Acquire) {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
            state.store(CUT, Ordering::Release);
            break None;
        }
        if let Err(error) = send(&socket, &response[..answer]) {
            break Some(format!("send failed: {error}"));
        }
    };
    if failure.is_some() {
        state.store(FAILED, Ordering::Release);
    }
    BridgeLog {
        events: services.events,
        failure,
        socket,
    }
}

#[cfg(test)]
mod branch_isolation_tests {
    use super::*;
    use environment::channel::{
        Answer, ChannelError, MAX_CHANNEL_BYTES, Question, ServiceResponse,
    };
    use hypercall_proto::encode_request;

    #[test]
    fn branch_restarts_host_bridge_entropy_only_after_preflight() {
        fn draw(services: &mut Services) -> Vec<u8> {
            let mut request = [0_u8; MAX_FRAME];
            let length =
                encode_request(ServiceId::Entropy, 1, 7, &8_u32.to_le_bytes(), &mut request)
                    .unwrap();
            let mut response = [0_u8; MAX_FRAME];
            let reply = services
                .answer(1, &request[..length], &mut response)
                .unwrap();
            let (header, bytes) = decode(&response[..reply.length]).unwrap();
            assert_eq!(header.status, Status::Ok as u16);
            bytes.to_vec()
        }

        let mut services = Services::new(5);
        let first = draw(&mut services);
        let checkpoint = services.clone();

        let mut rejected = checkpoint.clone();
        assert!(
            rejected
                .branch(
                    7,
                    Box::new(NominalHandler),
                    vec![vec![0; MAX_CHANNEL_BYTES + 1]],
                    None,
                )
                .is_err()
        );
        let mut unchanged = checkpoint.clone();
        assert_eq!(draw(&mut rejected), draw(&mut unchanged));

        let mut seed_five = checkpoint.clone();
        seed_five
            .branch(5, Box::new(NominalHandler), Vec::new(), None)
            .unwrap();
        assert_eq!(draw(&mut seed_five), first);

        let mut seed_seven = checkpoint;
        seed_seven
            .branch(7, Box::new(NominalHandler), Vec::new(), None)
            .unwrap();
        let mut expected = Services::new(7);
        let seven = draw(&mut seed_seven);
        assert_eq!(seven, draw(&mut expected));
        assert_ne!(seven, first);
    }

    #[derive(Clone)]
    struct SharedReseedHandler {
        state: Arc<AtomicU8>,
    }

    impl ServiceHandler for SharedReseedHandler {
        fn identity(&self) -> &[u8] {
            b"shared-reseed"
        }

        fn configuration(&self) -> &[u8] {
            b""
        }

        fn respond(
            &mut self,
            _moment: u64,
            _question: &Question,
        ) -> Result<ServiceResponse, ChannelError> {
            Ok(ServiceResponse::Answered(Answer::Nominal))
        }

        fn snapshot_state(&self) -> Result<Vec<u8>, ChannelError> {
            Ok(vec![self.state.load(Ordering::SeqCst)])
        }

        fn restore_state(&mut self, state: &[u8]) -> Result<(), ChannelError> {
            let [byte] = state else {
                return Err(ChannelError::Malformed);
            };
            self.state.store(*byte, Ordering::SeqCst);
            Ok(())
        }

        fn reseed(&mut self, _seed: u64) {
            self.state.fetch_add(1, Ordering::SeqCst);
        }

        fn clone_box(&self) -> Box<dyn ServiceHandler> {
            Box::new(self.clone())
        }
    }

    #[test]
    fn rejected_branch_cannot_certify_mutated_current_handler() {
        let handler = SharedReseedHandler {
            state: Arc::new(AtomicU8::new(0)),
        };
        let mut services = Services::new(1);
        services.env = RecordedEnv::new(1, Box::new(handler.clone()));
        assert!(services.env.snapshot_state().is_ok());
        assert!(
            services
                .branch(
                    2,
                    Box::new(handler),
                    vec![vec![0; MAX_CHANNEL_BYTES + 1]],
                    None
                )
                .is_err()
        );
        assert_eq!(services.env.snapshot_state(), Err(ChannelError::Poisoned));
    }

    #[test]
    fn branch_rejects_a_shared_clone_changed_by_reseed_preflight() {
        let handler = SharedReseedHandler {
            state: Arc::new(AtomicU8::new(0)),
        };
        let mut services = Services::new(1);
        let before = services.env.snapshot_state().unwrap().encode();
        assert!(
            services
                .branch(2, Box::new(handler), Vec::new(), None)
                .is_err()
        );
        assert_eq!(services.env.snapshot_state().unwrap().encode(), before);
        assert!(
            services
                .branch(2, Box::new(NominalHandler), Vec::new(), None)
                .is_ok()
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use environment::channel::{Answer, ChannelError, Question, ServiceResponse};
    use hypercall_proto::encode_request;

    #[derive(Clone)]
    struct ReseedContractHandler {
        identity: Vec<u8>,
        configuration: Vec<u8>,
        change_identity: bool,
        zero_only: bool,
    }

    impl ReseedContractHandler {
        fn new(change_identity: bool) -> Self {
            Self {
                identity: b"before-identity".to_vec(),
                configuration: b"before-configuration".to_vec(),
                change_identity,
                zero_only: false,
            }
        }
    }

    impl ServiceHandler for ReseedContractHandler {
        fn identity(&self) -> &[u8] {
            &self.identity
        }

        fn configuration(&self) -> &[u8] {
            &self.configuration
        }

        fn respond(
            &mut self,
            _moment: u64,
            _question: &Question,
        ) -> Result<ServiceResponse, ChannelError> {
            Ok(ServiceResponse::Answered(Answer::Nominal))
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

        fn reseed(&mut self, seed: u64) {
            if self.change_identity {
                self.identity = if self.zero_only == (seed == 0) {
                    b"after-identity".to_vec()
                } else {
                    b"before-identity".to_vec()
                };
            } else {
                self.configuration = if self.zero_only == (seed == 0) {
                    b"after-configuration".to_vec()
                } else {
                    b"before-configuration".to_vec()
                };
            }
        }

        fn clone_box(&self) -> Box<dyn ServiceHandler> {
            Box::new(self.clone())
        }
    }

    #[test]
    fn branch_rejects_reseed_contract_drift_before_replacing_services() {
        for (change_identity, zero_only) in
            [(true, false), (false, false), (true, true), (false, true)]
        {
            let handler = ReseedContractHandler {
                zero_only,
                ..ReseedContractHandler::new(change_identity)
            };
            let mut source = Services::new(1);
            source.env = RecordedEnv::new(7, Box::new(handler.clone()) as Box<dyn ServiceHandler>);
            let encoded = source.encode().unwrap();
            let factory: ServiceFactory = Arc::new(move |config| {
                Ok(Box::new(ReseedContractHandler {
                    identity: config.identity.clone(),
                    configuration: config.configuration.clone(),
                    change_identity,
                    zero_only,
                }))
            });
            assert!(Services::decode(&encoded, &factory).is_err());

            let mut services = Services::new(1);
            let before = services.env.snapshot_state().unwrap().encode();
            assert!(
                services
                    .branch(7, Box::new(handler), Vec::new(), None)
                    .is_err()
            );
            assert_eq!(services.env.snapshot_state().unwrap().encode(), before);
            assert!(
                services
                    .branch(7, Box::new(NominalHandler), Vec::new(), None)
                    .is_ok()
            );
        }
    }

    #[test]
    fn unrecorded_custom_service_waits_before_user_mode_linux_answers() {
        let mut services = Services::new(1);
        services.env = RecordedEnv::new(
            1,
            Box::new(ReseedContractHandler::new(false)) as Box<dyn ServiceHandler>,
        );
        let mut payload = 19_u16.to_le_bytes().to_vec();
        payload.extend(7_u64.to_le_bytes());
        payload.extend(b"choose");
        let mut request = [0; MAX_FRAME];
        let length = encode_request(ServiceId::Sdk, 3, 1, &payload, &mut request).unwrap();
        let mut response = [0; MAX_FRAME];
        let reply = services
            .answer(4, &request[..length], &mut response)
            .unwrap();
        assert_eq!(reply.length, 0);
        assert!(matches!(
            reply.signal,
            Some(Signal::Decision(question)) if question.service() == 19 && question.request_id() == 7
        ));
        assert!(response.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn unrecorded_coverage_quantum_waits_before_user_mode_linux_answers() {
        let mut services = Services::new(1);
        services.env = RecordedEnv::new(
            1,
            Box::new(ReseedContractHandler::new(false)) as Box<dyn ServiceHandler>,
        );
        let mut payload = [0_u8; hypercall_proto::SDK_COVERAGE_REQUEST_LEN];
        payload[0..4].copy_from_slice(&7_u32.to_le_bytes());
        payload[4..12].copy_from_slice(&1_u64.to_le_bytes());
        payload[12..16].copy_from_slice(&2_u32.to_le_bytes());
        let mut request = [0; MAX_FRAME];
        let length = encode_request(ServiceId::Sdk, 2, 1, &payload, &mut request).unwrap();
        let mut response = [0; MAX_FRAME];
        let reply = services
            .answer(4, &request[..length], &mut response)
            .unwrap();
        assert_eq!(reply.length, 0);
        assert!(matches!(
            reply.signal,
            Some(Signal::Decision(question)) if question.service() == environment::channel::SERVICE_COVERAGE_QUANTUM && question.request_id() == 7
        ));
        assert!(response.iter().all(|byte| *byte == 0));
        assert!(services.coverage.is_empty());
        let mut stamped = [0; STAMP_LEN + MAX_FRAME];
        stamped[..STAMP_LEN].copy_from_slice(&4_u64.to_le_bytes());
        stamped[STAMP_LEN..STAMP_LEN + length].copy_from_slice(&request[..length]);
        let request = &stamped[..STAMP_LEN + length];
        let before = services.encode().unwrap();
        assert!(
            services
                .prepare_resolution(
                    request,
                    4,
                    environment::channel::SERVICE_COVERAGE_QUANTUM,
                    7,
                    &Answer::Data(0_u64.to_le_bytes().to_vec()).encode(),
                )
                .is_err()
        );
        assert_eq!(services.encode().unwrap(), before);
        let (resolved, reply) = services
            .prepare_resolution(
                request,
                4,
                environment::channel::SERVICE_COVERAGE_QUANTUM,
                7,
                &Answer::Data(64_u64.to_le_bytes().to_vec()).encode(),
            )
            .unwrap();
        let (header, body) = decode(&reply).unwrap();
        assert_eq!(header.status, Status::Ok as u16);
        assert_eq!(u64::from_le_bytes(body[..8].try_into().unwrap()), 65);
        assert_eq!(resolved.coverage.get(&7), Some(&65));
    }

    #[test]
    fn resolution_records_before_releasing_a_held_custom_service() {
        let mut services = Services::new(1);
        services.env = RecordedEnv::new(
            1,
            Box::new(ReseedContractHandler::new(false)) as Box<dyn ServiceHandler>,
        );
        let mut payload = 19_u16.to_le_bytes().to_vec();
        payload.extend(7_u64.to_le_bytes());
        payload.extend(b"choose");
        let mut request = [0; STAMP_LEN + MAX_FRAME];
        request[..STAMP_LEN].copy_from_slice(&4_u64.to_le_bytes());
        let length =
            encode_request(ServiceId::Sdk, 3, 1, &payload, &mut request[STAMP_LEN..]).unwrap();
        let request = &request[..STAMP_LEN + length];
        let before = services.encode().unwrap();
        let answer = Answer::Data(vec![42]).encode();
        assert!(
            services
                .prepare_resolution(request, 4, 19, 8, &answer)
                .is_err()
        );
        assert!(
            services
                .prepare_resolution(request, 4, 19, 7, &[9])
                .is_err()
        );
        assert_eq!(services.encode().unwrap(), before);
        let (resolved, reply) = services
            .prepare_resolution(request, 4, 19, 7, &answer)
            .unwrap();
        let (header, body) = decode(&reply).unwrap();
        assert_eq!(header.status, Status::Ok as u16);
        assert_eq!(body, &[1, 42]);
        assert_eq!(resolved.env.snapshot_state().unwrap().answers().count(), 1);
        let saved = resolved.encode().unwrap();
        let factory: ServiceFactory = Arc::new(|config| {
            Ok(Box::new(ReseedContractHandler {
                identity: config.identity.clone(),
                configuration: config.configuration.clone(),
                change_identity: false,
                zero_only: true,
            }))
        });
        let restored = Services::decode(&saved, &factory).unwrap();
        assert_eq!(restored.encode().unwrap(), saved);
    }

    #[test]
    fn coverage_schedule_answers_without_a_decision_and_survives_bridge_state() {
        let mut services = Services::new(1);
        let schedule = CoverageSchedule::new(1024, vec![(10, 20, 1)]).unwrap();
        services.env.set_coverage_schedule(Some(schedule.clone()));
        let mut payload = [0; hypercall_proto::SDK_COVERAGE_REQUEST_LEN];
        payload[..4].copy_from_slice(&7_u32.to_le_bytes());
        payload[4..12].copy_from_slice(&1_u64.to_le_bytes());
        payload[12..16].copy_from_slice(&1_u32.to_le_bytes());
        let mut request = [0; STAMP_LEN + MAX_FRAME];
        request[..STAMP_LEN].copy_from_slice(&10_u64.to_le_bytes());
        let length =
            encode_request(ServiceId::Sdk, 2, 1, &payload, &mut request[STAMP_LEN..]).unwrap();
        let mut response = [0; MAX_FRAME];
        let reply = services
            .exchange(&request[..STAMP_LEN + length], &mut response)
            .unwrap();
        assert!(reply.signal.is_none());
        let (header, body) = decode(&response[..reply.length]).unwrap();
        assert_eq!(header.status, Status::Ok as u16);
        assert_eq!(u64::from_le_bytes(body[..8].try_into().unwrap()), 2);
        assert_eq!(services.env.snapshot_state().unwrap().answers().count(), 0);
        let encoded = services.encode().unwrap();
        let restored =
            Services::decode(&encoded, &environment::input_spec::nominal_factory()).unwrap();
        assert_eq!(restored.env.coverage_schedule(), Some(&schedule));
        assert_eq!(restored.encode().unwrap(), encoded);
    }

    fn exchange(guest: &OwnedFd, moment: u64, service: ServiceId, payload: &[u8]) -> Vec<u8> {
        let mut frame = vec![0_u8; STAMP_LEN + MAX_FRAME];
        frame[..STAMP_LEN].copy_from_slice(&moment.to_le_bytes());
        let length = encode_request(service, 1, 7, payload, &mut frame[STAMP_LEN..]).unwrap();
        send(guest, &frame[..STAMP_LEN + length]).unwrap();
        let mut reply = vec![0_u8; MAX_FRAME];
        let length = receive(guest, &mut reply).unwrap();
        reply.truncate(length);
        reply
    }

    fn start(bridge: Bridge) -> (OwnedFd, Arc<AtomicU8>, std::thread::JoinHandle<BridgeLog>) {
        let (host, guest) = socket_pair().unwrap();
        let state = Arc::new(AtomicU8::new(RUNNING));
        let thread = {
            let state = Arc::clone(&state);
            std::thread::spawn(move || serve(host, bridge, state))
        };
        (guest, state, thread)
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn records_events_and_answers_seeded_entropy() {
        let (guest, state, thread) = start(Bridge::new(5));
        let reply = exchange(&guest, 10, ServiceId::Entropy, &8_u32.to_le_bytes());
        let (header, drawn) = decode(&reply).unwrap();
        assert_eq!(header.status, Status::Ok as u16);
        let mut expected = [0_u8; 8];
        SeededEntropy::new(5).handle(1, &8_u32.to_le_bytes(), &mut expected);
        assert_eq!(drawn, expected);
        let reply = exchange(&guest, 20, ServiceId::Event, b"\x00\x00\x00\x00{}");
        assert_eq!(decode(&reply).unwrap().0.status, Status::Ok as u16);
        let reply = exchange(&guest, 20, ServiceId::Sdk, b"");
        assert_eq!(
            decode(&reply).unwrap().0.status,
            Status::UnknownOpcode as u16
        );
        let reply = exchange(&guest, 20, ServiceId::Payload, &8_u32.to_le_bytes());
        assert_eq!(
            decode(&reply).unwrap().0.status,
            Status::UnknownService as u16
        );
        drop(guest);
        let log = thread.join().unwrap();
        assert_eq!(state.load(Ordering::Acquire), CLOSED);
        assert!(log.failure.is_none());
        assert_eq!(
            log.events,
            vec![Event {
                moment: 20,
                id: 0,
                data: b"{}"[..].into()
            }]
        );
        assert_ne!(event_hash(&log.events), event_hash(&[]));
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn cut_stops_answering_at_the_event() {
        let (guest, state, thread) = start(Bridge {
            seed: 1,
            cut: Some(2),
        });
        exchange(&guest, 1, ServiceId::Event, b"\x00\x00\x00\x00{}");
        let mut frame = vec![0_u8; STAMP_LEN + MAX_FRAME];
        frame[..STAMP_LEN].copy_from_slice(&2_u64.to_le_bytes());
        let length = encode_request(
            ServiceId::Event,
            1,
            8,
            b"\x00\x00\x00\x00{}",
            &mut frame[STAMP_LEN..],
        )
        .unwrap();
        send(&guest, &frame[..STAMP_LEN + length]).unwrap();
        let log = thread.join().unwrap();
        assert_eq!(state.load(Ordering::Acquire), CUT);
        assert_eq!(log.events.len(), 2);
        assert!(log.failure.is_none());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn zero_event_cut_stops_before_the_first_request() {
        let (host, guest) = socket_pair().unwrap();
        let state = Arc::new(AtomicU8::new(RUNNING));
        let worker_state = Arc::clone(&state);
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            sender
                .send(serve(
                    host,
                    Bridge {
                        seed: 1,
                        cut: Some(0),
                    },
                    worker_state,
                ))
                .unwrap();
        });
        let first = receiver.recv_timeout(std::time::Duration::from_secs(5));
        let stopped_without_request = first.is_ok();
        drop(guest);
        let log = match first {
            Ok(log) => log,
            Err(_) => receiver
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap(),
        };
        worker.join().unwrap();
        assert!(
            stopped_without_request,
            "zero-event cut waited for a guest request"
        );
        assert_eq!(state.load(Ordering::Acquire), CUT);
        assert!(log.events.is_empty());
        assert!(log.failure.is_none());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn time_going_backwards_fails_the_bridge() {
        let (guest, state, thread) = start(Bridge::new(1));
        exchange(&guest, 9, ServiceId::Event, b"\x00\x00\x00\x00{}");
        let mut frame = vec![0_u8; STAMP_LEN + MAX_FRAME];
        frame[..STAMP_LEN].copy_from_slice(&8_u64.to_le_bytes());
        let length = encode_request(ServiceId::Event, 1, 9, b"", &mut frame[STAMP_LEN..]).unwrap();
        send(&guest, &frame[..STAMP_LEN + length]).unwrap();
        let log = thread.join().unwrap();
        assert_eq!(state.load(Ordering::Acquire), FAILED);
        assert!(log.failure.unwrap().contains("from 9 to 8"));
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod loader_epoch_tests {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::process::Command;

    use environment::channel::{Answer, ChannelError, Question, ServiceResponse};
    use hypercall_proto::{ServiceId, encode_request};

    use super::*;

    #[derive(Clone)]
    struct TransientHandler(CString);

    impl TransientHandler {
        fn churn(&self) -> Result<libc::c_int, ChannelError> {
            // SAFETY: the NUL-terminated path names the test dylib and remains live for dlopen.
            let handle =
                unsafe { libc::dlopen(self.0.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
            if handle.is_null() {
                return Err(ChannelError::Malformed);
            }
            // SAFETY: the NUL-terminated symbol names the no-argument C function in this dylib.
            let symbol = unsafe { libc::dlsym(handle, c"harmony_loader_witness".as_ptr()) };
            if symbol.is_null() {
                // SAFETY: handle came from this successful dlopen and has not been closed.
                unsafe { libc::dlclose(handle) };
                return Err(ChannelError::Malformed);
            }
            // SAFETY: the compiled fixture exports this exact C signature at the named symbol.
            let call: unsafe extern "C" fn() -> libc::c_int =
                unsafe { std::mem::transmute(symbol) };
            // SAFETY: the function is live until the handle is closed below.
            let answer = unsafe { call() };
            // SAFETY: handle came from this successful dlopen and is closed once after the call.
            if unsafe { libc::dlclose(handle) } != 0 {
                return Err(ChannelError::Malformed);
            }
            Ok(answer)
        }
    }

    impl ServiceHandler for TransientHandler {
        fn identity(&self) -> &[u8] {
            b"transient-loader-witness"
        }

        fn configuration(&self) -> &[u8] {
            b""
        }

        fn respond(
            &mut self,
            _moment: u64,
            _question: &Question,
        ) -> Result<ServiceResponse, ChannelError> {
            Ok(ServiceResponse::Answered(Answer::Data(vec![
                self.churn()? as u8
            ])))
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
            assert_eq!(self.churn().unwrap(), 17);
            Box::new(self.clone())
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn transient_service_clone_rejects_exchange_without_loaded_identity_change() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("helper.c");
        let library = directory.path().join(if cfg!(target_os = "macos") {
            "helper.dylib"
        } else {
            "helper.so"
        });
        std::fs::write(&source, "int harmony_loader_witness(void) { return 17; }\n").unwrap();
        let mut compiler = Command::new("cc");
        if cfg!(target_os = "macos") {
            compiler.arg("-dynamiclib");
        } else {
            compiler.args(["-shared", "-fPIC"]);
        }
        assert!(
            compiler
                .arg("-o")
                .arg(&library)
                .arg(&source)
                .status()
                .unwrap()
                .success()
        );
        let handler = TransientHandler(CString::new(library.as_os_str().as_bytes()).unwrap());
        let mut services = Services::new(7);
        services
            .branch(7, Box::new(handler), Vec::new(), None)
            .unwrap();
        let mut request = vec![0_u8; STAMP_LEN + MAX_FRAME];
        request[..STAMP_LEN].copy_from_slice(&1_u64.to_le_bytes());
        let mut payload = Vec::from(100_u16.to_le_bytes());
        payload.extend_from_slice(&1_u64.to_le_bytes());
        let length =
            encode_request(ServiceId::Sdk, 3, 1, &payload, &mut request[STAMP_LEN..]).unwrap();
        request.truncate(STAMP_LEN + length);
        let mut response = vec![0_u8; MAX_FRAME];
        let loaded_before = host_code_identity::loaded_executable_identity("test-main").unwrap();
        let epoch = host_code_identity::loader_epoch().unwrap();
        let reply = services.exchange(&request, &mut response).unwrap();
        assert_eq!(reply.length, 0);
        assert!(matches!(reply.signal, Some(Signal::Decision(_))));
        assert_eq!(
            host_code_identity::loaded_executable_identity("test-main").unwrap(),
            loaded_before
        );
        assert_ne!(host_code_identity::loader_epoch().unwrap(), epoch);

        payload[2..10].copy_from_slice(&2_u64.to_le_bytes());
        let length =
            encode_request(ServiceId::Sdk, 3, 2, &payload, &mut request[STAMP_LEN..]).unwrap();
        request.truncate(STAMP_LEN + length);
        let guard = host_code_identity::loader_epoch().unwrap();
        assert!(matches!(
            services.exchange_guarded(
                || {
                    let actual = host_code_identity::loader_epoch().map_err(|error| error.to_string())?;
                    if actual != guard {
                        return Err("host loader changed after UML session admission".into());
                    }
                    Ok(())
                },
                &request,
                &mut response,
            ),
            Err(message) if message.contains("host loader changed")
        ));
        assert_eq!(
            host_code_identity::loaded_executable_identity("test-main").unwrap(),
            loaded_before
        );
        assert_ne!(host_code_identity::loader_epoch().unwrap(), guard);
    }
}
