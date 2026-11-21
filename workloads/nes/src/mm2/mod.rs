// SPDX-License-Identifier: AGPL-3.0-or-later

use machine::quicknes::QuickNesMachine;
pub use nes_games::mm2::*;
use nes_games::workload::NesWorkload;

pub type Mm2Workload = NesWorkload<MegaMan2, QuickNesMachine>;
