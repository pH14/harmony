// SPDX-License-Identifier: AGPL-3.0-or-later

use machine::quicknes::QuickNesMachine;
pub use nes_games::twin_dragons::{TwinDragons, TwinDragonsState, game};
use nes_games::workload::NesWorkload;

pub type TwinDragonsWorkload = NesWorkload<TwinDragons, QuickNesMachine>;
