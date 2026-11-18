// SPDX-License-Identifier: AGPL-3.0-or-later

use machine::quicknes::QuickNesMachine;
pub use nes_games::twin_dragons::{TwinDragons, TwinDragonsState, archive, game};
use nes_games::workload::NesWorkload;
pub mod campaign;
pub mod target;

pub type TwinDragonsWorkload = NesWorkload<TwinDragons, QuickNesMachine>;
