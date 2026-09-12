// SPDX-License-Identifier: AGPL-3.0-or-later

pub mod bundle;
pub mod directive;
pub mod process;
pub mod reconcile;
pub mod regs;
pub mod supervise;

pub use bundle::{Bundle, BundleError, HookSpec, NodeSpec, parse_bundle};
pub use directive::{Directive, DirectiveError, LineReader, parse_directive};
pub use reconcile::{ActiveWindows, HookWindow, NodeActions, Park};
pub use supervise::{Action, Counters, ProcessSupervisor, Supervisor};

pub const TICK_NANOS: u64 = 10_000_000;
pub const READY_TICKS: u32 = 6_000;
