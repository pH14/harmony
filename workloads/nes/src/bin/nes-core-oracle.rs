// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    error::Error,
    fs,
    num::NonZeroUsize,
    path::{Path, PathBuf},
};

use machine::{
    Machine, Moment, StopConditions,
    nes::{self, ButtonChord},
    quicknes::{QuickNesMachine, STATE_HEADER_LEN},
};
use nes_workload::{
    castlevania, chord::ChordVocabulary, donkey_kong, famidash, ice_climbers, joust, metroid, mm2,
    pac_man, punchout, search::rand::RomuDuoJrRand, smb, twin_dragons, zelda,
};
use serde_json::json;
use sha2::{Digest, Sha256};

const CARTRIDGE_RAM_LEN: usize = 0x2000;
const START: u8 = 0x08;
const SELECT: u8 = 0x04;
const MENU_TAP_ODDS: usize = 16;

struct Game {
    setup: Vec<ButtonChord>,
    chords: ChordVocabulary,
    cartridge_ram: bool,
}

fn game(name: &str) -> Result<Game, Box<dyn Error>> {
    Ok(match name {
        "twin-dragons" => Game {
            setup: twin_dragons::target::setup_tape(),
            chords: twin_dragons::archive::CHORDS,
            cartridge_ram: false,
        },
        "famidash" => Game {
            setup: famidash::target::setup_tape(),
            chords: famidash::archive::CHORDS,
            cartridge_ram: true,
        },
        "super-mario-bros" => Game {
            setup: smb::target::BOOT_WALK.to_vec(),
            chords: smb::campaign::SmbButtonVocabulary::default().chords(),
            cartridge_ram: false,
        },
        "mega-man-2" => Game {
            setup: mm2::target::power_on_walk(),
            chords: mm2::archive::CHORDS,
            cartridge_ram: false,
        },
        "metroid" => Game {
            setup: metroid::target::power_on_walk(),
            chords: metroid::archive::CHORDS,
            cartridge_ram: true,
        },
        "castlevania" => Game {
            setup: castlevania::target::setup_tape(),
            chords: castlevania::archive::CHORDS,
            cartridge_ram: false,
        },
        "punchout" => Game {
            setup: punchout::target::setup_tape(),
            chords: punchout::archive::CHORDS,
            cartridge_ram: false,
        },
        "zelda" => Game {
            setup: zelda::target::setup_tape(),
            chords: zelda::archive::CHORDS,
            cartridge_ram: false,
        },
        "donkey-kong" => Game {
            setup: donkey_kong::target::setup_tape(),
            chords: donkey_kong::archive::CHORDS,
            cartridge_ram: false,
        },
        "ice-climbers" => Game {
            setup: ice_climbers::target::setup_tape(),
            chords: ice_climbers::archive::CHORDS,
            cartridge_ram: false,
        },
        "pac-man" => Game {
            setup: pac_man::target::setup_tape(),
            chords: pac_man::archive::CHORDS,
            cartridge_ram: false,
        },
        "joust" => Game {
            setup: joust::target::setup_tape(),
            chords: joust::archive::CHORDS,
            cartridge_ram: false,
        },
        other => return Err(format!("unknown game {other}").into()),
    })
}

fn below(rand: &mut RomuDuoJrRand, choices: usize) -> usize {
    rand.below(NonZeroUsize::new(choices).unwrap_or(NonZeroUsize::MIN))
}

fn tape(game: &Game, seed: u64, frames: u64) -> Result<Vec<ButtonChord>, Box<dyn Error>> {
    let mut tape = game.setup.clone();
    let mut rand = RomuDuoJrRand::with_seed(seed);
    let mut previous: Option<ButtonChord> = None;
    let mut drawn = 0_u64;
    while drawn < frames {
        let chord = if below(&mut rand, MENU_TAP_ODDS) == 0 {
            let buttons = if below(&mut rand, 2) == 0 {
                START
            } else {
                SELECT
            };
            ButtonChord::new(buttons, 2 + u8::try_from(below(&mut rand, 6))?)
        } else {
            let chord = game.chords.draw(&mut rand, previous.as_ref())?;
            previous = Some(chord);
            chord
        };
        drawn += u64::from(chord.bounded_hold_frames());
        tape.push(chord);
    }
    Ok(tape)
}

fn genesis_prefix(name: &str, rom: &Path, core: &Path) -> Result<Vec<ButtonChord>, Box<dyn Error>> {
    let rom = fs::read(rom)?;
    let core_sha256 = format!("{:x}", Sha256::digest(fs::read(core)?));
    match name {
        "metroid" => {
            Ok(
                metroid::target::MetroidTarget::from_rom_bytes_headless(&rom, core, &core_sha256)?
                    .genesis_prefix()
                    .to_vec(),
            )
        }
        "super-mario-bros" => {
            let mut machine = QuickNesMachine::from_rom_bytes(&rom, core, &core_sha256)?;
            let mut prefix = smb::target::BOOT_WALK.to_vec();
            nes::run_actions(&mut machine, &prefix)?;
            let idle = ButtonChord::new(0, 1);
            while !smb::target::smb_is_playing(&machine.read_wram()?) {
                if prefix.len() > smb::target::BOOT_WALK.len() + 900 {
                    return Err("Super Mario Bros did not reach play".into());
                }
                nes::run_actions(&mut machine, &[idle])?;
                prefix.push(idle);
            }
            Ok(prefix)
        }
        other => Err(format!("--tape has no genesis prefix for {other}").into()),
    }
}

fn recorded_tape(path: &Path) -> Result<Vec<ButtonChord>, Box<dyn Error>> {
    let input: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
    let actions = input["actions"]
        .as_array()
        .ok_or("tape has no actions array")?;
    actions
        .iter()
        .map(|action| {
            let field = |name: &str| -> Result<u8, Box<dyn Error>> {
                Ok(u8::try_from(
                    action[name]
                        .as_u64()
                        .ok_or_else(|| format!("tape action has no {name}"))?,
                )?)
            };
            Ok(ButtonChord::new(field("buttons")?, field("hold_frames")?))
        })
        .collect()
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut play = false;
    let mut recorded = None;
    loop {
        match args.first().map(String::as_str) {
            Some("--play") => {
                play = true;
                args.remove(0);
            }
            Some("--tape") if args.len() > 1 => {
                recorded = Some(PathBuf::from(args.remove(1)));
                args.remove(0);
            }
            _ => break,
        }
    }
    let usage = "usage: nes-core-oracle [--play] GAME ROM CORE SEED RANDOM_FRAMES OUT_DIR\n       nes-core-oracle [--play] --tape INPUT_JSON GAME ROM CORE OUT_DIR";
    let (name, rom, core, out, tape, seed, setup_actions) = match (&recorded, args.as_slice()) {
        (None, [name, rom, core, seed, frames, out]) => {
            let game = game(name)?;
            let seed: u64 = seed.parse()?;
            (
                name,
                rom,
                core,
                out,
                tape(&game, seed, frames.parse()?)?,
                Some(seed),
                game.setup.len(),
            )
        }
        (Some(path), [name, rom, core, out]) => {
            let mut tape = genesis_prefix(name, Path::new(rom), Path::new(core))?;
            let setup_actions = tape.len();
            tape.extend(recorded_tape(path)?);
            (name, rom, core, out, tape, None, setup_actions)
        }
        _ => return Err(usage.into()),
    };
    let game = game(name)?;
    let rom = fs::read(rom)?;
    let image = if game.cartridge_ram {
        nes::with_cartridge_ram(&rom)?
    } else {
        rom
    };
    let core = PathBuf::from(core);
    let core_sha256 = format!("{:x}", Sha256::digest(fs::read(&core)?));
    let mut machine = QuickNesMachine::from_rom_bytes(&image, &core, &core_sha256)?;
    machine.set_video_capture(play);
    machine.set_audio_capture(play);
    let out = Path::new(out);
    fs::create_dir_all(out)?;

    let mut boundaries = Vec::new();
    let mut state = Vec::new();
    let mut record = |machine: &QuickNesMachine, boundaries: &mut Vec<u8>| {
        boundaries.extend_from_slice(&machine.read_wram()?);
        if game.cartridge_ram {
            let cartridge = machine.read_save_ram()?;
            boundaries.extend_from_slice(
                cartridge
                    .get(..CARTRIDGE_RAM_LEN)
                    .ok_or("cartridge RAM is shorter than 8 KiB")?,
            );
        }
        machine.capture_into(&mut state)?;
        boundaries.extend_from_slice(&state[STATE_HEADER_LEN..]);
        Ok::<_, Box<dyn Error>>(state.len() - STATE_HEADER_LEN)
    };
    let state_len = record(&machine, &mut boundaries)?;

    let genesis = machine.snapshot()?;
    machine.branch(genesis, &nes::reproducer(&tape))?;
    let mut frame_wram = Vec::new();
    for action in &tape {
        let deadline = machine.now().0 + u64::from(action.bounded_hold_frames());
        machine.run(
            StopConditions {
                deadline: Some(Moment(deadline)),
                ..StopConditions::default()
            },
            None,
        )?;
        for wram in machine.frames() {
            frame_wram.extend_from_slice(wram);
        }
        machine.take_video_frames();
        machine.take_audio_samples();
        record(&machine, &mut boundaries)?;
    }

    fs::write(out.join("frames.bin"), &frame_wram)?;
    fs::write(out.join("boundaries.bin"), &boundaries)?;
    let actions: Vec<[u8; 2]> = tape
        .iter()
        .map(|chord| [chord.buttons, chord.bounded_hold_frames()])
        .collect();
    let manifest = json!({
        "game": name,
        "seed": seed,
        "setup_actions": setup_actions,
        "cartridge_ram": game.cartridge_ram,
        "state_len": state_len,
        "frames": frame_wram.len() / 2048,
        "core_sha256": core_sha256,
        "actions": actions,
    });
    fs::write(out.join("tape.json"), serde_json::to_vec(&manifest)?)?;
    Ok(())
}
