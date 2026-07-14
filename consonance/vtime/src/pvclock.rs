// SPDX-License-Identifier: AGPL-3.0-or-later
//! The paravirt **work-derived clock page** — layout and stamping functions
//! (`docs/PARAVIRT-CLOCK.md` §1, ABI v1).
//!
//! One 4 KiB guest page carries **materialized** V-time: the already-computed
//! `vns` / `guest_clock` values as of the last refresh, seqlock-versioned so a
//! reader never sees a torn update. Every field is a pure function of
//! `(work, VClock config)` — there is no live-counter term in the guest read
//! path at all (the whole point of the design: it works on a chip whose
//! counter cannot be trapped, and deletes the counter-read exit on one where
//! it can).
//!
//! This module is deliberately **arch-blind and side-effect-free**: pure
//! functions over a byte slice. It knows nothing about guest RAM, backends, or
//! registration transports — the run-loop half (when to refresh, where the
//! page lives) is `vmm-core`'s. Keeping the stamping arithmetic here means the
//! one function that turns `(vns, guest_clock, hz)` into page bytes is shared
//! verbatim by the x86 vendor today and an ARM vendor later, and is testable
//! (and property-testable) with no VM at all.
//!
//! ## Write protocol (§1)
//!
//! [`stamp`] publishes values with the kvmclock-precedent seqlock sequence —
//! `seq |= 1` (odd: in progress), fields, `seq = (odd + 1)` (even, one epoch
//! newer). The host writer runs only while the guest is paused, so the
//! ordering is belt-and-braces for a single-vCPU guest — but it is
//! load-bearing for a reader that **straddles a refresh** (read `seq`, get
//! descheduled by an exit, resume after a re-stamp): the changed epoch forces
//! the retry that keeps the read consistent. [`stamp`] therefore bumps the
//! epoch **only when the published values actually change** — a value-identical
//! refresh leaves the page byte-for-byte untouched, which is what makes the
//! page bytes (hashed as guest RAM) a pure function of the clock-value stream
//! rather than of the refresh *schedule*.
//!
//! ## Canonical form (§1.1)
//!
//! [`stamp_canonical`] re-stamps the whole page to its canonical form —
//! `seq = 0`, values for the given `(vns, guest_clock)`, reserved tail zeroed —
//! a total function of `(work, config)` carrying zero refresh-history entropy.
//! Sealed/snapshotted pages are always canonical, so a restored run and a
//! never-restored run at the same effective V-time hash identically.

/// The page-layout ABI version stamped at [`ABI_VERSION_OFF`]
/// (`HARMONY_PVCLOCK_ABI = 1`). A guest reads it once at clocksource
/// registration; a mismatch is a guest-side hard fault, never a silent
/// reinterpret.
pub const PVCLOCK_ABI_VERSION: u32 = 1;

/// The page size (one 4 KiB guest page).
pub const PVCLOCK_PAGE_LEN: usize = 4096;

/// `flags` bit 0: the values are **materialized** (finished numbers — do not
/// interpolate against a live counter). Always set for ABI v1.
pub const PVCLOCK_FLAG_MATERIALIZED: u32 = 1;

/// `flags` bit 1: the values are **work-derived** — computed from the
/// deterministic work counter by a real stamping path, never a placeholder.
/// Set by every stamp this module writes; the ARM vendor spike's *static
/// placeholder page* deliberately leaves it clear, so the AA-5 gate (and any
/// consumer that requires it) fails closed against a page nothing is
/// actually deriving. (Ruled at the PR #108 r9 / PR #110 coordination,
/// 2026-07-14.)
pub const PVCLOCK_FLAG_WORK_DERIVED: u32 = 1 << 1;

/// The full ABI-v1 flags word every real stamp publishes
/// ([`PVCLOCK_FLAG_MATERIALIZED`] | [`PVCLOCK_FLAG_WORK_DERIVED`]); remaining
/// bits reserved-zero.
pub const PVCLOCK_FLAGS_V1: u32 = PVCLOCK_FLAG_MATERIALIZED | PVCLOCK_FLAG_WORK_DERIVED;

/// Byte offset of `abi_version: u32` (little-endian, like every field).
pub const ABI_VERSION_OFF: usize = 0x00;
/// Byte offset of `seq: u32` — the seqlock counter (odd ⇒ update in progress).
pub const SEQ_OFF: usize = 0x04;
/// Byte offset of `vns: u64` — materialized V-time in nanoseconds.
pub const VNS_OFF: usize = 0x08;
/// Byte offset of `guest_clock: u64` — the materialized virtual counter (the
/// guest-visible clock: on x86 exactly what the retained RDTSC trap returns).
pub const GUEST_CLOCK_OFF: usize = 0x10;
/// Byte offset of `guest_clock_hz: u64` — the counter frequency in Hz,
/// constant for the machine's life.
pub const GUEST_CLOCK_HZ_OFF: usize = 0x18;
/// Byte offset of `flags: u32`.
pub const FLAGS_OFF: usize = 0x20;
/// Byte offset of `vcpu_index: u32` (pinned 0 — single-vCPU).
pub const VCPU_INDEX_OFF: usize = 0x24;
/// First byte of the reserved-zero tail (to the end of the page).
pub const RESERVED_OFF: usize = 0x28;

/// The decoded fixed fields of a pvclock page — what [`read`] returns and
/// what the stamping functions publish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PvclockFields {
    /// Layout ABI version ([`PVCLOCK_ABI_VERSION`]).
    pub abi_version: u32,
    /// The seqlock counter at read time (even — [`read`] refuses odd).
    pub seq: u32,
    /// Materialized V-time in nanoseconds.
    pub vns: u64,
    /// Materialized virtual counter (guest-visible clock).
    pub guest_clock: u64,
    /// Counter frequency in Hz.
    pub guest_clock_hz: u64,
    /// Flag bits ([`PVCLOCK_FLAG_MATERIALIZED`] | [`PVCLOCK_FLAG_WORK_DERIVED`]).
    pub flags: u32,
    /// vCPU index (pinned 0 for ABI v1).
    pub vcpu_index: u32,
}

#[inline]
fn put_u32(page: &mut [u8], off: usize, v: u32) {
    page[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

#[inline]
fn put_u64(page: &mut [u8], off: usize, v: u64) {
    page[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

#[inline]
fn get_u32(page: &[u8], off: usize) -> u32 {
    // Callers bounds-check `page` once up front; the offsets are compile-time
    // constants within PVCLOCK_PAGE_LEN.
    u32::from_le_bytes([page[off], page[off + 1], page[off + 2], page[off + 3]])
}

#[inline]
fn get_u64(page: &[u8], off: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&page[off..off + 8]);
    u64::from_le_bytes(b)
}

/// `true` iff the page already publishes exactly these values in a stable
/// (even-`seq`) frame with the fixed fields at their ABI-v1 constants — i.e. a
/// [`stamp`] with the same values would be a no-op.
///
/// Returns `false` for a short slice (never panics on untrusted input).
pub fn published(page: &[u8], vns: u64, guest_clock: u64, guest_clock_hz: u64) -> bool {
    if page.len() < PVCLOCK_PAGE_LEN {
        return false;
    }
    get_u32(page, SEQ_OFF) & 1 == 0
        && get_u32(page, ABI_VERSION_OFF) == PVCLOCK_ABI_VERSION
        && get_u64(page, VNS_OFF) == vns
        && get_u64(page, GUEST_CLOCK_OFF) == guest_clock
        && get_u64(page, GUEST_CLOCK_HZ_OFF) == guest_clock_hz
        && get_u32(page, FLAGS_OFF) == PVCLOCK_FLAGS_V1
        && get_u32(page, VCPU_INDEX_OFF) == 0
}

/// Publish `(vns, guest_clock, guest_clock_hz)` into the page with the §1
/// seqlock write protocol. Returns `true` iff the page bytes changed (the
/// caller's dirty-tracking signal).
///
/// **Value-keyed idempotence:** if the page already publishes exactly these
/// values ([`published`]), nothing is written and the epoch does not move —
/// so the page bytes are a pure function of the *distinct-value* stream, not
/// of how many times the run loop refreshed. When the values do change, the
/// epoch advances to the next even count (`seq → seq|1 → (seq|1)+1`), which is
/// what forces a straddling reader's retry.
///
/// The write order in this in-process implementation is program order over a
/// byte slice; the guest observes it only across a VM-entry boundary, which is
/// the (much stronger) publication barrier. A short slice
/// (`len < PVCLOCK_PAGE_LEN`) is a no-op returning `false` — library code
/// never panics on untrusted input; the caller validates the page location at
/// registration.
pub fn stamp(page: &mut [u8], vns: u64, guest_clock: u64, guest_clock_hz: u64) -> bool {
    if page.len() < PVCLOCK_PAGE_LEN {
        return false;
    }
    if published(page, vns, guest_clock, guest_clock_hz) {
        return false;
    }
    // seq ← seq | 1 (odd: update in progress).
    let odd = get_u32(page, SEQ_OFF) | 1;
    put_u32(page, SEQ_OFF, odd);
    // Publish the new materialized values (+ the fixed ABI fields, so a stamp
    // also repairs a page the guest scribbled on — self-healing, and keeps the
    // stable frame a total function of the published values).
    put_u32(page, ABI_VERSION_OFF, PVCLOCK_ABI_VERSION);
    put_u64(page, VNS_OFF, vns);
    put_u64(page, GUEST_CLOCK_OFF, guest_clock);
    put_u64(page, GUEST_CLOCK_HZ_OFF, guest_clock_hz);
    put_u32(page, FLAGS_OFF, PVCLOCK_FLAGS_V1);
    put_u32(page, VCPU_INDEX_OFF, 0);
    // seq ← odd + 1 (even: stable, one epoch newer). Wrapping: a u32 epoch
    // rolling over is deterministic and the reader only compares equality.
    put_u32(page, SEQ_OFF, odd.wrapping_add(1));
    true
}

/// Re-stamp the **whole page** to canonical form (§1.1): `seq = 0`, the given
/// values, fixed fields at their constants, reserved tail zeroed. Returns
/// `true` iff the page bytes changed.
///
/// This is the seal/snapshot-quiescent-point form: a total function of
/// `(vns, guest_clock, guest_clock_hz)` and nothing else, carrying zero
/// refresh-history entropy — two same-seed runs that refreshed different
/// numbers of times (or a restored vs. never-restored run) seal byte-identical
/// pages. The guest never observes `seq = 0` as special: it only reads the
/// page while running, and a reader that straddled the seal retries (the
/// epoch changed) and then succeeds against the canonical frame, whose values
/// equal the pre-seal stable frame's at the same work count.
///
/// A short slice is a no-op returning `false` (see [`stamp`]).
pub fn stamp_canonical(page: &mut [u8], vns: u64, guest_clock: u64, guest_clock_hz: u64) -> bool {
    if page.len() < PVCLOCK_PAGE_LEN {
        return false;
    }
    let mut canonical = [0u8; PVCLOCK_PAGE_LEN];
    put_u32(&mut canonical, ABI_VERSION_OFF, PVCLOCK_ABI_VERSION);
    // seq = 0 (even, stable) — the zeroed default.
    put_u64(&mut canonical, VNS_OFF, vns);
    put_u64(&mut canonical, GUEST_CLOCK_OFF, guest_clock);
    put_u64(&mut canonical, GUEST_CLOCK_HZ_OFF, guest_clock_hz);
    put_u32(&mut canonical, FLAGS_OFF, PVCLOCK_FLAGS_V1);
    // vcpu_index = 0, reserved tail = 0 — already the zeroed default.
    if page[..PVCLOCK_PAGE_LEN] == canonical {
        return false;
    }
    page[..PVCLOCK_PAGE_LEN].copy_from_slice(&canonical);
    true
}

/// Decode the page's fixed fields, seqlock-checked: `None` if the slice is
/// short, the frame is mid-update (odd `seq`), or the ABI version is not
/// [`PVCLOCK_ABI_VERSION`]. This is the host-side mirror of the guest reader
/// (for gates and tests); it performs the same odd-check the guest's retry
/// loop does, minus the retry (the host reads only while it is not writing).
pub fn read(page: &[u8]) -> Option<PvclockFields> {
    if page.len() < PVCLOCK_PAGE_LEN {
        return None;
    }
    let seq = get_u32(page, SEQ_OFF);
    if seq & 1 != 0 {
        return None;
    }
    let abi_version = get_u32(page, ABI_VERSION_OFF);
    if abi_version != PVCLOCK_ABI_VERSION {
        return None;
    }
    Some(PvclockFields {
        abi_version,
        seq,
        vns: get_u64(page, VNS_OFF),
        guest_clock: get_u64(page, GUEST_CLOCK_OFF),
        guest_clock_hz: get_u64(page, GUEST_CLOCK_HZ_OFF),
        flags: get_u32(page, FLAGS_OFF),
        vcpu_index: get_u32(page, VCPU_INDEX_OFF),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_page() -> Vec<u8> {
        vec![0u8; PVCLOCK_PAGE_LEN]
    }

    #[test]
    fn stamp_publishes_all_fields_le() {
        let mut page = fresh_page();
        assert!(stamp(
            &mut page,
            0x1122_3344_5566_7788,
            0xAABB_CCDD_EEFF_0011,
            2_000_000_000
        ));
        // Raw little-endian bytes at the §1 offsets.
        assert_eq!(
            page[ABI_VERSION_OFF..ABI_VERSION_OFF + 4],
            1u32.to_le_bytes()
        );
        assert_eq!(
            page[VNS_OFF..VNS_OFF + 8],
            0x1122_3344_5566_7788u64.to_le_bytes()
        );
        assert_eq!(
            page[GUEST_CLOCK_OFF..GUEST_CLOCK_OFF + 8],
            0xAABB_CCDD_EEFF_0011u64.to_le_bytes()
        );
        assert_eq!(
            page[GUEST_CLOCK_HZ_OFF..GUEST_CLOCK_HZ_OFF + 8],
            2_000_000_000u64.to_le_bytes()
        );
        assert_eq!(page[FLAGS_OFF..FLAGS_OFF + 4], 3u32.to_le_bytes());
        assert_eq!(page[VCPU_INDEX_OFF..VCPU_INDEX_OFF + 4], 0u32.to_le_bytes());
        let f = read(&page).expect("stable frame");
        assert_eq!(f.vns, 0x1122_3344_5566_7788);
        assert_eq!(f.guest_clock, 0xAABB_CCDD_EEFF_0011);
        assert_eq!(f.guest_clock_hz, 2_000_000_000);
        assert_eq!(f.flags, PVCLOCK_FLAGS_V1);
        assert_eq!(f.vcpu_index, 0);
        // First publish from a zeroed page: 0 → 1 → 2.
        assert_eq!(f.seq, 2);
    }

    #[test]
    fn stamp_is_value_keyed_idempotent() {
        let mut page = fresh_page();
        assert!(stamp(&mut page, 100, 200, 1_000_000_000));
        let snapshot = page.clone();
        // Same values again: byte-identical, seq unmoved, reports unchanged.
        assert!(!stamp(&mut page, 100, 200, 1_000_000_000));
        assert_eq!(page, snapshot);
        // New values: epoch advances by exactly one even step.
        assert!(stamp(&mut page, 101, 202, 1_000_000_000));
        assert_eq!(read(&page).unwrap().seq, 4);
    }

    #[test]
    fn stamp_epoch_forces_straddling_reader_retry() {
        let mut page = fresh_page();
        stamp(&mut page, 1, 2, 3);
        let seq_before = read(&page).unwrap().seq;
        stamp(&mut page, 10, 20, 3);
        // The epoch moved, so a reader holding `seq_before` re-reads.
        assert_ne!(read(&page).unwrap().seq, seq_before);
    }

    #[test]
    fn canonical_is_total_function_of_values() {
        let mut a = fresh_page();
        let mut b = fresh_page();
        // Two different histories...
        for i in 0..7u64 {
            stamp(&mut a, i, 2 * i, 5);
        }
        stamp(&mut b, 999, 999, 5);
        // ...and some guest scribbles in the reserved tail of one of them.
        b[RESERVED_OFF + 100] = 0xEE;
        // Canonicalizing both to the same values yields byte-identical pages.
        assert!(stamp_canonical(&mut a, 42, 84, 5));
        assert!(stamp_canonical(&mut b, 42, 84, 5));
        assert_eq!(a, b);
        assert_eq!(read(&a).unwrap().seq, 0);
        // Canonicalizing an already-canonical page is a byte-level no-op.
        let snap = a.clone();
        assert!(!stamp_canonical(&mut a, 42, 84, 5));
        assert_eq!(a, snap);
    }

    #[test]
    fn post_canonical_epochs_match_restored_and_continued_runs() {
        // A sealed-and-continued run and a restored run both continue from the
        // canonical page: the next distinct-value stamp lands the same epoch.
        let mut continued = fresh_page();
        for i in 0..5u64 {
            stamp(&mut continued, i, i, 7);
        }
        stamp_canonical(&mut continued, 4, 4, 7);
        let mut restored = fresh_page();
        stamp_canonical(&mut restored, 4, 4, 7); // the snapshot's page image
        assert_eq!(continued, restored);
        assert!(stamp(&mut continued, 9, 9, 7));
        assert!(stamp(&mut restored, 9, 9, 7));
        assert_eq!(continued, restored);
        assert_eq!(read(&continued).unwrap().seq, 2);
    }

    #[test]
    fn stamp_repairs_guest_scribbles_in_fixed_fields() {
        let mut page = fresh_page();
        stamp(&mut page, 5, 10, 3);
        // A (misbehaving but deterministic) guest scribbles the flags field.
        put_u32(&mut page, FLAGS_OFF, 0xDEAD);
        // `published` no longer holds, so the next stamp re-publishes and
        // repairs the fixed fields.
        assert!(stamp(&mut page, 5, 10, 3));
        assert_eq!(read(&page).unwrap().flags, PVCLOCK_FLAGS_V1);
    }

    #[test]
    fn read_refuses_odd_seq_and_bad_abi() {
        let mut page = fresh_page();
        stamp(&mut page, 1, 2, 3);
        put_u32(&mut page, SEQ_OFF, 7); // mid-update frame
        assert!(read(&page).is_none());
        put_u32(&mut page, SEQ_OFF, 8);
        put_u32(&mut page, ABI_VERSION_OFF, 2); // foreign ABI
        assert!(read(&page).is_none());
    }

    #[test]
    fn short_slices_are_total_no_ops() {
        let mut short = vec![0u8; PVCLOCK_PAGE_LEN - 1];
        assert!(!stamp(&mut short, 1, 2, 3));
        assert!(!stamp_canonical(&mut short, 1, 2, 3));
        assert!(!published(&short, 1, 2, 3));
        assert!(read(&short).is_none());
        assert!(short.iter().all(|&b| b == 0), "no partial write");
    }

    #[test]
    fn seq_epoch_wraps_deterministically() {
        let mut page = fresh_page();
        stamp(&mut page, 1, 1, 1);
        // Force the epoch to the wrap boundary and publish once more.
        put_u32(&mut page, SEQ_OFF, u32::MAX - 1); // even
        stamp(&mut page, 2, 2, 1);
        // (u32::MAX - 1) | 1 = u32::MAX (odd), +1 wraps to 0 (even).
        assert_eq!(read(&page).unwrap().seq, 0);
    }
}
