// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 3 — strict decode / fuzz robustness. Every malformed blob yields the
//! matching `VmStateError`, never a panic.

mod common;

use common::{config, fully_populated};
use proptest::prelude::*;
use vm_state::{
    ARCH_X86_64, VM_STATE_LEGACY_VERSION, VM_STATE_MAGIC, VM_STATE_VERSION, VmState, VmStateError,
};

/// magic:u32 + version:u16 + arch:u16 + section_count:u16 (10 bytes; the v2
/// arch tag did not add padding).
const HEADER_LEN: usize = 10;

/// Split a valid blob into its `(section_count_field, [(tag, payload)])`.
fn split(blob: &[u8]) -> (u16, Vec<(u16, Vec<u8>)>) {
    let count = u16::from_le_bytes([blob[8], blob[9]]);
    let mut secs = Vec::new();
    let mut pos = HEADER_LEN;
    while pos < blob.len() {
        let tag = u16::from_le_bytes([blob[pos], blob[pos + 1]]);
        let len = u32::from_le_bytes([blob[pos + 2], blob[pos + 3], blob[pos + 4], blob[pos + 5]])
            as usize;
        pos += 6;
        secs.push((tag, blob[pos..pos + len].to_vec()));
        pos += len;
    }
    (count, secs)
}

/// Re-pack a header with the given section count and section list.
fn pack(count: u16, secs: &[(u16, Vec<u8>)]) -> Vec<u8> {
    pack_version(VM_STATE_LEGACY_VERSION, count, secs)
}

/// Re-pack a header with an explicit version, section count, and section list.
fn pack_version(version: u16, count: u16, secs: &[(u16, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&VM_STATE_MAGIC.to_le_bytes());
    out.extend_from_slice(&version.to_le_bytes());
    out.extend_from_slice(&ARCH_X86_64.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    for (tag, payload) in secs {
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(payload);
    }
    out
}

fn valid() -> Vec<u8> {
    fully_populated().encode().unwrap()
}

fn valid_v4(engine_state: &[u8]) -> Vec<u8> {
    assert!(!engine_state.is_empty());
    let mut state = fully_populated();
    state.engine_state = engine_state.to_vec();
    state.encode().unwrap()
}

#[test]
fn bad_magic() {
    let mut blob = valid();
    blob[0] ^= 0xff;
    let magic = u32::from_le_bytes([blob[0], blob[1], blob[2], blob[3]]);
    assert_eq!(VmState::decode(&blob), Err(VmStateError::BadMagic(magic)));
}

#[test]
fn wrong_version() {
    let mut blob = valid();
    let bumped = VM_STATE_VERSION + 1;
    blob[4..6].copy_from_slice(&bumped.to_le_bytes());
    assert_eq!(
        VmState::decode(&blob),
        Err(VmStateError::UnsupportedVersion(bumped))
    );
}

#[test]
fn legacy_v3_decodes_with_empty_engine_state() {
    let blob = valid();
    assert_eq!(
        u16::from_le_bytes(blob[4..6].try_into().unwrap()),
        VM_STATE_LEGACY_VERSION
    );
    assert_eq!(u16::from_le_bytes(blob[8..10].try_into().unwrap()), 13);
    assert!(VmState::decode(&blob).unwrap().engine_state.is_empty());
}

#[test]
fn v4_engine_state_round_trips_multiple_payloads() {
    for payload in [vec![0x01], vec![0x00, 0xFE, 0xA5], (0u8..=255).collect()] {
        let blob = valid_v4(&payload);
        assert_eq!(
            u16::from_le_bytes(blob[4..6].try_into().unwrap()),
            VM_STATE_VERSION
        );
        assert_eq!(
            u16::from_le_bytes(blob[8..10].try_into().unwrap()),
            14,
            "v4 adds exactly one trailing section"
        );
        let decoded = VmState::decode(&blob).unwrap();
        assert_eq!(decoded.engine_state, payload);
        assert_eq!(decoded.encode().unwrap(), blob);
    }
}

#[test]
fn v4_engine_state_section_is_required_nonempty_and_last() {
    let good = valid_v4(&[1, 2, 3]);
    let (count, sections) = split(&good);
    assert_eq!(count, 14);
    assert_eq!(sections.last().map(|(tag, _)| *tag), Some(14));

    let mut empty = sections.clone();
    empty.last_mut().unwrap().1.clear();
    assert_eq!(
        VmState::decode(&pack_version(VM_STATE_VERSION, count, &empty)),
        Err(VmStateError::InvalidField)
    );

    let mut missing = sections.clone();
    missing.pop();
    assert_eq!(
        VmState::decode(&pack_version(VM_STATE_VERSION, count - 1, &missing)),
        Err(VmStateError::MissingSection(14))
    );

    let mut duplicate = sections.clone();
    duplicate.push(sections.last().unwrap().clone());
    assert_eq!(
        VmState::decode(&pack_version(VM_STATE_VERSION, count + 1, &duplicate)),
        Err(VmStateError::DuplicateTag(14))
    );

    let mut out_of_order = sections;
    let last = out_of_order.len() - 1;
    out_of_order.swap(last - 1, last);
    assert_eq!(
        VmState::decode(&pack_version(VM_STATE_VERSION, count, &out_of_order)),
        Err(VmStateError::SectionOrder(13))
    );

    let mut trailing = good;
    trailing.push(0);
    assert_eq!(VmState::decode(&trailing), Err(VmStateError::TrailingBytes));
}

#[test]
fn legacy_v3_rejects_a_well_formed_engine_state_section() {
    let (legacy_count, legacy_sections) = split(&valid());
    assert_eq!(legacy_count, 13);
    assert_eq!(legacy_sections.len(), usize::from(legacy_count));

    // TLV 14 belongs only to v4. Both an empty payload and an opaque nonempty
    // payload are otherwise well-formed sections, so the v3 reader must reject
    // them instead of accepting and discarding the field.
    for payload in [Vec::new(), vec![0xA5]] {
        let mut sections = legacy_sections.clone();
        sections.push((14, payload));
        assert_eq!(sections.len(), 14);
        let blob = pack_version(VM_STATE_LEGACY_VERSION, 14, &sections);
        assert_eq!(
            VmState::decode(&blob),
            Err(VmStateError::UnknownTag(14)),
            "v3 must reject a planted engine TLV regardless of payload length"
        );
    }
}

/// The v2 arch tag is a **hard gate on the record set**: a blob whose sections are
/// byte-perfect but whose arch tag is another architecture's is REJECTED, never
/// decoded into this build's x86 fields (`docs/ARCHITECTURE.md` — versioned
/// wire evolution, never silent reinterpretation).
#[test]
fn foreign_arch_tag_is_rejected_not_reinterpreted() {
    let mut blob = valid();
    // Sanity: it decodes under its own tag.
    assert!(VmState::decode(&blob).is_ok());
    let foreign = ARCH_X86_64 + 1;
    blob[6..8].copy_from_slice(&foreign.to_le_bytes());
    assert_eq!(
        VmState::decode(&blob),
        Err(VmStateError::UnsupportedArch(foreign)),
        "a foreign arch tag must fail closed — the records are not ours to read"
    );
}

#[test]
fn truncated_header() {
    let blob = valid();
    for n in 0..HEADER_LEN {
        assert_eq!(VmState::decode(&blob[..n]), Err(VmStateError::Truncated));
    }
}

#[test]
fn truncated_body() {
    let blob = valid();
    // Drop the final byte: the last section's len now claims more than remains.
    assert_eq!(
        VmState::decode(&blob[..blob.len() - 1]),
        Err(VmStateError::Truncated)
    );
}

#[test]
fn trailing_bytes() {
    let mut blob = valid();
    blob.push(0x00);
    assert_eq!(VmState::decode(&blob), Err(VmStateError::TrailingBytes));
}

#[test]
fn duplicate_tag() {
    let (count, secs) = split(&valid());
    // Insert a second copy of the first section right after it: tags 1,1,2,...
    let mut dup = secs.clone();
    dup.insert(1, secs[0].clone());
    let blob = pack(count + 1, &dup);
    assert_eq!(
        VmState::decode(&blob),
        Err(VmStateError::DuplicateTag(secs[0].0))
    );
}

#[test]
fn out_of_order_tags() {
    let (count, secs) = split(&valid());
    let mut swapped = secs.clone();
    swapped.swap(0, 1); // tags 2,1,3,... — the 1 is now out of order
    let blob = pack(count, &swapped);
    assert_eq!(
        VmState::decode(&blob),
        Err(VmStateError::SectionOrder(secs[0].0))
    );
}

#[test]
fn tag_ordering_boundary() {
    // Pin the exact `tag <= prev` boundary in decode's section-ordering check so
    // neither the `<=` guard nor the `==` split has an untested (equivalent)
    // mutant — `< vs <=` is only observable right AT equality:
    //   tag == prev → DuplicateTag  (the boundary; `<=` accepts it, a `<` mutant
    //                                would let the duplicate through)
    //   tag <  prev → SectionOrder  (distinguishes the inner `==`)
    //   tag >  prev → accepted      (strictly ascending)
    let (count, secs) = split(&valid());

    // tag == prev: duplicate the first section adjacently (tags 1,1,2,...).
    let mut equal = secs.clone();
    equal.insert(1, secs[0].clone());
    assert_eq!(
        VmState::decode(&pack(count + 1, &equal)),
        Err(VmStateError::DuplicateTag(secs[0].0)),
    );

    // tag < prev: swap the first two sections (tags 2,1,3,...).
    let mut less = secs.clone();
    less.swap(0, 1);
    assert_eq!(
        VmState::decode(&pack(count, &less)),
        Err(VmStateError::SectionOrder(secs[0].0)),
    );

    // tag > prev (strictly ascending): the untouched valid blob decodes.
    assert!(VmState::decode(&valid()).is_ok());
}

#[test]
fn dropped_required_section() {
    let (count, secs) = split(&valid());
    // Drop the V-time section (tag 9) and decrement the count so the loop reads
    // a clean, in-order set that is simply missing one required tag.
    let dropped_tag = 9;
    let kept: Vec<(u16, Vec<u8>)> = secs
        .iter()
        .filter(|(t, _)| *t != dropped_tag)
        .cloned()
        .collect();
    assert_eq!(kept.len(), secs.len() - 1);
    let blob = pack(count - 1, &kept);
    assert_eq!(
        VmState::decode(&blob),
        Err(VmStateError::MissingSection(dropped_tag))
    );
}

#[test]
fn section_count_zero_is_missing_section() {
    // A header-only blob: count 0, no sections. The first required tag is absent.
    let blob = pack(0, &[]);
    assert_eq!(VmState::decode(&blob), Err(VmStateError::MissingSection(1)));
}

#[test]
fn oversized_section_len() {
    let mut blob = valid();
    // The first section's len field lives at offset 8+2..8+6. Make it enormous.
    blob[10..14].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(VmState::decode(&blob), Err(VmStateError::Truncated));
}

#[test]
fn unknown_tag() {
    let (count, mut secs) = split(&valid());
    // Append a section with a tag past the v1 set; bump count so it is read.
    secs.push((9999, vec![0xde, 0xad]));
    let blob = pack(count + 1, &secs);
    assert_eq!(VmState::decode(&blob), Err(VmStateError::UnknownTag(9999)));
}

#[test]
fn bad_mp_state_byte() {
    let (count, mut secs) = split(&valid());
    // MP-state is tag 6; force an out-of-range byte.
    for (tag, payload) in &mut secs {
        if *tag == 6 {
            *payload = vec![0x07];
        }
    }
    let blob = pack(count, &secs);
    assert_eq!(VmState::decode(&blob), Err(VmStateError::InvalidField));
}

proptest! {
    #![proptest_config(config(1024))]

    /// Arbitrary bytes never panic decode or peek_version.
    #[test]
    fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        let _ = VmState::decode(&bytes);
        let _ = VmState::peek_version(&bytes);
    }

    /// A single-byte mutation of a valid blob never panics, and never silently
    /// decodes to a state that re-encodes to *different* bytes (decode only ever
    /// accepts canonical blobs).
    #[test]
    fn mutated_valid_blob_never_panics(
        idx in any::<prop::sample::Index>(),
        xor in 1u8..=255,
    ) {
        let mut blob = valid();
        let i = idx.index(blob.len());
        blob[i] ^= xor;
        if let Ok(state) = VmState::decode(&blob)
            && let Ok(reencoded) = state.encode()
        {
            prop_assert_eq!(reencoded, blob);
        }
    }
}
