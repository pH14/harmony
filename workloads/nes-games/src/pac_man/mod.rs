// SPDX-License-Identifier: AGPL-3.0-or-later

pub mod archive;
pub mod campaign;
#[cfg(all(test, unix))]
mod equivalence_tests;
pub mod game;
pub mod target;

pub use game::{PacMan, PacManState};
