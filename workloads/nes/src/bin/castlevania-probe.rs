// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{env, error::Error, fs, path::PathBuf};

use machine::nes::{ButtonChord, WRAM_SIZE};
use machine::{Machine, StopConditions, nes, quicknes::QuickNesMachine};
use sha2::{Digest, Sha256};

const WAIT_LIMIT: u32 = 20_000;

struct Probe {
    machine: QuickNesMachine,
    frame: u32,
    prior: [u8; WRAM_SIZE],
    shots: Option<PathBuf>,
    shot_every: u32,
    diff: bool,
    every: u32,
    addrs: Vec<usize>,
}

impl Probe {
    fn hold(&mut self, buttons: u8, hold: u8) -> Result<(), Box<dyn Error>> {
        let chord = ButtonChord::new(buttons, hold);
        let snap = self.machine.snapshot()?;
        self.machine.branch(snap, &nes::reproducer(&[chord]))?;
        self.machine.run(StopConditions::default(), None)?;
        self.machine.drop_snapshot(snap)?;
        let video = self.machine.take_video_frames();
        for (index, w) in self.machine.frames().iter().enumerate() {
            self.frame += 1;
            let frame = self.frame;
            if let Some(dir) = &self.shots
                && frame.is_multiple_of(self.shot_every)
                && let Some(v) = video.get(index)
            {
                fs::write(
                    dir.join(format!("{frame:06}-{}x{}.rgb", v.width, v.height)),
                    &v.rgb24,
                )?;
            }
            if self.diff && w != &self.prior {
                let diffs: Vec<String> = w
                    .iter()
                    .zip(self.prior.iter())
                    .enumerate()
                    .filter(|(i, (a, b))| a != b && *i < 0x100)
                    .map(|(i, (a, _))| format!("{i:03x}={a:02x}"))
                    .collect();
                if !diffs.is_empty() {
                    println!("{frame:5} diff {}", diffs.join(" "));
                }
            }
            if (self.every > 0 && frame.is_multiple_of(self.every))
                || (self.every == 0 && self.addrs.iter().any(|a| w[*a] != self.prior[*a]))
            {
                print!("{frame:5}  {buttons:02x}    ");
                for a in &self.addrs {
                    print!("{:02x}   ", w[*a]);
                }
                println!();
            }
            self.prior = *w;
        }
        Ok(())
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let schedule = args
        .next()
        .ok_or("usage: castlevania-probe <buttons:hold|pADDR=VALUE|uADDR=VALUE/buttons,...> <every> <addr,addr,...>")?;
    let every: u32 = args.next().ok_or("missing print interval")?.parse()?;
    let addrs = args
        .next()
        .ok_or("missing address list")?
        .split(',')
        .map(|a| usize::from_str_radix(a.trim_start_matches("0x"), 16))
        .collect::<Result<Vec<_>, _>>()?;
    if addrs.iter().any(|address| *address >= WRAM_SIZE) {
        return Err("a printed address is outside the 2 KiB of work RAM".into());
    }
    let rom = fs::read(PathBuf::from(
        env::var_os("HARMONY_CASTLEVANIA_ROM").ok_or("HARMONY_CASTLEVANIA_ROM")?,
    ))?;
    let core_path =
        PathBuf::from(env::var_os("HARMONY_QUICKNES_CORE").ok_or("HARMONY_QUICKNES_CORE")?);
    let core_sha256 = format!("{:x}", Sha256::digest(fs::read(&core_path)?));
    let mut machine = QuickNesMachine::from_rom_bytes(&rom, &core_path, &core_sha256)?;
    let shots = env::var_os("PROBE_SHOTS").map(PathBuf::from);
    let shot_every: u32 = env::var("PROBE_SHOT_EVERY")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);
    let diff = env::var_os("PROBE_DIFF").is_some();
    if let Some(dir) = &shots {
        fs::create_dir_all(dir)?;
        machine.set_video_capture(true);
    }
    print!("frame  chord ");
    for a in &addrs {
        print!("{a:04x} ");
    }
    println!();
    let prior = machine.read_wram()?;
    let mut probe = Probe {
        machine,
        frame: 0,
        prior,
        shots,
        shot_every,
        diff,
        every,
        addrs,
    };
    for step in schedule.split(',') {
        if let Some(poke) = step.strip_prefix('p') {
            let (address, value) = poke.split_once('=').ok_or("bad poke")?;
            probe.machine.poke_wram(
                usize::from_str_radix(address, 16)?,
                u8::from_str_radix(value, 16)?,
            )?;
            continue;
        }
        if let Some(until) = step.strip_prefix('u') {
            let (condition, b) = until.split_once('/').ok_or("bad wait")?;
            let (address, value) = condition.split_once('=').ok_or("bad wait")?;
            let address = usize::from_str_radix(address, 16)?;
            if address >= WRAM_SIZE {
                return Err("a wait address is outside the 2 KiB of work RAM".into());
            }
            let value = u8::from_str_radix(value, 16)?;
            let buttons = u8::from_str_radix(b, 16)?;
            let mut waited = 0;
            while probe.prior[address] != value {
                if waited == WAIT_LIMIT {
                    return Err(format!("{address:#x} never read {value:#04x}").into());
                }
                probe.hold(buttons, 1)?;
                waited += 1;
            }
            continue;
        }
        let (b, h) = step.split_once(':').ok_or("bad step")?;
        let buttons = u8::from_str_radix(b.trim_start_matches("0x"), 16)?;
        let mut remaining: u32 = h.parse()?;
        while remaining > 0 {
            let hold = remaining.min(120);
            probe.hold(buttons, u8::try_from(hold)?)?;
            remaining -= hold;
        }
    }
    let Probe {
        mut machine,
        prior,
        shots,
        ..
    } = probe;
    if let Some(dir) = &shots {
        fs::write(dir.join("final-wram.bin"), prior)?;
        let snap = machine.snapshot()?;
        fs::write(dir.join("final-state.bin"), machine.snapshot_bytes(snap)?)?;
    }
    Ok(())
}
