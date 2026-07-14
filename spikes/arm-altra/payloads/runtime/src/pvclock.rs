// SPDX-License-Identifier: AGPL-3.0-or-later
//! The work-derived clock page (`docs/PARAVIRT-CLOCK.md` ABI 1).
//!
//! The guest reads a **materialized** value — the V-time and virtual counter the
//! vmm already computed from `work` — and performs no arithmetic against any live
//! hardware counter (`docs/PARAVIRT-CLOCK.md` §0). That is the whole design: on
//! ARM without FEAT_ECV a guest counter read cannot be trapped, so the counter is
//! closed at the *contract* level and time is handed over as a finished number.
//! Neoverse N1 is Armv8.2 and has no ECV, and neither does any other reachable ARM
//! server part (`docs/ARM-ALTRA.md` §1).
//!
//! This module is the minimum needed to *test* that design at payload level
//! (AA-5(a)); `hm-8h8` owns the design itself and is not duplicated here.
//!
//! # Same self-seeding attestation as the params page
//!
//! Under the harness the page is published and refreshed. Under TCG it is zeroed
//! RAM, so [`ensure`] publishes a static, plausible page and reports
//! `self-seeded` — which the AA-5 acceptance forbids and the TCG golden requires.
//! A harness that never published the page cannot therefore be mistaken for one
//! that did.
//!
//! # Why the seqlock never spins inside a counting window
//!
//! The `clock-page` payload reads the page in a loop inside its counting window.
//! A retry would add a taken branch that no analytical oracle could predict — so
//! it is worth being precise about why one cannot happen: the harness can only
//! write the page when the vCPU has **exited**, and a counting window contains no
//! exits by construction (its only MMIO accesses are the two mark stores that
//! delimit it). The page is therefore quiescent for the whole window. The payload
//! counts retries anyway, branch-free, and reports the total — so if that argument
//! is ever wrong, it fails loudly instead of quietly corrupting the oracle.

use oracle_model::pvclock::{OFF_ABI, OFF_FLAGS, OFF_GUEST_CLOCK, OFF_HZ, OFF_SEQ, OFF_VNS};
use oracle_model::{PVCLOCK_ABI, PVCLOCK_GPA};

/// `flags` bit 0: the materialized-value flag (`oracle_model::pvclock::FLAG_MATERIALIZED`),
/// re-exported so the `clock-page` payload can test it as `pvclock::FLAG_MATERIALIZED`.
pub use oracle_model::pvclock::FLAG_MATERIALIZED;

/// A plausible counter frequency for the self-seeded page. Never used by a
/// managed run; present only so the TCG protocol exercise has a nonzero field.
const SELF_SEED_HZ: u64 = 1_000_000_000;

/// Where the clock page came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// The harness published it.
    Managed,
    /// No valid page: this payload published a static one (the TCG case).
    SelfSeeded,
}

impl Mode {
    /// The token this mode prints as, on the `CLOCKPAGE` protocol line.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Mode::Managed => "managed",
            Mode::SelfSeeded => "self-seeded",
        }
    }
}

/// Ensure a valid clock page exists, publishing a static one if the harness did
/// not. Returns which happened.
pub fn ensure() -> Mode {
    // The field values and offsets are the shared, Miri-tested layout
    // (`oracle_model::pvclock`); the writes here are the payload-side seqlock protocol
    // that layout is written *through*. `at(off)` turns a page offset into its GPA.
    let at = |off: usize| PVCLOCK_GPA + off as u64;
    // SAFETY: PVCLOCK_GPA is the second page of guest RAM, mapped Normal by the
    // boot shim and left uncovered by every output section of `linker.ld`.
    unsafe {
        let abi = core::ptr::read_volatile(at(OFF_ABI) as *const u32);
        if abi == PVCLOCK_ABI {
            return Mode::Managed;
        }

        // Publish a static page in the seqlock's own update order (odd, publish,
        // even), so the TCG path exercises the real protocol rather than a
        // shortcut.
        core::ptr::write_volatile(at(OFF_SEQ) as *mut u32, 1);
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        core::ptr::write_volatile(at(OFF_VNS) as *mut u64, 0);
        core::ptr::write_volatile(at(OFF_GUEST_CLOCK) as *mut u64, 0);
        core::ptr::write_volatile(at(OFF_HZ) as *mut u64, SELF_SEED_HZ);
        core::ptr::write_volatile(at(OFF_FLAGS) as *mut u32, FLAG_MATERIALIZED);
        core::ptr::write_volatile(at(OFF_ABI) as *mut u32, PVCLOCK_ABI);
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        core::ptr::write_volatile(at(OFF_SEQ) as *mut u32, 2);

        Mode::SelfSeeded
    }
}

/// The page's non-time fields, read once for the payload's protocol report.
#[must_use]
pub fn header() -> (u32, u32) {
    // SAFETY: as [`ensure`].
    unsafe {
        (
            core::ptr::read_volatile((PVCLOCK_GPA + OFF_ABI as u64) as *const u32),
            core::ptr::read_volatile((PVCLOCK_GPA + OFF_FLAGS as u64) as *const u32),
        )
    }
}
