// SPDX-License-Identifier: AGPL-3.0-or-later

pub mod bundle;
pub mod directive;
pub mod faults;
pub mod recovery;
pub mod regs;
pub mod supervisor;

pub use bundle::{Bundle, BundleError, HookSpec, NodeSpec};
pub use directive::{Directive, DirectiveError, LineReader};
pub use faults::{ActiveFaults, NodeFaults, Park};
pub use recovery::{RecoveryError, RecoveryGate};
pub use regs::{RegisterSnapshot, Registers};
pub use supervisor::{Action, Counters, Supervisor};

pub const TICK_NANOS: u64 = 10_000_000;

pub trait Clock {
    type Error;

    fn wait(&mut self) -> Result<(), Self::Error>;
}
