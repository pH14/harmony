// SPDX-License-Identifier: AGPL-3.0-or-later

use machine::quicknes::QuickNesMachine;

pub use nes_games::metroid::target::*;

pub type MetroidTarget<M = QuickNesMachine> = nes_games::metroid::target::MetroidTarget<M>;
