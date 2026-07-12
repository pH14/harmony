// SPDX-License-Identifier: AGPL-3.0-or-later
//! Hostile-blob property + regression tests for the fallible [`SpecEnvCodec`]
//! seam (task 99, bead `hm-5d9`).
//!
//! A serialized reproducer is the artifact users pass around, load from disk,
//! and feed back in — untrusted by definition. These tests fuzz the public
//! decode path (`compose`/`mutate`) with truncations at every boundary, header
//! bit-flips, version skew, length-field overflow, and unsupported/overflowing
//! compositions, and assert every one yields a **typed
//! [`EnvCodecError`](explorer::EnvCodecError)** — never a panic, never an
//! abort. There is a named regression test for each malformation class the
//! spec calls out, plus proptest fuzzers over the whole space.
//!
//! The wire layout these tests exploit (stable; wire format is out of task-99
//! scope): a `R2A1` adapter blob is a 22-byte wrapper header
//! (`magic(4) | version(2) | base_offset(8) | pos(8)`) followed by the inner
//! task-24 `EnvSpec`, which itself opens with a fixed 15-byte prefix
//! (`magic(4) | version(2) | variant(1) | seed(8)`) and then a `u32`
//! length-prefixed policy.

use environment::{Action, EnvSpec, FaultPolicy, HostFault};
use explorer::{
    ADAPTER_BLOB_VERSION, AdapterEnv, EnvCodec, EnvCodecError, Environment, SpecEnvCodec,
};
use proptest::prelude::*;

/// Wrapper header length: `magic(4) | version(2) | base_offset(8) | pos(8)`.
const HEADER_LEN: usize = 22;
/// Byte offset of the inner `EnvSpec`'s version field within a blob:
/// after the 22-byte wrapper and the inner `magic(4)`.
const INNER_VERSION_OFF: usize = HEADER_LEN + 4;
/// Byte offset of the inner `EnvSpec`'s variant tag: after `magic(4) | ver(2)`.
const INNER_VARIANT_OFF: usize = HEADER_LEN + 6;
/// Byte offset of the inner policy's `u32` length prefix: after the inner
/// 15-byte prefix (`magic(4) | ver(2) | variant(1) | seed(8)`).
const INNER_POLICY_LEN_OFF: usize = HEADER_LEN + 15;

/// A well-formed `Recorded` spec with the given seed and override keys (host
/// faults, matching the in-crate adapter tests).
fn spec(seed: u64, keys: &[u64]) -> EnvSpec {
    let mut overrides = std::collections::BTreeMap::new();
    for &k in keys {
        overrides.insert(k, Action::Host(HostFault::InjectInterrupt { vector: 32 }));
    }
    EnvSpec::Recorded {
        seed,
        policy: FaultPolicy::none(),
        overrides,
        standing: Vec::new(),
        reseeds: std::collections::BTreeMap::new(),
    }
}

/// A well-formed adapter blob (the artifact the seam is supposed to accept).
fn valid_blob(base_offset: u64, pos: u64, seed: u64, keys: &[u64]) -> Environment {
    AdapterEnv {
        base_offset,
        pos,
        spec: spec(seed, keys),
    }
    .encode()
}

/// Both untrusted decode-path methods, so every test asserts the property on the
/// whole public surface at once. `mutate` takes one blob; `compose` takes the
/// blob as both operands (enough to exercise its decode of each side).
fn drive_both(env: &Environment) -> [Result<Environment, EnvCodecError>; 2] {
    [
        SpecEnvCodec.mutate(env, 0xA11CE),
        SpecEnvCodec.compose(env, env),
    ]
}

// ---------------------------------------------------------------------------
// Positive control: a valid blob is accepted (so the fuzzers are not vacuous).
// ---------------------------------------------------------------------------

#[test]
fn a_valid_blob_still_decodes_and_composes() {
    let base = valid_blob(0, 100, 7, &[40, 150]);
    let delta = valid_blob(100, 260, 7, &[5, 60]);
    assert!(SpecEnvCodec.mutate(&base, 0x5A17).is_ok());
    assert!(SpecEnvCodec.compose(&base, &delta).is_ok());
}

// ---------------------------------------------------------------------------
// Regression: one named test per malformation class in the spec.
// ---------------------------------------------------------------------------

/// Truncation at **every** byte boundary of a valid blob is `Malformed`, never a
/// panic. (Fuzz-shaped: truncations at every boundary.)
#[test]
fn truncation_at_every_boundary_is_malformed() {
    let full = valid_blob(10, 200, 7, &[40, 150, 220]);
    for cut in 0..full.bytes.len() {
        let env = Environment {
            blob_version: ADAPTER_BLOB_VERSION,
            bytes: full.bytes[..cut].to_vec(),
        };
        for r in drive_both(&env) {
            assert_eq!(
                r,
                Err(EnvCodecError::Malformed(ADAPTER_BLOB_VERSION)),
                "a blob truncated to {cut} bytes must be a typed Malformed"
            );
        }
    }
}

/// A bit flipped in the container magic (bytes 0..4) is `Malformed`. (Bit flips
/// in headers.)
#[test]
fn magic_bit_flip_is_malformed() {
    let full = valid_blob(0, 0, 1, &[]);
    for byte in 0..4 {
        for bit in 0..8 {
            let mut bytes = full.bytes.clone();
            bytes[byte] ^= 1 << bit;
            let env = Environment {
                blob_version: ADAPTER_BLOB_VERSION,
                bytes,
            };
            for r in drive_both(&env) {
                assert_eq!(
                    r,
                    Err(EnvCodecError::Malformed(ADAPTER_BLOB_VERSION)),
                    "magic byte {byte} bit {bit} flip must be Malformed"
                );
            }
        }
    }
}

/// A skewed wrapper version — both in the declared `blob_version` field and in
/// the header's own version bytes (4..6) — is `Malformed` carrying the declared
/// version. (Version skew.)
#[test]
fn wrapper_version_skew_is_malformed() {
    let full = valid_blob(0, 0, 1, &[]);
    // The declared blob_version drives the error's carried version.
    for v in [0u16, 2, 99, u16::MAX] {
        let env = Environment {
            blob_version: v,
            bytes: full.bytes.clone(),
        };
        for r in drive_both(&env) {
            assert_eq!(r, Err(EnvCodecError::Malformed(v)));
        }
    }
    // A header-internal version byte flip (the declared version stays 1) is
    // caught by the in-body magic/version check.
    let mut bytes = full.bytes.clone();
    bytes[4] ^= 0x01;
    let env = Environment {
        blob_version: ADAPTER_BLOB_VERSION,
        bytes,
    };
    for r in drive_both(&env) {
        assert_eq!(r, Err(EnvCodecError::Malformed(ADAPTER_BLOB_VERSION)));
    }
}

/// A skewed **inner** `EnvSpec` version (bytes 26..28) is `Malformed`: the
/// wrapper decodes, the inner task-24 codec rejects the version. (Version skew,
/// inner layer.)
#[test]
fn inner_spec_version_skew_is_malformed() {
    let full = valid_blob(0, 0, 1, &[]);
    let mut bytes = full.bytes.clone();
    bytes[INNER_VERSION_OFF] ^= 0xFF;
    let env = Environment {
        blob_version: ADAPTER_BLOB_VERSION,
        bytes,
    };
    for r in drive_both(&env) {
        assert_eq!(r, Err(EnvCodecError::Malformed(ADAPTER_BLOB_VERSION)));
    }
}

/// A length field claiming far more bytes than the buffer holds is `Malformed`,
/// not an out-of-bounds panic or a huge allocation. Here the inner policy's
/// `u32` length prefix is set to `u32::MAX`. (Length-field overflow.)
#[test]
fn length_field_overflow_is_malformed() {
    let full = valid_blob(0, 0, 7, &[10, 20]);
    assert!(full.bytes.len() >= INNER_POLICY_LEN_OFF + 4);
    let mut bytes = full.bytes.clone();
    bytes[INNER_POLICY_LEN_OFF..INNER_POLICY_LEN_OFF + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    let env = Environment {
        blob_version: ADAPTER_BLOB_VERSION,
        bytes,
    };
    for r in drive_both(&env) {
        assert_eq!(r, Err(EnvCodecError::Malformed(ADAPTER_BLOB_VERSION)));
    }
}

/// An unknown inner variant tag (neither `0` Seeded nor `1` Recorded) is
/// `Malformed`. (Unknown composition/structure tags.)
#[test]
fn unknown_variant_tag_is_malformed() {
    let full = valid_blob(0, 0, 7, &[]);
    for tag in [2u8, 7, 255] {
        let mut bytes = full.bytes.clone();
        bytes[INNER_VARIANT_OFF] = tag;
        let env = Environment {
            blob_version: ADAPTER_BLOB_VERSION,
            bytes,
        };
        for r in drive_both(&env) {
            assert_eq!(r, Err(EnvCodecError::Malformed(ADAPTER_BLOB_VERSION)));
        }
    }
}

/// Two well-formed blobs the wire codec cannot compose (a seed mismatch) fail
/// closed with `UnsupportedComposition`. (Unsupported composition.)
#[test]
fn unsupported_composition_is_typed() {
    let base = valid_blob(0, 100, 7, &[]);
    let delta = valid_blob(100, 200, 8, &[]); // seed 8 ≠ 7
    assert_eq!(
        SpecEnvCodec.compose(&base, &delta),
        Err(EnvCodecError::UnsupportedComposition)
    );
}

/// A `Moment` re-key that pushes an override past `u64::MAX` is `Overflow`, not a
/// wrap. (Overflow.)
#[test]
fn compose_rekey_overflow_is_typed() {
    let base = valid_blob(0, u64::MAX - 1, 7, &[]);
    let delta = valid_blob(u64::MAX - 1, u64::MAX, 7, &[10]); // 10 + (MAX-1) overflows
    assert_eq!(
        SpecEnvCodec.compose(&base, &delta),
        Err(EnvCodecError::Overflow)
    );
}

/// A structurally-valid but internally mis-ordered chain (a delta keyed before
/// the base's root; a base captured behind its root) is `MisorderedChain`.
#[test]
fn misordered_chain_is_typed() {
    // compose: delta origin 100 < base root 200.
    let base = valid_blob(200, 300, 7, &[]);
    let delta = valid_blob(100, 250, 7, &[]);
    assert!(matches!(
        SpecEnvCodec.compose(&base, &delta),
        Err(EnvCodecError::MisorderedChain(_))
    ));
    // mutate: capture pos 100 behind root 200.
    let bad = valid_blob(200, 100, 7, &[]);
    assert!(matches!(
        SpecEnvCodec.mutate(&bad, 1),
        Err(EnvCodecError::MisorderedChain(_))
    ));
}

// ---------------------------------------------------------------------------
// Proptest fuzzers over the whole hostile space.
// ---------------------------------------------------------------------------

// Miri (if ever enabled for this crate) runs the interpreted suite ~10–100×
// slower, so cut the case count under it; the crate has no `unsafe` today, so
// this is just future-proofing the runtime budget.
#[cfg(miri)]
const CASES: u32 = 16;
#[cfg(not(miri))]
const CASES: u32 = 512;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    /// The core untrusted-input property: **arbitrary bytes at any declared
    /// version never panic** — both public decode-path methods return a typed
    /// `Ok`/`Err`, never abort (conventions rule 4).
    #[test]
    fn arbitrary_bytes_never_panic(
        va in any::<u16>(),
        a in prop::collection::vec(any::<u8>(), 0..300),
        salt in any::<u64>(),
    ) {
        let env = Environment { blob_version: va, bytes: a };
        // A panic in either call fails the test; assert totality explicitly.
        prop_assert!(matches!(SpecEnvCodec.mutate(&env, salt), Ok(_) | Err(_)));
        prop_assert!(matches!(SpecEnvCodec.compose(&env, &env), Ok(_) | Err(_)));
    }

    /// Any blob whose declared version is not the adapter version is a typed
    /// `Malformed` carrying that version — regardless of payload.
    #[test]
    fn any_wrong_version_is_malformed(
        v in any::<u16>().prop_filter("off-version", |v| *v != ADAPTER_BLOB_VERSION),
        bytes in prop::collection::vec(any::<u8>(), 0..300),
    ) {
        let env = Environment { blob_version: v, bytes };
        prop_assert_eq!(SpecEnvCodec.mutate(&env, 0), Err(EnvCodecError::Malformed(v)));
        prop_assert_eq!(SpecEnvCodec.compose(&env, &env), Err(EnvCodecError::Malformed(v)));
    }

    /// Truncating a valid blob to any length below the full encoding is always a
    /// typed `Malformed` — a header/body cut can never be silently accepted.
    #[test]
    fn arbitrary_truncation_is_malformed(cut in 0usize..300) {
        let full = valid_blob(3, 210, 5, &[7, 90, 140]);
        prop_assume!(cut < full.bytes.len());
        let env = Environment {
            blob_version: ADAPTER_BLOB_VERSION,
            bytes: full.bytes[..cut].to_vec(),
        };
        prop_assert_eq!(
            SpecEnvCodec.mutate(&env, 0),
            Err(EnvCodecError::Malformed(ADAPTER_BLOB_VERSION))
        );
        prop_assert_eq!(
            SpecEnvCodec.compose(&env, &env),
            Err(EnvCodecError::Malformed(ADAPTER_BLOB_VERSION))
        );
    }

    /// A single bit flipped anywhere in the fixed header region (magic + inner
    /// magic/version/variant) is caught as `Malformed` — never a panic, never a
    /// silent accept. Confined to the structural bytes; flips in the
    /// `base_offset`/`pos`/override payload can legitimately re-decode.
    #[test]
    fn structural_bit_flip_is_malformed(
        byte in prop::sample::select(vec![0usize, 1, 2, 3, INNER_VERSION_OFF, INNER_VARIANT_OFF]),
        bit in 0u8..8,
    ) {
        // Variant byte flips to 0 (Seeded) stay well-formed — Seeded is a valid
        // decode — so only assert on the never-panic guarantee for that byte and
        // require Malformed for the pure structural (magic/version) bytes.
        let full = valid_blob(0, 0, 1, &[]);
        let mut bytes = full.bytes.clone();
        bytes[byte] ^= 1 << bit;
        let env = Environment { blob_version: ADAPTER_BLOB_VERSION, bytes };
        let m = SpecEnvCodec.mutate(&env, 0);
        prop_assert!(matches!(m, Ok(_) | Err(_)));
        if byte != INNER_VARIANT_OFF {
            prop_assert_eq!(m, Err(EnvCodecError::Malformed(ADAPTER_BLOB_VERSION)));
        }
    }
}
