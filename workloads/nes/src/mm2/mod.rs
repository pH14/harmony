// SPDX-License-Identifier: AGPL-3.0-or-later

use machine::quicknes::QuickNesMachine;
pub use nes_games::mm2::{MegaMan2, action, archive, game, progress};
use nes_games::workload::NesWorkload;

pub mod campaign;
pub mod target;

pub type Mm2Workload = NesWorkload<MegaMan2, QuickNesMachine>;
