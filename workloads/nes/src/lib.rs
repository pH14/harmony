// SPDX-License-Identifier: AGPL-3.0-or-later

//! NES workloads for deterministic search using native or Consonance execution.

pub use searcher::{search, target};
pub mod nes_backend;
pub mod nova;
pub mod prepare;
pub mod smb;

pub mod package;

pub mod stb;

pub mod mm2;

pub mod metroid;
