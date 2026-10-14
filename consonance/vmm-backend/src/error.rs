// SPDX-License-Identifier: AGPL-3.0-or-later

pub type Result<T> = core::result::Result<T, BackendError>;

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("backend does not support: {what}")]
    Unsupported { what: &'static str },

    #[error("missing capability: {cap}")]
    Capability { cap: &'static str },

    #[error("backend not configured: set_cpuid + set_msr_filter required before run")]
    NotConfigured,

    #[error("memory mapping error: {0}")]
    Memory(&'static str),

    #[error("exit awaiting completion before resume")]
    PendingCompletion,

    #[error("no pending read/hypercall exit to complete")]
    NoPendingRead,

    #[error("completion does not match the pending exit")]
    BadCompletion,

    #[error("invalid vcpu state for restore")]
    InvalidState,

    #[error("non-init MXCSR without SSE xstate presence is not portable across KVM hosts")]
    NonportableXsaveMxcsr,

    #[error("AMD KVM cannot safely capture or continue a running 32-bit PAE guest")]
    AmdPaeSnapshotUnsafe,

    #[error("snapshot host core class {saved:?} differs from this backend's {current:?}")]
    HostCoreClassMismatch {
        saved: Option<crate::HostCoreClass>,
        current: Option<crate::HostCoreClass>,
    },

    #[error(
        "ID register {encoding:#06x} requests {requested:#018x}, above the host's {host:#018x}"
    )]
    IdRegisterAboveHost {
        encoding: u32,
        requested: u64,
        host: u64,
    },

    #[error("backend internal error: {0}")]
    Internal(&'static str),

    #[error("KVM_EXIT_FAIL_ENTRY: hardware reason {hardware_reason:#018x}, CPU {cpu}")]
    KvmEntryFailure { hardware_reason: u64, cpu: u32 },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
