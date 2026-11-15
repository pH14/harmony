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
    trace: Option<fs::File>,
    shot_at: Vec<u32>,
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
                && (frame.is_multiple_of(self.shot_every) || self.shot_at.contains(&frame))
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
            if let Some(file) = self.trace.as_mut() {
                std::io::Write::write_all(file, w)?;
            }
            self.prior = *w;
        }
        Ok(())
    }
}

fn bot_buttons(w: &[u8; WRAM_SIZE], frame: u32) -> u8 {
    let mut me: Vec<(i32, i32)> = Vec::new();
    let mut foes: Vec<(i32, i32)> = Vec::new();
    let mut eggs: Vec<(i32, i32)> = Vec::new();
    for sprite in w[0x200..0x300].chunks_exact(4) {
        let (y, attr, x) = (i32::from(sprite[0]), sprite[2], i32::from(sprite[3]));
        if !(0x10..0xc0).contains(&y) {
            continue;
        }
        match attr & 3 {
            0 => me.push((x, y)),
            2 => foes.push((x, y)),
            3 if sprite[1] == 204 => eggs.push((x, y)),
            _ => {}
        }
    }
    if me.is_empty() {
        return if frame % 6 < 2 { 1 } else { 0 };
    }
    let n = me.len() as i32;
    let (px, py) = (
        me.iter().map(|m| m.0).sum::<i32>() / n,
        me.iter().map(|m| m.1).sum::<i32>() / n,
    );
    let wrap = |d: i32| ((d + 128).rem_euclid(256)) - 128;
    let egg_mode = !eggs.is_empty();
    let pool = if egg_mode { &eggs } else { &foes };
    let target = pool
        .iter()
        .min_by_key(|f| wrap(f.0 - px).abs() + (f.1 - py).abs())
        .copied();
    let (tx, ty) = target.unwrap_or((px, py));
    let dx = wrap(tx - px);
    let mut buttons = 0u8;
    if dx > 6 {
        buttons |= 0x80;
    } else if dx < -6 {
        buttons |= 0x40;
    }
    let want = if egg_mode { ty - 4 } else { ty - 12 };
    let period = if py > 170 || py > want + 6 {
        3
    } else if py > want - 6 {
        7
    } else {
        40
    };
    if frame.is_multiple_of(period) {
        buttons |= 1;
    }
    buttons
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let schedule = args
        .next()
        .ok_or("usage: joust-probe <buttons:hold|pADDR=VALUE|uADDR=VALUE/buttons,...> <every> <addr,addr,...>")?;
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
        env::var_os("HARMONY_JOUST_ROM").ok_or("HARMONY_JOUST_ROM")?,
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
        shot_at: env::var("PROBE_SHOT_AT")
            .map(|v| v.split(',').filter_map(|n| n.parse().ok()).collect())
            .unwrap_or_default(),
        trace: env::var_os("PROBE_TRACE")
            .map(fs::File::create)
            .transpose()?,
    };
    for step in schedule.split(',') {
        if let Some(range) = step.strip_prefix('z') {
            let (a, b) = range.split_once('-').ok_or("bad zero range")?;
            let (a, b) = (usize::from_str_radix(a, 16)?, usize::from_str_radix(b, 16)?);
            for address in a..b {
                probe.machine.poke_wram(address, 0)?;
            }
            continue;
        }
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
        if let Some(n) = step.strip_prefix('b') {
            let n: u32 = n.parse()?;
            for _ in 0..n {
                let buttons = bot_buttons(&probe.prior, probe.frame);
                probe.hold(buttons, 1)?;
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
