// SPDX-License-Identifier: AGPL-3.0-or-later

use nes_games::arcade::{ArcadeGame, famidash};
use nes_workload::{
    castlevania::campaign::{CastlevaniaCampaignRun, CastlevaniaGame},
    donkey_kong::{DonkeyKong, DonkeyKongWorkload},
    eval::{Request, Result, evaluate, run_cli},
    famidash::{
        Famidash, FamidashGame,
        levels::{GROUPS, Side, Stage},
    },
    ice_climbers::{IceClimbers, IceClimbersWorkload},
    joust::{Joust, JoustGame},
    live::{MemoryView, NativeMachine, evaluate_live, evaluate_watching},
    metroid::{
        campaign::{MetroidCampaignRun, MetroidGame},
        target::{GenesisDepth, MetroidInput, MetroidTerminalPolicy},
    },
    mm2::{
        campaign::{Mm2CampaignRun, Mm2Game},
        target::{Mm2Input, Mm2Stage},
    },
    pac_man::{PacMan, PacManWorkload},
    punchout::{PunchOut, PunchOutWorkload},
    search::campaign::TargetExecution,
    smb::{
        archive::{SmbCoverage, SmbCoverageIds, SmbCoverageTerm},
        campaign::{SmbCampaignRun, SmbGame, SmbTerminalPredicate},
        plan::SmbRoute,
    },
    stb::{
        campaign::{StbCampaignRun, StbGame},
        target::StbAi,
    },
    twin_dragons::{TwinDragons, TwinDragonsWorkload},
    zelda::{Zelda, ZeldaGame},
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
        .genesis_actions();
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

fn ice_climbers_root(root_input: Option<&Path>) -> Result<Option<Vec<machine::nes::ButtonChord>>> {
    match root_actions::<nes_workload::search::archive::Input<machine::nes::ButtonChord>>(
        root_input,
    )? {
        Some(input) if input.actions.is_empty() => Err("root input carries no actions".into()),
        Some(input) => Ok(Some(input.actions)),
        None => Ok(None),
    }
}

type LiveFamidash<const GROUP: usize> =
    nes_games::workload::NesWorkload<Famidash<GROUP>, NativeMachine>;

fn live(request: &Request, rom: &[u8], out: &Path, started: Instant) -> Result<()> {
    if (request.stage.is_some() && !request.game.starts_with("famidash"))
        || request.ai.is_some()
        || request.root_input.is_some()
        || request.metroid_terminal.is_some()
        || request.smb_route.is_some()
        || request.smb_terminal.is_some()
        || request.smb_coverage_cap.is_some()
        || request.smb_coverage_ids.is_some()
        || request.smb_coverage_new_ids
        || request.minimize
        || request.resume.is_some()
        || request.resume_states.is_some()
        || !request.mixture.is_empty()
        || request.strategy.is_some()
        || request.records
        || request.keep_stream
    {
        return Err(
            "a live view runs the arcade game from genesis with its own mixture and built-in strategy, without ai, root, minimize, resume, mixture, strategy, records or keep_stream options"
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
        "punchout" => evaluate_live::<nes_games::workload::NesWorkload<PunchOut, NativeMachine>>(
            rom,
            request,
            out,
            started,
            "punch-out",
        ),
        "twin-dragons" => evaluate_live::<
            nes_games::workload::NesWorkload<TwinDragons, NativeMachine>,
        >(rom, request, out, started, "twin-dragons"),
        "zelda" => evaluate_live::<nes_games::workload::NesWorkload<Zelda, NativeMachine>>(
            rom, request, out, started, "zelda",
        ),
        "donkey-kong" => {
            evaluate_live::<nes_games::workload::NesWorkload<DonkeyKong, NativeMachine>>(
                rom,
                request,
                out,
                started,
                "donkey-kong",
            )
        }
        "ice-climbers" => evaluate_live::<
            nes_games::workload::NesWorkload<IceClimbers, NativeMachine>,
        >(rom, request, out, started, "ice-climbers"),
        "pac-man" => evaluate_live::<nes_games::workload::NesWorkload<PacMan, NativeMachine>>(
            rom, request, out, started, "pac-man",
        ),
        "joust" => evaluate_live::<nes_games::workload::NesWorkload<Joust, NativeMachine>>(
            rom, request, out, started, "joust",
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
        if (request.smb_route.is_some()
            || request.smb_terminal.is_some()
            || request.smb_coverage_cap.is_some()
            || request.smb_coverage_ids.is_some()
            || request.smb_coverage_new_ids)
            && request.game != "smb"
        {
            return Err(
                "only Super Mario Bros. takes smb_route, smb_terminal and smb_coverage options"
                    .into(),
            );
        }
        if request.smb_coverage_new_ids && request.smb_coverage_cap.is_some() {
            return Err("smb_coverage_new_ids and smb_coverage_cap are two terms; take one".into());
        }
        if request.smb_coverage_ids.is_some()
            && request.smb_coverage_cap.is_none()
            && !request.smb_coverage_new_ids
        {
            return Err("smb_coverage_ids needs smb_coverage_cap or smb_coverage_new_ids".into());
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
            "twin-dragons" | "castlevania" | "punchout" | "zelda" | "donkey-kong"
            | "ice-climbers" | "pac-man" | "joust"
                if request.stage.is_some() || request.ai.is_some() || request.whole_game =>
            {
                return Err("this game takes no stage, ai or whole_game option".into());
            }
            "famidash"
                if request.ai.is_some() || (request.stage.is_none() && !request.whole_game) =>
            {
                return Err("Famidash takes a stage, whole_game, or both".into());
            }
            "joust" | "zelda" | "famidash" | "donkey-kong" | "pac-man" | "punchout"
            | "ice-climbers" | "twin-dragons" => {}
            _ if request.strategy.is_some() || request.records => {
                return Err(
                    "only games declared through assertions take a strategy or records".into(),
                );
            }
            _ => {}
        }
        let p = &request.core;
        let h = &request.core_sha256;
        let execution_sha256 = machine::quicknes::prepared_execution_sha256(p, h)?;
        match request.game.as_str() {
            "smb" => evaluate_watching(
                {
                    let game = SmbGame::new(&rom, p, h, &execution_sha256)
                        .with_root(
                            root_actions::<nes_games::smb::target::SmbInput>(
                                request.root_input.as_deref(),
                            )?
                            .unwrap_or_default(),
                        )
                        .with_route(
                            request
                                .smb_route
                                .as_deref()
                                .map(SmbRoute::from_identifier)
                                .transpose()?
                                .unwrap_or_default(),
                        );
                    let term = match (request.smb_coverage_cap, request.smb_coverage_new_ids) {
                        (Some(cap), _) => Some(SmbCoverageTerm::Rarity { cap }),
                        (None, true) => Some(SmbCoverageTerm::NewId),
                        (None, false) => None,
                    };
                    match term {
                        Some(term) => game.with_coverage(SmbCoverage {
                            term,
                            ids: request
                                .smb_coverage_ids
                                .as_deref()
                                .map(SmbCoverageIds::from_identifier)
                                .transpose()?
                                .unwrap_or_default(),
                        }),
                        None => game,
                    }
                },
                SmbCampaignRun {
                    vocabulary: Default::default(),
                    terminal: Some(
                        request
                            .smb_terminal
                            .as_deref()
                            .map(SmbTerminalPredicate::from_identifier)
                            .transpose()?
                            .unwrap_or_default(),
                    ),
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
                nes_workload::workload::configured(
                    TwinDragonsWorkload::new(TwinDragons, &rom, p, h, &execution_sha256)?,
                    &request,
                    &out,
                )?,
                nes_games::workload::NesRun,
                &rom,
                &request,
                &execution_sha256,
                &out,
                started,
                "twin-dragons",
                <TwinDragonsWorkload>::memory_view,
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
                nes_workload::workload::configured(
                    PunchOutWorkload::new(PunchOut, &rom, p, h, &execution_sha256)?,
                    &request,
                    &out,
                )?,
                nes_games::workload::NesRun,
                &rom,
                &request,
                &execution_sha256,
                &out,
                started,
                "punch-out",
                <PunchOutWorkload>::memory_view,
            ),
            "zelda" => evaluate_watching(
                nes_workload::workload::configured(
                    ZeldaGame::new(Zelda, &rom, p, h, &execution_sha256)?,
                    &request,
                    &out,
                )?,
                nes_games::workload::NesRun,
                &rom,
                &request,
                &execution_sha256,
                &out,
                started,
                "zelda",
                <ZeldaGame>::memory_view,
            ),
            "donkey-kong" => evaluate_watching(
                nes_workload::workload::configured(
                    DonkeyKongWorkload::new(DonkeyKong, &rom, p, h, &execution_sha256)?,
                    &request,
                    &out,
                )?,
                nes_games::workload::NesRun,
                &rom,
                &request,
                &execution_sha256,
                &out,
                started,
                "donkey-kong",
                <DonkeyKongWorkload>::memory_view,
            ),
            "ice-climbers" => evaluate_watching(
                nes_workload::workload::configured(
                    IceClimbersWorkload::new(
                        ice_climbers_root(request.root_input.as_deref())?
                            .map_or_else(IceClimbers::default, IceClimbers::rooted),
                        &rom,
                        p,
                        h,
                        &execution_sha256,
                    )?,
                    &request,
                    &out,
                )?,
                nes_games::workload::NesRun,
                &rom,
                &request,
                &execution_sha256,
                &out,
                started,
                "ice-climbers",
                <IceClimbersWorkload>::memory_view,
            ),
            "pac-man" => evaluate_watching(
                nes_workload::workload::configured(
                    PacManWorkload::new(PacMan, &rom, p, h, &execution_sha256)?,
                    &request,
                    &out,
                )?,
                nes_games::workload::NesRun,
                &rom,
                &request,
                &execution_sha256,
                &out,
                started,
                "pac-man",
                <PacManWorkload>::memory_view,
            ),
            "joust" => evaluate_watching(
                nes_workload::workload::configured(
                    JoustGame::new(Joust, &rom, p, h, &execution_sha256)?,
                    &request,
                    &out,
                )?,
                nes_games::workload::NesRun,
                &rom,
                &request,
                &execution_sha256,
                &out,
                started,
                "joust",
                <JoustGame>::memory_view,
            ),
            "famidash" => {
                let stage = famidash_stage(&rom, request.stage, request.whole_game)?;
                let (id, view) = famidash_live(stage);
                evaluate_watching(
                    nes_workload::workload::configured(
                        FamidashGame::new(Famidash::new(stage), &rom, p, h, &execution_sha256)?,
                        &request,
                        &out,
                    )?,
                    nes_games::workload::NesRun,
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
