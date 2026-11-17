// SPDX-License-Identifier: AGPL-3.0-or-later

use machine::quicknes::QuickNesMachine;
use nes_games::workload::NesWorkload;
pub use nes_games::zelda::*;

pub type ZeldaGame = NesWorkload<Zelda, QuickNesMachine>;
