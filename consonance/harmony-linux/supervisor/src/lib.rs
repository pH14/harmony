// SPDX-License-Identifier: AGPL-3.0-or-later

pub mod bundle;
pub mod evidence;
pub mod process;
pub mod reconcile;
pub mod recovery;
pub mod regs;
pub mod supervise;

pub use bundle::{Bundle, BundleError, HookSpec, NodeSpec, parse_bundle};
pub use evidence::CheckEvidence;
pub use reconcile::{ActiveWindows, EventKillWindow, EventPark, HookWindow, NodeActions};
pub use recovery::{RecoveryError, RecoveryReadiness};
pub use supervise::{Action, Counters, ProcessSupervisor, Supervisor};

pub const TICK_NANOS: u64 = 10_000_000;
pub const READY_TICKS: u32 = 6_000;
pub const ANTITHESIS_OUTPUT_DIR: &str = "/run/antithesis";
