// SPDX-License-Identifier: AGPL-3.0-or-later

//! SMB archive policy, retention, selection, and diagnostic tooling.

use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet},
    error::Error,
    num::NonZeroUsize,
};

use libafl::executors::ExitKind;
use libafl_bolts::rands::{Rand, StdRand};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    search::draw_budget::{DrawBudgetParameters, DrawBudgets},
    smb::target::{
        ButtonChord, FRAME_HEIGHT, FRAME_WIDTH, PLAYER_BELOW_PLAY_AREA_PAGE, PLAYER_KILLED_STATE,
        SmbDeathBytes, SmbInput, SmbMechanicalState, SmbMilestoneInputs, SmbMilestoneTimes,
        SmbMilestones, SmbObservations, SmbProgressWatermark, SmbSnapshot, SmbTarget,
        observe_smb_input, smb_camera_pixels, smb_death_bytes, smb_mechanical_state_from_wram,
        smb_milestones_from_wram,
    },
    target::Target,
};

/// Compiled ceiling on archive entries. A ceiling is not an allocation:
/// memory tracks actual retention (about 20 KB per entry with its snapshot),
/// and a whole-tree resume inherits the source population in full, so the
/// ceiling stays far above any run's retention. Campaign runs register their
/// own per-run bound at or below this.
pub const MAX_ARCHIVE_ENTRIES: usize = 1_048_576;
const MAX_ENTRIES_PER_KEY: usize = 2;
/// Auxiliary per-cell bound for cells inside a registered waypoint region.
/// The waypoint's retention preference is exactly this widened cell: the
/// region holds finitely many cells, each capped here, so waypoint states
/// cannot flood the archive.
const WAYPOINT_ENTRIES_PER_KEY: usize = 4;
const FRONTIER_PROGRESS_BAND: u16 = 8;
const STATE_FINGERPRINT_MASK: u8 = 0x3f;
pub(crate) const FROZEN_BUTTON_MASKS: [u8; 9] =
    [0x00, 0x01, 0x02, 0x40, 0x80, 0x81, 0x82, 0x83, 0x10];

/// Largest bounded action horizon accepted by the completion-only archive.
/// Raised from 512, then from 4096 when the endgame lineage approached it;
/// a ceiling is not an allocation, and every campaign registers its own
/// explicit per-run action limit that replay retains and validates under.
pub const MAX_SMB_COMPLETION_ACTIONS: usize = 8192;

/// Duration distribution used by completion suffix mutation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SmbArchiveDurationPolicy {
    /// Frozen H1 distribution: three quarters short, one quarter full-range.
    Legacy,
    /// Generic two-stratum distribution covering short control and long time horizons.
    Stratified,
}

/// Version stamped into every extended ladder record.
pub const SMB_LADDER_VERSION: u32 = 2;

/// One observed `(world, level)` pair and what was reached inside it.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SmbLadderTransition {
    /// Decoded world number.
    pub world: u8,
    /// Decoded level number, corrected the same way the archive key corrects it.
    pub level: u8,
    /// Earliest execution that produced a retained state here; zero is bootstrap.
    pub first_execution: u64,
    /// Deepest progress bucket reached here.
    pub max_progress: u16,
}

/// A ladder that grows with the campaign instead of saturating.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbLadder {
    /// Stamped version of this record; zero means no extended ladder was kept.
    pub version: u32,
    /// Maximum corrected world, level and progress observed.
    pub max_tuple: Option<(u8, u8, u16)>,
    /// Every observed pair, in key order.
    pub transitions: Vec<SmbLadderTransition>,
}

impl SmbLadder {
    /// Report whether this record should be omitted from a report entirely.
    ///
    /// A frozen-ladder campaign must serialize exactly the fields it serialized
    /// before this mechanism existed, so an absent ladder writes nothing.
    #[must_use]
    pub fn is_absent(&self) -> bool {
        self.version == 0 && self.max_tuple.is_none() && self.transitions.is_empty()
    }
}

/// One stage of the frame-slack measurement.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbFrameSlackStage {
    /// What was tried.
    pub stage: String,
    /// Actions whose hold was shortened.
    pub shortened_actions: usize,
    /// Frames the whole input costs after shortening.
    pub frames: u64,
    /// Frames the censused segment costs after shortening.
    pub segment_frames: u64,
    /// Deepest tuple the shortened input reaches.
    pub reached: Option<(u8, u8, u16)>,
    /// Whether the shortened input still reaches the baseline tuple alive.
    pub preserved: bool,
}

/// How many of a recorded lineage's frames inside one level are removable.
///
/// This is a measurement on a recorded artifact, not a search: it reports what
/// the recorded route could have cost, and adopts nothing.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbFrameSlackReport {
    /// Censused world.
    pub world: u8,
    /// Censused level.
    pub level: u8,
    /// Identifier of the replayed archive entry.
    pub entry_id: u64,
    /// Action index at which the censused pair is first observed.
    pub segment_start: usize,
    /// Actions in the censused segment.
    pub segment_actions: usize,
    /// Frames the segment costs as recorded.
    pub baseline_segment_frames: u64,
    /// Frames the whole input costs as recorded.
    pub baseline_frames: u64,
    /// Deepest tuple the recorded input reaches.
    pub baseline_reached: Option<(u8, u8, u16)>,
    /// Segment actions that gained no progress bucket as recorded.
    pub no_gain_actions: usize,
    /// Frames those actions cost.
    pub no_gain_frames: u64,
    /// Stages tried, cheapest first.
    pub stages: Vec<SmbFrameSlackStage>,
}

/// Replay one input from genesis and report its per-action decoded states.
fn replay_smb_action_states(
    rom: &[u8],
    input: &SmbInput,
) -> Result<Vec<SmbMechanicalState>, Box<dyn Error>> {
    let mut target = SmbTarget::from_smb_rom_bytes_headless(rom)?;
    let mut states = Vec::with_capacity(input.actions.len().saturating_add(1));
    let genesis = target.observe();
    let wram: &[u8; 2_048] = genesis
        .wram
        .as_slice()
        .try_into()
        .map_err(|_| "replay observation WRAM is not exactly 2 KiB")?;
    states.push(smb_mechanical_state_from_wram(wram));
    for action in &input.actions {
        target.apply(action);
        if target.exit_kind() != ExitKind::Ok {
            return Err("emulation failed during a frame-slack replay".into());
        }
        let observation = target.observe();
        let wram: &[u8; 2_048] = observation
            .wram
            .as_slice()
            .try_into()
            .map_err(|_| "replay observation WRAM is not exactly 2 KiB")?;
        states.push(smb_mechanical_state_from_wram(wram));
        if target.is_dead() {
            break;
        }
    }
    Ok(states)
}

fn smb_reached_tuple(states: &[SmbMechanicalState]) -> Option<(u8, u8, u16)> {
    states
        .iter()
        .map(|state| (state.world, state.level, state.progress))
        .max()
}

/// Measure how many frames of a recorded lineage's traverse of one level are
/// removable without giving up the depth it reached.
pub fn diagnose_smb_frame_slack(
    rom: &[u8],
    source: &SmbArchiveReport,
    world: u8,
    level: u8,
) -> Result<SmbFrameSlackReport, Box<dyn Error>> {
    let entry = source
        .entries
        .iter()
        .max_by_key(|entry| {
            (
                entry.key.world,
                entry.key.level,
                entry.key.progress,
                Reverse(entry.input.actions.len()),
                Reverse(entry.id),
            )
        })
        .ok_or("source archive contains no retained entries")?;
    let input = entry.input.clone();
    let states = replay_smb_action_states(rom, &input)?;
    let baseline_reached = smb_reached_tuple(&states);
    let segment_start = states
        .iter()
        .position(|state| state.world == world && state.level == level)
        .ok_or("the recorded lineage never enters the censused pair")?;
    let frames_of = |actions: &[ButtonChord]| -> u64 {
        actions
            .iter()
            .map(|action| u64::from(action.hold_frames))
            .sum()
    };
    let baseline_frames = frames_of(&input.actions);
    let baseline_segment_frames = frames_of(&input.actions[segment_start..]);
    let mut no_gain = Vec::new();
    for index in segment_start..input.actions.len() {
        let before = states.get(index).map(|state| state.progress);
        let after = states
            .get(index.saturating_add(1))
            .map(|state| state.progress);
        if let (Some(before), Some(after)) = (before, after)
            && after <= before
            && input.actions[index].hold_frames > 1
        {
            no_gain.push(index);
        }
    }
    let no_gain_frames: u64 = no_gain
        .iter()
        .map(|&index| u64::from(input.actions[index].hold_frames))
        .sum();
    let mut stages = Vec::new();
    let shorten = |actions: &[ButtonChord], indices: &[usize]| -> Vec<ButtonChord> {
        let mut out = actions.to_vec();
        for &index in indices {
            out[index] = ButtonChord::new(out[index].buttons, 1);
        }
        out
    };
    // Cheapest first: shorten every no-gain action at once and replay once.
    let one_shot = SmbInput {
        actions: shorten(&input.actions, &no_gain),
    };
    let one_shot_states = replay_smb_action_states(rom, &one_shot)?;
    let one_shot_reached = smb_reached_tuple(&one_shot_states);
    stages.push(SmbFrameSlackStage {
        stage: "all_no_gain_actions_shortened".to_owned(),
        shortened_actions: no_gain.len(),
        frames: frames_of(&one_shot.actions),
        segment_frames: frames_of(&one_shot.actions[segment_start..]),
        reached: one_shot_reached,
        preserved: one_shot_reached == baseline_reached,
    });
    // Then a greedy pass that keeps only the shortenings which hold.
    let mut greedy = input.actions.clone();
    let mut kept = 0_usize;
    for &index in &no_gain {
        let trial = shorten(&greedy, &[index]);
        let trial_states = replay_smb_action_states(
            rom,
            &SmbInput {
                actions: trial.clone(),
            },
        )?;
        if smb_reached_tuple(&trial_states) == baseline_reached {
            greedy = trial;
            kept += 1;
        }
    }
    let greedy_states = replay_smb_action_states(
        rom,
        &SmbInput {
            actions: greedy.clone(),
        },
    )?;
    let greedy_reached = smb_reached_tuple(&greedy_states);
    stages.push(SmbFrameSlackStage {
        stage: "greedy_per_action".to_owned(),
        shortened_actions: kept,
        frames: frames_of(&greedy),
        segment_frames: frames_of(&greedy[segment_start..]),
        reached: greedy_reached,
        preserved: greedy_reached == baseline_reached,
    });
    Ok(SmbFrameSlackReport {
        world,
        level,
        entry_id: entry.id,
        segment_start,
        segment_actions: input.actions.len().saturating_sub(segment_start),
        baseline_segment_frames,
        baseline_frames,
        baseline_reached,
        no_gain_actions: no_gain.len(),
        no_gain_frames,
        stages,
    })
}

/// One `(world, level)` segment of a single recorded lineage.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbLineageLevelSegment {
    /// Pair world.
    pub world: u8,
    /// Pair level.
    pub level: u8,
    /// Action index at which the lineage first stands in this pair.
    pub first_action: usize,
    /// Actions the lineage spends in this pair.
    pub actions: usize,
    /// Frames the lineage spends in this pair.
    pub frames: u64,
    /// Lowest progress bucket the lineage stands on in this pair.
    pub first_progress: u16,
    /// Highest progress bucket the lineage reaches in this pair.
    pub last_progress: u16,
    /// Frames per progress bucket crossed, in thousandths, so the rate is an
    /// exact integer comparison rather than a rounded float.
    pub frames_per_bucket_milli: u64,
    /// Frames this one lineage had spent in the pair when it first stood on
    /// each progress bucket, in ascending bucket order.
    ///
    /// Every other frame figure this experiment records for a bucket is a
    /// minimum over all retained entries, and those minima belong to different
    /// lineages, so differencing them across buckets describes no route that
    /// exists. This curve is one lineage's own, so its differences are real
    /// costs and can only be non-negative.
    pub bucket_frames: Vec<(u16, u64)>,
}

/// One stand-still in a lineage's traverse, and what shortening it costs.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbStallVerdict {
    /// Progress bucket the lineage stood on entering the stall.
    pub from_progress: u16,
    /// Progress bucket it reached leaving it.
    pub to_progress: u16,
    /// Action index range of the stall within the whole input.
    pub action_range: (usize, usize),
    /// Frames the stall costs as recorded.
    pub frames: u64,
    /// Frames it costs with every action in it shortened to the vocabulary's
    /// shortest hold.
    pub shortened_frames: u64,
    /// Whether the whole input still reaches its recorded deepest tuple with
    /// this stall — and only this stall — shortened.
    pub slack: bool,
    /// Deepest tuple the shortened input reaches, for a failure to name itself.
    pub shortened_reached: Option<(u8, u8, u16)>,
    /// Held frames of the stall's actions as recorded, so a forced stall's
    /// shape can be read rather than guessed.
    pub holds: Vec<u8>,
    /// Shortest hold cap this stall's actions tolerate while the whole input
    /// still reaches its recorded deepest tuple, found by bisection; `None`
    /// when no cap below the recorded maximum works.
    ///
    /// This measures **the recorded suffix's phase tolerance**, not any minimum
    /// the game imposes: capping a wait shifts the phase of everything after
    /// it, and the recorded continuation either still meets its hazards in time
    /// or does not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tolerated_cap: Option<u8>,
    /// Frames the stall costs at `tolerated_cap`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tolerated_frames: Option<u64>,
    /// Replays the bisection spent on this stall.
    #[serde(default, skip_serializing_if = "bisection_is_absent")]
    pub bisection_replays: u64,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn bisection_is_absent(replays: &u64) -> bool {
    *replays == 0
}

/// Per-stall forced-versus-slack verdict for one recorded lineage.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbStallSlackReport {
    /// Censused pair.
    pub world: u8,
    /// Censused pair level.
    pub level: u8,
    /// Archive identifier of the lineage examined.
    pub entry_id: u64,
    /// Deepest tuple the recorded input reaches.
    pub baseline_reached: Option<(u8, u8, u16)>,
    /// Frames the lineage spends in the pair as recorded.
    pub baseline_frames: u64,
    /// Frames the stalls cost between them.
    pub stall_frames: u64,
    /// Frames recoverable across the stalls that proved slack.
    pub recoverable_frames: u64,
    /// One verdict per stall, in traverse order.
    pub stalls: Vec<SmbStallVerdict>,
}

/// Decide, stall by stall, whether a lineage's stand-stills are forced.
///
/// A stall is a run of actions that costs at least `minimum_frames` while
/// advancing at most `maximum_buckets` progress buckets. Each is shortened
/// alone — every other action left exactly as recorded — and the whole input is
/// replayed from gameplay genesis, so a stall counts as slack only when the run
/// still reaches the tuple it reached before. Judging them one at a time is the
/// point: shortening them together cannot say which one mattered.
///
/// # Errors
///
/// Returns an error when the archive holds no retained entry or a replay fails.
pub fn diagnose_smb_stall_slack(
    rom: &[u8],
    source: &SmbArchiveReport,
    world: u8,
    level: u8,
    minimum_frames: u64,
    maximum_buckets: u16,
    bisect_tolerance: bool,
) -> Result<SmbStallSlackReport, Box<dyn Error>> {
    let entry = source
        .entries
        .iter()
        .max_by_key(|entry| {
            (
                entry.key.world,
                entry.key.level,
                entry.key.progress,
                Reverse(entry.input.actions.len()),
                Reverse(entry.id),
            )
        })
        .ok_or("source archive contains no retained entries")?;
    let input = entry.input.clone();
    let states = replay_smb_action_states(rom, &input)?;
    let baseline_reached = smb_reached_tuple(&states);
    let segment_start = states
        .iter()
        .position(|state| state.world == world && state.level == level)
        .ok_or("the recorded lineage never enters the censused pair")?;
    let frames_of = |actions: &[ButtonChord]| -> u64 {
        actions
            .iter()
            .map(|action| u64::from(action.bounded_hold_frames()))
            .sum()
    };
    let baseline_frames = frames_of(input.actions.get(segment_start..).unwrap_or(&[]));
    // Walk the segment cutting it at every action that sets a new progress
    // high-water mark; the runs between those cuts are the candidate stalls.
    let mut cuts: Vec<usize> = vec![segment_start];
    let mut best: Option<u16> = None;
    for (offset, state) in states.iter().enumerate().skip(segment_start) {
        if best.is_none_or(|seen| state.progress > seen) {
            best = Some(state.progress);
            if offset > segment_start {
                cuts.push(offset);
            }
        }
    }
    cuts.push(input.actions.len());
    let mut stalls = Vec::new();
    for pair in cuts.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        let actions = match input.actions.get(start..end) {
            Some(actions) if !actions.is_empty() => actions,
            _ => continue,
        };
        let frames = frames_of(actions);
        let from_progress = states.get(start).map_or(0, |state| state.progress);
        let to_progress = states
            .get(end)
            .map_or(from_progress, |state| state.progress);
        if frames < minimum_frames || to_progress.saturating_sub(from_progress) > maximum_buckets {
            continue;
        }
        // Shorten this stall alone and replay the whole input.
        let mut trial = input.clone();
        for action in trial.actions.get_mut(start..end).unwrap_or(&mut []) {
            *action = ButtonChord::new(action.buttons, 1);
        }
        let trial_states = replay_smb_action_states(rom, &trial)?;
        let shortened_reached = smb_reached_tuple(&trial_states);
        // Phase tolerance: the smallest cap on this stall's holds that the
        // recorded suffix still survives. Bisect between one, which the trial
        // above already refuted, and the stall's own longest hold, which it
        // trivially tolerates because it changes nothing.
        let recorded_cap = actions
            .iter()
            .map(|action| action.bounded_hold_frames())
            .max()
            .unwrap_or(1);
        let mut bisection_replays = 0_u64;
        let mut tolerated: Option<u8> = None;
        if bisect_tolerance && shortened_reached != baseline_reached {
            let (mut low, mut high) = (2_u8, recorded_cap);
            while low < high {
                let probe = low.saturating_add(high.saturating_sub(low) / 2);
                let mut capped = input.clone();
                for action in capped.actions.get_mut(start..end).unwrap_or(&mut []) {
                    *action =
                        ButtonChord::new(action.buttons, action.bounded_hold_frames().min(probe));
                }
                bisection_replays = bisection_replays.saturating_add(1);
                if smb_reached_tuple(&replay_smb_action_states(rom, &capped)?) == baseline_reached {
                    high = probe;
                } else {
                    low = probe.saturating_add(1);
                }
            }
            if low < recorded_cap {
                tolerated = Some(low);
            }
        }
        let tolerated_frames = tolerated.map(|cap| {
            actions
                .iter()
                .map(|action| u64::from(action.bounded_hold_frames().min(cap)))
                .sum()
        });
        stalls.push(SmbStallVerdict {
            from_progress,
            to_progress,
            action_range: (start, end),
            frames,
            shortened_frames: frames_of(trial.actions.get(start..end).unwrap_or(&[])),
            slack: shortened_reached == baseline_reached,
            shortened_reached,
            holds: actions
                .iter()
                .map(|action| action.bounded_hold_frames())
                .collect(),
            tolerated_cap: tolerated,
            tolerated_frames,
            bisection_replays,
        });
    }
    let stall_frames = stalls.iter().map(|stall| stall.frames).sum();
    let recoverable_frames = stalls
        .iter()
        .map(|stall| {
            let floor = if stall.slack {
                stall.shortened_frames
            } else {
                stall.tolerated_frames.unwrap_or(stall.frames)
            };
            stall.frames.saturating_sub(floor)
        })
        .sum();
    Ok(SmbStallSlackReport {
        world,
        level,
        entry_id: entry.id,
        baseline_reached,
        baseline_frames,
        stall_frames,
        recoverable_frames,
        stalls,
    })
}

/// Per-level traverse speed of one recorded lineage.
///
/// The archive's frame-cost census compares routes within one pair, but its
/// per-bucket minima may come from different lineages, so its totals cannot be
/// differenced across pairs. This mode measures one lineage end to end
/// instead: a single replay from gameplay genesis, segmented by the recorded
/// level transitions, which is the only way to state what crossing a level
/// costs the search in the currency the level clock is denominated in.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbLineageLevelReport {
    /// Archive identifier of the censused lineage.
    pub entry_id: u64,
    /// Actions in its input.
    pub actions: usize,
    /// Frames its whole input costs.
    pub frames: u64,
    /// Deepest tuple it reaches.
    pub reached: Option<(u8, u8, u16)>,
    /// Segments in the order the lineage walks them.
    pub segments: Vec<SmbLineageLevelSegment>,
}

/// Measure what each level cost one recorded lineage, in actions and frames.
///
/// The lineage is the archive's deepest by exactly the rule the frontier film
/// and the claim replay gate use — deepest tuple, then shortest input, then
/// lowest identifier — so all three speak about the same input.
///
/// # Errors
///
/// Returns an error when the archive holds no retained entry or the replay
/// fails.
pub fn census_smb_lineage_levels(
    rom: &[u8],
    source: &SmbArchiveReport,
) -> Result<SmbLineageLevelReport, Box<dyn Error>> {
    let entry = source
        .entries
        .iter()
        .max_by_key(|entry| {
            (
                entry.key.world,
                entry.key.level,
                entry.key.progress,
                Reverse(entry.input.actions.len()),
                Reverse(entry.id),
            )
        })
        .ok_or("source archive contains no retained entries")?;
    let input = entry.input.clone();
    let states = replay_smb_action_states(rom, &input)?;
    let frames_of = |actions: &[ButtonChord]| -> u64 {
        actions
            .iter()
            .map(|action| u64::from(action.bounded_hold_frames()))
            .sum()
    };
    let mut segments: Vec<SmbLineageLevelSegment> = Vec::new();
    for (index, state) in states.iter().enumerate() {
        match segments.last_mut() {
            // A pair is a segment only while the lineage stays in it; a
            // re-entry after leaving records as a second segment, because it
            // is a second traverse with a second clock.
            Some(last) if last.world == state.world && last.level == state.level => {
                last.last_progress = last.last_progress.max(state.progress);
            }
            _ => segments.push(SmbLineageLevelSegment {
                world: state.world,
                level: state.level,
                first_action: index,
                actions: 0,
                frames: 0,
                first_progress: state.progress,
                last_progress: state.progress,
                frames_per_bucket_milli: 0,
                bucket_frames: Vec::new(),
            }),
        }
    }
    // A segment runs from its first state to the state before the next
    // segment's first, so its actions are the actions between those indices.
    for position in 0..segments.len() {
        let start = segments[position].first_action;
        let end = segments
            .get(position.saturating_add(1))
            .map_or(states.len().saturating_sub(1), |next| next.first_action);
        let actions = input.actions.get(start..end).unwrap_or(&[]);
        let crossed = u64::from(
            segments[position]
                .last_progress
                .saturating_sub(segments[position].first_progress),
        );
        let frames = frames_of(actions);
        // The lineage's own cost at each bucket it first reaches: walk its
        // states across the segment accumulating held frames, and record the
        // running total the first time each bucket appears.
        let mut curve: Vec<(u16, u64)> = Vec::new();
        let mut spent = 0_u64;
        let mut best: Option<u16> = None;
        for offset in 0..=actions.len() {
            if let Some(state) = states.get(start.saturating_add(offset))
                && best.is_none_or(|seen| state.progress > seen)
            {
                best = Some(state.progress);
                curve.push((state.progress, spent));
            }
            if let Some(action) = actions.get(offset) {
                spent = spent.saturating_add(u64::from(action.bounded_hold_frames()));
            }
        }
        segments[position].bucket_frames = curve;
        segments[position].actions = actions.len();
        segments[position].frames = frames;
        segments[position].frames_per_bucket_milli = if crossed == 0 {
            0
        } else {
            frames.saturating_mul(1_000) / crossed
        };
    }
    Ok(SmbLineageLevelReport {
        entry_id: entry.id,
        actions: input.actions.len(),
        frames: frames_of(&input.actions),
        reached: smb_reached_tuple(&states),
        segments,
    })
}

/// Frame cost of the cheapest recorded route to one progress bucket.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbFrameCostBucket {
    /// Progress bucket within the censused pair.
    pub progress: u16,
    /// Entries retained at this bucket.
    pub entries: u64,
    /// Fewest emulated frames any retained entry here spent, summed over its
    /// whole input from gameplay genesis.
    pub min_frames: u64,
    /// Identifier of the entry holding `min_frames`.
    pub min_frames_id: u64,
    /// Actions in that entry's input.
    pub min_frames_actions: usize,
    /// Fewest actions any retained entry here used, which is the quantity the
    /// resume rule minimises.
    pub min_actions: usize,
    /// Frames used by the fewest-actions entry, for the comparison the census
    /// exists to make.
    pub min_actions_frames: u64,
}

/// Frame cost of every recorded route into one `(world, level)` pair.
///
/// The archive stores no clock and no frame count, but an input's frame cost is
/// exactly the sum of its held frames, so this needs no emulation. Within one
/// resumed run every entry descends from the same resume input, so differences
/// in total frames are differences spent inside the censused pair.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbFrameCostReport {
    /// Censused world.
    pub world: u8,
    /// Censused level.
    pub level: u8,
    /// Entries examined in this pair.
    pub entries: u64,
    /// Buckets in ascending progress order.
    pub buckets: Vec<SmbFrameCostBucket>,
}

/// Census the frame cost of each recorded route depth in one `(world, level)`.
#[must_use]
pub fn census_smb_frame_cost(
    source: &SmbArchiveReport,
    world: u8,
    level: u8,
) -> SmbFrameCostReport {
    let mut buckets = BTreeMap::<u16, (u64, u64, u64, usize, usize, u64)>::new();
    let mut entries = 0_u64;
    for entry in &source.entries {
        if entry.key.world != world || entry.key.level != level {
            continue;
        }
        entries += 1;
        let frames: u64 = entry
            .input
            .actions
            .iter()
            .map(|action| u64::from(action.hold_frames))
            .sum();
        let actions = entry.input.actions.len();
        let record = buckets.entry(entry.key.progress).or_insert((
            0,
            u64::MAX,
            0,
            actions,
            usize::MAX,
            u64::MAX,
        ));
        record.0 += 1;
        if frames < record.1 {
            record.1 = frames;
            record.2 = entry.id;
            record.3 = actions;
        }
        if actions < record.4 || (actions == record.4 && frames < record.5) {
            record.4 = actions;
            record.5 = frames;
        }
    }
    SmbFrameCostReport {
        world,
        level,
        entries,
        buckets: buckets
            .into_iter()
            .map(
                |(
                    progress,
                    (
                        count,
                        min_frames,
                        min_frames_id,
                        min_frames_actions,
                        min_actions,
                        min_actions_frames,
                    ),
                )| SmbFrameCostBucket {
                    progress,
                    entries: count,
                    min_frames,
                    min_frames_id,
                    min_frames_actions,
                    min_actions,
                    min_actions_frames,
                },
            )
            .collect(),
    }
}

/// One rung of a single lineage's ladder, measured by replaying that lineage.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbLineageRung {
    /// Decoded world number.
    pub world: u8,
    /// Decoded level number.
    pub level: u8,
    /// Emulated frame count at which this pair was first observed.
    pub first_frame: u64,
    /// Deepest progress bucket the lineage reached in this pair.
    pub max_progress: u16,
}

/// What one from-power-on serial replay of a recorded lineage observed.
///
/// This is the claim-gate measurement. It replays the input from gameplay
/// genesis through `observe_smb_input`, the same entry point the baseline
/// reproduction used, so no snapshot restore and none of the campaign's resume
/// machinery participates in the result.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbClaimReplayReport {
    /// Identifier of the replayed archive entry.
    pub entry_id: u64,
    /// Actions in the replayed input.
    pub actions: usize,
    /// Frames emulated by the replay.
    pub frames: u64,
    /// SHA-256 of the serialized input.
    pub input_sha256: String,
    /// SHA-256 of the serialized observation trace, the convention the
    /// baseline reproduction used for champion observations.
    pub trace_sha256: String,
    /// SHA-256 of the final work RAM.
    pub final_wram_sha256: String,
    /// Final decoded mechanical state.
    pub final_state: SmbMechanicalState,
    /// Whether the replay ended in the target's death state.
    pub died: bool,
    /// The ladder this one lineage walks.
    pub lineage_ladder: Vec<SmbLineageRung>,
    /// Deepest tuple the lineage reached.
    pub lineage_max_tuple: Option<(u8, u8, u16)>,
    /// Deepest tuple the source archive recorded, over every entry.
    pub archive_max_tuple: Option<(u8, u8, u16)>,
    /// Whether the replayed lineage reaches the archive's deepest tuple.
    pub max_tuple_matches: bool,
    /// Pairs the lineage walks that the archive's ladder does not carry, or
    /// where the lineage outruns the archive's recorded maximum. Empty is the
    /// passing case.
    pub ladder_disagreements: Vec<String>,
}

/// Replay one recorded lineage from gameplay genesis and report what it walks.
///
/// The lineage is chosen by the same rule the frontier film uses: the deepest
/// `(world, level, progress)` tuple, then the shortest input, then the lowest
/// entry identifier — so the gate and the film replay the same input.
pub fn replay_smb_claim_lineage(
    rom: &[u8],
    source: &SmbArchiveReport,
) -> Result<SmbClaimReplayReport, Box<dyn Error>> {
    let entry = source
        .entries
        .iter()
        .max_by_key(|entry| {
            (
                entry.key.world,
                entry.key.level,
                entry.key.progress,
                Reverse(entry.input.actions.len()),
                Reverse(entry.id),
            )
        })
        .ok_or("source archive contains no retained entries")?;
    let observations = observe_smb_input(rom, &entry.input)?;
    let mut rungs = BTreeMap::<(u8, u8), (u64, u16)>::new();
    let mut frames = 0_u64;
    let mut died = false;
    for observation in &observations {
        let wram: &[u8; 2_048] = observation
            .wram
            .as_slice()
            .try_into()
            .map_err(|_| "claim replay observation WRAM is not exactly 2 KiB")?;
        let state = smb_mechanical_state_from_wram(wram);
        frames = frames.max(observation.frame_count);
        died |= state.dead;
        let rung = rungs
            .entry((state.world, state.level))
            .or_insert((observation.frame_count, 0));
        rung.1 = rung.1.max(state.progress);
    }
    let final_observation = observations
        .last()
        .ok_or("claim replay produced no observations")?;
    let final_wram: &[u8; 2_048] = final_observation
        .wram
        .as_slice()
        .try_into()
        .map_err(|_| "claim replay final WRAM is not exactly 2 KiB")?;
    let lineage_ladder: Vec<SmbLineageRung> = rungs
        .iter()
        .map(
            |(&(world, level), &(first_frame, max_progress))| SmbLineageRung {
                world,
                level,
                first_frame,
                max_progress,
            },
        )
        .collect();
    let lineage_max_tuple = lineage_ladder
        .iter()
        .map(|rung| (rung.world, rung.level, rung.max_progress))
        .max();
    let archive_ladder = derive_smb_ladder(source);
    let mut ladder_disagreements = Vec::new();
    for rung in &lineage_ladder {
        match archive_ladder
            .transitions
            .iter()
            .find(|transition| transition.world == rung.world && transition.level == rung.level)
        {
            None => ladder_disagreements.push(format!(
                "pair ({}, {}) walked by the lineage is absent from the archive ladder",
                rung.world, rung.level
            )),
            Some(transition) if rung.max_progress > transition.max_progress => {
                ladder_disagreements.push(format!(
                    "pair ({}, {}) reaches {} on replay but the archive ladder records {}",
                    rung.world, rung.level, rung.max_progress, transition.max_progress
                ));
            }
            Some(_) => {}
        }
    }
    Ok(SmbClaimReplayReport {
        entry_id: entry.id,
        actions: entry.input.actions.len(),
        frames,
        input_sha256: format!("{:x}", Sha256::digest(serde_json::to_vec(&entry.input)?)),
        trace_sha256: format!("{:x}", Sha256::digest(serde_json::to_vec(&observations)?)),
        final_wram_sha256: format!("{:x}", Sha256::digest(final_wram)),
        final_state: smb_mechanical_state_from_wram(final_wram),
        died,
        lineage_ladder,
        lineage_max_tuple,
        archive_max_tuple: archive_ladder.max_tuple,
        max_tuple_matches: lineage_max_tuple == archive_ladder.max_tuple,
        ladder_disagreements,
    })
}

/// Derive the extended ladder from a recorded archive without emulating anything.
///
/// Frames are not recorded per entry, so this reports each pair's first creating
/// execution but not its first frame.
#[must_use]
pub fn derive_smb_ladder(source: &SmbArchiveReport) -> SmbLadder {
    let mut observed = BTreeMap::<(u8, u8), (u64, u16)>::new();
    for entry in &source.entries {
        let record = observed
            .entry((entry.key.world, entry.key.level))
            .or_insert((u64::MAX, 0));
        record.0 = record.0.min(entry.created_execution);
        record.1 = record.1.max(entry.key.progress);
    }
    SmbLadder {
        version: SMB_LADDER_VERSION,
        max_tuple: source
            .entries
            .iter()
            .map(|entry| (entry.key.world, entry.key.level, entry.key.progress))
            .max(),
        transitions: observed
            .into_iter()
            .map(
                |((world, level), (first_execution, max_progress))| SmbLadderTransition {
                    world,
                    level,
                    first_execution,
                    max_progress,
                },
            )
            .collect(),
    }
}

/// Whether the archive key separates the two live vertical pages.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum SmbArchiveKeyPolicy {
    /// Frozen behaviour: the vertical term is the low position byte over sixteen.
    #[default]
    Frozen,
    /// H51: the vertical term also carries the recorded vertical page byte.
    VerticalPage,
    /// H75: the frozen key plus a 16-pixel screen-x bucket, applied only to
    /// states whose mechanical tuple equals the registered scroll-frozen
    /// room; all other states key exactly as `Frozen`.
    FrozenRoomX16 {
        /// Registered room world.
        world: u8,
        /// Registered room level.
        level: u8,
        /// Registered room progress bucket.
        progress: u16,
    },
    /// The frozen key plus `rooms`: the count of distinct rooms a lineage has
    /// visited inside its current level. A room is an area identity (the
    /// bytes at `ROOM_IDENTITY_BYTES`) together with the page the lineage
    /// arrived in it at, see [`SmbRoomIdentity`]; a child in the same `(world, level)` as its
    /// parent inherits the parent's room set, any other child starts a new
    /// set. The search learns only that a value is new, never which value is
    /// wanted.
    FrozenRooms,
    /// The frozen key plus `room`: the room the entry stands in, as a cell
    /// coordinate rather than a count. Each room is explored once, so a loop
    /// that returns the lineage to a room it has already entered adds no
    /// novelty; which room gets drawn is left to the selector.
    FrozenRoom,
    /// `FrozenRoom` with the room changing only when the area bytes change.
    /// A warp that returns the lineage to an earlier page of the same area
    /// lands on screens that area already holds, so it stays in the same
    /// room instead of opening one per arrival page.
    FrozenArea,
    /// `FrozenArea` with a same-area warp landing in the lineage's room of
    /// that area whose arrival page is the greatest one not past the landing
    /// page, or opening a room at the landing page when there is none. A
    /// pipe that returns the lineage to the start of an area it entered
    /// twice lands in the first room, not the current one.
    FrozenAreaSpan,
}

/// How a lineage's current room follows a candidate's area and page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RoomRule {
    /// A backward warp inside one area opens a room keyed by its arrival page.
    WarpsOpenRooms,
    /// Only an area change opens a room.
    AreaOnly,
    /// Only an area change opens a room; a backward warp inside one area
    /// lands in the lineage's room of that area with the greatest arrival
    /// page not past the landing page.
    AreaSpan,
}

/// Work RAM addresses whose byte pair identifies the current area (area type
/// and area data offset).
pub const ROOM_IDENTITY_BYTES: [usize; 2] = [0x074e, 0x074f];

/// One room identity: the area bytes at `ROOM_IDENTITY_BYTES` followed by
/// the level page the lineage arrived in that area at. The arrival page is
/// part of the identity because a warp can drop the player back into an area
/// already walked through; the game keeps no settled byte that says so, but
/// the screen never scrolls backward, so a child standing more than a page
/// behind its parent inside one level can only have arrived by warp. Looping
/// through the same warp arrives at the same page and adds no room.
pub type SmbRoomIdentity = [u8; 3];

/// Smallest backward progress step, in buckets, that only a warp can produce
/// within one level: one full screen plus one bucket.
const ROOM_ARRIVAL_SNAP: u16 = 17;

/// Whether admission probes a candidate for viability before retaining it.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum SmbArchiveRetentionPolicy {
    /// Frozen behaviour: every non-terminal action boundary is a retention candidate.
    #[default]
    Frozen,
    /// H45: retain only candidates some fixed probe mask keeps alive for the horizon.
    ProbeAtAdmission,
    /// D68 corridor ruling: the same probe at a 45-frame horizon, admitting
    /// the measured shallow tail the 120-frame demand refuses.
    #[serde(rename = "probe_at_admission_45")]
    ProbeAtAdmission45,
    /// D82 maze ruling: the 45-frame probe plus refusal of candidates whose
    /// progress lands more than sixteen buckets below their parent's within
    /// the same pair — loop traps starve instead of absorbing retention.
    #[serde(rename = "probe_at_admission_45_snapback_16")]
    ProbeAtAdmission45Snapback16,
}

/// Which of a full cell's entries a better candidate displaces.
///
/// The archive key locates a state; it says nothing about what reaching that
/// state cost. Two routes to the same cell therefore collide, and the rule
/// below decides which survives. The frozen rule counts controller actions,
/// which is the currency the search has always ranked in. The level clock is
/// denominated in frames, so a rule that counts frames is a different
/// preference over the same collisions.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SmbArchiveReplacementPolicy {
    /// Frozen behaviour: the candidate displaces the cell's costliest entry
    /// when it uses strictly fewer controller actions.
    #[default]
    FewestActions,
    /// The candidate displaces the cell's costliest entry when it spent
    /// strictly fewer frames inside the current level. Frames-in-level is
    /// derived from the recorded action durations and the recorded level
    /// transitions alone: an entry whose parent shares its pair carries the
    /// parent's count plus its own action's held frames, and an entry whose
    /// parent sits in a different pair starts the count at its own action.
    FewestFramesInLevel,
}

/// How the archive chooses expansion parents.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SmbArchiveSelectorPolicy {
    /// The only selector: corrected tuple key, tie-class frontier with
    /// fall-through, exhaustion accounting, and the H59 recency window.
    ///
    /// The frozen and uncapped-corrected paths were deleted on promotion; a
    /// campaign recorded under either reproduces only at its recording commit.
    #[default]
    ConcentratedRecency,
    /// Cost-normalized per-parent budgets layered over the promoted selector.
    ///
    /// Every active parent receives the registered nonzero exploration floor.
    YieldBudgeted(DrawBudgetParameters),
    /// C84 ruling: every draw is pinned to active entries of the registered
    /// pair inside the registered bucket window, with the concentrated
    /// recency draw applied within the pin; selection falls back to the
    /// promoted behaviour only when the pin is empty.
    PinnedWindow {
        /// Registered pair world.
        world: u8,
        /// Registered pair level.
        level: u8,
        /// Inclusive window low bucket.
        low: u16,
        /// Inclusive window high bucket.
        high: u16,
    },
    /// Every distinct occupied state class gets an equal share of the
    /// three-in-four frontier draws, so effort spreads over every position
    /// and height the archive has reached instead of concentrating on the
    /// deepest progress band. A class is the key tuple
    /// `(world, level, rooms, progress, player_y_bucket)`; the concentrated
    /// recency draw is applied within the chosen class, and exhaustion
    /// accounting is unchanged.
    ClassUniform,
    /// The three-in-four frontier draws split evenly across the rooms of the
    /// deepest occupied `(world, level)` pair that still holds an unexhausted
    /// entry; inside the chosen room the promoted frontier-band walk and the
    /// concentrated recency draw apply unchanged. A room is the key's `room`
    /// coordinate, so a room reached by a warp gets the same share as the
    /// room the level starts in.
    RoomUniform,
    /// `RoomUniform` with the draw inside the chosen room spread evenly over
    /// its progress bands (`FRONTIER_PROGRESS_BAND` wide) that still hold an
    /// unexhausted entry, instead of always taking the deepest one. A band
    /// that keeps admitting new fingerprints of the same screen no longer
    /// monopolizes the room.
    RoomBandUniform,
}

/// Whether a declared waypoint region receives auxiliary retention and
/// selection preference.
///
/// The mechanism is region-agnostic: the region arrives as a registered
/// runtime parameter, exactly as the room key policy carries its room tuple,
/// and every decision derives from recorded keys and the registered region.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum SmbArchiveWaypointPolicy {
    /// Frozen behaviour: no waypoint; retention and selection are untouched.
    #[default]
    Absent,
    /// Track-2 stage 3: states of the registered pair whose progress bucket
    /// and vertical bucket both land inside the inclusive windows form the
    /// waypoint region. In-region cells retain up to
    /// [`WAYPOINT_ENTRIES_PER_KEY`] entries, in-region candidates are exempt
    /// from the snapback refusal, and tie-class selection prefers unexhausted
    /// in-region parents under the concentrated recency draw.
    Region {
        /// Registered pair world.
        world: u8,
        /// Registered pair level.
        level: u8,
        /// Inclusive window low progress bucket.
        low: u16,
        /// Inclusive window high progress bucket.
        high: u16,
        /// Inclusive vertical band low bucket, in the key's vertical term.
        band_low: u8,
        /// Inclusive vertical band high bucket, in the key's vertical term.
        band_high: u8,
    },
    /// C86 census ruling: the same region and preference, with the draw
    /// allocated bucket-uniformly — each occupied progress bucket in the
    /// region equally likely, the concentrated recency draw applied only
    /// within the chosen bucket — so gap-adjacent buckets earn turns
    /// instead of the newest tip cells absorbing the draw.
    RegionBucketUniform {
        /// Registered pair world.
        world: u8,
        /// Registered pair level.
        level: u8,
        /// Inclusive window low progress bucket.
        low: u16,
        /// Inclusive window high progress bucket.
        high: u16,
        /// Inclusive vertical band low bucket, in the key's vertical term.
        band_low: u8,
        /// Inclusive vertical band high bucket, in the key's vertical term.
        band_high: u8,
    },
}

impl SmbArchiveWaypointPolicy {
    /// Report whether a recorded key lands inside the registered region.
    ///
    /// Membership is defined over the key exactly as the active key policy
    /// recorded it, so the band composes with whatever vertical term that
    /// policy wrote; `Absent` contains nothing.
    #[must_use]
    pub fn contains(self, key: &SmbArchiveKey) -> bool {
        match self {
            Self::Absent => false,
            Self::Region {
                world,
                level,
                low,
                high,
                band_low,
                band_high,
            }
            | Self::RegionBucketUniform {
                world,
                level,
                low,
                high,
                band_low,
                band_high,
            } => {
                (key.world, key.level) == (world, level)
                    && (low..=high).contains(&key.progress)
                    && (band_low..=band_high).contains(&key.player_y_bucket)
            }
        }
    }
}

/// Selections since the last retained descendant at which a parent is exhausted.
const SELECTION_EXHAUSTION_THRESHOLD: u64 = 64;

/// H59 recency window: a concentrated tie-class draw samples only this many of
/// the winning class's greatest-id members.
const CONCENTRATION_WINDOW: usize = 128;

/// Which selection path one recorded draw took.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SmbSelectorPath {
    /// The untouched one-in-four uniform draw over all active entries.
    Uniform,
    /// The corrected tie-class frontier draw.
    TieClass,
    /// One occupied state class chosen uniformly, then the concentrated
    /// recency draw within it.
    ClassUniform,
    /// One room of the deepest pair chosen uniformly, then the frontier-band
    /// walk and the concentrated recency draw within it.
    RoomUniform,
    /// One room of the deepest pair chosen uniformly, then one of its
    /// unexhausted progress bands uniformly, then the concentrated recency
    /// draw within it.
    RoomBandUniform,
}

/// One corrected-selector draw, recorded so selection-time state is checkable.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbSelectorDraw {
    /// Path this draw took.
    pub path: SmbSelectorPath,
    /// Fully exhausted tie classes skipped before this draw found its class.
    pub classes_skipped: u64,
    /// Whether this draw found every active entry exhausted and reset the
    /// exhaustion counters.
    pub counter_reset: bool,
    /// Sampled-set state, present only on concentrated tie-class draws.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concentration: Option<SmbConcentrationDraw>,
    /// Whether this tie-class draw was taken through the waypoint
    /// preference; false — and omitted from serialization — everywhere
    /// else, so streams without a waypoint policy are byte-identical.
    #[serde(default, skip_serializing_if = "waypoint_draw_is_absent")]
    pub waypoint: bool,
}

fn waypoint_draw_is_absent(waypoint: &bool) -> bool {
    !*waypoint
}

/// Concentrated sampled-set state at one tie-class draw.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbConcentrationDraw {
    /// Members of the concentrated sampled set at this draw.
    pub window_size: u64,
    /// Sampled-set members at this draw that were never members before.
    pub entered_window: u64,
}

/// Per-campaign accounting for the selector policy that ran.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbSelectorAccounting {
    /// Selector policy that chose every parent in this campaign.
    pub policy: SmbArchiveSelectorPolicy,
    /// Parent selections drawn through the uniform path.
    pub uniform_selections: u64,
    /// Parent selections drawn through the tie-class path.
    pub tie_class_selections: u64,
    /// Selections that produced at least one retained descendant.
    pub productive_selections: u64,
    /// Fully exhausted tie classes skipped across all draws.
    pub classes_skipped: u64,
    /// Deterministic all-exhausted counter resets.
    pub counter_resets: u64,
    /// Concentrated-window accounting, absent under every other policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concentration: Option<SmbConcentrationAccounting>,
    /// Tie-class draws taken through the waypoint preference; zero — and
    /// omitted — whenever no waypoint policy is registered.
    #[serde(default, skip_serializing_if = "waypoint_selections_is_absent")]
    pub waypoint_selections: u64,
}

fn waypoint_selections_is_absent(count: &u64) -> bool {
    *count == 0
}

/// Per-campaign accounting for the concentrated recency window.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbConcentrationAccounting {
    /// Fixed cap on the sampled set.
    pub window_cap: u64,
    /// Sampled-set size at the most recent concentrated tie-class draw.
    pub final_window_size: u64,
    /// Tie-class draws taken through the concentrated window.
    pub window_draws: u64,
    /// Distinct parents that were ever sampled-set members.
    pub distinct_window_parents: u64,
    /// Draws per parent through the window, in thousandths:
    /// `window_draws * 1000 / distinct_window_parents`, floored.
    pub draws_per_parent_milli: u64,
}

impl SmbSelectorAccounting {
    /// Report whether this record should be omitted from a report entirely.
    ///
    /// A frozen-selector campaign must serialize exactly the fields it
    /// serialized before this mechanism existed, so a frozen record writes
    /// nothing.
    #[must_use]
    pub fn is_absent(&self) -> bool {
        *self == Self::default()
    }
}

/// Per-entry selection counters reported under the corrected selector policy.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbEntrySelectorCounters {
    /// Times this entry was selected as a parent.
    pub selected: u64,
    /// Selections of this entry that produced at least one retained descendant.
    pub productive: u64,
}

/// Fixed masks the admission probe tries, in order, stopping at the first survivor.
const VIABILITY_PROBE_MASKS: [u8; 3] = [0x00, 0x01, 0x81];
/// Fixed admission-probe horizon in frames.
const VIABILITY_PROBE_FRAMES: u16 = 120;
/// D68 corridor ruling: the shortened admission-probe horizon in frames.
const VIABILITY_PROBE_FRAMES_SHORT: u16 = 45;

/// One bounded quality-diversity cell for an action-boundary snapshot.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SmbArchiveKey {
    /// Mechanical world number.
    pub world: u8,
    /// Mechanical level number.
    pub level: u8,
    /// Current 16-pixel progress bucket.
    pub progress: u16,
    /// Coarse player vertical-position bucket.
    pub player_y_bucket: u8,
    /// Mechanical player engine state.
    pub player_engine_state: u8,
    /// Six-bit deterministic fingerprint of otherwise-hidden work RAM state.
    pub state_fingerprint: u8,
    /// H75: one-based 16-pixel screen-x bucket, present only for states
    /// inside a registered scroll-frozen room; zero — and omitted from
    /// serialization — everywhere else, so legacy keys are byte-identical.
    #[serde(default, skip_serializing_if = "room_x_bucket_is_absent")]
    pub room_x_bucket: u8,
    /// Count of distinct room values visited within the current level along
    /// the entry's lineage. Zero means the producer does not track rooms;
    /// omitted from serialization so legacy keys stay byte-identical.
    #[serde(default, skip_serializing_if = "rooms_is_absent")]
    pub rooms: u8,
    /// The room the entry stands in, see [`SmbRoomIdentity`]; all zero when
    /// the producer does not track rooms, and omitted from serialization so
    /// legacy keys stay byte-identical.
    #[serde(default, skip_serializing_if = "room_is_absent")]
    pub room: SmbRoomIdentity,
}

fn rooms_is_absent(rooms: &u8) -> bool {
    *rooms == 0
}

fn room_is_absent(room: &SmbRoomIdentity) -> bool {
    *room == SmbRoomIdentity::default()
}

fn room_x_bucket_is_absent(bucket: &u8) -> bool {
    *bucket == 0
}

/// Serializable lineage and retention record for one archived testcase.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbArchiveEntryReport {
    /// Stable insertion-order archive identifier.
    pub id: u64,
    /// Archive parent selected for the suffix execution.
    pub parent_id: Option<u64>,
    /// Target execution that created the entry; zero denotes bootstrap.
    pub created_execution: u64,
    /// Complete clean-reset input represented by this snapshot.
    pub input: SmbInput,
    /// Route-agnostic quality-diversity key.
    pub key: SmbArchiveKey,
    /// Strongest milestones observed along this input.
    pub milestones: SmbMilestones,
    /// Selection counters, present only under the corrected selector policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<SmbEntrySelectorCounters>,
}

/// Deterministic progress sample from one archive campaign.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbArchiveProgressPoint {
    /// Completed target executions.
    pub executions: u64,
    /// Strongest milestone state observed so far.
    pub milestones: SmbMilestones,
    /// Number of active retained archive entries.
    pub active_entries: usize,
    /// Number of occupied quality-diversity cells.
    pub occupied_cells: usize,
    /// Number of terminal death transitions seen so far.
    pub deaths: u64,
}

/// Complete deterministic report for one snapshot-backed suffix-search campaign.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbArchiveReport {
    /// Caller-provided seeded RNG value.
    pub seed: u64,
    /// Number of suffix target executions.
    pub executions: u64,
    /// Strongest milestone values reached.
    pub milestones: SmbMilestones,
    /// Furthest per-frame mechanical position, including action interiors.
    #[serde(default)]
    pub progress_watermark: SmbProgressWatermark,
    /// First execution reaching each frozen milestone rung.
    pub first_reached: SmbMilestoneTimes,
    /// First clean-reset input reaching each rung.
    pub first_inputs: SmbMilestoneInputs,
    /// Current best clean-reset input.
    pub champion_input: SmbInput,
    /// Insertion and replacement records for retained testcases.
    ///
    /// On disk each entry carries only the actions past its parent's input;
    /// the full inputs are rebuilt on load. Archives written with full inputs
    /// still load.
    #[serde(with = "entries_by_suffix")]
    pub entries: Vec<SmbArchiveEntryReport>,
    /// Fixed-interval deterministic progress curve.
    pub progress_curve: Vec<SmbArchiveProgressPoint>,
    /// Candidate snapshots admitted to the active archive.
    pub retained: u64,
    /// Candidate snapshots rejected by bounded quality-diversity retention.
    pub rejected: u64,
    /// Terminal death transitions observed.
    pub deaths: u64,
    /// Extended ladder record, omitted entirely under the frozen ladder policy.
    #[serde(default, skip_serializing_if = "SmbLadder::is_absent")]
    pub ladder: SmbLadder,
    /// Selector accounting, omitted entirely under the frozen selector policy.
    #[serde(default, skip_serializing_if = "SmbSelectorAccounting::is_absent")]
    pub selector: SmbSelectorAccounting,
}

/// Serialized form of the entry list: every entry extends its parent, so the
/// actions past the parent's length identify the input once the parent is
/// rebuilt, at a small fraction of the size of the full input.
mod entries_by_suffix {
    use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};

    use super::{SmbArchiveEntryReport, SmbArchiveKey, SmbEntrySelectorCounters};
    use crate::smb::target::{ButtonChord, SmbInput, SmbMilestones};

    #[derive(Deserialize, Serialize)]
    struct Wire {
        id: u64,
        parent_id: Option<u64>,
        created_execution: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<SmbInput>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_suffix: Option<Vec<ButtonChord>>,
        key: SmbArchiveKey,
        milestones: SmbMilestones,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<SmbEntrySelectorCounters>,
    }

    pub fn serialize<S: Serializer>(
        entries: &[SmbArchiveEntryReport],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let index_of: std::collections::BTreeMap<u64, usize> = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.id, index))
            .collect();
        let wires: Vec<Wire> = entries
            .iter()
            .map(|entry| {
                let parent = entry
                    .parent_id
                    .and_then(|id| index_of.get(&id))
                    .map(|index| &entries[*index].input.actions)
                    .filter(|parent| entry.input.actions.starts_with(parent));
                let (input, input_suffix) = match parent {
                    Some(parent) => (None, Some(entry.input.actions[parent.len()..].to_vec())),
                    None => (Some(entry.input.clone()), None),
                };
                Wire {
                    id: entry.id,
                    parent_id: entry.parent_id,
                    created_execution: entry.created_execution,
                    input,
                    input_suffix,
                    key: entry.key,
                    milestones: entry.milestones,
                    selector: entry.selector,
                }
            })
            .collect();
        wires.serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<SmbArchiveEntryReport>, D::Error> {
        let wires = Vec::<Wire>::deserialize(deserializer)?;
        let mut entries: Vec<SmbArchiveEntryReport> = Vec::with_capacity(wires.len());
        let mut index_of = std::collections::BTreeMap::<u64, usize>::new();
        for wire in wires {
            let input = match (wire.input, wire.input_suffix) {
                (Some(input), None) => input,
                (None, Some(suffix)) => {
                    let mut actions = match wire.parent_id.and_then(|id| index_of.get(&id)) {
                        Some(index) => entries[*index].input.actions.clone(),
                        None => {
                            return Err(D::Error::custom(format!(
                                "archive entry {} carries an input suffix without a loaded parent",
                                wire.id
                            )));
                        }
                    };
                    actions.extend(suffix);
                    SmbInput { actions }
                }
                _ => {
                    return Err(D::Error::custom(format!(
                        "archive entry {} must carry exactly one of input and input_suffix",
                        wire.id
                    )));
                }
            };
            index_of.insert(wire.id, entries.len());
            entries.push(SmbArchiveEntryReport {
                id: wire.id,
                parent_id: wire.parent_id,
                created_execution: wire.created_execution,
                input,
                key: wire.key,
                milestones: wire.milestones,
                selector: wire.selector,
            });
        }
        Ok(entries)
    }
}

/// Mechanical outcome of one fixed frontier-viability continuation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SmbViabilityClass {
    /// The continuation reached player engine kill state `$0b`.
    KillState,
    /// The continuation ended in vertical bucket 15 without registering kill.
    BelowPlayable,
    /// The continuation ended outside the two doomed classes.
    Controllable,
}

/// Viability result for one active archive representative.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbViabilityEntry {
    /// Stable source archive identifier.
    pub id: u64,
    /// Whether this entry belongs to the maximal progress-39 frontier.
    pub frontier: bool,
    /// Recorded archive key at the audited endpoint.
    pub key: SmbArchiveKey,
    /// No-input continuation followed by the nine frozen controller masks.
    pub continuations: Vec<SmbViabilityClass>,
    /// True only when no continuation remains controllable.
    pub doomed: bool,
}

/// Count summary for one audited archive slice.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbViabilityCounts {
    /// Audited active representatives.
    pub total: u64,
    /// Representatives with no controllable continuation.
    pub doomed: u64,
}

/// Deterministic D27 frontier-viability report.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbFrontierViabilityReport {
    /// Frames applied by every fixed continuation.
    pub continuation_frames: u8,
    /// No-input plus frozen mask order used by every entry.
    pub continuation_masks: Vec<Option<u8>>,
    /// Maximal progress-39 counts.
    pub frontier: SmbViabilityCounts,
    /// Inclusive progress-32-through-39 approach-band counts.
    pub approach_band: SmbViabilityCounts,
    /// Stable per-entry evidence in input lexical order.
    pub entries: Vec<SmbViabilityEntry>,
}

#[derive(Clone, Debug)]
pub(crate) struct ArchiveEntry {
    pub(crate) report: SmbArchiveEntryReport,
    pub(crate) snapshot: SmbSnapshot,
}

pub(crate) struct ArchiveCandidate {
    pub(crate) input: SmbInput,
    pub(crate) key: SmbArchiveKey,
    pub(crate) milestones: SmbMilestones,
}

pub(crate) struct Archive {
    /// Retention stops when the entry count reaches this bound; campaign
    /// runs record their bound in the stream header and replay under it.
    pub(crate) max_entries: usize,
    pub(crate) entries: Vec<ArchiveEntry>,
    pub(crate) active: Vec<bool>,
    pub(crate) cells: BTreeMap<SmbArchiveKey, Vec<usize>>,
    pub(crate) input_ids: BTreeMap<SmbInput, usize>,
    pub(crate) retained: u64,
    pub(crate) rejected: u64,
    selected: Vec<u64>,
    productive: Vec<u64>,
    since_retained: Vec<u64>,
    budget_draws: Vec<u64>,
    draw_budgets: DrawBudgets<usize>,
    in_window_ever: Vec<bool>,
    selector_accounting: SmbSelectorAccounting,
    waypoint_policy: SmbArchiveWaypointPolicy,
    waypoint_retained: u64,
    replacement_policy: SmbArchiveReplacementPolicy,
    /// Frames each retained entry spent inside its own pair, in entry-id
    /// order. Carried alongside the entries rather than in the serialized
    /// report, so an archive written under either policy is byte-identical.
    frames_in_level: Vec<u64>,
    replacement_frames_displaced: u64,
    key_policy: SmbArchiveKeyPolicy,
    /// Sorted distinct room identities per entry, filled only under
    /// `SmbArchiveKeyPolicy::FrozenRooms`; otherwise every entry holds an
    /// empty set.
    room_sets: Vec<Vec<SmbRoomIdentity>>,
    /// The room each entry currently stands in, aligned with `room_sets`.
    current_rooms: Vec<SmbRoomIdentity>,
}

impl Archive {
    pub(crate) fn set_key_policy(&mut self, policy: SmbArchiveKeyPolicy) {
        self.key_policy = policy;
    }

    /// Distinct room identities a retained entry's lineage visited inside
    /// its level, sorted; empty unless the rooms key policy is active.
    #[must_use]
    pub fn room_set(&self, id: usize) -> &[SmbRoomIdentity] {
        self.room_sets.get(id).map_or(&[], Vec::as_slice)
    }

    pub(crate) fn set_selector_policy(&mut self, policy: SmbArchiveSelectorPolicy) {
        self.selector_accounting.policy = policy;
    }

    pub(crate) fn set_waypoint_policy(&mut self, policy: SmbArchiveWaypointPolicy) {
        self.waypoint_policy = policy;
    }

    pub(crate) fn set_replacement_policy(&mut self, policy: SmbArchiveReplacementPolicy) {
        self.replacement_policy = policy;
    }

    /// Cell collisions the frames-in-level rule decided, counted for the
    /// report; the frozen rule never increments it.
    pub(crate) fn replacement_frames_displaced(&self) -> u64 {
        self.replacement_frames_displaced
    }

    /// Frames a retained entry spent inside its own pair.
    #[cfg(test)]
    pub(crate) fn entry_frames_in_level(&self, id: usize) -> u64 {
        self.frames_in_level[id]
    }

    /// Deepest recorded tuple, the fewest frames any entry there spent inside
    /// its pair, and the retained total.
    ///
    /// Read-only. Nothing here consumes randomness or mutates archive state, so
    /// calling it cannot change what a run records.
    pub(crate) fn live_progress(&self) -> (u8, u8, u16, u64, u64) {
        let deepest = self
            .entries
            .iter()
            .map(|entry| {
                (
                    entry.report.key.world,
                    entry.report.key.level,
                    entry.report.key.progress,
                )
            })
            .max()
            .unwrap_or((0, 0, 0));
        let cheapest = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                (
                    entry.report.key.world,
                    entry.report.key.level,
                    entry.report.key.progress,
                ) == deepest
            })
            .map(|(index, _)| self.frames_in_level.get(index).copied().unwrap_or(0))
            .min()
            .unwrap_or(0);
        (deepest.0, deepest.1, deepest.2, cheapest, self.retained)
    }

    /// Report whether a key sits inside the registered waypoint region.
    pub(crate) fn waypoint_contains(&self, key: &SmbArchiveKey) -> bool {
        self.waypoint_policy.contains(key)
    }

    /// Candidates retained through the waypoint auxiliary cell capacity.
    pub(crate) fn waypoint_retained(&self) -> u64 {
        self.waypoint_retained
    }

    pub(crate) fn new() -> Self {
        Self {
            max_entries: MAX_ARCHIVE_ENTRIES,
            entries: Vec::new(),
            active: Vec::new(),
            cells: BTreeMap::new(),
            input_ids: BTreeMap::new(),
            retained: 0,
            rejected: 0,
            selected: Vec::new(),
            productive: Vec::new(),
            since_retained: Vec::new(),
            budget_draws: Vec::new(),
            draw_budgets: DrawBudgets::default(),
            in_window_ever: Vec::new(),
            selector_accounting: SmbSelectorAccounting {
                policy: SmbArchiveSelectorPolicy::ConcentratedRecency,
                concentration: Some(SmbConcentrationAccounting {
                    window_cap: u64::try_from(CONCENTRATION_WINDOW).unwrap_or(u64::MAX),
                    ..SmbConcentrationAccounting::default()
                }),
                ..SmbSelectorAccounting::default()
            },
            waypoint_policy: SmbArchiveWaypointPolicy::Absent,
            waypoint_retained: 0,
            replacement_policy: SmbArchiveReplacementPolicy::FewestActions,
            frames_in_level: Vec::new(),
            replacement_frames_displaced: 0,
            key_policy: SmbArchiveKeyPolicy::Frozen,
            room_sets: Vec::new(),
            current_rooms: Vec::new(),
        }
    }

    /// Frames a candidate spent inside its own pair.
    ///
    /// An input extends its parent's, so the frames added since the parent are
    /// the held frames of the actions past the parent's length. A candidate
    /// whose parent already sits in the same pair inherits the parent's count;
    /// one whose parent sits elsewhere entered the pair during those actions
    /// and starts the count there. A candidate with no parent — genesis, and
    /// only genesis — counts its whole input.
    fn frames_in_level_of(
        &self,
        parent_id: Option<usize>,
        input: &SmbInput,
        key: SmbArchiveKey,
    ) -> u64 {
        let frames_of = |actions: &[ButtonChord]| -> u64 {
            actions
                .iter()
                .map(|action| u64::from(action.bounded_hold_frames()))
                .sum()
        };
        let Some(parent) = parent_id.and_then(|id| self.entries.get(id)) else {
            return frames_of(&input.actions);
        };
        let parent_actions = parent.report.input.actions.len();
        let added = frames_of(input.actions.get(parent_actions..).unwrap_or(&[]));
        let parent_key = parent.report.key;
        if (parent_key.world, parent_key.level) == (key.world, key.level) {
            self.frames_in_level
                .get(parent_id.unwrap_or_default())
                .copied()
                .unwrap_or(0)
                .saturating_add(added)
        } else {
            added
        }
    }

    pub(crate) fn insert(
        &mut self,
        parent_id: Option<usize>,
        execution: u64,
        candidate: ArchiveCandidate,
        snapshot: SmbSnapshot,
    ) -> Result<Option<usize>, Box<dyn Error>> {
        let ArchiveCandidate {
            input,
            mut key,
            milestones,
        } = candidate;
        if let Some(existing) = self.input_ids.get(&input) {
            return Ok(Some(*existing));
        }
        let (room_set, current_room) = match self.key_policy {
            SmbArchiveKeyPolicy::FrozenRooms => {
                let (set, current) = self.room_set_for(parent_id, key, snapshot.wram())?;
                key.rooms = u8::try_from(set.len())?;
                (set, current)
            }
            SmbArchiveKeyPolicy::FrozenRoom => {
                let (set, current) = self.room_set_for(parent_id, key, snapshot.wram())?;
                key.room = current;
                (set, current)
            }
            SmbArchiveKeyPolicy::FrozenArea => {
                let (set, current) = self.area_room_set_for(parent_id, key, snapshot.wram())?;
                key.room = current;
                (set, current)
            }
            SmbArchiveKeyPolicy::FrozenAreaSpan => {
                let (set, current) =
                    self.room_set_with(parent_id, key, snapshot.wram(), RoomRule::AreaSpan)?;
                key.room = current;
                (set, current)
            }
            _ => (Vec::new(), SmbRoomIdentity::default()),
        };
        // Frames this candidate spent inside its own pair, derived from the
        // recorded action durations and the parent's recorded pair alone. It
        // is computed for every insertion under either policy so the counts
        // stay aligned with entry ids, and read only by the frames rule.
        let candidate_frames_in_level = self.frames_in_level_of(parent_id, &input, key);
        // Waypoint retention preference: cells inside the registered region
        // retain up to the auxiliary bound before the replacement rules
        // apply; every other cell keeps the base bound, so an absent policy
        // is byte-identical to base.
        let entries_per_key = if self.waypoint_policy.contains(&key) {
            WAYPOINT_ENTRIES_PER_KEY
        } else {
            MAX_ENTRIES_PER_KEY
        };
        let cell = self.cells.entry(key).or_default();
        let mut frames_replacement = false;
        let replace = if cell.len() < entries_per_key {
            None
        } else if self.replacement_policy == SmbArchiveReplacementPolicy::FewestFramesInLevel {
            // The costliest entry in the level's own currency loses to a
            // candidate that reached the same cell in strictly fewer frames.
            // The entry id breaks ties exactly as the frozen rule's cost does,
            // so the choice stays a total order over the cell.
            cell.iter()
                .copied()
                .max_by_key(|id| (self.frames_in_level[*id], self.entries[*id].report.id))
                .filter(|id| candidate_frames_in_level < self.frames_in_level[*id])
                .inspect(|_| frames_replacement = true)
        } else {
            cell.iter()
                .copied()
                .max_by_key(|id| entry_cost(&self.entries[*id].report))
                .filter(|id| input.actions.len() < self.entries[*id].report.input.actions.len())
        };
        if cell.len() >= entries_per_key && replace.is_none() {
            self.rejected = self.rejected.saturating_add(1);
            return Ok(None);
        }
        if self.entries.len() >= self.max_entries {
            self.rejected = self.rejected.saturating_add(1);
            return Ok(None);
        }
        if replace.is_none() && cell.len() >= MAX_ENTRIES_PER_KEY {
            self.waypoint_retained = self.waypoint_retained.saturating_add(1);
        }
        if let Some(replaced) = replace {
            self.active[replaced] = false;
            cell.retain(|id| *id != replaced);
        }
        if frames_replacement {
            self.replacement_frames_displaced = self.replacement_frames_displaced.saturating_add(1);
        }
        let id = self.entries.len();
        let report = SmbArchiveEntryReport {
            id: u64::try_from(id)?,
            parent_id: parent_id.map(u64::try_from).transpose()?,
            created_execution: execution,
            input: input.clone(),
            key,
            milestones,
            selector: None,
        };
        self.entries.push(ArchiveEntry { report, snapshot });
        self.active.push(true);
        self.room_sets.push(room_set);
        self.current_rooms.push(current_room);
        self.frames_in_level.push(candidate_frames_in_level);
        self.selected.push(0);
        self.productive.push(0);
        self.since_retained.push(0);
        self.budget_draws.push(0);
        self.in_window_ever.push(false);
        cell.push(id);
        self.input_ids.insert(input, id);
        self.retained = self.retained.saturating_add(1);
        Ok(Some(id))
    }

    /// The candidate's sorted room set and the room it stands in: the parent's
    /// set when the candidate stays in the parent's level, otherwise a fresh
    /// set, plus the candidate's room. The candidate keeps the parent's room
    /// unless the area bytes changed or it arrived by warp.
    fn room_set_for(
        &self,
        parent_id: Option<usize>,
        key: SmbArchiveKey,
        wram: &[u8],
    ) -> Result<(Vec<SmbRoomIdentity>, SmbRoomIdentity), Box<dyn Error>> {
        self.room_set_with(parent_id, key, wram, RoomRule::WarpsOpenRooms)
    }

    fn area_room_set_for(
        &self,
        parent_id: Option<usize>,
        key: SmbArchiveKey,
        wram: &[u8],
    ) -> Result<(Vec<SmbRoomIdentity>, SmbRoomIdentity), Box<dyn Error>> {
        self.room_set_with(parent_id, key, wram, RoomRule::AreaOnly)
    }

    /// Room set and current room of a candidate. With `warps_open_rooms` a
    /// backward warp inside one area opens a room keyed by its arrival page;
    /// without it only an area change does.
    fn room_set_with(
        &self,
        parent_id: Option<usize>,
        key: SmbArchiveKey,
        wram: &[u8],
        rule: RoomRule,
    ) -> Result<(Vec<SmbRoomIdentity>, SmbRoomIdentity), Box<dyn Error>> {
        let mut area = [0_u8; 2];
        for (slot, offset) in area.iter_mut().zip(ROOM_IDENTITY_BYTES) {
            *slot = *wram
                .get(offset)
                .ok_or("room identity byte outside work RAM")?;
        }
        let arrival_page = u8::try_from(key.progress / 16)?;
        let arrived_here = [area[0], area[1], arrival_page];
        let parent = parent_id
            .map(|parent| {
                self.entries
                    .get(parent)
                    .map(|entry| (entry.report.key, self.current_rooms[parent]))
                    .ok_or("room set parent is missing")
            })
            .transpose()?;
        let (mut set, current) = match parent {
            Some((parent_key, parent_room))
                if (parent_key.world, parent_key.level) == (key.world, key.level) =>
            {
                let same_area = parent_room[..2] == area;
                let warped = parent_key.progress >= key.progress.saturating_add(ROOM_ARRIVAL_SNAP);
                let set = self.room_set(parent_id.unwrap_or_default()).to_vec();
                let current = match rule {
                    _ if !same_area => arrived_here,
                    RoomRule::WarpsOpenRooms if warped => arrived_here,
                    RoomRule::AreaSpan if warped => set
                        .iter()
                        .copied()
                        .filter(|room| room[..2] == area && room[2] <= arrival_page)
                        .max_by_key(|room| room[2])
                        .unwrap_or(arrived_here),
                    _ => parent_room,
                };
                (set, current)
            }
            _ => (Vec::new(), arrived_here),
        };
        if let Err(slot) = set.binary_search(&current) {
            set.insert(slot, current);
        }
        Ok((set, current))
    }

    fn active_ids(&self, max_actions: usize) -> Vec<usize> {
        self.active
            .iter()
            .enumerate()
            .filter_map(|(id, active)| {
                (*active && self.entries[id].report.input.actions.len() < max_actions).then_some(id)
            })
            .collect()
    }

    fn selector_unexhausted(&self, id: usize) -> bool {
        match self.selector_accounting.policy {
            SmbArchiveSelectorPolicy::YieldBudgeted(parameters) => self
                .draw_budgets
                .budget(&id, parameters)
                .is_ok_and(|budget| self.budget_draws[id] < budget),
            SmbArchiveSelectorPolicy::ConcentratedRecency
            | SmbArchiveSelectorPolicy::PinnedWindow { .. }
            | SmbArchiveSelectorPolicy::ClassUniform
            | SmbArchiveSelectorPolicy::RoomUniform
            | SmbArchiveSelectorPolicy::RoomBandUniform => {
                self.since_retained[id] < SELECTION_EXHAUSTION_THRESHOLD
            }
        }
    }

    /// Choose a parent. There is one selector, so every draw reports a record.
    pub(crate) fn select_parent(
        &mut self,
        rand: &mut StdRand,
        max_actions: usize,
    ) -> Result<(usize, Option<SmbSelectorDraw>), Box<dyn Error>> {
        self.choose_parent_corrected(rand, max_actions)
            .map(|(id, draw)| (id, Some(draw)))
    }

    /// H56 corrected selection: corrected key, tie-class frontier with
    /// fall-through, exhaustion-aware sampling. Under the H59 concentrated
    /// policy the final tie-class draw narrows to the recency window.
    fn choose_parent_corrected(
        &mut self,
        rand: &mut StdRand,
        max_actions: usize,
    ) -> Result<(usize, SmbSelectorDraw), Box<dyn Error>> {
        let active = self.active_ids(max_actions);
        if active.is_empty() {
            return Err("SMB archive has no expandable entry".into());
        }
        if let SmbArchiveSelectorPolicy::YieldBudgeted(parameters) = self.selector_accounting.policy
        {
            parameters.validate()?;
        }
        // C84 ruling: under the pinned policy every draw narrows to the
        // registered window when it is populated.
        let base_pool: Vec<usize> = match self.selector_accounting.policy {
            SmbArchiveSelectorPolicy::PinnedWindow {
                world,
                level,
                low,
                high,
            } => {
                let members: Vec<usize> = active
                    .iter()
                    .copied()
                    .filter(|id| {
                        let key = self.entries[*id].report.key;
                        (key.world, key.level) == (world, level)
                            && key.progress >= low
                            && key.progress <= high
                    })
                    .collect();
                if members.is_empty() { active } else { members }
            }
            SmbArchiveSelectorPolicy::ConcentratedRecency
            | SmbArchiveSelectorPolicy::YieldBudgeted(_)
            | SmbArchiveSelectorPolicy::ClassUniform
            | SmbArchiveSelectorPolicy::RoomUniform
            | SmbArchiveSelectorPolicy::RoomBandUniform => active,
        };
        let mut counter_reset = false;
        let pool =
            if let SmbArchiveSelectorPolicy::YieldBudgeted(_) = self.selector_accounting.policy {
                let living = base_pool
                    .iter()
                    .copied()
                    .filter(|id| self.selector_unexhausted(*id))
                    .collect::<Vec<_>>();
                if living.is_empty() {
                    for id in &base_pool {
                        self.budget_draws[*id] = 0;
                    }
                    counter_reset = true;
                    base_pool
                } else {
                    living
                }
            } else {
                base_pool
            };
        let use_frontier = rand.below(NonZeroUsize::new(4).ok_or("invalid frontier odds")?) != 0;
        if !use_frontier {
            let id = pool[rand.below(NonZeroUsize::new(pool.len()).ok_or("empty archive")?)];
            return Ok((
                id,
                SmbSelectorDraw {
                    path: SmbSelectorPath::Uniform,
                    classes_skipped: 0,
                    counter_reset,
                    concentration: None,
                    waypoint: false,
                },
            ));
        }
        let mut classes_skipped = 0_u64;
        loop {
            // Waypoint selection preference. While the region holds
            // unexhausted pool members, every tie-class draw samples them
            // through the same concentrated recency draw a winning class
            // gets; the uniform path stays untouched. Composition with the
            // pinned window is pool-first: the pin narrowed the pool above,
            // so a waypoint outside the pin finds no members here and the
            // draw falls through — the pin outranks the preference. The
            // preference is bounded by the exhaustion discipline: each
            // member barrens after the standing threshold of unproductive
            // selections, after which the promoted class walk resumes.
            if self.waypoint_policy != SmbArchiveWaypointPolicy::Absent {
                let members: Vec<usize> = pool
                    .iter()
                    .copied()
                    .filter(|id| {
                        self.waypoint_policy.contains(&self.entries[*id].report.key)
                            && self.selector_unexhausted(*id)
                    })
                    .collect();
                if !members.is_empty() {
                    let members = if matches!(
                        self.waypoint_policy,
                        SmbArchiveWaypointPolicy::RegionBucketUniform { .. }
                    ) {
                        // Bucket-uniform allocation: choose one occupied
                        // progress bucket uniformly, then apply the recency
                        // draw within it.
                        let mut buckets: Vec<u16> = members
                            .iter()
                            .map(|id| self.entries[*id].report.key.progress)
                            .collect();
                        buckets.sort_unstable();
                        buckets.dedup();
                        let chosen = buckets[rand.below(
                            NonZeroUsize::new(buckets.len()).ok_or("empty waypoint bucket set")?,
                        )];
                        members
                            .into_iter()
                            .filter(|id| self.entries[*id].report.key.progress == chosen)
                            .collect()
                    } else {
                        members
                    };
                    let (id, concentration) = self.draw_from_class(rand, members)?;
                    return Ok((
                        id,
                        SmbSelectorDraw {
                            path: SmbSelectorPath::TieClass,
                            classes_skipped,
                            counter_reset,
                            concentration,
                            waypoint: true,
                        },
                    ));
                }
            }
            if self.selector_accounting.policy == SmbArchiveSelectorPolicy::ClassUniform {
                if let Some(class) = self.uniform_unexhausted_class(rand, &pool)? {
                    let (id, concentration) = self.draw_from_class(rand, class)?;
                    return Ok((
                        id,
                        SmbSelectorDraw {
                            path: SmbSelectorPath::ClassUniform,
                            classes_skipped,
                            counter_reset,
                            concentration,
                            waypoint: false,
                        },
                    ));
                }
            } else if self.selector_accounting.policy == SmbArchiveSelectorPolicy::RoomUniform {
                if let Some(class) = self.room_uniform_class(rand, &pool, &mut classes_skipped)? {
                    let (id, concentration) = self.draw_from_class(rand, class)?;
                    return Ok((
                        id,
                        SmbSelectorDraw {
                            path: SmbSelectorPath::RoomUniform,
                            classes_skipped,
                            counter_reset,
                            concentration,
                            waypoint: false,
                        },
                    ));
                }
            } else if self.selector_accounting.policy == SmbArchiveSelectorPolicy::RoomBandUniform {
                if let Some(class) =
                    self.room_band_uniform_class(rand, &pool, &mut classes_skipped)?
                {
                    let (id, concentration) = self.draw_from_class(rand, class)?;
                    return Ok((
                        id,
                        SmbSelectorDraw {
                            path: SmbSelectorPath::RoomBandUniform,
                            classes_skipped,
                            counter_reset,
                            concentration,
                            waypoint: false,
                        },
                    ));
                }
            } else if let Some(class) = self.best_unexhausted_class(&pool, &mut classes_skipped) {
                let (id, concentration) = self.draw_from_class(rand, class)?;
                return Ok((
                    id,
                    SmbSelectorDraw {
                        path: SmbSelectorPath::TieClass,
                        classes_skipped,
                        counter_reset,
                        concentration,
                        waypoint: false,
                    },
                ));
            }
            if counter_reset {
                return Err("selection counter reset freed no entry".into());
            }
            let counters = if matches!(
                self.selector_accounting.policy,
                SmbArchiveSelectorPolicy::YieldBudgeted(_)
            ) {
                &mut self.budget_draws
            } else {
                &mut self.since_retained
            };
            for counter in counters {
                *counter = 0;
            }
            counter_reset = true;
        }
    }

    /// The unexhausted members of one occupied state class chosen uniformly
    /// among the classes that still hold an unexhausted member, or `None`
    /// when every active entry is exhausted.
    fn uniform_unexhausted_class(
        &self,
        rand: &mut StdRand,
        active: &[usize],
    ) -> Result<Option<Vec<usize>>, Box<dyn Error>> {
        let mut classes = BTreeMap::<(u8, u8, u8, u16, u8), Vec<usize>>::new();
        for id in active {
            if !self.selector_unexhausted(*id) {
                continue;
            }
            let key = self.entries[*id].report.key;
            classes
                .entry((
                    key.world,
                    key.level,
                    key.rooms,
                    key.progress,
                    key.player_y_bucket,
                ))
                .or_default()
                .push(*id);
        }
        let Some(count) = NonZeroUsize::new(classes.len()) else {
            return Ok(None);
        };
        let chosen = rand.below(count);
        Ok(classes.into_values().nth(chosen))
    }

    /// The unexhausted members of the best surviving tie class, or `None` when
    /// every active entry is exhausted.
    ///
    /// Classes are `(world, level)` pairs in descending order, banded within a
    /// pair by successive `FRONTIER_PROGRESS_BAND` windows below each deepest
    /// remaining progress. Fully exhausted classes are counted and skipped.
    fn best_unexhausted_class(
        &self,
        active: &[usize],
        classes_skipped: &mut u64,
    ) -> Option<Vec<usize>> {
        let mut pairs = BTreeMap::<(u8, u8, u8), Vec<usize>>::new();
        for id in active {
            let key = self.entries[*id].report.key;
            pairs
                .entry((key.world, key.level, key.rooms))
                .or_default()
                .push(*id);
        }
        for (_, members) in pairs.into_iter().rev() {
            if let Some(unexhausted) = self.best_unexhausted_band(members, classes_skipped) {
                return Some(unexhausted);
            }
        }
        None
    }

    /// The unexhausted members of the deepest surviving progress band among
    /// `members`, walking successive `FRONTIER_PROGRESS_BAND` windows down
    /// from the deepest progress; exhausted bands are counted and skipped.
    fn best_unexhausted_band(
        &self,
        mut members: Vec<usize>,
        classes_skipped: &mut u64,
    ) -> Option<Vec<usize>> {
        members.sort_by_key(|id| (Reverse(self.entries[*id].report.key.progress), *id));
        let mut start = 0;
        while start < members.len() {
            let anchor = self.entries[members[start]].report.key.progress;
            let mut end = start;
            while end < members.len()
                && self.entries[members[end]]
                    .report
                    .key
                    .progress
                    .saturating_add(FRONTIER_PROGRESS_BAND - 1)
                    >= anchor
            {
                end += 1;
            }
            let unexhausted = members[start..end]
                .iter()
                .copied()
                .filter(|id| self.selector_unexhausted(*id))
                .collect::<Vec<_>>();
            if !unexhausted.is_empty() {
                return Some(unexhausted);
            }
            *classes_skipped = classes_skipped.saturating_add(1);
            start = end;
        }
        None
    }

    /// The unexhausted members of the deepest band of one room, the room
    /// chosen uniformly among the rooms of the deepest `(world, level)` pair
    /// that still holds an unexhausted entry; `None` when every active entry
    /// is exhausted. Pairs whose rooms are all exhausted are walked past and
    /// their bands counted as skipped.
    fn room_uniform_class(
        &self,
        rand: &mut StdRand,
        active: &[usize],
        classes_skipped: &mut u64,
    ) -> Result<Option<Vec<usize>>, Box<dyn Error>> {
        let mut pairs = BTreeMap::<(u8, u8), BTreeMap<SmbRoomIdentity, Vec<usize>>>::new();
        for id in active {
            let key = self.entries[*id].report.key;
            pairs
                .entry((key.world, key.level))
                .or_default()
                .entry(key.room)
                .or_default()
                .push(*id);
        }
        for (_, rooms) in pairs.into_iter().rev() {
            let mut live = Vec::new();
            for (_, members) in rooms {
                let mut skipped = 0_u64;
                match self.best_unexhausted_band(members, &mut skipped) {
                    Some(band) => live.push((band, skipped)),
                    None => *classes_skipped = classes_skipped.saturating_add(skipped),
                }
            }
            let Some(count) = NonZeroUsize::new(live.len()) else {
                continue;
            };
            let (band, skipped) = live.swap_remove(rand.below(count));
            *classes_skipped = classes_skipped.saturating_add(skipped);
            return Ok(Some(band));
        }
        Ok(None)
    }

    /// The unexhausted members of every fixed-width progress band of
    /// `members`, deepest band first; exhausted bands are counted as skipped.
    fn unexhausted_bands(&self, members: &[usize], classes_skipped: &mut u64) -> Vec<Vec<usize>> {
        let mut bands = BTreeMap::<Reverse<u16>, Vec<usize>>::new();
        for id in members {
            let band = self.entries[*id].report.key.progress / FRONTIER_PROGRESS_BAND;
            bands.entry(Reverse(band)).or_default().push(*id);
        }
        let mut live = Vec::new();
        for (_, band) in bands {
            let unexhausted = band
                .into_iter()
                .filter(|id| self.selector_unexhausted(*id))
                .collect::<Vec<_>>();
            if unexhausted.is_empty() {
                *classes_skipped = classes_skipped.saturating_add(1);
            } else {
                live.push(unexhausted);
            }
        }
        live
    }

    /// One unexhausted band of one room: the room chosen uniformly among the
    /// rooms of the deepest `(world, level)` pair with an unexhausted entry,
    /// the band uniformly among that room's unexhausted bands; `None` when
    /// every active entry is exhausted.
    fn room_band_uniform_class(
        &self,
        rand: &mut StdRand,
        active: &[usize],
        classes_skipped: &mut u64,
    ) -> Result<Option<Vec<usize>>, Box<dyn Error>> {
        let mut pairs = BTreeMap::<(u8, u8), BTreeMap<SmbRoomIdentity, Vec<usize>>>::new();
        for id in active {
            let key = self.entries[*id].report.key;
            pairs
                .entry((key.world, key.level))
                .or_default()
                .entry(key.room)
                .or_default()
                .push(*id);
        }
        for (_, rooms) in pairs.into_iter().rev() {
            let mut live = Vec::new();
            for (_, members) in rooms {
                let mut skipped = 0_u64;
                let bands = self.unexhausted_bands(&members, &mut skipped);
                if bands.is_empty() {
                    *classes_skipped = classes_skipped.saturating_add(skipped);
                } else {
                    live.push((bands, skipped));
                }
            }
            let Some(count) = NonZeroUsize::new(live.len()) else {
                continue;
            };
            let (mut bands, skipped) = live.swap_remove(rand.below(count));
            *classes_skipped = classes_skipped.saturating_add(skipped);
            let band = bands
                .swap_remove(rand.below(NonZeroUsize::new(bands.len()).ok_or("empty band list")?));
            return Ok(Some(band));
        }
        Ok(None)
    }

    /// Uniform draw within the winning tie class; the H59 concentrated policy
    /// narrows it to the class's `CONCENTRATION_WINDOW` greatest-id members.
    ///
    /// Entry ids are creation order, so the greatest ids are the class's most
    /// recently retained members. Membership is recomputed at every draw: a
    /// member leaves when `CONCENTRATION_WINDOW` newer sampleable class
    /// members exist, or immediately when it exhausts.
    fn draw_from_class(
        &mut self,
        rand: &mut StdRand,
        mut class: Vec<usize>,
    ) -> Result<(usize, Option<SmbConcentrationDraw>), Box<dyn Error>> {
        class.sort_unstable();
        let window = &class[class.len().saturating_sub(CONCENTRATION_WINDOW)..];
        let mut entered_window = 0_u64;
        for id in window {
            if !self.in_window_ever[*id] {
                self.in_window_ever[*id] = true;
                entered_window = entered_window.saturating_add(1);
            }
        }
        let id = window[rand.below(NonZeroUsize::new(window.len()).ok_or("empty tie window")?)];
        Ok((
            id,
            Some(SmbConcentrationDraw {
                window_size: u64::try_from(window.len())?,
                entered_window,
            }),
        ))
    }

    /// Account one recorded selection of `id`.
    pub(crate) fn record_selection(&mut self, id: usize, draw: &SmbSelectorDraw) {
        self.selected[id] = self.selected[id].saturating_add(1);
        self.since_retained[id] = self.since_retained[id].saturating_add(1);
        match draw.path {
            SmbSelectorPath::Uniform => {
                self.selector_accounting.uniform_selections = self
                    .selector_accounting
                    .uniform_selections
                    .saturating_add(1);
            }
            SmbSelectorPath::TieClass
            | SmbSelectorPath::ClassUniform
            | SmbSelectorPath::RoomUniform
            | SmbSelectorPath::RoomBandUniform => {
                self.selector_accounting.tie_class_selections = self
                    .selector_accounting
                    .tie_class_selections
                    .saturating_add(1);
            }
        }
        self.selector_accounting.classes_skipped = self
            .selector_accounting
            .classes_skipped
            .saturating_add(draw.classes_skipped);
        self.selector_accounting.counter_resets = self
            .selector_accounting
            .counter_resets
            .saturating_add(u64::from(draw.counter_reset));
        self.selector_accounting.waypoint_selections = self
            .selector_accounting
            .waypoint_selections
            .saturating_add(u64::from(draw.waypoint));
        if let (Some(accounting), Some(concentration)) = (
            self.selector_accounting.concentration.as_mut(),
            draw.concentration.as_ref(),
        ) {
            accounting.window_draws = accounting.window_draws.saturating_add(1);
            accounting.final_window_size = concentration.window_size;
            accounting.distinct_window_parents = accounting
                .distinct_window_parents
                .saturating_add(concentration.entered_window);
            accounting.draws_per_parent_milli = accounting
                .window_draws
                .saturating_mul(1000)
                .checked_div(accounting.distinct_window_parents)
                .unwrap_or(0);
        }
    }

    /// Account one selection's discovery outcome and deterministic execution cost.
    pub(crate) fn record_selection_outcome(
        &mut self,
        id: usize,
        retained_descendant: bool,
        cost: u64,
    ) -> Result<(), Box<dyn Error>> {
        if let SmbArchiveSelectorPolicy::YieldBudgeted(parameters) = self.selector_accounting.policy
        {
            self.budget_draws[id] = self.budget_draws[id].saturating_add(1);
            self.draw_budgets
                .record(id, retained_descendant, cost, parameters)?;
        }
        if !retained_descendant {
            return Ok(());
        }
        self.productive[id] = self.productive[id].saturating_add(1);
        self.since_retained[id] = 0;
        self.budget_draws[id] = 0;
        self.selector_accounting.productive_selections = self
            .selector_accounting
            .productive_selections
            .saturating_add(1);
        Ok(())
    }

    /// The per-campaign selector accounting for the report.
    pub(crate) fn selector_report(&self) -> SmbSelectorAccounting {
        self.selector_accounting
    }

    /// Extract the entry reports, stamping per-entry selection counters.
    pub(crate) fn take_entry_reports(&mut self) -> Vec<SmbArchiveEntryReport> {
        let corrected = true;
        std::mem::take(&mut self.entries)
            .into_iter()
            .enumerate()
            .map(|(id, entry)| {
                let mut report = entry.report;
                if corrected {
                    report.selector = Some(SmbEntrySelectorCounters {
                        selected: self.selected[id],
                        productive: self.productive[id],
                    });
                }
                report
            })
            .collect()
    }
}

/// Run deterministic snapshot-backed short-horizon suffix search.
/// Audit whether active frontier and approach-band representatives can recover.
pub fn audit_smb_frontier_viability(
    rom: &[u8],
    source: &SmbArchiveReport,
) -> Result<SmbFrontierViabilityReport, Box<dyn Error>> {
    let active = active_source_entries(source);
    let mut selected = active
        .into_iter()
        .filter(|entry| {
            entry.key.world == 0 && entry.key.level == 2 && (32..=39).contains(&entry.key.progress)
        })
        .collect::<Vec<_>>();
    selected.sort_by_key(|entry| (entry.input.clone(), entry.id));
    let continuation_masks = std::iter::once(None)
        .chain(FROZEN_BUTTON_MASKS.into_iter().map(Some))
        .collect::<Vec<_>>();
    let mut target = SmbTarget::from_smb_rom_bytes_headless(rom)?;
    let genesis = target
        .snapshot()
        .ok_or("failed to snapshot audit genesis")?;
    let mut prior_input = SmbInput::default();
    let mut prior_snapshots = vec![genesis];
    let mut entries = Vec::with_capacity(selected.len());
    for entry in selected {
        let common = prior_input
            .actions
            .iter()
            .zip(&entry.input.actions)
            .take_while(|(left, right)| left == right)
            .count();
        target.restore(&prior_snapshots[common])?;
        prior_snapshots.truncate(common + 1);
        for action in &entry.input.actions[common..] {
            target.apply(action);
            let snapshot = target
                .snapshot()
                .ok_or("failed to snapshot audit replay prefix")?;
            prior_snapshots.push(snapshot);
            if target.is_dead() || target.exit_kind() != ExitKind::Ok {
                break;
            }
        }
        let endpoint = target
            .snapshot()
            .ok_or("failed to snapshot audit endpoint")?;
        let mut continuations = Vec::with_capacity(continuation_masks.len());
        for mask in &continuation_masks {
            target.restore(&endpoint)?;
            target.apply(&ButtonChord::new(mask.unwrap_or(0), 120));
            let state = smb_mechanical_state_from_wram(target.wram());
            let reached_kill_state = state.player_engine_state == 0x0b
                || target
                    .last_action_observations()
                    .iter()
                    .any(|observation| observation.decoded.player_engine_state == 0x0b);
            continuations.push(if reached_kill_state {
                SmbViabilityClass::KillState
            } else if state.player_y_bucket == 15 {
                SmbViabilityClass::BelowPlayable
            } else {
                SmbViabilityClass::Controllable
            });
        }
        let doomed = continuations
            .iter()
            .all(|class| *class != SmbViabilityClass::Controllable);
        entries.push(SmbViabilityEntry {
            id: entry.id,
            frontier: entry.key.progress == 39,
            key: entry.key,
            continuations,
            doomed,
        });
        prior_input = entry.input.clone();
    }
    let counts = |frontier: bool| {
        let matching = entries.iter().filter(|entry| entry.frontier == frontier);
        SmbViabilityCounts {
            total: u64::try_from(matching.clone().count()).unwrap_or(u64::MAX),
            doomed: u64::try_from(matching.filter(|entry| entry.doomed).count())
                .unwrap_or(u64::MAX),
        }
    };
    let frontier = counts(true);
    let nonfrontier = counts(false);
    Ok(SmbFrontierViabilityReport {
        continuation_frames: 120,
        continuation_masks,
        frontier,
        approach_band: SmbViabilityCounts {
            total: frontier.total.saturating_add(nonfrontier.total),
            doomed: frontier.doomed.saturating_add(nonfrontier.doomed),
        },
        entries,
    })
}

/// One audited representative in the screen-relative player-column decode audit.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbPlayerColumnAuditedEntry {
    /// Stable source archive identifier.
    pub id: u64,
    /// Recorded progress bucket of the audited slice.
    pub progress: u16,
    /// Whether the entry belongs to the maximal frontier slice.
    pub frontier: bool,
    /// Camera position in pixels at the audited endpoint.
    pub endpoint_camera: u32,
    /// Recorded frame count per continuation, including the endpoint.
    pub recorded_frames: Vec<u16>,
}

/// Film-check evidence for one candidate work-RAM index.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbPlayerColumnFilmEvidence {
    /// Work-RAM index under test.
    pub index: u16,
    /// Smallest offset with at least the required agreeing comparisons.
    pub offset: i16,
    /// Comparisons agreeing with that offset inside the fixed tolerance.
    pub agreeing_comparisons: u64,
    /// Comparisons available for this index.
    pub comparisons: u64,
    /// Largest recorded camera difference among the agreeing comparisons.
    #[serde(default)]
    pub camera_spread: u32,
    /// Equal-camera comparisons whose candidate values differ by at least the film gap.
    #[serde(default)]
    pub separating_comparisons: u64,
    /// Of those, the count in which the held-left continuation holds the smaller value.
    #[serde(default)]
    pub left_is_smaller: u64,
    /// Recorded direction: "right_increasing", "left_increasing" or "inconsistent".
    #[serde(default)]
    pub polarity: String,
}

/// Deterministic screen-relative player-column decode report.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbPlayerColumnReport {
    /// Frames requested by every continuation.
    pub continuation_frames: u8,
    /// Fixed continuation masks in execution order.
    pub continuation_masks: Vec<u8>,
    /// Audited representatives in selection order.
    pub audited: Vec<SmbPlayerColumnAuditedEntry>,
    /// Ordered active entries examined per slice before auditing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scanned_per_slice: Vec<u64>,
    /// Examined entries the controller steered, per slice.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steerable_per_slice: Vec<u64>,
    /// Indices taking at least the required number of distinct values.
    pub distinct_value_survivors: u64,
    /// Indices additionally changing by at most the frame-step bound.
    pub smooth_survivors: u64,
    /// Indices additionally decreasing under the left continuation.
    pub left_direction_survivors: u64,
    /// Indices additionally not decreasing under the right continuation.
    pub right_direction_survivors: u64,
    /// Right continuations whose camera advance qualifies for the relative test.
    pub qualifying_right_continuations: u64,
    /// Indices surviving every mechanical filter, in ascending order.
    pub camera_relative_survivors: Vec<u16>,
    /// Surviving indices that additionally pass the film check.
    pub film_survivors: Vec<SmbPlayerColumnFilmEvidence>,
    /// Film survivors discarded as members of a four-byte-stride group.
    pub stride_rejected: Vec<u16>,
    /// Selected index, if the audit is conclusive.
    pub selected: Option<SmbPlayerColumnFilmEvidence>,
}

/// Control-authority counts for one progress bucket.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbControlCensusBucket {
    /// Corrected progress bucket.
    pub progress: u16,
    /// Active representatives in the bucket.
    pub active: u64,
    /// Representatives whose right continuation advanced the camera.
    pub admitted: u64,
}

/// Deterministic control-authority census over one archive level.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbControlCensusReport {
    /// Frames applied by the single right continuation.
    pub continuation_frames: u8,
    /// Camera pixels an admitted continuation must advance.
    pub camera_advance: u32,
    /// Per-bucket counts in ascending progress order.
    pub buckets: Vec<SmbControlCensusBucket>,
    /// Active representatives examined.
    pub active: u64,
    /// Representatives admitted anywhere.
    pub admitted: u64,
    /// Admitted entry identifiers in descending progress then `(input, id)` order.
    pub admitted_ids: Vec<u64>,
}

/// One rendered audit frame retained for direct visual inspection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SmbAuditFrame {
    /// Stable file name for the rendered frame.
    pub name: String,
    /// Raw RGBA pixels in rendering order.
    pub rgba: Vec<u8>,
}

const PLAYER_COLUMN_MASKS: [u8; 3] = [0x00, 0x01, 0x02];
const PLAYER_COLUMN_FRAMES: u8 = 120;
const PLAYER_COLUMN_SLICES: [u16; 2] = [39, 32];
const PLAYER_COLUMN_SLICE_SIZE: usize = 8;
const PLAYER_COLUMN_MIN_DISTINCT: usize = 8;
const PLAYER_COLUMN_MAX_STEP: i32 = 8;
const PLAYER_COLUMN_LEFT_DECREASE: i32 = 8;
const PLAYER_COLUMN_LEFT_SLACK: i32 = 4;
const PLAYER_COLUMN_LEFT_ENTRIES: usize = 12;
const PLAYER_COLUMN_LEFT_ENTRIES_BASE: usize = 16;
const PLAYER_COLUMN_RIGHT_SLACK: i32 = 16;
const PLAYER_COLUMN_CAMERA_ADVANCE: u32 = 32;
const PLAYER_COLUMN_FILM_GAP: i32 = 8;
const PLAYER_COLUMN_FILM_OFFSETS: i32 = 24;
const PLAYER_COLUMN_FILM_TOLERANCE: i32 = 6;
const PLAYER_COLUMN_FILM_MIN_AGREE: usize = 8;
const PLAYER_COLUMN_FILM_MIN_WIDTH: i32 = 4;
const PLAYER_COLUMN_FILM_MAX_WIDTH: i32 = 40;
const PLAYER_COLUMN_STRIDES: [u16; 3] = [4, 8, 12];
const PLAYER_COLUMN_SCAN_CAP: usize = 64;
const PLAYER_COLUMN_ADVANCING_SCAN_CAP: usize = 128;
const PLAYER_COLUMN_RENDERED_COMPARISONS: usize = 4;
const PLAYER_COLUMN_CAMERA_SPREAD: u32 = 16;
const PLAYER_COLUMN_BUCKET_CAP: usize = 2;
const PLAYER_COLUMN_BUCKET_SCAN_CAP: usize = 4;
/// D48 representative: the lowest index of the family D47's film rule verified.
const DERIVED_COLUMN_INDEX: u16 = 516;
const PLAYER_COLUMN_RESPONSIVE_BUCKET_SCAN: usize = 8;
const PLAYER_COLUMN_RESPONSIVE_SCAN_CAP: usize = 256;
const PLAYER_COLUMN_RESPONSIVE_FRAMES: usize = 60;
const PLAYER_COLUMN_SPAN_MIN: i32 = 24;
const PLAYER_COLUMN_SPAN_MAX: i32 = 128;

/// One ordered audit candidate: an active entry and its slice progress bucket.
type PlayerColumnCandidate<'a> = (&'a SmbArchiveEntryReport, u16);

/// Which active entries a screen-column audit records.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SmbPlayerColumnSelection {
    /// D29: the first eight ordered active entries of each slice.
    FirstOrdered,
    /// D30: the first eight ordered active entries the controller steers.
    FirstSteerable,
    /// D31: the first eight ordered active entries whose right continuation
    /// advances the recorded camera.
    FirstCameraAdvancing,
}

/// Which registered filter and truncation rules an audit applies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PlayerColumnRules {
    truncate_on_camera_decrease: bool,
    require_right_direction: bool,
    require_camera_relative: bool,
    require_camera_spread: bool,
    left_versus_right: bool,
    separation_frame: bool,
    skip_direction_filter: bool,
    complement_index: Option<u16>,
    require_right_polarity: bool,
}

impl PlayerColumnRules {
    /// D29 through D32: no camera-epoch truncation, with filters C3 and C4.
    const LEGACY: Self = Self {
        complement_index: None,
        require_right_polarity: false,
        skip_direction_filter: false,
        separation_frame: false,
        truncate_on_camera_decrease: false,
        require_right_direction: true,
        require_camera_relative: true,
        require_camera_spread: false,
        left_versus_right: false,
    };

    /// D33: one camera epoch per continuation, C3 and C4 replaced by camera spread.
    const SPREAD: Self = Self {
        complement_index: None,
        require_right_polarity: false,
        skip_direction_filter: false,
        separation_frame: false,
        truncate_on_camera_decrease: true,
        require_right_direction: false,
        require_camera_relative: false,
        require_camera_spread: true,
        left_versus_right: false,
    };

    /// D38: the direction filter contrasts the two opposite masks at the same frame.
    const CONTRAST: Self = Self {
        complement_index: None,
        require_right_polarity: false,
        skip_direction_filter: false,
        separation_frame: false,
        truncate_on_camera_decrease: true,
        require_right_direction: false,
        require_camera_relative: false,
        require_camera_spread: true,
        left_versus_right: true,
    };

    /// D47: no direction pre-filter; the film rule alone selects and polarity is recorded.
    const VERIFIED: Self = Self {
        complement_index: None,
        require_right_polarity: false,
        skip_direction_filter: true,
        separation_frame: false,
        truncate_on_camera_decrease: true,
        require_right_direction: false,
        require_camera_relative: false,
        require_camera_spread: true,
        left_versus_right: false,
    };

    /// D48: one complemented byte evaluated alone, with rightward polarity required.
    const DERIVED: Self = Self {
        complement_index: Some(DERIVED_COLUMN_INDEX),
        require_right_polarity: true,
        skip_direction_filter: true,
        separation_frame: false,
        truncate_on_camera_decrease: true,
        require_right_direction: false,
        require_camera_relative: false,
        require_camera_spread: true,
        left_versus_right: false,
    };

    /// D42: the direction filter contrasts at each entry's maximum-separation frame.
    const SEPARATION: Self = Self {
        complement_index: None,
        require_right_polarity: false,
        skip_direction_filter: false,
        truncate_on_camera_decrease: true,
        require_right_direction: false,
        require_camera_relative: false,
        require_camera_spread: true,
        left_versus_right: true,
        separation_frame: true,
    };
}

struct ContinuationRecording {
    wram: Vec<[u8; 2_048]>,
    columns: Vec<[u64; 256]>,
    camera: Vec<u32>,
}

struct EntryRecording {
    id: u64,
    progress: u16,
    frontier: bool,
    continuations: Vec<ContinuationRecording>,
}

struct FilmComparison {
    entry: usize,
    left: usize,
    right: usize,
    frame: usize,
    lowest: i32,
    highest: i32,
    camera: u32,
}

/// Identify the work-RAM byte holding the player's horizontal column on screen.
///
/// The audit runs no search and consults no model. It returns its deterministic
/// report together with the rendered frames that support the visual half.
///
/// # Errors
///
/// Returns an error when the source lacks the registered audit slices or when
/// emulation, snapshotting, or rendering fails.
pub fn audit_smb_player_screen_column(
    rom: &[u8],
    source: &SmbArchiveReport,
) -> Result<(SmbPlayerColumnReport, Vec<SmbAuditFrame>), Box<dyn Error>> {
    audit_smb_player_column_with_selection(rom, source, SmbPlayerColumnSelection::FirstOrdered)
}

/// Identify the horizontal-column byte using an explicit audited-entry selection.
///
/// `FirstOrdered` reproduces the frozen D29 audit byte for byte. `FirstSteerable`
/// records D30's control-authority test and its per-slice scan counts.
///
/// # Errors
///
/// Returns an error when the source lacks the registered audit slices or when
/// emulation, snapshotting, or rendering fails.
pub fn audit_smb_player_column_with_selection(
    rom: &[u8],
    source: &SmbArchiveReport,
    selection_mode: SmbPlayerColumnSelection,
) -> Result<(SmbPlayerColumnReport, Vec<SmbAuditFrame>), Box<dyn Error>> {
    let candidates = player_column_candidates(source, selection_mode)?;
    let mut target = SmbTarget::from_smb_rom_bytes(rom)?;
    let mut prefix = PlayerColumnPrefix::new(&mut target)?;
    let mut selected = Vec::new();
    let mut recordings = Vec::new();
    let mut scanned_per_slice = Vec::new();
    let mut steerable_per_slice = Vec::new();
    for slice in &candidates {
        let mut scanned = 0_u64;
        let mut steerable = 0_u64;
        let mut audited = 0_usize;
        for (entry, progress) in slice {
            if audited >= PLAYER_COLUMN_SLICE_SIZE {
                break;
            }
            scanned = scanned.saturating_add(1);
            let recording = record_player_column_entry(
                &mut target,
                &mut prefix,
                entry,
                *progress,
                PlayerColumnRules::LEGACY,
            )?;
            let keep = match selection_mode {
                SmbPlayerColumnSelection::FirstOrdered => true,
                SmbPlayerColumnSelection::FirstSteerable => {
                    let steered = player_column_is_steerable(&recording);
                    if steered {
                        steerable = steerable.saturating_add(1);
                    }
                    steered
                }
                SmbPlayerColumnSelection::FirstCameraAdvancing => {
                    let advanced = player_column_advances_camera(&recording);
                    if advanced {
                        steerable = steerable.saturating_add(1);
                    }
                    advanced
                }
            };
            if keep {
                selected.push((*entry, *progress));
                recordings.push(recording);
                audited = audited.saturating_add(1);
            }
        }
        if selection_mode != SmbPlayerColumnSelection::FirstOrdered {
            scanned_per_slice.push(scanned);
            steerable_per_slice.push(steerable);
        }
    }
    let (mut report, comparisons) = analyze_player_column(&recordings);
    report.scanned_per_slice = scanned_per_slice;
    report.steerable_per_slice = steerable_per_slice;
    let report = report;
    let requests = player_column_frame_requests(&recordings, &comparisons, &report);
    let frames = render_player_column_frames(&mut target, &selected, &requests)?;
    Ok((report, frames))
}

fn player_column_frame_requests(
    recordings: &[EntryRecording],
    comparisons: &[FilmComparison],
    report: &SmbPlayerColumnReport,
) -> BTreeSet<(usize, usize, usize)> {
    let mut requests = BTreeSet::new();
    for entry in first_audited_entry_per_slice(recordings) {
        let Some(recording) = recordings.get(entry) else {
            continue;
        };
        for (continuation, recorded) in recording.continuations.iter().enumerate() {
            let last = recorded.wram.len().saturating_sub(1);
            for frame in [0, last / 2, last] {
                requests.insert((entry, continuation, frame));
            }
        }
    }
    if let Some(selection) = &report.selected {
        for comparison in comparisons
            .iter()
            .filter(|comparison| {
                film_offset(recordings, comparison, selection.index).is_some_and(
                    |(offset, width)| {
                        (offset - i32::from(selection.offset)).abs() <= PLAYER_COLUMN_FILM_TOLERANCE
                            && (PLAYER_COLUMN_FILM_MIN_WIDTH..=PLAYER_COLUMN_FILM_MAX_WIDTH)
                                .contains(&width)
                    },
                )
            })
            .take(PLAYER_COLUMN_RENDERED_COMPARISONS)
        {
            requests.insert((comparison.entry, comparison.left, comparison.frame));
            requests.insert((comparison.entry, comparison.right, comparison.frame));
        }
    }
    requests
}

/// Audit the horizontal-column byte over an explicit list of source entries.
///
/// D32 uses this with the highest-progress entries its control-authority census
/// admitted. Recording, filters, film check, selection, and rendering are the
/// audit's own.
///
/// # Errors
///
/// Returns an error when an identifier is absent from the source or when
/// emulation, snapshotting, or rendering fails.
pub fn audit_smb_player_column_from_ids(
    rom: &[u8],
    source: &SmbArchiveReport,
    ids: &[u64],
) -> Result<(SmbPlayerColumnReport, Vec<SmbAuditFrame>), Box<dyn Error>> {
    audit_player_column_from_ids(rom, source, ids, PlayerColumnRules::LEGACY)
}

/// Audit the horizontal-column byte under D33's camera-epoch and camera-spread rules.
///
/// # Errors
///
/// Returns an error when an identifier is absent from the source or when
/// emulation, snapshotting, or rendering fails.
pub fn audit_smb_player_column_spread(
    rom: &[u8],
    source: &SmbArchiveReport,
    ids: &[u64],
) -> Result<(SmbPlayerColumnReport, Vec<SmbAuditFrame>), Box<dyn Error>> {
    audit_player_column_from_ids(rom, source, ids, PlayerColumnRules::SPREAD)
}

/// Audit the horizontal-column byte under D38's opposite-mask direction filter.
///
/// # Errors
///
/// Returns an error when an identifier is absent from the source or when
/// emulation, snapshotting, or rendering fails.
pub fn audit_smb_player_column_contrast(
    rom: &[u8],
    source: &SmbArchiveReport,
    ids: &[u64],
) -> Result<(SmbPlayerColumnReport, Vec<SmbAuditFrame>), Box<dyn Error>> {
    audit_player_column_from_ids(rom, source, ids, PlayerColumnRules::CONTRAST)
}

/// Audit the horizontal-column byte under D42's maximum-separation direction filter.
///
/// # Errors
///
/// Returns an error when an identifier is absent from the source or when
/// emulation, snapshotting, or rendering fails.
pub fn audit_smb_player_column_separation(
    rom: &[u8],
    source: &SmbArchiveReport,
    ids: &[u64],
) -> Result<(SmbPlayerColumnReport, Vec<SmbAuditFrame>), Box<dyn Error>> {
    audit_player_column_from_ids(rom, source, ids, PlayerColumnRules::SEPARATION)
}

/// Audit the horizontal-column byte with the film rule alone deciding.
///
/// # Errors
///
/// Returns an error when an identifier is absent from the source or when
/// emulation, snapshotting, or rendering fails.
pub fn audit_smb_player_column_verified(
    rom: &[u8],
    source: &SmbArchiveReport,
    ids: &[u64],
) -> Result<(SmbPlayerColumnReport, Vec<SmbAuditFrame>), Box<dyn Error>> {
    audit_player_column_from_ids(rom, source, ids, PlayerColumnRules::VERIFIED)
}

/// Audit the single complemented byte D48 registered, with rightward polarity required.
///
/// # Errors
///
/// Returns an error when an identifier is absent from the source or when
/// emulation, snapshotting, or rendering fails.
pub fn audit_smb_player_column_derived(
    rom: &[u8],
    source: &SmbArchiveReport,
    ids: &[u64],
) -> Result<(SmbPlayerColumnReport, Vec<SmbAuditFrame>), Box<dyn Error>> {
    audit_player_column_from_ids(rom, source, ids, PlayerColumnRules::DERIVED)
}

fn audit_player_column_from_ids(
    rom: &[u8],
    source: &SmbArchiveReport,
    ids: &[u64],
    rules: PlayerColumnRules,
) -> Result<(SmbPlayerColumnReport, Vec<SmbAuditFrame>), Box<dyn Error>> {
    let active = active_source_entries(source);
    let mut selected = Vec::with_capacity(ids.len());
    for id in ids {
        let entry = active
            .iter()
            .find(|entry| entry.id == *id)
            .ok_or("audit identifier is not an active source entry")?;
        selected.push((*entry, entry.key.progress));
    }
    let mut target = SmbTarget::from_smb_rom_bytes(rom)?;
    let mut prefix = PlayerColumnPrefix::new(&mut target)?;
    let mut recordings = Vec::with_capacity(selected.len());
    for (entry, progress) in &selected {
        recordings.push(record_player_column_entry(
            &mut target,
            &mut prefix,
            entry,
            *progress,
            rules,
        )?);
    }
    if let Some(index) = rules.complement_index {
        complement_recorded_index(&mut recordings, index);
    }
    let (report, comparisons) = analyze_player_column_with_rules(&recordings, rules);
    let requests = player_column_frame_requests(&recordings, &comparisons, &report);
    let frames = render_player_column_frames(&mut target, &selected, &requests)?;
    Ok((report, frames))
}

/// Per-frame rendered-difference and work-RAM record for one audited entry.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbFilmColumnTrace {
    /// Stable source archive identifier.
    pub id: u64,
    /// Recorded progress bucket.
    pub progress: u16,
    /// Recorded camera per frame of the no-input continuation.
    pub camera: Vec<u32>,
    /// Lowest differing rendered column per frame, or -1 when none differs.
    pub lowest: Vec<i32>,
    /// Highest differing rendered column per frame, or -1 when none differs.
    pub highest: Vec<i32>,
    /// Complete work RAM per frame of the left continuation.
    pub left_wram: Vec<Vec<u8>>,
    /// Complete work RAM per frame of the no-input continuation.
    pub still_wram: Vec<Vec<u8>>,
}

/// Record the rendered difference between the no-input and left continuations.
///
/// # Errors
///
/// Returns an error when an identifier is absent from the source or when
/// emulation or snapshotting fails.
pub fn diagnose_smb_film_columns(
    rom: &[u8],
    source: &SmbArchiveReport,
    ids: &[u64],
) -> Result<Vec<SmbFilmColumnTrace>, Box<dyn Error>> {
    let active = active_source_entries(source);
    let mut target = SmbTarget::from_smb_rom_bytes(rom)?;
    let mut prefix = PlayerColumnPrefix::new(&mut target)?;
    let mut traces = Vec::with_capacity(ids.len());
    for id in ids {
        let entry = active
            .iter()
            .find(|entry| entry.id == *id)
            .ok_or("audit identifier is not an active source entry")?;
        let recording = record_player_column_entry(
            &mut target,
            &mut prefix,
            entry,
            entry.key.progress,
            PlayerColumnRules::SPREAD,
        )?;
        let still = &recording.continuations[0];
        let left = &recording.continuations[2];
        let frames = still.wram.len().min(left.wram.len());
        let mut lowest = Vec::with_capacity(frames);
        let mut highest = Vec::with_capacity(frames);
        for frame in 0..frames {
            let differing = (0..256)
                .filter(|column| still.columns[frame][*column] != left.columns[frame][*column])
                .collect::<Vec<_>>();
            lowest.push(
                differing
                    .first()
                    .map_or(-1, |column| i32::try_from(*column).unwrap_or(i32::MAX)),
            );
            highest.push(
                differing
                    .last()
                    .map_or(-1, |column| i32::try_from(*column).unwrap_or(i32::MAX)),
            );
        }
        traces.push(SmbFilmColumnTrace {
            id: *id,
            progress: entry.key.progress,
            camera: still.camera[..frames].to_vec(),
            lowest,
            highest,
            left_wram: left.wram[..frames]
                .iter()
                .map(|wram| wram.to_vec())
                .collect(),
            still_wram: still.wram[..frames]
                .iter()
                .map(|wram| wram.to_vec())
                .collect(),
        });
    }
    Ok(traces)
}

/// One film-check measurement recorded for diagnosis.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbFilmMeasurement {
    /// Work-RAM index under test.
    pub index: u16,
    /// Audited entry position.
    pub entry: u16,
    /// Recorded camera at the compared frame.
    pub camera: u32,
    /// Recorded frame index.
    pub frame: u16,
    /// Absolute difference of the two candidate values.
    pub difference: i32,
    /// Lowest differing rendered column minus the smaller candidate value.
    pub offset: i32,
    /// Differing span minus the candidate difference.
    pub width: i32,
}

/// One recorded progress bucket of the steered-entry scan.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbSteerScanBucket {
    /// Recorded progress bucket.
    pub progress: u16,
    /// Entries of this bucket whose continuations were run.
    pub scanned: u64,
    /// Entries whose held-right continuation advances the recorded camera.
    pub camera_advancing: u64,
    /// Entries whose opposite-mask continuations differ in a rendered column.
    pub answering: u64,
    /// Entries of this bucket admitted to the audited set.
    pub admitted: u64,
}

/// Report for the steered-entry scan that sources the corrected column audit.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbSteerScanReport {
    /// Fixed continuation length.
    pub continuation_frames: u8,
    /// Fixed camera advance recorded alongside each entry, in pixels.
    pub camera_advance: u32,
    /// Entries whose continuations were run.
    pub scanned: u64,
    /// Entries whose held-right continuation advances the recorded camera.
    pub camera_advancing: u64,
    /// Entries whose opposite-mask continuations differ in a rendered column.
    pub answering: u64,
    /// Entries admitted to the audited set.
    pub steered: u64,
    /// Identifiers admitted, in scan order.
    pub steered_ids: Vec<u64>,
    /// Per-bucket scan counts, in progress order.
    pub buckets: Vec<SmbSteerScanBucket>,
}

/// Select audited entries whose rendered frames answer the controller.
///
/// An entry is admitted when its held-right and held-left continuations differ
/// in at least one rendered column on at least one frame in common. That is the
/// discriminator a camera advance cannot supply: a falling player coasts the
/// camera forward while rendering identically under every mask. The scan
/// examines a bounded number of entries per bucket so that one unresponsive
/// bucket cannot consume the whole budget, and records both clauses separately.
///
/// # Errors
///
/// Returns an error when emulation or snapshotting fails.
pub fn select_smb_steered_audit_ids(
    rom: &[u8],
    source: &SmbArchiveReport,
    wanted: usize,
) -> Result<SmbSteerScanReport, Box<dyn Error>> {
    let active = active_source_entries(source);
    let max_tuple = active
        .iter()
        .map(|entry| (entry.key.world, entry.key.level))
        .max()
        .ok_or("source archive has no active entries")?;
    let mut entries = active
        .iter()
        .copied()
        .filter(|entry| (entry.key.world, entry.key.level) == max_tuple)
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| (Reverse(entry.key.progress), entry.input.clone(), entry.id));
    let mut target = SmbTarget::from_smb_rom_bytes(rom)?;
    let mut prefix = PlayerColumnPrefix::new(&mut target)?;
    let mut buckets = BTreeMap::<u16, SmbSteerScanBucket>::new();
    let mut steered_ids = Vec::with_capacity(wanted);
    let mut scanned = 0_usize;
    let bucket_scan_cap = u64::try_from(PLAYER_COLUMN_BUCKET_SCAN_CAP).unwrap_or(u64::MAX);
    let bucket_admit_cap = u64::try_from(PLAYER_COLUMN_BUCKET_CAP).unwrap_or(u64::MAX);
    for entry in entries {
        if steered_ids.len() >= wanted || scanned >= PLAYER_COLUMN_ADVANCING_SCAN_CAP {
            break;
        }
        let bucket = buckets
            .entry(entry.key.progress)
            .or_insert_with(|| SmbSteerScanBucket {
                progress: entry.key.progress,
                ..SmbSteerScanBucket::default()
            });
        if bucket.scanned >= bucket_scan_cap || bucket.admitted >= bucket_admit_cap {
            continue;
        }
        scanned = scanned.saturating_add(1);
        bucket.scanned = bucket.scanned.saturating_add(1);
        let recording = record_player_column_entry(
            &mut target,
            &mut prefix,
            entry,
            entry.key.progress,
            PlayerColumnRules::SPREAD,
        )?;
        let advancing = player_column_advances_camera(&recording);
        let answering = player_column_answers_controller(&recording);
        let bucket = buckets
            .get_mut(&entry.key.progress)
            .ok_or("scan bucket vanished between lookups")?;
        if advancing {
            bucket.camera_advancing = bucket.camera_advancing.saturating_add(1);
        }
        if answering {
            bucket.answering = bucket.answering.saturating_add(1);
            bucket.admitted = bucket.admitted.saturating_add(1);
            steered_ids.push(entry.id);
        }
    }
    Ok(SmbSteerScanReport {
        continuation_frames: PLAYER_COLUMN_FRAMES,
        camera_advance: PLAYER_COLUMN_CAMERA_ADVANCE,
        scanned: u64::try_from(scanned).unwrap_or(u64::MAX),
        camera_advancing: buckets.values().map(|bucket| bucket.camera_advancing).sum(),
        answering: buckets.values().map(|bucket| bucket.answering).sum(),
        steered: u64::try_from(steered_ids.len()).unwrap_or(u64::MAX),
        steered_ids,
        buckets: buckets.into_values().collect(),
    })
}

const VIABLE_PROGRESS_BUCKET_SCAN: usize = 8;

/// One action boundary of a walked input, with what the admission probe does there.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbSpanBoundary {
    /// Actions consumed from the walked input.
    pub action_index: usize,
    /// Decoded world, level and progress at the boundary.
    pub world: u8,
    /// Decoded level.
    pub level: u8,
    /// Decoded progress bucket.
    pub progress: u16,
    /// Raw engine-state byte.
    pub engine_state: u8,
    /// Raw vertical page byte.
    pub vertical_page: u8,
    /// Raw low vertical byte.
    pub vertical_low: u8,
    /// Change in combined vertical position across the action that produced this boundary.
    pub vertical_trend: i32,
    /// Recorded camera in pixels.
    pub camera: u32,
    /// Frames the no-input probe survived, and what stopped it.
    pub still_frames: u16,
    /// Which clause ended the no-input probe: "kill_state", "below_play_area" or "survived".
    pub still_outcome: String,
    /// Frames the held-right probe survived.
    pub right_frames: u16,
    /// Which clause ended the held-right probe.
    pub right_outcome: String,
    /// Frames the button-plus-right probe survived.
    pub jump_frames: u16,
    /// Which clause ended the button-plus-right probe.
    pub jump_outcome: String,
    /// Whether the admission probe would retain this boundary.
    pub probe_admits: bool,
}

/// Walk one recorded input and characterise the admission probe across a progress span.
///
/// This is a measurement over recorded artifacts. It runs no search, changes no
/// search behaviour, involves no model, and retains nothing.
///
/// # Errors
///
/// Returns an error when the source has no entry at the requested endpoint or
/// when emulation or snapshotting fails.
pub fn diagnose_smb_span(
    rom: &[u8],
    source: &SmbArchiveReport,
    endpoint_progress: u16,
    low: u16,
    high: u16,
) -> Result<Vec<SmbSpanBoundary>, Box<dyn Error>> {
    let tuple = source
        .entries
        .iter()
        .map(|entry| (entry.key.world, entry.key.level))
        .max()
        .ok_or("source archive contains no entries")?;
    let walked = source
        .entries
        .iter()
        .filter(|entry| {
            (entry.key.world, entry.key.level) == tuple && entry.key.progress == endpoint_progress
        })
        .min_by_key(|entry| (entry.input.actions.len(), entry.id))
        .ok_or("source archive contains no entry at the requested endpoint")?
        .input
        .clone();
    let mut target = SmbTarget::from_smb_rom_bytes_headless(rom)?;
    target.reset();
    let mut boundaries = Vec::new();
    let mut previous_vertical = 0_i32;
    for (action_index, action) in walked.actions.iter().enumerate() {
        target.apply(action);
        if target.exit_kind() != ExitKind::Ok {
            break;
        }
        let bytes = smb_death_bytes(target.wram());
        let decoded = smb_mechanical_state_from_wram(target.wram());
        let vertical = i32::from(bytes.vertical_page) * 256 + i32::from(bytes.vertical_low);
        let trend = vertical - previous_vertical;
        previous_vertical = vertical;
        if (decoded.world, decoded.level) != tuple
            || decoded.progress < low
            || decoded.progress > high
        {
            continue;
        }
        let resume = target
            .snapshot()
            .ok_or("failed to snapshot a span boundary")?;
        let mut probes = Vec::with_capacity(3);
        for mask in VIABILITY_PROBE_MASKS {
            target.restore(&resume)?;
            probes.push(probe_outcome(&mut target, mask));
        }
        target.restore(&resume)?;
        boundaries.push(SmbSpanBoundary {
            action_index,
            world: decoded.world,
            level: decoded.level,
            progress: decoded.progress,
            engine_state: bytes.engine_state,
            vertical_page: bytes.vertical_page,
            vertical_low: bytes.vertical_low,
            vertical_trend: trend,
            camera: smb_camera_pixels(target.wram()),
            still_frames: probes[0].0,
            still_outcome: probes[0].1.clone(),
            right_frames: probes[1].0,
            right_outcome: probes[1].1.clone(),
            jump_frames: probes[2].0,
            jump_outcome: probes[2].1.clone(),
            probe_admits: probes.iter().any(|probe| probe.1 == "survived"),
        });
    }
    Ok(boundaries)
}

/// Run one probe mask and report how long it lasted and what stopped it.
fn probe_outcome(target: &mut SmbTarget, mask: u8) -> (u16, String) {
    for frame in 0..VIABILITY_PROBE_FRAMES {
        target.apply(&ButtonChord::new(mask, 1));
        if target.exit_kind() != ExitKind::Ok {
            return (frame, "emulation_failed".to_owned());
        }
        let bytes = smb_death_bytes(target.wram());
        if bytes.engine_state == PLAYER_KILLED_STATE {
            return (frame.saturating_add(1), "kill_state".to_owned());
        }
        if bytes.vertical_page >= PLAYER_BELOW_PLAY_AREA_PAGE {
            return (frame.saturating_add(1), "below_play_area".to_owned());
        }
    }
    (VIABILITY_PROBE_FRAMES, "survived".to_owned())
}

/// One examined progress bucket of the viable-progress measurement.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbViableBucket {
    /// Recorded progress bucket.
    pub progress: u16,
    /// Entries of this bucket whose no-input continuation was run.
    pub examined: u64,
    /// Entries whose no-input continuation survived the fixed horizon.
    pub viable: u64,
}

/// Deepest progress bucket holding a state that survives doing nothing.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbViableProgressReport {
    /// Fixed no-input horizon in frames.
    pub horizon: u8,
    /// Fixed number of entries examined per bucket.
    pub per_bucket: u64,
    /// Deepest bucket with at least one viable entry, when one exists.
    pub viable_progress: Option<u16>,
    /// Deepest bucket holding a state whose rendered frames answer the controller.
    #[serde(default)]
    pub play_progress: Option<u16>,
    /// Maximum recorded progress bucket at the deepest tuple, viable or not.
    pub recorded_progress: Option<u16>,
    /// Buckets examined, deepest first, up to and including the first viable one.
    pub buckets: Vec<SmbViableBucket>,
}

/// Measure the deepest progress bucket that holds a state surviving a no-input horizon.
///
/// This is a measurement, not a retention rule: it changes nothing the search
/// does. It exists because the archive admits states the corrected terminal
/// condition stops a few frames later, so the recorded maximum bucket overstates
/// how far live play has reached.
///
/// # Errors
///
/// Returns an error when emulation or snapshotting fails.
pub fn measure_smb_viable_progress(
    rom: &[u8],
    source: &SmbArchiveReport,
) -> Result<SmbViableProgressReport, Box<dyn Error>> {
    let active = active_source_entries(source);
    let Some(max_tuple) = active
        .iter()
        .map(|entry| (entry.key.world, entry.key.level))
        .max()
    else {
        return Ok(SmbViableProgressReport {
            horizon: PLAYER_COLUMN_FRAMES,
            per_bucket: u64::try_from(VIABLE_PROGRESS_BUCKET_SCAN).unwrap_or(u64::MAX),
            viable_progress: None,
            play_progress: None,
            recorded_progress: None,
            buckets: Vec::new(),
        });
    };
    let mut entries = active
        .iter()
        .copied()
        .filter(|entry| (entry.key.world, entry.key.level) == max_tuple)
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| (Reverse(entry.key.progress), entry.input.clone(), entry.id));
    let recorded_progress = entries.first().map(|entry| entry.key.progress);
    let mut target = SmbTarget::from_smb_rom_bytes_headless(rom)?;
    let mut prefix = PlayerColumnPrefix::new(&mut target)?;
    let mut buckets: Vec<SmbViableBucket> = Vec::new();
    let mut viable_progress = None;
    let mut play_progress = None;
    let mut video = SmbTarget::from_smb_rom_bytes(rom)?;
    let mut video_prefix = PlayerColumnPrefix::new(&mut video)?;
    let per_bucket = u64::try_from(VIABLE_PROGRESS_BUCKET_SCAN).unwrap_or(u64::MAX);
    for entry in entries {
        if viable_progress.is_some() && play_progress.is_some() {
            break;
        }
        if buckets
            .last()
            .is_none_or(|bucket| bucket.progress != entry.key.progress)
        {
            buckets.push(SmbViableBucket {
                progress: entry.key.progress,
                examined: 0,
                viable: 0,
            });
        }
        let Some(bucket) = buckets.last_mut() else {
            return Err("viable-progress bucket list is empty".into());
        };
        if bucket.examined >= per_bucket {
            continue;
        }
        bucket.examined = bucket.examined.saturating_add(1);
        let endpoint = replay_player_column_endpoint(&mut target, &mut prefix, entry)?;
        target.restore(&endpoint)?;
        let mut survived = true;
        for _ in 0..PLAYER_COLUMN_FRAMES {
            target.apply(&ButtonChord::new(PLAYER_COLUMN_MASKS[0], 1));
            if target.is_dead() || target.exit_kind() != ExitKind::Ok {
                survived = false;
                break;
            }
        }
        if survived {
            let Some(bucket) = buckets.last_mut() else {
                return Err("viable-progress bucket list is empty".into());
            };
            bucket.viable = bucket.viable.saturating_add(1);
            if viable_progress.is_none() {
                viable_progress = Some(entry.key.progress);
            }
        }
        if play_progress.is_none() {
            // The rendered test D37 established: a scripted sequence survives
            // doing nothing but does not answer the controller.
            let recording = record_player_column_entry(
                &mut video,
                &mut video_prefix,
                entry,
                entry.key.progress,
                PlayerColumnRules::SPREAD,
            )?;
            if player_column_answers_controller(&recording) {
                play_progress = Some(entry.key.progress);
            }
        }
    }
    Ok(SmbViableProgressReport {
        horizon: PLAYER_COLUMN_FRAMES,
        per_bucket,
        viable_progress,
        play_progress,
        recorded_progress,
        buckets,
    })
}

/// One examined entry of the responsiveness scan.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbResponsiveEntry {
    /// Stable source archive identifier.
    pub id: u64,
    /// Recorded progress bucket.
    pub progress: u16,
    /// Frames on which the opposite-mask continuations differ in a rendered column.
    pub responsive_frames: u64,
    /// Frames the two opposite-mask continuations have in common.
    pub common_frames: u64,
    /// Largest differing column span at or below the recorded ceiling.
    pub largest_span: i32,
    /// Equal-camera frames whose differing span exceeded the recorded ceiling.
    pub wide_frames: u64,
    /// Whether the entry was admitted to the audited set.
    pub admitted: bool,
}

/// Report for the responsiveness scan that sources the D38 audit.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbResponsiveScanReport {
    /// Fixed continuation length.
    pub continuation_frames: u8,
    /// Fixed responsive-frame threshold for admission.
    pub responsive_threshold: u64,
    /// Whether admission ranked by largest differing span rather than frame count.
    pub by_span: bool,
    /// Entries whose continuations were run.
    pub scanned: u64,
    /// Entries reaching the responsive-frame threshold.
    pub responsive: u64,
    /// Identifiers admitted, in admission order.
    pub steered_ids: Vec<u64>,
    /// Every examined entry, in descending responsive frames.
    pub entries: Vec<SmbResponsiveEntry>,
}

/// Select audited entries by how many frames answer the controller.
///
/// D37 recorded that depth does not supply horizontal motion in this archive:
/// its deepest buckets are falls in flight and half its admitted entries were
/// pinned against terrain. This scan ranks by a rendered measurement that names
/// no work-RAM index, so it is not circular with what the audit is looking for.
///
/// # Errors
///
/// Returns an error when emulation or snapshotting fails.
pub fn select_smb_responsive_audit_ids(
    rom: &[u8],
    source: &SmbArchiveReport,
    wanted: usize,
) -> Result<SmbResponsiveScanReport, Box<dyn Error>> {
    select_responsive_audit_ids(rom, source, wanted, false)
}

/// Select audited entries by the largest rendered separation the controller produces.
///
/// D38 recorded that counting differing frames ranks facing changes, which
/// repaint one sprite while moving the player nowhere. The width of the
/// differing span separates the two: a facing flip spans about one sprite,
/// while two players genuinely apart span their separation plus a sprite.
///
/// # Errors
///
/// Returns an error when emulation or snapshotting fails.
pub fn select_smb_span_audit_ids(
    rom: &[u8],
    source: &SmbArchiveReport,
    wanted: usize,
) -> Result<SmbResponsiveScanReport, Box<dyn Error>> {
    select_responsive_audit_ids(rom, source, wanted, true)
}

fn select_responsive_audit_ids(
    rom: &[u8],
    source: &SmbArchiveReport,
    wanted: usize,
    by_span: bool,
) -> Result<SmbResponsiveScanReport, Box<dyn Error>> {
    let active = active_source_entries(source);
    let max_tuple = active
        .iter()
        .map(|entry| (entry.key.world, entry.key.level))
        .max()
        .ok_or("source archive has no active entries")?;
    let mut entries = active
        .iter()
        .copied()
        .filter(|entry| (entry.key.world, entry.key.level) == max_tuple)
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| (Reverse(entry.key.progress), entry.input.clone(), entry.id));
    let mut target = SmbTarget::from_smb_rom_bytes(rom)?;
    let mut prefix = PlayerColumnPrefix::new(&mut target)?;
    let mut examined = Vec::new();
    let mut per_bucket = BTreeMap::<u16, usize>::new();
    for entry in entries {
        if examined.len() >= PLAYER_COLUMN_RESPONSIVE_SCAN_CAP {
            break;
        }
        let taken = per_bucket.entry(entry.key.progress).or_insert(0);
        if *taken >= PLAYER_COLUMN_RESPONSIVE_BUCKET_SCAN {
            continue;
        }
        *taken = taken.saturating_add(1);
        let recording = record_player_column_entry(
            &mut target,
            &mut prefix,
            entry,
            entry.key.progress,
            PlayerColumnRules::SPREAD,
        )?;
        let measured = player_column_responsive_frames(&recording);
        examined.push(SmbResponsiveEntry {
            id: entry.id,
            progress: entry.key.progress,
            responsive_frames: measured.responsive,
            common_frames: measured.common,
            largest_span: measured.largest_span,
            wide_frames: measured.wide,
            admitted: false,
        });
    }
    let threshold = u64::try_from(PLAYER_COLUMN_RESPONSIVE_FRAMES).unwrap_or(u64::MAX);
    let mut order = (0..examined.len()).collect::<Vec<_>>();
    order.sort_by_key(|position| {
        let entry = examined[*position];
        let rank = if by_span {
            u64::try_from(entry.largest_span).unwrap_or(0)
        } else {
            entry.responsive_frames
        };
        (Reverse(rank), Reverse(entry.progress), *position)
    });
    let mut admitted_per_bucket = BTreeMap::<u16, usize>::new();
    let mut steered_ids = Vec::with_capacity(wanted);
    for position in order {
        if steered_ids.len() >= wanted {
            break;
        }
        let qualifies = if by_span {
            examined[position].largest_span >= PLAYER_COLUMN_SPAN_MIN
        } else {
            examined[position].responsive_frames >= threshold
        };
        if !qualifies {
            break;
        }
        let taken = admitted_per_bucket
            .entry(examined[position].progress)
            .or_insert(0);
        if *taken >= PLAYER_COLUMN_BUCKET_CAP {
            continue;
        }
        *taken = taken.saturating_add(1);
        examined[position].admitted = true;
        steered_ids.push(examined[position].id);
    }
    let responsive = examined
        .iter()
        .filter(|entry| {
            if by_span {
                entry.largest_span >= PLAYER_COLUMN_SPAN_MIN
            } else {
                entry.responsive_frames >= threshold
            }
        })
        .count();
    if by_span {
        examined.sort_by_key(|entry| (Reverse(entry.largest_span), Reverse(entry.progress)));
    } else {
        examined.sort_by_key(|entry| (Reverse(entry.responsive_frames), Reverse(entry.progress)));
    }
    Ok(SmbResponsiveScanReport {
        continuation_frames: PLAYER_COLUMN_FRAMES,
        responsive_threshold: threshold,
        by_span,
        scanned: u64::try_from(examined.len()).unwrap_or(u64::MAX),
        responsive: u64::try_from(responsive).unwrap_or(u64::MAX),
        steered_ids,
        entries: examined,
    })
}

/// Count differing frames and the largest differing span the controller produces.
fn player_column_responsive_frames(recording: &EntryRecording) -> PlayerColumnResponsiveness {
    let right = &recording.continuations[1];
    let left = &recording.continuations[2];
    let frames = right.columns.len().min(left.columns.len());
    let mut responsive = 0_usize;
    let mut largest_span = 0_i32;
    let mut wide = 0_u64;
    for frame in 0..frames {
        if right.camera[frame] != left.camera[frame] {
            continue;
        }
        let differing = (0..256)
            .filter(|column| right.columns[frame][*column] != left.columns[frame][*column])
            .collect::<Vec<_>>();
        let (Some(lowest), Some(highest)) = (differing.first(), differing.last()) else {
            continue;
        };
        responsive = responsive.saturating_add(1);
        let span =
            i32::try_from(highest.saturating_sub(*lowest).saturating_add(1)).unwrap_or(i32::MAX);
        if span <= PLAYER_COLUMN_SPAN_MAX {
            largest_span = largest_span.max(span);
        } else {
            wide = wide.saturating_add(1);
        }
    }
    PlayerColumnResponsiveness {
        responsive: u64::try_from(responsive).unwrap_or(u64::MAX),
        common: u64::try_from(frames).unwrap_or(u64::MAX),
        largest_span,
        wide,
    }
}

/// Recorded shape of one entry's response to the two opposite masks.
struct PlayerColumnResponsiveness {
    responsive: u64,
    common: u64,
    largest_span: i32,
    wide: u64,
}

/// Report whether the held-right and held-left continuations ever render differently.
fn player_column_answers_controller(recording: &EntryRecording) -> bool {
    let right = &recording.continuations[1].columns;
    let left = &recording.continuations[2].columns;
    let frames = right.len().min(left.len());
    (0..frames).any(|frame| right[frame] != left[frame])
}

/// Record every film-check measurement for the indices that reach verification.
///
/// # Errors
///
/// Returns an error when an identifier is absent from the source or when
/// emulation or snapshotting fails.
pub fn diagnose_smb_film_measurements_derived(
    rom: &[u8],
    source: &SmbArchiveReport,
    ids: &[u64],
) -> Result<Vec<SmbFilmMeasurement>, Box<dyn Error>> {
    diagnose_film_measurements(rom, source, ids, PlayerColumnRules::DERIVED)
}

/// Record every film-check measurement for the indices that reach verification.
///
/// # Errors
///
/// Returns an error when an identifier is absent from the source or when
/// emulation or snapshotting fails.
pub fn diagnose_smb_film_measurements(
    rom: &[u8],
    source: &SmbArchiveReport,
    ids: &[u64],
) -> Result<Vec<SmbFilmMeasurement>, Box<dyn Error>> {
    diagnose_film_measurements(rom, source, ids, PlayerColumnRules::SPREAD)
}

fn diagnose_film_measurements(
    rom: &[u8],
    source: &SmbArchiveReport,
    ids: &[u64],
    rules: PlayerColumnRules,
) -> Result<Vec<SmbFilmMeasurement>, Box<dyn Error>> {
    let active = active_source_entries(source);
    let mut selected = Vec::with_capacity(ids.len());
    for id in ids {
        let entry = active
            .iter()
            .find(|entry| entry.id == *id)
            .ok_or("audit identifier is not an active source entry")?;
        selected.push((*entry, entry.key.progress));
    }
    let mut target = SmbTarget::from_smb_rom_bytes(rom)?;
    let mut prefix = PlayerColumnPrefix::new(&mut target)?;
    let mut recordings = Vec::with_capacity(selected.len());
    for (entry, progress) in &selected {
        recordings.push(record_player_column_entry(
            &mut target,
            &mut prefix,
            entry,
            *progress,
            rules,
        )?);
    }
    if let Some(index) = rules.complement_index {
        complement_recorded_index(&mut recordings, index);
    }
    let (report, comparisons) = analyze_player_column_with_rules(&recordings, rules);
    let mut measurements = Vec::new();
    for index in &report.camera_relative_survivors {
        for comparison in &comparisons {
            let recording = &recordings[comparison.entry];
            let position = usize::from(*index);
            let left = i32::from(
                recording.continuations[comparison.left].wram[comparison.frame][position],
            );
            let right = i32::from(
                recording.continuations[comparison.right].wram[comparison.frame][position],
            );
            let difference = (left - right).abs();
            if difference < PLAYER_COLUMN_FILM_GAP {
                continue;
            }
            measurements.push(SmbFilmMeasurement {
                index: *index,
                entry: u16::try_from(comparison.entry).unwrap_or(u16::MAX),
                camera: comparison.camera,
                frame: u16::try_from(comparison.frame).unwrap_or(u16::MAX),
                difference,
                offset: comparison.lowest - left.min(right),
                width: comparison.highest - comparison.lowest + 1 - difference,
            });
        }
    }
    Ok(measurements)
}

/// Choose census-admitted entries in descending progress with a per-bucket cap.
///
/// The cap makes the audited endpoints span several camera positions, which the
/// camera-spread verification requires.
///
/// # Errors
///
/// Returns an error when an admitted identifier is absent from the source.
pub fn select_smb_spread_audit_ids(
    source: &SmbArchiveReport,
    admitted: &[u64],
    wanted: usize,
) -> Result<Vec<u64>, Box<dyn Error>> {
    let active = active_source_entries(source);
    let mut per_bucket = BTreeMap::<u16, usize>::new();
    let mut chosen = Vec::with_capacity(wanted);
    for id in admitted {
        if chosen.len() >= wanted {
            break;
        }
        let entry = active
            .iter()
            .find(|entry| entry.id == *id)
            .ok_or("admitted identifier is not an active source entry")?;
        let taken = per_bucket.entry(entry.key.progress).or_insert(0);
        if *taken >= PLAYER_COLUMN_BUCKET_CAP {
            continue;
        }
        *taken = taken.saturating_add(1);
        chosen.push(*id);
    }
    Ok(chosen)
}

fn player_column_candidates(
    source: &SmbArchiveReport,
    selection_mode: SmbPlayerColumnSelection,
) -> Result<Vec<Vec<PlayerColumnCandidate<'_>>>, Box<dyn Error>> {
    let active = active_source_entries(source);
    let cap = match selection_mode {
        SmbPlayerColumnSelection::FirstOrdered => PLAYER_COLUMN_SLICE_SIZE,
        SmbPlayerColumnSelection::FirstSteerable => PLAYER_COLUMN_SCAN_CAP,
        SmbPlayerColumnSelection::FirstCameraAdvancing => PLAYER_COLUMN_ADVANCING_SCAN_CAP,
    };
    let mut slices = Vec::with_capacity(PLAYER_COLUMN_SLICES.len());
    for progress in PLAYER_COLUMN_SLICES {
        let mut slice = active
            .iter()
            .filter(|entry| {
                entry.key.world == 0 && entry.key.level == 2 && entry.key.progress == progress
            })
            .copied()
            .collect::<Vec<_>>();
        slice.sort_by_key(|entry| (entry.input.clone(), entry.id));
        if slice.len() < PLAYER_COLUMN_SLICE_SIZE {
            return Err("audit slice has fewer than eight active entries".into());
        }
        slice.truncate(cap);
        slices.push(
            slice
                .into_iter()
                .map(|entry| (entry, progress))
                .collect::<Vec<_>>(),
        );
    }
    Ok(slices)
}

/// Count how many retained representatives per progress bucket the controller
/// can still move rightwards.
///
/// # Errors
///
/// Returns an error when emulation or snapshotting fails.
pub fn census_smb_control_authority(
    rom: &[u8],
    source: &SmbArchiveReport,
) -> Result<SmbControlCensusReport, Box<dyn Error>> {
    let mut entries = active_source_entries(source)
        .into_iter()
        .filter(|entry| entry.key.world == 0 && entry.key.level == 2)
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| (entry.key.progress, entry.input.clone(), entry.id));
    let mut target = SmbTarget::from_smb_rom_bytes_headless(rom)?;
    let mut prefix = PlayerColumnPrefix::new(&mut target)?;
    let mut buckets = BTreeMap::<u16, (u64, u64)>::new();
    let mut admitted_entries = Vec::new();
    for entry in &entries {
        let endpoint = replay_player_column_endpoint(&mut target, &mut prefix, entry)?;
        target.restore(&endpoint)?;
        let first = smb_camera_pixels(target.wram());
        let mut last = first;
        for _ in 0..PLAYER_COLUMN_FRAMES {
            if target.is_dead() || target.exit_kind() != ExitKind::Ok {
                break;
            }
            target.apply(&ButtonChord::new(PLAYER_COLUMN_MASKS[1], 1));
            last = smb_camera_pixels(target.wram());
        }
        let admitted = last.saturating_sub(first) >= PLAYER_COLUMN_CAMERA_ADVANCE;
        let counts = buckets.entry(entry.key.progress).or_insert((0, 0));
        counts.0 = counts.0.saturating_add(1);
        if admitted {
            counts.1 = counts.1.saturating_add(1);
            admitted_entries.push(*entry);
        }
    }
    admitted_entries
        .sort_by_key(|entry| (Reverse(entry.key.progress), entry.input.clone(), entry.id));
    Ok(SmbControlCensusReport {
        continuation_frames: PLAYER_COLUMN_FRAMES,
        camera_advance: PLAYER_COLUMN_CAMERA_ADVANCE,
        buckets: buckets
            .iter()
            .map(|(progress, (active, admitted))| SmbControlCensusBucket {
                progress: *progress,
                active: *active,
                admitted: *admitted,
            })
            .collect(),
        active: u64::try_from(entries.len()).unwrap_or(u64::MAX),
        admitted: u64::try_from(admitted_entries.len()).unwrap_or(u64::MAX),
        admitted_ids: admitted_entries.iter().map(|entry| entry.id).collect(),
    })
}

fn player_column_advances_camera(recording: &EntryRecording) -> bool {
    let camera = &recording.continuations[1].camera;
    camera[camera.len().saturating_sub(1)].saturating_sub(camera[0]) >= PLAYER_COLUMN_CAMERA_ADVANCE
}

fn player_column_is_steerable(recording: &EntryRecording) -> bool {
    let right = &recording.continuations[1].wram;
    let left = &recording.continuations[2].wram;
    right.last() != left.last()
}

fn first_audited_entry_per_slice(recordings: &[EntryRecording]) -> Vec<usize> {
    let mut seen = BTreeSet::new();
    recordings
        .iter()
        .enumerate()
        .filter(|(_, recording)| seen.insert(recording.progress))
        .map(|(entry, _)| entry)
        .collect()
}

/// Reusable genesis-rooted prefix so consecutive ordered candidates share replay work.
struct PlayerColumnPrefix {
    input: SmbInput,
    snapshots: Vec<SmbSnapshot>,
}

impl PlayerColumnPrefix {
    fn new(target: &mut SmbTarget) -> Result<Self, Box<dyn Error>> {
        target.reset();
        let genesis = target
            .snapshot()
            .ok_or("failed to snapshot audit genesis")?;
        Ok(Self {
            input: SmbInput::default(),
            snapshots: vec![genesis],
        })
    }
}

fn replay_player_column_endpoint(
    target: &mut SmbTarget,
    prefix: &mut PlayerColumnPrefix,
    entry: &SmbArchiveEntryReport,
) -> Result<SmbSnapshot, Box<dyn Error>> {
    let common = prefix
        .input
        .actions
        .iter()
        .zip(&entry.input.actions)
        .take_while(|(left, right)| left == right)
        .count();
    target.restore(&prefix.snapshots[common])?;
    prefix.snapshots.truncate(common + 1);
    for action in &entry.input.actions[common..] {
        target.apply(action);
        let snapshot = target
            .snapshot()
            .ok_or("failed to snapshot audit replay prefix")?;
        prefix.snapshots.push(snapshot);
        if target.is_dead() || target.exit_kind() != ExitKind::Ok {
            break;
        }
    }
    prefix.input = entry.input.clone();
    target
        .snapshot()
        .ok_or_else(|| "failed to snapshot audit endpoint".into())
}

fn record_player_column_entry(
    target: &mut SmbTarget,
    prefix: &mut PlayerColumnPrefix,
    entry: &SmbArchiveEntryReport,
    progress: u16,
    rules: PlayerColumnRules,
) -> Result<EntryRecording, Box<dyn Error>> {
    let endpoint = replay_player_column_endpoint(target, prefix, entry)?;
    // The emulator's frame buffer is not part of a restored snapshot, so the endpoint
    // image is captured once here and reused as every continuation's frame zero.
    let endpoint_columns = column_signatures(&target.frame_rgba())?;
    let mut continuations = Vec::with_capacity(PLAYER_COLUMN_MASKS.len());
    for mask in PLAYER_COLUMN_MASKS {
        target.restore(&endpoint)?;
        let mut recording = ContinuationRecording {
            wram: vec![*target.wram()],
            columns: vec![endpoint_columns],
            camera: vec![smb_camera_pixels(target.wram())],
        };
        for _ in 0..PLAYER_COLUMN_FRAMES {
            if target.is_dead() || target.exit_kind() != ExitKind::Ok {
                break;
            }
            target.apply(&ButtonChord::new(mask, 1));
            push_player_column_frame(target, &mut recording)?;
            if rules.truncate_on_camera_decrease {
                let camera = &recording.camera;
                let last = camera.len().saturating_sub(1);
                if last > 0 && camera[last] < camera[last - 1] {
                    break;
                }
            }
        }
        continuations.push(recording);
    }
    Ok(EntryRecording {
        id: entry.id,
        progress,
        frontier: progress == PLAYER_COLUMN_SLICES[0],
        continuations,
    })
}

fn push_player_column_frame(
    target: &mut SmbTarget,
    recording: &mut ContinuationRecording,
) -> Result<(), Box<dyn Error>> {
    recording.wram.push(*target.wram());
    recording.camera.push(smb_camera_pixels(target.wram()));
    recording
        .columns
        .push(column_signatures(&target.frame_rgba())?);
    Ok(())
}

fn column_signatures(rgba: &[u8]) -> Result<[u64; 256], Box<dyn Error>> {
    if rgba.len() != FRAME_WIDTH * FRAME_HEIGHT * 4 {
        return Err("unexpected TetaNES RGBA frame length".into());
    }
    let mut signatures = [0xcbf2_9ce4_8422_2325_u64; 256];
    for row in rgba.chunks_exact(FRAME_WIDTH * 4) {
        for (column, pixel) in row.chunks_exact(4).enumerate() {
            let mut hash = signatures[column];
            for byte in pixel {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
            signatures[column] = hash;
        }
    }
    Ok(signatures)
}

fn analyze_player_column(
    recordings: &[EntryRecording],
) -> (SmbPlayerColumnReport, Vec<FilmComparison>) {
    analyze_player_column_with_rules(recordings, PlayerColumnRules::LEGACY)
}

/// Replace one byte of every recorded frame with its complement.
///
/// A complement maps distinct values to distinct values and preserves every
/// frame-to-frame step size, so filters C0 and C1 decide exactly as they would
/// on the raw byte.
fn complement_recorded_index(recordings: &mut [EntryRecording], index: u16) {
    let position = usize::from(index);
    for recording in recordings.iter_mut() {
        for continuation in &mut recording.continuations {
            for wram in &mut continuation.wram {
                wram[position] = u8::MAX - wram[position];
            }
        }
    }
}

fn analyze_player_column_with_rules(
    recordings: &[EntryRecording],
    rules: PlayerColumnRules,
) -> (SmbPlayerColumnReport, Vec<FilmComparison>) {
    let mut distinct_value_survivors = 0_u64;
    let mut smooth_survivors = 0_u64;
    let mut left_direction_survivors = 0_u64;
    let mut right_direction_survivors = 0_u64;
    let mut camera_relative_survivors = Vec::new();
    let qualifying = qualifying_right_continuations(recordings);
    let indices: Vec<usize> = match rules.complement_index {
        Some(index) => vec![usize::from(index)],
        None => (0..2_048_usize).collect(),
    };
    let separation_frames = if rules.separation_frame {
        recordings
            .iter()
            .map(player_column_max_span_frame)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    for index in indices {
        if !player_column_distinct(recordings, index) {
            continue;
        }
        distinct_value_survivors = distinct_value_survivors.saturating_add(1);
        if !player_column_smooth(recordings, index) {
            continue;
        }
        smooth_survivors = smooth_survivors.saturating_add(1);
        let directed = if rules.skip_direction_filter {
            true
        } else if rules.separation_frame {
            player_column_separation_direction(recordings, &separation_frames, index)
        } else if rules.left_versus_right {
            player_column_left_versus_right(recordings, index)
        } else {
            player_column_left_direction(recordings, index)
        };
        if !directed {
            continue;
        }
        left_direction_survivors = left_direction_survivors.saturating_add(1);
        if rules.require_right_direction && !player_column_right_direction(recordings, index) {
            continue;
        }
        right_direction_survivors = right_direction_survivors.saturating_add(1);
        if rules.require_camera_relative
            && (qualifying.is_empty()
                || !player_column_camera_relative(recordings, index, &qualifying))
        {
            continue;
        }
        camera_relative_survivors.push(u16::try_from(index).unwrap_or(u16::MAX));
    }
    let comparisons = film_comparisons(recordings);
    let film_survivors = camera_relative_survivors
        .iter()
        .filter_map(|index| film_evidence(recordings, &comparisons, *index, rules))
        .collect::<Vec<_>>();
    let stride_rejected = film_survivors
        .iter()
        .map(|evidence| evidence.index)
        .filter(|index| {
            film_survivors.iter().any(|other| {
                PLAYER_COLUMN_STRIDES.iter().any(|stride| {
                    other.index == index.saturating_add(*stride)
                        || other.index.saturating_add(*stride) == *index
                })
            })
        })
        .collect::<Vec<_>>();
    let selected = film_survivors
        .iter()
        .find(|evidence| {
            !stride_rejected.contains(&evidence.index)
                && (!rules.require_right_polarity || evidence.polarity == "right_increasing")
        })
        .cloned();
    let audited = recordings
        .iter()
        .map(|recording| SmbPlayerColumnAuditedEntry {
            id: recording.id,
            progress: recording.progress,
            frontier: recording.frontier,
            endpoint_camera: recording.continuations[0].camera[0],
            recorded_frames: recording
                .continuations
                .iter()
                .map(|continuation| u16::try_from(continuation.wram.len()).unwrap_or(u16::MAX))
                .collect(),
        })
        .collect::<Vec<_>>();
    (
        SmbPlayerColumnReport {
            continuation_frames: PLAYER_COLUMN_FRAMES,
            continuation_masks: PLAYER_COLUMN_MASKS.to_vec(),
            audited,
            scanned_per_slice: Vec::new(),
            steerable_per_slice: Vec::new(),
            distinct_value_survivors,
            smooth_survivors,
            left_direction_survivors,
            right_direction_survivors,
            qualifying_right_continuations: u64::try_from(qualifying.len()).unwrap_or(u64::MAX),
            camera_relative_survivors,
            film_survivors,
            stride_rejected,
            selected,
        },
        comparisons,
    )
}

/// One audited entry's continuation shape, recorded for diagnosis.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbLeftDirectionEntry {
    /// Stable source archive identifier.
    pub id: u64,
    /// Recorded progress bucket.
    pub progress: u16,
    /// Recorded frames per continuation, in mask order.
    pub frames: Vec<u16>,
    /// Recorded camera at the first and last frame of the no-input continuation.
    pub camera: (u32, u32),
    /// Raw recorded bytes per frame of the no-input continuation.
    pub still: Vec<SmbDeathBytes>,
    /// Raw recorded bytes per frame of the held-right continuation.
    pub right: Vec<SmbDeathBytes>,
    /// Frame of largest equal-camera differing span, when one exists.
    pub separation_frame: Option<usize>,
    /// Largest equal-camera differing span and its lowest and highest columns.
    pub separation_span: Option<(i32, i32, i32)>,
}

/// One smooth candidate index's endpoint values across the audited entries.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbLeftDirectionCandidate {
    /// Work-RAM index under test.
    pub index: u16,
    /// Value at each entry's endpoint.
    pub endpoint: Vec<i32>,
    /// Value at the last recorded frame of each held-left continuation.
    pub left_final: Vec<i32>,
    /// Smallest value anywhere in each held-left continuation.
    pub left_min: Vec<i32>,
    /// Value at the last recorded frame of each held-right continuation.
    pub right_final: Vec<i32>,
    /// Held-left value at each entry's maximum-separation frame.
    pub left_at_separation: Vec<i32>,
    /// Held-right value at each entry's maximum-separation frame.
    pub right_at_separation: Vec<i32>,
}

/// Record why the left-direction filter accepted or rejected each smooth index.
///
/// # Errors
///
/// Returns an error when an identifier is absent from the source or when
/// emulation or snapshotting fails.
pub fn diagnose_smb_left_direction(
    rom: &[u8],
    source: &SmbArchiveReport,
    ids: &[u64],
) -> Result<(Vec<SmbLeftDirectionEntry>, Vec<SmbLeftDirectionCandidate>), Box<dyn Error>> {
    let active = active_source_entries(source);
    let mut target = SmbTarget::from_smb_rom_bytes(rom)?;
    let mut prefix = PlayerColumnPrefix::new(&mut target)?;
    let mut recordings = Vec::with_capacity(ids.len());
    for id in ids {
        let entry = active
            .iter()
            .find(|entry| entry.id == *id)
            .ok_or("audit identifier is not an active source entry")?;
        recordings.push(record_player_column_entry(
            &mut target,
            &mut prefix,
            entry,
            entry.key.progress,
            PlayerColumnRules::SPREAD,
        )?);
    }
    let entries = recordings
        .iter()
        .map(|recording| SmbLeftDirectionEntry {
            id: recording.id,
            progress: recording.progress,
            frames: recording
                .continuations
                .iter()
                .map(|continuation| u16::try_from(continuation.wram.len()).unwrap_or(u16::MAX))
                .collect(),
            camera: (
                recording.continuations[0].camera[0],
                recording.continuations[0].camera[recording.continuations[0].camera.len() - 1],
            ),
            still: recording.continuations[0]
                .wram
                .iter()
                .map(smb_death_bytes)
                .collect(),
            right: recording.continuations[1]
                .wram
                .iter()
                .map(smb_death_bytes)
                .collect(),
            separation_frame: player_column_max_span_frame(recording),
            separation_span: player_column_max_span_frame(recording).map(|frame| {
                let right = &recording.continuations[1].columns[frame];
                let left = &recording.continuations[2].columns[frame];
                let differing = (0..256)
                    .filter(|column| right[*column] != left[*column])
                    .collect::<Vec<_>>();
                let lowest = differing
                    .first()
                    .map_or(-1, |column| i32::try_from(*column).unwrap_or(i32::MAX));
                let highest = differing
                    .last()
                    .map_or(-1, |column| i32::try_from(*column).unwrap_or(i32::MAX));
                (highest - lowest + 1, lowest, highest)
            }),
        })
        .collect::<Vec<_>>();
    let smooth = (0..2_048_usize)
        .filter(|index| {
            player_column_distinct(&recordings, *index) && player_column_smooth(&recordings, *index)
        })
        .filter_map(|index| u16::try_from(index).ok())
        .collect::<Vec<_>>();
    let candidates = smooth
        .iter()
        .map(|index| {
            let position = usize::from(*index);
            SmbLeftDirectionCandidate {
                index: *index,
                endpoint: recordings
                    .iter()
                    .map(|recording| i32::from(recording.continuations[2].wram[0][position]))
                    .collect(),
                left_final: recordings
                    .iter()
                    .map(|recording| continuation_endpoints(recording, 2, position).1)
                    .collect(),
                left_min: recordings
                    .iter()
                    .map(|recording| {
                        recording.continuations[2]
                            .wram
                            .iter()
                            .map(|wram| i32::from(wram[position]))
                            .min()
                            .unwrap_or(-1)
                    })
                    .collect(),
                right_final: recordings
                    .iter()
                    .map(|recording| continuation_endpoints(recording, 1, position).1)
                    .collect(),
                left_at_separation: recordings
                    .iter()
                    .map(|recording| {
                        player_column_max_span_frame(recording).map_or(-1, |frame| {
                            i32::from(recording.continuations[2].wram[frame][position])
                        })
                    })
                    .collect(),
                right_at_separation: recordings
                    .iter()
                    .map(|recording| {
                        player_column_max_span_frame(recording).map_or(-1, |frame| {
                            i32::from(recording.continuations[1].wram[frame][position])
                        })
                    })
                    .collect(),
            }
        })
        .collect::<Vec<_>>();
    Ok((entries, candidates))
}

fn player_column_distinct(recordings: &[EntryRecording], index: usize) -> bool {
    let mut seen = [false; 256];
    let mut distinct = 0_usize;
    for recording in recordings {
        for continuation in &recording.continuations {
            for wram in &continuation.wram {
                let value = usize::from(wram[index]);
                if !seen[value] {
                    seen[value] = true;
                    distinct = distinct.saturating_add(1);
                    if distinct >= PLAYER_COLUMN_MIN_DISTINCT {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn player_column_smooth(recordings: &[EntryRecording], index: usize) -> bool {
    recordings.iter().all(|recording| {
        recording.continuations.iter().all(|continuation| {
            continuation.wram.windows(2).all(|pair| {
                (i32::from(pair[1][index]) - i32::from(pair[0][index])).abs()
                    <= PLAYER_COLUMN_MAX_STEP
            })
        })
    })
}

fn player_column_left_direction(recordings: &[EntryRecording], index: usize) -> bool {
    let mut decreasing = 0_usize;
    for recording in recordings {
        let (first, last) = continuation_endpoints(recording, 2, index);
        if last > first + PLAYER_COLUMN_LEFT_SLACK {
            return false;
        }
        if last <= first - PLAYER_COLUMN_LEFT_DECREASE {
            decreasing = decreasing.saturating_add(1);
        }
    }
    decreasing >= player_column_left_threshold(recordings.len())
}

/// Report the frame of largest equal-camera differing span, if any.
fn player_column_max_span_frame(recording: &EntryRecording) -> Option<usize> {
    let right = &recording.continuations[1];
    let left = &recording.continuations[2];
    let frames = right.columns.len().min(left.columns.len());
    let mut best: Option<(i32, usize)> = None;
    for frame in 0..frames {
        if right.camera[frame] != left.camera[frame] {
            continue;
        }
        let differing = (0..256)
            .filter(|column| right.columns[frame][*column] != left.columns[frame][*column])
            .collect::<Vec<_>>();
        let (Some(lowest), Some(highest)) = (differing.first(), differing.last()) else {
            continue;
        };
        let span =
            i32::try_from(highest.saturating_sub(*lowest).saturating_add(1)).unwrap_or(i32::MAX);
        if span > PLAYER_COLUMN_SPAN_MAX {
            continue;
        }
        if best.is_none_or(|(recorded, _)| span > recorded) {
            best = Some((span, frame));
        }
    }
    best.map(|(_, frame)| frame)
}

/// Contrast the two opposite masks at each entry's maximum-separation frame.
///
/// D41 recorded that a continuation ending in death makes its final frame
/// meaningless for a positional contrast. At the maximum-separation frame both
/// continuations are still running and their recorded cameras are equal.
fn player_column_separation_direction(
    recordings: &[EntryRecording],
    separation_frames: &[Option<usize>],
    index: usize,
) -> bool {
    let mut decreasing = 0_usize;
    for (recording, frame) in recordings.iter().zip(separation_frames) {
        let Some(frame) = frame else {
            return false;
        };
        let left = i32::from(recording.continuations[2].wram[*frame][index]);
        let right = i32::from(recording.continuations[1].wram[*frame][index]);
        if left > right + PLAYER_COLUMN_LEFT_SLACK {
            return false;
        }
        if left <= right - PLAYER_COLUMN_LEFT_DECREASE {
            decreasing = decreasing.saturating_add(1);
        }
    }
    decreasing >= player_column_left_threshold(recordings.len())
}

/// Contrast the held-left and held-right endpoints of the same entry.
///
/// D37 recorded that comparing the held-left endpoint against the entry's own
/// starting value is confounded by momentum and by pinning. The two opposite
/// masks at the same frame are the contrast the film rule itself uses.
fn player_column_left_versus_right(recordings: &[EntryRecording], index: usize) -> bool {
    let mut decreasing = 0_usize;
    for recording in recordings {
        let left = continuation_endpoints(recording, 2, index).1;
        let right = continuation_endpoints(recording, 1, index).1;
        if left > right + PLAYER_COLUMN_LEFT_SLACK {
            return false;
        }
        if left <= right - PLAYER_COLUMN_LEFT_DECREASE {
            decreasing = decreasing.saturating_add(1);
        }
    }
    decreasing >= player_column_left_threshold(recordings.len())
}

fn player_column_left_threshold(audited: usize) -> usize {
    if audited == PLAYER_COLUMN_LEFT_ENTRIES_BASE {
        return PLAYER_COLUMN_LEFT_ENTRIES;
    }
    audited
        .saturating_mul(3)
        .saturating_add(3)
        .saturating_div(4)
}

fn player_column_right_direction(recordings: &[EntryRecording], index: usize) -> bool {
    recordings.iter().all(|recording| {
        let (first, last) = continuation_endpoints(recording, 1, index);
        last >= first - PLAYER_COLUMN_RIGHT_SLACK
    })
}

fn continuation_endpoints(
    recording: &EntryRecording,
    continuation: usize,
    index: usize,
) -> (i32, i32) {
    let frames = &recording.continuations[continuation].wram;
    let first = i32::from(frames[0][index]);
    let last = i32::from(frames[frames.len().saturating_sub(1)][index]);
    (first, last)
}

fn qualifying_right_continuations(recordings: &[EntryRecording]) -> Vec<usize> {
    recordings
        .iter()
        .enumerate()
        .filter(|(_, recording)| {
            let camera = &recording.continuations[1].camera;
            camera[camera.len().saturating_sub(1)].saturating_sub(camera[0])
                >= PLAYER_COLUMN_CAMERA_ADVANCE
        })
        .map(|(entry, _)| entry)
        .collect()
}

fn player_column_camera_relative(
    recordings: &[EntryRecording],
    index: usize,
    qualifying: &[usize],
) -> bool {
    qualifying.iter().all(|entry| {
        let recording = &recordings[*entry];
        let camera = &recording.continuations[1].camera;
        let advance = camera[camera.len().saturating_sub(1)].saturating_sub(camera[0]);
        let (first, last) = continuation_endpoints(recording, 1, index);
        u32::try_from((last - first).abs()).unwrap_or(u32::MAX) < advance
    })
}

fn film_comparisons(recordings: &[EntryRecording]) -> Vec<FilmComparison> {
    let mut comparisons = Vec::new();
    for (entry, recording) in recordings.iter().enumerate() {
        for left in 0..recording.continuations.len() {
            for right in left.saturating_add(1)..recording.continuations.len() {
                let first = &recording.continuations[left];
                let second = &recording.continuations[right];
                for frame in 0..first.wram.len().min(second.wram.len()) {
                    if first.camera[frame] != second.camera[frame] {
                        continue;
                    }
                    let differing = (0..256)
                        .filter(|column| {
                            first.columns[frame][*column] != second.columns[frame][*column]
                        })
                        .collect::<Vec<_>>();
                    let (Some(lowest), Some(highest)) = (differing.first(), differing.last())
                    else {
                        continue;
                    };
                    comparisons.push(FilmComparison {
                        entry,
                        left,
                        right,
                        frame,
                        lowest: i32::try_from(*lowest).unwrap_or(i32::MAX),
                        highest: i32::try_from(*highest).unwrap_or(i32::MAX),
                        camera: first.camera[frame],
                    });
                }
            }
        }
    }
    comparisons
}

fn film_offset(
    recordings: &[EntryRecording],
    comparison: &FilmComparison,
    index: u16,
) -> Option<(i32, i32)> {
    let recording = &recordings[comparison.entry];
    let index = usize::from(index);
    let left = i32::from(recording.continuations[comparison.left].wram[comparison.frame][index]);
    let right = i32::from(recording.continuations[comparison.right].wram[comparison.frame][index]);
    let difference = (left - right).abs();
    if difference < PLAYER_COLUMN_FILM_GAP {
        return None;
    }
    let offset = comparison.lowest - left.min(right);
    let width = comparison.highest - comparison.lowest + 1 - difference;
    Some((offset, width))
}

fn film_evidence(
    recordings: &[EntryRecording],
    comparisons: &[FilmComparison],
    index: u16,
    rules: PlayerColumnRules,
) -> Option<SmbPlayerColumnFilmEvidence> {
    let measured = comparisons
        .iter()
        .filter_map(|comparison| {
            film_offset(recordings, comparison, index)
                .map(|(offset, width)| (offset, width, comparison.camera))
        })
        .collect::<Vec<_>>();
    // Pass or fail is "some offset agrees at least PLAYER_COLUMN_FILM_MIN_AGREE times";
    // the offset reported is the one the most comparisons agree with, so the recorded
    // number describes the identification rather than the low edge of the tolerance band.
    let best = (-PLAYER_COLUMN_FILM_OFFSETS..=PLAYER_COLUMN_FILM_OFFSETS)
        .map(|offset| {
            let agreeing = measured
                .iter()
                .filter(|(measured_offset, width, _)| {
                    (measured_offset - offset).abs() <= PLAYER_COLUMN_FILM_TOLERANCE
                        && (PLAYER_COLUMN_FILM_MIN_WIDTH..=PLAYER_COLUMN_FILM_MAX_WIDTH)
                            .contains(width)
                })
                .map(|(_, _, camera)| *camera)
                .collect::<Vec<_>>();
            let spread = match (agreeing.iter().min(), agreeing.iter().max()) {
                (Some(low), Some(high)) => high.saturating_sub(*low),
                _ => 0,
            };
            (agreeing.len(), offset, spread)
        })
        .filter(|(agreeing, _, spread)| {
            *agreeing >= PLAYER_COLUMN_FILM_MIN_AGREE
                && (!rules.require_camera_spread || *spread >= PLAYER_COLUMN_CAMERA_SPREAD)
        })
        .max_by_key(|(agreeing, offset, _)| (*agreeing, Reverse(offset.abs()), Reverse(*offset)))?;
    let (separating, left_smaller) = film_polarity(recordings, comparisons, index);
    Some(SmbPlayerColumnFilmEvidence {
        index,
        offset: i16::try_from(best.1).unwrap_or(i16::MAX),
        agreeing_comparisons: u64::try_from(best.0).unwrap_or(u64::MAX),
        comparisons: u64::try_from(measured.len()).unwrap_or(u64::MAX),
        camera_spread: best.2,
        separating_comparisons: separating,
        left_is_smaller: left_smaller,
        polarity: film_polarity_name(separating, left_smaller),
    })
}

/// Count separating comparisons and those in which the held-left value is smaller.
///
/// Only the held-right and held-left pair carries a direction, so comparisons
/// drawn from other continuation pairs are ignored.
fn film_polarity(
    recordings: &[EntryRecording],
    comparisons: &[FilmComparison],
    index: u16,
) -> (u64, u64) {
    let position = usize::from(index);
    let mut separating = 0_u64;
    let mut left_smaller = 0_u64;
    for comparison in comparisons {
        if (comparison.left, comparison.right) != (1, 2) {
            continue;
        }
        let recording = &recordings[comparison.entry];
        let right = i32::from(recording.continuations[1].wram[comparison.frame][position]);
        let left = i32::from(recording.continuations[2].wram[comparison.frame][position]);
        if (right - left).abs() < PLAYER_COLUMN_FILM_GAP {
            continue;
        }
        separating = separating.saturating_add(1);
        if left < right {
            left_smaller = left_smaller.saturating_add(1);
        }
    }
    (separating, left_smaller)
}

/// Name the recorded direction from the separating-comparison counts.
fn film_polarity_name(separating: u64, left_smaller: u64) -> String {
    if separating == 0 {
        return "inconsistent".to_owned();
    }
    if left_smaller.saturating_mul(4) >= separating.saturating_mul(3) {
        return "right_increasing".to_owned();
    }
    if left_smaller.saturating_mul(4) <= separating {
        return "left_increasing".to_owned();
    }
    "inconsistent".to_owned()
}

/// One recorded frame of a screen-column diagnosis continuation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbPlayerColumnFrame {
    /// Recorded frame index, with zero the audited endpoint.
    pub frame: u16,
    /// Raw screen-page byte.
    pub camera_page: u8,
    /// Raw screen-x byte.
    pub camera_x: u8,
    /// Raw player vertical-position byte.
    pub player_y: u8,
    /// Raw player engine-state byte.
    pub engine_state: u8,
    /// Program's own decoded mechanical state at this frame.
    pub decoded: SmbMechanicalState,
    /// Program's own decoded milestone ladder at this frame.
    pub milestones: SmbMilestones,
}

/// Per-entry continuation traces recorded for the screen-column diagnosis.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbPlayerColumnTrace {
    /// Stable source archive identifier.
    pub id: u64,
    /// Recorded progress bucket of the audited slice.
    pub progress: u16,
    /// One frame list per fixed continuation mask.
    pub continuations: Vec<Vec<SmbPlayerColumnFrame>>,
    /// Work RAM sampled every tenth recorded frame of every continuation.
    pub raw_wram: Vec<Vec<Vec<u8>>>,
}

/// Record continuation traces and frame strips for the audited screen-column slices.
///
/// This diagnosis runs no search, changes no search behavior, and involves no
/// model. It exposes recorded evidence about the same sixteen entries the audit
/// selected.
///
/// # Errors
///
/// Returns an error when the source lacks the audit slices or when emulation,
/// snapshotting, or rendering fails.
pub fn diagnose_smb_player_column(
    rom: &[u8],
    source: &SmbArchiveReport,
) -> Result<(Vec<SmbPlayerColumnTrace>, Vec<SmbAuditFrame>), Box<dyn Error>> {
    let selected = player_column_candidates(source, SmbPlayerColumnSelection::FirstOrdered)?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let mut target = SmbTarget::from_smb_rom_bytes(rom)?;
    let mut prefix = PlayerColumnPrefix::new(&mut target)?;
    let mut traces = Vec::with_capacity(selected.len());
    for (source_entry, progress) in &selected {
        let recording = record_player_column_entry(
            &mut target,
            &mut prefix,
            source_entry,
            *progress,
            PlayerColumnRules::LEGACY,
        )?;
        traces.push(SmbPlayerColumnTrace {
            id: source_entry.id,
            progress: *progress,
            continuations: recording
                .continuations
                .iter()
                .map(|continuation| {
                    continuation
                        .wram
                        .iter()
                        .enumerate()
                        .map(|(frame, wram)| SmbPlayerColumnFrame {
                            frame: u16::try_from(frame).unwrap_or(u16::MAX),
                            camera_page: wram[0x071a],
                            camera_x: wram[0x071c],
                            player_y: wram[0x00ce],
                            engine_state: wram[0x000e],
                            decoded: smb_mechanical_state_from_wram(wram),
                            milestones: smb_milestones_from_wram(wram),
                        })
                        .collect()
                })
                .collect(),
            raw_wram: recording
                .continuations
                .iter()
                .map(|continuation| {
                    continuation
                        .wram
                        .iter()
                        .step_by(10)
                        .map(|wram| wram.to_vec())
                        .collect()
                })
                .collect(),
        });
    }
    let mut requests = BTreeSet::new();
    for slice in 0..PLAYER_COLUMN_SLICES.len() {
        let entry = slice.saturating_mul(PLAYER_COLUMN_SLICE_SIZE);
        for continuation in 0..PLAYER_COLUMN_MASKS.len() {
            for frame in (0..=usize::from(PLAYER_COLUMN_FRAMES)).step_by(10) {
                requests.insert((entry, continuation, frame));
            }
        }
    }
    let frames = render_player_column_frames(&mut target, &selected, &requests)?;
    Ok((traces, frames))
}

fn render_player_column_frames(
    target: &mut SmbTarget,
    selected: &[PlayerColumnCandidate<'_>],
    requests: &BTreeSet<(usize, usize, usize)>,
) -> Result<Vec<SmbAuditFrame>, Box<dyn Error>> {
    let mut frames = Vec::new();
    let mut prefix = PlayerColumnPrefix::new(target)?;
    let mut entries = requests
        .iter()
        .map(|(entry, _, _)| *entry)
        .collect::<Vec<_>>();
    entries.dedup();
    for entry in entries {
        let (source, _) = selected[entry];
        let endpoint = replay_player_column_endpoint(target, &mut prefix, source)?;
        let endpoint_rgba = target.frame_rgba();
        for (continuation, mask) in PLAYER_COLUMN_MASKS.into_iter().enumerate() {
            let wanted = requests
                .iter()
                .filter(|(request_entry, request_continuation, _)| {
                    *request_entry == entry && *request_continuation == continuation
                })
                .map(|(_, _, frame)| *frame)
                .collect::<Vec<_>>();
            let Some(last) = wanted.last().copied() else {
                continue;
            };
            target.restore(&endpoint)?;
            for frame in 0..=last {
                if frame > 0 {
                    if target.is_dead() || target.exit_kind() != ExitKind::Ok {
                        break;
                    }
                    target.apply(&ButtonChord::new(mask, 1));
                }
                if wanted.contains(&frame) {
                    frames.push(SmbAuditFrame {
                        name: format!(
                            "entry-{entry:02}-id-{}-mask-{mask:02x}-frame-{frame:03}.png",
                            source.id
                        ),
                        rgba: if frame == 0 {
                            endpoint_rgba.clone()
                        } else {
                            target.frame_rgba()
                        },
                    });
                }
            }
        }
    }
    Ok(frames)
}

/// One recorded progress bucket of the re-admission pass.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbReadmissionBucket {
    /// Mechanical world number of the recorded entries.
    pub world: u8,
    /// Mechanical level number of the recorded entries.
    pub level: u8,
    /// Recorded progress bucket.
    pub progress: u16,
    /// Entries the source archive recorded in this bucket.
    pub recorded: u64,
    /// Entries of this bucket that survive the corrected terminal condition.
    pub surviving: u64,
}

/// Complete report for re-admitting a recorded archive under the corrected condition.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbReadmissionReport {
    /// Entries recorded by the source archive.
    pub recorded: u64,
    /// Entries that survive the corrected terminal condition.
    pub surviving: u64,
    /// Entries already below the play area at their recorded endpoint.
    pub below_play_area_at_endpoint: u64,
    /// Per-bucket recorded and surviving counts, in key order.
    pub buckets: Vec<SmbReadmissionBucket>,
    /// Maximum surviving world, level and progress, when anything survives.
    pub max_surviving: Option<(u8, u8, u16)>,
}

/// Replay a recorded archive under the corrected terminal condition and keep the survivors.
///
/// An entry survives when the corrected condition is false on every frame up to
/// and including its endpoint. The pass runs no search and involves no model.
///
/// # Errors
///
/// Returns an error when emulation or snapshotting fails.
pub fn readmit_smb_archive(
    rom: &[u8],
    source: &SmbArchiveReport,
) -> Result<(SmbReadmissionReport, SmbArchiveReport), Box<dyn Error>> {
    let mut ordered = source.entries.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        left.input
            .cmp(&right.input)
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut target = SmbTarget::from_smb_rom_bytes_headless(rom)?;
    target.reset();
    let mut snapshots = vec![
        target
            .snapshot()
            .ok_or("failed to snapshot re-admission genesis")?,
    ];
    let mut replayed = SmbInput::default();
    let mut surviving_ids = BTreeSet::new();
    let mut below_play_area = 0_u64;
    for entry in &ordered {
        let common = replayed
            .actions
            .iter()
            .zip(&entry.input.actions)
            .take_while(|(left, right)| left == right)
            .count();
        target.restore(&snapshots[common])?;
        snapshots.truncate(common + 1);
        for action in &entry.input.actions[common..] {
            target.apply(action);
            snapshots.push(
                target
                    .snapshot()
                    .ok_or("failed to snapshot a re-admission prefix")?,
            );
        }
        replayed = entry.input.clone();
        if target.exit_kind() != ExitKind::Ok {
            return Err("re-admission replay failed to emulate a recorded entry".into());
        }
        if smb_death_bytes(target.wram()).vertical_page >= PLAYER_BELOW_PLAY_AREA_PAGE {
            below_play_area = below_play_area.saturating_add(1);
        }
        if !target.is_dead() {
            surviving_ids.insert(entry.id);
        }
    }
    let mut buckets = BTreeMap::<(u8, u8, u16), (u64, u64)>::new();
    for entry in &source.entries {
        let counts = buckets
            .entry((entry.key.world, entry.key.level, entry.key.progress))
            .or_insert((0, 0));
        counts.0 = counts.0.saturating_add(1);
        if surviving_ids.contains(&entry.id) {
            counts.1 = counts.1.saturating_add(1);
        }
    }
    let survivors = source
        .entries
        .iter()
        .filter(|entry| surviving_ids.contains(&entry.id))
        .cloned()
        .collect::<Vec<_>>();
    let report = SmbReadmissionReport {
        recorded: u64::try_from(source.entries.len()).unwrap_or(u64::MAX),
        surviving: u64::try_from(survivors.len()).unwrap_or(u64::MAX),
        below_play_area_at_endpoint: below_play_area,
        buckets: buckets
            .iter()
            .map(
                |((world, level, progress), (recorded, surviving))| SmbReadmissionBucket {
                    world: *world,
                    level: *level,
                    progress: *progress,
                    recorded: *recorded,
                    surviving: *surviving,
                },
            )
            .collect(),
        max_surviving: survivors
            .iter()
            .map(|entry| (entry.key.world, entry.key.level, entry.key.progress))
            .max(),
    };
    let mut milestones = SmbMilestones::default();
    for entry in &survivors {
        merge_milestones(&mut milestones, entry.milestones);
    }
    let champion_input = survivors
        .iter()
        .max_by_key(|entry| {
            (
                milestone_key(entry.milestones),
                entry.key.world,
                entry.key.level,
                entry.key.progress,
                Reverse(entry.input.actions.len()),
            )
        })
        .map(|entry| entry.input.clone())
        .unwrap_or_default();
    let rebuilt = SmbArchiveReport {
        seed: source.seed,
        executions: 0,
        milestones,
        progress_watermark: SmbProgressWatermark::default(),
        first_reached: SmbMilestoneTimes::default(),
        first_inputs: SmbMilestoneInputs::default(),
        champion_input,
        retained: report.surviving,
        rejected: 0,
        deaths: 0,
        entries: survivors,
        progress_curve: Vec::new(),
        ladder: SmbLadder::default(),
        selector: SmbSelectorAccounting::default(),
    };
    Ok((report, rebuilt))
}

const DEATH_AUDIT_ENTRIES: usize = 8;
const DEATH_AUDIT_SCAN_CAP: usize = 128;
const DEATH_AUDIT_BUCKET_CAP: usize = 2;
const DEATH_AUDIT_FRAMES: usize = 240;
const DEATH_AUDIT_THRESHOLDS: [u8; 7] = [1, 2, 3, 4, 5, 6, 7];

/// One candidate terminal condition evaluated by the D34 audit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeathCandidate {
    /// The frozen condition: engine state equals its killed value.
    FrozenKill,
    /// The life counter is strictly below its value at the start of the replay.
    LifeCounterBelowStart,
    /// The vertical page byte is at least a fixed threshold.
    VerticalPageAtLeast(u8),
    /// The combined vertical position is at least a fixed threshold of pages.
    VerticalPositionAtLeast(u8),
}

impl DeathCandidate {
    fn name(self) -> String {
        match self {
            Self::FrozenKill => "K0".to_owned(),
            Self::LifeCounterBelowStart => "K1".to_owned(),
            Self::VerticalPageAtLeast(threshold) => format!("K2({threshold})"),
            Self::VerticalPositionAtLeast(threshold) => format!("K3({threshold})"),
        }
    }

    fn holds(self, bytes: SmbDeathBytes, start_life_counter: u8) -> bool {
        match self {
            Self::FrozenKill => bytes.engine_state == PLAYER_KILLED_STATE,
            Self::LifeCounterBelowStart => bytes.life_counter < start_life_counter,
            Self::VerticalPageAtLeast(threshold) => bytes.vertical_page >= threshold,
            Self::VerticalPositionAtLeast(threshold) => {
                u32::from(bytes.vertical_page) * 256 + u32::from(bytes.vertical_low)
                    >= u32::from(threshold) * 256
            }
        }
    }
}

fn death_candidate_order() -> Vec<DeathCandidate> {
    let mut candidates = vec![
        DeathCandidate::FrozenKill,
        DeathCandidate::LifeCounterBelowStart,
    ];
    candidates.extend(DEATH_AUDIT_THRESHOLDS.map(DeathCandidate::VerticalPageAtLeast));
    candidates.extend(DEATH_AUDIT_THRESHOLDS.map(DeathCandidate::VerticalPositionAtLeast));
    candidates
}

/// One evaluated candidate terminal condition.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbDeathCandidateReport {
    /// Fixed candidate name from the registration.
    pub name: String,
    /// Control frames on which the candidate is true; zero is required to pass.
    pub control_true_frames: u64,
    /// First-trip frame index per uncontrolled continuation, or `-1` when it never trips.
    pub trip_frames: Vec<i32>,
    /// Identifiers of the uncontrolled continuations on which the candidate never trips.
    pub without_trip: Vec<u64>,
    /// Median first-trip frame index, recorded only for a passing candidate.
    pub median_trip_frame: Option<u16>,
    /// Largest first-trip frame index, recorded only for a passing candidate.
    pub max_trip_frame: Option<u16>,
    /// Whether the registered acceptance rule admits this candidate.
    pub passes: bool,
}

/// One recorded uncontrolled continuation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbDeathTrace {
    /// Stable source archive identifier.
    pub id: u64,
    /// Recorded progress bucket of the continued entry.
    pub progress: u16,
    /// Whether the life counter was already below its genesis value at the endpoint.
    pub life_counter_below_genesis_at_endpoint: bool,
    /// Raw recorded bytes per frame, starting at the endpoint.
    pub frames: Vec<SmbDeathBytes>,
}

/// Complete terminal-death decode audit report.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbTerminalDeathReport {
    /// Champion actions consumed before the control replay was truncated.
    pub control_actions: usize,
    /// Control frames recorded, including the genesis frame.
    pub control_frames: u64,
    /// Fixed uncontrolled continuation length.
    pub continuation_frames: u16,
    /// Entries whose continuations were run during the qualification scan.
    pub scanned: u64,
    /// Identifiers admitted into the uncontrolled population.
    pub uncontrolled_ids: Vec<u64>,
    /// Every candidate in its fixed registered order.
    pub candidates: Vec<SmbDeathCandidateReport>,
    /// The candidate the registered adoption rule would select, if any.
    pub adoption_rule_selects: Option<String>,
    /// Raw recorded bytes per control frame.
    pub control_trace: Vec<SmbDeathBytes>,
    /// Raw recorded bytes per uncontrolled continuation.
    pub uncontrolled_traces: Vec<SmbDeathTrace>,
}

/// Audit candidate terminal-death conditions against recorded live and uncontrolled play.
///
/// # Errors
///
/// Returns an error when the source has no active entries, when the recorded
/// champion input never reaches the maximum recorded tuple, or when emulation
/// or snapshotting fails.
pub fn audit_smb_terminal_death(
    rom: &[u8],
    source: &SmbArchiveReport,
) -> Result<SmbTerminalDeathReport, Box<dyn Error>> {
    let active = active_source_entries(source);
    let max_tuple = active
        .iter()
        .map(|entry| (entry.key.world, entry.key.level))
        .max()
        .ok_or("source archive has no active entries")?;
    let mut target = SmbTarget::from_smb_rom_bytes(rom)?;
    let (control_trace, control_actions, genesis_life) =
        record_death_audit_control(&mut target, source, max_tuple)?;
    let (uncontrolled_traces, scanned) =
        record_death_audit_uncontrolled(&mut target, &active, max_tuple, genesis_life)?;
    let complete = uncontrolled_traces.len() == DEATH_AUDIT_ENTRIES;
    let candidates = death_candidate_order()
        .into_iter()
        .map(|candidate| {
            evaluate_death_candidate(
                candidate,
                &control_trace,
                &uncontrolled_traces,
                genesis_life,
                complete,
            )
        })
        .collect::<Vec<_>>();
    Ok(SmbTerminalDeathReport {
        control_actions,
        control_frames: u64::try_from(control_trace.len()).unwrap_or(u64::MAX),
        continuation_frames: u16::try_from(DEATH_AUDIT_FRAMES).unwrap_or(u16::MAX),
        scanned,
        uncontrolled_ids: uncontrolled_traces.iter().map(|trace| trace.id).collect(),
        adoption_rule_selects: adopt_death_candidate(&candidates),
        candidates,
        control_trace,
        uncontrolled_traces,
    })
}

/// Result of replaying the recorded champion input under the current terminal condition.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbLiveControlReport {
    /// Champion actions consumed before the maximum recorded tuple was reached.
    pub actions: usize,
    /// Frames recorded, including the genesis frame.
    pub frames: u64,
    /// Largest `$00b5` value seen anywhere along the replay.
    pub max_vertical_page: u8,
    /// Largest combined vertical position seen anywhere along the replay.
    pub max_vertical_position: u32,
}

/// Gate the current terminal condition against the recorded champion input.
///
/// The replay must reach the maximum recorded tuple without terminating. A
/// terminal condition that stops it is a false positive over recorded live play.
///
/// # Errors
///
/// Returns an error when the source has no active entries, when the replay
/// terminates or fails, or when it never reaches the maximum recorded tuple.
pub fn gate_smb_live_control(
    rom: &[u8],
    source: &SmbArchiveReport,
) -> Result<SmbLiveControlReport, Box<dyn Error>> {
    let max_tuple = active_source_entries(source)
        .iter()
        .map(|entry| (entry.key.world, entry.key.level))
        .max()
        .ok_or("source archive has no active entries")?;
    let mut target = SmbTarget::from_smb_rom_bytes(rom)?;
    let (trace, actions, _) = record_death_audit_control(&mut target, source, max_tuple)?;
    Ok(SmbLiveControlReport {
        actions,
        frames: u64::try_from(trace.len()).unwrap_or(u64::MAX),
        max_vertical_page: trace
            .iter()
            .map(|bytes| bytes.vertical_page)
            .max()
            .unwrap_or(0),
        max_vertical_position: trace
            .iter()
            .map(|bytes| u32::from(bytes.vertical_page) * 256 + u32::from(bytes.vertical_low))
            .max()
            .unwrap_or(0),
    })
}

fn record_death_audit_control(
    target: &mut SmbTarget,
    source: &SmbArchiveReport,
    max_tuple: (u8, u8),
) -> Result<(Vec<SmbDeathBytes>, usize, u8), Box<dyn Error>> {
    target.reset();
    let genesis_life = smb_death_bytes(target.wram()).life_counter;
    let mut trace = vec![smb_death_bytes(target.wram())];
    let mut actions = 0_usize;
    for action in &source.champion_input.actions {
        actions = actions.saturating_add(1);
        for _ in 0..action.bounded_hold_frames() {
            if target.is_dead() {
                return Err("champion control replay reached the frozen terminal condition".into());
            }
            target.apply(&ButtonChord::new(action.buttons, 1));
            if target.exit_kind() != ExitKind::Ok {
                return Err("champion control replay failed to emulate".into());
            }
            trace.push(smb_death_bytes(target.wram()));
            let decoded = smb_mechanical_state_from_wram(target.wram());
            if (decoded.world, decoded.level) == max_tuple {
                return Ok((trace, actions, genesis_life));
            }
        }
    }
    Err("the recorded champion input never reaches the maximum recorded tuple".into())
}

fn record_death_audit_uncontrolled(
    target: &mut SmbTarget,
    active: &[&SmbArchiveEntryReport],
    max_tuple: (u8, u8),
    genesis_life: u8,
) -> Result<(Vec<SmbDeathTrace>, u64), Box<dyn Error>> {
    let mut entries = active
        .iter()
        .copied()
        .filter(|entry| (entry.key.world, entry.key.level) == max_tuple)
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| (Reverse(entry.key.progress), entry.input.clone(), entry.id));
    let mut prefix = PlayerColumnPrefix::new(target)?;
    let mut per_bucket = BTreeMap::<u16, usize>::new();
    let mut traces = Vec::with_capacity(DEATH_AUDIT_ENTRIES);
    let mut scanned = 0_usize;
    for entry in entries {
        if traces.len() >= DEATH_AUDIT_ENTRIES || scanned >= DEATH_AUDIT_SCAN_CAP {
            break;
        }
        let taken = per_bucket.entry(entry.key.progress).or_insert(0);
        if *taken >= DEATH_AUDIT_BUCKET_CAP {
            continue;
        }
        scanned = scanned.saturating_add(1);
        let endpoint = replay_player_column_endpoint(target, &mut prefix, entry)?;
        // The frame buffer survives no restore, so the endpoint image is captured here.
        let endpoint_columns = column_signatures(&target.frame_rgba())?;
        let endpoint_bytes = smb_death_bytes(target.wram());
        if !death_audit_is_uncontrolled(target, &endpoint, endpoint_columns)? {
            continue;
        }
        *per_bucket.entry(entry.key.progress).or_insert(0) += 1;
        target.restore(&endpoint)?;
        let mut frames = vec![endpoint_bytes];
        for _ in 0..DEATH_AUDIT_FRAMES {
            target.apply(&ButtonChord::new(PLAYER_COLUMN_MASKS[0], 1));
            if target.exit_kind() != ExitKind::Ok {
                break;
            }
            frames.push(smb_death_bytes(target.wram()));
            if target.is_dead() {
                break;
            }
        }
        traces.push(SmbDeathTrace {
            id: entry.id,
            progress: entry.key.progress,
            life_counter_below_genesis_at_endpoint: endpoint_bytes.life_counter < genesis_life,
            frames,
        });
    }
    Ok((traces, u64::try_from(scanned).unwrap_or(u64::MAX)))
}

/// Report whether the controller has no rendered effect over the fixed continuation.
fn death_audit_is_uncontrolled(
    target: &mut SmbTarget,
    endpoint: &SmbSnapshot,
    endpoint_columns: [u64; 256],
) -> Result<bool, Box<dyn Error>> {
    let mut recorded = Vec::with_capacity(2);
    for mask in [PLAYER_COLUMN_MASKS[0], PLAYER_COLUMN_MASKS[2]] {
        target.restore(endpoint)?;
        let mut columns = vec![endpoint_columns];
        for _ in 0..PLAYER_COLUMN_FRAMES {
            if target.is_dead() || target.exit_kind() != ExitKind::Ok {
                break;
            }
            target.apply(&ButtonChord::new(mask, 1));
            columns.push(column_signatures(&target.frame_rgba())?);
        }
        recorded.push(columns);
    }
    let frames = recorded[0].len().min(recorded[1].len());
    Ok((0..frames).all(|frame| recorded[0][frame] == recorded[1][frame]))
}

fn evaluate_death_candidate(
    candidate: DeathCandidate,
    control: &[SmbDeathBytes],
    uncontrolled: &[SmbDeathTrace],
    genesis_life: u8,
    complete: bool,
) -> SmbDeathCandidateReport {
    let control_true_frames = control
        .iter()
        .filter(|bytes| candidate.holds(**bytes, genesis_life))
        .count();
    let mut trip_frames = Vec::with_capacity(uncontrolled.len());
    let mut without_trip = Vec::new();
    for trace in uncontrolled {
        match trace
            .frames
            .iter()
            .position(|bytes| candidate.holds(*bytes, genesis_life))
        {
            Some(frame) => trip_frames.push(i32::try_from(frame).unwrap_or(i32::MAX)),
            None => {
                trip_frames.push(-1);
                without_trip.push(trace.id);
            }
        }
    }
    let passes = complete && control_true_frames == 0 && without_trip.is_empty();
    let mut sorted = trip_frames.clone();
    sorted.sort_unstable();
    let median = sorted
        .get(sorted.len() / 2)
        .and_then(|frame| u16::try_from(*frame).ok());
    let largest = sorted.last().and_then(|frame| u16::try_from(*frame).ok());
    SmbDeathCandidateReport {
        name: candidate.name(),
        control_true_frames: u64::try_from(control_true_frames).unwrap_or(u64::MAX),
        trip_frames,
        without_trip,
        median_trip_frame: passes.then_some(median).flatten(),
        max_trip_frame: passes.then_some(largest).flatten(),
        passes,
    }
}

/// Apply the registered adoption rule: the passing candidate that trips earliest.
fn adopt_death_candidate(candidates: &[SmbDeathCandidateReport]) -> Option<String> {
    candidates
        .iter()
        .filter(|candidate| candidate.passes)
        .min_by_key(|candidate| candidate.max_trip_frame.unwrap_or(u16::MAX))
        .map(|candidate| candidate.name.clone())
}

fn active_source_entries(source: &SmbArchiveReport) -> Vec<&SmbArchiveEntryReport> {
    let mut cells = BTreeMap::<SmbArchiveKey, Vec<&SmbArchiveEntryReport>>::new();
    for entry in &source.entries {
        let cell = cells.entry(entry.key).or_default();
        if cell.len() < MAX_ENTRIES_PER_KEY {
            cell.push(entry);
            continue;
        }
        if let Some((index, existing)) = cell
            .iter()
            .enumerate()
            .max_by_key(|(_, existing)| entry_cost(existing))
            && entry.input.actions.len() < existing.input.actions.len()
        {
            cell[index] = entry;
        }
    }
    cells.into_values().flatten().collect()
}

/// Run completion search with an explicit bounded completion-only action horizon.
pub(crate) fn admission_is_viable(
    target: &mut SmbTarget,
    snapshot: &SmbSnapshot,
    policy: SmbArchiveRetentionPolicy,
) -> Result<bool, Box<dyn Error>> {
    let horizon = match policy {
        SmbArchiveRetentionPolicy::Frozen => return Ok(true),
        SmbArchiveRetentionPolicy::ProbeAtAdmission => VIABILITY_PROBE_FRAMES,
        SmbArchiveRetentionPolicy::ProbeAtAdmission45
        | SmbArchiveRetentionPolicy::ProbeAtAdmission45Snapback16 => VIABILITY_PROBE_FRAMES_SHORT,
    };
    let mut viable = false;
    for mask in VIABILITY_PROBE_MASKS {
        target.restore(snapshot)?;
        if target.survives_probe(mask, horizon) {
            viable = true;
            break;
        }
    }
    target.restore(snapshot)?;
    Ok(viable)
}

pub(crate) fn merge_progress_watermark(
    watermark: &mut SmbProgressWatermark,
    observations: &[SmbObservations],
) {
    for observation in observations {
        let decoded = observation.decoded;
        *watermark = (*watermark).max(SmbProgressWatermark {
            world: decoded.world,
            level: decoded.level,
            progress: decoded.progress,
        });
    }
}

pub(crate) fn archive_key(wram: &[u8; 2_048], policy: SmbArchiveKeyPolicy) -> SmbArchiveKey {
    let state = smb_mechanical_state_from_wram(wram);
    let digest = Sha256::digest(wram);
    // The decoded observation field keeps its recorded 0..=15 meaning; only the
    // key term carries the page, so both operator views stay true.
    let vertical = match policy {
        SmbArchiveKeyPolicy::Frozen
        | SmbArchiveKeyPolicy::FrozenRooms
        | SmbArchiveKeyPolicy::FrozenRoom
        | SmbArchiveKeyPolicy::FrozenArea
        | SmbArchiveKeyPolicy::FrozenAreaSpan
        | SmbArchiveKeyPolicy::FrozenRoomX16 { .. } => state.player_y_bucket,
        SmbArchiveKeyPolicy::VerticalPage => smb_death_bytes(wram)
            .vertical_page
            .saturating_mul(16)
            .saturating_add(state.player_y_bucket),
    };
    let room_x_bucket = match policy {
        SmbArchiveKeyPolicy::FrozenRoomX16 {
            world,
            level,
            progress,
        } if (state.world, state.level, state.progress) == (world, level, progress) => {
            let player_x = u32::from(wram[PLAYER_ROOM_X_PAGE_OFFSET]) * 256
                + u32::from(wram[PLAYER_ROOM_X_LOW_OFFSET]);
            let camera = smb_camera_pixels(wram);
            let screen_x = player_x.saturating_sub(camera).min(255);
            u8::try_from(screen_x / 16).unwrap_or(15).saturating_add(1)
        }
        _ => 0,
    };
    SmbArchiveKey {
        world: state.world,
        level: state.level,
        progress: state.progress,
        player_y_bucket: vertical,
        player_engine_state: state.player_engine_state,
        state_fingerprint: digest[0] & STATE_FINGERPRINT_MASK,
        room_x_bucket,
        rooms: 0,
        room: [0; 3],
    }
}

/// SMB player horizontal page byte, `$006d`, read by the room-x key term.
const PLAYER_ROOM_X_PAGE_OFFSET: usize = 0x006d;
/// SMB player horizontal position byte within the page, `$0086`.
const PLAYER_ROOM_X_LOW_OFFSET: usize = 0x0086;

/// D71 ruling: the frozen nine masks plus Down (`0x20`), appended so the
/// shared prefix keeps its order. Selecting this table changes every
/// derived suffix, so campaigns record their vocabulary in the header.
pub const DOWN_TEN_BUTTON_MASKS: [u8; 10] =
    [0x00, 0x01, 0x02, 0x40, 0x80, 0x81, 0x82, 0x83, 0x10, 0x20];

pub(crate) fn sample_chord_from_masks(
    rand: &mut StdRand,
    duration_policy: SmbArchiveDurationPolicy,
    masks: &[u8],
) -> Result<ButtonChord, Box<dyn Error>> {
    let buttons =
        masks[rand.below(NonZeroUsize::new(masks.len()).ok_or("empty SMB button vocabulary")?)];
    let hold_frames = match duration_policy {
        SmbArchiveDurationPolicy::Legacy => {
            if rand.below(NonZeroUsize::new(4).ok_or("invalid hold odds")?) != 0 {
                u8::try_from(
                    2 + rand.below(NonZeroUsize::new(11).ok_or("invalid short hold span")?),
                )?
            } else {
                u8::try_from(1 + rand.below(NonZeroUsize::new(120).ok_or("invalid hold span")?))?
            }
        }
        SmbArchiveDurationPolicy::Stratified => {
            if rand.below(NonZeroUsize::new(2).ok_or("invalid stratum odds")?) == 0 {
                u8::try_from(
                    2 + rand.below(NonZeroUsize::new(11).ok_or("invalid short hold span")?),
                )?
            } else {
                u8::try_from(
                    96 + rand.below(NonZeroUsize::new(25).ok_or("invalid long hold span")?),
                )?
            }
        }
    };
    Ok(ButtonChord::new(buttons, hold_frames))
}

pub(crate) fn merge_action_milestones(
    milestones: &mut SmbMilestones,
    target: &SmbTarget,
) -> Result<(), Box<dyn Error>> {
    for observation in target.last_action_observations() {
        let wram: &[u8; 2_048] = observation
            .wram
            .as_slice()
            .try_into()
            .map_err(|_| "SMB observation WRAM is not exactly 2 KiB")?;
        merge_milestones(milestones, smb_milestones_from_wram(wram));
    }
    Ok(())
}

pub(crate) fn merge_milestones(aggregate: &mut SmbMilestones, current: SmbMilestones) {
    aggregate.max_1_1_scroll_bucket = aggregate
        .max_1_1_scroll_bucket
        .max(current.max_1_1_scroll_bucket);
    aggregate.reached_1_1_flag |= current.reached_1_1_flag;
    aggregate.reached_1_2 |= current.reached_1_2;
    aggregate.reached_onward |= current.reached_onward;
}

pub(crate) fn update_first_inputs(
    times: &mut SmbMilestoneTimes,
    inputs: &mut SmbMilestoneInputs,
    current: SmbMilestones,
    execution: u64,
    input: &SmbInput,
) {
    if current.max_1_1_scroll_bucket > 0 {
        times.progress_into_1_1.get_or_insert(execution);
        inputs
            .progress_into_1_1
            .get_or_insert_with(|| input.clone());
    }
    if current.reached_1_1_flag {
        times.flag_1_1.get_or_insert(execution);
        inputs.flag_1_1.get_or_insert_with(|| input.clone());
    }
    if current.reached_1_2 {
        times.level_1_2.get_or_insert(execution);
        inputs.level_1_2.get_or_insert_with(|| input.clone());
    }
    if current.reached_onward {
        times.onward.get_or_insert(execution);
        inputs.onward.get_or_insert_with(|| input.clone());
    }
}

pub(crate) fn milestone_key(milestones: SmbMilestones) -> (bool, bool, bool, u16) {
    (
        milestones.reached_onward,
        milestones.reached_1_2,
        milestones.reached_1_1_flag,
        milestones.max_1_1_scroll_bucket,
    )
}

fn entry_cost(entry: &SmbArchiveEntryReport) -> (usize, u64) {
    (entry.input.actions.len(), entry.id)
}

#[cfg(test)]
mod tests {
    use super::{
        Archive, ArchiveCandidate, ContinuationRecording, EntryRecording,
        MAX_SMB_COMPLETION_ACTIONS, ROOM_IDENTITY_BYTES, SELECTION_EXHAUSTION_THRESHOLD,
        SmbArchiveKey, SmbArchiveKeyPolicy, SmbArchiveReplacementPolicy, SmbArchiveSelectorPolicy,
        SmbArchiveWaypointPolicy, SmbDeathBytes, SmbProgressWatermark, SmbRoomIdentity,
        SmbSelectorDraw, SmbSelectorPath, analyze_player_column, merge_progress_watermark,
    };
    use crate::{
        smb::target::{ButtonChord, SmbInput, SmbObservations, SmbSnapshot, SmbTarget},
        target::Target,
    };
    use libafl_bolts::rands::StdRand;

    const SCREEN_COLUMN_INDEX: usize = 100;
    const ABSOLUTE_INDEX: usize = 200;
    const CONSTANT_INDEX: usize = 150;
    const NOISY_INDEX: usize = 160;
    const RISING_UNDER_LEFT_INDEX: usize = 170;
    const REPLICATED_INDICES: [usize; 4] = [300, 304, 308, 312];

    fn scripted_column(entry: usize, continuation: usize, frame: usize) -> i32 {
        let start = 40 + i32::try_from(entry).expect("entry index");
        let frame = i32::try_from(frame).expect("frame index");
        match continuation {
            1 => start + frame.min(60),
            2 => (start - frame).max(0),
            _ => start,
        }
    }

    fn scripted_camera(continuation: usize, frame: usize) -> u32 {
        let frame = u32::try_from(frame).expect("frame index");
        if continuation == 1 {
            2 * frame.saturating_sub(60)
        } else {
            0
        }
    }

    fn scripted_signatures(column: i32) -> [u64; 256] {
        let mut signatures = [0_u64; 256];
        for offset in 0..16 {
            let lit = usize::try_from(column + offset).expect("lit column");
            if lit < signatures.len() {
                signatures[lit] = 1;
            }
        }
        signatures
    }

    fn scripted_recording(entry: usize) -> EntryRecording {
        let mut continuations = Vec::new();
        for continuation in 0..3 {
            let mut recording = ContinuationRecording {
                wram: Vec::new(),
                columns: Vec::new(),
                camera: Vec::new(),
            };
            for frame in 0..=120 {
                let column = scripted_column(entry, continuation, frame);
                let camera = scripted_camera(continuation, frame);
                let mut wram = [0_u8; 2_048];
                let byte = u8::try_from(column).expect("column byte");
                wram[SCREEN_COLUMN_INDEX] = byte;
                for index in REPLICATED_INDICES {
                    wram[index] = byte;
                }
                wram[ABSOLUTE_INDEX] =
                    u8::try_from(column + i32::try_from(camera).expect("camera"))
                        .unwrap_or(u8::MAX);
                wram[CONSTANT_INDEX] = 7;
                wram[NOISY_INDEX] = if frame % 2 == 0 { 0 } else { 200 };
                wram[RISING_UNDER_LEFT_INDEX] = u8::try_from(frame).unwrap_or(u8::MAX);
                recording.wram.push(wram);
                recording.camera.push(camera);
                recording.columns.push(scripted_signatures(column));
            }
            continuations.push(recording);
        }
        EntryRecording {
            id: u64::try_from(entry).expect("entry id"),
            progress: if entry < 8 { 39 } else { 32 },
            frontier: entry < 8,
            continuations,
        }
    }

    #[test]
    fn player_column_audit_selects_the_screen_relative_byte() {
        let recordings = (0..16).map(scripted_recording).collect::<Vec<_>>();
        let (report, comparisons) = analyze_player_column(&recordings);
        assert!(!comparisons.is_empty());
        assert_eq!(report.qualifying_right_continuations, 16);
        let survivors = report.camera_relative_survivors.clone();
        assert!(survivors.contains(&u16::try_from(SCREEN_COLUMN_INDEX).expect("index")));
        assert!(!survivors.contains(&u16::try_from(ABSOLUTE_INDEX).expect("index")));
        assert!(!survivors.contains(&u16::try_from(CONSTANT_INDEX).expect("index")));
        assert!(!survivors.contains(&u16::try_from(NOISY_INDEX).expect("index")));
        assert!(!survivors.contains(&u16::try_from(RISING_UNDER_LEFT_INDEX).expect("index")));
        for index in REPLICATED_INDICES {
            let index = u16::try_from(index).expect("index");
            assert!(report.stride_rejected.contains(&index));
        }
        let selected = report.selected.expect("conclusive audit");
        assert_eq!(
            selected.index,
            u16::try_from(SCREEN_COLUMN_INDEX).expect("index")
        );
        assert_eq!(selected.offset, 0);
        assert!(selected.agreeing_comparisons >= 8);
    }

    #[test]
    fn player_column_audit_reports_nothing_without_a_camera_advance() {
        let mut recordings = (0..16).map(scripted_recording).collect::<Vec<_>>();
        for recording in &mut recordings {
            for camera in &mut recording.continuations[1].camera {
                *camera = 0;
            }
        }
        let (report, _) = analyze_player_column(&recordings);
        assert_eq!(report.qualifying_right_continuations, 0);
        assert!(report.camera_relative_survivors.is_empty());
        assert!(report.selected.is_none());
    }

    #[test]
    fn player_column_steerability_and_left_threshold_scale_with_the_audited_set() {
        let steerable = scripted_recording(0);
        assert!(super::player_column_is_steerable(&steerable));
        let mut frozen = scripted_recording(1);
        let right = frozen.continuations[1].wram.clone();
        frozen.continuations[2].wram = right;
        assert!(!super::player_column_is_steerable(&frozen));
        assert_eq!(super::player_column_left_threshold(16), 12);
        assert_eq!(super::player_column_left_threshold(8), 6);
        assert_eq!(super::player_column_left_threshold(9), 7);
        assert_eq!(super::player_column_left_threshold(4), 3);
    }

    #[test]
    fn player_column_audit_still_selects_with_a_smaller_audited_set() {
        let recordings = (0..8).map(scripted_recording).collect::<Vec<_>>();
        let (report, _) = analyze_player_column(&recordings);
        let selected = report.selected.expect("conclusive audit");
        assert_eq!(
            selected.index,
            u16::try_from(SCREEN_COLUMN_INDEX).expect("index")
        );
    }

    fn scripted_death_trace(id: u64, frames: &[(u8, u8, u8, u8)]) -> super::SmbDeathTrace {
        super::SmbDeathTrace {
            id,
            progress: 0,
            life_counter_below_genesis_at_endpoint: false,
            frames: frames
                .iter()
                .map(
                    |(engine_state, life_counter, vertical_page, vertical_low)| SmbDeathBytes {
                        engine_state: *engine_state,
                        life_counter: *life_counter,
                        vertical_page: *vertical_page,
                        vertical_low: *vertical_low,
                        ..SmbDeathBytes::default()
                    },
                )
                .collect(),
        }
    }

    #[test]
    fn death_candidates_read_only_the_bytes_they_are_named_for() {
        let falling = SmbDeathBytes {
            engine_state: 0x06,
            life_counter: 1,
            vertical_page: 3,
            vertical_low: 0x20,
            ..SmbDeathBytes::default()
        };
        assert!(!super::DeathCandidate::FrozenKill.holds(falling, 2));
        assert!(super::DeathCandidate::LifeCounterBelowStart.holds(falling, 2));
        assert!(!super::DeathCandidate::LifeCounterBelowStart.holds(falling, 1));
        assert!(super::DeathCandidate::VerticalPageAtLeast(3).holds(falling, 2));
        assert!(!super::DeathCandidate::VerticalPageAtLeast(4).holds(falling, 2));
        assert!(super::DeathCandidate::VerticalPositionAtLeast(3).holds(falling, 2));
        assert!(!super::DeathCandidate::VerticalPositionAtLeast(4).holds(falling, 2));
    }

    #[test]
    fn death_audit_rejects_a_candidate_that_is_true_during_live_play() {
        let control = vec![SmbDeathBytes {
            vertical_page: 1,
            ..SmbDeathBytes::default()
        }];
        let uncontrolled = (0..8)
            .map(|id| scripted_death_trace(id, &[(0x00, 2, 1, 0x00), (0x00, 2, 3, 0x00)]))
            .collect::<Vec<_>>();
        let report = super::evaluate_death_candidate(
            super::DeathCandidate::VerticalPageAtLeast(1),
            &control,
            &uncontrolled,
            2,
            true,
        );
        assert_eq!(report.control_true_frames, 1);
        assert!(!report.passes);
        let later = super::evaluate_death_candidate(
            super::DeathCandidate::VerticalPageAtLeast(3),
            &control,
            &uncontrolled,
            2,
            true,
        );
        assert_eq!(later.control_true_frames, 0);
        assert!(later.passes);
        assert_eq!(later.max_trip_frame, Some(1));
    }

    #[test]
    fn death_audit_requires_a_trip_on_every_uncontrolled_continuation() {
        let mut uncontrolled = (0..7)
            .map(|id| scripted_death_trace(id, &[(0x00, 2, 3, 0x00)]))
            .collect::<Vec<_>>();
        uncontrolled.push(scripted_death_trace(7, &[(0x00, 2, 1, 0x00)]));
        let report = super::evaluate_death_candidate(
            super::DeathCandidate::VerticalPageAtLeast(3),
            &[],
            &uncontrolled,
            2,
            true,
        );
        assert_eq!(report.without_trip, vec![7]);
        assert!(!report.passes);
        assert_eq!(report.max_trip_frame, None);
    }

    #[test]
    fn death_audit_adopts_the_passing_candidate_that_trips_earliest() {
        let candidates = vec![
            super::SmbDeathCandidateReport {
                name: "K0".to_owned(),
                control_true_frames: 0,
                trip_frames: vec![-1],
                without_trip: vec![0],
                median_trip_frame: None,
                max_trip_frame: None,
                passes: false,
            },
            super::SmbDeathCandidateReport {
                name: "K1".to_owned(),
                control_true_frames: 0,
                trip_frames: vec![90],
                without_trip: Vec::new(),
                median_trip_frame: Some(90),
                max_trip_frame: Some(90),
                passes: true,
            },
            super::SmbDeathCandidateReport {
                name: "K2(3)".to_owned(),
                control_true_frames: 0,
                trip_frames: vec![12],
                without_trip: Vec::new(),
                median_trip_frame: Some(12),
                max_trip_frame: Some(12),
                passes: true,
            },
        ];
        assert_eq!(
            super::adopt_death_candidate(&candidates),
            Some("K2(3)".to_owned())
        );
        assert_eq!(super::adopt_death_candidate(&candidates[..1]), None);
    }

    fn synthetic_nrom() -> Vec<u8> {
        let mut rom = vec![0_u8; 16 + (16 * 1024) + (8 * 1024)];
        rom[..16].copy_from_slice(&[b'N', b'E', b'S', 0x1a, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let prg = &mut rom[16..16 + (16 * 1024)];
        prg.fill(0xea);
        prg[..3].copy_from_slice(&[0x4c, 0x00, 0x80]);
        for vector in [0x3ffa, 0x3ffc, 0x3ffe] {
            prg[vector..vector + 2].copy_from_slice(&0x8000_u16.to_le_bytes());
        }
        rom
    }

    #[test]
    fn progress_watermark_uses_action_interiors() {
        let mut watermark = SmbProgressWatermark::default();
        let mut first = SmbObservations {
            frame_count: 1,
            wram: Vec::new(),
            decoded: Default::default(),
            milestones: Default::default(),
            changed_indices: Vec::new(),
            dead: false,
            log_line: String::new(),
        };
        first.decoded.world = 0;
        first.decoded.level = 2;
        first.decoded.progress = 41;
        let mut endpoint = first.clone();
        endpoint.frame_count = 2;
        endpoint.decoded.progress = 39;
        merge_progress_watermark(&mut watermark, &[first, endpoint]);
        assert_eq!(watermark.progress, 41);
    }

    fn selector_snapshot() -> SmbSnapshot {
        let rom = synthetic_nrom();
        let mut target =
            SmbTarget::from_smb_rom_bytes_headless(&rom).expect("load selector target");
        target.reset();
        target.snapshot().expect("snapshot selector genesis")
    }

    fn selector_archive(keys: &[(u8, u8, u16)]) -> Archive {
        let snapshot = selector_snapshot();
        let mut archive = Archive::new();
        for (index, (world, level, progress)) in keys.iter().enumerate() {
            let input = SmbInput {
                actions: vec![ButtonChord::new(
                    u8::try_from(index / 120).expect("chord mask"),
                    u8::try_from((index % 120) + 1).expect("hold frames"),
                )],
            };
            let key = SmbArchiveKey {
                world: *world,
                level: *level,
                progress: *progress,
                player_y_bucket: u8::try_from(index / 64).expect("vertical bucket"),
                player_engine_state: 0,
                state_fingerprint: u8::try_from(index % 64).expect("fingerprint"),
                room_x_bucket: 0,
                rooms: 0,
                room: [0; 3],
            };
            archive
                .insert(
                    None,
                    0,
                    ArchiveCandidate {
                        input,
                        key,
                        milestones: crate::smb::target::SmbMilestones::default(),
                    },
                    snapshot.clone(),
                )
                .expect("insert selector entry")
                .expect("retain selector entry");
        }
        archive
    }

    #[test]
    fn corrected_selector_draws_only_the_maximal_pair_band() {
        let mut keys: Vec<(u8, u8, u16)> = vec![(1, 0, 144)];
        keys.extend(std::iter::repeat_n((0, 0, 100), 6));
        keys.extend([(1, 0, 124), (1, 0, 120), (0, 1, 60)]);
        let mut archive = selector_archive(&keys);
        let mut rand = StdRand::with_seed(0x5eed_5e1e);
        let mut tie_class_draws = 0;
        for _ in 0..64 {
            let (id, draw) = archive
                .select_parent(&mut rand, MAX_SMB_COMPLETION_ACTIONS)
                .expect("corrected selection");
            let draw = draw.expect("corrected draw record");
            if draw.path == SmbSelectorPath::TieClass {
                tie_class_draws += 1;
                assert_eq!(
                    id, 0,
                    "tie-class draws must come from the (1, 0, 144) entry"
                );
                assert_eq!(draw.classes_skipped, 0);
                assert!(!draw.counter_reset);
            }
        }
        assert!(tie_class_draws > 0);
    }

    #[test]
    fn tie_class_prefers_more_rooms_over_higher_progress() {
        let keys: Vec<(u8, u8, u16)> = vec![(7, 3, 153), (7, 3, 152), (7, 3, 20), (7, 3, 19)];
        let mut archive = selector_archive(&keys);
        archive.entries[2].report.key.rooms = 2;
        archive.entries[3].report.key.rooms = 2;
        let mut rand = StdRand::with_seed(0x0700_0400);
        let mut tie_class_draws = 0;
        for _ in 0..128 {
            let (id, draw) = archive
                .select_parent(&mut rand, MAX_SMB_COMPLETION_ACTIONS)
                .expect("room selection");
            let draw = draw.expect("room draw record");
            if draw.path == SmbSelectorPath::TieClass {
                tie_class_draws += 1;
                assert!(
                    id == 2 || id == 3,
                    "tie-class draws must come from the two-room entries, got {id}"
                );
                assert_eq!(draw.classes_skipped, 0);
            }
        }
        assert!(tie_class_draws > 0);
    }

    #[test]
    fn class_uniform_spreads_frontier_draws_over_every_occupied_class() {
        let mut keys: Vec<(u8, u8, u16)> = vec![(7, 3, 153)];
        keys.extend(std::iter::repeat_n((7, 3, 150), 40));
        keys.push((7, 3, 20));
        let mut archive = selector_archive(&keys);
        archive.entries[41].report.key.player_y_bucket = 3;
        archive.set_selector_policy(SmbArchiveSelectorPolicy::ClassUniform);
        let mut rand = StdRand::with_seed(0xc1a5_5e5e);
        let mut class_draws = 0;
        let mut lone_low_draws = 0;
        for _ in 0..400 {
            let (id, draw) = archive
                .select_parent(&mut rand, MAX_SMB_COMPLETION_ACTIONS)
                .expect("class-uniform selection");
            let draw = draw.expect("class-uniform draw record");
            if draw.path == SmbSelectorPath::ClassUniform {
                class_draws += 1;
                if id == 41 {
                    lone_low_draws += 1;
                }
            }
        }
        assert!(class_draws > 200);
        // The single low entry is one class among many (one per distinct
        // fingerprint-free tuple), so it must receive a visible share rather
        // than the near-zero share the frontier walk would give it.
        assert!(lone_low_draws > 10, "lone low class drew {lone_low_draws}");
    }

    #[test]
    fn room_uniform_splits_frontier_draws_evenly_across_the_rooms_of_the_deepest_pair() {
        // Deepest pair 8-4 with three rooms: the start room at progress 153
        // (40 entries), a loop-return room at progress 150 (40 entries), and
        // one pipe-arrival entry at progress 17. A shallower pair 8-3 at
        // progress 200 must not be drawn while 8-4 has unexhausted entries.
        let mut keys: Vec<(u8, u8, u16)> = Vec::new();
        keys.extend(std::iter::repeat_n((7, 3, 153), 40));
        keys.extend(std::iter::repeat_n((7, 3, 150), 40));
        keys.push((7, 3, 17));
        keys.extend(std::iter::repeat_n((7, 2, 200), 10));
        let mut archive = selector_archive(&keys);
        for id in 0..40 {
            archive.entries[id].report.key.room = [3, 5, 0];
        }
        for id in 40..80 {
            archive.entries[id].report.key.room = [3, 5, 5];
        }
        archive.entries[80].report.key.room = [3, 5, 1];
        archive.set_selector_policy(SmbArchiveSelectorPolicy::RoomUniform);
        let mut rand = StdRand::with_seed(0x5e1e_c7ed);
        let mut per_room = std::collections::BTreeMap::<SmbRoomIdentity, u64>::new();
        let mut room_draws = 0_u64;
        for _ in 0..900 {
            let (id, draw) = archive
                .select_parent(&mut rand, MAX_SMB_COMPLETION_ACTIONS)
                .expect("room-uniform selection");
            let draw = draw.expect("room-uniform draw record");
            if draw.path != SmbSelectorPath::RoomUniform {
                continue;
            }
            room_draws += 1;
            let key = archive.entries[id].report.key;
            assert_eq!((key.world, key.level), (7, 3), "drew the shallower pair");
            *per_room.entry(key.room).or_default() += 1;
            // Every retained child keeps the counters fresh, so exhaustion
            // never narrows the comparison.
            archive.record_selection(id, &draw);
            archive
                .record_selection_outcome(id, true, 1)
                .expect("outcome");
        }
        assert!(room_draws > 600);
        let lone = per_room[&[3, 5, 1]];
        let start = per_room[&[3, 5, 0]];
        let loop_room = per_room[&[3, 5, 5]];
        for share in [lone, start, loop_room] {
            assert!(
                share * 3 > room_draws / 2 && share * 3 < room_draws * 3 / 2,
                "uneven room shares: {per_room:?}"
            );
        }
    }

    #[test]
    fn room_band_uniform_splits_a_room_over_its_unexhausted_bands() {
        // One 8-4 room with 40 entries in the deepest band (progress 304),
        // 20 in the band below it (300), and 40 and 10 in two shallow bands
        // (256, 270). The deepest-band walk never leaves 304 while it stays
        // unexhausted; the band rule gives each band a quarter of the draws.
        let mut keys: Vec<(u8, u8, u16)> = Vec::new();
        keys.extend(std::iter::repeat_n((7, 3, 304), 40));
        keys.extend(std::iter::repeat_n((7, 3, 300), 20));
        keys.extend(std::iter::repeat_n((7, 3, 256), 40));
        keys.extend(std::iter::repeat_n((7, 3, 270), 10));
        let mut archive = selector_archive(&keys);
        for entry in &mut archive.entries {
            entry.report.key.room = [3, 5, 16];
        }
        archive.set_selector_policy(SmbArchiveSelectorPolicy::RoomBandUniform);
        let mut rand = StdRand::with_seed(0x5e1e_c7ee);
        let mut per_band = std::collections::BTreeMap::<u16, u64>::new();
        let mut band_draws = 0_u64;
        for _ in 0..900 {
            let (id, draw) = archive
                .select_parent(&mut rand, MAX_SMB_COMPLETION_ACTIONS)
                .expect("room-band selection");
            let draw = draw.expect("room-band draw record");
            if draw.path != SmbSelectorPath::RoomBandUniform {
                continue;
            }
            band_draws += 1;
            let key = archive.entries[id].report.key;
            *per_band
                .entry(key.progress / super::FRONTIER_PROGRESS_BAND)
                .or_default() += 1;
            archive.record_selection(id, &draw);
            archive
                .record_selection_outcome(id, true, 1)
                .expect("outcome");
        }
        assert!(band_draws > 600);
        assert_eq!(per_band.len(), 4, "bands drawn: {per_band:?}");
        for share in per_band.values() {
            assert!(
                share * 4 > band_draws / 2 && share * 4 < band_draws * 3 / 2,
                "uneven band shares: {per_band:?}"
            );
        }
    }

    #[test]
    fn frozen_room_keys_the_current_room_and_ignores_repeated_loops() {
        let mut archive = Archive::new();
        archive.set_key_policy(SmbArchiveKeyPolicy::FrozenRoom);
        let genesis = selector_snapshot();
        let area_snapshot = |area: [u8; 2]| -> SmbSnapshot {
            let mut value = serde_json::to_value(&genesis).expect("serialize snapshot");
            let wram = value["observation"]["wram"]
                .as_array_mut()
                .expect("snapshot work RAM");
            for (offset, byte) in ROOM_IDENTITY_BYTES.into_iter().zip(area) {
                wram[offset] = serde_json::json!(byte);
            }
            serde_json::from_value(value).expect("rebuild snapshot")
        };
        let key = |progress: u16| SmbArchiveKey {
            world: 7,
            level: 3,
            progress,
            ..BASELINE_LIKE_KEY
        };
        let insert = |archive: &mut Archive, parent, actions: usize, key, area| {
            archive
                .insert(
                    parent,
                    0,
                    ArchiveCandidate {
                        input: SmbInput {
                            actions: vec![ButtonChord::new(1, 2); actions],
                        },
                        key,
                        milestones: crate::smb::target::SmbMilestones::default(),
                    },
                    area_snapshot(area),
                )
                .expect("insert")
        };
        let root = insert(&mut archive, None, 1, key(10), [3, 5]).expect("root");
        assert_eq!(archive.entries[root].report.key.room, [3, 5, 0]);
        assert_eq!(archive.entries[root].report.key.rooms, 0);
        let deep = insert(&mut archive, Some(root), 2, key(150), [3, 5]).expect("deep");
        assert_eq!(archive.entries[deep].report.key.room, [3, 5, 0]);
        let looped = insert(&mut archive, Some(deep), 3, key(85), [3, 5]).expect("loop");
        assert_eq!(archive.entries[looped].report.key.room, [3, 5, 5]);
        let deep_again = insert(&mut archive, Some(looped), 4, key(150), [3, 5]).expect("deep");
        assert_eq!(archive.entries[deep_again].report.key.room, [3, 5, 5]);
        // A second loop lands in the same room, so the cell already holds an
        // entry with the same key and fewer actions: no new cell, and the
        // replacement rule refuses the longer input once the cell is full.
        let looped_again =
            insert(&mut archive, Some(deep_again), 5, key(85), [3, 5]).expect("loop");
        assert_eq!(
            archive.entries[looped_again].report.key,
            archive.entries[looped].report.key
        );
        assert!(insert(&mut archive, Some(looped_again), 6, key(85), [3, 5]).is_none());
    }

    #[test]
    fn frozen_area_keeps_same_area_warps_in_one_room_and_opens_rooms_on_area_change() {
        let mut archive = Archive::new();
        archive.set_key_policy(SmbArchiveKeyPolicy::FrozenArea);
        let genesis = selector_snapshot();
        let area_snapshot = |area: [u8; 2]| -> SmbSnapshot {
            let mut value = serde_json::to_value(&genesis).expect("serialize snapshot");
            let wram = value["observation"]["wram"]
                .as_array_mut()
                .expect("snapshot work RAM");
            for (offset, byte) in ROOM_IDENTITY_BYTES.into_iter().zip(area) {
                wram[offset] = serde_json::json!(byte);
            }
            serde_json::from_value(value).expect("rebuild snapshot")
        };
        let key = |progress: u16| SmbArchiveKey {
            world: 7,
            level: 3,
            progress,
            ..BASELINE_LIKE_KEY
        };
        let insert = |archive: &mut Archive, parent, actions: usize, key, area| {
            archive
                .insert(
                    parent,
                    0,
                    ArchiveCandidate {
                        input: SmbInput {
                            actions: vec![ButtonChord::new(1, 2); actions],
                        },
                        key,
                        milestones: crate::smb::target::SmbMilestones::default(),
                    },
                    area_snapshot(area),
                )
                .expect("insert")
        };
        let root = insert(&mut archive, None, 1, key(10), [3, 5]).expect("root");
        assert_eq!(archive.entries[root].report.key.room, [3, 5, 0]);
        let deep = insert(&mut archive, Some(root), 2, key(150), [3, 5]).expect("deep");
        // A warp back inside the same area stays in the area's room.
        let looped = insert(&mut archive, Some(deep), 3, key(85), [3, 5]).expect("loop");
        assert_eq!(archive.entries[looped].report.key.room, [3, 5, 0]);
        // An area change opens a room keyed by its arrival page, and a
        // return to the first area opens another.
        let water = insert(&mut archive, Some(looped), 4, key(20), [0, 2]).expect("water");
        assert_eq!(archive.entries[water].report.key.room, [0, 2, 1]);
        let back = insert(&mut archive, Some(water), 5, key(170), [3, 5]).expect("back");
        assert_eq!(archive.entries[back].report.key.room, [3, 5, 10]);
        assert_eq!(archive.room_set(back), &[[0, 2, 1], [3, 5, 0], [3, 5, 10]]);
    }

    #[test]
    fn frozen_area_span_lands_same_area_warps_in_the_room_that_covers_the_page() {
        let mut archive = Archive::new();
        archive.set_key_policy(SmbArchiveKeyPolicy::FrozenAreaSpan);
        let genesis = selector_snapshot();
        let area_snapshot = |area: [u8; 2]| -> SmbSnapshot {
            let mut value = serde_json::to_value(&genesis).expect("serialize snapshot");
            let wram = value["observation"]["wram"]
                .as_array_mut()
                .expect("snapshot work RAM");
            for (offset, byte) in ROOM_IDENTITY_BYTES.into_iter().zip(area) {
                wram[offset] = serde_json::json!(byte);
            }
            serde_json::from_value(value).expect("rebuild snapshot")
        };
        let key = |progress: u16| SmbArchiveKey {
            world: 7,
            level: 3,
            progress,
            ..BASELINE_LIKE_KEY
        };
        let insert = |archive: &mut Archive, parent, actions: usize, key, area| {
            archive
                .insert(
                    parent,
                    0,
                    ArchiveCandidate {
                        input: SmbInput {
                            actions: vec![ButtonChord::new(1, 2); actions],
                        },
                        key,
                        milestones: crate::smb::target::SmbMilestones::default(),
                    },
                    area_snapshot(area),
                )
                .expect("insert")
        };
        let root = insert(&mut archive, None, 1, key(10), [3, 5]).expect("root");
        let deep = insert(&mut archive, Some(root), 2, key(230), [3, 5]).expect("deep");
        let water = insert(&mut archive, Some(deep), 3, key(20), [0, 2]).expect("water");
        let back = insert(&mut archive, Some(water), 4, key(260), [3, 5]).expect("back");
        assert_eq!(archive.entries[back].report.key.room, [3, 5, 16]);
        let tip = insert(&mut archive, Some(back), 5, key(304), [3, 5]).expect("tip");
        // The page-19 loop returns to page 16: the after-water room covers it.
        let looped = insert(&mut archive, Some(tip), 6, key(258), [3, 5]).expect("loop");
        assert_eq!(archive.entries[looped].report.key.room, [3, 5, 16]);
        // A pipe back to page 1 lands in the start room, which covers page 1.
        let restart = insert(&mut archive, Some(looped), 7, key(20), [3, 5]).expect("restart");
        assert_eq!(archive.entries[restart].report.key.room, [3, 5, 0]);
        assert_eq!(
            archive.room_set(restart),
            &[[0, 2, 1], [3, 5, 0], [3, 5, 16]]
        );
        // Under the area-only rule the same pipe stays in the current room.
        let mut area_only = Archive::new();
        area_only.set_key_policy(SmbArchiveKeyPolicy::FrozenArea);
        let root = insert(&mut area_only, None, 1, key(10), [3, 5]).expect("root");
        let water = insert(&mut area_only, Some(root), 2, key(20), [0, 2]).expect("water");
        let back = insert(&mut area_only, Some(water), 3, key(260), [3, 5]).expect("back");
        let restart = insert(&mut area_only, Some(back), 4, key(20), [3, 5]).expect("restart");
        assert_eq!(area_only.entries[restart].report.key.room, [3, 5, 16]);
    }

    #[test]
    fn frozen_rooms_counts_distinct_rooms_along_a_lineage_and_resets_on_level_change() {
        let mut archive = Archive::new();
        archive.set_key_policy(SmbArchiveKeyPolicy::FrozenRooms);
        let genesis = selector_snapshot();
        let area_snapshot = |area: [u8; 2]| -> SmbSnapshot {
            let mut value = serde_json::to_value(&genesis).expect("serialize snapshot");
            let wram = value["observation"]["wram"]
                .as_array_mut()
                .expect("snapshot work RAM");
            for (offset, byte) in ROOM_IDENTITY_BYTES.into_iter().zip(area) {
                wram[offset] = serde_json::json!(byte);
            }
            serde_json::from_value(value).expect("rebuild snapshot")
        };
        let key = |world: u8, level: u8, progress: u16| SmbArchiveKey {
            world,
            level,
            progress,
            ..BASELINE_LIKE_KEY
        };
        let insert = |archive: &mut Archive, parent, actions: usize, key, area| {
            archive
                .insert(
                    parent,
                    0,
                    ArchiveCandidate {
                        input: SmbInput {
                            actions: vec![ButtonChord::new(1, 2); actions],
                        },
                        key,
                        milestones: crate::smb::target::SmbMilestones::default(),
                    },
                    area_snapshot(area),
                )
                .expect("insert")
                .expect("retained")
        };
        let root = insert(&mut archive, None, 1, key(7, 3, 10), [3, 5]);
        assert_eq!(archive.entries[root].report.key.rooms, 1);
        assert_eq!(archive.room_set(root), &[[3, 5, 0]]);
        // Walking on, and even stepping back a whole screen, stays in the room.
        let same = insert(&mut archive, Some(root), 2, key(7, 3, 40), [3, 5]);
        assert_eq!(archive.entries[same].report.key.rooms, 1);
        let stepped_back = insert(&mut archive, Some(same), 3, key(7, 3, 24), [3, 5]);
        assert_eq!(archive.entries[stepped_back].report.key.rooms, 1);
        // A different area is a new room.
        let pipe = insert(&mut archive, Some(stepped_back), 4, key(7, 3, 2), [2, 0]);
        assert_eq!(archive.entries[pipe].report.key.rooms, 2);
        assert_eq!(archive.room_set(pipe), &[[2, 0, 0], [3, 5, 0]]);
        // Returning to the first area at a later page arrives in a third room.
        let back = insert(&mut archive, Some(pipe), 5, key(7, 3, 70), [3, 5]);
        assert_eq!(archive.entries[back].report.key.rooms, 3);
        assert_eq!(archive.room_set(back), &[[2, 0, 0], [3, 5, 0], [3, 5, 4]]);
        // A warp inside the area (more than a screen backward) arrives at a
        // new page and counts; the same warp again adds nothing.
        let warped = insert(&mut archive, Some(back), 6, key(7, 3, 17), [3, 5]);
        assert_eq!(archive.entries[warped].report.key.rooms, 4);
        let onward = insert(&mut archive, Some(warped), 7, key(7, 3, 70), [3, 5]);
        assert_eq!(archive.entries[onward].report.key.rooms, 4);
        let looped = insert(&mut archive, Some(onward), 8, key(7, 3, 17), [3, 5]);
        assert_eq!(archive.entries[looped].report.key.rooms, 4);
        let next_level = insert(&mut archive, Some(looped), 9, key(8, 0, 0), [1, 1]);
        assert_eq!(archive.entries[next_level].report.key.rooms, 1);
        assert_eq!(archive.room_set(next_level), &[[1, 1, 0]]);

        let mut frozen = Archive::new();
        let plain = insert(&mut frozen, None, 1, key(7, 3, 10), [3, 5]);
        assert_eq!(frozen.entries[plain].report.key.rooms, 0);
        assert!(frozen.room_set(plain).is_empty());
    }

    #[test]
    fn rooms_is_omitted_from_serialized_keys_when_zero() {
        let mut key = BASELINE_LIKE_KEY;
        let legacy = serde_json::to_string(&key).expect("serialize legacy key");
        assert!(!legacy.contains("rooms"));
        key.rooms = 2;
        let extended = serde_json::to_string(&key).expect("serialize extended key");
        assert!(extended.contains("\"rooms\":2"));
        let decoded: SmbArchiveKey = serde_json::from_str(&legacy).expect("decode legacy key");
        assert_eq!(decoded.rooms, 0);
    }

    const BASELINE_LIKE_KEY: SmbArchiveKey = SmbArchiveKey {
        world: 7,
        level: 3,
        progress: 153,
        player_y_bucket: 11,
        player_engine_state: 8,
        state_fingerprint: 9,
        room_x_bucket: 0,
        rooms: 0,
        room: [0; 3],
    };

    #[test]
    fn corrected_selector_starves_exhausted_parents_and_falls_through() {
        let keys: Vec<(u8, u8, u16)> = vec![(1, 0, 144), (1, 0, 124), (1, 0, 123), (0, 0, 100)];
        let mut archive = selector_archive(&keys);
        let exhausting_draw = SmbSelectorDraw {
            path: SmbSelectorPath::TieClass,
            classes_skipped: 0,
            counter_reset: false,
            concentration: None,
            waypoint: false,
        };
        for _ in 0..SELECTION_EXHAUSTION_THRESHOLD {
            archive.record_selection(0, &exhausting_draw);
        }
        let mut rand = StdRand::with_seed(0x5eed_5e1f);
        let mut fell_through = 0;
        for _ in 0..64 {
            let (id, draw) = archive
                .select_parent(&mut rand, MAX_SMB_COMPLETION_ACTIONS)
                .expect("corrected selection");
            let draw = draw.expect("corrected draw record");
            if draw.path == SmbSelectorPath::TieClass {
                fell_through += 1;
                assert!(
                    id == 1 || id == 2,
                    "tie-class draws must fall through to the 124 band"
                );
                assert_eq!(draw.classes_skipped, 1);
                assert!(!draw.counter_reset);
            }
        }
        assert!(fell_through > 0);
        assert_eq!(
            archive.selector_report().tie_class_selections,
            SELECTION_EXHAUSTION_THRESHOLD
        );
    }

    #[test]
    fn corrected_selector_resets_deterministically_when_all_are_exhausted() {
        let keys: Vec<(u8, u8, u16)> = vec![(1, 0, 144), (0, 0, 100)];
        let mut archive = selector_archive(&keys);
        let exhausting_draw = SmbSelectorDraw {
            path: SmbSelectorPath::TieClass,
            classes_skipped: 0,
            counter_reset: false,
            concentration: None,
            waypoint: false,
        };
        for id in 0..keys.len() {
            for _ in 0..SELECTION_EXHAUSTION_THRESHOLD {
                archive.record_selection(id, &exhausting_draw);
            }
        }
        let mut rand = StdRand::with_seed(0x5eed_5e20);
        let mut reset_seen = false;
        for _ in 0..256 {
            let (id, draw) = archive
                .select_parent(&mut rand, MAX_SMB_COMPLETION_ACTIONS)
                .expect("corrected selection");
            let draw = draw.expect("corrected draw record");
            if draw.path == SmbSelectorPath::TieClass {
                assert!(
                    draw.counter_reset,
                    "the first tie-class draw after full exhaustion must reset"
                );
                assert_eq!(draw.classes_skipped, 2);
                assert_eq!(id, 0);
                archive.record_selection(id, &draw);
                reset_seen = true;
                break;
            }
        }
        assert!(reset_seen);
        assert_eq!(archive.selector_report().counter_resets, 1);
    }

    #[test]
    fn concentrated_selector_samples_only_the_recency_window() {
        // 140 entries in one tie class: the window is the 128 greatest ids.
        let keys: Vec<(u8, u8, u16)> = (0..140).map(|index| (1, 0, 118 + (index % 7))).collect();
        let mut archive = selector_archive(&keys);
        let mut rand = StdRand::with_seed(0x5eed_5e21);
        let mut tie_class_draws = 0;
        for _ in 0..256 {
            let (id, draw) = archive
                .select_parent(&mut rand, MAX_SMB_COMPLETION_ACTIONS)
                .expect("concentrated selection");
            let draw = draw.expect("concentrated draw record");
            match draw.path {
                SmbSelectorPath::ClassUniform
                | SmbSelectorPath::RoomUniform
                | SmbSelectorPath::RoomBandUniform => {
                    panic!("concentrated policy took a uniform class path")
                }
                SmbSelectorPath::TieClass => {
                    tie_class_draws += 1;
                    assert!(
                        id >= 12,
                        "tie-class draws must come from the 128 most recent members, got {id}"
                    );
                    let concentration = draw.concentration.expect("concentration record");
                    assert_eq!(concentration.window_size, 128);
                }
                SmbSelectorPath::Uniform => {
                    assert!(draw.concentration.is_none());
                }
            }
        }
        assert!(tie_class_draws > 0);
    }

    #[test]
    fn concentrated_window_slides_off_exhausted_members() {
        // 129 members at one progress: the window starts as ids 1..=128; when
        // all of them exhaust, the sampled set must refill from the
        // next-most-recent unexhausted member below, not skip the class.
        let keys: Vec<(u8, u8, u16)> = vec![(1, 0, 124); 129];
        let mut archive = selector_archive(&keys);
        let exhausting_draw = SmbSelectorDraw {
            path: SmbSelectorPath::TieClass,
            classes_skipped: 0,
            counter_reset: false,
            concentration: None,
            waypoint: false,
        };
        for id in 1..=128 {
            for _ in 0..SELECTION_EXHAUSTION_THRESHOLD {
                archive.record_selection(id, &exhausting_draw);
            }
        }
        let mut rand = StdRand::with_seed(0x5eed_5e22);
        let mut slid = false;
        for _ in 0..64 {
            let (id, draw) = archive
                .select_parent(&mut rand, MAX_SMB_COMPLETION_ACTIONS)
                .expect("concentrated selection");
            let draw = draw.expect("concentrated draw record");
            if draw.path == SmbSelectorPath::TieClass {
                assert_eq!(id, 0, "the only unexhausted member must be sampled");
                assert_eq!(draw.classes_skipped, 0);
                assert!(!draw.counter_reset);
                let concentration = draw.concentration.expect("concentration record");
                assert_eq!(concentration.window_size, 1);
                slid = true;
            }
        }
        assert!(slid);
    }

    fn waypoint_probe_key(world: u8, level: u8, progress: u16, vertical: u8) -> SmbArchiveKey {
        SmbArchiveKey {
            world,
            level,
            progress,
            player_y_bucket: vertical,
            player_engine_state: 0,
            state_fingerprint: 0,
            room_x_bucket: 0,
            rooms: 0,
            room: [0; 3],
        }
    }

    #[test]
    fn waypoint_region_membership_is_inclusive_and_pair_scoped() {
        let region = SmbArchiveWaypointPolicy::Region {
            world: 2,
            level: 1,
            low: 10,
            high: 20,
            band_low: 4,
            band_high: 8,
        };
        assert!(region.contains(&waypoint_probe_key(2, 1, 10, 4)));
        assert!(region.contains(&waypoint_probe_key(2, 1, 20, 8)));
        assert!(region.contains(&waypoint_probe_key(2, 1, 15, 6)));
        assert!(!region.contains(&waypoint_probe_key(2, 1, 9, 6)));
        assert!(!region.contains(&waypoint_probe_key(2, 1, 21, 6)));
        assert!(!region.contains(&waypoint_probe_key(2, 1, 15, 3)));
        assert!(!region.contains(&waypoint_probe_key(2, 1, 15, 9)));
        assert!(!region.contains(&waypoint_probe_key(2, 0, 15, 6)));
        assert!(!region.contains(&waypoint_probe_key(1, 1, 15, 6)));
        assert!(!SmbArchiveWaypointPolicy::Absent.contains(&waypoint_probe_key(2, 1, 15, 6)));
    }

    #[test]
    fn waypoint_cells_retain_auxiliary_entries() {
        let snapshot = selector_snapshot();
        let waypoint = SmbArchiveWaypointPolicy::Region {
            world: 1,
            level: 0,
            low: 16,
            high: 47,
            band_low: 0,
            band_high: 15,
        };
        let mut archive = Archive::new();
        archive.set_waypoint_policy(waypoint);
        let insert = |archive: &mut Archive, key: SmbArchiveKey, actions: usize| {
            // Distinct masks per cell keep the inputs distinct, so the
            // input-hash duplicate check never short-circuits the bound.
            let mask = if key.progress == 30 { 0x02 } else { 0x01 };
            let input = SmbInput {
                actions: vec![
                    ButtonChord::new(mask, u8::try_from(actions).expect("hold frames"));
                    actions
                ],
            };
            archive
                .insert(
                    None,
                    0,
                    ArchiveCandidate {
                        input,
                        key,
                        milestones: crate::smb::target::SmbMilestones::default(),
                    },
                    snapshot.clone(),
                )
                .expect("insert waypoint candidate")
        };
        // Outside the region the base cell bound holds: a longer third input
        // neither fits nor replaces.
        let outside = waypoint_probe_key(1, 0, 60, 5);
        assert!(insert(&mut archive, outside, 1).is_some());
        assert!(insert(&mut archive, outside, 2).is_some());
        assert!(insert(&mut archive, outside, 3).is_none());
        // Inside the region the auxiliary bound retains two more entries
        // before the same replacement discipline applies.
        let inside = waypoint_probe_key(1, 0, 30, 5);
        assert!(insert(&mut archive, inside, 1).is_some());
        assert!(insert(&mut archive, inside, 2).is_some());
        assert!(insert(&mut archive, inside, 3).is_some());
        assert!(insert(&mut archive, inside, 4).is_some());
        assert!(insert(&mut archive, inside, 5).is_none());
        assert_eq!(archive.waypoint_retained(), 2);
    }

    /// Insert one action onto a parent and report the new entry's identifier.
    fn chain_insert(
        archive: &mut Archive,
        parent: Option<usize>,
        prefix: &SmbInput,
        buttons: u8,
        hold: u8,
        key: SmbArchiveKey,
        snapshot: &SmbSnapshot,
    ) -> (Option<usize>, SmbInput) {
        let mut input = prefix.clone();
        input.actions.push(ButtonChord::new(buttons, hold));
        let id = archive
            .insert(
                parent,
                0,
                ArchiveCandidate {
                    input: input.clone(),
                    key,
                    milestones: crate::smb::target::SmbMilestones::default(),
                },
                snapshot.clone(),
            )
            .expect("chained insert");
        (id, input)
    }

    #[test]
    fn frames_in_level_counts_from_the_recorded_pair_transition() {
        let snapshot = selector_snapshot();
        let mut archive = Archive::new();
        let genesis = archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    input: SmbInput::default(),
                    key: waypoint_probe_key(0, 0, 0, 0),
                    milestones: crate::smb::target::SmbMilestones::default(),
                },
                snapshot.clone(),
            )
            .expect("genesis insert")
            .expect("genesis retained");
        assert_eq!(archive.entry_frames_in_level(genesis), 0);
        // Two actions inside the genesis pair accumulate their held frames.
        let (first, input) = chain_insert(
            &mut archive,
            Some(genesis),
            &SmbInput::default(),
            0x01,
            30,
            waypoint_probe_key(0, 0, 4, 0),
            &snapshot,
        );
        let first = first.expect("first retained");
        assert_eq!(archive.entry_frames_in_level(first), 30);
        let (second, input) = chain_insert(
            &mut archive,
            Some(first),
            &input,
            0x01,
            20,
            waypoint_probe_key(0, 0, 8, 0),
            &snapshot,
        );
        let second = second.expect("second retained");
        assert_eq!(archive.entry_frames_in_level(second), 50);
        // Crossing into the next pair restarts the count at the crossing
        // action, and the next action inside the new pair adds to that.
        let (crossed, input) = chain_insert(
            &mut archive,
            Some(second),
            &input,
            0x01,
            40,
            waypoint_probe_key(0, 1, 2, 0),
            &snapshot,
        );
        let crossed = crossed.expect("crossing retained");
        assert_eq!(archive.entry_frames_in_level(crossed), 40);
        let (after, _) = chain_insert(
            &mut archive,
            Some(crossed),
            &input,
            0x01,
            10,
            waypoint_probe_key(0, 1, 6, 0),
            &snapshot,
        );
        assert_eq!(archive.entry_frames_in_level(after.expect("retained")), 50);
    }

    #[test]
    fn the_frames_rule_displaces_a_slower_route_the_actions_rule_keeps() {
        let snapshot = selector_snapshot();
        let cell = waypoint_probe_key(0, 0, 16, 0);
        // Three routes into one cell. The first two are short in actions and
        // long in frames; the third is longer in actions and much shorter in
        // frames, which is exactly the collision the level clock cares about
        // and the frozen rule cannot see.
        let fill = |archive: &mut Archive| {
            let genesis = archive
                .insert(
                    None,
                    0,
                    ArchiveCandidate {
                        input: SmbInput::default(),
                        key: waypoint_probe_key(0, 0, 0, 0),
                        milestones: crate::smb::target::SmbMilestones::default(),
                    },
                    snapshot.clone(),
                )
                .expect("genesis insert")
                .expect("genesis retained");
            for buttons in [0x01_u8, 0x02] {
                chain_insert(
                    archive,
                    Some(genesis),
                    &SmbInput::default(),
                    buttons,
                    120,
                    cell,
                    &snapshot,
                );
            }
            let (fast, input) = chain_insert(
                archive,
                Some(genesis),
                &SmbInput::default(),
                0x04,
                5,
                waypoint_probe_key(0, 0, 8, 0),
                &snapshot,
            );
            chain_insert(archive, fast, &input, 0x04, 6, cell, &snapshot).0
        };
        let mut frozen = Archive::new();
        assert!(
            fill(&mut frozen).is_none(),
            "two actions never displace a one-action entry under the frozen rule"
        );
        let mut fastest = Archive::new();
        fastest.set_replacement_policy(SmbArchiveReplacementPolicy::FewestFramesInLevel);
        let admitted = fill(&mut fastest).expect("the eleven-frame route displaces a slower one");
        assert_eq!(fastest.entry_frames_in_level(admitted), 11);
        assert_eq!(fastest.replacement_frames_displaced(), 1);
        assert_eq!(frozen.replacement_frames_displaced(), 0);
    }

    #[test]
    fn waypoint_selection_prefers_region_members_until_exhausted() {
        let keys: Vec<(u8, u8, u16)> = vec![(1, 0, 144), (1, 0, 140), (0, 0, 100), (0, 0, 101)];
        let mut archive = selector_archive(&keys);
        archive.set_waypoint_policy(SmbArchiveWaypointPolicy::Region {
            world: 0,
            level: 0,
            low: 64,
            high: 127,
            band_low: 0,
            band_high: 15,
        });
        let mut rand = StdRand::with_seed(0x5eed_3a10);
        let mut waypoint_draws = 0;
        for _ in 0..32 {
            let (id, draw) = archive
                .select_parent(&mut rand, MAX_SMB_COMPLETION_ACTIONS)
                .expect("waypoint preference draw");
            let draw = draw.expect("selector annotation");
            if draw.path == SmbSelectorPath::TieClass {
                assert!(draw.waypoint, "tie-class draw must prefer the region");
                assert!([2, 3].contains(&id), "waypoint draw left the region");
                assert!(draw.concentration.is_some());
                waypoint_draws += 1;
            } else {
                assert!(!draw.waypoint, "uniform draws never claim the waypoint");
            }
        }
        assert!(waypoint_draws > 0);
        // Exhaust both region members; the preference falls through to the
        // promoted class walk and the deepest pair wins again.
        let exhausting_draw = SmbSelectorDraw {
            path: SmbSelectorPath::TieClass,
            classes_skipped: 0,
            counter_reset: false,
            concentration: None,
            waypoint: true,
        };
        for id in [2, 3] {
            for _ in 0..SELECTION_EXHAUSTION_THRESHOLD {
                archive.record_selection(id, &exhausting_draw);
            }
        }
        let mut fell_through = 0;
        for _ in 0..32 {
            let (id, draw) = archive
                .select_parent(&mut rand, MAX_SMB_COMPLETION_ACTIONS)
                .expect("post-exhaustion draw");
            let draw = draw.expect("selector annotation");
            if draw.path == SmbSelectorPath::TieClass {
                assert!(!draw.waypoint, "exhausted region must not be preferred");
                assert!([0, 1].contains(&id), "fall-through left the best class");
                fell_through += 1;
            }
        }
        assert!(fell_through > 0);
    }

    #[test]
    fn pinned_window_outranks_the_waypoint_preference() {
        let keys: Vec<(u8, u8, u16)> = vec![(1, 0, 144), (1, 0, 140), (0, 0, 100), (0, 0, 101)];
        let mut archive = selector_archive(&keys);
        archive.set_selector_policy(SmbArchiveSelectorPolicy::PinnedWindow {
            world: 1,
            level: 0,
            low: 128,
            high: 191,
        });
        archive.set_waypoint_policy(SmbArchiveWaypointPolicy::Region {
            world: 0,
            level: 0,
            low: 64,
            high: 127,
            band_low: 0,
            band_high: 15,
        });
        let mut rand = StdRand::with_seed(0x5eed_3a11);
        for _ in 0..64 {
            let (id, draw) = archive
                .select_parent(&mut rand, MAX_SMB_COMPLETION_ACTIONS)
                .expect("pinned draw");
            let draw = draw.expect("selector annotation");
            assert!(
                [0, 1].contains(&id),
                "the pin narrows every draw to the registered window"
            );
            assert!(
                !draw.waypoint,
                "a waypoint outside the pin finds no members and defers"
            );
        }
    }
}
