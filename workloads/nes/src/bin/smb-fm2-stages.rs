// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{env, error::Error, fs, path::PathBuf};

use machine::nes::{ButtonChord, WRAM_SIZE};
use machine::{Machine, StopConditions, nes, quicknes::QuickNesMachine};
use nes_games::smb::target::{smb_is_playing, smb_is_victory, smb_player_has_control};
use serde::Serialize;
use sha2::{Digest, Sha256};

const WORLD_NUMBER_OFFSET: usize = 0x075f;
const LEVEL_NUMBER_OFFSET: usize = 0x075c;
const FM2_BUTTONS: [u8; 8] = [0x80, 0x40, 0x20, 0x10, 0x08, 0x04, 0x02, 0x01];

#[derive(Serialize)]
struct Level {
    level: String,
    start_frame: u64,
    control_frame: Option<u64>,
    exit_frame: u64,
}

#[derive(Serialize)]
struct Stages {
    movie_frames: u64,
    victory_frame: Option<u64>,
    levels: Vec<Level>,
}

fn buttons(line: &str) -> Result<u8, Box<dyn Error>> {
    let fields = line.split('|').collect::<Vec<_>>();
    let port = fields
        .get(2)
        .ok_or("an FM2 input line has no port 0 field")?;
    if port.len() != FM2_BUTTONS.len() {
        return Err(format!("an FM2 port 0 field is not 8 characters: {port}").into());
    }
    Ok(port
        .chars()
        .zip(FM2_BUTTONS)
        .filter(|(character, _)| *character != '.' && *character != ' ')
        .fold(0, |mask, (_, bit)| mask | bit))
}

fn main() -> Result<(), Box<dyn Error>> {
    let movie = PathBuf::from(
        env::args()
            .nth(1)
            .ok_or("usage: smb-fm2-stages <movie.fm2> [shift]")?,
    );
    let shift: i64 = env::args().nth(2).map_or(Ok(0), |value| value.parse())?;
    let rom = fs::read(PathBuf::from(
        env::var_os("HARMONY_SMB_ROM").ok_or("HARMONY_SMB_ROM must name the ROM")?,
    ))?;
    let core_path = PathBuf::from(
        env::var_os("HARMONY_QUICKNES_CORE").ok_or("HARMONY_QUICKNES_CORE must name the core")?,
    );
    let core_sha256 = format!("{:x}", Sha256::digest(fs::read(&core_path)?));
    let mut machine = QuickNesMachine::from_rom_bytes(&rom, &core_path, &core_sha256)?;
    let text = fs::read_to_string(&movie)?;
    let mut levels: Vec<Level> = Vec::new();
    let mut frame = 0_u64;
    let mut victory_frame = None;
    let trace: Option<u64> = env::var("SMB_FM2_TRACE")
        .ok()
        .map(|value| value.parse())
        .transpose()?;
    let mut inputs = vec![0; usize::try_from(shift.max(0))?];
    for line in text
        .lines()
        .filter(|line| line.starts_with('|'))
        .skip(usize::try_from((-shift).max(0))?)
    {
        inputs.push(buttons(line)?);
    }
    for pressed in inputs {
        let chord = ButtonChord::new(pressed, 1);
        let snapshot = machine.snapshot()?;
        machine.branch(snapshot, &nes::reproducer(&[chord]))?;
        machine.run(StopConditions::default(), None)?;
        machine.drop_snapshot(snapshot)?;
        frame += 1;
        let wram: [u8; WRAM_SIZE] = machine
            .read(0, WRAM_SIZE as u32)?
            .try_into()
            .map_err(|_| "the work RAM window has an invalid length")?;
        if trace.is_some_and(|every| frame.is_multiple_of(every)) {
            eprintln!(
                "frame {frame} world {} level {} x {} y {} engine {} mode {}",
                wram[WORLD_NUMBER_OFFSET],
                wram[LEVEL_NUMBER_OFFSET],
                u32::from(wram[0x006d]) * 256 + u32::from(wram[0x0086]),
                wram[0x00ce],
                wram[0x000e],
                wram[0x0770]
            );
        }
        if victory_frame.is_none() && smb_is_victory(&wram) {
            victory_frame = Some(frame);
        }
        if !smb_is_playing(&wram) {
            continue;
        }
        let here = format!(
            "{}-{}",
            wram[WORLD_NUMBER_OFFSET] + 1,
            wram[LEVEL_NUMBER_OFFSET] + 1
        );
        if levels.last().is_none_or(|level| level.level != here) {
            levels.push(Level {
                level: here,
                start_frame: frame,
                control_frame: None,
                exit_frame: frame,
            });
        }
        if let Some(level) = levels.last_mut()
            && smb_player_has_control(&wram)
        {
            level.control_frame.get_or_insert(frame);
            level.exit_frame = frame;
        }
    }
    let stages = Stages {
        movie_frames: frame,
        victory_frame,
        levels,
    };
    println!("{}", serde_json::to_string_pretty(&stages)?);
    Ok(())
}
