// SPDX-License-Identifier: AGPL-3.0-or-later

use nes_games::{
    arcade::{ArcadeGame, famidash},
    mm2::MegaMan2,
    workload::Shadow,
};
use nes_workload::{
    castlevania::campaign::{CastlevaniaCampaignRun, CastlevaniaGame},
    donkey_kong::{DonkeyKong, DonkeyKongWorkload},
    eval::{MILESTONE_INPUTS_DIR, Request, Result, evaluate, run_cli},
    famidash::{
        Famidash, FamidashGame, LuckyDraw, LuckyDrawGame,
        levels::{GROUPS, Side, Stage},
    },
    ice_climbers::{IceClimbers, IceClimbersWorkload},
    joust::{Joust, JoustGame},
    live::{MemoryView, NativeMachine, evaluate_live, evaluate_watching},
    metroid::{
        MetroidStrategyGame,
        archive::MetroidArchiveKey,
        campaign::{MetroidCampaignRun, MetroidGame},
        game::Metroid,
        keying::StrategyKeying,
        target::{GenesisDepth, MetroidInput, MetroidTerminalPolicy},
    },
    mm2::Mm2Workload,
    pac_man::{PacMan, PacManWorkload},
    punchout::{PunchOut, PunchOutWorkload},
    search::campaign::TargetExecution,
    smb::{
        SmbStrategyGame,
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

type LiveFamidash<const GROUP: usize> =
    nes_games::workload::NesWorkload<Famidash<GROUP>, NativeMachine>;

fn live(request: &Request, rom: &[u8], out: &Path, started: Instant) -> Result<()> {
    if (request.stage.is_some() && !request.game.starts_with("famidash"))
        || request.ai.is_some()
        || request.root_input.is_some()
        || request.metroid_terminal.is_some()
        || request.smb_route.is_some()
        || request.smb_terminal.is_some()
        || request.minimize
        || request.resume.is_some()
        || request.resume_states.is_some()
        || !request.mixture.is_empty()
        || request.strategy.is_some()
        || request.records
        || request.shadow
        || request.keep_stream
    {
        return Err(
            "a live view runs the arcade game from genesis with its own mixture and built-in strategy, without ai, root, minimize, resume, mixture, strategy, records, shadow or keep_stream options"
                .into(),
        );
    }
    match request.game.as_str() {
        "smb" => {
            evaluate_live::<SmbGame<NativeMachine>>(rom, request, out, started, "super-mario-bros")
        }
        "mm2-strategy" => {
            evaluate_live::<nes_games::workload::NesWorkload<MegaMan2, NativeMachine>>(
                rom,
                request,
                out,
                started,
                "mega-man-2",
            )
        }
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

const SHADOW_GAMES: &[&str] = &["smb", "metroid"];
const ROOT_GAMES: &[&str] = &["smb", "metroid"];
const STRATEGY_GAMES: &[&str] = &[
    "joust",
    "zelda",
    "famidash",
    "famidash-lucky-draw",
    "donkey-kong",
    "pac-man",
    "punchout",
    "ice-climbers",
    "twin-dragons",
    "smb-strategy",
    "mm2-strategy",
    "metroid-strategy",
];

fn root_input_taken(game: &str, root_input: bool) -> Result<()> {
    if root_input && !ROOT_GAMES.contains(&game) && !STRATEGY_GAMES.contains(&game) {
        return Err(format!(
            "{game} takes no root_input; the games that do are {} and every strategy game",
            ROOT_GAMES.join(", ")
        )
        .into());
    }
    Ok(())
}

fn main() -> Result<()> {
    nes_workload::allocator::use_one_malloc_arena();
    run_cli(|request, rom, out, started| {
        if request.live.is_some() {
            return live(&request, &rom, &out, started);
        }
        if (request.smb_route.is_some() || request.smb_terminal.is_some()) && request.game != "smb"
        {
            return Err("only Super Mario Bros. takes smb_route and smb_terminal options".into());
        }
        match request.game.as_str() {
            "smb" | "metroid"
                if request.stage.is_some() || request.ai.is_some() || request.whole_game =>
            {
                return Err("this game takes no stage, ai or whole_game option".into());
            }
            "stb" if request.stage.is_some() || request.whole_game => {
                return Err("STB evaluation takes only an ai option".into());
            }
            "twin-dragons"
            | "castlevania"
            | "punchout"
            | "zelda"
            | "donkey-kong"
            | "ice-climbers"
            | "pac-man"
            | "joust"
            | "famidash-lucky-draw"
            | "smb-strategy"
            | "metroid-strategy"
                if request.stage.is_some() || request.ai.is_some() || request.whole_game =>
            {
                return Err("this game takes no stage, ai or whole_game option".into());
            }
            "mm2-strategy"
                if request.ai.is_some()
                    || request.whole_game == request.stage.is_some()
                    || (request.stage.is_some() && request.root_input.is_some()) =>
            {
                return Err(
                    "the strategy MM2 takes a stage, or whole_game with an optional root_input"
                        .into(),
                );
            }
            "famidash"
                if request.ai.is_some() || (request.stage.is_none() && !request.whole_game) =>
            {
                return Err("Famidash takes a stage, whole_game, or both".into());
            }
            game if STRATEGY_GAMES.contains(&game) => {}
            "smb" if request.shadow && !request.records => {}
            "metroid" if request.shadow && request.strategy.is_some() => {
                return Err(
                    "a Metroid shadow keys the hand campaign, which takes no strategy".into(),
                );
            }
            "metroid" if request.records => {
                return Err(
                    "Metroid keys a strategy inside its own campaign and keeps no records".into(),
                );
            }
            "metroid" => {}
            _ if request.strategy.is_some() || request.records => {
                return Err(
                    "only games declared through assertions take a strategy or records".into(),
                );
            }
            _ => {}
        }
        root_input_taken(&request.game, request.root_input.is_some())?;
        if request.shadow && !SHADOW_GAMES.contains(&request.game.as_str()) {
            return Err("only the hand-keyed games with a shadow hook take a shadow key".into());
        }
        let p = &request.core;
        let h = &request.core_sha256;
        let execution_sha256 = machine::quicknes::prepared_execution_sha256(p, h)?;
        match request.game.as_str() {
            "smb" => {
                let route = request
                    .smb_route
                    .as_deref()
                    .map(SmbRoute::from_identifier)
                    .transpose()?
                    .unwrap_or_default();
                let shadow = if request.shadow {
                    let strategy = match &request.strategy {
                        Some(path) => fs::read_to_string(path)?,
                        None if route == SmbRoute::Warps => {
                            nes_games::smb::game::WARPS_STRATEGY.to_owned()
                        }
                        None => <nes_games::smb::game::SuperMarioBros as nes_games::workload::Game>::STRATEGY.to_owned(),
                    };
                    fs::create_dir_all(&out)?;
                    fs::write(out.join(nes_workload::workload::STRATEGY_FILE), &strategy)?;
                    Some(nes_games::workload::Shadow::with_strategy(
                        nes_games::smb::game::SuperMarioBros,
                        &strategy,
                    )?)
                } else {
                    None
                };
                let game = SmbGame::new(&rom, p, h, &execution_sha256)
                    .with_root(
                        root_actions::<nes_games::smb::target::SmbInput>(
                            request.root_input.as_deref(),
                        )?
                        .unwrap_or_default(),
                    )
                    .with_route(route);
                evaluate_watching(
                    match &shadow {
                        Some(shadow) => game.with_shadow(shadow.hook()),
                        None => game,
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
                )?;
                nes_workload::workload::write_shadow(shadow.as_ref(), &out)
            }
            "mm2-strategy" => evaluate_watching(
                mm2_strategy_workload(&request, &rom, &execution_sha256, &out)?,
                nes_games::workload::NesRun,
                &rom,
                &request,
                &execution_sha256,
                &out,
                started,
                "mega-man-2",
                <Mm2Workload>::memory_view,
            ),
            "metroid" => {
                let game =
                    metroid_game(&rom, p, h, &execution_sha256, request.root_input.as_deref())?
                        .with_milestone_input_dir(out.join(MILESTONE_INPUTS_DIR))
                        .with_terminal_policy(match request.metroid_terminal.as_deref() {
                            Some(identifier) => MetroidTerminalPolicy::parse(identifier)?,
                            None => MetroidTerminalPolicy::default(),
                        });
                match request.strategy.as_deref() {
                    Some(path) => evaluate_watching(
                        game.with_keying(StrategyKeying::new(&fs::read_to_string(path)?)?),
                        MetroidCampaignRun,
                        &rom,
                        &request,
                        &execution_sha256,
                        &out,
                        started,
                        "metroid",
                        <MetroidGame>::memory_view,
                    ),
                    None => {
                        let shadow = request
                            .shadow
                            .then(|| Shadow::<Metroid, MetroidArchiveKey>::new(Metroid))
                            .transpose()?;
                        let game = match &shadow {
                            Some(shadow) => game.with_shadow(shadow.state_hook()),
                            None => game,
                        };
                        evaluate_watching(
                            game,
                            MetroidCampaignRun,
                            &rom,
                            &request,
                            &execution_sha256,
                            &out,
                            started,
                            "metroid",
                            <MetroidGame>::memory_view,
                        )?;
                        nes_workload::workload::write_shadow(shadow.as_ref(), &out)
                    }
                }
            }
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
                    .with_milestone_input_dir(out.join(MILESTONE_INPUTS_DIR)),
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
                    IceClimbersWorkload::new(IceClimbers, &rom, p, h, &execution_sha256)?,
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
            "smb-strategy" => evaluate_watching(
                nes_workload::workload::configured(
                    SmbStrategyGame::new(
                        nes_games::smb::game::SuperMarioBros,
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
                "super-mario-bros",
                <SmbStrategyGame>::memory_view,
            ),
            "metroid-strategy" => evaluate_watching(
                nes_workload::workload::configured(
                    MetroidStrategyGame::new(Metroid, &rom, p, h, &execution_sha256)?,
                    &request,
                    &out,
                )?,
                nes_games::workload::NesRun,
                &rom,
                &request,
                &execution_sha256,
                &out,
                started,
                "metroid",
                <MetroidStrategyGame>::memory_view,
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
            "famidash-lucky-draw" => {
                if Side::from_rom(&rom) != Some(Side::C) {
                    return Err("Lucky Draw needs the pinned Famidash C-sides ROM".into());
                }
                evaluate_watching(
                    nes_workload::workload::configured(
                        LuckyDrawGame::new(LuckyDraw::default(), &rom, p, h, &execution_sha256)?,
                        &request,
                        &out,
                    )?,
                    nes_games::workload::NesRun,
                    &rom,
                    &request,
                    &execution_sha256,
                    &out,
                    started,
                    &request.game,
                    <LuckyDrawGame>::memory_view,
                )
            }
            _ => Err("unsupported game".into()),
        }
    })
}

fn mm2_strategy_workload(
    request: &Request,
    rom: &[u8],
    execution_sha256: &str,
    out: &Path,
) -> Result<Mm2Workload> {
    let game = match request.stage {
        Some(stage) => MegaMan2::at_stage(stage)?,
        None => MegaMan2::WHOLE_GAME,
    };
    let workload = Mm2Workload::new(
        game,
        rom,
        &request.core,
        &request.core_sha256,
        execution_sha256,
    )?;
    let Some(strategy) = mm2_stage_strategy(game, request.strategy.as_deref())? else {
        return nes_workload::workload::configured(workload, request, out);
    };
    fs::create_dir_all(out)?;
    fs::write(out.join(nes_workload::workload::STRATEGY_FILE), &strategy)?;
    let request = Request {
        strategy: None,
        ..request.clone()
    };
    nes_workload::workload::configured(workload.with_strategy(&strategy)?, &request, out)
}

fn mm2_stage_strategy(game: MegaMan2, given: Option<&Path>) -> Result<Option<String>> {
    if game.stage().is_none() {
        return Ok(None);
    }
    Ok(Some(match given {
        Some(path) => game.strategy_from(&fs::read_to_string(path)?)?,
        None => game.strategy(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_given_ending_strategy_runs_a_stage_start_with_that_stage_goal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("given.toml");
        let given = MegaMan2::WHOLE_GAME
            .strategy()
            .replacen("capacity = 1", "capacity = 2", 1);
        assert!(given.starts_with("goal = \"ending\"\n"));
        fs::write(&path, &given).unwrap();
        let air = MegaMan2::at_stage(1).unwrap();
        let strategy = mm2_stage_strategy(air, Some(&path)).unwrap().unwrap();
        assert_eq!(
            strategy,
            given.replacen("goal = \"ending\"", "goal = \"air_defeated\"", 1)
        );
        assert_eq!(mm2_stage_strategy(air, None).unwrap(), Some(air.strategy()));
        assert_eq!(
            mm2_stage_strategy(MegaMan2::WHOLE_GAME, Some(&path)).unwrap(),
            None
        );
    }

    #[test]
    fn only_the_games_that_take_a_root_accept_root_input() {
        for game in ROOT_GAMES.iter().chain(STRATEGY_GAMES) {
            assert!(root_input_taken(game, true).is_ok(), "{game}");
        }
        for game in ["stb", "castlevania"] {
            let error = root_input_taken(game, true).unwrap_err().to_string();
            assert!(
                error.starts_with(&format!("{game} takes no root_input")),
                "{error}"
            );
            assert!(root_input_taken(game, false).is_ok(), "{game}");
        }
    }
}
