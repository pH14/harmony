// SPDX-License-Identifier: AGPL-3.0-or-later

#![no_main]

use control_proto::{
    PROTO_VERSION, decode_reply, decode_request, encode_reply, encode_request,
};
use libfuzzer_sys::fuzz_target;

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

fn wrap(body: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(14 + body.len());
    v.extend_from_slice(b"CTL1");
    v.extend_from_slice(&PROTO_VERSION.to_le_bytes());
    v.extend_from_slice(&0u32.to_le_bytes());
    v.extend_from_slice(&(body.len() as u32).to_le_bytes());
    v.extend_from_slice(body);
    v
}

fuzz_target!(|data: &[u8]| {
    check_request(data);
    check_reply(data);

    let framed = wrap(data);
    check_request(&framed);
    check_reply(&framed);
});
