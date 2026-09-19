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
    mm2::target::{Mm2Input, decode_state},
};
use sha2::{Digest, Sha256};

const USAGE: &str =
    "usage: mm2-replay INPUT.json [--film-from ACTION_INDEX --film-output OUTPUT.mp4]";

struct FilmRequest {
    first_action: usize,
    output: PathBuf,
}

struct ReplayOptions {
    input: PathBuf,
    film: Option<FilmRequest>,
    trace_output: Option<PathBuf>,
    trace_from: Option<usize>,
}

fn parse_args() -> Result<ReplayOptions, Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let input = PathBuf::from(args.next().ok_or(USAGE)?);
    let mut film_from = None;
    let mut film_output = None;
    let mut trace_output = None;
    let mut trace_from = None;
    while let Some(flag) = args.next() {
        match flag.as_str() {
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
    println!(
        "{}",
        serde_json::json!({
            "format": "mm2-raw-replay-v2",
            "restore_policy": "power_on_only",
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
