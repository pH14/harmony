// SPDX-License-Identifier: AGPL-3.0-or-later
//! The trap apparatus, decoupled behind the [`Backend`] trait (ruling
//! R-Backend), generic over the ISA it traps (the [`Arch`] seam,
//! `docs/ARCH-BOUNDARY.md`). `vmm-backend` is the lower half of the
//! `docs/BRINGUP.md` crate split: it owns the thing that holds the vCPU and
//! surfaces VM-exits, while the deterministic VMM above it (vmm-core) — the
//! CPU/MSR-contract dispositions, V-time, hypercalls, snapshot/restore, the
//! userspace interrupt-fabric models — compiles against this trait **alone**
//! and never branches on which backend or which ISA is in use. The portable
//! surface (the traits, the two-level [`Exit`] and the per-vendor value types
//! under [`arch`], [`Capabilities`]/[`ExitCounts`]/[`BackendError`], and a
//! deterministic in-process [`MockBackend`] behind the non-default `mock`
//! feature) compiles and is fully tested on macOS and Linux; the Linux-only
//! `KvmBackend` (the bring-up stock-KVM impl, `KVM_IRQCHIP_NONE`, one vCPU)
//! lives under `#[cfg(target_os = "linux")]` so a Mac build stays green with
//! the traits + types only. One impl per (substrate, arch) pair; the binary's
//! composition root is the one place a concrete pair is named.

pub mod arch;
mod backend;
mod error;
mod exit;
mod types;

// The two pointer seams (`region` slot table + GPA copies, `run_buf` kvm_run
// offset math). Used by `KvmBackend` on Linux and by their own `#[cfg(test)]`
// suites under Miri; dead on a non-test, non-Linux build, hence the conditional
// allow rather than shipping the seam unguarded.
#[cfg_attr(
    not(any(test, all(target_os = "linux", target_arch = "x86_64"))),
    allow(dead_code)
)]
mod region;
#[cfg_attr(
    not(any(test, all(target_os = "linux", target_arch = "x86_64"))),
    allow(dead_code)
)]
mod run_buf;

// The portable `Backend::run_until` orchestration (§2 inversion seam): drives the
// pure `vtime` planner over a guest-exit-aware `PreemptCpu` and maps the outcome to
// an `Exit`. Compiled on every platform (its determinism contract is property-tested
// against `vtime::sim::SimCpu` on macOS); the live `PreemptCpu` it serves is the
// box-only `KvmBackend` adapter. Dead on a non-test, non-Linux build (only the live
// adapter calls it), hence the conditional allow.
#[cfg_attr(
    not(any(test, all(target_os = "linux", target_arch = "x86_64"))),
    allow(dead_code)
)]
mod run_until;

#[cfg(feature = "mock")]
mod mock;
#[cfg(feature = "mock")]
mod mock_arm64;

// The **x86-64 KVM substrate**, gated on the architecture it traps as well as the
// OS (`all(target_os = "linux", target_arch = "x86_64")` — the same seam
// `vmm-core`'s `hostassert` already uses). `kvm_bindings` exposes a *different*
// `kvm_regs`/`kvm_sregs` on each arch, so this code is not merely Linux-only, it is
// x86-64-only: gating it on the OS alone made the crate fail to even `cargo check`
// on `aarch64-unknown-linux-gnu`, which would have blocked the additive ARM backend
// the `Arch` seam exists to enable (`docs/ARCH-BOUNDARY.md` §D). An ARM vendor adds
// its own `kvm_arm64`/`kvm_arm64_sys` pair beside these under its own arch gate.
//
// `kvm` is the pure KVM exit-mapping + state-conversion logic (covered + mutation-
// tested by its synthetic-`kvm_run` unit tests); `kvm_sys` is the box-only syscall
// orchestration that wires those helpers to the ioctls (excluded from the coverage
// + mutation gates — it cannot run without `/dev/kvm`).
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod kvm;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod kvm_sys;
// `pmu` is the **pure** `perf_event` config for the run_until branch counter (the
// `PerfEventAttr` builder + exact bit constants): no syscall, no `libc`, so it
// compiles everywhere and STAYS in the coverage + mutation gates (exact-value
// tested). `pmu_sys` is the box-only syscall orchestration (`PmuBranchCounter` +
// the raw `perf_event`/`fcntl`/`mmap` seams) that opens that config — like `kvm_sys`
// it cannot run without perf and is excluded from coverage + mutation, behind
// `#[cfg(not(miri))]` seams. Dead on a non-test, non-Linux build (only `pmu_sys`
// uses `pmu`), hence the conditional allow.
#[cfg_attr(
    not(any(test, all(target_os = "linux", target_arch = "x86_64"))),
    allow(dead_code)
)]
mod pmu;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod pmu_sys;
// `patched_kvm` is the box-only syscall orchestration for the determinism
// backend (the `KVM_EXIT_DETERMINISM` decode/complete logic it drives is the
// pure, unit-tested `kvm` module); like `kvm_sys` it is excluded from the
// coverage + mutation gates (it cannot run without the patched `/dev/kvm`).
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod patched_kvm;

pub use arch::arm64::{
    Arm64, Arm64Caps, Arm64Completion, Arm64CoreRegs, Arm64Exit, Arm64Injection, Arm64Policy,
    Arm64SysregFile, Arm64VcpuState, GicIntId, IdRegModel, RAW_BR_RETIRED, SysregTrapPolicy,
};
pub use arch::x86::{
    CpuidEntry, CpuidModel, DebugRegs, DescriptorTable, Injection, MsrFilter, MsrRange, Segment,
    VcpuEvents, VcpuRegs, VcpuSregs, VcpuState, X86, X86Caps, X86Completion, X86Exit, X86Policy,
};
pub use arch::{Arch, ArchCaps, ArchExit};
pub use backend::Backend;
pub use error::{BackendError, Result};
pub use exit::{Capabilities, CommonExit, Exit, ExitCounts, ExitReason, HypercallFrame};
pub use types::{Gpa, Moment, MpState};

#[cfg(feature = "mock")]
pub use mock::{Completion, MockBackend, MockCaps};
#[cfg(feature = "mock")]
pub use mock_arm64::{Arm64MockCompletion, MockArm64Backend, MockArm64Caps};

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub use kvm_sys::KvmBackend;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub use patched_kvm::PatchedKvmBackend;
