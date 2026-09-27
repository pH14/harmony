// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    env,
    error::Error,
    fs::{self, File},
    io::{BufWriter, Write},
    path::PathBuf,
};

use machine::{
    Machine, Moment, StopConditions, StopMask, StopReason, nes::reproducer,
    quicknes::QuickNesMachine,
};
use nes_workload::{
    film::{FPS, Film},
    mm2::target::{Mm2Input, Mm2Target, decode_state},
    search::archive::ArchiveKey,
    target::{ExitKind, Target},
};
use sha2::{Digest, Sha256};

const USAGE: &str = "usage: mm2-replay INPUT.json [--film-from ACTION_INDEX --film-output OUTPUT.mp4] [--verify-completion]";

struct FilmRequest {
    first_action: usize,
    output: PathBuf,
}

struct ReplayOptions {
    input: PathBuf,
    film: Option<FilmRequest>,
    trace_output: Option<PathBuf>,
    trace_from: Option<usize>,
    verify_completion: bool,
}

fn parse_args() -> Result<ReplayOptions, Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let input = PathBuf::from(args.next().ok_or(USAGE)?);
    let mut film_from = None;
    let mut film_output = None;
    let mut trace_output = None;
    let mut trace_from = None;
    let mut verify_completion = false;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--verify-completion" => verify_completion = true,
            "--film-from" => {
                film_from = Some(args.next().ok_or(USAGE)?.parse::<usize>()?);
            }
            "--film-output" | "--film" => {
                film_output = Some(PathBuf::from(args.next().ok_or(USAGE)?));
            }
            "--trace-output" | "--trace" => {
                trace_output = Some(PathBuf::from(args.next().ok_or(USAGE)?));
            }
            "--trace-from" => {
                trace_from = Some(args.next().ok_or(USAGE)?.parse::<usize>()?);
            }
            _ => return Err(USAGE.into()),
        }
    }
    let film = match (film_from, film_output) {
        (Some(first_action), Some(output)) => Some(FilmRequest {
            first_action,
            output,
        }),
        (None, None) => None,
        _ => return Err("--film-from and --film-output must be supplied together".into()),
    };
    if trace_from.is_some() && trace_output.is_none() {
        return Err("--trace-from requires --trace-output".into());
    }
    Ok(ReplayOptions {
        input,
        film,
        trace_output,
        trace_from,
        verify_completion,
    })
}

fn write_trace(
    trace: &mut BufWriter<File>,
    index: usize,
    action: machine::nes::ButtonChord,
    machine: &QuickNesMachine,
) -> Result<(), Box<dyn Error>> {
    let wram = machine.read_wram()?;
    let state = decode_state(&wram)?;
    let entry = serde_json::json!({
        "format": "mm2-raw-trace-v1",
        "action_index": index,
        "buttons": action.buttons,
        "hold_frames": action.bounded_hold_frames(),
        "raw_frame_count": machine.now().0,
        "state": state,
        "wram": {
            "zeropage_0x00_0x50": &wram[0x00..0x50],
            "menu_cursor_page_0xfd_0xff": &wram[0xfd..0xff],
            "scrolling_0x14_0x23": &wram[0x14..0x23],
            "scrolling_0x37_0x3a": &wram[0x37..0x3a],
            "object_in_process_0x2d_0x30": &wram[0x2d..0x30],
            "object_screen_0x440_0x460": &wram[0x440..0x460],
            "enemy_indices_0x100_0x110": &wram[0x100..0x110],
            "enemy_hit_flags_0x110_0x120": &wram[0x110..0x120],
            "object_ids_0x400_0x420": &wram[0x400..0x420],
            "object_flags_0x420_0x440": &wram[0x420..0x440],
            "object_x_0x460_0x480": &wram[0x460..0x480],
            "object_y_0x4a0_0x4c0": &wram[0x4a0..0x4c0],
            "object_temp_0x4e0_0x500": &wram[0x4e0..0x500],
            "boss_state_0xb0_0xc0": &wram[0xb0..0xc0],
            "difficulty_0xcb": wram[0xcb],
            "health_0x6c0_0x6e0": &wram[0x6c0..0x6e0],
            "weapon_energy_0x9c_0xa8": &wram[0x9c..0xa8],
        },
    });
    serde_json::to_writer(&mut *trace, &entry)?;
    trace.write_all(b"\n")?;
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let options = parse_args()?;
    let input_path = options.input;
    let film_request = options.film;
    let input_bytes = fs::read(&input_path)?;
    let input: Mm2Input = serde_json::from_slice(&input_bytes)?;
    if let Some(request) = &film_request
        && request.first_action >= input.actions.len()
    {
        return Err(format!(
            "--film-from {} is outside the {} recorded actions",
            request.first_action,
            input.actions.len()
        )
        .into());
    }
    let trace_start = options
        .trace_from
        .or_else(|| film_request.as_ref().map(|request| request.first_action))
        .unwrap_or(0);
    if options.trace_output.is_some() && trace_start >= input.actions.len() {
        return Err(format!(
            "trace start {} is outside the {} recorded actions",
            trace_start,
            input.actions.len()
        )
        .into());
    }

    let rom_path = PathBuf::from(
        env::var_os("HARMONY_MM2_ROM").ok_or("HARMONY_MM2_ROM must name the external ROM")?,
    );
    let core_path = PathBuf::from(
        env::var_os("HARMONY_QUICKNES_CORE")
            .ok_or("HARMONY_QUICKNES_CORE must name the pinned libretro core")?,
    );
    let rom = fs::read(&rom_path)?;
    let core = fs::read(&core_path)?;
    let rom_sha256 = format!("{:x}", Sha256::digest(&rom));
    let core_sha256 = format!("{:x}", Sha256::digest(&core));
    let input_sha256 = format!("{:x}", Sha256::digest(&input_bytes));
    let mut machine = QuickNesMachine::from_rom_bytes(&rom, &core_path, &core_sha256)?;
    let mut target = if options.verify_completion {
        Some(Mm2Target::from_rom_bytes_whole_game(
            &rom,
            &core_path,
            &core_sha256,
        )?)
    } else {
        None
    };
    let prefix_len = target
        .as_ref()
        .map_or(0, |target| target.genesis_prefix().len());
    if let Some(target) = &target
        && input.actions.get(..prefix_len) != Some(target.genesis_prefix())
    {
        return Err("completion fixture does not match the ordinary power-on prefix".into());
    }
    let mut named_progress = nes_workload::mm2::progress::NamedProgress::default();
    let mut last_milestone_tier = None;
    let mut castle_arrivals = Vec::new();
    let mut first_ending = None;
    let power_on = machine.snapshot()?;
    machine.branch(power_on, &reproducer(&input.actions))?;
    machine.drop_snapshot(power_on)?;
    let mut film = None;
    let mut film_frames = 0_u64;
    let mut trace = options
        .trace_output
        .map(|path| -> Result<_, Box<dyn Error>> {
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                fs::create_dir_all(parent)?;
            }
            Ok(BufWriter::new(File::create(path)?))
        })
        .transpose()?;

    for (index, action) in input.actions.iter().copied().enumerate() {
        if film_request
            .as_ref()
            .is_some_and(|request| request.first_action == index)
        {
            machine.set_video_capture(true);
            machine.set_audio_capture(true);
        }
        let deadline = Moment(machine.now().0 + u64::from(action.bounded_hold_frames()));
        if !matches!(
            machine.run(
                StopConditions {
                    deadline: Some(deadline),
                    on: StopMask::NONE
                },
                None
            )?,
            StopReason::Deadline { .. }
        ) {
            return Err("raw replay stopped before its next action boundary".into());
        }
        if index >= prefix_len
            && let Some(target) = &mut target
        {
            let previous_clears = target.castle_clears();
            target.apply(&action);
            if target.exit_kind() != ExitKind::Ok {
                return Err(format!("target failed at action {index}").into());
            }
            for observation in target.last_action_observations() {
                let discoveries =
                    named_progress.observe(observation, index as u64, observation.frame_count);
                for name in discoveries {
                    if name == "ending"
                        || (name.ends_with("_defeated") && !name.contains("_refight_"))
                    {
                        let tier =
                            nes_workload::mm2::archive::archive_key(observation.decoded).progress();
                        if last_milestone_tier.is_some_and(|previous| tier <= previous) {
                            return Err(
                                format!("progress did not rise at {name}, action {index}").into()
                            );
                        }
                        last_milestone_tier = Some(tier);
                    }
                }
            }
            let raw = decode_state(&machine.read_wram()?)?;
            let state = target.mechanical_state();
            if (
                state.x,
                state.y,
                state.health,
                state.lives,
                state.weapons_obtained,
                state.boss_health,
                state.boss_phase,
                state.weapon,
                state.weapon_energies,
            ) != (
                raw.x,
                raw.y,
                raw.health,
                raw.lives,
                raw.weapons_obtained,
                raw.boss_health,
                raw.boss_phase,
                raw.weapon,
                raw.weapon_energies,
            ) {
                return Err(format!("target differs from continuous replay at action {index}: {state:?} versus {raw:?}").into());
            }
            if target.castle_clears() > previous_clears {
                castle_arrivals.push((index, target.castle_clears()));
            }
            if target.ending_reached() && first_ending.is_none() {
                first_ending = Some(index);
            }
            if [1000, 4500, 6400].contains(&index) {
                let snapshot = target.snapshot().ok_or("cannot snapshot replay oracle")?;
                let observation = target.observe();
                target.restore(&snapshot)?;
                if target.observe() != observation {
                    return Err(format!("snapshot changed observation at action {index}").into());
                }
            }
        }
        if film_request
            .as_ref()
            .is_some_and(|request| request.first_action <= index)
        {
            let frames = machine.take_video_frames();
            let audio = machine.take_audio_samples();
            if film.is_none() {
                let first = frames
                    .first()
                    .ok_or("film capture produced no video frames")?;
                let output = &film_request.as_ref().expect("film request checked").output;
                if let Some(parent) = output.parent() {
                    fs::create_dir_all(parent)?;
                }
                film = Some(Film::start(output, first.width, first.height)?);
            }
            let active = film.as_mut().expect("film initialized");
            active.write(&frames, &audio)?;
            film_frames = active.frames();
        }
        if index >= trace_start
            && let Some(trace) = trace.as_mut()
        {
            write_trace(trace, index, action, &machine)?;
        }
    }

    let film_report = film.map(Film::finish).transpose()?.map(|summary| {
        serde_json::json!({
            "video": summary.video,
            "video_fast": summary.video_fast,
            "film_from_action": film_request.expect("film request exists").first_action,
            "frames": summary.frames,
            "duration_seconds": summary.frames as f64 / FPS as f64,
            "audio_sample_frames": summary.sample_frames,
        })
    });
    let endpoint = decode_state(&machine.read_wram()?)?;
    if options.verify_completion {
        if nes_workload::mm2::progress::required_milestones()
            .iter()
            .any(|name| {
                named_progress
                    .first_seen
                    .get(name)
                    .is_none_or(Option::is_none)
            })
        {
            return Err("completion fixture missed a named milestone".into());
        }
        for (name, index) in [
            ("heat_entered", 419),
            ("heat_defeated", 976),
            ("air_entered", 1023),
            ("air_defeated", 1392),
            ("wood_entered", 1439),
            ("wood_defeated", 1845),
            ("bubble_entered", 1870),
            ("bubble_defeated", 2276),
            ("quick_entered", 2304),
            ("quick_defeated", 2560),
            ("flash_entered", 2589),
            ("flash_defeated", 2769),
            ("metal_entered", 27),
            ("metal_defeated", 394),
            ("crash_entered", 2823),
            ("crash_defeated", 3197),
            ("wily1_entered", 3230),
            ("wily1_boss_defeated", 3603),
            ("wily2_entered", 3609),
            ("wily2_boss_defeated", 4010),
            ("wily3_entered", 4017),
            ("wily3_boss_defeated", 4177),
            ("wily4_entered", 4184),
            ("wily4_boss_defeated", 5074),
            ("wily5_entered", 5081),
            ("wily5_boss_defeated", 6456),
            ("wily6_entered", 6463),
            ("wily6_boss_defeated", 6696),
            ("ending", 6697),
        ] {
            if named_progress
                .first_seen
                .get(name)
                .and_then(|stamp| *stamp)
                .map(|stamp| stamp.execution)
                != Some(index)
            {
                return Err(
                    format!("milestone {name} did not occur at recorded action {index}").into(),
                );
            }
        }
        if first_ending != Some(6697)
            || castle_arrivals
                .iter()
                .map(|(_, count)| *count)
                .collect::<Vec<_>>()
                != [1, 2, 3, 4, 5, 6]
        {
            return Err(format!(
                "whole-game progress mismatch: ending={first_ending:?}, castles={castle_arrivals:?}"
            )
            .into());
        }
        if input.actions.len() != 6_800 || machine.now().0 != 254_990 {
            return Err("completion fixture must replay 6,800 actions and 254,990 frames".into());
        }
        if (
            endpoint.stage,
            endpoint.boss_phase,
            endpoint.health,
            endpoint.lives,
            endpoint.weapons_obtained,
        ) != (5, 0xff, 6, 2, 0xff)
        {
            return Err(format!("completion fixture ending mismatch: {endpoint:?}").into());
        }
    }
    println!(
        "{}",
        serde_json::json!({
            "format": "mm2-raw-replay-v2",
            "restore_policy": "power_on_only",
            "completion_verified": options.verify_completion,
            "target_castle_arrivals": castle_arrivals,
            "named_progress": named_progress,
            "milestone_unit": "zero-based tape action index; not search executions",
            "target_first_ending_action": first_ending,
            "input": input_path,
            "rom": rom_path,
            "core": core_path,
            "rom_sha256": rom_sha256,
            "core_sha256": core_sha256,
            "input_sha256": input_sha256,
            "actions_recorded": input.actions.len(),
            "actions_applied": input.actions.len(),
            "raw_frame_count": machine.now().0,
            "endpoint": endpoint,
            "film": film_report,
            "film_frames_seen": film_frames,
            "trace_from_action": trace.as_ref().map(|_| trace_start),
        })
    );
    if let Some(trace) = trace.as_mut() {
        trace.flush()?;
    }
    Ok(())
}
