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

use environment::{Action, DecisionClass, EnvSpec, FaultPolicy, HostFault, StandingFault, VTime};
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

/// A known-good blob to pair a hostile one against, so `compose`'s **second**
/// operand decode is reached: with a valid base, `require(base)?` succeeds and
/// execution proceeds to decode the (hostile) tail. A round trip proves it is
/// genuinely well-formed.
fn good_partner() -> Environment {
    let g = valid_blob(0, 100, 7, &[10, 40]);
    assert!(
        AdapterEnv::decode(&g).is_ok(),
        "partner must be well-formed"
    );
    g
}

/// Assert a hostile blob is rejected as `Malformed(version)` on **every** public
/// decode entry point — including both `compose` operands independently, since
/// `require(base)?` short-circuits before the tail is decoded. Passing the
/// hostile blob only as `compose(env, env)` would leave the tail-decode path
/// with zero coverage (round-1 blocking finding), so this drives it as the base
/// (`compose(env, good)`) **and** as the tail (`compose(good, env)`) separately.
fn assert_malformed_everywhere(env: &Environment, version: u16, ctx: &str) {
    let want = Err(EnvCodecError::Malformed(version));
    let good = good_partner();
    assert_eq!(SpecEnvCodec.mutate(env, 0xA11CE), want, "mutate: {ctx}");
    assert_eq!(
        SpecEnvCodec.compose(env, &good),
        want,
        "compose malformed base: {ctx}"
    );
    assert_eq!(
        SpecEnvCodec.compose(&good, env),
        want,
        "compose malformed tail (second operand): {ctx}"
    );
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
        assert_malformed_everywhere(&env, ADAPTER_BLOB_VERSION, &format!("truncated to {cut}"));
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
            assert_malformed_everywhere(
                &env,
                ADAPTER_BLOB_VERSION,
                &format!("magic byte {byte} bit {bit} flip"),
            );
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
        assert_malformed_everywhere(&env, v, &format!("declared version {v}"));
    }
    // A header-internal version byte flip (the declared version stays 1) is
    // caught by the in-body magic/version check.
    let mut bytes = full.bytes.clone();
    bytes[4] ^= 0x01;
    let env = Environment {
        blob_version: ADAPTER_BLOB_VERSION,
        bytes,
    };
    assert_malformed_everywhere(&env, ADAPTER_BLOB_VERSION, "header version byte flip");
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
    assert_malformed_everywhere(&env, ADAPTER_BLOB_VERSION, "inner spec version skew");
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
    assert_malformed_everywhere(&env, ADAPTER_BLOB_VERSION, "policy length overflow");
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
        assert_malformed_everywhere(&env, ADAPTER_BLOB_VERSION, &format!("variant tag {tag}"));
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

/// A base captured behind its own root (`pos < base_offset`) is the per-operand
/// `MisorderedChain` invariant, on both `mutate` and `compose`.
#[test]
fn misordered_chain_is_typed() {
    // mutate: capture pos 100 behind root 200.
    let bad = valid_blob(200, 100, 7, &[]);
    assert!(matches!(
        SpecEnvCodec.mutate(&bad, 1),
        Err(EnvCodecError::MisorderedChain(_))
    ));
    // compose: the same malformed operand as the base (require catches it
    // before the pair-adjacency check).
    let good = good_partner();
    assert!(matches!(
        SpecEnvCodec.compose(&bad, &good),
        Err(EnvCodecError::MisorderedChain(_))
    ));
}

/// The pair-adjacency invariant: the branch-local delta must begin exactly where
/// the base was captured (`d.base_offset == b.pos`). A gap or overlap between two
/// individually-well-formed operands is `NonAdjacentChain` (task 99, round 4).
#[test]
fn non_adjacent_chain_is_typed() {
    let base = valid_blob(0, 300, 7, &[]);
    // Gap: delta branched past the base's capture point.
    let gap = valid_blob(400, 500, 7, &[]);
    assert!(matches!(
        SpecEnvCodec.compose(&base, &gap),
        Err(EnvCodecError::NonAdjacentChain(_))
    ));
    // Overlap: delta branched before the base's capture point.
    let overlap = valid_blob(150, 350, 7, &[]);
    assert!(matches!(
        SpecEnvCodec.compose(&base, &overlap),
        Err(EnvCodecError::NonAdjacentChain(_))
    ));
    // The adjacent pair composes.
    let adjacent = valid_blob(300, 450, 7, &[]);
    assert!(SpecEnvCodec.compose(&base, &adjacent).is_ok());
}

/// The delegated spec-content invariants (contract point 4): even a positionally
/// valid, adjacent pair fails closed with `UnsupportedComposition` when the wire
/// codec cannot splice the specs — a `Seeded` operand, or a standing-fault
/// carrier. (Seed/policy mismatch is `unsupported_composition_is_typed`; these
/// complete the enumeration of the delegated cases.)
#[test]
fn spec_content_incompatibility_is_typed() {
    // Adjacent positions (base captured at 100, delta branched at 100), so the
    // rejection is purely spec-content, not positional.
    let seeded_spec = EnvSpec::Seeded {
        seed: 7,
        policy: FaultPolicy::none(),
    };
    let seeded_base = AdapterEnv {
        base_offset: 0,
        pos: 100,
        spec: seeded_spec,
    }
    .encode();
    let recorded_delta = valid_blob(100, 200, 7, &[]);
    assert_eq!(
        SpecEnvCodec.compose(&seeded_base, &recorded_delta),
        Err(EnvCodecError::UnsupportedComposition),
        "a Seeded operand cannot be spliced"
    );

    // A standing-fault carrier (a different axis than the Moment offset).
    let standing_spec = EnvSpec::Recorded {
        seed: 7,
        policy: FaultPolicy::none(),
        overrides: std::collections::BTreeMap::new(),
        standing: vec![StandingFault {
            class: DecisionClass::Entropy,
            target: Vec::new(),
            window: (VTime(0), VTime(10)),
        }],
        reseeds: std::collections::BTreeMap::new(),
    };
    let standing_delta = AdapterEnv {
        base_offset: 100,
        pos: 200,
        spec: standing_spec,
    }
    .encode();
    let good_base = valid_blob(0, 100, 7, &[]);
    assert_eq!(
        SpecEnvCodec.compose(&good_base, &standing_delta),
        Err(EnvCodecError::UnsupportedComposition),
        "a standing-fault carrier cannot be spliced on the Moment axis"
    );
}

/// A byte-valid blob whose **own** capture precedes its root (`pos < base_offset`)
/// is a semantically impossible lineage — a snapshot taken before the branch it
/// is keyed from. It must be `MisorderedChain` on every entry point, and in
/// particular `compose` must reject it whether it is the base **or** the tail
/// operand: `require(base)?` short-circuits, so the tail's own invariant is only
/// enforced when the base is well-formed (round-3 blocking finding — such an
/// operand previously composed to `Ok` with an inconsistent artifact).
#[test]
fn operand_capture_before_its_own_root_is_typed() {
    let good = good_partner();
    // base_offset = 100 but pos = 50: capture precedes the root.
    let inconsistent = valid_blob(100, 50, 7, &[]);
    // It decodes at the byte level (so `require`, not `decode`, must catch it).
    assert!(
        AdapterEnv::decode(&inconsistent).is_ok(),
        "the blob is byte-valid; only its internal invariant is violated"
    );
    let is_misordered =
        |r: Result<Environment, EnvCodecError>| matches!(r, Err(EnvCodecError::MisorderedChain(_)));
    assert!(
        is_misordered(SpecEnvCodec.mutate(&inconsistent, 1)),
        "mutate"
    );
    assert!(
        is_misordered(SpecEnvCodec.compose(&inconsistent, &good)),
        "compose: inconsistent base"
    );
    assert!(
        is_misordered(SpecEnvCodec.compose(&good, &inconsistent)),
        "compose: inconsistent tail (second operand) — must not mint an Ok artifact"
    );
    // Even a self-consistent pairing with itself is refused (never Ok).
    assert!(
        is_misordered(SpecEnvCodec.compose(&inconsistent, &inconsistent)),
        "compose: both operands inconsistent"
    );
}

/// `compose`'s **second** operand (the branch-local tail) is decoded on its own
/// untrusted path, reached only when the base decodes cleanly — so a valid base
/// with a hostile tail must still be a typed error, never a panic (round-1
/// blocking finding: `require(base)?` short-circuits the whole-surface gate).
/// One direct case per malformed-tail shape, keyed off a known-good base.
#[test]
fn compose_rejects_a_malformed_tail_behind_a_valid_base() {
    let good = good_partner();
    let full = valid_blob(0, 200, 7, &[10, 90]);

    // Truncation of the tail at every boundary.
    for cut in 0..full.bytes.len() {
        let tail = Environment {
            blob_version: ADAPTER_BLOB_VERSION,
            bytes: full.bytes[..cut].to_vec(),
        };
        assert_eq!(
            SpecEnvCodec.compose(&good, &tail),
            Err(EnvCodecError::Malformed(ADAPTER_BLOB_VERSION)),
            "valid base + tail truncated to {cut} must be a typed Malformed"
        );
    }

    // A structural tail corruption at each header/spec anchor, and a skewed
    // declared tail version.
    let corruptions: &[(usize, u8, u16, &str)] = &[
        (0, 0x01, ADAPTER_BLOB_VERSION, "tail magic bit-flip"),
        (
            INNER_VERSION_OFF,
            0xFF,
            ADAPTER_BLOB_VERSION,
            "tail inner-version skew",
        ),
        (
            INNER_VARIANT_OFF,
            0x07,
            ADAPTER_BLOB_VERSION,
            "tail unknown variant",
        ),
    ];
    for &(off, xor, ver, ctx) in corruptions {
        let mut bytes = full.bytes.clone();
        bytes[off] ^= xor;
        let tail = Environment {
            blob_version: ADAPTER_BLOB_VERSION,
            bytes,
        };
        assert_eq!(
            SpecEnvCodec.compose(&good, &tail),
            Err(EnvCodecError::Malformed(ver)),
            "valid base + {ctx}"
        );
    }

    // A skewed declared tail version (payload intact).
    let tail = Environment {
        blob_version: 99,
        bytes: full.bytes.clone(),
    };
    assert_eq!(
        SpecEnvCodec.compose(&good, &tail),
        Err(EnvCodecError::Malformed(99)),
        "valid base + off-version tail"
    );
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

    /// **Completeness proof for the `compose` operand-pair contract** (task 99,
    /// round 4). Over arbitrary positional metadata `(b.base_offset, b.pos,
    /// d.base_offset, d.pos)` — with the spec-content invariants held constant
    /// (same seed, both `Recorded`, no overrides/standing, so no
    /// `UnsupportedComposition`/`Overflow` can fire) — `compose` returns `Ok`
    /// **exactly** when every positional invariant holds, and the precise typed
    /// error otherwise:
    ///   * base ill-formed (`b.pos < b.base_offset`)  → `MisorderedChain`
    ///   * else delta ill-formed (`d.pos < d.base_offset`) → `MisorderedChain`
    ///   * else non-adjacent (`d.base_offset != b.pos`) → `NonAdjacentChain`
    ///   * else → `Ok`
    /// `d.base_offset` is biased to `b.pos` half the time so the `Ok` region is
    /// richly sampled. This is the reviewable artifact: the biconditional, not
    /// any single check.
    #[test]
    fn compose_ok_exactly_on_the_valid_operand_pair(
        b_base in 0u64..200,
        b_pos in 0u64..200,
        d_pos in 0u64..200,
        force_adjacent in any::<bool>(),
        d_base_rand in 0u64..200,
    ) {
        let d_base = if force_adjacent { b_pos } else { d_base_rand };
        // Identical seed/policy, both Recorded, no overrides ⇒ the spec-content
        // invariants always hold and no re-key can overflow, isolating the
        // positional contract.
        let b = valid_blob(b_base, b_pos, 7, &[]);
        let d = valid_blob(d_base, d_pos, 7, &[]);

        let base_wf = b_pos >= b_base;
        let delta_wf = d_pos >= d_base;
        let adjacent = d_base == b_pos;
        let valid = base_wf && delta_wf && adjacent;

        let got = SpecEnvCodec.compose(&b, &d);
        prop_assert_eq!(
            got.is_ok(), valid,
            "b=({},{}) d=({},{}) base_wf={} delta_wf={} adjacent={}",
            b_base, b_pos, d_base, d_pos, base_wf, delta_wf, adjacent
        );
        match got {
            Ok(env) => {
                // The Ok artifact is itself well-formed: rooted at the base's
                // root, captured at the delta's pos, and pos >= base_offset (so
                // it can seed a further compose/mutate).
                let out = AdapterEnv::decode(&env).expect("Ok artifact decodes");
                prop_assert_eq!(out.base_offset, b_base);
                prop_assert_eq!(out.pos, d_pos);
                prop_assert!(out.pos >= out.base_offset);
            }
            Err(e) => {
                if !base_wf || !delta_wf {
                    prop_assert!(
                        matches!(e, EnvCodecError::MisorderedChain(_)),
                        "expected MisorderedChain, got {e:?}"
                    );
                } else {
                    prop_assert!(
                        matches!(e, EnvCodecError::NonAdjacentChain(_)),
                        "expected NonAdjacentChain, got {e:?}"
                    );
                }
            }
        }
    }

    /// The core untrusted-input property: **hostile mutations of a valid blob
    /// never panic** — both public decode-path methods return a typed `Ok`/`Err`,
    /// never abort (conventions rule 4).
    ///
    /// It fuzzes a **valid current-version encoding**, not arbitrary bytes: a
    /// uniform-random blob would fail the outer version guard (~65,535/65,536 of
    /// the time) or the 4-byte magic (almost always) and short-circuit *before*
    /// the header/inner-`EnvSpec`/length-field parser — so a panic deep in the
    /// decoder (an out-of-bounds read on a corrupted length prefix) could hide
    /// behind a green property. Instead: declare the adapter version, keep a
    /// **valid prefix that always covers the full 22-byte wrapper header** (so
    /// the wrapper decodes and `EnvSpec::decode` is entered), retain a random
    /// amount of the valid inner spec, then append hostile bytes. The base is
    /// generated with a variable override/reseed count so the inner spec has the
    /// length-prefixed sections whose parsing is the target. Wrong-version
    /// coverage lives in `any_wrong_version_is_malformed`.
    #[test]
    fn mutations_of_a_valid_encoding_never_panic(
        seed in any::<u64>(),
        keys in prop::collection::vec(0u64..2000, 0..8),
        base_offset in 0u64..2000,
        span in 0u64..2000,
        keep in any::<usize>(),
        tail in prop::collection::vec(any::<u8>(), 0..96),
        salt in any::<u64>(),
    ) {
        // A well-formed base (pos >= base_offset) with the given overrides, so
        // its encoding carries the deep length-prefixed sections.
        let full = valid_blob(base_offset, base_offset + span, seed, &keys).bytes;
        // Retain a valid prefix in [HEADER_LEN, full.len()] — always the whole
        // wrapper header plus a random slice of the inner spec, so the wrapper
        // decodes and the inner parser is always entered — then append garbage.
        // With an empty tail this is truncation at an arbitrary inner boundary;
        // with a non-empty tail it is corruption/extension past that boundary.
        let cut = HEADER_LEN + keep % (full.len() - HEADER_LEN + 1);
        let mut bytes = full[..cut].to_vec();
        bytes.extend_from_slice(&tail);
        let env = Environment { blob_version: ADAPTER_BLOB_VERSION, bytes };
        let good = good_partner();
        // A panic in any call fails the test; assert totality explicitly. The
        // hostile blob is driven as mutate's operand, as compose's base, AND as
        // compose's tail (valid base ⇒ the tail-decode path is reached), so a
        // panic that only fires while decoding the second operand is caught too.
        prop_assert!(matches!(SpecEnvCodec.mutate(&env, salt), Ok(_) | Err(_)));
        prop_assert!(matches!(SpecEnvCodec.compose(&env, &good), Ok(_) | Err(_)));
        prop_assert!(matches!(SpecEnvCodec.compose(&good, &env), Ok(_) | Err(_)));
    }

    /// Complements the above with **fully arbitrary bytes at the correct declared
    /// version**: these pass the version guard and stress the outer magic/length
    /// guards on wild input (they almost never reach the deep parser — that is the
    /// job of `mutations_of_a_valid_encoding_never_panic` — but they must still
    /// never panic).
    #[test]
    fn arbitrary_current_version_bytes_never_panic(
        a in prop::collection::vec(any::<u8>(), 0..300),
        salt in any::<u64>(),
    ) {
        let env = Environment { blob_version: ADAPTER_BLOB_VERSION, bytes: a };
        let good = good_partner();
        prop_assert!(matches!(SpecEnvCodec.mutate(&env, salt), Ok(_) | Err(_)));
        prop_assert!(matches!(SpecEnvCodec.compose(&env, &good), Ok(_) | Err(_)));
        prop_assert!(matches!(SpecEnvCodec.compose(&good, &env), Ok(_) | Err(_)));
    }

    /// Any blob whose declared version is not the adapter version is a typed
    /// `Malformed` carrying that version — regardless of payload.
    #[test]
    fn any_wrong_version_is_malformed(
        v in any::<u16>().prop_filter("off-version", |v| *v != ADAPTER_BLOB_VERSION),
        bytes in prop::collection::vec(any::<u8>(), 0..300),
    ) {
        let env = Environment { blob_version: v, bytes };
        assert_malformed_everywhere(&env, v, "arbitrary off-version blob");
    }

    /// A byte-valid blob whose capture precedes its own root (`pos < base_offset`)
    /// is always `MisorderedChain` — on `mutate` and on **both** `compose`
    /// operands — regardless of the offsets, and is never composed to `Ok`.
    #[test]
    fn operand_capture_before_root_is_misordered(
        base_offset in 1u64..=u64::MAX,
        gap in 1u64..=u64::MAX,
    ) {
        // pos = base_offset - gap (clamped), so pos < base_offset by construction.
        let pos = base_offset.saturating_sub(gap).min(base_offset - 1);
        let bad = valid_blob(base_offset, pos, 7, &[]);
        let good = good_partner();
        let is_misordered =
            |r: Result<Environment, EnvCodecError>| matches!(r, Err(EnvCodecError::MisorderedChain(_)));
        prop_assert!(is_misordered(SpecEnvCodec.mutate(&bad, 0)), "mutate");
        prop_assert!(is_misordered(SpecEnvCodec.compose(&bad, &good)), "base");
        prop_assert!(
            is_misordered(SpecEnvCodec.compose(&good, &bad)),
            "tail (second operand) must never mint an Ok artifact"
        );
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
        assert_malformed_everywhere(&env, ADAPTER_BLOB_VERSION, "arbitrary truncation");
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
        let good = good_partner();
        let mut bytes = full.bytes.clone();
        bytes[byte] ^= 1 << bit;
        let env = Environment { blob_version: ADAPTER_BLOB_VERSION, bytes };
        let m = SpecEnvCodec.mutate(&env, 0);
        let as_base = SpecEnvCodec.compose(&env, &good);
        let as_tail = SpecEnvCodec.compose(&good, &env);
        prop_assert!(matches!(m, Ok(_) | Err(_)));
        prop_assert!(matches!(as_base, Ok(_) | Err(_)));
        prop_assert!(matches!(as_tail, Ok(_) | Err(_)));
        if byte != INNER_VARIANT_OFF {
            let want = Err(EnvCodecError::Malformed(ADAPTER_BLOB_VERSION));
            prop_assert_eq!(m, want.clone());
            prop_assert_eq!(as_base, want.clone());
            prop_assert_eq!(as_tail, want, "malformed tail (second operand)");
        }
    }
}
