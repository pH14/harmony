// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 3 — strict decode / fuzz robustness. Every malformed blob yields the
//! matching `VmStateError`, never a panic.

mod common;

use common::{config, fully_populated};
use proptest::prelude::*;
use vm_state::{VM_STATE_MAGIC, VM_STATE_VERSION, VmState, VmStateError};

const HEADER_LEN: usize = 8;

/// Split a valid blob into its `(section_count_field, [(tag, payload)])`.
fn split(blob: &[u8]) -> (u16, Vec<(u16, Vec<u8>)>) {
    let count = u16::from_le_bytes([blob[6], blob[7]]);
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
    let mut out = Vec::new();
    out.extend_from_slice(&VM_STATE_MAGIC.to_le_bytes());
    out.extend_from_slice(&VM_STATE_VERSION.to_le_bytes());
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
    /// accepts canonical blobs). Re-encoding may itself fail — a mutation can flip
    /// `ratio_den` away from 1, which `encode` rejects — so that case is skipped.
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
