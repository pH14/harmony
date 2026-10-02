// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use environment::channel::{NominalHandler, RecordedEnv, ServiceHandler};
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
    ) -> Result<(), String> {
        let mut env = RecordedEnv::new(seed, handler);
        env.set_payloads(Some(payloads))
            .map_err(|error| format!("payload tape: {error}"))?;
        self.env = env;
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
                    Ok(ServiceOutcome::External(question)) => Err(format!(
                        "service {} asked for a host decision, which User-mode Linux cannot pause for",
                        question.service()
                    )),
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
                    Ok(coverage) => {
                        respond(ServiceId::Sdk, Status::Ok, &coverage.encode(), response)
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
            Ok(answer) => answer.length,
            Err(failure) => break Some(failure),
        };
        if services.events.len() > recorded && bridge.cut == Some(services.events.len()) {
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use hypercall_proto::encode_request;

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
