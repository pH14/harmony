// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, path::Path};

use machine::quicknes::QuickNesMachine;

pub use nes_games::mm2::target::*;

pub type Mm2Target<M = QuickNesMachine> = nes_games::mm2::target::Mm2Target<M>;

pub fn target_from_args<'a>(
    args: &'a [String],
    rom: &[u8],
    core_path: &Path,
    core_sha256: &str,
) -> Result<(Mm2Target, &'a [String]), Box<dyn Error>> {
    let read = |path: &String| -> Result<Mm2Input, Box<dyn Error>> {
        Ok(serde_json::from_slice(&std::fs::read(path)?)?)
    };
    match args {
        [mode, option, path, rest @ ..] if mode == "whole-game" && option == "--root" => Ok((
            Mm2Target::whole_game(rom, core_path, core_sha256, &read(path)?.actions)?,
            rest,
        )),
        [mode, option, path, rest @ ..] if mode == "whole-game" && option == "--tape" => Ok((
            Mm2Target::whole_game_after_tape(rom, core_path, core_sha256, &read(path)?.actions)?,
            rest,
        )),
        [mode, rest @ ..] if mode == "whole-game" => Ok((
            Mm2Target::whole_game(rom, core_path, core_sha256, &[])?,
            rest,
        )),
        [stage, prefix, rest @ ..] => Ok((
            Mm2Target::from_rom_bytes_after(
                rom,
                core_path,
                core_sha256,
                &read(prefix)?.actions,
                Mm2Stage::parse(stage)?,
            )?,
            rest,
        )),
        _ => Err("expected <stage> <chain-prefix.json> or whole-game".into()),
    }
}
