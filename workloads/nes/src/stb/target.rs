// SPDX-License-Identifier: AGPL-3.0-or-later

use machine::quicknes::QuickNesMachine;

pub use nes_games::stb::target::*;

pub type StbTarget<M = QuickNesMachine> = nes_games::stb::target::StbTarget<M>;
