// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end gate for the guest SDK with **no hypervisor**: an `Sdk` over a
//! loopback `Transport` that services a real `hypercall_proto::Dispatcher`
//! (exactly as `vmcall-transport` is loopback-tested). The SDK's every emission
//! must land as the expected `(event_id, payload)` on the host EventSink, the
//! `buggify` verb must round-trip the host's fire decision and record it, and the
//! whole thing must be deterministic (same catalog + calls ⇒ identical stream).

use std::cell::RefCell;
use std::rc::Rc;

use harmony_sdk::wire;
use harmony_sdk::{Point, Sdk, SdkError};
use hypercall_proto::{Client, Dispatcher, ProtoError, Service, ServiceId, Status, Transport};

// ---------------------------------------------------------------------------
// A safe loopback transport over one preconfigured dispatcher.
// ---------------------------------------------------------------------------

struct DispatcherLoopback(Dispatcher);

impl Transport for DispatcherLoopback {
    type Error = ();
    fn exchange(&mut self, req: &[u8], resp: &mut [u8]) -> Result<usize, Self::Error> {
        Ok(self.0.dispatch(req, resp))
    }
}

/// Recorded `(event_id, data)` pairs shared between the host service and the test.
type EventLog = Rc<RefCell<Vec<(u32, Vec<u8>)>>>;

/// Event sink recording `(id, data)` into a handle the test keeps a clone of.
#[derive(Clone, Default)]
struct SharedEvent(EventLog);

impl Service for SharedEvent {
    fn handle(&mut self, opcode: u16, payload: &[u8], _resp: &mut [u8]) -> (Status, usize) {
        if opcode != 1 {
            return (Status::UnknownOpcode, 0);
        }
        if payload.len() < 4 {
            return (Status::BadRequest, 0);
        }
        let id = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
        self.0.borrow_mut().push((id, payload[4..].to_vec()));
        (Status::Ok, 0)
    }
    fn save_state(&self) -> Vec<u8> {
        Vec::new()
    }
    fn restore_state(&mut self, _state: &[u8]) -> Result<(), ProtoError> {
        Ok(())
    }
}

/// Buggify service that fires iff the point is odd, and records every ask, into
/// handles the test keeps clones of.
#[derive(Clone, Default)]
struct SharedBuggify {
    asks: Rc<RefCell<Vec<u32>>>,
}

impl Service for SharedBuggify {
    fn handle(&mut self, opcode: u16, payload: &[u8], resp: &mut [u8]) -> (Status, usize) {
        if opcode != 1 {
            return (Status::UnknownOpcode, 0);
        }
        if payload.len() != 4 || resp.is_empty() {
            return (Status::BadRequest, 0);
        }
        let point = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
        self.asks.borrow_mut().push(point);
        resp[0] = u8::from(point % 2 == 1); // fire on odd points
        (Status::Ok, 1)
    }
    fn save_state(&self) -> Vec<u8> {
        Vec::new()
    }
    fn restore_state(&mut self, _state: &[u8]) -> Result<(), ProtoError> {
        Ok(())
    }
}

/// The demo catalog: two sometimes points, an always, an unreachable, a state
/// register, and a buggify site.
fn catalog() -> Vec<Point> {
    vec![
        Point::sometimes(1, "commit_seen"),
        Point::sometimes(2, "rollback_seen"),
        Point::always(20, "balance_nonneg"),
        Point::unreachable(30, "dead_branch"),
        Point::state(40, "max_lsn"),
        Point::buggify(50, "slow_disk"),
    ]
}

/// Build an `Sdk` over a fresh loopback, returning it plus the shared event log
/// and buggify-ask log. `init` has already emitted the catalog declaration.
fn harness() -> (Sdk<DispatcherLoopback>, EventLog, Rc<RefCell<Vec<u32>>>) {
    let events: EventLog = Rc::new(RefCell::new(Vec::new()));
    let asks = Rc::new(RefCell::new(Vec::new()));
    let mut d = Dispatcher::new();
    d.register(ServiceId::Event, Box::new(SharedEvent(events.clone())));
    d.register(
        ServiceId::Sdk,
        Box::new(SharedBuggify { asks: asks.clone() }),
    );
    let sdk = Sdk::init(DispatcherLoopback(d), &catalog()).expect("init");
    (sdk, events, asks)
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

/// The first emission is the catalog declaration (event id 0, `SDKC` magic +
/// version + point count).
#[test]
fn init_declares_the_catalog_first() {
    let (_sdk, events, _asks) = harness();
    let ev = events.borrow();
    assert_eq!(ev.len(), 1, "init emits exactly one event (the catalog)");
    let (id, data) = &ev[0];
    assert_eq!(*id, wire::CATALOG_EVENT_ID);
    assert_eq!(
        u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
        wire::CATALOG_MAGIC
    );
    assert_eq!(data[4], wire::SDK_WIRE_VERSION);
    let count = u32::from_le_bytes([data[5], data[6], data[7], data[8]]);
    assert_eq!(count, 6, "six declared points");
}

/// Always/sometimes/reachable/unreachable emit exactly the right disposition,
/// and only when they should.
#[test]
fn assertion_verbs_emit_expected_dispositions() {
    let (mut sdk, events, _asks) = harness();
    let base = events.borrow().len();

    sdk.assert_always(true, 20).unwrap(); // holds -> nothing
    sdk.assert_always(false, 20).unwrap(); // violated -> violation
    sdk.assert_sometimes(false, 1).unwrap(); // not satisfied -> nothing
    sdk.assert_sometimes(true, 1).unwrap(); // satisfied -> hit
    sdk.assert_reachable(30).unwrap(); // reached -> hit
    sdk.assert_unreachable(30).unwrap(); // reached -> violation

    let ev = events.borrow();
    let got: Vec<(u32, Vec<u8>)> = ev[base..].to_vec();
    assert_eq!(
        got,
        vec![
            (
                wire::event_id(wire::NS_ASSERT, 20),
                vec![wire::DISP_VIOLATION, 0, 0]
            ),
            (
                wire::event_id(wire::NS_ASSERT, 1),
                vec![wire::DISP_HIT, 0, 0]
            ),
            (
                wire::event_id(wire::NS_ASSERT, 30),
                vec![wire::DISP_HIT, 0, 0]
            ),
            (
                wire::event_id(wire::NS_ASSERT, 30),
                vec![wire::DISP_VIOLATION, 0, 0]
            ),
        ]
    );
}

/// state_set / state_max emit `[op, value_le]` under the state namespace.
#[test]
fn state_verbs_emit_op_and_value() {
    let (mut sdk, events, _asks) = harness();
    let base = events.borrow().len();

    sdk.state_set(40, 0x0102_0304_0506_0708).unwrap();
    sdk.state_max(40, 42).unwrap();

    let ev = events.borrow();
    let got: Vec<(u32, Vec<u8>)> = ev[base..].to_vec();
    let mut set_payload = vec![wire::STATE_SET];
    set_payload.extend_from_slice(&0x0102_0304_0506_0708_u64.to_le_bytes());
    let mut max_payload = vec![wire::STATE_MAX];
    max_payload.extend_from_slice(&42_u64.to_le_bytes());
    assert_eq!(
        got,
        vec![
            (wire::event_id(wire::NS_STATE, 40), set_payload),
            (wire::event_id(wire::NS_STATE, 40), max_payload),
        ]
    );
}

/// buggify returns the host's fire decision and records it on the event stream.
#[test]
fn buggify_round_trips_and_records_the_result() {
    let (mut sdk, events, asks) = harness();
    let base = events.borrow().len();

    // The host fires on odd points (SharedBuggify).
    assert!(sdk.buggify(51).unwrap(), "odd point fires");
    assert!(!sdk.buggify(50).unwrap(), "even point is nominal");

    assert_eq!(
        asks.borrow().as_slice(),
        &[51, 50],
        "both points were asked"
    );
    let ev = events.borrow();
    let got: Vec<(u32, Vec<u8>)> = ev[base..].to_vec();
    assert_eq!(
        got,
        vec![
            (wire::event_id(wire::NS_BUGGIFY, 51), vec![1]),
            (wire::event_id(wire::NS_BUGGIFY, 50), vec![0]),
        ]
    );
}

/// setup_complete emits the lifecycle event (empty payload).
#[test]
fn setup_complete_emits_the_lifecycle_event() {
    let (mut sdk, events, _asks) = harness();
    let base = events.borrow().len();
    sdk.setup_complete().unwrap();
    let ev = events.borrow();
    assert_eq!(ev[base..], [(wire::SETUP_COMPLETE_EVENT_ID, vec![])]);
}

/// Determinism: the same catalog + call sequence yields a byte-identical event
/// stream (the gate-4 determinism property, at the SDK layer).
#[test]
fn same_calls_yield_identical_event_streams() {
    fn run() -> Vec<(u32, Vec<u8>)> {
        let (mut sdk, events, _asks) = harness();
        sdk.assert_sometimes(true, 1).unwrap();
        sdk.state_max(40, 7).unwrap();
        let _ = sdk.buggify(51).unwrap();
        sdk.assert_always(false, 20).unwrap();
        sdk.setup_complete().unwrap();
        events.borrow().clone()
    }
    assert_eq!(run(), run());
}

/// An id past the 24-bit local space is a typed error, never a silent overflow.
#[test]
fn oversize_ids_are_rejected() {
    let (mut sdk, _events, _asks) = harness();
    assert_eq!(
        sdk.assert_always(false, wire::LOCAL_MAX + 1),
        Err(SdkError::PointIdTooLarge)
    );
    assert_eq!(sdk.state_set(u32::MAX, 0), Err(SdkError::PointIdTooLarge));
    assert_eq!(sdk.buggify(1 << 24), Err(SdkError::PointIdTooLarge));
}

/// A catalog too large to fit one Event frame is rejected, not truncated.
#[test]
fn oversize_catalog_is_rejected() {
    let events: EventLog = Rc::new(RefCell::new(Vec::new()));
    let mut d = Dispatcher::new();
    d.register(ServiceId::Event, Box::new(SharedEvent(events.clone())));
    // Long names × many points overflow one frame.
    let big: Vec<Point> = (0..500)
        .map(|i| Point::sometimes(i, "a-very-long-signal-name-that-eats-frame-budget"))
        .collect();
    assert_eq!(
        Sdk::init(DispatcherLoopback(d), &big).err(),
        Some(SdkError::CatalogTooLarge)
    );
    assert!(events.borrow().is_empty(), "nothing emitted on overflow");
}

// ---------------------------------------------------------------------------
// Compile-time proof that the SDK composes over the REAL guest doorbell
// transport (`Client<VmcallTransport>`) with zero new transport code. Never
// called — constructing a real `VmcallTransport` needs `unsafe` page setup that
// the box gate performs; here we only type-check the composition, so the SDK
// crate itself stays free of `unsafe` (and of the Miri obligation).
// ---------------------------------------------------------------------------

#[allow(dead_code)]
fn _sdk_composes_over_vmcall_transport(
    t: vmcall_transport::VmcallTransport,
) -> Result<Sdk<vmcall_transport::VmcallTransport>, SdkError<vmcall_transport::TransportError>> {
    Sdk::init(t, &[])
}

#[allow(dead_code)]
fn _sdk_client_escape_hatch_is_a_real_client(sdk: &mut Sdk<DispatcherLoopback>) {
    let _c: &mut Client<DispatcherLoopback> = sdk.client_mut();
}
