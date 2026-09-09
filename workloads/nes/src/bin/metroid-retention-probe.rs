// SPDX-License-Identifier: AGPL-3.0-or-later
//! Equal-suffix diagnostic on sampled competing states; never fresh validation.

use nes_workload::{
    metroid::{
        archive::sample_chord,
        retention_audit::{MAX_ACTIONS, PER_STRATUM, ReplacementPair, STRATA},
        target::{
            ButtonChord, MetroidInput, MetroidMechanicalState, MetroidSnapshot, MetroidTarget,
            MetroidTerminalPolicy,
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
    io::{BufWriter, Read, Write},
    path::Path,
};

#[derive(Deserialize)]
struct Audit {
    format: String,
    samples: Vec<Vec<ReplacementPair>>,
}
const MAX_AUDIT_BYTES: u64 = 64 * 1024 * 1024;

fn validate_audit(audit: &Audit) -> Result<(), Box<dyn Error>> {
    if audit.format != "metroid-retention-audit-v1" || audit.samples.len() != STRATA {
        return Err("unsupported audit format or category count".into());
    }
    for (stratum, pairs) in audit.samples.iter().enumerate() {
        if pairs.len() > PER_STRATUM {
            return Err("audit exceeds bounded pair count".into());
        }
        for pair in pairs {
            if pair.stratum != stratum
                || pair.candidate_input.actions.len() > MAX_ACTIONS
                || pair.incumbent_input.actions.len() > MAX_ACTIONS
            {
                return Err("audit pair category or input exceeds bounds".into());
            }
        }
    }
    Ok(())
}

fn read_audit(path: &Path) -> Result<(Audit, Vec<u8>), Box<dyn Error>> {
    let file = fs::File::open(path)?;
    if file.metadata()?.len() > MAX_AUDIT_BYTES {
        return Err("audit exceeds 64 MiB".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_AUDIT_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_AUDIT_BYTES {
        return Err("audit exceeds 64 MiB".into());
    }
    let audit: Audit = serde_json::from_slice(&bytes)?;
    validate_audit(&audit)?;
    Ok((audit, bytes))
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
#[allow(clippy::too_many_arguments)]
fn prepare(
    core: &Path,
    rom: &[u8],
    hash: &str,
    input: &MetroidInput,
    expected: MetroidMechanicalState,
    policy: MetroidTerminalPolicy,
    mut known_maps: Option<&mut BTreeSet<(u8, u8, u8)>>,
) -> Result<(MetroidTarget, MetroidSnapshot, u64), Box<dyn Error>> {
    let mut target =
        MetroidTarget::from_rom_bytes_headless(rom, core, hash)?.with_terminal_policy(policy);
    if let Some(known) = known_maps.as_deref_mut() {
        record_living_maps(&target, policy, known);
    }
    for action in &input.actions {
        if target.is_dead() || target.is_victory() {
            return Err("sample input continues after terminal".into());
        }
        target.apply(action);
        if target.exit_kind() != ExitKind::Ok {
            return Err("sample replay emulator failure".into());
        }
        if let Some(known) = known_maps.as_deref_mut() {
            record_living_maps(&target, policy, known);
        }
    }
    if target.mechanical_state() != expected {
        return Err("sample replay endpoint differs from recorded competitor".into());
    }
    let snapshot = target.snapshot().ok_or("sample snapshot failed")?;
    let frames = target.frames_clocked();
    Ok((target, snapshot, frames))
}
fn record_living_maps(
    target: &MetroidTarget,
    policy: MetroidTerminalPolicy,
    maps: &mut BTreeSet<(u8, u8, u8)>,
) {
    for observation in target.last_action_observations() {
        let state = observation.decoded;
        if state.in_play() && !policy.is_dead(state) {
            maps.insert((state.area, state.map_x, state.map_y));
        }
    }
}
fn capped_action(action: ButtonChord, remaining: u64) -> Option<ButtonChord> {
    let hold = u64::from(action.bounded_hold_frames()).min(remaining);
    (hold > 0).then(|| ButtonChord::new(action.buttons, hold as u8))
}
fn extend(
    target: &mut MetroidTarget,
    snapshot: &MetroidSnapshot,
    suffix: &[ButtonChord],
    policy: MetroidTerminalPolicy,
) -> Result<Outcome, Box<dyn Error>> {
    Ok(extend_budget(target, snapshot, suffix, policy, u64::MAX)?.0)
}
fn extend_budget(
    target: &mut MetroidTarget,
    snapshot: &MetroidSnapshot,
    suffix: &[ButtonChord],
    policy: MetroidTerminalPolicy,
    frame_budget: u64,
) -> Result<(Outcome, Vec<ButtonChord>), Box<dyn Error>> {
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
    let mut applied = Vec::new();
    for action in suffix {
        if target.is_dead() || target.is_victory() {
            break;
        }
        let remaining = frame_budget.saturating_sub(target.frames_clocked() - before);
        let Some(action) = capped_action(*action, remaining) else {
            break;
        };
        target.apply(&action);
        applied.push(action);
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
                && !policy.is_dead(s)
                && (s.area, s.map_x, s.map_y) != (start.area, start.map_x, start.map_y)
            {
                outcome.reached_maps.insert((s.area, s.map_x, s.map_y));
            }
        }
    }
    outcome.frames = target.frames_clocked() - before;
    outcome.dead = target.is_dead();
    outcome.endpoint = target.mechanical_state();
    Ok((outcome, applied))
}
fn validate_fixed_work(audit: &Audit, budget: u64) -> Result<(), Box<dyn Error>> {
    let pairs = audit.samples.iter().map(Vec::len).sum::<usize>();
    if pairs == 0 || pairs > 16 || budget == 0 || budget > 100_000 {
        return Err("fixed-work probe exceeds 16 pairs or 100k frames per side".into());
    }
    let frames: u64 = audit
        .samples
        .iter()
        .flatten()
        .flat_map(|p| {
            p.candidate_input
                .actions
                .iter()
                .chain(&p.incumbent_input.actions)
        })
        .map(|a| u64::from(a.bounded_hold_frames()))
        .sum();
    if frames > 2_000_000 {
        return Err("fixed-work source reconstruction exceeds 2M action frames".into());
    }
    Ok(())
}

#[derive(Serialize)]
struct FixedSide {
    frames: u64,
    attempts: usize,
    dead_endpoints: usize,
    reached_maps: BTreeSet<(u8, u8, u8)>,
    beyond_known_source_maps: BTreeSet<(u8, u8, u8)>,
    equipment_gained: u8,
    capacity_gain: bool,
    boss_gain: bool,
    witness_files: Vec<String>,
}

// All probe work is outside campaign search and reported separately.
#[allow(clippy::too_many_arguments)]
fn fixed_side(
    target: &mut MetroidTarget,
    snapshot: &MetroidSnapshot,
    prefix: &MetroidInput,
    known: &BTreeSet<(u8, u8, u8)>,
    suffixes: &[Vec<ButtonChord>],
    policy: MetroidTerminalPolicy,
    budget: u64,
    out: &Path,
    label: &str,
    log: &mut BufWriter<fs::File>,
) -> Result<FixedSide, Box<dyn Error>> {
    let mut result = FixedSide {
        frames: 0,
        attempts: 0,
        dead_endpoints: 0,
        reached_maps: BTreeSet::new(),
        beyond_known_source_maps: BTreeSet::new(),
        equipment_gained: 0,
        capacity_gain: false,
        boss_gain: false,
        witness_files: Vec::new(),
    };
    for (trial, suffix) in suffixes.iter().enumerate() {
        if result.frames == budget {
            break;
        }
        let (outcome, applied) =
            extend_budget(target, snapshot, suffix, policy, budget - result.frames)?;
        if outcome.frames == 0 || outcome.frames > budget - result.frames {
            return Err("fixed-work side made no progress or exceeded physical budget".into());
        }
        let unknown: BTreeSet<_> = outcome.reached_maps.difference(known).copied().collect();
        let new_unknown = unknown
            .difference(&result.beyond_known_source_maps)
            .next()
            .is_some();
        let new_gain = outcome.equipment_gained & !result.equipment_gained != 0
            || (outcome.capacity_gain && !result.capacity_gain)
            || (outcome.boss_gain && !result.boss_gain);
        if new_unknown || new_gain {
            if result.witness_files.len() >= 32 {
                return Err("fixed-work witness export exceeds 32 per side".into());
            }
            let mut input = prefix.clone();
            input.actions.extend(applied);
            let file = format!("witness-{label}-t{trial}.json");
            fs::write(out.join(&file), serde_json::to_vec(&input)?)?;
            result.witness_files.push(file);
        }
        result.frames += outcome.frames;
        result.attempts += 1;
        result.dead_endpoints += usize::from(outcome.dead);
        result.reached_maps.extend(&outcome.reached_maps);
        result.beyond_known_source_maps.extend(unknown);
        result.equipment_gained |= outcome.equipment_gained;
        result.capacity_gain |= outcome.capacity_gain;
        result.boss_gain |= outcome.boss_gain;
        writeln!(
            log,
            "{}",
            json!({"side":label,"trial":trial,"cumulative_frames":result.frames,"outcome":outcome})
        )?;
        log.flush()?;
    }
    if result.frames != budget {
        return Err("fixed-work suffix bank exhausted before matched physical budget".into());
    }
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
fn fixed_work(
    audit: &Audit,
    audit_bytes: &[u8],
    core: &Path,
    rom: &[u8],
    core_hash: &str,
    out: &Path,
    suffixes: &[Vec<ButtonChord>],
    policy: MetroidTerminalPolicy,
    budget: u64,
) -> Result<(), Box<dyn Error>> {
    let mut log = BufWriter::new(fs::File::create(out.join("outcomes.jsonl"))?);
    let mut rows = Vec::new();
    let mut prefix_frames = 0;
    let mut snapshot_bytes_peak = 0;
    for (stratum, pairs) in audit.samples.iter().enumerate() {
        for (index, pair) in pairs.iter().enumerate() {
            let mut known = BTreeSet::new();
            let (mut candidate, cs, cf) = prepare(
                core,
                rom,
                core_hash,
                &pair.candidate_input,
                pair.candidate,
                policy,
                Some(&mut known),
            )?;
            let (mut incumbent, is, inf) = prepare(
                core,
                rom,
                core_hash,
                &pair.incumbent_input,
                pair.incumbent,
                policy,
                Some(&mut known),
            )?;
            prefix_frames += cf + inf;
            snapshot_bytes_peak = snapshot_bytes_peak
                .max(postcard::to_allocvec(&cs)?.len() + postcard::to_allocvec(&is)?.len());
            let c = fixed_side(
                &mut candidate,
                &cs,
                &pair.candidate_input,
                &known,
                suffixes,
                policy,
                budget,
                out,
                &format!("s{stratum}-p{index}-candidate"),
                &mut log,
            )?;
            let i = fixed_side(
                &mut incumbent,
                &is,
                &pair.incumbent_input,
                &known,
                suffixes,
                policy,
                budget,
                out,
                &format!("s{stratum}-p{index}-incumbent"),
                &mut log,
            )?;
            let (discarded, survivor) = if pair.replaces { (i, c) } else { (c, i) };
            rows.push(json!({"stratum":stratum,"pair":index,"execution":pair.execution,
                "candidate_replaces":pair.replaces,"known_source_maps":known,
                "discarded_only_maps":discarded.reached_maps.difference(&survivor.reached_maps).collect::<Vec<_>>(),
                "survivor_only_maps":survivor.reached_maps.difference(&discarded.reached_maps).collect::<Vec<_>>(),
                "discarded_only_beyond_known":discarded.beyond_known_source_maps.difference(&survivor.beyond_known_source_maps).collect::<Vec<_>>(),
                "survivor_only_beyond_known":survivor.beyond_known_source_maps.difference(&discarded.beyond_known_source_maps).collect::<Vec<_>>(),
                "discarded":discarded,"survivor":survivor}));
            fs::write(
                out.join("completed-pairs.json"),
                serde_json::to_vec_pretty(&rows)?,
            )?;
        }
    }
    let report = json!({"format":"metroid-fixed-work-probe-v1","terminal_policy":policy.identifier(),
        "scope":"development source-pair counterfactual, not fresh search or full-archive novelty",
        "rom_sha256":format!("{:x}",Sha256::digest(rom)),"core_sha256":core_hash,
        "audit_sha256":format!("{:x}",Sha256::digest(audit_bytes)),
        "suffixes_sha256":format!("{:x}",Sha256::digest(fs::read(out.join("suffixes.json"))?)),
        "outcomes_sha256":format!("{:x}",Sha256::digest(fs::read(out.join("outcomes.jsonl"))?)),
        "frames_per_side":budget,"suffix_bank_trials":suffixes.len(),"actions_per_trial":suffixes[0].len(),
        "prefix_including_bootstrap_frames":prefix_frames,"probe_frames":2*budget*rows.len() as u64,
        "two_source_snapshot_serialized_bytes_peak":snapshot_bytes_peak,"pairs":rows,
        "limitations":["Source paths bound prior observed map coverage from below; absence is unknown against the complete campaign.",
        "Equal physical frame budgets give early-terminal sources more attempts; final action is clipped at the cap.",
        "Witness exports are unverified until independent ordinary-genesis replay.",
        "Source sampling is conditional on cached snapshots and bounded input history; one development campaign."]});
    fs::write(
        out.join("summary.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!(
        "{}",
        json!({"format":"metroid-fixed-work-probe-v1","pairs":report["pairs"].as_array().map(Vec::len),"probe_frames":report["probe_frames"],"prefix_frames":prefix_frames})
    );
    Ok(())
}

fn verify_sources(args: &[String]) -> Result<(), Box<dyn Error>> {
    if args.len() != 5 {
        return Err("usage: metroid-retention-probe --verify-sources CORE ROM AUDIT OUTPUT".into());
    }
    let (audit, bytes) = read_audit(Path::new(&args[3]))?;
    validate_fixed_work(&audit, 1)?;
    let core = Path::new(&args[1]);
    let rom = fs::read(&args[2])?;
    let core_hash = format!("{:x}", Sha256::digest(fs::read(core)?));
    let out = Path::new(&args[4]);
    fs::create_dir(out)?;
    let mut rows = Vec::new();
    let mut physical_frames = 0;
    for (stratum, pairs) in audit.samples.iter().enumerate() {
        for (index, pair) in pairs.iter().enumerate() {
            let mut sides = Vec::new();
            for (input, state) in [
                (&pair.candidate_input, pair.candidate),
                (&pair.incumbent_input, pair.incumbent),
            ] {
                let mut maps = BTreeSet::new();
                let (target, snapshot, frames) = prepare(
                    core,
                    &rom,
                    &core_hash,
                    input,
                    state,
                    MetroidTerminalPolicy::BcdUnderflow,
                    Some(&mut maps),
                )?;
                physical_frames += frames;
                sides.push(json!({"endpoint":target.mechanical_state(),"dead":target.is_dead(),"living_maps":maps,"physical_frames":frames,"snapshot_sha256":format!("{:x}", Sha256::digest(postcard::to_allocvec(&snapshot)?))}));
            }
            rows.push(json!({"stratum":stratum,"pair":index,"sides":sides}));
            fs::write(
                out.join("completed-pairs.json"),
                serde_json::to_vec_pretty(&rows)?,
            )?;
        }
    }
    let result = json!({"format":"metroid-source-replay-v1","audit_sha256":format!("{:x}",Sha256::digest(bytes)),"core_sha256":core_hash,"rom_sha256":format!("{:x}",Sha256::digest(rom)),"physical_frames":physical_frames,"pairs":rows});
    fs::write(
        out.join("summary.json"),
        serde_json::to_vec_pretty(&result)?,
    )?;
    println!("{}", json!({"physical_frames":physical_frames}));
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|v| v == "--verify-sources") {
        return verify_sources(&args);
    }
    if !(6..=8).contains(&args.len()) {
        return Err(
            "usage: metroid-retention-probe CORE ROM AUDIT OUT TRIALS ACTIONS [TERMINAL_POLICY] [FRAMES_PER_SIDE]"
                .into(),
        );
    }
    let policy =
        MetroidTerminalPolicy::parse(args.get(6).map_or("death_or_ending_v2", String::as_str))?;
    let core = Path::new(&args[0]);
    let (audit, bytes) = read_audit(Path::new(&args[2]))?;
    let out = Path::new(&args[3]);
    let trials: usize = args[4].parse()?;
    let actions: usize = args[5].parse()?;
    let frame_budget = args.get(7).map(|v| v.parse::<u64>()).transpose()?;
    let max_trials = if frame_budget.is_some() { 4096 } else { 256 };
    if trials == 0 || trials > max_trials || actions == 0 || actions > 128 {
        return Err("probe limits exceed bounded diagnostic".into());
    }
    if let Some(budget) = frame_budget {
        validate_fixed_work(&audit, budget)?;
    }
    let rom = fs::read(&args[1])?;
    fs::create_dir(out)?;
    let core_hash = format!("{:x}", Sha256::digest(fs::read(core)?));
    let mut rng = RomuDuoJrRand::with_seed(0x616c_7466_7574_7572);
    let suffixes: Vec<Vec<ButtonChord>> = (0..trials)
        .map(|_| (0..actions).map(|_| sample_chord(&mut rng)).collect())
        .collect::<Result<_, _>>()?;
    fs::write(out.join("suffixes.json"), serde_json::to_vec(&suffixes)?)?;
    if let Some(budget) = frame_budget {
        return fixed_work(
            &audit, &bytes, core, &rom, &core_hash, out, &suffixes, policy, budget,
        );
    }
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
                policy,
                None,
            )?;
            let (mut incumbent, is, inf) = prepare(
                core,
                &rom,
                &core_hash,
                &pair.incumbent_input,
                pair.incumbent,
                policy,
                None,
            )?;
            prefix_frames += cf + inf;
            pair_count += 1;
            for (trial, suffix) in suffixes.iter().enumerate() {
                let c = extend(&mut candidate, &cs, suffix, policy)?;
                let i = extend(&mut incumbent, &is, suffix, policy)?;
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
                        MetroidTarget::from_rom_bytes_headless(&rom, core, &core_hash)?
                            .with_terminal_policy(policy);
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
    let report = json!({"format":"metroid-equal-suffix-probe-v1","terminal_policy":policy.identifier(),"scope":"development-discovered competitors; diagnostic, not fresh search","rom_sha256":format!("{:x}",Sha256::digest(&rom)),"core_sha256":core_hash,"audit_sha256":format!("{:x}",Sha256::digest(&bytes)),"pairs":pair_count,"trials_per_pair":trials,"actions_per_trial":actions,"equal_work_rule":"identical pre-sampled action suffixes and frame allowances; terminal branches stop early, actual frames reported separately","totals_columns":["trials","discarded_only_gain","survivor_only_gain","discarded_only_living_map_exit","survivor_only_living_map_exit","discarded_survives_survivor_dies"],"totals_by_stratum":totals,"prefix_and_gain_export_frames":prefix_frames,"probe_frames_discarded_survivor":probe_frames,"limitations":["cached incumbent sample only","finite matching suffixes do not prove equivalence","map exit is local reach evidence, not a boss or new global coverage claim","resource gain is relative to each starting state"]});
    fs::write(
        out.join("summary.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{report}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        Audit, MAX_ACTIONS, MAX_AUDIT_BYTES, PER_STRATUM, STRATA, read_audit, validate_audit,
    };
    use nes_workload::metroid::{
        retention_audit::ReplacementPair,
        target::{ButtonChord, MetroidInput},
    };

    fn pair() -> ReplacementPair {
        ReplacementPair {
            stratum: 0,
            execution: 0,
            replaces: true,
            candidate: Default::default(),
            incumbent: Default::default(),
            created_execution: 0,
            exposure: Default::default(),
            in_window_ever: false,
            candidate_input: MetroidInput { actions: vec![] },
            incumbent_input: MetroidInput { actions: vec![] },
        }
    }

    #[test]
    fn physical_budget_never_normalizes_zero_into_an_extra_frame() {
        use super::capped_action;
        let action = ButtonChord::new(0x83, 120);
        assert!(capped_action(action, 0).is_none());
        assert_eq!(capped_action(action, 1), Some(ButtonChord::new(0x83, 1)));
        assert_eq!(
            capped_action(action, 119),
            Some(ButtonChord::new(0x83, 119))
        );
        assert_eq!(capped_action(action, 121), Some(action));
    }

    #[test]
    fn fixed_work_rejects_aggregate_prefix_work_before_replay() {
        use super::validate_fixed_work;
        let mut audit = Audit {
            format: "metroid-retention-audit-v1".into(),
            samples: vec![vec![]; STRATA],
        };
        assert!(validate_fixed_work(&audit, 100_000).is_err());
        audit.samples[0].push(pair());
        assert!(validate_fixed_work(&audit, 100_000).is_ok());
        assert!(validate_fixed_work(&audit, 100_001).is_err());
        audit.samples[0][0].candidate_input.actions = vec![ButtonChord::new(0, 120); MAX_ACTIONS];
        audit.samples[0][0].incumbent_input.actions = vec![ButtonChord::new(0, 120); MAX_ACTIONS];
        let second = audit.samples[0][0].clone();
        audit.samples[0].push(second);
        assert!(validate_fixed_work(&audit, 100_000).is_err());
    }

    #[test]
    fn external_audit_shape_and_source_lengths_are_bounded() {
        let mut audit = Audit {
            format: "metroid-retention-audit-v1".into(),
            samples: vec![vec![]; STRATA],
        };
        audit.samples[0].push(pair());
        assert!(validate_audit(&audit).is_ok());
        audit.samples[0][0].stratum = 1;
        assert!(validate_audit(&audit).is_err());
        audit.samples[0] = vec![pair(); PER_STRATUM + 1];
        assert!(validate_audit(&audit).is_err());
        audit.samples[0] = vec![pair()];
        audit.samples[0][0].candidate_input.actions = vec![ButtonChord::new(0, 1); MAX_ACTIONS + 1];
        assert!(validate_audit(&audit).is_err());
        audit.samples[0].clear();
        audit.samples.push(vec![]);
        assert!(validate_audit(&audit).is_err());
    }

    #[test]
    fn oversized_audit_is_rejected_before_deserialization() {
        let path =
            std::env::temp_dir().join(format!("harmony-audit-byte-bound-{}", std::process::id()));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        file.set_len(MAX_AUDIT_BYTES + 1).unwrap();
        let error = read_audit(&path).err().unwrap().to_string();
        std::fs::remove_file(path).unwrap();
        assert_eq!(error, "audit exceeds 64 MiB");
    }
}
