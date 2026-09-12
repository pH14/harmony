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

    #[error("backend internal error: {0}")]
    Internal(&'static str),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
