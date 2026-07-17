// SPDX-License-Identifier: AGPL-3.0-or-later
//! The client speaks the **real codec** for every verb `control-proto` already
//! carries.
//!
//! The `Session`/`MockServer` loopback is an in-process seam, but the verbs it
//! carries are `control-proto`'s real wire vocabulary. These tests pin that: the
//! exact request/reply *values* the client constructs — most importantly the
//! `branch` environment `materialize()` ships (`blob_version` +
//! `EnvSpec::encode()`) — round-trip through `control-proto`'s codec
//! byte-for-byte and decode back to the originals. So the seam cannot drift from
//! the wire contract; when tasks 80/81 land the `read`/`regs`/`exec` verbs, they
//! join this same codec.

use control_proto::{
    ControlError, HashScope, Moment, Reply, Reproducer, Request, SnapId, StopConditions, StopMask,
    StopReason, decode_reply, decode_request, encode_reply, encode_request,
};
use environment::{EnvCodec, EnvSpec, FaultPolicy};
use resolution::{MomentRef, client_caps};

#[test]
fn branch_env_the_client_ships_round_trips_and_decodes_to_the_spec() {
    // The exact wire `Reproducer` `Session::materialize` builds for `branch`.
    let mref = MomentRef::new(EnvCodec::seeded(0xABCD, FaultPolicy::none()), 1234);
    let wire = Reproducer {
        blob_version: EnvSpec::BLOB_VERSION,
        bytes: mref.env.encode(),
    };
    let req = Request::Branch {
        snap: SnapId(0),
        env: wire,
    };

    let mut buf = Vec::new();
    encode_request(1, &req, &mut buf).unwrap();
    let (seq, decoded, consumed) = decode_request(&buf).unwrap().expect("a complete frame");
    assert_eq!(seq, 1);
    assert_eq!(consumed, buf.len());
    assert_eq!(decoded, req, "the branch request round-trips byte-for-byte");

    // The server decodes the shipped bytes back to the original genesis-complete
    // reproducer — the whole point of the moment address.
    let Request::Branch { env, .. } = decoded else {
        panic!("expected a Branch request");
    };
    assert_eq!(EnvSpec::decode(&env.bytes).unwrap(), mref.env);
}

#[test]
fn classic_requests_round_trip_through_the_codec() {
    let reqs = [
        Request::Hello(client_caps()),
        Request::Snapshot,
        Request::Run {
            until: StopConditions {
                deadline: Some(Moment(9_999)),
                on: StopMask::NONE,
            },
            resolve: None,
        },
        Request::Hash {
            scope: HashScope::Whole,
        },
        Request::Replay(SnapId(3)),
        Request::Drop(SnapId(3)),
    ];
    for (i, req) in reqs.iter().enumerate() {
        let mut buf = Vec::new();
        encode_request(i as u32, req, &mut buf).unwrap();
        let (seq, decoded, _) = decode_request(&buf).unwrap().expect("a complete frame");
        assert_eq!(seq, i as u32);
        assert_eq!(&decoded, req);
    }
}

#[test]
fn classic_replies_round_trip_through_the_codec() {
    let replies: [Result<Reply, ControlError>; 6] = [
        Ok(Reply::Hello(client_caps())),
        Ok(Reply::Snapshot {
            id: SnapId(7),
            at: Moment(500),
            sdk_events: 1,
            tainted: false,
        }),
        Ok(Reply::Unit),
        Ok(Reply::Stop(StopReason::Quiescent { vtime: Moment(500) })),
        Ok(Reply::Hash([0x42; 32])),
        // The error result category also round-trips.
        Err(ControlError::UnknownSnapshot(SnapId(9))),
    ];
    for (i, reply) in replies.iter().enumerate() {
        let mut buf = Vec::new();
        encode_reply(i as u32, reply, &mut buf).unwrap();
        let (seq, decoded, _) = decode_reply(&buf).unwrap().expect("a complete frame");
        assert_eq!(seq, i as u32);
        assert_eq!(&decoded, reply);
    }
}
