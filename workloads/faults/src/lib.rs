// SPDX-License-Identifier: AGPL-3.0-or-later

pub mod archive;
pub mod bundle;
pub mod package;
pub mod prepare;
pub mod report;
pub mod target;

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
pub mod campaign;
#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
pub mod consonance;

pub use bundle::FaultVocabulary;
pub use package::{Artifacts, Options, RecordedActions, Report, parse_recorded_input};
pub use target::{DEFAULT_HORIZON_NANOS, FaultAction, MAX_FAULT_ACTIONS};
