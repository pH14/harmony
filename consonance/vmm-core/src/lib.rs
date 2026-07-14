// SPDX-License-Identifier: AGPL-3.0-or-later
//! # vmm-core — the deterministic VMM above the `Backend` trait
//!
//! `vmm-core` is the upper half of the `docs/BRINGUP.md` crate split: everything
//! that sits **above** the [`vmm_backend::Backend`] trait and compiles against
//! that trait **alone**. It **never issues `KVM_RUN` itself** — that lives below
//! the trait in `vmm-backend`'s `KvmBackend`. Nothing here branches on which
//! backend is in use; the one place a concrete `(Backend impl, Arch vendor)`
//! pair is named is the composition root ([`bringup`]).
//!
//! It is split in two along the ISA seam (`docs/ARCH-BOUNDARY.md` §B):
//!
//! - **The engine** — everything outside [`vendor`]: the **event loop** ([`vmm`])
//!   that drives the vCPU through [`vmm_backend::Backend::run`] and dispatches the
//!   returned [`vmm_backend::Exit`], the owned guest RAM, the snapshot/branch
//!   machinery ([`snapshot`]), the state-hash *framework*, the control transport
//!   ([`control`]), the corpus adapter ([`corpus`]), the work seam ([`work`]), and
//!   the V-time/idle wiring. It speaks only `(Gpa, Moment, bytes, hashes)` plus the
//!   common exit vocabulary, and is **compiler-provably arch-blind**: it holds
//!   `<B::A as Vendor>::Devices` and reaches everything ISA-specific through the
//!   [`vendor::Vendor`] trait, so it can neither name a vendor's devices nor match a
//!   vendor's exit enum.
//! - **The vendor** ([`vendor::x86`], the sole one today) — the CPU/MSR contract and
//!   its installed policy, the exit dispatch and dispositions, the boot loaders and
//!   entry state (Multiboot v1 + the direct 64-bit Linux bzImage protocol), the
//!   interrupt fabric and platform device models (the userspace xAPIC per ruling R1,
//!   the 8259/PIT/PCI shims, the 8250 UART), the host-homogeneity probe, the
//!   retired-branch work-counter event, and the `vm_state` record set.
//!
//! An ARM vendor is a sibling module under [`vendor`], not an edit to the engine.
//!
//! Most of the crate is **pure logic, unit-testable on macOS** against a scripted
//! [`vmm_backend::MockBackend`] with no `/dev/kvm`; only the live M1/M2 gates
//! ([`bringup::boot`] over a real `KvmBackend`) are box-only. The one granted
//! `unsafe` is the box path's pinned [`vmm::GuestRam`] backing and the call to
//! the `unsafe` [`vmm_backend::Backend::map_memory`]; under Miri (and wherever
//! `mmap` is unavailable) `GuestRam` falls back to a `Vec<u8>` so the loader /
//! event-loop / `state_blob` pointer-and-bounds logic is still exercised.
//!
//! Determinism (conventions rule 4) is structural: the contract tables are
//! sorted, [`vmm::Vmm::state_blob`] is a fixed length-prefixed byte layout over
//! all observable state, no `HashMap` iteration reaches a hash, no floating
//! point, and the skeleton introduces no time source (V-time arrives later).

pub mod bringup;
pub mod control;
pub mod corpus;
// Task 81 — the `exec` improvisation's pure sentinel state machine (what bytes to
// type at the serial shell + how to detect completion/status). Portable and
// off-record by ruling; the real serial wiring lives in `vmm`/`control`.
pub mod exec;
// Task 63 — the pure-logic half of the arbitrary-V-time seal-rate measurement (the
// Wave-5 go/no-go): the V-time sampling schedule and the seal-rate / `sealable`-predicate
// bookkeeping the box harness (`tests/seal_rate_sweep.rs`) feeds live measurements into.
// Pure and portable (macOS + Linux); no `/dev/kvm`, no wall clock, no RNG.
pub mod seal_rate;
pub mod snapshot;
// The engine/vendor seam (`docs/ARCH-BOUNDARY.md` §B): every module OUTSIDE
// `vendor` is the arch-neutral engine; everything x86 lives under `vendor::x86`
// (the CPU contract, exit dispatch + dispositions, the boot loaders + entry
// state, the interrupt fabric + platform devices, the host-homogeneity probe,
// the work-counter event, and the `vm_state` record set). A module split, not a
// crate split — the reserved engine/vendor crate names activate with the ARM
// window.
pub mod vendor;
pub mod vmm;
pub mod work;
