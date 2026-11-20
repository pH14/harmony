// SPDX-License-Identifier: AGPL-3.0-or-later

pub mod action;
pub mod archive;
pub mod campaign;
#[cfg(test)]
mod draw_check;
pub mod game;
pub mod progress;
pub mod target;

pub use game::{MegaMan2, Mm2State};
