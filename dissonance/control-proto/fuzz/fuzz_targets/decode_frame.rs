// SPDX-License-Identifier: AGPL-3.0-or-later
//! Acceptance gate 3 — the wire decoder is a Tier-1 fuzz target. `decode_request`
//! and `decode_reply` must never panic and never read out of bounds on arbitrary
//! bytes, and every frame they *accept* must round-trip canonically:
//! `encode(decode(x)) == x[..consumed]`, and re-decoding reproduces the value.
//!
//! Two passes per input: the raw bytes (the untrusted-transport boundary), and
//! the same bytes wrapped in a valid header so the body parser is reached far
//! more often than random magic would allow.
//!
//! Run (needs the pinned nightly + cargo-fuzz, per the crate IMPLEMENTATION.md):
//!   cargo +nightly-2026-06-16 fuzz run decode_frame

#![no_main]

use control_proto::{
    PROTO_VERSION, decode_reply, decode_request, encode_reply, encode_request,
};
use libfuzzer_sys::fuzz_target;

/// Any request the decoder accepts must re-encode to exactly the consumed bytes
/// and re-decode to the same value (canonical, stable encoding).
fn check_request(data: &[u8]) {
    if let Ok(Some((seq, req, consumed))) = decode_request(data) {
        let mut re = Vec::new();
        encode_request(seq, &req, &mut re).expect("a decoded request must re-encode");
        assert_eq!(
            re.as_slice(),
            &data[..consumed],
            "encode∘decode is canonical for requests"
        );
        let (s2, r2, c2) = decode_request(&re)
            .expect("re-decode is clean")
            .expect("re-decode is complete");
        assert_eq!(s2, seq);
        assert_eq!(r2, req);
        assert_eq!(c2, re.len());
    }
}

/// Same canonical round-trip for replies.
fn check_reply(data: &[u8]) {
    if let Ok(Some((seq, reply, consumed))) = decode_reply(data) {
        let mut re = Vec::new();
        encode_reply(seq, &reply, &mut re).expect("a decoded reply must re-encode");
        assert_eq!(
            re.as_slice(),
            &data[..consumed],
            "encode∘decode is canonical for replies"
        );
        let (s2, r2, c2) = decode_reply(&re)
            .expect("re-decode is clean")
            .expect("re-decode is complete");
        assert_eq!(s2, seq);
        assert_eq!(r2, reply);
        assert_eq!(c2, re.len());
    }
}

/// Wrap `body` in a valid frame header so the body parser is exercised.
fn wrap(body: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(14 + body.len());
    v.extend_from_slice(b"CTL1");
    v.extend_from_slice(&PROTO_VERSION.to_le_bytes());
    v.extend_from_slice(&0u32.to_le_bytes()); // seq
    v.extend_from_slice(&(body.len() as u32).to_le_bytes());
    v.extend_from_slice(body);
    v
}

fuzz_target!(|data: &[u8]| {
    // 1. Raw untrusted bytes: never panic; accepted frames round-trip.
    check_request(data);
    check_reply(data);

    // 2. Valid envelope around arbitrary body bytes: reach the body parsers.
    let framed = wrap(data);
    check_request(&framed);
    check_reply(&framed);
});
