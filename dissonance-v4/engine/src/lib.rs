// SPDX-License-Identifier: AGPL-3.0-or-later

pub mod archive;
pub mod campaign;
pub mod fenwick;
pub mod report;
pub mod rng;
pub mod target;

#[global_allocator]
static ALLOCATOR: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[cfg(test)]
mod toy;
