// SPDX-License-Identifier: AGPL-3.0-or-later
//! The trap apparatus, decoupled behind the [`Backend`] trait (ruling
//! R-Backend). `vmm-backend` is the lower half of the `docs/BRINGUP.md` crate
//! split: it owns the thing that holds the vCPU and surfaces VM-exits, while the
//! deterministic VMM above it (vmm-core, task 15) — the CPU/MSR-contract
//! dispositions, V-time, hypercalls, snapshot/restore, the userspace xAPIC/PIT
//! models — compiles against this trait **alone** and never branches on which
//! backend is in use. The portable surface (the trait, the
//! [`Exit`]/[`Event`]/[`VcpuState`]/[`Capabilities`]/[`ExitCounts`]/[`BackendError`]
//! value types, and a deterministic in-process [`MockBackend`] behind the
//! non-default `mock` feature) compiles and is fully tested on macOS and Linux;
//! the Linux-only `KvmBackend` (the bring-up stock-KVM impl, `KVM_IRQCHIP_NONE`,
//! one vCPU) lives under `#[cfg(target_os = "linux")]` so a Mac build stays green
//! with the trait + types only. One impl per substrate; the binary's composition
//! root is the one place a concrete backend is named.

mod backend;
mod config;
mod error;
mod exit;
mod state;
mod types;

// The two pointer seams (`region` slot table + GPA copies, `run_buf` kvm_run
// offset math). Used by `KvmBackend` on Linux and by their own `#[cfg(test)]`
// suites under Miri; dead on a non-test, non-Linux build, hence the conditional
// allow rather than shipping the seam unguarded.
#[cfg_attr(not(any(test, target_os = "linux")), allow(dead_code))]
mod region;
#[cfg_attr(not(any(test, target_os = "linux")), allow(dead_code))]
mod run_buf;

// The portable `Backend::run_until` orchestration (§2 inversion seam): drives the
// pure `vtime` planner over a guest-exit-aware `PreemptCpu` and maps the outcome to
// an `Exit`. Compiled on every platform (its determinism contract is property-tested
// against `vtime::sim::SimCpu` on macOS); the live `PreemptCpu` it serves is the
// box-only `KvmBackend` adapter. Dead on a non-test, non-Linux build (only the live
// adapter calls it), hence the conditional allow.
#[cfg_attr(not(any(test, target_os = "linux")), allow(dead_code))]
mod run_until;

#[cfg(feature = "mock")]
mod mock;

// `kvm` is the pure KVM exit-mapping + state-conversion logic (covered + mutation-
// tested by its synthetic-`kvm_run` unit tests); `kvm_sys` is the box-only syscall
// orchestration that wires those helpers to the ioctls (excluded from the coverage
// + mutation gates — it cannot run without `/dev/kvm`).
#[cfg(target_os = "linux")]
mod kvm;
#[cfg(target_os = "linux")]
mod kvm_sys;
// `pmu` is the **pure** `perf_event` config for the run_until branch counter (the
// `PerfEventAttr` builder + exact bit constants): no syscall, no `libc`, so it
// compiles everywhere and STAYS in the coverage + mutation gates (exact-value
// tested). `pmu_sys` is the box-only syscall orchestration (`PmuBranchCounter` +
// the raw `perf_event`/`fcntl`/`mmap` seams) that opens that config — like `kvm_sys`
// it cannot run without perf and is excluded from coverage + mutation, behind
// `#[cfg(not(miri))]` seams. Dead on a non-test, non-Linux build (only `pmu_sys`
// uses `pmu`), hence the conditional allow.
#[cfg_attr(not(any(test, target_os = "linux")), allow(dead_code))]
mod pmu;
#[cfg(target_os = "linux")]
mod pmu_sys;
// `patched_kvm` is the box-only syscall orchestration for the determinism
// backend (the `KVM_EXIT_DETERMINISM` decode/complete logic it drives is the
// pure, unit-tested `kvm` module); like `kvm_sys` it is excluded from the
// coverage + mutation gates (it cannot run without the patched `/dev/kvm`).
#[cfg(target_os = "linux")]
mod patched_kvm;

pub use backend::Backend;
pub use config::{CpuidEntry, CpuidModel, MsrFilter, MsrRange};
pub use error::{BackendError, Result};
pub use exit::{Capabilities, Event, Exit, ExitCounts, ExitReason, HypercallRegs};
pub use state::{
    DebugRegs, DescriptorTable, MpState, Segment, VcpuEvents, VcpuRegs, VcpuSregs, VcpuState,
};
pub use types::{Gpa, Vtime};

#[cfg(feature = "mock")]
pub use mock::{Completion, MockBackend};

#[cfg(target_os = "linux")]
pub use kvm_sys::KvmBackend;

#[cfg(target_os = "linux")]
pub use patched_kvm::PatchedKvmBackend;
