// SPDX-License-Identifier: AGPL-3.0-or-later

use machine::quicknes::QuickNesMachine;

pub use nes_games::twin_dragons::target::*;

pub type TwinDragonsTarget<M = QuickNesMachine> =
    nes_games::twin_dragons::target::TwinDragonsTarget<M>;
