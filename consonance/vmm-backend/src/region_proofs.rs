// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

fn assume_no_wrap(base: u64, len: u64, hole_base: u64, hole_len: u64) {
    kani::assume(base.checked_add(len).is_some());
    kani::assume(hole_base.checked_add(hole_len).is_some());
}

#[kani::proof]
fn split_parts_structural_invariants() {
    let base: u64 = kani::any();
    let len: u64 = kani::any();
    let hole_base: u64 = kani::any();
    let hole_len: u64 = kani::any();
    assume_no_wrap(base, len, hole_base, hole_len);
    let end = base + len;
    let hole_end = hole_base + hole_len;

    let mut prev_end = base;
    for slot in split_parts(base, len, hole_base, hole_len) {
        if let Some(p) = slot {
            assert!(p.size > 0, "non-empty");
            assert!(p.host_off == p.gpa - base, "host offset tracks the gpa");
            assert!(p.gpa >= base, "starts within the region");
            assert!(p.gpa + p.size <= end, "ends within the region");
            assert!(
                p.gpa >= prev_end,
                "ordered + non-overlapping with earlier parts"
            );
            prev_end = p.gpa + p.size;
            assert!(
                hole_len == 0 || p.gpa + p.size <= hole_base || p.gpa >= hole_end,
                "a part never intersects the hole"
            );
        }
    }
}

#[kani::proof]
fn split_parts_pointwise_coverage() {
    let base: u64 = kani::any();
    let len: u64 = kani::any();
    let hole_base: u64 = kani::any();
    let hole_len: u64 = kani::any();
    assume_no_wrap(base, len, hole_base, hole_len);
    let end = base + len;
    let hole_end = hole_base + hole_len;
    let parts = split_parts(base, len, hole_base, hole_len);

    let x: u64 = kani::any();
    kani::assume(base <= x && x < end);
    let in_hole = hole_base <= x && x < hole_end;
    let mut covered = false;
    for slot in parts {
        if let Some(p) = slot
            && p.gpa <= x
            && x < p.gpa + p.size
        {
            covered = true;
        }
    }
    assert!(covered == !in_hole, "covered iff not in the hole");
}

#[kani::proof]
fn split_parts_preserves_page_alignment() {
    let base: u64 = kani::any();
    let len: u64 = kani::any();
    let hole_base: u64 = kani::any();
    let hole_len: u64 = kani::any();
    assume_no_wrap(base, len, hole_base, hole_len);
    kani::assume(base % PAGE == 0);
    kani::assume(len % PAGE == 0);
    kani::assume(hole_base % PAGE == 0);
    kani::assume(hole_len % PAGE == 0);

    for slot in split_parts(base, len, hole_base, hole_len) {
        if let Some(p) = slot {
            assert!(p.gpa % PAGE == 0);
            assert!(p.size % PAGE == 0);
            assert!(p.host_off % PAGE == 0);
        }
    }
}

#[kani::proof]
fn split_parts_disjoint_hole_is_single_region() {
    let base: u64 = kani::any();
    let len: u64 = kani::any();
    let hole_base: u64 = kani::any();
    let hole_len: u64 = kani::any();
    assume_no_wrap(base, len, hole_base, hole_len);
    kani::assume(len > 0);
    let end = base + len;
    let hole_end = hole_base + hole_len;
    kani::assume(hole_end <= base || hole_base >= end);

    let parts = split_parts(base, len, hole_base, hole_len);
    assert!(
        parts[0]
            == Some(MemSlotPart {
                gpa: base,
                size: len,
                host_off: 0,
            })
    );
    assert!(parts[1].is_none());
}
