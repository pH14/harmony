// SPDX-License-Identifier: AGPL-3.0-or-later

use machine::quicknes::QuickNesMachine;
pub use nes_games::famidash::game::*;
pub use nes_games::famidash::{game, levels};
use nes_games::workload::NesWorkload;

pub type FamidashGame = NesWorkload<Famidash, QuickNesMachine>;
