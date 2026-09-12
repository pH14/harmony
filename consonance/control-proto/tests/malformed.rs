// SPDX-License-Identifier: AGPL-3.0-or-later

mod common;

use common::{arb_reply_result, arb_request};
use control_proto::{
    MAX_FRAME_LEN, PROTO_VERSION, ProtocolError, decode_reply, decode_request, encode_reply,
    encode_request,
};
use proptest::prelude::*;

const MAGIC: [u8; 4] = *b"CTL1";
const HEADER_LEN: usize = 14;

fn header_only(version: u16, seq: u32, len: u32) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&MAGIC);
    v.extend_from_slice(&version.to_le_bytes());
    v.extend_from_slice(&seq.to_le_bytes());
    v.extend_from_slice(&len.to_le_bytes());
    v
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn decode_never_panics_on_arbitrary_bytes(bytes in prop::collection::vec(any::<u8>(), 0..2048)) {
        let _ = decode_request(&bytes);
        let _ = decode_reply(&bytes);
    }

    #[test]
    fn single_byte_mutations_never_panic(
        seq in any::<u32>(),
        req in arb_request(),
        idx in any::<prop::sample::Index>(),
        val in any::<u8>(),
    ) {
        let mut buf = Vec::new();
        encode_request(seq, &req, &mut buf).unwrap();
        let i = idx.index(buf.len());
        buf[i] = val;
        let _ = decode_request(&buf);
        let _ = decode_reply(&buf);
    }

    #[test]
    fn single_byte_mutations_of_reply_never_panic(
        seq in any::<u32>(),
        reply in arb_reply_result(),
        idx in any::<prop::sample::Index>(),
        val in any::<u8>(),
    ) {
        let mut buf = Vec::new();
        encode_reply(seq, &reply, &mut buf).unwrap();
        let i = idx.index(buf.len());
        buf[i] = val;
        let _ = decode_reply(&buf);
        let _ = decode_request(&buf);
    }

    #[test]
    fn truncations_of_every_length_need_more(seq in any::<u32>(), req in arb_request()) {
        let mut buf = Vec::new();
        encode_request(seq, &req, &mut buf).unwrap();
        for n in 0..buf.len() {
            prop_assert_eq!(
                decode_request(&buf[..n]).expect("truncation is never an error"),
                None,
                "prefix of length {} must be need-more", n
            );
        }
        prop_assert!(decode_request(&buf).unwrap().is_some(), "full frame decodes");
    }

    #[test]
    fn reply_truncations_need_more(seq in any::<u32>(), reply in arb_reply_result()) {
        let mut buf = Vec::new();
        encode_reply(seq, &reply, &mut buf).unwrap();
        for n in 0..buf.len() {
            prop_assert_eq!(decode_reply(&buf[..n]).expect("never an error"), None);
        }
        prop_assert!(decode_reply(&buf).unwrap().is_some());
    }
}

#[test]
fn oversize_len_is_bad_length_before_buffering() {
    for len in [MAX_FRAME_LEN as u32 + 1, u32::MAX, 0x4000_0000] {
        let header = header_only(PROTO_VERSION, 1, len);
        assert_eq!(header.len(), HEADER_LEN, "no body present");
        assert_eq!(
            decode_request(&header),
            Err(ProtocolError::BadLength),
            "request: oversize len rejected from the header alone"
        );
        assert_eq!(
            decode_reply(&header),
            Err(ProtocolError::BadLength),
            "reply: oversize len rejected from the header alone"
        );
    }
}

#[test]
fn len_exactly_at_cap_is_need_more_not_bad_length() {
    let header = header_only(PROTO_VERSION, 1, MAX_FRAME_LEN as u32);
    assert_eq!(decode_request(&header), Ok(None));
    assert_eq!(decode_reply(&header), Ok(None));
}

#[test]
fn bad_magic_and_version_are_distinct_errors() {
    let mut bad_magic = header_only(PROTO_VERSION, 1, 0);
    bad_magic[0] ^= 0xFF;
    assert_eq!(decode_request(&bad_magic), Err(ProtocolError::BadMagic));
    assert_eq!(decode_reply(&bad_magic), Err(ProtocolError::BadMagic));

    let bad_version = header_only(PROTO_VERSION + 1, 1, 0);
    assert_eq!(decode_request(&bad_version), Err(ProtocolError::BadVersion));
    assert_eq!(decode_reply(&bad_version), Err(ProtocolError::BadVersion));
}

#[test]
fn malformed_complete_body_is_short_frame() {
    let mut buf = header_only(PROTO_VERSION, 1, 1);
    buf.push(0xFF);
    assert_eq!(decode_request(&buf), Err(ProtocolError::ShortFrame));

    let mut buf = header_only(PROTO_VERSION, 1, 2);
    buf.push(0x02);
    buf.push(0x00);
    assert_eq!(decode_request(&buf), Err(ProtocolError::ShortFrame));
}

#[test]
fn retired_snapid_tag_is_rejected() {
    let mut body = vec![0x00u8, 0x02];
    body.extend_from_slice(&9u64.to_le_bytes());
    let mut buf = header_only(PROTO_VERSION, 1, body.len() as u32);
    buf.extend_from_slice(&body);
    assert_eq!(decode_reply(&buf), Err(ProtocolError::ShortFrame));
}

#[test]
fn snapshot_reply_malformed_bodies_are_rejected() {
    let mut body = vec![0x00u8, 0x0A];
    body.extend_from_slice(&9u64.to_le_bytes());
    body.extend_from_slice(&0x1234u64.to_le_bytes());
    body.extend_from_slice(&3u64.to_le_bytes());
    body.push(0x00);
    let frame = |body: &[u8]| {
        let mut buf = header_only(PROTO_VERSION, 1, body.len() as u32);
        buf.extend_from_slice(body);
        buf
    };
    assert!(
        decode_reply(&frame(&body)).unwrap().is_some(),
        "the intact body decodes"
    );
    for n in 2..body.len() {
        assert_eq!(
            decode_reply(&frame(&body[..n])),
            Err(ProtocolError::ShortFrame),
            "snapshot body truncated to {n} bytes must be rejected"
        );
    }
    let mut bad_taint = body.clone();
    *bad_taint.last_mut().unwrap() = 0x02;
    assert_eq!(
        decode_reply(&frame(&bad_taint)),
        Err(ProtocolError::ShortFrame)
    );
    let mut trailing = body.clone();
    trailing.push(0x00);
    assert_eq!(
        decode_reply(&frame(&trailing)),
        Err(ProtocolError::ShortFrame)
    );
}

#[test]
fn inner_length_overrun_is_short_frame_not_overread() {
    let mut body = vec![0x04u8];
    body.extend_from_slice(&7u64.to_le_bytes());
    body.extend_from_slice(&2u16.to_le_bytes());
    body.extend_from_slice(&u32::MAX.to_le_bytes());
    let mut buf = header_only(PROTO_VERSION, 1, body.len() as u32);
    buf.extend_from_slice(&body);
    assert_eq!(decode_request(&buf), Err(ProtocolError::ShortFrame));
}

#[test]
fn snapshot_refusal_diagnostic_rejects_malformed_text_and_lengths() {
    let frame = |body: &[u8]| {
        let mut buf = header_only(PROTO_VERSION, 1, body.len() as u32);
        buf.extend_from_slice(body);
        buf
    };
    let body = [0x01, 0x14, 0x03, 0x00, 0x00, 0x00, b'S', b'D', b'K'];
    assert!(decode_reply(&frame(&body)).unwrap().is_some());
    for n in 2..body.len() {
        assert_eq!(
            decode_reply(&frame(&body[..n])),
            Err(ProtocolError::ShortFrame)
        );
    }
    let mut invalid_utf8 = body;
    invalid_utf8[6] = 0xff;
    assert_eq!(
        decode_reply(&frame(&invalid_utf8)),
        Err(ProtocolError::ShortFrame)
    );
    let mut trailing = body.to_vec();
    trailing.push(0);
    assert_eq!(
        decode_reply(&frame(&trailing)),
        Err(ProtocolError::ShortFrame)
    );
    assert_eq!(
        decode_reply(&frame(&[0x01, 0x14, 0xff, 0xff, 0xff, 0xff])),
        Err(ProtocolError::ShortFrame)
    );
}
