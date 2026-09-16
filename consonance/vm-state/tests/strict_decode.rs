// SPDX-License-Identifier: AGPL-3.0-or-later

mod common;

use common::{config, fully_populated};
use proptest::prelude::*;
use vm_state::{ARCH_X86_64, VM_STATE_MAGIC, VM_STATE_VERSION, VmState, VmStateError};

const HEADER_LEN: usize = 10;
const ENGINE_STATE_TAG: u16 = 14;
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
    pack_version(VM_STATE_VERSION, count, secs)
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
    let mut state = fully_populated();
    state.sregs.flags = 0x0102_0304_0506_0708;
    state.sregs.pdptrs = [0x10, 0x20, 0x30, 0x40];
    state.debugregs.flags = 0x090a_0b0c_0d0e_0f10;
    state.engine_state = vec![0xCA, 0xFE];
    state.xsave_restore_bv = Some(0x1112_1314_1516_1718);
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
fn unsupported_versions_are_rejected() {
    let blob = valid();
    for version in [3, 4, 5, VM_STATE_VERSION + 1] {
        let mut unsupported = blob.clone();
        unsupported[4..6].copy_from_slice(&version.to_le_bytes());
        assert_eq!(
            VmState::decode(&unsupported),
            Err(VmStateError::UnsupportedVersion(version))
        );
    }
}

#[test]
fn current_version_round_trips_and_peeks() {
    let blob = valid();
    assert_eq!(
        u16::from_le_bytes(blob[4..6].try_into().unwrap()),
        VM_STATE_VERSION
    );
    assert_eq!(VmState::peek_version(&blob), Ok(VM_STATE_VERSION));
    assert_eq!(VmState::decode(&blob).unwrap().encode().unwrap(), blob);
}

#[test]
fn current_optional_sections_round_trip() {
    for (engine_state, restore_bv, section_count) in [
        (Vec::new(), None, 13),
        (vec![1, 2, 3], None, 14),
        (Vec::new(), Some(0x55), 14),
        (vec![4, 5, 6], Some(0x66), 15),
    ] {
        let mut state = fully_populated();
        state.engine_state = engine_state;
        state.xsave_restore_bv = restore_bv;
        let blob = state.encode().unwrap();
        assert_eq!(split(&blob).0, section_count);
        assert_eq!(VmState::decode(&blob), Ok(state));
        assert_eq!(VmState::decode(&blob).unwrap().encode().unwrap(), blob);
    }
}

#[test]
fn current_engine_state_section_is_nonempty_and_optional() {
    let mut state = fully_populated();
    state.engine_state = vec![1, 2, 3];
    let good = state.encode().unwrap();
    let (count, sections) = split(&good);
    assert_eq!(count, 14);
    assert_eq!(sections.last().map(|(tag, _)| *tag), Some(ENGINE_STATE_TAG));

    let mut empty = sections.clone();
    empty.last_mut().unwrap().1.clear();
    assert_eq!(
        VmState::decode(&pack(count, &empty)),
        Err(VmStateError::InvalidField)
    );

    let mut missing = sections.clone();
    missing.pop();
    let decoded = VmState::decode(&pack(count - 1, &missing)).unwrap();
    assert!(decoded.engine_state.is_empty());

    let mut duplicate = sections.clone();
    duplicate.push(sections.last().unwrap().clone());
    assert_eq!(
        VmState::decode(&pack(count + 1, &duplicate)),
        Err(VmStateError::DuplicateTag(ENGINE_STATE_TAG))
    );

    let mut out_of_order = sections;
    let last = out_of_order.len() - 1;
    out_of_order.swap(last - 1, last);
    assert_eq!(
        VmState::decode(&pack(count, &out_of_order)),
        Err(VmStateError::SectionOrder(13))
    );
}

#[test]
fn current_restore_bits_section_is_optional_and_exact() {
    let good = valid();
    let (count, sections) = split(&good);
    assert_eq!(count, 15);
    assert_eq!(
        sections.last().map(|(tag, _)| *tag),
        Some(XSAVE_RESTORE_BV_TAG)
    );
    assert_eq!(sections.last().unwrap().1.len(), 8);

    let mut missing = sections.clone();
    missing.pop();
    let decoded = VmState::decode(&pack(count - 1, &missing)).unwrap();
    assert_eq!(decoded.xsave_restore_bv, None);
    assert_eq!(decoded.engine_state, vec![0xCA, 0xFE]);

    for payload_len in [0, 7, 9] {
        let mut malformed = sections.clone();
        malformed.last_mut().unwrap().1.resize(payload_len, 0);
        assert_eq!(
            VmState::decode(&pack(count, &malformed)),
            Err(VmStateError::InvalidField),
            "tag 15 length {payload_len} must be rejected"
        );
    }

    let mut duplicate = sections.clone();
    duplicate.push(sections.last().unwrap().clone());
    assert_eq!(
        VmState::decode(&pack(count + 1, &duplicate)),
        Err(VmStateError::DuplicateTag(XSAVE_RESTORE_BV_TAG))
    );

    let mut out_of_order = sections;
    let last = out_of_order.len() - 1;
    out_of_order.swap(last - 1, last);
    assert_eq!(
        VmState::decode(&pack(count, &out_of_order)),
        Err(VmStateError::SectionOrder(ENGINE_STATE_TAG))
    );
}

#[test]
fn current_extended_cpu_fields_and_lengths_are_strict() {
    let good = valid();
    let (count, sections) = split(&good);
    let decoded = VmState::decode(&good).unwrap();
    assert_eq!(decoded.sregs.flags, 0x0102_0304_0506_0708);
    assert_eq!(decoded.sregs.pdptrs, [0x10, 0x20, 0x30, 0x40]);
    assert_eq!(decoded.debugregs.flags, 0x090a_0b0c_0d0e_0f10);

    for tag in [2, 4] {
        let mut truncated = sections.clone();
        truncated
            .iter_mut()
            .find(|(section_tag, _)| *section_tag == tag)
            .unwrap()
            .1
            .pop();
        assert_eq!(
            VmState::decode(&pack(count, &truncated)),
            Err(VmStateError::InvalidField),
            "current section {tag} truncation"
        );

        let mut extended = sections.clone();
        extended
            .iter_mut()
            .find(|(section_tag, _)| *section_tag == tag)
            .unwrap()
            .1
            .push(0);
        assert_eq!(
            VmState::decode(&pack(count, &extended)),
            Err(VmStateError::InvalidField),
            "current section {tag} extension"
        );
    }
}

#[test]
fn truncations_never_panic() {
    let blob = valid();
    for n in 0..blob.len() {
        assert!(VmState::decode(&blob[..n]).is_err());
    }
}

#[test]
fn truncated_header() {
    let blob = valid();
    for n in 0..HEADER_LEN {
        assert_eq!(VmState::decode(&blob[..n]), Err(VmStateError::Truncated));
    }
}

#[test]
fn trailing_bytes() {
    let mut blob = valid();
    blob.push(0x00);
    assert_eq!(VmState::decode(&blob), Err(VmStateError::TrailingBytes));
}

#[test]
fn duplicate_and_out_of_order_tags() {
    let (count, secs) = split(&valid());

    let mut dup = secs.clone();
    dup.insert(1, secs[0].clone());
    assert_eq!(
        VmState::decode(&pack(count + 1, &dup)),
        Err(VmStateError::DuplicateTag(secs[0].0))
    );

    let mut swapped = secs.clone();
    swapped.swap(0, 1);
    assert_eq!(
        VmState::decode(&pack(count, &swapped)),
        Err(VmStateError::SectionOrder(secs[0].0))
    );
}

#[test]
fn dropped_required_section() {
    let (count, secs) = split(&valid());
    let dropped_tag = 9;
    let kept: Vec<(u16, Vec<u8>)> = secs
        .iter()
        .filter(|(tag, _)| *tag != dropped_tag)
        .cloned()
        .collect();
    assert_eq!(
        VmState::decode(&pack(count - 1, &kept)),
        Err(VmStateError::MissingSection(dropped_tag))
    );
}

#[test]
fn section_count_zero_is_missing_section() {
    assert_eq!(
        VmState::decode(&pack(0, &[])),
        Err(VmStateError::MissingSection(1))
    );
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
    assert_eq!(
        VmState::decode(&pack(count + 1, &secs)),
        Err(VmStateError::UnknownTag(9999))
    );
}

#[test]
fn bad_mp_state_byte() {
    let (count, mut secs) = split(&valid());
    for (tag, payload) in &mut secs {
        if *tag == 6 {
            *payload = vec![0x07];
        }
    }
    assert_eq!(
        VmState::decode(&pack(count, &secs)),
        Err(VmStateError::InvalidField)
    );
}

proptest! {
    #![proptest_config(config(1024))]

    #[test]
    fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        let _ = VmState::decode(&bytes);
        let _ = VmState::peek_version(&bytes);
    }

    #[test]
    fn round_trip_current_states(s in common::arb_vm_state()) {
        let bytes = s.encode().expect("a current VmState always encodes");
        prop_assert_eq!(VmState::decode(&bytes).unwrap(), s);
    }
}
