// SPDX-License-Identifier: AGPL-3.0-or-later
//! # gicv3 — deterministic userspace GICv3 + generic-timer model
//!
//! The arm64 interrupt fabric of the deterministic hypervisor, in the ruled
//! pure shape (`docs/ARCH-BOUNDARY.md` §B, ARM row — the same seam the `lapic`
//! crate proved): **V-time nanoseconds in, deadlines and deliverable INTIDs
//! out.** Every method that needs time takes `now_vns: u64`; the crate never
//! reads a clock, holds no dependency on `vtime` (the vmm run loop joins
//! them), uses no float and no hash-ordered container, and never panics on
//! untrusted input — same seed ⇒ bit-identical fabric state.
//!
//! ## What is modeled (single vCPU, single security state, ARE=1)
//!
//! - **The INTID space** (`docs/ARCH-BOUNDARY.md` / GICv3 architecture): SGIs
//!   `0..16` (deliverable — never x86's reserved-vector rule), PPIs `16..32`,
//!   SPIs `32..32+impl_spis` where `impl_spis` is the distributor-configured
//!   implementation limit (`GICD_TYPER.ITLinesNumber`; architectural max
//!   INTID 1019). Special INTIDs `1020..1024` and extended-SPI/LPI spaces are
//!   not modeled.
//! - **Register files** per INTID: group, enable, pending, active, and an
//!   8-bit priority — programmed through the distributor frame (SPIs) and the
//!   redistributor SGI frame (SGIs/PPIs), 32-bit accesses, deny-ignore-write
//!   for read-only/unmodeled in-range offsets, loud
//!   [`GicError::BadOffset`] for a malformed offset.
//! - **Arbitration** ([`Gicv3::peek_interrupt`]): the one highest-priority
//!   deliverable Group-1 INTID — pending ∧ enabled ∧ group 1 ∧ Group-1
//!   forwarding enabled ∧ strictly higher priority (lower value) than both the
//!   priority mask (`PMR`) and the running priority (the highest-priority
//!   active interrupt). Ties resolve to the lowest INTID. Pure and
//!   deterministic; the pending→active transition happens only at
//!   [`Gicv3::take_interrupt`] (acceptance), so a snapshot taken while an
//!   INTID awaits injection shows it pending.
//! - **The EL1 virtual timer** (the generic timer's `CNTV` channel, PPI by
//!   configuration — conventionally INTID 27): `CVAL` (absolute ticks at the
//!   fixed `timer_hz`) and `CTL` (`ENABLE`/`IMASK`) programmed through
//!   sysreg-shaped methods; [`Gicv3::advance_to`] latches the timer INTID
//!   pending when the deadline passes, and [`Gicv3::next_timer_deadline`]
//!   exposes the armed deadline in V-time ns for the run loop's `run_until`
//!   seam. The tick→vns arithmetic is exact integer math (`u128`, ceilings),
//!   mirroring `lapic`'s timer discipline.
//!
//! ## What is deliberately NOT modeled (skeleton honesty, `tasks/112` M2)
//!
//! - **Delivery into a real guest.** Stock KVM/arm64 has no arbitrary-INTID
//!   queue into a userspace GIC (the CPU interface and the timer PPI couple to
//!   the in-kernel vGICv3); whether the port uses the in-kernel vGIC (whose
//!   bit-identical save/restore is exactly AA-6's measured question) or a
//!   userspace model with a patched injection seam is the spike's verdict.
//!   This crate computes arbitration and deadlines; wiring its output into a
//!   guest is `TODO(AA-6)`.
//! - **The ICC sysreg CPU interface** (`ICC_IAR1_EL1`/`ICC_EOIR1_EL1`/
//!   `ICC_PMR_EL1`): those trap only on the patched ABI (`TODO(patched-abi)`);
//!   the model exposes their state transitions as direct methods
//!   ([`Gicv3::take_interrupt`], [`Gicv3::eoi`], [`Gicv3::set_pmr`]).
//! - **Level-triggered timer semantics.** The generic timer's output is a
//!   level; the model latches one pending edge per arming (re-armed by
//!   reprogramming `CVAL`/`CTL`), which is deterministic and sufficient for
//!   the deadline seam. The full level model is contract work (AA-6).
//! - Group 0 / FIQ, SGI generation via `ICC_SGI1R_EL1`, interrupt
//!   configuration (`ICFGR`), and multi-vCPU routing (`IROUTER`) — absent or
//!   deny-ignore.

#![no_std]

mod device;
mod error;
mod state;

pub use device::{GicConfig, GicFrame, Gicv3};
pub use error::GicError;
pub use state::{
    CNTV_CTL_ENABLE, CNTV_CTL_IMASK, GIC_MAX_INTID, GIC_STATE_VERSION, GICD_FRAME_SIZE,
    GICR_FRAME_SIZE, GicState, SGI_PPI_COUNT,
};
