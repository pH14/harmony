// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::config::Result;
use nes_workload::package::{SearchEvidence, SearchOptions, SearchStart};
use std::path::Path;

pub trait Runner {
    fn replay(
        rom: &[u8],
        core: &Path,
        digest: &str,
        input: serde_json::Value,
    ) -> Result<serde_json::Value>;
    fn search(
        rom: &[u8],
        core: &Path,
        digest: &str,
        options: &SearchOptions,
        start: &SearchStart,
    ) -> Result<SearchEvidence>;
}

#[cfg(feature = "frontend")]
pub struct Native;
pub struct Guarded;

#[cfg(feature = "frontend")]
impl Runner for Native {
    fn replay(
        rom: &[u8],
        core: &Path,
        digest: &str,
        input: serde_json::Value,
    ) -> Result<serde_json::Value> {
        nes_workload::package::replay_native(rom, core, digest, input)
    }

    fn search(
        rom: &[u8],
        core: &Path,
        digest: &str,
        options: &SearchOptions,
        start: &SearchStart,
    ) -> Result<SearchEvidence> {
        nes_workload::package::search_native(rom, core, digest, options, start)
    }
}

impl Runner for Guarded {
    fn replay(_: &[u8], _: &Path, _: &str, _: serde_json::Value) -> Result<serde_json::Value> {
        Err("the guarded runtime cannot execute native QuickNES".into())
    }
    fn search(
        _: &[u8],
        _: &Path,
        _: &str,
        _: &SearchOptions,
        _: &SearchStart,
    ) -> Result<SearchEvidence> {
        Err("the guarded runtime cannot execute native QuickNES".into())
    }
}
