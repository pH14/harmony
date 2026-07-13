// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 2 — round-trip property. Arbitrary in-bounds `Request` / `Reply` /
//! `ControlError` values encode → decode to an identical value; `seq` echoes; and
//! a body exceeding `MAX_FRAME_LEN` makes `encode_*` return `BadLength` (never a
//! panic, a truncation, or an undecodable frame). ≥256 cases.

mod common;

use common::{arb_caps, arb_environment, arb_reply_result, arb_request};
use control_proto::{
    MAX_FRAME_LEN, ProtocolError, Reproducer, Request, SnapId, decode_reply, decode_request,
    encode_reply, encode_request,
};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// A request encodes then decodes to itself; `seq` echoes; the decoder
    /// consumes exactly the bytes the encoder produced; and re-encoding is
    /// byte-stable (the encoding is canonical).
    #[test]
    fn request_round_trips(seq in any::<u32>(), req in arb_request()) {
        let mut buf = Vec::new();
        encode_request(seq, &req, &mut buf).expect("in-bounds request encodes");

        let (got_seq, got, consumed) =
            decode_request(&buf).expect("decodes cleanly").expect("frame is complete");
        prop_assert_eq!(got_seq, seq, "seq echoes");
        prop_assert_eq!(&got, &req, "value round-trips");
        prop_assert_eq!(consumed, buf.len(), "consumes the whole frame");

        let mut reencoded = Vec::new();
        encode_request(seq, &got, &mut reencoded).expect("re-encode");
        prop_assert_eq!(reencoded, buf, "encoding is canonical / byte-stable");
    }

    /// Same for a reply (`Ok(Reply)` or `Err(ControlError)`).
    #[test]
    fn reply_round_trips(seq in any::<u32>(), reply in arb_reply_result()) {
        let mut buf = Vec::new();
        encode_reply(seq, &reply, &mut buf).expect("in-bounds reply encodes");

        let (got_seq, got, consumed) =
            decode_reply(&buf).expect("decodes cleanly").expect("frame is complete");
        prop_assert_eq!(got_seq, seq, "seq echoes");
        prop_assert_eq!(&got, &reply, "value round-trips");
        prop_assert_eq!(consumed, buf.len(), "consumes the whole frame");

        let mut reencoded = Vec::new();
        encode_reply(seq, &got, &mut reencoded).expect("re-encode");
        prop_assert_eq!(reencoded, buf, "encoding is canonical / byte-stable");
    }

    /// A trailing frame after a complete one is left untouched: `decode_*`
    /// reports `consumed` at the first frame boundary, so a stream of frames
    /// decodes one at a time.
    #[test]
    fn decode_stops_at_the_first_frame_boundary(
        seq_a in any::<u32>(),
        req_a in arb_request(),
        seq_b in any::<u32>(),
        req_b in arb_request(),
    ) {
        let mut buf = Vec::new();
        encode_request(seq_a, &req_a, &mut buf).unwrap();
        let first_len = buf.len();
        encode_request(seq_b, &req_b, &mut buf).unwrap();

        let (got_seq, got, consumed) =
            decode_request(&buf).expect("decode").expect("complete");
        prop_assert_eq!(got_seq, seq_a);
        prop_assert_eq!(&got, &req_a);
        prop_assert_eq!(consumed, first_len, "stops at the first frame boundary");

        // The remainder decodes to the second frame.
        let (got_seq2, got2, consumed2) =
            decode_request(&buf[consumed..]).expect("decode").expect("complete");
        prop_assert_eq!(got_seq2, seq_b);
        prop_assert_eq!(&got2, &req_b);
        prop_assert_eq!(consumed + consumed2, buf.len());
    }

    /// Caps and Reproducer carry every field bit-exactly through a Hello /
    /// Branch (the negotiation- and schema-blind-carry surfaces).
    #[test]
    fn caps_and_env_carry_exactly(caps in arb_caps(), env in arb_environment()) {
        let mut buf = Vec::new();
        encode_request(0, &Request::Hello(caps), &mut buf).unwrap();
        let (_, got, _) = decode_request(&buf).unwrap().unwrap();
        prop_assert_eq!(got, Request::Hello(caps));

        let mut buf = Vec::new();
        let req = Request::Branch { snap: SnapId(1), env: env.clone() };
        encode_request(0, &req, &mut buf).unwrap();
        let (_, got, _) = decode_request(&buf).unwrap().unwrap();
        prop_assert_eq!(got, req);
    }
}

/// A request body exceeding `MAX_FRAME_LEN` makes `encode_request` return
/// `BadLength` and leave `buf` unchanged — never a panic, a truncation, or a
/// frame the decoder's cap would reject.
#[test]
fn oversize_request_body_is_bad_length_and_leaves_buf_untouched() {
    let req = Request::Branch {
        snap: SnapId(0),
        // One byte past the cap, before adding the tag/snap/version/len overhead.
        env: Reproducer {
            blob_version: 0,
            bytes: vec![0u8; MAX_FRAME_LEN + 1],
        },
    };
    let mut buf = vec![0xAB, 0xCD]; // pre-existing content must survive.
    let err = encode_request(0, &req, &mut buf).unwrap_err();
    assert_eq!(err, ProtocolError::BadLength);
    assert_eq!(buf, vec![0xAB, 0xCD], "buf is unchanged on BadLength");
}

/// A reply body exceeding `MAX_FRAME_LEN` likewise returns `BadLength`. Exercised
/// via a `Decision` stop with an over-cap `ctx`.
#[test]
fn oversize_reply_body_is_bad_length() {
    use control_proto::{DecisionId, Moment, Reply, StopReason};
    let reply = Ok(Reply::Stop(StopReason::Decision {
        vtime: Moment(0),
        id: DecisionId(0),
        ctx: vec![0u8; MAX_FRAME_LEN + 1],
    }));
    let mut buf = Vec::new();
    let err = encode_reply(0, &reply, &mut buf).unwrap_err();
    assert_eq!(err, ProtocolError::BadLength);
    assert!(buf.is_empty(), "buf is unchanged on BadLength");
}

/// A body exactly at `MAX_FRAME_LEN` is accepted and round-trips (the cap is
/// inclusive). Uses a reply `Hash` is too small, so use a `Decision` `ctx` sized
/// so the whole body is exactly `MAX_FRAME_LEN`.
#[test]
fn body_exactly_at_cap_is_accepted() {
    use control_proto::{DecisionId, Moment, Reply, StopReason};
    // body = RESULT_OK(1) + REPLY_STOP(1) + SR_DECISION(1) + vtime(8) + id(8)
    //        + ctx_len(4) + ctx => overhead 23 bytes before ctx.
    let overhead = 1 + 1 + 1 + 8 + 8 + 4;
    let ctx = vec![0u8; MAX_FRAME_LEN - overhead];
    let reply = Ok(Reply::Stop(StopReason::Decision {
        vtime: Moment(1),
        id: DecisionId(2),
        ctx,
    }));
    let mut buf = Vec::new();
    encode_reply(7, &reply, &mut buf).expect("body exactly at cap encodes");
    let (seq, got, consumed) = decode_reply(&buf).expect("decode").expect("complete");
    assert_eq!(seq, 7);
    assert_eq!(got, reply);
    assert_eq!(consumed, buf.len());
}
