// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 4 — version negotiation. A `Hello` with an out-of-range
//! `protocol_version` / `env_version` range is detectable from the decoded
//! `Caps` alone; and an off-version `Environment.blob_version` decodes to a
//! `Request` carrying it (so the backend can answer `BadEnvVersion`), never a
//! decode error.

use control_proto::{
    CapFlags, Caps, CoverageGeometry, Environment, PROTO_VERSION, Request, SnapId, decode_request,
    encode_request,
};

/// What a backend supports, for the negotiation predicates below.
const OUR_PROTOCOL: u16 = PROTO_VERSION;
const OUR_ENV_MIN: u16 = 1;
const OUR_ENV_MAX: u16 = 3;

/// Acceptable iff the peer speaks our protocol version and the env-version ranges
/// overlap — decided from `Caps` fields only.
fn caps_acceptable(c: &Caps) -> bool {
    c.protocol_version == OUR_PROTOCOL
        && c.env_version_min <= OUR_ENV_MAX
        && c.env_version_max >= OUR_ENV_MIN
        && c.env_version_min <= c.env_version_max
}

fn hello_caps(caps: Caps) -> Caps {
    // round-trip a Hello and return the decoded Caps.
    let mut buf = Vec::new();
    encode_request(0, &Request::Hello(caps), &mut buf).unwrap();
    match decode_request(&buf).unwrap().unwrap().1 {
        Request::Hello(c) => c,
        other => panic!("expected Hello, got {other:?}"),
    }
}

#[test]
fn in_range_hello_is_accepted() {
    let caps = Caps {
        protocol_version: OUR_PROTOCOL,
        env_version_min: 1,
        env_version_max: 2,
        coverage: CoverageGeometry {
            map_bytes: 4096,
            producer: 0,
        },
        flags: CapFlags::GUEST_HAS_SDK,
    };
    let decoded = hello_caps(caps);
    assert_eq!(decoded, caps, "Caps carry exactly");
    assert!(caps_acceptable(&decoded));
}

#[test]
fn wrong_protocol_version_is_detectable_from_caps() {
    let caps = Caps {
        protocol_version: OUR_PROTOCOL + 7,
        env_version_min: 1,
        env_version_max: 2,
        coverage: CoverageGeometry {
            map_bytes: 0,
            producer: 0,
        },
        flags: CapFlags::NONE,
    };
    // The frame itself decodes fine (wire framing is unaffected)...
    let decoded = hello_caps(caps);
    assert_eq!(decoded.protocol_version, OUR_PROTOCOL + 7);
    // ...and the mismatch is detectable from the Caps alone.
    assert!(!caps_acceptable(&decoded));
}

#[test]
fn disjoint_env_range_is_detectable_from_caps() {
    let caps = Caps {
        protocol_version: OUR_PROTOCOL,
        env_version_min: 9,
        env_version_max: 12, // entirely above our 1..=3
        coverage: CoverageGeometry {
            map_bytes: 0,
            producer: 0,
        },
        flags: CapFlags::NONE,
    };
    let decoded = hello_caps(caps);
    assert!(!caps_acceptable(&decoded), "disjoint env range rejected");
}

/// The load-bearing gate-4 property: an off-version `Environment.blob_version`
/// is **carried**, not a decode error — so the backend (not the codec) gets to
/// answer `BadEnvVersion`.
#[test]
fn off_version_env_blob_decodes_and_carries_the_version() {
    for blob_version in [0u16, 4, 99, u16::MAX] {
        let req = Request::Branch {
            snap: SnapId(1),
            env: Environment {
                blob_version,
                bytes: vec![0xDE, 0xAD, 0xBE, 0xEF],
            },
        };
        let mut buf = Vec::new();
        encode_request(0, &req, &mut buf).unwrap();

        // Decodes cleanly (NOT an error) regardless of the env blob version...
        let (_, got, _) = decode_request(&buf)
            .expect("clean decode")
            .expect("complete");
        match got {
            Request::Branch { env, .. } => {
                // ...and carries the exact version for the backend to judge.
                assert_eq!(env.blob_version, blob_version);
                assert_eq!(env.bytes, vec![0xDE, 0xAD, 0xBE, 0xEF]);
            }
            other => panic!("expected Branch, got {other:?}"),
        }
    }
}
