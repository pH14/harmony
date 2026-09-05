// SPDX-License-Identifier: AGPL-3.0-or-later

//! Fault-library adapter over the game-neutral Dissonance searcher.
//!
//! The searcher branches on process faults applied to a Consonance Linux guest
//! running a database workload under the guest fault agent. One action is one
//! environment delta relative to the parent's seal `Moment` plus a fixed
//! horizon: the delta is staged, the VM runs to the horizon deadline, and the
//! endpoint is snapshotted and observed. A run that stops on an SDK assertion
//! or a guest crash has found a bug.

pub mod archive;
pub mod bundle;
#[cfg(all(target_os = "linux", target_arch = "x86_64", not(miri)))]
pub mod campaign;
#[cfg(all(target_os = "linux", target_arch = "x86_64", not(miri)))]
pub mod consonance;
pub mod report;
pub mod target;
