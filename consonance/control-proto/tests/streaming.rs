// SPDX-License-Identifier: AGPL-3.0-or-later

mod common;

use common::{arb_reply_result, arb_request};
use control_proto::{
    ControlError, Reply, Request, decode_reply, decode_request, encode_reply, encode_request,
};
use proptest::prelude::*;

fn decode_all_requests(bytes: &[u8]) -> Vec<(u32, Request)> {
    let mut out = Vec::new();
    let mut off = 0;
    while let Some((seq, req, consumed)) = decode_request(&bytes[off..]).expect("clean decode") {
        out.push((seq, req));
        off += consumed;
    }
    out
}

fn stream_decode_requests(bytes: &[u8]) -> Vec<(u32, Request)> {
    let mut out = Vec::new();
    let mut acc: Vec<u8> = Vec::new();
    for &b in bytes {
        acc.push(b);
        while let Some((seq, req, consumed)) = decode_request(&acc).expect("clean decode") {
            out.push((seq, req));
            acc.drain(..consumed);
        }
    }
    out
}

fn decode_all_replies(bytes: &[u8]) -> Vec<(u32, Result<Reply, ControlError>)> {
    let mut out = Vec::new();
    let mut off = 0;
    while let Some((seq, reply, consumed)) = decode_reply(&bytes[off..]).expect("clean decode") {
        out.push((seq, reply));
        off += consumed;
    }
    out
}

fn stream_decode_replies(bytes: &[u8]) -> Vec<(u32, Result<Reply, ControlError>)> {
    let mut out = Vec::new();
    let mut acc: Vec<u8> = Vec::new();
    for &b in bytes {
        acc.push(b);
        while let Some((seq, reply, consumed)) = decode_reply(&acc).expect("clean decode") {
            out.push((seq, reply));
            acc.drain(..consumed);
        }
    }
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn requests_stream_same_as_whole(
        frames in prop::collection::vec((any::<u32>(), arb_request()), 0..8)
    ) {
        let mut buf = Vec::new();
        for (seq, req) in &frames {
            encode_request(*seq, req, &mut buf).unwrap();
        }
        let whole = decode_all_requests(&buf);
        let streamed = stream_decode_requests(&buf);
        prop_assert_eq!(&whole, &streamed, "byte-at-a-time matches whole-buffer");
        prop_assert_eq!(whole, frames, "and both equal what was encoded");
    }

    #[test]
    fn replies_stream_same_as_whole(
        frames in prop::collection::vec((any::<u32>(), arb_reply_result()), 0..8)
    ) {
        let mut buf = Vec::new();
        for (seq, reply) in &frames {
            encode_reply(*seq, reply, &mut buf).unwrap();
        }
        let whole = decode_all_replies(&buf);
        let streamed = stream_decode_replies(&buf);
        prop_assert_eq!(&whole, &streamed);
        prop_assert_eq!(whole, frames);
    }
}
