// SPDX-License-Identifier: AGPL-3.0-or-later
use hypercall_proto::*;
use proptest::prelude::*;
use std::{cell::RefCell, rc::Rc};

fn enc_req(service: ServiceId, opcode: u16, seq: u32, payload: &[u8]) -> Vec<u8> {
    let mut buf = [0_u8; MAX_FRAME];
    let len = encode_request(service, opcode, seq, payload, &mut buf).unwrap();
    buf[..len].to_vec()
}

fn enc_resp(service: ServiceId, opcode: u16, seq: u32, status: Status, payload: &[u8]) -> Vec<u8> {
    let mut buf = [0_u8; MAX_FRAME];
    let len = encode_response(service, opcode, seq, status, payload, &mut buf).unwrap();
    buf[..len].to_vec()
}

fn le32(v: u32) -> [u8; 4] {
    v.to_le_bytes()
}

fn le64(v: u64) -> [u8; 8] {
    v.to_le_bytes()
}

#[test]
fn golden_request_bytes_for_every_service_opcode() {
    let mut expected = Vec::new();
    expected.extend_from_slice(b"HCP1");
    expected.extend_from_slice(&1_u16.to_le_bytes());
    expected.extend_from_slice(&1_u16.to_le_bytes());
    expected.extend_from_slice(&1_u16.to_le_bytes());
    expected.extend_from_slice(&0_u16.to_le_bytes());
    expected.extend_from_slice(&7_u32.to_le_bytes());
    expected.extend_from_slice(&3_u32.to_le_bytes());
    expected.extend_from_slice(&0_u32.to_le_bytes());
    expected.extend_from_slice(b"abc");
    assert_eq!(enc_req(ServiceId::Console, 1, 7, b"abc"), expected);

    let mut entropy = b"HCP1".to_vec();
    entropy.extend_from_slice(&[1, 0, 2, 0, 1, 0, 0, 0]);
    entropy.extend_from_slice(&le32(8));
    entropy.extend_from_slice(&le32(4));
    entropy.extend_from_slice(&le32(0));
    entropy.extend_from_slice(&le32(16));
    assert_eq!(enc_req(ServiceId::Entropy, 1, 8, &le32(16)), entropy);

    let mut cap = b"HCP1".to_vec();
    cap.extend_from_slice(&[1, 0, 3, 0, 1, 0, 0, 0]);
    cap.extend_from_slice(&le32(9));
    cap.extend_from_slice(&le32(0));
    cap.extend_from_slice(&le32(0));
    assert_eq!(enc_req(ServiceId::Block, 1, 9, &[]), cap);

    let mut read_payload = Vec::new();
    read_payload.extend_from_slice(&le64(5));
    read_payload.extend_from_slice(&le32(2));
    let mut read = b"HCP1".to_vec();
    read.extend_from_slice(&[1, 0, 3, 0, 2, 0, 0, 0]);
    read.extend_from_slice(&le32(10));
    read.extend_from_slice(&le32(12));
    read.extend_from_slice(&le32(0));
    read.extend_from_slice(&read_payload);
    assert_eq!(enc_req(ServiceId::Block, 2, 10, &read_payload), read);

    let mut event_payload = Vec::new();
    event_payload.extend_from_slice(&le32(42));
    event_payload.extend_from_slice(b"evt");
    let mut event = b"HCP1".to_vec();
    event.extend_from_slice(&[1, 0, 4, 0, 1, 0, 0, 0]);
    event.extend_from_slice(&le32(11));
    event.extend_from_slice(&le32(7));
    event.extend_from_slice(&le32(0));
    event.extend_from_slice(&event_payload);
    assert_eq!(enc_req(ServiceId::Event, 1, 11, &event_payload), event);

    let mut service_payload = le32(7).to_vec();
    service_payload.extend_from_slice(&le64(50));
    service_payload.extend_from_slice(&[0xa5, 0x5a]);
    let mut sdk = b"HCP1".to_vec();
    sdk.extend_from_slice(&[1, 0, 6, 0, 3, 0, 0, 0]);
    sdk.extend_from_slice(&le32(12));
    sdk.extend_from_slice(&le32(service_payload.len() as u32));
    sdk.extend_from_slice(&le32(0));
    sdk.extend_from_slice(&service_payload);
    assert_eq!(enc_req(ServiceId::Sdk, 3, 12, &service_payload), sdk);

    let mut coverage_payload = Vec::new();
    coverage_payload.extend_from_slice(&le32(7));
    coverage_payload.extend_from_slice(&le64(1));
    coverage_payload.extend_from_slice(&le32(3));
    let mut coverage = b"HCP1".to_vec();
    coverage.extend_from_slice(&[1, 0, 6, 0, 2, 0, 0, 0]);
    coverage.extend_from_slice(&le32(13));
    coverage.extend_from_slice(&le32(SDK_COVERAGE_REQUEST_LEN as u32));
    coverage.extend_from_slice(&le32(0));
    coverage.extend_from_slice(&coverage_payload);
    assert_eq!(enc_req(ServiceId::Sdk, 2, 13, &coverage_payload), coverage);

    let mut payload = Vec::new();
    payload.extend_from_slice(b"HCP1");
    payload.extend_from_slice(&1_u16.to_le_bytes());
    payload.extend_from_slice(&(ServiceId::Payload as u16).to_le_bytes());
    payload.extend_from_slice(&1_u16.to_le_bytes());
    payload.extend_from_slice(&0_u16.to_le_bytes());
    payload.extend_from_slice(&14_u32.to_le_bytes());
    payload.extend_from_slice(&le32(4));
    payload.extend_from_slice(&0_u32.to_le_bytes());
    payload.extend_from_slice(&le32(2));
    assert_eq!(enc_req(ServiceId::Payload, 1, 14, &le32(2)), payload);
}

#[test]
fn golden_response_bytes_for_every_service_opcode() {
    let cases = [
        (ServiceId::Console, 1, 1, Status::Ok, Vec::new()),
        (ServiceId::Entropy, 1, 2, Status::Ok, vec![1, 2, 3, 4]),
        (ServiceId::Block, 1, 3, Status::Ok, le64(99).to_vec()),
        (ServiceId::Block, 2, 4, Status::OutOfRange, Vec::new()),
        (ServiceId::Event, 1, 5, Status::Ok, Vec::new()),
        (ServiceId::Sdk, 3, 6, Status::Ok, vec![1, 0xa5]),
        (
            ServiceId::Sdk,
            2,
            7,
            Status::Ok,
            [le64(2).as_slice(), le32(1).as_slice()].concat(),
        ),
    ];
    for (service, opcode, seq, status, payload) in cases {
        let got = enc_resp(service, opcode, seq, status, &payload);
        assert_eq!(&got[0..4], b"HCP1");
        assert_eq!(&got[4..6], &2_u16.to_le_bytes());
        assert_eq!(&got[6..8], &(service as u16).to_le_bytes());
        assert_eq!(&got[8..10], &opcode.to_le_bytes());
        assert_eq!(&got[10..12], &(status as u16).to_le_bytes());
        assert_eq!(&got[12..16], &seq.to_le_bytes());
        assert_eq!(&got[16..20], &(payload.len() as u32).to_le_bytes());
        assert_eq!(&got[20..24], &0_u32.to_le_bytes());
        assert_eq!(&got[24..], payload.as_slice());
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn round_trip_valid_frames(service in 1_u16..=4, opcode in any::<u16>(), seq in any::<u32>(), payload in proptest::collection::vec(any::<u8>(), 0..=MAX_PAYLOAD)) {
        let service = match service {
            1 => ServiceId::Console,
            2 => ServiceId::Entropy,
            3 => ServiceId::Block,
            _ => ServiceId::Event,
        };
        let mut buf = [0_u8; MAX_FRAME];
        let len = encode_request(service, opcode, seq, &payload, &mut buf)?;
        let (header, decoded) = decode(&buf[..len])?;
        prop_assert_eq!(header.kind, 1);
        prop_assert_eq!(header.service, service as u16);
        prop_assert_eq!(header.opcode, opcode);
        prop_assert_eq!(header.seq, seq);
        prop_assert_eq!(decoded, payload.as_slice());
    }

    #[test]
    fn malformed_decode_and_dispatch_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..=5000), resp_size in 0_usize..=MAX_FRAME) {
        let _ = decode(&bytes);
        let mut dispatcher = test_dispatcher(1);
        let mut resp = vec![0_u8; resp_size];
        let len = dispatcher.dispatch(&bytes, &mut resp);
        if resp_size < 24 {
            prop_assert_eq!(len, 0);
        } else {
            prop_assert!(len >= 24);
            prop_assert!(len <= resp_size);
            let (header, payload) = decode(&resp[..len])?;
            prop_assert_eq!(header.kind, 2);
            prop_assert!(payload.len() <= MAX_PAYLOAD);
        }
    }

    #[test]
    fn single_byte_mutations_never_panic(mut payload in proptest::collection::vec(any::<u8>(), 0..=64), index in 0_usize..128, value in any::<u8>(), resp_size in 0_usize..=MAX_FRAME) {
        let mut frame = enc_req(ServiceId::Console, 1, 123, &payload);
        if index < frame.len() {
            frame[index] = value;
        } else {
            payload.push(value);
            frame.extend_from_slice(&payload[payload.len() - 1..]);
        }
        let _ = decode(&frame);
        let mut dispatcher = test_dispatcher(2);
        let mut resp = vec![0_u8; resp_size];
        let len = dispatcher.dispatch(&frame, &mut resp);
        if resp_size < 24 {
            prop_assert_eq!(len, 0);
        } else {
            prop_assert!(len >= 24);
            prop_assert!(len <= resp_size);
        }
    }
}

fn test_dispatcher(seed: u64) -> Dispatcher {
    let mut dispatcher = Dispatcher::new();
    dispatcher.register(ServiceId::Console, Box::new(ConsoleSink::new()));
    dispatcher.register(ServiceId::Entropy, Box::new(SeededEntropy::new(seed)));
    dispatcher.register(
        ServiceId::Block,
        Box::new(MemBlockDevice::new((0_u8..=255).cycle().take(4096).collect()).unwrap()),
    );
    dispatcher.register(ServiceId::Event, Box::new(EventSink::new()));
    dispatcher
}

struct Loopback {
    dispatcher: Dispatcher,
    transcript: Rc<RefCell<Vec<u8>>>,
}

impl Loopback {
    fn new(seed: u64) -> (Self, Rc<RefCell<Vec<u8>>>) {
        let transcript = Rc::new(RefCell::new(Vec::new()));
        (
            Self {
                dispatcher: test_dispatcher(seed),
                transcript: Rc::clone(&transcript),
            },
            transcript,
        )
    }
}

impl Transport for Loopback {
    type Error = ();

    fn exchange(&mut self, req: &[u8], resp: &mut [u8]) -> Result<usize, Self::Error> {
        let mut transcript = self.transcript.borrow_mut();
        transcript.extend_from_slice(&(req.len() as u32).to_le_bytes());
        transcript.extend_from_slice(req);
        let len = self.dispatcher.dispatch(req, resp);
        transcript.extend_from_slice(&(len as u32).to_le_bytes());
        transcript.extend_from_slice(&resp[..len]);
        Ok(len)
    }
}

fn run_session(seed: u64) -> Vec<u8> {
    let (loopback, transcript) = Loopback::new(seed);
    let mut client = Client::new(loopback);
    client.console_write(b"hello").unwrap();
    let mut entropy = vec![0_u8; MAX_PAYLOAD + 17];
    client.entropy_fill(&mut entropy).unwrap();
    assert_eq!(client.block_capacity().unwrap(), 8);
    let mut block = vec![0_u8; 4096];
    client.block_read(0, &mut block).unwrap();
    client.event_emit(7, b"event data").unwrap();
    drop(client);
    transcript.borrow().clone()
}

#[test]
fn end_to_end_loopback_and_identical_transcripts() {
    let a = run_session(0xabc);
    let b = run_session(0xabc);
    assert_eq!(a, b);
}

/// A bare loopback that services one preconfigured dispatcher.
struct DispatcherLoopback(Dispatcher);

impl Transport for DispatcherLoopback {
    type Error = ();
    fn exchange(&mut self, req: &[u8], resp: &mut [u8]) -> Result<usize, Self::Error> {
        Ok(self.0.dispatch(req, resp))
    }
}

/// Host response seam for pinning each half of the coverage response guard.
struct CoverageResponseTransport {
    next: u64,
    selected: u32,
}

impl Transport for CoverageResponseTransport {
    type Error = ();

    fn exchange(&mut self, req: &[u8], resp: &mut [u8]) -> Result<usize, Self::Error> {
        let (header, _) = decode(req).map_err(|_| ())?;
        let mut payload = [0_u8; SDK_COVERAGE_RESPONSE_LEN];
        payload[0..8].copy_from_slice(&self.next.to_le_bytes());
        payload[8..12].copy_from_slice(&self.selected.to_le_bytes());
        encode_response(ServiceId::Sdk, 2, header.seq, Status::Ok, &payload, resp).map_err(|_| ())
    }
}

#[derive(Clone)]
struct OnePayload(Option<Vec<u8>>);

impl Service for OnePayload {
    fn handle(&mut self, opcode: u16, payload: &[u8], resp_payload: &mut [u8]) -> (Status, usize) {
        if opcode != 1 || payload.len() != 4 {
            return (Status::BadRequest, 0);
        }
        let requested = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
        let Some(entry) = self.0.as_ref() else {
            return (Status::OutOfRange, 0);
        };
        if entry.len() as u64 != u64::from(requested) || resp_payload.len() < entry.len() {
            return (Status::BadRequest, 0);
        }
        resp_payload[..entry.len()].copy_from_slice(entry);
        let len = entry.len();
        self.0 = None;
        (Status::Ok, len)
    }

    fn save_state(&self) -> Vec<u8> {
        self.0.clone().unwrap_or_default()
    }

    fn restore_state(&mut self, state: &[u8]) -> Result<(), ProtoError> {
        self.0 = (!state.is_empty()).then(|| state.to_vec());
        Ok(())
    }
}

#[test]
fn payload_fetch_is_exact_and_exhaustion_is_a_clean_status() {
    let mut dispatcher = Dispatcher::new();
    dispatcher.register(
        ServiceId::Payload,
        Box::new(OnePayload(Some(vec![0x81, 4]))),
    );
    let mut client = Client::new(DispatcherLoopback(dispatcher));
    let mut chord = [0_u8; 2];
    client.payload_fetch(&mut chord).unwrap();
    assert_eq!(chord, [0x81, 4]);
    assert_eq!(
        client.payload_fetch(&mut chord),
        Err(ClientError::Status(Status::OutOfRange))
    );
    assert_eq!(
        client.payload_fetch(&mut []),
        Err(ClientError::InvalidLength)
    );
    let mut oversized = vec![0_u8; MAX_PAYLOAD + 1];
    assert_eq!(
        client.payload_fetch(&mut oversized),
        Err(ClientError::InvalidLength)
    );

    let mut dispatcher = Dispatcher::new();
    dispatcher.register(
        ServiceId::Payload,
        Box::new(OnePayload(Some(vec![0xa5; MAX_PAYLOAD]))),
    );
    let mut client = Client::new(DispatcherLoopback(dispatcher));
    let mut maximum = vec![0_u8; MAX_PAYLOAD];
    client.payload_fetch(&mut maximum).unwrap();
    assert!(maximum.iter().all(|&byte| byte == 0xa5));
}

/// M6 threshold handshake: the first per-thread threshold is one, each exit
/// prescribes the next exact count, and the selected runnable is in range.
#[test]
fn coverage_yield_round_trips_threshold_and_scheduler_selection() {
    let mut dispatcher = Dispatcher::new();
    dispatcher.register(ServiceId::Sdk, Box::new(CoverageService::new()));
    let mut client = Client::new(DispatcherLoopback(dispatcher));

    assert_eq!(client.coverage_yield(7, 1, 3).unwrap(), (2, 0));
    assert_eq!(client.coverage_yield(7, 2, 3).unwrap(), (3, 2));
    assert_eq!(client.coverage_yield(9, 1, 2).unwrap(), (2, 0));
}

/// A wrong counter is the planted protocol negative: it must fail before a
/// scheduling answer is minted, proving the previous-exit threshold is
/// load-bearing rather than advisory.
#[test]
fn coverage_yield_rejects_skipped_stale_and_invalid_thresholds() {
    let mut dispatcher = Dispatcher::new();
    dispatcher.register(ServiceId::Sdk, Box::new(CoverageService::new()));
    let mut client = Client::new(DispatcherLoopback(dispatcher));

    assert_eq!(
        client.coverage_yield(7, 2, 3),
        Err(ClientError::Status(Status::BadRequest))
    );
    assert_eq!(client.coverage_yield(7, 1, 3).unwrap(), (2, 0));
    assert_eq!(
        client.coverage_yield(7, 1, 3),
        Err(ClientError::Status(Status::BadRequest))
    );
    assert_eq!(
        client.coverage_yield(7, 2, 0),
        Err(ClientError::InvalidLength)
    );
}

/// Each response invariant is independently load-bearing: a host cannot hide
/// one malformed field behind a valid value in the other field.
#[test]
fn coverage_yield_rejects_each_malformed_response_field_independently() {
    for (next, selected) in [(7, 0), (8, 2)] {
        let mut client = Client::new(CoverageResponseTransport { next, selected });
        assert_eq!(
            client.coverage_yield(4, 7, 2),
            Err(ClientError::Protocol(ProtoError::BadPayload))
        );
    }
}

/// Request and response buffer lengths are separate protocol invariants.
#[test]
fn sdk_coverage_rejects_each_bad_buffer_length_independently() {
    let mut svc = CoverageService::new();
    let mut request = [0_u8; SDK_COVERAGE_REQUEST_LEN];
    request[0..4].copy_from_slice(&7_u32.to_le_bytes());
    request[4..12].copy_from_slice(&SDK_COVERAGE_QUANTUM.to_le_bytes());
    request[12..16].copy_from_slice(&2_u32.to_le_bytes());
    let mut response = [0_u8; SDK_COVERAGE_RESPONSE_LEN];

    assert_eq!(
        svc.handle(2, &request[..request.len() - 1], &mut response),
        (Status::BadRequest, 0)
    );
    assert_eq!(
        svc.handle(2, &request, &mut response[..SDK_COVERAGE_RESPONSE_LEN - 1]),
        (Status::BadRequest, 0)
    );
    assert!(svc.asked().is_empty());
}

/// Coverage state is the generic SDK reference service's only stateful policy.
#[test]
fn coverage_service_state_round_trips() {
    let mut service = CoverageService::new();
    let mut request = [0_u8; SDK_COVERAGE_REQUEST_LEN];
    request[0..4].copy_from_slice(&11_u32.to_le_bytes());
    request[4..12].copy_from_slice(&SDK_COVERAGE_QUANTUM.to_le_bytes());
    request[12..16].copy_from_slice(&2_u32.to_le_bytes());
    let mut response = [0_u8; SDK_COVERAGE_RESPONSE_LEN];
    assert_eq!(
        service.handle(2, &request, &mut response),
        (Status::Ok, SDK_COVERAGE_RESPONSE_LEN)
    );
    assert_eq!(service.asked(), [(11, SDK_COVERAGE_QUANTUM, 2, 0)]);
    let saved = service.save_state();
    let mut restored = CoverageService::new();
    restored.restore_state(&saved).unwrap();
    assert_eq!(restored, service);
    assert_eq!(restored.save_state(), saved);
}

/// A persisted runnable selection is checked even when `ready` is nonzero;
/// `selected == ready` is out of range and must fail closed.
#[test]
fn coverage_service_restore_rejects_out_of_range_selection() {
    let mut state = Vec::new();
    state.extend_from_slice(&0_u32.to_le_bytes());
    state.extend_from_slice(&1_u32.to_le_bytes());
    state.extend_from_slice(&3_u32.to_le_bytes());
    state.extend_from_slice(&5_u64.to_le_bytes());
    state.extend_from_slice(&2_u32.to_le_bytes());
    state.extend_from_slice(&2_u32.to_le_bytes());
    assert_eq!(
        CoverageService::new().restore_state(&state),
        Err(ProtoError::BadState)
    );
}

#[test]
fn snapshot_round_trip_restores_entropy_stream() {
    let mut dispatcher = test_dispatcher(55);
    let mut req = [0_u8; MAX_FRAME];
    let mut resp = [0_u8; MAX_FRAME];
    let payload = le32(20);
    let len = encode_request(ServiceId::Entropy, 1, 1, &payload, &mut req).unwrap();
    let _ = dispatcher.dispatch(&req[..len], &mut resp);
    let saved = dispatcher.save_state();

    let len2 = encode_request(ServiceId::Entropy, 1, 2, &payload, &mut req).unwrap();
    let resp_len = dispatcher.dispatch(&req[..len2], &mut resp);
    let expected = resp[..resp_len].to_vec();

    dispatcher.restore_state(&saved).unwrap();
    let resp_len = dispatcher.dispatch(&req[..len2], &mut resp);
    assert_eq!(resp[..resp_len].to_vec(), expected);

    let mut same = test_dispatcher(55);
    let _ = same.dispatch(&req[..len], &mut resp);
    assert_eq!(same.save_state(), saved);

    let mut mismatched = Dispatcher::new();
    mismatched.register(ServiceId::Entropy, Box::new(SeededEntropy::new(55)));
    assert!(mismatched.restore_state(&saved).is_err());
    assert!(dispatcher.restore_state(&[1, 2, 3]).is_err());
}

#[test]
fn malformed_dispatch_edge_cases() {
    let mut dispatcher = test_dispatcher(9);
    let mut resp = [0_u8; MAX_FRAME];
    assert_eq!(dispatcher.dispatch(b"short", &mut resp[..23]), 0);
    let len = dispatcher.dispatch(b"short", &mut resp);
    let (header, payload) = decode(&resp[..len]).unwrap();
    assert_eq!(header.service, 0);
    assert_eq!(header.opcode, 0);
    assert_eq!(header.seq, 0);
    assert_eq!(header.status, Status::BadRequest as u16);
    assert!(payload.is_empty());

    let mut bad_reserved = enc_req(ServiceId::Block, 1, 44, &[]);
    bad_reserved[20] = 1;
    let len = dispatcher.dispatch(&bad_reserved, &mut resp);
    let (header, _) = decode(&resp[..len]).unwrap();
    assert_eq!(header.service, ServiceId::Block as u16);
    assert_eq!(header.opcode, 1);
    assert_eq!(header.seq, 44);
    assert_eq!(header.status, Status::BadRequest as u16);

    let truncated = enc_req(ServiceId::Console, 1, 77, b"abcdef");
    let len = dispatcher.dispatch(&truncated[..truncated.len() - 3], &mut resp);
    let (header, payload) = decode(&resp[..len]).unwrap();
    assert_eq!(header.service, ServiceId::Console as u16);
    assert_eq!(header.opcode, 1);
    assert_eq!(header.seq, 77);
    assert_eq!(header.status, Status::BadRequest as u16);
    assert!(payload.is_empty());

    let entropy_req = enc_req(ServiceId::Entropy, 1, 78, &le32(64));
    let len = dispatcher.dispatch(&entropy_req, &mut resp[..32]);
    let (header, payload) = decode(&resp[..len]).unwrap();
    assert_eq!(header.service, ServiceId::Entropy as u16);
    assert_eq!(header.opcode, 1);
    assert_eq!(header.seq, 78);
    assert_eq!(header.status, Status::Internal as u16);
    assert!(payload.is_empty());
}

struct FixedLenTransport(usize);

impl Transport for FixedLenTransport {
    type Error = ();

    fn exchange(&mut self, _req: &[u8], _resp: &mut [u8]) -> Result<usize, ()> {
        Ok(self.0)
    }
}

#[test]
fn client_rejects_out_of_bounds_transport_length() {
    let mut client = Client::new(FixedLenTransport(MAX_FRAME + 904));
    assert_eq!(
        client.block_capacity(),
        Err(ClientError::Protocol(ProtoError::Truncated))
    );
}

struct CountingTransport {
    dispatcher: Dispatcher,
    frames: Rc<RefCell<usize>>,
}

impl Transport for CountingTransport {
    type Error = ();

    fn exchange(&mut self, req: &[u8], resp: &mut [u8]) -> Result<usize, ()> {
        *self.frames.borrow_mut() += 1;
        Ok(self.dispatcher.dispatch(req, resp))
    }
}

#[test]
fn event_emit_never_fragments() {
    let frames = Rc::new(RefCell::new(0_usize));
    let transport = CountingTransport {
        dispatcher: test_dispatcher(3),
        frames: Rc::clone(&frames),
    };
    let mut client = Client::new(transport);

    client.event_emit(7, &[0xa5; MAX_PAYLOAD - 4]).unwrap();
    assert_eq!(*frames.borrow(), 1);

    assert_eq!(
        client.event_emit(7, &[0xa5; MAX_PAYLOAD - 3]),
        Err(ClientError::InvalidLength)
    );
    assert_eq!(*frames.borrow(), 1);
}

#[test]
fn entropy_restore_rejects_zero_state() {
    let mut entropy = SeededEntropy::new(42);
    assert_eq!(entropy.restore_state(&[0_u8; 8]), Err(ProtoError::BadState));
    let mut out = [0_u8; 16];
    let (status, len) = entropy.handle(1, &le32(16), &mut out);
    assert_eq!(status, Status::Ok);
    assert_eq!(len, 16);
    assert_ne!(out, [0_u8; 16]);
}

#[test]
fn dispatcher_failed_restore_preserves_state() {
    let mut dispatcher = test_dispatcher(7);
    let mut req = [0_u8; MAX_FRAME];
    let mut resp = [0_u8; MAX_FRAME];
    let len = encode_request(ServiceId::Console, 1, 1, b"original", &mut req).unwrap();
    let _ = dispatcher.dispatch(&req[..len], &mut resp);
    let saved = dispatcher.save_state();

    let mut bad = Vec::new();
    bad.extend_from_slice(&(ServiceId::Console as u16).to_le_bytes());
    bad.extend_from_slice(&le32(8));
    bad.extend_from_slice(b"TAMPERED");
    bad.extend_from_slice(&(ServiceId::Entropy as u16).to_le_bytes());
    bad.extend_from_slice(&le32(3));
    bad.extend_from_slice(&[1, 2, 3]);

    assert_eq!(dispatcher.restore_state(&bad), Err(ProtoError::BadState));
    assert_eq!(dispatcher.save_state(), saved);
}

#[derive(Clone)]
struct OpaquePackageService;

impl Service for OpaquePackageService {
    fn handle(&mut self, opcode: u16, payload: &[u8], response: &mut [u8]) -> (Status, usize) {
        if opcode != 3 || payload.len() < 10 {
            return (Status::BadRequest, 0);
        }
        if response.len() < payload.len() - 10 + 1 {
            return (Status::Internal, 0);
        }
        response[0] = 1;
        response[1..payload.len() - 9].copy_from_slice(&payload[10..]);
        (Status::Ok, payload.len() - 9)
    }

    fn save_state(&self) -> Vec<u8> {
        Vec::new()
    }

    fn restore_state(&mut self, state: &[u8]) -> Result<(), ProtoError> {
        if state.is_empty() {
            Ok(())
        } else {
            Err(ProtoError::BadState)
        }
    }
}

#[test]
fn generic_service_request_round_trips_opaque_namespace_and_payload() {
    let mut dispatcher = Dispatcher::new();
    dispatcher.register(ServiceId::Sdk, Box::new(OpaquePackageService));
    let mut client = Client::new(DispatcherLoopback(dispatcher));
    let mut out = [0_u8; 8];
    let n = client
        .service_request(7, 0xDEAD_BEEF, &[0x11, 0x22, 0x33], &mut out)
        .unwrap()
        .unwrap();
    assert_eq!(n, 3);
    assert_eq!(&out[..n], &[0x11, 0x22, 0x33]);
}

#[test]
fn generic_service_request_preserves_buffer_and_namespace_guards() {
    let mut dispatcher = Dispatcher::new();
    dispatcher.register(ServiceId::Sdk, Box::new(OpaquePackageService));
    let mut client = Client::new(DispatcherLoopback(dispatcher));
    let mut out = [0_u8; 2];
    assert_eq!(
        client.service_request(7, 1, &[1, 2, 3], &mut out),
        Err(ClientError::Protocol(ProtoError::BufferTooSmall))
    );
    assert_eq!(
        client.service_request(3, 1, &[1], &mut out),
        Err(ClientError::InvalidLength)
    );
    assert_eq!(
        client.service_request(7, 1, &[1; MAX_PAYLOAD - 9], &mut out),
        Err(ClientError::InvalidLength)
    );
}

#[test]
fn retired_fault_service_id_has_no_package_decoder() {
    let mut dispatcher = Dispatcher::new();
    dispatcher.register(ServiceId::Net, Box::new(OpaquePackageService));
    let mut client = Client::new(DispatcherLoopback(dispatcher));
    let mut frame = [0_u8; 1];
    assert_eq!(
        client.service_request(7, 1, &[1, 2, 3, 4], &mut frame),
        Err(ClientError::Status(Status::UnknownService))
    );
    assert_eq!(ServiceId::Net as u16, 5);
    assert_eq!(ServiceId::Sdk as u16, 6);
}

/// The pvclock registration round-trip: the guest
/// `pvclock_register(gpa)` reaches the [`PvclockRegistrar`] service (id 7,
/// op 1), which validates the page-aligned in-RAM GPA, records it, and answers
/// the ABI version; a bad GPA is a clean status, never a silent accept.
#[test]
fn pvclock_register_round_trips_the_abi_version() {
    let fresh = || {
        let mut dispatcher = Dispatcher::new();
        dispatcher.register(
            ServiceId::Pvclock,
            Box::new(PvclockRegistrar::new(1 << 20, 1)),
        );
        Client::new(DispatcherLoopback(dispatcher))
    };
    let mut client = fresh();
    assert_eq!(
        client.pvclock_register(0x5001),
        Err(ClientError::Status(Status::OutOfRange))
    );
    assert_eq!(
        client.pvclock_register(1 << 20),
        Err(ClientError::Status(Status::OutOfRange))
    );
    assert_eq!(client.pvclock_register(0x5000).unwrap(), 1);
    assert_eq!(
        client.pvclock_register(0x5000),
        Err(ClientError::Status(Status::BadRequest))
    );
    assert_eq!(
        client.pvclock_register(0x6000),
        Err(ClientError::Status(Status::BadRequest))
    );
    assert_eq!(fresh().pvclock_register((1 << 20) - 4096).unwrap(), 1);
}

/// A host with no pvclock service answers `UnknownService`, so a guest probing
/// for the clock page gets a clean "not offered", never a panic — the pure
/// opt-in posture of `consonance/vtime/README.md`.
#[test]
fn pvclock_register_without_service_is_a_clean_status() {
    let mut dispatcher = Dispatcher::new();
    dispatcher.register(ServiceId::Event, Box::new(EventSink::new()));
    let mut client = Client::new(DispatcherLoopback(dispatcher));
    assert_eq!(
        client.pvclock_register(0x5000),
        Err(ClientError::Status(Status::UnknownService))
    );
}

/// `PvclockRegistrar` snapshots and restores its registration, like the other
/// reference services.
#[test]
fn pvclock_registrar_state_round_trips() {
    let mut svc = PvclockRegistrar::new(1 << 20, 1);
    let mut out = [0_u8; 4];
    let (status, n) = svc.handle(1, &0x7000u64.to_le_bytes(), &mut out);
    assert_eq!((status, n), (Status::Ok, 4));
    assert_eq!(svc.registered(), Some(0x7000));
    assert_eq!(
        svc.handle(1, &0x8000u64.to_le_bytes(), &mut out).0,
        Status::BadRequest
    );
    assert_eq!(svc.registered(), Some(0x7000));
    let saved = svc.save_state();
    let mut restored = PvclockRegistrar::new(0, 0);
    restored.restore_state(&saved).unwrap();
    assert_eq!(restored.registered(), Some(0x7000));
    assert_eq!(
        restored.handle(1, &0x9000u64.to_le_bytes(), &mut out).0,
        Status::BadRequest
    );
    assert_eq!(restored.registered(), Some(0x7000));
    assert_eq!(
        restored.restore_state(&saved[..saved.len() - 1]),
        Err(ProtoError::BadState)
    );
    assert_eq!(restored.registered(), Some(0x7000));
}

/// A corrupt state blob cannot restore a registration `handle` would
/// have rejected: `restore_state` re-runs the SAME 4 KiB-alignment +
/// RAM-containment check on the decoded GPA (cross-model r12 P2). Without the
/// check a crafted blob could pin an unaligned or out-of-RAM GPA that the live
/// registration path forbids, and the host would then stamp outside the page
/// window.
#[test]
fn pvclock_registrar_restore_revalidates_the_gpa() {
    let blob = |ram_len: u64, gpa: u64| {
        let mut b = Vec::new();
        b.extend_from_slice(&ram_len.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        b.push(1);
        b.extend_from_slice(&gpa.to_le_bytes());
        b
    };
    let ram_len = 1u64 << 20;

    let mut svc = PvclockRegistrar::new(0, 0);
    assert_eq!(
        svc.restore_state(&blob(ram_len, 0x7001)),
        Err(ProtoError::BadState)
    );
    assert_eq!(svc.registered(), None, "no partial restore on rejection");

    let mut svc = PvclockRegistrar::new(0, 0);
    assert_eq!(
        svc.restore_state(&blob(ram_len, ram_len)),
        Err(ProtoError::BadState)
    );
    assert_eq!(svc.registered(), None);

    let mut svc = PvclockRegistrar::new(0, 0);
    svc.restore_state(&blob(ram_len, ram_len - 4096)).unwrap();
    assert_eq!(svc.registered(), Some(ram_len - 4096));
}

/// An unknown pvclock opcode and a malformed payload are clean statuses.
#[test]
fn pvclock_registrar_rejects_bad_frames() {
    let mut svc = PvclockRegistrar::new(1 << 20, 1);
    let mut out = [0_u8; 4];
    assert_eq!(svc.handle(2, &[], &mut out).0, Status::UnknownOpcode);
    assert_eq!(svc.handle(1, &[0; 7], &mut out).0, Status::BadRequest);
    assert_eq!(svc.handle(1, &[0; 9], &mut out).0, Status::BadRequest);
    assert_eq!(svc.registered(), None, "no registration on any rejection");
}

struct PackageResponse(Vec<u8>);
impl Transport for PackageResponse {
    type Error = ();
    fn exchange(&mut self, req: &[u8], resp: &mut [u8]) -> Result<usize, ()> {
        let (header, payload) = decode(req).map_err(|_| ())?;
        assert_eq!((header.service, header.opcode), (6, 3));
        assert_eq!(payload, &[19, 0, 77, 0, 0, 0, 0, 0, 0, 0, b'x']);
        encode_response(ServiceId::Sdk, 3, header.seq, Status::Ok, &self.0, resp).map_err(|_| ())
    }
}

#[test]
fn package_service_response_distinguishes_nominal_empty_data_and_malformed_frames() {
    for (bytes, expected) in [
        (vec![0], None),
        (vec![1], Some(0)),
        (vec![1, 9, 8], Some(2)),
    ] {
        let mut client = Client::new(PackageResponse(bytes));
        let mut out = [0; 2];
        assert_eq!(
            client.service_request(19, 77, b"x", &mut out).unwrap(),
            expected
        );
        if expected == Some(2) {
            assert_eq!(out, [9, 8]);
        }
    }
    for bytes in [vec![], vec![0, 9], vec![2], vec![1, 1, 2, 3]] {
        let mut client = Client::new(PackageResponse(bytes));
        assert!(client.service_request(19, 77, b"x", &mut [0; 2]).is_err());
    }
    let mut client = Client::new(PackageResponse(vec![0]));
    assert!(client.service_request(3, 77, b"x", &mut []).is_err());
    assert!(
        client
            .service_request(19, 77, &vec![0; MAX_PAYLOAD], &mut [])
            .is_err()
    );
}

#[test]
fn package_service_uses_the_complete_frame_and_rejects_overflow_before_transport() {
    struct FullFrame(Rc<RefCell<usize>>);
    impl Transport for FullFrame {
        type Error = ();
        fn exchange(&mut self, req: &[u8], resp: &mut [u8]) -> Result<usize, ()> {
            *self.0.borrow_mut() += 1;
            let (header, payload) = decode(req).map_err(|_| ())?;
            assert_eq!((header.service, header.opcode), (ServiceId::Sdk as u16, 3));
            assert_eq!(payload.len(), MAX_PAYLOAD);
            assert_eq!(&payload[..2], &4_u16.to_le_bytes());
            assert_eq!(&payload[2..10], &u64::MAX.to_le_bytes());
            assert!(payload[10..].iter().all(|byte| *byte == 0xa5));
            let mut answer = vec![0x5a; MAX_PAYLOAD];
            answer[0] = 1;
            encode_response(ServiceId::Sdk, 3, header.seq, Status::Ok, &answer, resp)
                .map_err(|_| ())
        }
    }
    let calls = Rc::new(RefCell::new(0));
    let mut client = Client::new(FullFrame(calls.clone()));
    let mut out = vec![0; MAX_PAYLOAD - 1];
    assert_eq!(
        client.service_request(4, u64::MAX, &vec![0xa5; MAX_PAYLOAD - 10], &mut out),
        Ok(Some(MAX_PAYLOAD - 1))
    );
    assert!(out.iter().all(|byte| *byte == 0x5a));
    assert_eq!(*calls.borrow(), 1);
    assert_eq!(
        client.service_request(4, 0, &vec![0; MAX_PAYLOAD - 9], &mut out),
        Err(ClientError::InvalidLength)
    );
    for namespace in 0..=3 {
        assert_eq!(
            client.service_request(namespace, 0, &[], &mut out),
            Err(ClientError::InvalidLength)
        );
    }
    assert_eq!(*calls.borrow(), 1, "invalid requests never reach transport");
}
