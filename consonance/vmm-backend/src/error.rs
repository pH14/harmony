// SPDX-License-Identifier: AGPL-3.0-or-later
//! The backend error type and crate `Result` alias.
//!
//! Every `Backend` method is fallible (rule #4: library code never panics on
//! untrusted input — malformed completions, bad offsets, and incompatible
//! `VcpuState` are *errors*, not panics). `BackendError` is impl-agnostic: a
//! `MockBackend` and a `KvmBackend` surface the same closed set, so vmm-core's
//! error handling never branches on which backend produced it.

/// Crate result alias.
pub type Result<T> = core::result::Result<T, BackendError>;

/// Every way a `Backend` operation can fail. Closed and impl-agnostic.
#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    /// A trait method this backend does not implement (e.g. bring-up
    /// `KvmBackend::run_until`/`inject`, which are Phase 2). `what` names the
    /// method so the unison report can say what is missing.
    #[error("backend does not support: {what}")]
    Unsupported {
        /// The unsupported operation (`"run_until"`, `"inject"`, …).
        what: &'static str,
    },

    /// A required KVM capability is missing on this host.
    #[error("missing capability: {cap}")]
    Capability {
        /// The capability that was probed and found absent.
        cap: &'static str,
    },

    /// `run`/`run_until` called before BOTH `set_cpuid` and `set_msr_filter`
    /// succeeded — running on host-derived CPUID/MSR defaults would leak
    /// nondeterminism, so the backend fails closed instead.
    #[error("backend not configured: set_cpuid + set_msr_filter required before run")]
    NotConfigured,

    /// `map_memory` misuse: bad alignment, region overlap, or zero length. The
    /// `&'static str` names which invariant was violated.
    #[error("memory mapping error: {0}")]
    Memory(&'static str),

    /// `run`/`run_until` called with an un-serviced read-style / `Wrmsr` /
    /// `Hypercall` / `Cpuid` exit still pending. Fail closed: resuming such an
    /// exit without its completion would silently mis-service the guest.
    #[error("exit awaiting completion before resume")]
    PendingCompletion,

    /// A read-style completion (`complete_read`) was called with no matching
    /// read-style exit pending.
    #[error("no pending read/hypercall exit to complete")]
    NoPendingRead,

    /// A completion method did not match the pending exit (e.g. `complete_fault`
    /// on a pending `Io`, or `complete_ok` on a `Cpuid`).
    #[error("completion does not match the pending exit")]
    BadCompletion,

    /// `restore` was given a malformed or incompatible `VcpuState`.
    #[error("invalid vcpu state for restore")]
    InvalidState,

    /// KVM reported `KVM_EXIT_INTERNAL_ERROR` / `KVM_EXIT_FAIL_ENTRY`, or an
    /// otherwise-unhandled raw exit reason — fail closed, never silently
    /// continue. The `&'static str` adds context for the report.
    #[error("backend internal error: {0}")]
    Internal(&'static str),

    /// An underlying ioctl/syscall failed (carries the OS errno). Portable across
    /// platforms so vmm-core handles `KvmBackend` and `MockBackend` uniformly.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
