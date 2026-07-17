// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 3 — adversarial decode (the in-tree twin of the `cargo-fuzz` target in
//! `fuzz/`). `decode_*` on arbitrary byte strings, on valid frames with
//! single-byte mutations, and on truncations of every length never panics, never
//! reads out of bounds, and reports `ProtocolError` cleanly. A header advertising
//! a body length `> MAX_FRAME_LEN` is rejected with `BadLength` immediately —
//! before the body is buffered.

mod common;

use common::{arb_reply_result, arb_request};
use control_proto::{
    MAX_FRAME_LEN, PROTO_VERSION, ProtocolError, decode_reply, decode_request, encode_reply,
    encode_request,
};
use proptest::prelude::*;

const MAGIC: [u8; 4] = *b"CTL1";
const HEADER_LEN: usize = 14;

/// Assemble a raw header with an explicit `len` field and no body.
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

    /// `decode_request` / `decode_reply` never panic on arbitrary bytes.
    #[test]
    fn decode_never_panics_on_arbitrary_bytes(bytes in prop::collection::vec(any::<u8>(), 0..2048)) {
        let _ = decode_request(&bytes);
        let _ = decode_reply(&bytes);
    }

    /// Every single-byte mutation of a valid request frame decodes without
    /// panicking (it may now be a different valid frame, an error, or need-more).
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
        // Must not panic; whatever it returns is acceptable.
        let _ = decode_request(&buf);
        let _ = decode_reply(&buf);
    }

    /// Symmetric to the above for the reply decoder: every single-byte mutation
    /// of a valid *reply* frame decodes without panicking. Seeding from
    /// `arb_reply_result` exercises the reply-only encodings a mutated request
    /// frame never reaches — `RESULT_ERR`/`ControlError`, `Reply::Hash`'s fixed
    /// 32-byte array, and the `StopReason` variants.
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
        // Must not panic; feed the mutated reply frame to both decoders.
        let _ = decode_reply(&buf);
        let _ = decode_request(&buf);
    }

    /// Every proper prefix (truncation) of a valid frame is "need more"
    /// (`Ok(None)`), and the full frame decodes — for both decoders. Truncation
    /// is never an error and never a panic.
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

    /// Same truncation property for replies.
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

/// A header advertising a body length `> MAX_FRAME_LEN` is rejected with
/// `BadLength` from the **header alone** — the buffer holds only the 14-byte
/// header and no body, proving the cap is checked before any body is buffered or
/// allocated.
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

/// The cap is inclusive: a header declaring exactly `MAX_FRAME_LEN` is *not*
/// `BadLength` — it is need-more (`Ok(None)`), waiting for the (huge but legal)
/// body. This pins that the rejection boundary is `len > MAX_FRAME_LEN`, not `>=`.
#[test]
fn len_exactly_at_cap_is_need_more_not_bad_length() {
    let header = header_only(PROTO_VERSION, 1, MAX_FRAME_LEN as u32);
    assert_eq!(decode_request(&header), Ok(None));
    assert_eq!(decode_reply(&header), Ok(None));
}

/// Bad magic and bad wire-version are reported cleanly and distinctly.
#[test]
fn bad_magic_and_version_are_distinct_errors() {
    // Wrong magic, otherwise a well-formed empty-body header.
    let mut bad_magic = header_only(PROTO_VERSION, 1, 0);
    bad_magic[0] ^= 0xFF;
    assert_eq!(decode_request(&bad_magic), Err(ProtocolError::BadMagic));
    assert_eq!(decode_reply(&bad_magic), Err(ProtocolError::BadMagic));

    // Right magic, unsupported wire-format version.
    let bad_version = header_only(PROTO_VERSION + 1, 1, 0);
    assert_eq!(decode_request(&bad_version), Err(ProtocolError::BadVersion));
    assert_eq!(decode_reply(&bad_version), Err(ProtocolError::BadVersion));
}

/// A complete frame whose body is an unknown discriminant, or carries trailing
/// bytes inside the declared length, is `ShortFrame` — not a panic, not need-more.
#[test]
fn malformed_complete_body_is_short_frame() {
    // Declared len = 1, body = an unknown request tag (0xFF).
    let mut buf = header_only(PROTO_VERSION, 1, 1);
    buf.push(0xFF);
    assert_eq!(decode_request(&buf), Err(ProtocolError::ShortFrame));

    // A valid Snapshot (tag 2) but with one trailing byte inside the body.
    let mut buf = header_only(PROTO_VERSION, 1, 2);
    buf.push(0x02); // REQ_SNAPSHOT
    buf.push(0x00); // trailing byte — body must be exactly the tag
    assert_eq!(decode_request(&buf), Err(ProtocolError::ShortFrame));
}

/// The retired bare-handle snapshot reply (wire tag 2, pre-127 `Reply::SnapId`)
/// is rejected as a malformed body — a hostile or stale peer cannot smuggle a
/// **cut-less** snapshot handle past the decoder. The tag is reserved, never
/// reused.
#[test]
fn retired_snapid_tag_is_rejected() {
    // RESULT_OK (0x00) · retired tag 0x02 · a plausible u64 handle.
    let mut body = vec![0x00u8, 0x02];
    body.extend_from_slice(&9u64.to_le_bytes());
    let mut buf = header_only(PROTO_VERSION, 1, body.len() as u32);
    buf.extend_from_slice(&body);
    assert_eq!(decode_reply(&buf), Err(ProtocolError::ShortFrame));
}

/// Hostile decodes of the seal-bound `Snapshot` reply (task 127): a body
/// truncated at **every** field boundary of `id · at · sdk_events · tainted` is
/// `ShortFrame`; a non-canonical taint byte is rejected (the encoding stays
/// one-to-one); trailing bytes inside the declared body are rejected. No
/// partial cut can ever decode.
#[test]
fn snapshot_reply_hostile_bodies_are_rejected() {
    // The full well-formed body: RESULT_OK · REPLY_SNAPSHOT (0x0A) · id ·
    // at · sdk_events · tainted.
    let mut body = vec![0x00u8, 0x0A];
    body.extend_from_slice(&9u64.to_le_bytes()); // id
    body.extend_from_slice(&0x1234u64.to_le_bytes()); // at
    body.extend_from_slice(&3u64.to_le_bytes()); // sdk_events
    body.push(0x00); // tainted = false
    let frame = |body: &[u8]| {
        let mut buf = header_only(PROTO_VERSION, 1, body.len() as u32);
        buf.extend_from_slice(body);
        buf
    };
    assert!(
        decode_reply(&frame(&body)).unwrap().is_some(),
        "the intact body decodes"
    );
    // Every proper truncation of the body (declared len shrunk with it) fails
    // loudly — a handle can never arrive without its complete cut and taint.
    for n in 2..body.len() {
        assert_eq!(
            decode_reply(&frame(&body[..n])),
            Err(ProtocolError::ShortFrame),
            "snapshot body truncated to {n} bytes must be rejected"
        );
    }
    // A non-canonical taint byte (2) is rejected — no spurious `true`.
    let mut bad_taint = body.clone();
    *bad_taint.last_mut().unwrap() = 0x02;
    assert_eq!(decode_reply(&frame(&bad_taint)), Err(ProtocolError::ShortFrame));
    // Trailing bytes inside the declared body are rejected (canonical encoding).
    let mut trailing = body.clone();
    trailing.push(0x00);
    assert_eq!(decode_reply(&frame(&trailing)), Err(ProtocolError::ShortFrame));
}

/// An inner length field that runs past the declared frame body is `ShortFrame`
/// — and never causes an over-read or a multi-gigabyte allocation, because the
/// inner blob is sliced against the (bounded) body, not the wire.
#[test]
fn inner_length_overrun_is_short_frame_not_overread() {
    // Branch body: tag(4) + snap u64(8) + blob_version u16(2) + env_len u32.
    // Declare env_len = u32::MAX with no env bytes present in the body.
    let mut body = vec![0x04u8]; // REQ_BRANCH
    body.extend_from_slice(&7u64.to_le_bytes()); // snap
    body.extend_from_slice(&2u16.to_le_bytes()); // blob_version
    body.extend_from_slice(&u32::MAX.to_le_bytes()); // env_len lies
    let mut buf = header_only(PROTO_VERSION, 1, body.len() as u32);
    buf.extend_from_slice(&body);
    assert_eq!(decode_request(&buf), Err(ProtocolError::ShortFrame));
}
