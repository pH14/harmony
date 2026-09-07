// SPDX-License-Identifier: AGPL-3.0-or-later
use std::cell::RefCell;
use std::rc::Rc;

use fault_sdk::{BUGGIFY_NAMESPACE, FaultClientExt, FaultSdkExt, NET_FLOW_NAMESPACE};
use harmony_sdk::Sdk;
use harmony_sdk::wire;
use hypercall_proto::{Client, Dispatcher, ProtoError, Service, ServiceId, Status, Transport};

struct Loopback(Dispatcher);

impl Transport for Loopback {
    type Error = ();

    fn exchange(&mut self, request: &[u8], response: &mut [u8]) -> Result<usize, Self::Error> {
        Ok(self.0.dispatch(request, response))
    }
}

#[derive(Clone)]
struct FaultService {
    malformed_buggify: bool,
}

struct FixedService(&'static [u8]);

impl Service for FixedService {
    fn handle(&mut self, opcode: u16, _payload: &[u8], response: &mut [u8]) -> (Status, usize) {
        if opcode != 3 || response.len() < self.0.len() {
            return (Status::BadRequest, 0);
        }
        response[..self.0.len()].copy_from_slice(self.0);
        (Status::Ok, self.0.len())
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

impl Service for FaultService {
    fn handle(&mut self, opcode: u16, payload: &[u8], response: &mut [u8]) -> (Status, usize) {
        if opcode != 3 || payload.len() < 10 {
            return (Status::BadRequest, 0);
        }
        let namespace = u16::from_le_bytes([payload[0], payload[1]]);
        let request = &payload[10..];
        match namespace {
            BUGGIFY_NAMESPACE if request.len() == 4 => {
                if self.malformed_buggify {
                    if response.len() < 3 {
                        return (Status::Internal, 0);
                    }
                    response[..3].copy_from_slice(&[1, 1, 0]);
                    return (Status::Ok, 3);
                }
                if response.len() < 2 {
                    return (Status::Internal, 0);
                }
                response[..2].copy_from_slice(&[1, request[0] & 1]);
                (Status::Ok, 2)
            }
            NET_FLOW_NAMESPACE if request.len() == 18 => {
                if response.len() < 3 {
                    return (Status::Internal, 0);
                }
                response[..3].copy_from_slice(&[1, 0xa5, request[8]]);
                (Status::Ok, 3)
            }
            _ => (Status::BadRequest, 0),
        }
    }

    fn save_state(&self) -> Vec<u8> {
        vec![u8::from(self.malformed_buggify)]
    }

    fn restore_state(&mut self, state: &[u8]) -> Result<(), ProtoError> {
        match state {
            [] => Err(ProtoError::BadState),
            [value] => {
                self.malformed_buggify = *value != 0;
                Ok(())
            }
            _ => Err(ProtoError::BadState),
        }
    }
}

type Events = Rc<RefCell<Vec<(u32, Vec<u8>)>>>;

#[derive(Clone)]
struct EventService(Events);

impl Service for EventService {
    fn handle(&mut self, opcode: u16, payload: &[u8], _response: &mut [u8]) -> (Status, usize) {
        if opcode != 1 || payload.len() < 4 {
            return (Status::BadRequest, 0);
        }
        let id = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
        self.0.borrow_mut().push((id, payload[4..].to_vec()));
        (Status::Ok, 0)
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

fn dispatcher(malformed_buggify: bool) -> Dispatcher {
    let mut dispatcher = Dispatcher::new();
    dispatcher.register(ServiceId::Sdk, Box::new(FaultService { malformed_buggify }));
    dispatcher
}

#[test]
fn client_adapter_packs_buggify_and_net_requests_over_opaque_service() {
    let mut client = Client::new(Loopback(dispatcher(false)));
    assert!(client.buggify_decide(3).unwrap());
    assert!(!client.buggify_decide(4).unwrap());

    let mut answer = [0_u8; 8];
    let length = client.net_decide(1, 2, 0x1234, 9, &mut answer).unwrap();
    assert_eq!(&answer[..length], &[0xa5, 0x34]);
}

#[test]
fn client_adapter_rejects_non_data_buggify_answers() {
    let mut client = Client::new(Loopback(dispatcher(true)));
    assert_eq!(
        client.buggify_decide(3),
        Err(hypercall_proto::ClientError::Protocol(
            ProtoError::BufferTooSmall
        ))
    );
}

#[test]
fn adapters_map_nominal_answers_and_preserve_missing_service_errors() {
    let mut dispatcher = Dispatcher::new();
    dispatcher.register(ServiceId::Sdk, Box::new(FixedService(&[0])));
    let mut client = Client::new(Loopback(dispatcher));
    assert!(!client.buggify_decide(3).unwrap());
    let mut answer = [0xa5; 1];
    assert_eq!(client.net_decide(1, 2, 3, 4, &mut answer).unwrap(), 1);
    assert_eq!(answer, [0]);
    assert_eq!(
        Client::new(Loopback(Dispatcher::new())).buggify_decide(3),
        Err(hypercall_proto::ClientError::Status(Status::UnknownService))
    );
}

#[test]
fn buggify_adapter_rejects_empty_and_invalid_boolean_data() {
    for bytes in [&[1_u8][..], &[1_u8, 2][..]] {
        let mut dispatcher = Dispatcher::new();
        dispatcher.register(ServiceId::Sdk, Box::new(FixedService(bytes)));
        let mut client = Client::new(Loopback(dispatcher));
        assert_eq!(
            client.buggify_decide(3),
            Err(hypercall_proto::ClientError::Protocol(
                ProtoError::BadPayload
            ))
        );
    }
}

#[test]
fn sdk_adapter_records_the_buggify_result_without_generic_sdk_policy() {
    let events = Rc::new(RefCell::new(Vec::new()));
    let mut dispatcher = dispatcher(false);
    dispatcher.register(ServiceId::Event, Box::new(EventService(events.clone())));
    let mut sdk = Sdk::init(Loopback(dispatcher), &[]).unwrap();

    assert!(sdk.buggify(3).unwrap());
    assert!(!sdk.buggify(4).unwrap());

    let events = events.borrow();
    assert_eq!(
        events[1..],
        [
            (wire::event_id(wire::NS_BUGGIFY, 3), vec![1]),
            (wire::event_id(wire::NS_BUGGIFY, 4), vec![0]),
        ]
    );
}

#[test]
fn sdk_adapter_rejects_out_of_range_points_before_transport() {
    let events = Rc::new(RefCell::new(Vec::new()));
    let mut dispatcher = dispatcher(false);
    dispatcher.register(ServiceId::Event, Box::new(EventService(events)));
    let mut sdk = Sdk::init(Loopback(dispatcher), &[]).unwrap();
    assert_eq!(
        sdk.buggify(wire::LOCAL_MAX + 1),
        Err(harmony_sdk::SdkError::PointIdTooLarge)
    );
}
