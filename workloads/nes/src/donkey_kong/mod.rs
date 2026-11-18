// SPDX-License-Identifier: AGPL-3.0-or-later

use machine::quicknes::QuickNesMachine;
use nes_games::workload::NesWorkload;
pub use nes_games::donkey_kong::{DonkeyKong, DonkeyKongState, archive, game, target};
pub mod campaign;

pub type DonkeyKongWorkload = NesWorkload<DonkeyKong, QuickNesMachine>;
