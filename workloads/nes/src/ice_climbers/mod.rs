// SPDX-License-Identifier: AGPL-3.0-or-later

use machine::quicknes::QuickNesMachine;
pub use nes_games::ice_climbers::{IceClimbers, IceClimbersState, archive, game, target};
use nes_games::workload::NesWorkload;
pub mod campaign;

pub type IceClimbersWorkload = NesWorkload<IceClimbers, QuickNesMachine>;
