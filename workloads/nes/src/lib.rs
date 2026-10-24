// SPDX-License-Identifier: AGPL-3.0-or-later

pub use nes_games::{chord, nes_backend};
pub use searcher::{search, target};
pub mod admission;
pub mod allocator;
pub mod castlevania;
pub mod donkey_kong;
pub mod eval;
pub mod famidash;
pub mod film;
#[cfg(unix)]
pub mod live;
pub mod prepare;
pub mod punchout;
pub mod smb;
pub mod witness;
pub mod zelda;

pub mod package;

pub mod stb;

pub mod twin_dragons;

pub mod mm2;

pub mod metroid;
