// SPDX-License-Identifier: AGPL-3.0-or-later
//! The host-side ARM spike harness.
//!
//! The minimal ioctl-level KVM harness (single vCPU, pinned, raw `BR_RETIRED`
//! armed guest-only) plus everything around it: the aarch64 opcode scanner, a
//! minimal ELF reader, the counting-window verifier, the PL011 console decoder,
//! deterministic run planning, and the canonical evidence formats.
//!
//! # The architecture, and what it is tested by
//!
//! Almost all of this is pure logic — no syscalls, no `unsafe` — and so it is
//! fully testable on the development Mac (which is itself aarch64, so even the
//! opcode fixtures are native). That includes the `KVM_RUN` measurement loop
//! ([`run`]): it programs against two narrow seams rather than against ioctls, so
//! window-mark decode, counter bookkeeping, overflow multiplicity and record
//! assembly are all driven natively against a scripted vCPU. The one exception is
//! [`sys`], the perf/KVM syscall seam, which is Linux-only and **has never run**:
//! the Altra box is not yet in hand. The seam is deliberately thin, so that logic is
//! testable and the syscall layer is small, and so that a silent fallback cannot
//! masquerade as the mechanism under test (`docs/ARM-ALTRA.md` §Evidence integrity
//! #4). Its ABI half — the `perf_event_attr` flag bits, the ioctl numbers, the
//! `kvm_run` offsets — is portable data and is unit-tested here too, because a flag
//! on the wrong bit arms a different event and reports it green.
//!
//! **This whole crate is untested on silicon.** It is apparatus, built so that
//! arrival day is spent measuring, not scaffolding.

// `deny`, not `forbid`: the single perf/KVM syscall seam ([`sys`]) needs a local
// `#![allow(unsafe_code)]`, which `forbid` would make impossible. `deny` still
// fails the build on any unsafe outside that one module's explicit opt-in, so the
// guarantee is the same everywhere it matters — the scanner, ELF reader, console
// decoder, planner and evidence writer contain none.
#![deny(unsafe_code)]

pub mod console;
pub mod el0;
pub mod elf;
pub mod evidence;
pub mod plan;
pub mod run;
pub mod scan;
pub mod truth_table;
pub mod verify;

// The syscall seam is the crate's only `unsafe`; its own module-level
// `#![allow(unsafe_code)]` scopes the opt-in. Everything else is `deny` above.
pub mod sys;
