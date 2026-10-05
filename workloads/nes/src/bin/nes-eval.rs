// SPDX-License-Identifier: AGPL-3.0-or-later

use nes_workload::{
    eval::{Result, evaluate, run_cli},
    metroid::{
        campaign::{MetroidCampaignRun, MetroidGame},
        target::{GenesisDepth, MetroidInput, MetroidTerminalPolicy},
    },
    mm2::{
        campaign::{Mm2CampaignRun, Mm2Game},
        target::{Mm2Input, Mm2Stage},
    },
    nova::{
        campaign::{NovaCampaignRun, NovaGame},
        target::NovaLevel,
    },
    search::campaign::TargetExecution,
    smb::campaign::{SmbCampaignRun, SmbGame, SmbTerminalPredicate},
    stb::{
        campaign::{StbCampaignRun, StbGame},
        target::StbAi,
    },
};
use std::{fs, io::BufReader, path::Path};

fn metroid_game(
    rom: &[u8],
    core_path: &Path,
    core_sha256: &str,
    root_input: Option<&Path>,
) -> Result<MetroidGame> {
    let Some(root_input) = root_input else {
        return Ok(MetroidGame::new(rom, core_path, core_sha256));
    };
    let input: MetroidInput = serde_json::from_reader(BufReader::new(fs::File::open(root_input)?))?;
    if input.actions.is_empty() {
        return Err("root input carries no actions".into());
    }
    let mut prefix = MetroidGame::new(rom, core_path, core_sha256)
        .new_target()?
        .genesis_prefix()
        .to_vec();
    prefix.extend(input.actions.iter().copied());
    Ok(MetroidGame::new_rooted(
        rom,
        core_path,
        core_sha256,
        prefix,
        GenesisDepth::Rooted,
    ))
}

fn root_actions<I: serde::de::DeserializeOwned>(root_input: Option<&Path>) -> Result<Option<I>> {
    root_input
        .map(|path| Ok(serde_json::from_reader(BufReader::new(fs::File::open(path)?))?))
        .transpose()
}

fn main() -> Result<()> {
    nes_workload::allocator::use_one_malloc_arena();
    run_cli(|request, rom, out, started| {
        match request.game.as_str() {
            "smb" | "metroid"
                if request.level.is_some()
                    || request.stage.is_some()
                    || request.ai.is_some()
                    || request.whole_game =>
            {
                return Err("this game takes no level, stage, ai or whole_game option".into());
            }
            "nova"
                if request.stage.is_some()
                    || request.ai.is_some()
                    || (request.whole_game && request.level.unwrap_or(1) != 1) =>
            {
                return Err(
                "Nova whole-game evaluation must start at level 1; stage and ai are unsupported"
                    .into(),
            );
            }
            "mm2"
                if request.level.is_some()
                    || request.ai.is_some()
                    || (request.whole_game && request.stage.is_some())
                    || (!request.whole_game && request.root_input.is_some()) =>
            {
                return Err(
                    "MM2 takes a stage, or whole_game with an optional root_input".into(),
                );
            }
            "stb" if request.level.is_some() || request.stage.is_some() || request.whole_game => {
                return Err("STB evaluation takes only an ai option".into());
            }
            _ => {}
        }
        let p = &request.core;
        let h = &request.core_sha256;
        match request.game.as_str() {
            "smb" => evaluate(
                SmbGame::new(&rom, p, h),
                SmbCampaignRun {
                    vocabulary: Default::default(),
                    terminal: Some(SmbTerminalPredicate::GameVictory),
                },
                &request,
                &out,
                started,
            ),
            "nova" => {
                let game = NovaGame::new_at_level(
                    &rom,
                    p,
                    h,
                    NovaLevel::from_number(request.level.unwrap_or(1))?,
                );
                evaluate(
                    if request.whole_game {
                        game.with_whole_game()
                    } else {
                        game
                    },
                    NovaCampaignRun,
                    &request,
                    &out,
                    started,
                )
            }
            "mm2" if request.whole_game => evaluate(
                Mm2Game::new_whole_game(
                    &rom,
                    p,
                    h,
                    root_actions::<Mm2Input>(request.root_input.as_deref())?
                        .map(|input| input.actions)
                        .unwrap_or_default(),
                )
                .with_milestone_input_dir(out.join("milestone-inputs")),
                Mm2CampaignRun,
                &request,
                &out,
                started,
            ),
            "mm2" => evaluate(
                Mm2Game::new_at_stage(
                    &rom,
                    p,
                    h,
                    Mm2Stage::from_number(request.stage.unwrap_or(0))?,
                ),
                Mm2CampaignRun,
                &request,
                &out,
                started,
            ),
            "metroid" => evaluate(
                metroid_game(&rom, p, h, request.root_input.as_deref())?
                    .with_milestone_input_dir(out.join("milestone-inputs"))
                    .with_terminal_policy(match request.metroid_terminal.as_deref() {
                        Some(identifier) => MetroidTerminalPolicy::parse(identifier)?,
                        None => MetroidTerminalPolicy::default(),
                    }),
                MetroidCampaignRun,
                &request,
                &out,
                started,
            ),
            "stb" => {
                let ai = match request.ai.as_deref().unwrap_or("hard") {
                    "easy" => StbAi::Easy,
                    "fair" => StbAi::Fair,
                    "hard" => StbAi::Hard,
                    _ => return Err("unknown STB difficulty".into()),
                };
                evaluate(
                    StbGame::with_ai(&rom, p, h, ai),
                    StbCampaignRun,
                    &request,
                    &out,
                    started,
                )
            }
            _ => Err("unsupported game".into()),
        }
    })
}
