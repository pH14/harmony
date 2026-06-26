// SPDX-License-Identifier: AGPL-3.0-or-later
//! Address and V-time newtypes that cross the `Backend` boundary.
//!
//! Both are `#[repr(transparent)]` so they carry no representation cost over the
//! bare `u64` while making a guest-physical address un-confusable with a host
//! pointer or a length, and a V-time deadline un-confusable with a nanosecond
//! count (`[refinement]` of R-Backend's bare `Gpa`/`VTime`).

/// Guest-physical address. `[refinement]` of R-Backend's bare `Gpa`: a
/// transparent newtype so an address can't be confused with a host pointer or a
/// length at a call site.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[repr(transparent)]
pub struct Gpa(pub u64);

/// A V-time deadline for `run_until`. `[refinement]` of R-Backend's bare `VTime`:
/// the unit is a **retired-conditional-branch work count** — the same axis
/// `vtime`'s `work` and task 07's PMU measure — **not** nanoseconds. vmm-core
/// converts vns↔work via `vtime`; the backend counts hardware events, so the
/// unit is fixed here at the trait boundary and never silently reinterpreted.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(transparent)]
pub struct Vtime(pub u64);
