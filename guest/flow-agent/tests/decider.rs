// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `HostFlowDecider` round-trip against the reference host answerer: a flow
//! decision travels guest→host→guest over a loopback `Dispatcher`, and the decoded
//! `FlowPolicy` matches the encoded `Answer` the host set — proving the agent's
//! decider seam speaks the real wire protocol, not a stand-in.

use environment::{Answer, Fault, Span as EnvSpan};
use flow::{ConnId, FlowDecider, FlowPolicy, NodeId};
use harmony_flow_agent::{DecideError, HostFlowDecider};
use hypercall_proto::{Client, Dispatcher, NetDecider, ServiceId, Transport};

/// A host-in-a-box: the guest `Client`'s `exchange` runs the host `Dispatcher`
/// synchronously, exactly as the real doorbell does one exit at a time.
struct Loopback(Dispatcher);

impl Transport for Loopback {
    type Error = ();
    fn exchange(&mut self, req: &[u8], resp: &mut [u8]) -> Result<usize, Self::Error> {
        Ok(self.0.dispatch(req, resp))
    }
}

fn client_with(default: Answer, per_conn: &[(u64, Answer)]) -> Client<Loopback> {
    let mut svc = NetDecider::new(default.encode());
    for (conn, ans) in per_conn {
        svc.set_flow(*conn, ans.encode());
    }
    let mut dispatcher = Dispatcher::new();
    dispatcher.register(ServiceId::Net, Box::new(svc));
    Client::new(Loopback(dispatcher))
}

#[test]
fn decider_maps_the_hosts_answer_to_a_policy() {
    let mut client = client_with(
        Answer::Nominal,
        &[
            (7, Answer::Fault(Fault::NetReset)),
            (9, Answer::Fault(Fault::NetLatency(EnvSpan(1234)))),
        ],
    );
    // Seed each flow by its conn id, so a seeded-loss policy would replay exactly.
    let mut decider = HostFlowDecider::new(&mut client, |c: ConnId, _s, _d| c.0);

    // A nominal flow → deliver normally.
    assert_eq!(
        decider.decide_flow(ConnId(1), NodeId(10), NodeId(20)),
        FlowPolicy::Nominal
    );
    // The reset flow → Reset.
    assert_eq!(
        decider.decide_flow(ConnId(7), NodeId(10), NodeId(20)),
        FlowPolicy::Reset
    );
    // The latency flow → Latency, in guest V-time.
    assert_eq!(
        decider.decide_flow(ConnId(9), NodeId(10), NodeId(20)),
        FlowPolicy::Latency(flow::Span(1234))
    );
    assert!(decider.last_error().is_none());
    assert_eq!(decider.decisions().len(), 3);
}

#[test]
fn a_missing_service_falls_back_to_nominal_not_a_hang() {
    // A dispatcher with no Net service → UnknownService status → the decider
    // deterministically delivers normally and records the reason.
    let mut dispatcher = Dispatcher::new();
    dispatcher.register(
        ServiceId::Event,
        Box::new(hypercall_proto::EventSink::new()),
    );
    let mut client = Client::new(Loopback(dispatcher));
    let mut decider = HostFlowDecider::new(&mut client, |_c, _s, _d| 0);
    assert_eq!(
        decider.decide_flow(ConnId(1), NodeId(1), NodeId(2)),
        FlowPolicy::Nominal
    );
    // An unwired host (UnknownService) is the clean no-op case, not a failure.
    assert_eq!(decider.last_error(), Some(&DecideError::DoorbellUnwired));
}

#[test]
fn a_supply_answer_for_a_flow_is_refused_to_nominal() {
    // A well-formed host never does this, but a Supply answer for a flow must be
    // refused (mapped to Nominal + a Map error), never enforced as garbage.
    let mut client = client_with(Answer::Supply(vec![1, 2, 3]), &[]);
    let mut decider = HostFlowDecider::new(&mut client, |_c, _s, _d| 0);
    assert_eq!(
        decider.decide_flow(ConnId(1), NodeId(1), NodeId(2)),
        FlowPolicy::Nominal
    );
    assert!(matches!(decider.last_error(), Some(DecideError::Map(_))));
}
