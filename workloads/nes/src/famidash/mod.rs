// SPDX-License-Identifier: AGPL-3.0-or-later

use machine::quicknes::QuickNesMachine;
pub use nes_games::famidash::game::*;
pub use nes_games::famidash::lucky_draw::LuckyDraw;
pub use nes_games::famidash::{game, levels, lucky_draw};
use nes_games::workload::NesWorkload;

pub type FamidashGame = NesWorkload<Famidash, QuickNesMachine>;
pub type LuckyDrawGame = NesWorkload<LuckyDraw, QuickNesMachine>;
