// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ordinary continuations from a fresh chain's own trajectory; never validation.
use nes_workload::{
    mm2::{
        archive::sample_chord,
        target::{
            ButtonChord, MENU_CLOSED, Mm2Input, Mm2MechanicalState, Mm2Snapshot, Mm2Stage,
            Mm2Target,
        },
    },
    search::rand::RomuDuoJrRand,
    target::{ExitKind, Target},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fs,
    io::{BufWriter, Read, Write},
    path::{Path, PathBuf},
};
type Result<T> = std::result::Result<T, Box<dyn Error>>;
type Cell = [u8; 5];
const TOTAL_FRAMES: u64 = 4_000_000;
// Mm2Target's award wait is at most 900 rounded up to a MAX_HOLD_FRAMES block,
// plus one held action. Reserving 2048 conservatively bounds the next action.
const ACTION_RESERVE: u64 = 2048;
#[derive(Deserialize)]
struct Request {
    core: PathBuf,
    rom: PathBuf,
    prefix: PathBuf,
    prefix_sha256: String,
    input: PathBuf,
    input_sha256: String,
    source_snapshot_sha256: String,
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err("file exceeds bounded diagnostic input".into());
    }
    Ok(bytes)
}
fn input(path: &Path, expected: &str) -> Result<Mm2Input> {
    let bytes = read_bounded(path, 2_097_152)?;
    if hash(&bytes) != expected {
        return Err("input checksum mismatch".into());
    }
    Ok(serde_json::from_slice(&bytes)?)
}
fn cell(s: Mm2MechanicalState) -> Cell {
    [s.stage, s.screen, s.room, s.x / 16, s.y / 16]
}
#[derive(Clone, Serialize)]
struct Boundary {
    actions: usize,
    state: Mm2MechanicalState,
}
fn choose(boundaries: &[Boundary]) -> Vec<(String, Boundary)> {
    let screens: BTreeSet<_> = boundaries.iter().map(|b| b.state.screen).collect();
    let screens: Vec<_> = screens.into_iter().collect();
    let Some(&maximum) = screens.last() else {
        return Vec::new();
    };
    let median = screens[screens.len() / 2];
    let mut result = Vec::new();
    for (label, screen, count) in [("frontier", maximum, 8), ("calibration", median, 4)] {
        if label == "calibration" && median == maximum {
            continue;
        }
        let mut seen = BTreeSet::new();
        for b in boundaries.iter().filter(|b| b.state.screen == screen) {
            if seen.insert(cell(b.state)) {
                result.push((label.into(), b.clone()));
            }
            if seen.len() == count {
                break;
            }
        }
    }
    result
}
fn apply(target: &mut Mm2Target, action: &ButtonChord, spent: &mut u64) -> Result<()> {
    if *spent > TOTAL_FRAMES - ACTION_RESERVE {
        return Err("physical diagnostic budget exhausted".into());
    }
    let before = target.frames_clocked();
    target.apply(action);
    *spent += target.frames_clocked() - before;
    if target.exit_kind() != ExitKind::Ok || target.frames_clocked() == before {
        return Err("emulator failure or no outgoing work".into());
    }
    if *spent > TOTAL_FRAMES {
        return Err("automatic action work exceeded reserved budget".into());
    }
    Ok(())
}
fn observe(target: &Mm2Target, known: &mut BTreeSet<Cell>) {
    for o in target.last_action_observations() {
        if !o.dead && o.decoded.menu == MENU_CLOSED && o.decoded.camera_state != 0x80 {
            known.insert(cell(o.decoded));
        }
    }
}
fn eligible(target: &Mm2Target) -> bool {
    let s = target.mechanical_state();
    !target.is_dead()
        && !target.defeated_a_boss()
        && s.menu == MENU_CLOSED
        && s.camera_state != 0x80
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        return Err("usage: mm2-future-probe REQUEST OUTPUT".into());
    }
    let bytes = read_bounded(Path::new(&args[0]), 65_536)?;
    let request: Request = serde_json::from_slice(&bytes)?;
    let prefix = input(&request.prefix, &request.prefix_sha256)?;
    let source = input(&request.input, &request.input_sha256)?;
    if prefix.actions.len() + source.actions.len() > 20_000
        || prefix
            .actions
            .iter()
            .chain(&source.actions)
            .map(|a| u64::from(a.bounded_hold_frames()))
            .sum::<u64>()
            > 2_000_000
    {
        return Err("combined source tape exceeds action/frame bound".into());
    }
    let out = Path::new(&args[1]);
    fs::create_dir(out)?;
    let rom = read_bounded(&request.rom, 4_194_304)?;
    let core_hash = hash(&read_bounded(&request.core, 67_108_864)?);
    let mut target = Mm2Target::from_rom_bytes_after(
        &rom,
        &request.core,
        &core_hash,
        &prefix.actions,
        Mm2Stage::parse("wily1")?,
    )?;
    let origin = target.mechanical_state();
    if target.genesis_weapons() != 255 {
        return Err("diagnostic source lacks all eight weapons".into());
    }
    let mut spent = target.frames_clocked();
    let mut known = BTreeSet::new();
    observe(&target, &mut known);
    let mut boundaries = Vec::new();
    if eligible(&target) {
        boundaries.push(Boundary {
            actions: 0,
            state: origin,
        });
    }
    for (index, action) in source.actions.iter().enumerate() {
        if target.is_dead() || target.defeated_a_boss() {
            return Err("source continues after terminal".into());
        }
        apply(&mut target, action, &mut spent)?;
        observe(&target, &mut known);
        if eligible(&target) {
            boundaries.push(Boundary {
                actions: index + 1,
                state: target.mechanical_state(),
            });
        }
    }
    let endpoint = target.mechanical_state();
    let endpoint_snapshot = target.snapshot().ok_or("source endpoint snapshot failed")?;
    let source_snapshot_sha256 = hash(&postcard::to_allocvec(&endpoint_snapshot)?);
    if source_snapshot_sha256 != request.source_snapshot_sha256 {
        return Err("source endpoint differs from the frozen chain witness snapshot".into());
    }
    let selected = choose(&boundaries);
    if selected.is_empty() {
        return Err("no eligible source boundaries".into());
    }
    let frontier_sources = selected
        .iter()
        .filter(|(label, _)| label == "frontier")
        .count();
    let calibration_sources = selected.len() - frontier_sources;
    let missing_calibration = calibration_sources == 0;
    fs::write(
        out.join("sources.json"),
        serde_json::to_vec_pretty(
            &json!({"origin":origin,"endpoint":endpoint,"source_snapshot_sha256":source_snapshot_sha256,"missing_calibration":missing_calibration,"frontier_sources":frontier_sources,"calibration_sources":calibration_sources,"eligible_boundaries":boundaries,"selected":selected,"known_trajectory_cells":known}),
        )?,
    )?;
    let wanted: BTreeSet<_> = selected.iter().map(|(_, b)| b.actions).collect();
    // Mm2Target::reset calls QuickNesMachine::replay, which restores a snapshot
    // without running emulator frames. The second traversal is charged below.
    target.reset();
    if target.exit_kind() != ExitKind::Ok || target.mechanical_state() != origin {
        return Err("reset did not restore the recorded origin".into());
    }
    let mut snapshots: BTreeMap<usize, Mm2Snapshot> = BTreeMap::new();
    if wanted.contains(&0) {
        snapshots.insert(0, target.snapshot().ok_or("snapshot failed")?);
    }
    for (index, action) in source.actions.iter().enumerate() {
        apply(&mut target, action, &mut spent)?;
        if wanted.contains(&(index + 1)) {
            snapshots.insert(index + 1, target.snapshot().ok_or("snapshot failed")?);
        }
    }
    if target.mechanical_state() != endpoint {
        return Err("source replay endpoint mismatch".into());
    }
    let reconstruction_frames = spent;
    let snapshot_bytes: usize = snapshots
        .values()
        .map(|s| postcard::to_allocvec(s).map(|v| v.len()))
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .sum();
    let mut rng = RomuDuoJrRand::with_seed(0x616c_7466_7574_7572);
    let suffixes: Vec<Vec<ButtonChord>> = (0..64)
        .map(|_| (0..24).map(|_| sample_chord(&mut rng)).collect())
        .collect::<Result<_>>()?;
    let suffix_bytes = serde_json::to_vec(&suffixes)?;
    fs::write(out.join("suffixes.json"), &suffix_bytes)?;
    let mut log = BufWriter::new(fs::File::create(out.join("outcomes.jsonl"))?);
    let mut reports = Vec::new();
    for (label, boundary) in selected {
        let snapshot = snapshots
            .get(&boundary.actions)
            .ok_or("selected snapshot missing")?;
        let mut reached: BTreeSet<Cell> = BTreeSet::new();
        let mut deaths = 0;
        let mut extension_trials = 0;
        let mut unknown_trials = 0;
        let mut source_frames = 0;
        let mut max_screen = boundary.state.screen;
        let mut boss_gains = 0;
        for (trial, suffix) in suffixes.iter().enumerate() {
            target.restore(snapshot)?;
            if target.mechanical_state() != boundary.state {
                return Err("restored source mismatch".into());
            }
            let before = spent;
            let mut trial_cells = BTreeSet::new();
            let mut scrolling = 0;
            let mut menu = 0;
            let mut used = 0;
            for action in suffix {
                if target.is_dead() || target.defeated_a_boss() {
                    break;
                }
                apply(&mut target, action, &mut spent)?;
                used += 1;
                observe(&target, &mut trial_cells);
                for o in target.last_action_observations() {
                    scrolling += usize::from(o.decoded.camera_state == 0x80);
                    menu += usize::from(o.decoded.menu != MENU_CLOSED);
                }
            }
            let dead = target.is_dead();
            let gain = target.defeated_a_boss();
            let extension = trial_cells.iter().any(|c| *c != cell(boundary.state));
            let unknown = trial_cells.difference(&known).next().is_some();
            if eligible(&target) {
                max_screen = max_screen.max(target.mechanical_state().screen);
            }
            deaths += usize::from(dead);
            boss_gains += usize::from(gain);
            extension_trials += usize::from(extension);
            unknown_trials += usize::from(unknown);
            source_frames += spent - before;
            reached.extend(&trial_cells);
            if gain {
                let mut witness = Mm2Input {
                    actions: source.actions[..boundary.actions].to_vec(),
                };
                witness.actions.extend(&suffix[..used]);
                let file = format!("gain-a{}-t{trial}.json", boundary.actions);
                let witness_bytes = serde_json::to_vec(&witness)?;
                fs::write(out.join(&file), &witness_bytes)?;
                fs::write(
                    out.join(format!("{file}.proof.json")),
                    serde_json::to_vec_pretty(&json!({
                        "prefix_sha256":request.prefix_sha256,"input_sha256":hash(&witness_bytes),
                        "source_input_sha256":request.input_sha256,"source_actions":boundary.actions,
                        "core_sha256":core_hash,"rom_sha256":hash(&rom),"stage":"wily1",
                        "verification":"unverified composed own-chain input"
                    }))?,
                )?;
            }
            writeln!(
                log,
                "{}",
                json!({"label":label,"source_actions":boundary.actions,"trial":trial,"frames":spent-before,"dead":dead,"boss_gain":gain,"living_cells":trial_cells,"extension":extension,"beyond_known_trajectory":unknown,"scrolling_observations":scrolling,"menu_observations":menu,"endpoint":target.mechanical_state()})
            )?;
            log.flush()?;
        }
        reports.push(json!({"label":label,"source":boundary,"trials":64,"frames":source_frames,"deaths":deaths,"extension_trials":extension_trials,"beyond_known_trajectory_trials":unknown_trials,"max_living_non_scroll_endpoint_screen":max_screen,"boss_gains_unverified":boss_gains,"reached_cells":reached}));
        fs::write(
            out.join("completed-sources.json"),
            serde_json::to_vec_pretty(&reports)?,
        )?;
    }
    let report = json!({"format":"mm2-trajectory-future-probe-v2","request_sha256":hash(&bytes),"core_sha256":core_hash,"rom_sha256":hash(&rom),"prefix_sha256":request.prefix_sha256,"input_sha256":request.input_sha256,"suffixes_sha256":hash(&suffix_bytes),"origin":origin,"endpoint":endpoint,"source_snapshot_sha256":source_snapshot_sha256,"missing_calibration":missing_calibration,"frontier_sources":frontier_sources,"calibration_sources":calibration_sources,"reconstruction_frames_including_bootstrap":reconstruction_frames,"probe_frames":spent-reconstruction_frames,"total_physical_frames":spent,"snapshot_serialized_bytes":snapshot_bytes,"sources":reports,"limitations":["Trajectory-conditioned starts, not a complete retained-archive sample.","Source-path absence is unknown against the whole campaign.","Sparse scrolling observations are not frame durations or proof of control availability.","Equal suffix allowances, early terminal stops: actual work differs across sources.","Boss/stage gains need independent ordinary-genesis witness verification."]});
    fs::write(
        out.join("summary.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!(
        "{}",
        json!({"total_physical_frames":spent,"sources":reports.len()})
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn two_screens_have_an_explicit_missing_upper_median_calibration() {
        let bs = [
            Boundary {
                actions: 0,
                state: Mm2MechanicalState {
                    screen: 1,
                    ..Default::default()
                },
            },
            Boundary {
                actions: 1,
                state: Mm2MechanicalState {
                    screen: 2,
                    ..Default::default()
                },
            },
        ];
        let selected = choose(&bs);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].0, "frontier");
        assert_eq!(selected[0].1.state.screen, 2);
        assert!(!selected.iter().any(|(label, _)| label == "calibration"));
    }
    #[test]
    fn source_selection_deduplicates_cells_and_keeps_calibration_separate() {
        let mut bs = Vec::new();
        for i in 0..30 {
            bs.push(Boundary {
                actions: i,
                state: Mm2MechanicalState {
                    screen: if i < 10 {
                        1
                    } else if i < 20 {
                        2
                    } else {
                        3
                    },
                    x: ((i % 10) * 16) as u8,
                    ..Default::default()
                },
            });
        }
        bs.push(bs[20].clone());
        let selected = choose(&bs);
        assert_eq!(selected.len(), 12);
        assert_eq!(selected.iter().filter(|(l, _)| l == "frontier").count(), 8);
        assert_eq!(
            selected.iter().filter(|(l, _)| l == "calibration").count(),
            4
        );
        assert!(
            selected
                .iter()
                .filter(|(l, _)| l == "calibration")
                .all(|(_, b)| b.state.screen == 2)
        );
        assert!(choose(&[]).is_empty());
        assert_eq!(choose(&bs[..1]).len(), 1);
    }
}
