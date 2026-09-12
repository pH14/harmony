// SPDX-License-Identifier: AGPL-3.0-or-later
//! The trap apparatus, decoupled behind the [`Backend`] trait (ruling
//! R-Backend), generic over the ISA it traps (the [`Arch`] seam,
//! `docs/ARCHITECTURE.md`). `vmm-backend` is the lower half of the
//! `docs/ARCHITECTURE.md` crate split: it owns the thing that holds the vCPU and
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

#[cfg(feature = "contract-tests")]
pub mod contract;

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

#[cfg(feature = "mock")]
mod mock;
#[cfg(feature = "mock")]
mod mock_arm64;

mod arm64_kvm;
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
mod arm64_kvm_sys;
#[cfg(all(target_os = "macos", target_arch = "aarch64", not(miri), not(kani)))]
mod hvf;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod kvm;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod kvm_sys;

pub use arch::arm64::{
    ARM64_GIC_BITMAP_WORDS, ARM64_GIC_PRIORITY_BYTES, Arm64, Arm64Caps, Arm64Completion,
    Arm64CoreRegs, Arm64DebugState, Arm64Exit, Arm64GicState, Arm64Injection, Arm64InterruptState,
    Arm64Policy, Arm64SimdFpState, Arm64SysregFile, Arm64VcpuState, Arm64VtimerState, GicIntId,
    IdRegModel, SysregTrapPolicy,
};
pub use arch::x86::{
    CpuidEntry, CpuidModel, DebugRegs, DescriptorTable, Injection, MsrFilter, MsrRange, Segment,
    VcpuEvents, VcpuRegs, VcpuSregs, VcpuState, X86, X86Caps, X86Completion, X86Exit, X86Policy,
};
pub use arch::{Arch, ArchExit};
pub use backend::Backend;
pub use error::{BackendError, Result};
pub use exit::{Capabilities, CommonExit, Exit, ExitCounts, ExitReason, HypercallFrame};
pub use types::{Gpa, MpState};

#[cfg(feature = "mock")]
pub use mock::{Completion, MockBackend, MockCaps};
#[cfg(feature = "mock")]
pub use mock_arm64::{Arm64MockCompletion, MockArm64Backend, MockArm64Caps};

#[cfg(feature = "mock")]
pub use arm64_kvm::FakeKvm;
pub use arm64_kvm::{Arm64Kvm, Arm64KvmBackend, KvmRunView, MmioView};

#[cfg(all(target_os = "macos", target_arch = "aarch64", not(miri), not(kani)))]
pub use hvf::{HvfBackend, HvfExitHandle};

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub use arm64_kvm_sys::LiveKvm;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub use kvm_sys::KvmBackend;
