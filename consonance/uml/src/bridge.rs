// SPDX-License-Identifier: AGPL-3.0-or-later

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use hypercall_proto::{
    HEADER_LEN, MAX_FRAME, SeededEntropy, Service, ServiceId, Status, decode, encode_error,
    encode_response,
};
use sha2::{Digest, Sha256};

const STAMP_LEN: usize = 8;

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
    pub data: Vec<u8>,
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

fn receive(socket: &OwnedFd, buffer: &mut [u8]) -> io::Result<usize> {
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

fn send(socket: &OwnedFd, frame: &[u8]) -> io::Result<()> {
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

struct Services {
    entropy: SeededEntropy,
    events: Vec<Event>,
}

impl Services {
    fn answer(&mut self, moment: u64, request: &[u8], response: &mut [u8]) -> usize {
        let Ok((header, payload)) = decode(request) else {
            return encode_error(ServiceId::Event as u16, 1, 0, Status::BadRequest, response);
        };
        if !header.is_request() {
            return encode_error(
                header.service,
                header.opcode,
                header.seq,
                Status::BadRequest,
                response,
            );
        }
        if header.service == ServiceId::Entropy as u16 {
            let mut body = [0_u8; MAX_FRAME - HEADER_LEN];
            let (status, length) = self.entropy.handle(header.opcode, payload, &mut body);
            return encode_response(
                ServiceId::Entropy,
                header.opcode,
                header.seq,
                status,
                &body[..length],
                response,
            )
            .unwrap_or(0);
        }
        if header.service == ServiceId::Event as u16 {
            let status = match (header.opcode, payload) {
                (1, [a, b, c, d, data @ ..]) => {
                    self.events.push(Event {
                        moment,
                        id: u32::from_le_bytes([*a, *b, *c, *d]),
                        data: data.to_vec(),
                    });
                    Status::Ok
                }
                (1, _) => Status::BadRequest,
                _ => Status::UnknownOpcode,
            };
            return encode_error(header.service, header.opcode, header.seq, status, response);
        }
        encode_error(
            header.service,
            header.opcode,
            header.seq,
            Status::UnknownService,
            response,
        )
    }
}

pub(crate) fn serve(socket: OwnedFd, bridge: Bridge, state: Arc<AtomicU8>) -> BridgeLog {
    let mut services = Services {
        entropy: SeededEntropy::new(bridge.seed),
        events: Vec::new(),
    };
    let mut request = vec![0_u8; STAMP_LEN + MAX_FRAME];
    let mut response = vec![0_u8; MAX_FRAME];
    let mut latest = 0;
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
        let (stamp, frame) = request[..length].split_at(STAMP_LEN);
        let moment = u64::from_le_bytes(stamp.try_into().unwrap_or_default());
        if moment < latest {
            break Some(format!("virtual time went from {latest} to {moment} ns"));
        }
        latest = moment;
        let recorded = services.events.len();
        let answer = services.answer(moment, frame, &mut response);
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
                data: b"{}".to_vec()
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
