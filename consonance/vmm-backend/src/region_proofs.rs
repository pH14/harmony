// SPDX-License-Identifier: AGPL-3.0-or-later
//! Kani proof harnesses for the portable memslot splitter ([`split_parts`] /
//! [`split_around_hole`], gate 2), split out of `region.rs` so cargo-mutants can
//! glob-exclude them: they are `#[cfg(kani)]` and verified by the dedicated `kani`
//! CI job, not the mutation oracle. Declared as `#[cfg(kani)] #[path =
//! "region_proofs.rs"] mod proofs;` in `region.rs`, so it is a child of `region`
//! (`use super::*` reaches the private `split_parts` / `PAGE`).
//!
//! ## Why these are cheap for CBMC
//!
//! [`split_parts`] is pure interval arithmetic — only `+`, `-`, `min`, `max`,
//! comparisons, and (in the alignment harness) `% PAGE` with `PAGE` a power-of-two
//! constant (a bitmask). None of the symbolic `u128` multiply/divide that forces
//! the `vtime` / `lapic` harnesses to pin operands to concrete representatives, so
//! every harness here runs over **full symbolic `u64`** inputs — under only a
//! no-overflow `assume` (the regime in which the saturating adds are exact) — with
//! no value-range bounding. Harness runtimes are recorded in `IMPLEMENTATION.md`.

use super::*;

/// Assume `base + len` and `hole_base + hole_len` do not wrap — the regime in which
/// [`split_parts`]' saturating adds are exact (real guest RAM plus the 4 KiB LAPIC
/// hole never approach `u64::MAX`). Under it, `base + len` / `hole_base + hole_len`
/// in the harnesses below cannot overflow either.
fn assume_no_wrap(base: u64, len: u64, hole_base: u64, hole_len: u64) {
    kani::assume(base.checked_add(len).is_some());
    kani::assume(hole_base.checked_add(hole_len).is_some());
}

/// Structural invariants for ALL inputs: every emitted part is non-empty, lies
/// within the region, has `host_off == gpa - base`, never intersects the hole, and
/// the parts are ordered + pairwise non-overlapping.
#[kani::proof]
fn split_parts_structural_invariants() {
    let base: u64 = kani::any();
    let len: u64 = kani::any();
    let hole_base: u64 = kani::any();
    let hole_len: u64 = kani::any();
    assume_no_wrap(base, len, hole_base, hole_len);
    let end = base + len;
    let hole_end = hole_base + hole_len;

    let mut prev_end = base; // parts are ordered; each starts at or after the last end
    for slot in split_parts(base, len, hole_base, hole_len) {
        if let Some(p) = slot {
            assert!(p.size > 0, "non-empty");
            assert!(p.host_off == p.gpa - base, "host offset tracks the gpa");
            assert!(p.gpa >= base, "starts within the region");
            assert!(p.gpa + p.size <= end, "ends within the region"); // no wrap: <= end
            assert!(
                p.gpa >= prev_end,
                "ordered + non-overlapping with earlier parts"
            );
            prev_end = p.gpa + p.size;
            // Disjoint from the hole (an empty hole carves nothing, so the single
            // full region trivially does not intersect it).
            assert!(
                hole_len == 0 || p.gpa + p.size <= hole_base || p.gpa >= hole_end,
                "a part never intersects the hole"
            );
        }
    }
}

/// The union property in pointwise form, for ALL inputs: a byte of the region is
/// covered by exactly one part iff it is NOT in the hole. Together with
/// [`split_parts_structural_invariants`] (parts disjoint and hole-free) this is
/// "the union equals `[base, base+len)` minus `[hole, hole+hole_len)`".
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
    kani::assume(base <= x && x < end); // an arbitrary byte inside the region
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

/// Page-aligned inputs yield page-aligned parts (so KVM's 4 KiB-alignment
/// requirement on every `KVM_SET_USER_MEMORY_REGION` field is met without the FFI
/// re-checking). Proven for ALL page-aligned `u64` inputs.
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

/// A hole that does not overlap the (non-empty) region returns exactly the single
/// full region — the spec's "no-overlap-with-hole case returns the single region",
/// for ALL such inputs.
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
    kani::assume(hole_end <= base || hole_base >= end); // hole entirely outside

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
