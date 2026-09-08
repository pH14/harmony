// SPDX-License-Identifier: AGPL-3.0-or-later
//! Equal-suffix diagnostic on sampled competing states; never fresh validation.

use nes_workload::{
    metroid::{
        archive::sample_chord,
        retention_audit::ReplacementPair,
        target::{
            ButtonChord, MetroidInput, MetroidMechanicalState, MetroidSnapshot, MetroidTarget,
        },
    },
    search::rand::RomuDuoJrRand,
    target::{ExitKind, Target},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    error::Error,
    fs,
    io::{BufWriter, Write},
    path::Path,
};

#[derive(Deserialize)]
struct Audit {
    samples: Vec<Vec<ReplacementPair>>,
}
#[derive(Debug, Serialize)]
struct Outcome {
    frames: u64,
    dead: bool,
    endpoint: MetroidMechanicalState,
    equipment_gained: u8,
    boss_gain: bool,
    capacity_gain: bool,
    // Living action-interior map visits beyond the shared starting map.
    reached_maps: BTreeSet<(u8, u8, u8)>,
}
fn prepare(
    core: &Path,
    rom: &[u8],
    hash: &str,
    input: &MetroidInput,
    expected: MetroidMechanicalState,
) -> Result<(MetroidTarget, MetroidSnapshot, u64), Box<dyn Error>> {
    let mut target = MetroidTarget::from_rom_bytes_headless(rom, core, hash)?;
    for action in &input.actions {
        if target.is_dead() || target.is_victory() {
            return Err("sample input continues after terminal".into());
        }
        target.apply(action);
        if target.exit_kind() != ExitKind::Ok {
            return Err("sample replay emulator failure".into());
        }
    }
    if target.mechanical_state() != expected {
        return Err("sample replay endpoint differs from recorded competitor".into());
    }
    let snapshot = target.snapshot().ok_or("sample snapshot failed")?;
    let frames = target.frames_clocked();
    Ok((target, snapshot, frames))
}
fn extend(
    target: &mut MetroidTarget,
    snapshot: &MetroidSnapshot,
    suffix: &[ButtonChord],
) -> Result<Outcome, Box<dyn Error>> {
    target.restore(snapshot)?;
    let start = target.mechanical_state();
    let mut outcome = Outcome {
        frames: 0,
        dead: false,
        endpoint: start,
        equipment_gained: 0,
        boss_gain: false,
        capacity_gain: false,
        reached_maps: BTreeSet::new(),
    };
    let before = target.frames_clocked();
    for action in suffix {
        if target.is_dead() || target.is_victory() {
            break;
        }
        target.apply(action);
        if target.exit_kind() != ExitKind::Ok {
            return Err("probe emulator failure".into());
        }
        for observation in target.last_action_observations() {
            let s = observation.decoded;
            outcome.equipment_gained |= s.equipment & !start.equipment;
            outcome.boss_gain |= s.bosses > start.bosses;
            outcome.capacity_gain |=
                s.missile_capacity > start.missile_capacity || s.energy_tanks > start.energy_tanks;
            if s.in_play()
                && !s.is_dead()
                && (s.area, s.map_x, s.map_y) != (start.area, start.map_x, start.map_y)
            {
                outcome.reached_maps.insert((s.area, s.map_x, s.map_y));
            }
        }
    }
    outcome.frames = target.frames_clocked() - before;
    outcome.dead = target.is_dead();
    outcome.endpoint = target.mechanical_state();
    Ok(outcome)
}
fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 6 {
        return Err("usage: metroid-retention-probe CORE ROM AUDIT OUT TRIALS ACTIONS".into());
    }
    let core = Path::new(&args[0]);
    let rom = fs::read(&args[1])?;
    let bytes = fs::read(&args[2])?;
    let audit: Audit = serde_json::from_slice(&bytes)?;
    let out = Path::new(&args[3]);
    fs::create_dir(out)?;
    let trials: usize = args[4].parse()?;
    let actions: usize = args[5].parse()?;
    if trials == 0 || trials > 256 || actions == 0 || actions > 128 {
        return Err("probe limits exceed bounded diagnostic".into());
    }
    let core_hash = format!("{:x}", Sha256::digest(fs::read(core)?));
    let mut rng = RomuDuoJrRand::with_seed(0x616c_7466_7574_7572);
    let suffixes: Vec<Vec<ButtonChord>> = (0..trials)
        .map(|_| (0..actions).map(|_| sample_chord(&mut rng)).collect())
        .collect::<Result<_, _>>()?;
    fs::write(out.join("suffixes.json"), serde_json::to_vec(&suffixes)?)?;
    let mut log = BufWriter::new(fs::File::create(out.join("outcomes.jsonl"))?);
    let mut totals = vec![[0u64; 6]; audit.samples.len()];
    let mut prefix_frames = 0u64;
    let mut probe_frames = [0u64; 2];
    let mut pair_count = 0;
    for (stratum, pairs) in audit.samples.iter().enumerate() {
        for (index, pair) in pairs.iter().enumerate() {
            let (mut candidate, cs, cf) = prepare(
                core,
                &rom,
                &core_hash,
                &pair.candidate_input,
                pair.candidate,
            )?;
            let (mut incumbent, is, inf) = prepare(
                core,
                &rom,
                &core_hash,
                &pair.incumbent_input,
                pair.incumbent,
            )?;
            prefix_frames += cf + inf;
            pair_count += 1;
            for (trial, suffix) in suffixes.iter().enumerate() {
                let c = extend(&mut candidate, &cs, suffix)?;
                let i = extend(&mut incumbent, &is, suffix)?;
                let (discarded, survivor) = if pair.replaces { (&i, &c) } else { (&c, &i) };
                probe_frames[0] += discarded.frames;
                probe_frames[1] += survivor.frames;
                let discarded_gain = discarded.equipment_gained & !survivor.equipment_gained != 0
                    || (discarded.boss_gain && !survivor.boss_gain)
                    || (discarded.capacity_gain && !survivor.capacity_gain);
                let survivor_gain = survivor.equipment_gained & !discarded.equipment_gained != 0
                    || (survivor.boss_gain && !discarded.boss_gain)
                    || (survivor.capacity_gain && !discarded.capacity_gain);
                let discarded_exit = discarded
                    .reached_maps
                    .difference(&survivor.reached_maps)
                    .next()
                    .is_some();
                let survivor_exit = survivor
                    .reached_maps
                    .difference(&discarded.reached_maps)
                    .next()
                    .is_some();
                totals[stratum][0] += 1;
                totals[stratum][1] += u64::from(discarded_gain);
                totals[stratum][2] += u64::from(survivor_gain);
                totals[stratum][3] += u64::from(discarded_exit);
                totals[stratum][4] += u64::from(survivor_exit);
                totals[stratum][5] += u64::from(!discarded.dead && survivor.dead);
                if discarded_gain {
                    let mut input = if pair.replaces {
                        pair.incumbent_input.clone()
                    } else {
                        pair.candidate_input.clone()
                    };
                    // Suffix may terminate early: retain only through the first terminal action.
                    let mut replay =
                        MetroidTarget::from_rom_bytes_headless(&rom, core, &core_hash)?;
                    for action in &input.actions {
                        replay.apply(action);
                    }
                    for action in suffix {
                        if replay.is_dead() || replay.is_victory() {
                            break;
                        }
                        replay.apply(action);
                        input.actions.push(*action);
                    }
                    prefix_frames += replay.frames_clocked();
                    fs::write(
                        out.join(format!("gain-s{stratum}-p{index}-t{trial}.json")),
                        serde_json::to_vec(&input)?,
                    )?;
                }
                writeln!(
                    log,
                    "{}",
                    json!({"stratum":stratum,"pair":index,"execution":pair.execution,"trial":trial,"candidate_replaces":pair.replaces,"discarded":discarded,"survivor":survivor,"discarded_only_gain":discarded_gain,"survivor_only_gain":survivor_gain,"discarded_only_exit":discarded_exit,"survivor_only_exit":survivor_exit})
                )?;
                log.flush()?;
            }
        }
    }
    let report = json!({"format":"metroid-equal-suffix-probe-v1","scope":"development-discovered competitors; diagnostic, not fresh search","rom_sha256":format!("{:x}",Sha256::digest(&rom)),"core_sha256":core_hash,"audit_sha256":format!("{:x}",Sha256::digest(&bytes)),"pairs":pair_count,"trials_per_pair":trials,"actions_per_trial":actions,"equal_work_rule":"identical pre-sampled action suffixes and frame allowances; terminal branches stop early, actual frames reported separately","totals_columns":["trials","discarded_only_gain","survivor_only_gain","discarded_only_living_map_exit","survivor_only_living_map_exit","discarded_survives_survivor_dies"],"totals_by_stratum":totals,"prefix_and_gain_export_frames":prefix_frames,"probe_frames_discarded_survivor":probe_frames,"limitations":["cached incumbent sample only","finite matching suffixes do not prove equivalence","map exit is local reach evidence, not a boss or new global coverage claim","resource gain is relative to each starting state"]});
    fs::write(
        out.join("summary.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{report}");
    Ok(())
}
