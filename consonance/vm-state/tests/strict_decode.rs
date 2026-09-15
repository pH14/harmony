// SPDX-License-Identifier: AGPL-3.0-or-later

mod common;

use common::{config, fully_populated};
use proptest::prelude::*;
use vm_state::{
    ARCH_X86_64, VM_STATE_LEGACY_VERSION, VM_STATE_MAGIC, VM_STATE_VERSION, VmState, VmStateError,
};

const HEADER_LEN: usize = 10;
const ENGINE_VERSION: u16 = 4;
const XSAVE_RESTORE_BV_TAG: u16 = 15;

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

fn pack(count: u16, secs: &[(u16, Vec<u8>)]) -> Vec<u8> {
    pack_version(VM_STATE_LEGACY_VERSION, count, secs)
}

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

fn valid_v5(engine_state: &[u8]) -> Vec<u8> {
    let mut state = fully_populated();
    state.sregs.flags = 0x0102_0304_0506_0708;
    state.sregs.pdptrs = [0x10, 0x20, 0x30, 0x40];
    state.debugregs.flags = 0x090a_0b0c_0d0e_0f10;
    state.engine_state = engine_state.to_vec();
    state.encode().unwrap()
}

fn valid_v6(engine_state: &[u8], restore_bv: u64) -> Vec<u8> {
    let mut state = fully_populated();
    state.xsave_restore_bv = Some(restore_bv);
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
    let decoded = VmState::decode(&blob).unwrap();
    assert!(decoded.engine_state.is_empty());
    assert_eq!(decoded.sregs.flags, 0);
    assert_eq!(decoded.sregs.pdptrs, [0; 4]);
    assert_eq!(decoded.debugregs.flags, 0);
}

#[test]
fn v4_engine_state_round_trips_multiple_payloads() {
    for payload in [vec![0x01], vec![0x00, 0xFE, 0xA5], (0u8..=255).collect()] {
        let blob = valid_v4(&payload);
        assert_eq!(
            u16::from_le_bytes(blob[4..6].try_into().unwrap()),
            ENGINE_VERSION
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
        VmState::decode(&pack_version(ENGINE_VERSION, count, &empty)),
        Err(VmStateError::InvalidField)
    );

    let mut missing = sections.clone();
    missing.pop();
    assert_eq!(
        VmState::decode(&pack_version(ENGINE_VERSION, count - 1, &missing)),
        Err(VmStateError::MissingSection(14))
    );

    let mut duplicate = sections.clone();
    duplicate.push(sections.last().unwrap().clone());
    assert_eq!(
        VmState::decode(&pack_version(ENGINE_VERSION, count + 1, &duplicate)),
        Err(VmStateError::DuplicateTag(14))
    );

    let mut out_of_order = sections;
    let last = out_of_order.len() - 1;
    out_of_order.swap(last - 1, last);
    assert_eq!(
        VmState::decode(&pack_version(ENGINE_VERSION, count, &out_of_order)),
        Err(VmStateError::SectionOrder(13))
    );

    let mut trailing = good;
    trailing.push(0);
    assert_eq!(VmState::decode(&trailing), Err(VmStateError::TrailingBytes));
}

#[test]
fn v5_extended_cpu_fields_round_trip_with_optional_engine_state() {
    for (engine_state, section_count) in [(&[][..], 13), (&[0xCA, 0xFE][..], 14)] {
        let blob = valid_v5(engine_state);
        assert_eq!(u16::from_le_bytes(blob[4..6].try_into().unwrap()), 5);
        assert_eq!(
            u16::from_le_bytes(blob[8..10].try_into().unwrap()),
            section_count
        );
        let decoded = VmState::decode(&blob).unwrap();
        assert_eq!(decoded.sregs.flags, 0x0102_0304_0506_0708);
        assert_eq!(decoded.sregs.pdptrs, [0x10, 0x20, 0x30, 0x40]);
        assert_eq!(decoded.debugregs.flags, 0x090a_0b0c_0d0e_0f10);
        assert_eq!(decoded.engine_state, engine_state);
        assert_eq!(decoded.encode().unwrap(), blob);
    }
}

#[test]
fn v6_restore_bits_round_trip_with_optional_engine_state() {
    for (engine_state, section_count, restore_bv) in [
        (&[][..], 14, 0),
        (&[0xCA, 0xFE][..], 15, 0x0102_0304_0506_0708),
    ] {
        let blob = valid_v6(engine_state, restore_bv);
        assert_eq!(
            u16::from_le_bytes(blob[4..6].try_into().unwrap()),
            VM_STATE_VERSION
        );
        assert_eq!(
            u16::from_le_bytes(blob[8..10].try_into().unwrap()),
            section_count
        );
        let (count, sections) = split(&blob);
        assert_eq!(
            sections.last().map(|(tag, _)| *tag),
            Some(XSAVE_RESTORE_BV_TAG)
        );
        assert_eq!(sections.last().unwrap().1.len(), 8);
        let decoded = VmState::decode(&blob).unwrap();
        assert_eq!(decoded.xsave_restore_bv, Some(restore_bv));
        assert_eq!(decoded.engine_state, engine_state);
        assert_eq!(decoded.encode().unwrap(), blob);
        assert_eq!(count, section_count);
    }
}

#[test]
fn v6_restore_bits_section_is_required_exact_and_canonical() {
    let good = valid_v6(&[1, 2, 3], 0x1112_1314_1516_1718);
    let (count, sections) = split(&good);
    assert_eq!(count, 15);
    assert_eq!(
        sections.last().map(|(tag, _)| *tag),
        Some(XSAVE_RESTORE_BV_TAG)
    );

    let mut missing = sections.clone();
    missing.pop();
    assert_eq!(
        VmState::decode(&pack_version(VM_STATE_VERSION, count - 1, &missing)),
        Err(VmStateError::MissingSection(XSAVE_RESTORE_BV_TAG))
    );

    for payload_len in [0, 7, 9] {
        let mut malformed = sections.clone();
        malformed.last_mut().unwrap().1.resize(payload_len, 0);
        assert_eq!(
            VmState::decode(&pack_version(VM_STATE_VERSION, count, &malformed)),
            Err(VmStateError::InvalidField),
            "tag 15 length {payload_len} must be rejected"
        );
    }

    let mut duplicate = sections.clone();
    duplicate.push(sections.last().unwrap().clone());
    assert_eq!(
        VmState::decode(&pack_version(VM_STATE_VERSION, count + 1, &duplicate)),
        Err(VmStateError::DuplicateTag(XSAVE_RESTORE_BV_TAG))
    );

    let mut out_of_order = sections.clone();
    let last = out_of_order.len() - 1;
    out_of_order.swap(last - 1, last);
    assert_eq!(
        VmState::decode(&pack_version(VM_STATE_VERSION, count, &out_of_order)),
        Err(VmStateError::SectionOrder(14))
    );

    let mut empty_engine = sections.clone();
    empty_engine
        .iter_mut()
        .find(|(tag, _)| *tag == 14)
        .unwrap()
        .1
        .clear();
    assert_eq!(
        VmState::decode(&pack_version(VM_STATE_VERSION, count, &empty_engine)),
        Err(VmStateError::InvalidField)
    );

    let v5 = valid_v5(&[1, 2, 3]);
    let (v5_count, mut v5_sections) = split(&v5);
    v5_sections.push((XSAVE_RESTORE_BV_TAG, vec![0; 8]));
    assert_eq!(
        VmState::decode(&pack_version(5, v5_count + 1, &v5_sections)),
        Err(VmStateError::UnknownTag(XSAVE_RESTORE_BV_TAG))
    );
}

#[test]
fn v6_truncations_never_panic() {
    let blob = valid_v6(&[1, 2, 3], 0x0102_0304_0506_0708);
    for n in 0..blob.len() {
        assert!(
            VmState::decode(&blob[..n]).is_err(),
            "proper prefix of {}/{} bytes unexpectedly decoded",
            n,
            blob.len()
        );
    }
}

#[test]
fn each_extended_cpu_field_selects_v5() {
    for field in 0..3 {
        let mut state = fully_populated();
        match field {
            0 => state.sregs.flags = 1,
            1 => state.sregs.pdptrs[2] = 2,
            2 => state.debugregs.flags = 3,
            _ => unreachable!(),
        }
        let blob = state.encode().unwrap();
        assert_eq!(u16::from_le_bytes(blob[4..6].try_into().unwrap()), 5);
        assert_eq!(VmState::decode(&blob).unwrap(), state);
    }
}

#[test]
fn v5_engine_state_is_optional_but_nonempty_when_present() {
    let good = valid_v5(&[1, 2, 3]);
    let (count, sections) = split(&good);
    assert_eq!(count, 14);

    let mut missing = sections.clone();
    missing.pop();
    assert_eq!(
        VmState::decode(&pack_version(5, count - 1, &missing))
            .unwrap()
            .engine_state,
        Vec::<u8>::new()
    );

    let mut empty = sections.clone();
    empty.last_mut().unwrap().1.clear();
    assert_eq!(
        VmState::decode(&pack_version(5, count, &empty)),
        Err(VmStateError::InvalidField)
    );

    let mut duplicate = sections.clone();
    duplicate.push(sections.last().unwrap().clone());
    assert_eq!(
        VmState::decode(&pack_version(5, count + 1, &duplicate)),
        Err(VmStateError::DuplicateTag(14))
    );

    let mut out_of_order = sections;
    let last = out_of_order.len() - 1;
    out_of_order.swap(last - 1, last);
    assert_eq!(
        VmState::decode(&pack_version(5, count, &out_of_order)),
        Err(VmStateError::SectionOrder(13))
    );
}

#[test]
fn x86_version_selection_keeps_zero_extension_bytes_legacy_compatible() {
    let v3 = fully_populated().encode().unwrap();
    assert_eq!(u16::from_le_bytes(v3[4..6].try_into().unwrap()), 3);
    assert_eq!(v3.len(), fully_populated().encode().unwrap().len());

    let mut v4_state = fully_populated();
    v4_state.engine_state = vec![1, 2, 3];
    let v4 = v4_state.encode().unwrap();
    assert_eq!(
        u16::from_le_bytes(v4[4..6].try_into().unwrap()),
        ENGINE_VERSION
    );
    assert_eq!(u16::from_le_bytes(v4[8..10].try_into().unwrap()), 14);
    let decoded = VmState::decode(&v4).unwrap();
    assert_eq!(decoded.sregs.flags, 0);
    assert_eq!(decoded.sregs.pdptrs, [0; 4]);
    assert_eq!(decoded.debugregs.flags, 0);
}

#[test]
fn v5_rejects_zero_extended_fields_and_fixed_length_mismatches() {
    let good = valid_v5(&[]);
    let (count, sections) = split(&good);
    assert_eq!(count, 13);

    let mut zero_extension = sections.clone();
    for (tag, payload) in &mut zero_extension {
        match *tag {
            2 => {
                let start = payload.len() - 40;
                payload[start..].fill(0);
            }
            4 => {
                let start = payload.len() - 8;
                payload[start..].fill(0);
            }
            _ => {}
        }
    }
    assert_eq!(
        VmState::decode(&pack_version(5, count, &zero_extension)),
        Err(VmStateError::InvalidField)
    );

    for tag in [2, 4] {
        let mut truncated = sections.clone();
        truncated
            .iter_mut()
            .find(|(section_tag, _)| *section_tag == tag)
            .unwrap()
            .1
            .pop();
        assert_eq!(
            VmState::decode(&pack_version(5, count, &truncated)),
            Err(VmStateError::InvalidField),
            "v5 section {tag} truncation"
        );

        let mut extended = sections.clone();
        extended
            .iter_mut()
            .find(|(section_tag, _)| *section_tag == tag)
            .unwrap()
            .1
            .push(0);
        assert_eq!(
            VmState::decode(&pack_version(5, count, &extended)),
            Err(VmStateError::InvalidField),
            "v5 section {tag} extension"
        );
    }
}

#[test]
fn v5_truncations_never_panic() {
    let blob = valid_v5(&[1, 2, 3]);
    for n in 0..blob.len() {
        let _ = VmState::decode(&blob[..n]);
    }
}

#[test]
fn legacy_v3_rejects_a_well_formed_engine_state_section() {
    let (legacy_count, legacy_sections) = split(&valid());
    assert_eq!(legacy_count, 13);
    assert_eq!(legacy_sections.len(), usize::from(legacy_count));

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

#[test]
fn foreign_arch_tag_is_rejected_not_reinterpreted() {
    let mut blob = valid();
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
    swapped.swap(0, 1);
    let blob = pack(count, &swapped);
    assert_eq!(
        VmState::decode(&blob),
        Err(VmStateError::SectionOrder(secs[0].0))
    );
}

#[test]
fn tag_ordering_boundary() {
    let (count, secs) = split(&valid());

    let mut equal = secs.clone();
    equal.insert(1, secs[0].clone());
    assert_eq!(
        VmState::decode(&pack(count + 1, &equal)),
        Err(VmStateError::DuplicateTag(secs[0].0)),
    );

    let mut less = secs.clone();
    less.swap(0, 1);
    assert_eq!(
        VmState::decode(&pack(count, &less)),
        Err(VmStateError::SectionOrder(secs[0].0)),
    );

    assert!(VmState::decode(&valid()).is_ok());
}

#[test]
fn dropped_required_section() {
    let (count, secs) = split(&valid());
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
    let blob = pack(0, &[]);
    assert_eq!(VmState::decode(&blob), Err(VmStateError::MissingSection(1)));
}

#[test]
fn oversized_section_len() {
    let mut blob = valid();
    blob[10..14].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(VmState::decode(&blob), Err(VmStateError::Truncated));
}

#[test]
fn unknown_tag() {
    let (count, mut secs) = split(&valid());
    secs.push((9999, vec![0xde, 0xad]));
    let blob = pack(count + 1, &secs);
    assert_eq!(VmState::decode(&blob), Err(VmStateError::UnknownTag(9999)));
}

#[test]
fn bad_mp_state_byte() {
    let (count, mut secs) = split(&valid());
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

    #[test]
    fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        let _ = VmState::decode(&bytes);
        let _ = VmState::peek_version(&bytes);
    }

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
