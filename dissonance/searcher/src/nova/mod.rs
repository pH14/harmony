// SPDX-License-Identifier: AGPL-3.0-or-later

//! Nova the Squirrel adapter over the game-neutral Dissonance searcher.

pub mod archive;
pub mod campaign;
#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    target_arch = "x86_64",
    not(miri)
))]
pub mod consonance;
pub mod target;
