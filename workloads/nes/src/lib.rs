// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(test)]
#[global_allocator]
static TEST_ALLOCATOR: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

pub use searcher::{search, target};
pub mod admission;
pub mod allocator;
pub mod chord;
pub mod eval;
pub mod film;
pub mod nes_backend;
pub mod nova;
pub mod prepare;
pub mod smb;
pub mod witness;

pub mod package;

pub mod stb;

pub mod mm2;

pub mod metroid;
