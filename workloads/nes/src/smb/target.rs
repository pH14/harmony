// SPDX-License-Identifier: AGPL-3.0-or-later

use machine::quicknes::QuickNesMachine;

pub use nes_games::smb::target::*;

pub type SmbTarget<M = QuickNesMachine, P = Vec<u8>> = nes_games::smb::target::SmbTarget<M, P>;
