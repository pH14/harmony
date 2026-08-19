// SPDX-License-Identifier: AGPL-3.0-or-later

//! Read-only route minimizer for a recorded SMB film manifest.

use std::{
    env,
    error::Error,
    fmt, fs,
    io::{self, Write},
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::Instant,
};

use fuzzer::{
    phase4b::{
        ButtonChord, SmbInput, SmbMechanicalState, SmbSnapshot, SmbTarget,
        smb_mechanical_state_from_wram,
    },
    sequence_reducer::{
        ReductionConfig, ReplayEndpoint, SequenceReplay, projected_candidate_replays,
        reduce_verified_segmented_sequence,
    },
    target::Target,
};
use libafl::executors::ExitKind;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Deserialize)]
struct FilmManifest {
    source_report: PathBuf,
    milestone: String,
    rom_sha256: String,
    input: SmbInput,
    action_boundaries: Vec<FilmBoundary>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
struct FilmBoundary {
    action_count: usize,
    decoded: SmbMechanicalState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SmbReplayOutcome {
    state: SmbMechanicalState,
    dead: bool,
    frames: u64,
}

#[derive(Debug)]
struct SmbReplayError(String);

impl fmt::Display for SmbReplayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for SmbReplayError {}

struct SmbReplay {
    rom: Arc<[u8]>,
    targets: Mutex<Vec<SmbTarget>>,
}

impl SequenceReplay<ButtonChord> for SmbReplay {
    type Snapshot = SmbSnapshot;
    type Outcome = SmbReplayOutcome;
    type Error = SmbReplayError;

    fn replay(
        &self,
        entry: &Self::Snapshot,
        steps: &[ButtonChord],
    ) -> Result<ReplayEndpoint<Self::Snapshot, Self::Outcome>, Self::Error> {
        let pooled = self
            .targets
            .lock()
            .map_err(|_| SmbReplayError("SMB replay target pool was poisoned".to_owned()))?
            .pop();
        let mut target = if let Some(target) = pooled {
            target
        } else {
            SmbTarget::from_smb_rom_bytes_headless(&self.rom)
                .map_err(|error| SmbReplayError(error.to_string()))?
        };
        target
            .restore(entry)
            .map_err(|error| SmbReplayError(error.to_string()))?;
        let start_frames = target.frames_clocked();
        for step in steps {
            target.apply(step);
            if target.exit_kind() != ExitKind::Ok {
                self.return_target(target)?;
                return Err(SmbReplayError(
                    "SMB emulator failed during replay".to_owned(),
                ));
            }
            if target.is_dead() {
                break;
            }
        }
        let state = smb_mechanical_state_from_wram(target.wram());
        let snapshot = target
            .snapshot()
            .ok_or_else(|| SmbReplayError("failed to snapshot replay endpoint".to_owned()))?;
        let endpoint = ReplayEndpoint {
            snapshot,
            outcome: SmbReplayOutcome {
                state,
                dead: target.is_dead(),
                frames: target.frames_clocked().saturating_sub(start_frames),
            },
        };
        self.return_target(target)?;
        Ok(endpoint)
    }
}

impl SmbReplay {
    fn return_target(&self, target: SmbTarget) -> Result<(), SmbReplayError> {
        self.targets
            .lock()
            .map_err(|_| SmbReplayError("SMB replay target pool was poisoned".to_owned()))?
            .push(target);
        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct MinimizeManifest {
    source_manifest: PathBuf,
    source_report: PathBuf,
    milestone: String,
    rom_sha256: String,
    original_target: SmbMechanicalState,
    final_state: SmbMechanicalState,
    original_actions: usize,
    minimized_actions: usize,
    workers: usize,
    baseline_replay_millis: u128,
    baseline_frames: u64,
    baseline_frames_per_second: u64,
    projected_candidate_replays: u64,
    projection_sample_replays: u64,
    projection_sample_frames: u64,
    projection_sample_deaths: u64,
    projected_candidate_frames: u64,
    projected_wall_millis: u128,
    candidate_replays: u64,
    verification_replays: u64,
    segments: Vec<SegmentReport>,
    surviving_waits: Vec<SurvivingWait>,
    minimized_input: SmbInput,
}

#[derive(Debug, Serialize)]
struct SegmentReport {
    index: usize,
    entry_pair: (u8, u8),
    exit_pair: (u8, u8),
    original_actions: usize,
    minimized_actions: usize,
    removed_actions: usize,
    candidate_replays: u64,
    original_frames: u64,
    projection_sample_replays: u64,
    projection_sample_frames: u64,
    projection_sample_deaths: u64,
    projected_candidate_frames: u64,
}

#[derive(Clone, Copy, Debug)]
struct SegmentCalibration {
    original_frames: u64,
    sample_replays: u64,
    sample_frames: u64,
    sample_deaths: u64,
    sample_wall_nanos: u128,
    projected_candidate_frames: u64,
}

#[derive(Debug, Serialize)]
struct SurvivingWait {
    minimized_index: usize,
    original_index: usize,
    hold_frames: u8,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let source_manifest = PathBuf::from(
        args.next()
            .ok_or("usage: smb-minimize <film-manifest.json> <output.json> [workers]")?,
    );
    let output = PathBuf::from(args.next().ok_or("missing output manifest")?);
    let workers = args
        .next()
        .map(|value| value.to_string_lossy().parse())
        .transpose()?
        .unwrap_or(std::thread::available_parallelism()?.get());
    let workers = NonZeroUsize::new(workers).ok_or("worker count must be nonzero")?;
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }

    let film: FilmManifest = serde_json::from_slice(&fs::read(&source_manifest)?)?;
    if film.action_boundaries.is_empty() {
        return Err("film manifest has no action boundaries".into());
    }
    let final_boundary = film
        .action_boundaries
        .last()
        .copied()
        .ok_or("film manifest has no final action boundary")?;
    if final_boundary.action_count != film.input.actions.len() {
        return Err("film manifest final boundary does not match its input length".into());
    }
    let target_tuple = mechanical_tuple(final_boundary.decoded);
    let snapshot_points =
        level_transition_points(&film.action_boundaries, film.input.actions.len())?;
    let segment_lengths = segment_lengths(film.input.actions.len(), &snapshot_points);
    let pairs = segment_pairs(&film.action_boundaries, &snapshot_points)?;

    let rom_path = PathBuf::from(
        env::var_os("HARMONY_SMB_ROM")
            .ok_or("HARMONY_SMB_ROM must name the external Super Mario Bros ROM")?,
    );
    let rom: Arc<[u8]> = fs::read(rom_path)?.into();
    let rom_sha256 = format!("{:x}", Sha256::digest(&rom));
    if rom_sha256 != film.rom_sha256 {
        return Err("ROM SHA-256 does not match the film manifest".into());
    }
    let replay = SmbReplay {
        rom,
        targets: Mutex::new(Vec::with_capacity(workers.get())),
    };
    let mut genesis_target = SmbTarget::from_smb_rom_bytes_headless(&replay.rom)?;
    let genesis = genesis_target
        .snapshot()
        .ok_or("failed to snapshot SMB gameplay genesis")?;

    // Measurement-only wall time is printed before reduction and never enters replay state.
    #[allow(clippy::disallowed_methods)]
    let baseline_started = Instant::now();
    let baseline = replay.replay(&genesis, &film.input.actions)?;
    let baseline_elapsed = baseline_started.elapsed();
    if baseline.outcome.dead || mechanical_tuple(baseline.outcome.state) < target_tuple {
        return Err("the original film does not satisfy its minimization oracle".into());
    }
    let frames_per_second = u128::from(baseline.outcome.frames)
        .saturating_mul(1_000_000_000)
        .checked_div(baseline_elapsed.as_nanos().max(1))
        .and_then(|rate| u64::try_from(rate).ok())
        .unwrap_or(u64::MAX);
    let calibrations = calibrate_segments(
        &replay,
        &genesis,
        &film.input.actions,
        &snapshot_points,
        &pairs,
        target_tuple,
        workers,
    )?;
    let projected_replays = projected_candidate_replays(&segment_lengths);
    let projection_sample_replays = calibrations.iter().fold(0_u64, |total, segment| {
        total.saturating_add(segment.sample_replays)
    });
    let projection_sample_frames = calibrations.iter().fold(0_u64, |total, segment| {
        total.saturating_add(segment.sample_frames)
    });
    let projection_sample_deaths = calibrations.iter().fold(0_u64, |total, segment| {
        total.saturating_add(segment.sample_deaths)
    });
    let projected_candidate_frames = calibrations.iter().fold(0_u64, |total, segment| {
        total.saturating_add(segment.projected_candidate_frames)
    });
    let projected_nanos =
        calibrations
            .iter()
            .zip(&segment_lengths)
            .fold(0_u128, |total, (segment, &length)| {
                let candidates = projected_candidate_replays(&[length]);
                let waves = candidates
                    .saturating_add(segment.sample_replays.saturating_sub(1))
                    .checked_div(segment.sample_replays.max(1))
                    .unwrap_or(u64::MAX);
                total.saturating_add(segment.sample_wall_nanos.saturating_mul(u128::from(waves)))
            });
    eprintln!(
        "baseline: {} actions, {} frames in {:.3}s ({frames_per_second} frames/s)",
        film.input.actions.len(),
        baseline.outcome.frames,
        baseline_elapsed.as_secs_f64()
    );
    eprintln!(
        "projection before reduction: {projected_replays} segment-local candidates, {projected_candidate_frames} candidate frames, {projection_sample_deaths}/{projection_sample_replays} sampled cuts died early, {:.1} wall-hours at {} workers",
        projected_nanos as f64 / 3_600_000_000_000.0,
        workers.get()
    );
    io::stderr().flush()?;
    const WORKING_DAY_NANOS: u128 = 8 * 60 * 60 * 1_000_000_000;
    if projected_nanos > WORKING_DAY_NANOS {
        return Err("segment-local projection exceeds one eight-hour working day".into());
    }

    let reduced = reduce_verified_segmented_sequence(
        &replay,
        genesis.clone(),
        film.input.actions.clone(),
        &snapshot_points,
        ReductionConfig { workers },
        |index, outcome| {
            if outcome.dead {
                return false;
            }
            if index.saturating_add(1) == pairs.len() {
                mechanical_tuple(outcome.state) >= target_tuple
            } else {
                (outcome.state.world, outcome.state.level) >= pairs[index].1
            }
        },
        |outcome| !outcome.dead && mechanical_tuple(outcome.state) >= target_tuple,
    )?;
    let final_endpoint = replay.replay(&genesis, &reduced.steps)?;
    if final_endpoint.outcome.dead || mechanical_tuple(final_endpoint.outcome.state) < target_tuple
    {
        return Err("final independent replay rejected the minimized input".into());
    }

    let segments = reduced
        .segments
        .iter()
        .zip(pairs)
        .zip(&calibrations)
        .map(
            |((segment, (entry_pair, exit_pair)), calibration)| SegmentReport {
                index: segment.index,
                entry_pair,
                exit_pair,
                original_actions: segment.original_steps,
                minimized_actions: segment.reduced_steps,
                removed_actions: segment.original_steps.saturating_sub(segment.reduced_steps),
                candidate_replays: segment.candidate_replays,
                original_frames: calibration.original_frames,
                projection_sample_replays: calibration.sample_replays,
                projection_sample_frames: calibration.sample_frames,
                projection_sample_deaths: calibration.sample_deaths,
                projected_candidate_frames: calibration.projected_candidate_frames,
            },
        )
        .collect();
    let surviving_waits = reduced
        .steps
        .iter()
        .zip(&reduced.original_indices)
        .enumerate()
        .filter(|(_, (step, _))| step.buttons == 0)
        .map(|(minimized_index, (step, &original_index))| SurvivingWait {
            minimized_index,
            original_index,
            hold_frames: step.hold_frames,
        })
        .collect();
    let report = MinimizeManifest {
        source_manifest,
        source_report: film.source_report,
        milestone: film.milestone,
        rom_sha256,
        original_target: final_boundary.decoded,
        final_state: final_endpoint.outcome.state,
        original_actions: film.input.actions.len(),
        minimized_actions: reduced.steps.len(),
        workers: workers.get(),
        baseline_replay_millis: baseline_elapsed.as_millis(),
        baseline_frames: baseline.outcome.frames,
        baseline_frames_per_second: frames_per_second,
        projected_candidate_replays: projected_replays,
        projection_sample_replays,
        projection_sample_frames,
        projection_sample_deaths,
        projected_candidate_frames,
        projected_wall_millis: projected_nanos / 1_000_000,
        candidate_replays: reduced.candidate_replays,
        verification_replays: reduced.verification_replays.saturating_add(1),
        segments,
        surviving_waits,
        minimized_input: SmbInput {
            actions: reduced.steps,
        },
    };
    create_parent(&output)?;
    fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn calibrate_segments(
    replay: &SmbReplay,
    genesis: &SmbSnapshot,
    actions: &[ButtonChord],
    snapshot_points: &[usize],
    pairs: &[(LevelPair, LevelPair)],
    target_tuple: (u8, u8, u16),
    workers: NonZeroUsize,
) -> Result<Vec<SegmentCalibration>, Box<dyn Error>> {
    let mut boundaries = Vec::with_capacity(snapshot_points.len().saturating_add(2));
    boundaries.push(0);
    boundaries.extend_from_slice(snapshot_points);
    boundaries.push(actions.len());
    if boundaries.len().saturating_sub(1) != pairs.len() {
        return Err("segment calibration pairs do not match boundaries".into());
    }

    let mut entry = genesis.clone();
    let mut result = Vec::with_capacity(pairs.len());
    for (index, window) in boundaries.windows(2).enumerate() {
        let segment = actions
            .get(window[0]..window[1])
            .ok_or("segment calibration boundary is out of range")?;
        let (sample_replays, sample_frames, sample_deaths, sample_wall_nanos) =
            sample_segment_candidates(replay, &entry, segment, workers)?;
        let original = replay.replay(&entry, segment)?;
        let reaches_exit = if index.saturating_add(1) == pairs.len() {
            !original.outcome.dead && mechanical_tuple(original.outcome.state) >= target_tuple
        } else {
            !original.outcome.dead
                && (original.outcome.state.world, original.outcome.state.level) >= pairs[index].1
        };
        if !reaches_exit {
            return Err(format!("original segment {index} does not reach its exit").into());
        }
        let candidate_replays = projected_candidate_replays(&[segment.len()]);
        let projected_frames = u128::from(sample_frames)
            .saturating_mul(u128::from(candidate_replays))
            .checked_div(u128::from(sample_replays.max(1)))
            .unwrap_or(u128::MAX);
        let projected_candidate_frames = u64::try_from(projected_frames).unwrap_or(u64::MAX);
        eprintln!(
            "segment {index}: {} actions, {} original frames, {sample_deaths}/{sample_replays} sampled cuts died, projected {candidate_replays} candidates / {projected_candidate_frames} frames",
            segment.len(),
            original.outcome.frames,
        );
        result.push(SegmentCalibration {
            original_frames: original.outcome.frames,
            sample_replays,
            sample_frames,
            sample_deaths,
            sample_wall_nanos,
            projected_candidate_frames,
        });
        entry = original.snapshot;
    }
    Ok(result)
}

#[allow(clippy::disallowed_methods)] // wall time prices an offline tool and never reaches replay state.
fn sample_segment_candidates(
    replay: &SmbReplay,
    entry: &SmbSnapshot,
    segment: &[ButtonChord],
    workers: NonZeroUsize,
) -> Result<(u64, u64, u64, u128), Box<dyn Error>> {
    if segment.is_empty() {
        return Ok((0, 0, 0, 0));
    }
    let granularity = workers.get().min(segment.len());
    let mut candidates = Vec::with_capacity(granularity);
    for part in 0..granularity {
        let start = part.saturating_mul(segment.len()) / granularity;
        let end = (part.saturating_add(1)).saturating_mul(segment.len()) / granularity;
        let mut candidate =
            Vec::with_capacity(segment.len().saturating_sub(end.saturating_sub(start)));
        candidate.extend_from_slice(&segment[..start]);
        candidate.extend_from_slice(&segment[end..]);
        candidates.push(candidate);
    }
    let started = Instant::now();
    let results = thread::scope(|scope| {
        let handles = candidates
            .iter()
            .map(|candidate| scope.spawn(move || replay.replay(entry, candidate)))
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| handle.join())
            .collect::<Vec<_>>()
    });
    let elapsed = started.elapsed().as_nanos();
    let mut frames = 0_u64;
    let mut deaths = 0_u64;
    for result in results {
        let endpoint = match result {
            Ok(Ok(endpoint)) => endpoint,
            Ok(Err(error)) => return Err(Box::new(error)),
            Err(_) => return Err("segment calibration worker panicked".into()),
        };
        frames = frames.saturating_add(endpoint.outcome.frames);
        deaths = deaths.saturating_add(u64::from(endpoint.outcome.dead));
    }
    Ok((
        u64::try_from(candidates.len()).unwrap_or(u64::MAX),
        frames,
        deaths,
        elapsed,
    ))
}

fn mechanical_tuple(state: SmbMechanicalState) -> (u8, u8, u16) {
    (state.world, state.level, state.progress)
}

fn level_transition_points(
    boundaries: &[FilmBoundary],
    action_count: usize,
) -> Result<Vec<usize>, Box<dyn Error>> {
    let mut points = Vec::new();
    for pair in boundaries.windows(2) {
        let before = pair[0];
        let after = pair[1];
        if before.action_count > action_count || after.action_count > action_count {
            return Err("film boundary action count is out of range".into());
        }
        if (before.decoded.world, before.decoded.level)
            != (after.decoded.world, after.decoded.level)
            && after.action_count > 0
            && after.action_count < action_count
        {
            points.push(after.action_count);
        }
    }
    points.sort_unstable();
    points.dedup();
    Ok(points)
}

fn segment_lengths(action_count: usize, points: &[usize]) -> Vec<usize> {
    let mut boundaries = Vec::with_capacity(points.len().saturating_add(2));
    boundaries.push(0);
    boundaries.extend_from_slice(points);
    boundaries.push(action_count);
    boundaries
        .windows(2)
        .map(|window| window[1].saturating_sub(window[0]))
        .collect()
}

type LevelPair = (u8, u8);

fn segment_pairs(
    boundaries: &[FilmBoundary],
    points: &[usize],
) -> Result<Vec<(LevelPair, LevelPair)>, Box<dyn Error>> {
    let last = boundaries
        .last()
        .ok_or("film manifest has no action boundaries")?
        .action_count;
    let mut offsets = Vec::with_capacity(points.len().saturating_add(2));
    offsets.push(0);
    offsets.extend_from_slice(points);
    offsets.push(last);
    offsets
        .windows(2)
        .map(|window| {
            let entry = boundary_at(boundaries, window[0])?;
            let exit = boundary_at(boundaries, window[1])?;
            Ok((
                (entry.decoded.world, entry.decoded.level),
                (exit.decoded.world, exit.decoded.level),
            ))
        })
        .collect()
}

fn boundary_at(
    boundaries: &[FilmBoundary],
    action_count: usize,
) -> Result<FilmBoundary, Box<dyn Error>> {
    boundaries
        .binary_search_by_key(&action_count, |boundary| boundary.action_count)
        .ok()
        .and_then(|index| boundaries.get(index))
        .copied()
        .ok_or_else(|| "film manifest is missing a segment boundary".into())
}

fn create_parent(path: &Path) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{FilmBoundary, level_transition_points, segment_lengths};
    use fuzzer::phase4b::SmbMechanicalState;

    fn boundary(action_count: usize, world: u8, level: u8) -> FilmBoundary {
        FilmBoundary {
            action_count,
            decoded: SmbMechanicalState {
                world,
                level,
                ..SmbMechanicalState::default()
            },
        }
    }

    #[test]
    fn level_transitions_define_segments() {
        let boundaries = [
            boundary(0, 0, 0),
            boundary(1, 0, 0),
            boundary(2, 0, 1),
            boundary(3, 0, 1),
            boundary(4, 1, 0),
            boundary(5, 1, 0),
        ];
        let points = level_transition_points(&boundaries, 5).expect("valid boundaries");
        assert_eq!(points, vec![2, 4]);
        assert_eq!(segment_lengths(5, &points), vec![2, 2, 1]);
    }
}
