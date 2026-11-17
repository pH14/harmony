// SPDX-License-Identifier: AGPL-3.0-or-later

use machine::quicknes::QuickNesMachine;
pub use nes_games::joust::*;
use nes_games::workload::NesWorkload;

pub type JoustGame = NesWorkload<Joust, QuickNesMachine>;
