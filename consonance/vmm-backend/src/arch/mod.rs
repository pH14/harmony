// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-architecture vocabulary (`docs/ARCH-BOUNDARY.md`): the value types that
//! name guest-observable CPU events and state — the register record set, the
//! CPU-contract policy tables, interrupt identities, and the work-counter event
//! pin — live under one module per vendor, giving the ISA seam a
//! compiler-visible home. Everything outside this module speaks only
//! `(Gpa, Moment, bytes, hashes)` plus the common exit vocabulary.
//!
//! x86-64 is the sole vendor today; an ARM vendor is additive here.

pub mod x86;
