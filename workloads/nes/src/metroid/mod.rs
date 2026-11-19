// SPDX-License-Identifier: AGPL-3.0-or-later

use machine::quicknes::QuickNesMachine;
pub use nes_games::metroid::{action, archive, game, keying, plan, progress};
pub mod campaign;
pub mod target;

pub type MetroidStrategyGame = nes_games::workload::NesWorkload<game::Metroid, QuickNesMachine>;
