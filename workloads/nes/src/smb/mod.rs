// SPDX-License-Identifier: AGPL-3.0-or-later

use machine::quicknes::QuickNesMachine;
pub use nes_games::smb::archive;
pub mod campaign;
pub use nes_games::smb::game;
pub use nes_games::smb::plan;
pub mod target;

pub type SmbStrategyGame = nes_games::workload::NesWorkload<game::SuperMarioBros, QuickNesMachine>;
