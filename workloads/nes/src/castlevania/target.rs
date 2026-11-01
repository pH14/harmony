// SPDX-License-Identifier: AGPL-3.0-or-later

use machine::quicknes::QuickNesMachine;

pub use nes_games::castlevania::target::*;

pub type CastlevaniaTarget<M = QuickNesMachine> =
    nes_games::castlevania::target::CastlevaniaTarget<M>;
