// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{env, error::Error, fs, path::PathBuf};

use nes_workload::{
    search::archive::ArchiveKey,
    smb::{
        archive::{SmbArchiveKey, archive_key, stamp_arrival_room},
        plan::SmbRoute,
        target::{
            ROOM_IDENTITY_BYTES, SmbInput, SmbTarget, smb_is_playing, smb_player_has_control,
            smb_player_position,
        },
    },
    target::Target,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

const WORLD_NUMBER_OFFSET: usize = 0x075f;
const LEVEL_NUMBER_OFFSET: usize = 0x075c;

#[derive(Serialize)]
struct Level {
    level: String,
    start_frame: u64,
    control_frame: Option<u64>,
    frames: u64,
    exit: &'static str,
    exit_room: [u8; 2],
    exit_x: u32,
    exit_frame: u64,
}

#[derive(Serialize)]
struct StageVisit {
    stage: u8,
    name: &'static str,
    first_action: usize,
    frame: u64,
    objectives: Vec<u16>,
    doomed_actions: usize,
}

#[derive(Serialize)]
struct Readout {
    route: &'static str,
    stages_in_order: bool,
    doomed_actions: usize,
    visits: Vec<StageVisit>,
}

#[derive(Serialize)]
struct Route {
    boot_frames: u64,
    tape_frames: u64,
    frames_from_power_on: u64,
    actions_applied: usize,
    victory: bool,
    dead: bool,
    levels: Vec<Level>,
    #[serde(skip_serializing_if = "Option::is_none")]
    readout: Option<Readout>,
}

fn label(world: u8, level: u8) -> String {
    format!("{}-{}", world + 1, level + 1)
}

fn exit_kind(from: (u8, u8), to: (u8, u8)) -> &'static str {
    let next = if from.1 == 3 {
        (from.0 + 1, 0)
    } else {
        (from.0, from.1 + 1)
    };
    if to == next {
        "next"
    } else if to.0 > from.0 && to.1 == 0 {
        "warp"
    } else {
        "other"
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let input_path = PathBuf::from(
        args.next()
            .ok_or("usage: smb-route <input.json> [--route <identifier>]")?,
    );
    let route = match (args.next(), args.next()) {
        (None, None) => None,
        (Some(flag), Some(identifier)) if flag == "--route" => Some(SmbRoute::from_identifier(
            identifier.to_str().ok_or("the route is not UTF-8")?,
        )?),
        _ => return Err("usage: smb-route <input.json> [--route <identifier>]".into()),
    };
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    let rom = fs::read(PathBuf::from(
        env::var_os("HARMONY_SMB_ROM").ok_or("HARMONY_SMB_ROM must name the ROM")?,
    ))?;
    let core_path = PathBuf::from(
        env::var_os("HARMONY_QUICKNES_CORE").ok_or("HARMONY_QUICKNES_CORE must name the core")?,
    );
    let core_sha256 = format!("{:x}", Sha256::digest(fs::read(&core_path)?));
    let input: SmbInput = serde_json::from_slice(&fs::read(&input_path)?)?;

    let mut target = SmbTarget::from_smb_rom_bytes_headless(&rom, &core_path, &core_sha256)?;
    target.reset();
    let boot_frames = target.boot_frames();

    let mut levels: Vec<(u8, u8, u64, Option<u64>)> = Vec::new();
    let mut exits: Vec<([u8; 2], u32, u64)> = Vec::new();
    let mut frames = 0_u64;
    let mut applied = 0_usize;
    let wram = target.wram();
    levels.push((
        wram[WORLD_NUMBER_OFFSET],
        wram[LEVEL_NUMBER_OFFSET],
        0,
        None,
    ));
    exits.push(([0, 0], 0, boot_frames));
    let mut parent: Option<SmbArchiveKey> = None;
    let mut visits: Vec<StageVisit> = Vec::new();
    for action in &input.actions {
        if target.is_dead() || target.is_victory() {
            break;
        }
        target.apply(action);
        applied += 1;
        frames += target.last_action_frames();
        let wram = target.wram();
        if let Some(route) = route {
            let key = stamp_arrival_room(archive_key(&wram).on_route(route), &wram)?
                .complete(parent.map(|key| (key, &())));
            parent = Some(key);
            let (stage, objective) = key.progress();
            let doomed = !target.is_dead() && (target.is_doomed() || route.is_doomed(&wram));
            if visits.last().is_none_or(|visit| visit.stage != stage) {
                visits.push(StageVisit {
                    stage,
                    name: route.stage_name(stage),
                    first_action: applied,
                    frame: target.boot_frames() + frames,
                    objectives: Vec::new(),
                    doomed_actions: 0,
                });
            }
            if let Some(visit) = visits.last_mut() {
                if visit.objectives.last() != Some(&objective) {
                    visit.objectives.push(objective);
                }
                visit.doomed_actions += usize::from(doomed);
            }
        }
        let here = (wram[WORLD_NUMBER_OFFSET], wram[LEVEL_NUMBER_OFFSET]);
        if smb_is_playing(&wram)
            && levels
                .last()
                .is_some_and(|(world, level, _, _)| (*world, *level) != here)
        {
            levels.push((here.0, here.1, frames, None));
            exits.push(([0, 0], 0, boot_frames + frames));
        }
        if smb_player_has_control(&wram)
            && let Some(exit) = exits.last_mut()
        {
            *exit = (
                ROOM_IDENTITY_BYTES.map(|offset| wram[offset]),
                smb_player_position(&wram).x,
                boot_frames + frames,
            );
        }
        if smb_player_has_control(&wram)
            && let Some(current) = levels.last_mut()
            && (current.0, current.1) == here
            && current.3.is_none()
        {
            current.3 = Some(boot_frames + frames);
        }
    }
    let route = Route {
        boot_frames,
        tape_frames: frames,
        frames_from_power_on: boot_frames + frames,
        actions_applied: applied,
        victory: target.is_victory(),
        dead: target.is_dead(),
        readout: route.map(|route| Readout {
            route: route.identifier(),
            stages_in_order: visits
                .windows(2)
                .all(|pair| pair[1].stage == pair[0].stage + 1),
            doomed_actions: visits.iter().map(|visit| visit.doomed_actions).sum(),
            visits,
        }),
        levels: levels
            .iter()
            .enumerate()
            .map(|(index, (world, level, start, control))| {
                let next = levels.get(index + 1);
                Level {
                    level: label(*world, *level),
                    start_frame: boot_frames + start,
                    control_frame: *control,
                    frames: next.map_or(frames, |(_, _, end, _)| *end) - start,
                    exit: next.map_or("end", |(to_world, to_level, _, _)| {
                        exit_kind((*world, *level), (*to_world, *to_level))
                    }),
                    exit_room: exits[index].0,
                    exit_x: exits[index].1,
                    exit_frame: exits[index].2,
                }
            })
            .collect(),
    };
    println!("{}", serde_json::to_string_pretty(&route)?);
    Ok(())
}
