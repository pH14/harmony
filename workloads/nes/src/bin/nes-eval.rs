// SPDX-License-Identifier: AGPL-3.0-or-later

use nes_games::arcade::{ArcadeGame, famidash};
use nes_workload::{
    castlevania::campaign::{CastlevaniaCampaignRun, CastlevaniaGame},
    eval::{Request, Result, evaluate, run_cli},
    famidash::{
        campaign::{FamidashCampaignRun, FamidashGame},
        levels::{GROUPS, Side, Stage},
    },
    live::{MemoryView, NativeMachine, evaluate_live, evaluate_watching},
    metroid::{
        campaign::{MetroidCampaignRun, MetroidGame},
        target::{GenesisDepth, MetroidInput, MetroidTerminalPolicy},
    },
    mm2::{
        campaign::{Mm2CampaignRun, Mm2Game},
        target::{Mm2Input, Mm2Stage},
    },
    punchout::campaign::{PunchOutCampaignRun, PunchOutGame},
    search::campaign::TargetExecution,
    smb::campaign::{SmbCampaignRun, SmbGame, SmbTerminalPredicate},
    stb::{
        campaign::{StbCampaignRun, StbGame},
        target::StbAi,
    },
    twin_dragons::campaign::{TwinDragonsCampaignRun, TwinDragonsGame},
    zelda::campaign::{ZeldaCampaignRun, ZeldaGame},
};
use std::{fs, io::BufReader, path::Path, time::Instant};

fn metroid_game(
    rom: &[u8],
    core_path: &Path,
    core_sha256: &str,
    execution_sha256: &str,
    root_input: Option<&Path>,
) -> Result<MetroidGame> {
    let Some(root_input) = root_input else {
        return Ok(MetroidGame::new(
            rom,
            core_path,
            core_sha256,
            execution_sha256,
        ));
    };
    let input: MetroidInput = serde_json::from_reader(BufReader::new(fs::File::open(root_input)?))?;
    if input.actions.is_empty() {
        return Err("root input carries no actions".into());
    }
    let mut prefix = MetroidGame::new(rom, core_path, core_sha256, execution_sha256)
        .new_target()?
        .genesis_prefix()
        .to_vec();
    prefix.extend(input.actions.iter().copied());
    Ok(MetroidGame::new_rooted(
        rom,
        core_path,
        core_sha256,
        execution_sha256,
        prefix,
        GenesisDepth::Rooted,
    ))
}

fn root_actions<I: serde::de::DeserializeOwned>(root_input: Option<&Path>) -> Result<Option<I>> {
    root_input
        .map(|path| {
            Ok(serde_json::from_reader(BufReader::new(fs::File::open(
                path,
            )?))?)
        })
        .transpose()
}

type LiveFamidash<const GROUP: usize> =
    nes_games::famidash::campaign::FamidashGame<NativeMachine, GROUP>;

fn live(request: &Request, rom: &[u8], out: &Path, started: Instant) -> Result<()> {
    if request.stage.is_some()
        || request.ai.is_some()
        || request.root_input.is_some()
        || request.metroid_terminal.is_some()
        || request.resume.is_some()
        || !request.mixture.is_empty()
    {
        return Err(
            "a live view runs the arcade game from genesis with its own mixture, without stage, ai, root, resume or mixture options"
                .into(),
        );
    }
    match request.game.as_str() {
        "smb" => {
            evaluate_live::<SmbGame<NativeMachine>>(rom, request, out, started, "super-mario-bros")
        }
        "mm2" => evaluate_live::<Mm2Game<NativeMachine>>(rom, request, out, started, "mega-man-2"),
        "metroid" => {
            evaluate_live::<MetroidGame<NativeMachine>>(rom, request, out, started, "metroid")
        }
        "famidash" => evaluate_live::<LiveFamidash<0>>(rom, request, out, started, "famidash"),
        "famidash-community" => {
            evaluate_live::<LiveFamidash<1>>(rom, request, out, started, "famidash-community")
        }
        "famidash-b" => evaluate_live::<LiveFamidash<2>>(rom, request, out, started, "famidash-b"),
        "famidash-c" => evaluate_live::<LiveFamidash<3>>(rom, request, out, started, "famidash-c"),
        "famidash-d" => evaluate_live::<LiveFamidash<4>>(rom, request, out, started, "famidash-d"),
        "castlevania" => evaluate_live::<
            nes_games::castlevania::campaign::CastlevaniaGame<NativeMachine>,
        >(rom, request, out, started, "castlevania"),
        "punchout" => evaluate_live::<nes_games::punchout::campaign::PunchOutGame<NativeMachine>>(
            rom, request, out, started, "punchout",
        ),
        "twin-dragons" => evaluate_live::<
            nes_games::twin_dragons::campaign::TwinDragonsGame<NativeMachine>,
        >(rom, request, out, started, "twin-dragons"),
        "zelda" => evaluate_live::<nes_games::zelda::campaign::ZeldaGame<NativeMachine>>(
            rom, request, out, started, "zelda",
        ),
        _ => Err("this game has no live view".into()),
    }
}

fn famidash_live(stage: Stage) -> (String, MemoryView<famidash::View>) {
    let group = GROUPS
        .iter()
        .position(|group| group.contains(stage.side, stage.level))
        .unwrap_or(0);
    let id = match GROUPS[group].id {
        "main" => "famidash".to_owned(),
        id => format!("famidash-{id}"),
    };
    let view = match group {
        1 => <LiveFamidash<1>>::memory_view,
        2 => <LiveFamidash<2>>::memory_view,
        3 => <LiveFamidash<3>>::memory_view,
        4 => <LiveFamidash<4>>::memory_view,
        _ => <LiveFamidash<0>>::memory_view,
    };
    (id, view)
}

fn famidash_stage(rom: &[u8], stage: Option<u8>, whole_game: bool) -> Result<Stage> {
    let side = Side::from_rom(rom).ok_or("the ROM is not a pinned Famidash 1.2.8 release")?;
    let level = stage.unwrap_or(0);
    Ok(if whole_game {
        Stage::through_group(side, level)?
    } else {
        Stage::single(side, level)?
    })
}

fn main() -> Result<()> {
    nes_workload::allocator::use_one_malloc_arena();
    run_cli(|request, rom, out, started| {
        if request.live.is_some() {
            return live(&request, &rom, &out, started);
        }
        match request.game.as_str() {
            "smb" | "metroid"
                if request.stage.is_some() || request.ai.is_some() || request.whole_game =>
            {
                return Err("this game takes no stage, ai or whole_game option".into());
            }
            "mm2"
                if request.ai.is_some()
                    || (request.whole_game && request.stage.is_some())
                    || (!request.whole_game && request.root_input.is_some()) =>
            {
                return Err("MM2 takes a stage, or whole_game with an optional root_input".into());
            }
            "stb" if request.stage.is_some() || request.whole_game => {
                return Err("STB evaluation takes only an ai option".into());
            }
            "twin-dragons" | "castlevania" | "punchout" | "zelda"
                if request.stage.is_some() || request.ai.is_some() || request.whole_game =>
            {
                return Err("this game takes no stage, ai or whole_game option".into());
            }
            "famidash"
                if request.ai.is_some() || (request.stage.is_none() && !request.whole_game) =>
            {
                return Err("Famidash takes a stage, whole_game, or both".into());
            }
            _ => {}
        }
        let p = &request.core;
        let h = &request.core_sha256;
        let execution_sha256 = machine::quicknes::prepared_execution_sha256(p, h)?;
        match request.game.as_str() {
            "smb" => evaluate_watching(
                SmbGame::new(&rom, p, h, &execution_sha256),
                SmbCampaignRun {
                    vocabulary: Default::default(),
                    terminal: Some(SmbTerminalPredicate::GameVictory),
                },
                &rom,
                &request,
                &execution_sha256,
                &out,
                started,
                "super-mario-bros",
                <SmbGame>::memory_view,
            ),
            "mm2" if request.whole_game => evaluate_watching(
                Mm2Game::new_whole_game(
                    &rom,
                    p,
                    h,
                    &execution_sha256,
                    root_actions::<Mm2Input>(request.root_input.as_deref())?
                        .map(|input| input.actions)
                        .unwrap_or_default(),
                )
                .with_milestone_input_dir(out.join("milestone-inputs")),
                Mm2CampaignRun,
                &rom,
                &request,
                &execution_sha256,
                &out,
                started,
                "mega-man-2",
                <Mm2Game>::memory_view,
            ),
            "mm2" => evaluate_watching(
                Mm2Game::new_at_stage(
                    &rom,
                    p,
                    h,
                    &execution_sha256,
                    Mm2Stage::from_number(request.stage.unwrap_or(0))?,
                ),
                Mm2CampaignRun,
                &rom,
                &request,
                &execution_sha256,
                &out,
                started,
                "mega-man-2",
                <Mm2Game>::memory_view,
            ),
            "metroid" => evaluate_watching(
                metroid_game(&rom, p, h, &execution_sha256, request.root_input.as_deref())?
                    .with_milestone_input_dir(out.join("milestone-inputs"))
                    .with_terminal_policy(match request.metroid_terminal.as_deref() {
                        Some(identifier) => MetroidTerminalPolicy::parse(identifier)?,
                        None => MetroidTerminalPolicy::default(),
                    }),
                MetroidCampaignRun,
                &rom,
                &request,
                &execution_sha256,
                &out,
                started,
                "metroid",
                <MetroidGame>::memory_view,
            ),
            "stb" => {
                let ai = match request.ai.as_deref().unwrap_or("hard") {
                    "easy" => StbAi::Easy,
                    "fair" => StbAi::Fair,
                    "hard" => StbAi::Hard,
                    _ => return Err("unknown STB difficulty".into()),
                };
                evaluate(
                    StbGame::with_ai(&rom, p, h, &execution_sha256, ai),
                    StbCampaignRun,
                    &request,
                    &execution_sha256,
                    &out,
                    started,
                )
            }
            "twin-dragons" => evaluate_watching(
                TwinDragonsGame::new(&rom, p, h, &execution_sha256),
                TwinDragonsCampaignRun,
                &rom,
                &request,
                &execution_sha256,
                &out,
                started,
                "twin-dragons",
                <TwinDragonsGame>::memory_view,
            ),
            "castlevania" => evaluate_watching(
                CastlevaniaGame::new(&rom, p, h, &execution_sha256)
                    .with_milestone_input_dir(out.join("milestone-inputs")),
                CastlevaniaCampaignRun,
                &rom,
                &request,
                &execution_sha256,
                &out,
                started,
                "castlevania",
                <CastlevaniaGame>::memory_view,
            ),
            "punchout" => evaluate_watching(
                PunchOutGame::new(&rom, p, h, &execution_sha256),
                PunchOutCampaignRun,
                &rom,
                &request,
                &execution_sha256,
                &out,
                started,
                "punchout",
                <PunchOutGame>::memory_view,
            ),
            "zelda" => evaluate_watching(
                ZeldaGame::new(&rom, p, h, &execution_sha256),
                ZeldaCampaignRun,
                &rom,
                &request,
                &execution_sha256,
                &out,
                started,
                "zelda",
                <ZeldaGame>::memory_view,
            ),
            "famidash" => {
                let stage = famidash_stage(&rom, request.stage, request.whole_game)?;
                let (id, view) = famidash_live(stage);
                evaluate_watching(
                    FamidashGame::new(&rom, p, h, &execution_sha256, stage),
                    FamidashCampaignRun,
                    &rom,
                    &request,
                    &execution_sha256,
                    &out,
                    started,
                    &id,
                    view,
                )
            }
            _ => Err("unsupported game".into()),
        }
    })
}
