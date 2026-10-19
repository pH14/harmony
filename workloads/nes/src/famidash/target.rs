// SPDX-License-Identifier: AGPL-3.0-or-later

use machine::quicknes::QuickNesMachine;

pub use nes_games::famidash::target::*;

pub type FamidashTarget<M = QuickNesMachine> = nes_games::famidash::target::FamidashTarget<M>;
