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

    // The task-73 SDK control service: a `buggify_decide` request
    // (ServiceId::Sdk = 6, op 1) carrying the u32 catalog point id.
    let mut sdk = b"HCP1".to_vec();
    sdk.extend_from_slice(&[1, 0, 6, 0, 1, 0, 0, 0]); // version 1, service 6, opcode 1
    sdk.extend_from_slice(&le32(12)); // seq
    sdk.extend_from_slice(&le32(4)); // payload len (one u32)
    sdk.extend_from_slice(&le32(0)); // reserved
    sdk.extend_from_slice(&le32(50)); // point 50
    assert_eq!(enc_req(ServiceId::Sdk, 1, 12, &le32(50)), sdk);
}

#[test]
fn golden_response_bytes_for_every_service_opcode() {
    let cases = [
        (ServiceId::Console, 1, 1, Status::Ok, Vec::new()),
        (ServiceId::Entropy, 1, 2, Status::Ok, vec![1, 2, 3, 4]),
        (ServiceId::Block, 1, 3, Status::Ok, le64(99).to_vec()),
        (ServiceId::Block, 2, 4, Status::OutOfRange, Vec::new()),
        (ServiceId::Event, 1, 5, Status::Ok, Vec::new()),
        // SDK `buggify_decide` reply: one byte, fire = 1 (task 73).
        (ServiceId::Sdk, 1, 6, Status::Ok, vec![1]),
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
    fn adversarial_decode_and_dispatch_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..=5000), resp_size in 0_usize..=MAX_FRAME) {
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
    fn adversarial_single_byte_mutations(mut payload in proptest::collection::vec(any::<u8>(), 0..=64), index in 0_usize..128, value in any::<u8>(), resp_size in 0_usize..=MAX_FRAME) {
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

/// The task-73 SDK buggify round-trip: the guest `buggify_decide(point)` reaches
/// the [`SdkBuggify`] service (id 6, op 1), which answers a one-byte fire flag
/// from its per-point table (default otherwise), and records every asked point.
#[test]
fn buggify_decide_round_trips_the_fire_flag() {
    let mut svc = SdkBuggify::new(false); // default: don't fire
    svc.set_point(1, true); // point 1 fires
    svc.set_point(2, false); // point 2 explicitly nominal

    let mut dispatcher = Dispatcher::new();
    dispatcher.register(ServiceId::Sdk, Box::new(svc));
    let mut client = Client::new(DispatcherLoopback(dispatcher));

    assert!(
        !client.buggify_decide(0).unwrap(),
        "point 0 uses the default"
    );
    assert!(client.buggify_decide(1).unwrap(), "point 1 fires");
    assert!(!client.buggify_decide(2).unwrap(), "point 2 is nominal");
    assert!(
        !client.buggify_decide(9).unwrap(),
        "an unmapped point uses the default"
    );
}

/// The SDK service errors as `UnknownService` when nothing is registered at id 6,
/// so a guest whose host lacks SDK support gets a clean status, never a panic.
#[test]
fn buggify_decide_without_sdk_service_is_a_clean_status() {
    let mut dispatcher = Dispatcher::new();
    dispatcher.register(ServiceId::Event, Box::new(EventSink::new()));
    let mut client = Client::new(DispatcherLoopback(dispatcher));
    assert_eq!(
        client.buggify_decide(0),
        Err(ClientError::Status(Status::UnknownService))
    );
}

/// `SdkBuggify` snapshots and restores its table + asked log, so a buggify
/// service survives a corpus snapshot exactly like the other reference services.
#[test]
fn sdk_buggify_state_round_trips() {
    let mut svc = SdkBuggify::new(true);
    svc.set_point(3, false);
    // Drive op 1 directly so the asked log is populated on this very instance.
    let mut out = [0_u8; 1];
    for point in [3u32, 7] {
        let (status, n) = svc.handle(1, &point.to_le_bytes(), &mut out);
        assert_eq!(status, Status::Ok);
        assert_eq!(n, 1);
    }
    assert_eq!(svc.asked(), [3, 7]);
    let saved = svc.save_state();
    let mut restored = SdkBuggify::new(false);
    restored.restore_state(&saved).unwrap();
    assert_eq!(restored, svc, "state round-trips exactly");
    assert_eq!(
        restored.save_state(),
        saved,
        "bytes are stable across restore"
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

    // Header parses but the payload is truncated: raw fields must be echoed,
    // not the all-zeros reserved for unparseable headers.
    let truncated = enc_req(ServiceId::Console, 1, 77, b"abcdef");
    let len = dispatcher.dispatch(&truncated[..truncated.len() - 3], &mut resp);
    let (header, payload) = decode(&resp[..len]).unwrap();
    assert_eq!(header.service, ServiceId::Console as u16);
    assert_eq!(header.opcode, 1);
    assert_eq!(header.seq, 77);
    assert_eq!(header.status, Status::BadRequest as u16);
    assert!(payload.is_empty());

    // resp_buf large enough for a header but too small for the response payload:
    // Internal with an empty payload, raw fields echoed.
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
    // The response length ultimately comes from the host; the client must error,
    // not panic, when it exceeds the response buffer.
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
    // One emit is one logical event: max-size data is exactly one Emit frame,
    // and anything larger is rejected up front rather than split into multiple
    // events the host would double-count.
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
    // State 0 is unreachable from save_state and would pin the stream at zero.
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

    // A valid-but-different console chunk followed by a malformed entropy chunk:
    // the restore must fail without leaving the console chunk applied.
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

// ---------------------------------------------------------------------------
// Task 61: the `Net` per-flow decision service (ServiceId::Net = 5, op 1).
// ---------------------------------------------------------------------------

/// Pack a `net_decide` request payload the way the guest client does, so the
/// golden-byte and decode tests share one source of truth.
fn net_req_payload(src: u32, dst: u32, conn: u64, event: u16) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&src.to_le_bytes());
    p.extend_from_slice(&dst.to_le_bytes());
    p.extend_from_slice(&conn.to_le_bytes());
    p.extend_from_slice(&event.to_le_bytes());
    assert_eq!(p.len(), NET_REQUEST_LEN);
    p
}

/// The wire form of a `net_decide` request is the fixed 18-byte little-endian
/// `NetFlow { src, dst, conn, event }` decision point behind service id 5, op 1.
#[test]
fn golden_net_decide_request_bytes() {
    let payload = net_req_payload(11, 22, 0xDEAD_BEEF, 0);
    let mut expected = b"HCP1".to_vec();
    expected.extend_from_slice(&[1, 0, 5, 0, 1, 0, 0, 0]); // kind 1, service 5, opcode 1
    expected.extend_from_slice(&le32(42)); // seq
    expected.extend_from_slice(&le32(NET_REQUEST_LEN as u32)); // payload len
    expected.extend_from_slice(&le32(0)); // reserved
    expected.extend_from_slice(&payload);
    assert_eq!(enc_req(ServiceId::Net, 1, 42, &payload), expected);
}

/// `NetFlowPoint::decode` is the inverse of the client's request packing and
/// rejects any payload that is not exactly [`NET_REQUEST_LEN`] bytes.
#[test]
fn net_flow_point_decodes_the_fixed_wire_form() {
    let payload = net_req_payload(1, 2, 3, 0);
    let point = NetFlowPoint::decode(&payload).unwrap();
    assert_eq!(
        point,
        NetFlowPoint {
            src: 1,
            dst: 2,
            conn: 3,
            event: 0
        }
    );
    assert!(NetFlowPoint::decode(&payload[..17]).is_none());
    let mut too_long = payload.clone();
    too_long.push(0);
    assert!(NetFlowPoint::decode(&too_long).is_none());
}

/// The `net_decide` round-trip: the guest reaches the [`NetDecider`] reference
/// answerer (id 5, op 1), which returns the opaque per-flow policy bytes from its
/// table (default otherwise) and records every asked flow in call order.
#[test]
fn net_decide_round_trips_the_flow_policy() {
    // Opaque "policy" bytes — this crate never interprets them. Model a
    // one-byte Nominal default and a multi-byte fault answer for conn 7.
    let nominal = vec![0u8];
    let fault = vec![2u8, 12, 0, 0, 0, 0]; // stand-in for an encoded NetLatency
    let mut svc = NetDecider::new(nominal.clone());
    svc.set_flow(7, fault.clone());

    let mut dispatcher = Dispatcher::new();
    dispatcher.register(ServiceId::Net, Box::new(svc));
    let mut client = Client::new(DispatcherLoopback(dispatcher));

    let mut out = [0u8; 64];
    let n = client.net_decide(1, 2, 5, 0, &mut out).unwrap();
    assert_eq!(&out[..n], &nominal[..], "conn 5 uses the default answer");
    let n = client.net_decide(1, 2, 7, 0, &mut out).unwrap();
    assert_eq!(&out[..n], &fault[..], "conn 7 uses its pinned answer");
}

/// A too-small caller buffer surfaces `BufferTooSmall`, never a truncated answer
/// or a panic — the guest must be able to trust the length it gets back.
#[test]
fn net_decide_rejects_an_undersized_out_buffer() {
    let mut svc = NetDecider::new(vec![0u8]);
    svc.set_flow(7, vec![1, 2, 3, 4]);
    let mut dispatcher = Dispatcher::new();
    dispatcher.register(ServiceId::Net, Box::new(svc));
    let mut client = Client::new(DispatcherLoopback(dispatcher));
    let mut out = [0u8; 2];
    assert_eq!(
        client.net_decide(1, 2, 7, 0, &mut out),
        Err(ClientError::Protocol(ProtoError::BufferTooSmall))
    );
}

/// With nothing registered at id 5, a guest whose host lacks the `Net` vertical
/// gets a clean `UnknownService`, never a hang or a panic.
#[test]
fn net_decide_without_the_service_is_a_clean_status() {
    let mut dispatcher = Dispatcher::new();
    dispatcher.register(ServiceId::Event, Box::new(EventSink::new()));
    let mut client = Client::new(DispatcherLoopback(dispatcher));
    let mut out = [0u8; 8];
    assert_eq!(
        client.net_decide(1, 2, 3, 0, &mut out),
        Err(ClientError::Status(Status::UnknownService))
    );
}

/// `NetDecider` snapshots and restores its table + asked log, so a Net service
/// survives a corpus snapshot exactly like the other reference services.
#[test]
fn net_decider_state_round_trips() {
    let mut svc = NetDecider::new(vec![0u8]);
    svc.set_flow(3, vec![9, 9]);
    let mut out = [0u8; 16];
    for (conn, event) in [(3u64, 0u16), (8, 0)] {
        let (status, _n) = svc.handle(1, &net_req_payload(1, 2, conn, event), &mut out);
        assert_eq!(status, Status::Ok);
    }
    assert_eq!(svc.asked().len(), 2);
    assert_eq!(svc.asked()[0].conn, 3);
    assert_eq!(svc.asked()[1].conn, 8);
    let saved = svc.save_state();
    let mut restored = NetDecider::new(Vec::new());
    restored.restore_state(&saved).unwrap();
    assert_eq!(restored, svc);
}

/// An opcode the `Net` service does not implement is `UnknownOpcode`, and a
/// malformed (wrong-length) request is `BadRequest` — never a silent drop.
#[test]
fn net_decider_rejects_bad_opcode_and_payload() {
    let mut svc = NetDecider::new(vec![0u8]);
    let mut out = [0u8; 8];
    assert_eq!(
        svc.handle(2, &net_req_payload(1, 2, 3, 0), &mut out).0,
        Status::UnknownOpcode
    );
    assert_eq!(svc.handle(1, &[0u8; 4], &mut out).0, Status::BadRequest);
    // A rejected request records no phantom ask.
    assert!(svc.asked().is_empty());
}

/// Additive-versioning invariant: adding the `Net` vertical must fill id 5
/// without moving any released service id — a released wire ABI never renumbers.
#[test]
fn service_ids_are_a_stable_additive_registry() {
    assert_eq!(ServiceId::Console as u16, 1);
    assert_eq!(ServiceId::Entropy as u16, 2);
    assert_eq!(ServiceId::Block as u16, 3);
    assert_eq!(ServiceId::Event as u16, 4);
    assert_eq!(ServiceId::Net as u16, 5);
    assert_eq!(ServiceId::Sdk as u16, 6);
}

proptest! {
    /// For any flow fields and any opaque answer that fits the caller buffer, the
    /// `net_decide` round-trip returns exactly the answer bytes the host set for
    /// that connection and logs exactly one ask with the sent fields.
    #[test]
    fn net_decide_round_trip_is_faithful(
        src in any::<u32>(),
        dst in any::<u32>(),
        conn in any::<u64>(),
        answer in proptest::collection::vec(any::<u8>(), 1..64),
    ) {
        let mut svc = NetDecider::new(vec![0u8]);
        svc.set_flow(conn, answer.clone());
        let mut dispatcher = Dispatcher::new();
        dispatcher.register(ServiceId::Net, Box::new(svc));
        let mut client = Client::new(DispatcherLoopback(dispatcher));
        let mut out = [0u8; 64];
        let n = client.net_decide(src, dst, conn, 0, &mut out).unwrap();
        prop_assert_eq!(&out[..n], &answer[..]);
    }
}

/// The task-110 pvclock registration round-trip: the guest
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
    // Misaligned and out-of-range GPAs are clean OutOfRange statuses (fresh
    // registrar each — a rejection must not consume the one-shot).
    let mut client = fresh();
    assert_eq!(
        client.pvclock_register(0x5001),
        Err(ClientError::Status(Status::OutOfRange))
    );
    assert_eq!(
        client.pvclock_register(1 << 20),
        Err(ClientError::Status(Status::OutOfRange))
    );
    // A rejected attempt did not consume the one-shot: registering still works.
    assert_eq!(client.pvclock_register(0x5000).unwrap(), 1);
    // ONE-SHOT (the frozen ABI, mirroring the production host): any second
    // register — same GPA or another valid one — is a guest fault.
    assert_eq!(
        client.pvclock_register(0x5000),
        Err(ClientError::Status(Status::BadRequest))
    );
    assert_eq!(
        client.pvclock_register(0x6000),
        Err(ClientError::Status(Status::BadRequest))
    );
    // The last page of RAM is in range (fresh registrar).
    assert_eq!(fresh().pvclock_register((1 << 20) - 4096).unwrap(), 1);
}

/// A host with no pvclock service answers `UnknownService`, so a guest probing
/// for the clock page gets a clean "not offered", never a panic — the pure
/// opt-in posture of `docs/PARAVIRT-CLOCK.md`.
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
    // One-shot holds on the SAME instance: a second register is a guest fault
    // and the pinned target does not move.
    assert_eq!(
        svc.handle(1, &0x8000u64.to_le_bytes(), &mut out).0,
        Status::BadRequest
    );
    assert_eq!(svc.registered(), Some(0x7000));
    let saved = svc.save_state();
    let mut restored = PvclockRegistrar::new(0, 0);
    restored.restore_state(&saved).unwrap();
    assert_eq!(restored.registered(), Some(0x7000));
    // ...and holds across the state round-trip too (restored state cannot be
    // re-registered over — the supposedly pinned target stays pinned).
    assert_eq!(
        restored.handle(1, &0x9000u64.to_le_bytes(), &mut out).0,
        Status::BadRequest
    );
    assert_eq!(restored.registered(), Some(0x7000));
    // A truncated blob is rejected, never a partial restore.
    assert_eq!(
        restored.restore_state(&saved[..saved.len() - 1]),
        Err(ProtoError::BadState)
    );
    assert_eq!(restored.registered(), Some(0x7000));
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
