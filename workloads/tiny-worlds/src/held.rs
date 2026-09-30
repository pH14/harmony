// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, num::NonZeroUsize};

use searcher::search::{
    archive::{
        ArchiveEntryReport, ArchiveKey, Input, MAX_ARCHIVE_ENTRIES, RetentionPolicy,
        entries_by_suffix,
    },
    campaign::{
        ArchiveReportState, CampaignActionResult, CampaignAdmissionDecision, CampaignConfig,
        CampaignExecutionOptions, CampaignJobResult, CampaignOrigin, CampaignStreamHeader,
        CampaignStreamRecord, CampaignTypes, Evaluation, InputPolicy, Reporting, TargetExecution,
        WorkloadPolicies, default_window, postcard_value_sha256, replay_campaign_checkpointed,
        run_campaign_checkpointed_with_options,
    },
    draw::{draw_mixture_from_identifier, suffix_shape_from_identifier},
    rand::RomuDuoJrRand,
    rollout::ExecutionDisposition,
};
use serde::{Deserialize, Serialize};

const SUBPIXELS: i32 = 16;
const ACCEL: i32 = 1;
const WALK: i32 = 24;
const RUN: i32 = 40;
const JUMP: i32 = 64;
const HELD_GRAVITY: i32 = 2;
const GRAVITY: i32 = 7;
const RIGHT: u8 = 0x80;
const LEFT: u8 = 0x40;
const A: u8 = 0x01;
const B: u8 = 0x02;
const DIRECTION_BITS: u8 = 0xf0;
const DIRECTIONS: [u8; 9] = [0x00, 0x80, 0x40, 0x10, 0x20, 0x90, 0xa0, 0x50, 0x60];
const SHORT_HOLD: (u8, u8) = (2, 12);
const LONG_HOLD: (u8, u8) = (96, 120);
const MAX_HOLD: u8 = 120;
const BAND_PX: i32 = 128;
const COLUMN_PX: i32 = 16;
const HEIGHT_PX: i32 = 16;
const TIME_BUCKET_FRAMES: u32 = 1024;
const CLOCK_FRAMES: u32 = 1 << 24;
const MAX_WORK_BUDGET: u64 = 400_000_000;
const MAX_VERIFIED_WORK_BUDGET: u64 = 20_000_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Chord {
    pub buttons: u8,
    pub hold: u8,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChordDraw {
    #[default]
    ChangeOneControl,
    Fresh,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Course {
    pub ground_px: u16,
    pub pits_px: Vec<u16>,
}

impl Course {
    pub fn validate(&self) -> Result<(), String> {
        if !(16..=1024).contains(&self.ground_px) {
            return Err("ground must be 16..=1024 pixels".into());
        }
        if self.pits_px.is_empty() || self.pits_px.len() > 32 {
            return Err("a course has 1..=32 pits".into());
        }
        if self.pits_px.iter().any(|&pit| !(1..=120).contains(&pit)) {
            return Err("pits must be 1..=120 pixels wide".into());
        }
        Ok(())
    }

    fn section_px(&self, index: usize) -> i32 {
        self.pits_px[..index]
            .iter()
            .map(|&pit| i32::from(self.ground_px) + i32::from(pit))
            .sum()
    }

    fn end_px(&self) -> i32 {
        self.section_px(self.pits_px.len()) + i32::from(self.ground_px)
    }

    fn section_of(&self, x_px: i32) -> usize {
        (0..self.pits_px.len())
            .take_while(|&index| x_px >= self.section_px(index + 1))
            .count()
    }

    fn pit_section(&self, x_px: i32) -> Option<i32> {
        let index = self.section_of(x_px);
        let start = self.section_px(index);
        let pit = start + i32::from(self.ground_px);
        (index < self.pits_px.len() && x_px >= pit).then_some(start)
    }

    #[must_use]
    pub fn step(&self, mut state: State, buttons: u8) -> State {
        if state.goal {
            return state;
        }
        let a = buttons & A != 0;
        let cap = if buttons & B != 0 { RUN } else { WALK };
        let grounded = state.y == 0 && state.vy == 0;
        let direction = i32::from(buttons & RIGHT != 0) - i32::from(buttons & LEFT != 0);
        if direction != 0 {
            let speed = state.vx * direction;
            let speed = if speed < cap {
                (speed + ACCEL).min(cap)
            } else if grounded {
                (speed - ACCEL).max(cap)
            } else {
                speed
            };
            state.vx = speed * direction;
        } else if grounded {
            state.vx -= state.vx.signum() * ACCEL.min(state.vx.abs());
        }
        if grounded && a && !state.a_down {
            state.vy = JUMP;
        }
        state.a_down = a;
        if state.y > 0 || state.vy > 0 {
            state.y += state.vy;
            state.vy -= if a && state.vy > 0 {
                HELD_GRAVITY
            } else {
                GRAVITY
            };
            if state.y <= 0 {
                state.y = 0;
                state.vy = 0;
            }
        }
        state.x = (state.x + state.vx).max(0);
        if state.x == 0 {
            state.vx = state.vx.max(0);
        }
        state.frames = state.frames.saturating_add(1);
        if state.y == 0
            && let Some(start) = self.pit_section(state.x / SUBPIXELS)
        {
            state = State {
                x: start * SUBPIXELS,
                a_down: a,
                frames: state.frames,
                falls: state.falls.saturating_add(1),
                ..State::default()
            };
        }
        state.goal = state.x / SUBPIXELS >= self.end_px();
        state
    }

    fn key(&self, state: State) -> HeldKey {
        let x_px = state.x / SUBPIXELS;
        let y_px = state.y / SUBPIXELS;
        HeldKey {
            band: u16::try_from(x_px / BAND_PX).unwrap_or(u16::MAX),
            column: u16::try_from(x_px / COLUMN_PX).unwrap_or(u16::MAX),
            height: u8::try_from(y_px / HEIGHT_PX).unwrap_or(u8::MAX),
            time_bucket: u8::try_from(state.frames / TIME_BUCKET_FRAMES).unwrap_or(u8::MAX),
            time_left: CLOCK_FRAMES.saturating_sub(state.frames),
            goal: state.goal,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct State {
    x: i32,
    vx: i32,
    y: i32,
    vy: i32,
    a_down: bool,
    frames: u32,
    falls: u32,
    goal: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct HeldKey {
    band: u16,
    column: u16,
    height: u8,
    time_bucket: u8,
    time_left: u32,
    goal: bool,
}

impl ArchiveKey for HeldKey {
    type Place = (u16, u8, u8);
    type Progress = (bool, u16);
    type Identity = ();
    type Lineage = ();
    fn place(self) -> Self::Place {
        (self.column, self.height, self.time_bucket)
    }
    fn progress(self) -> Self::Progress {
        (self.goal, self.band)
    }
    fn identity(self) -> Self::Identity {}
    fn capacity() -> usize {
        1
    }
    fn preferences() -> usize {
        1
    }
    fn tier_rank_shift() -> u32 {
        1
    }
    fn preference_cmp(self, _preference: usize, other: Self) -> std::cmp::Ordering {
        self.time_left.cmp(&other.time_left)
    }
    fn complete(self, _parent: Option<(Self, &())>) -> Self {
        self
    }
    fn record(_lineage: &mut (), _key: Self) {}
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Observation {
    after: State,
    work_in_job: u64,
    job_work: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Evidence {
    pub objectives: u64,
    pub falls: u64,
    pub first_section_work: Vec<Option<u64>>,
    pub admitted_job_work: u64,
    pub job_start_work: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Report {
    #[serde(with = "entries_by_suffix")]
    pub entries: Vec<ArchiveEntryReport<Chord, HeldKey, bool>>,
    pub evidence: Evidence,
}

pub struct Target {
    state: State,
    work: u64,
    observation: Option<Observation>,
}

pub struct HeldWorkload {
    pub course: Course,
    pub chords: ChordDraw,
}

impl HeldWorkload {
    fn chord_identifier(&self) -> &'static str {
        match self.chords {
            ChordDraw::Fresh => "fresh",
            ChordDraw::ChangeOneControl => "change_one_control_v1",
        }
    }
}

fn below(rand: &mut RomuDuoJrRand, n: usize) -> Result<usize, Box<dyn Error>> {
    Ok(rand.below(NonZeroUsize::new(n).ok_or("empty draw")?))
}

fn stratified_hold(rand: &mut RomuDuoJrRand) -> Result<u8, Box<dyn Error>> {
    let (low, high) = if below(rand, 2)? == 0 {
        SHORT_HOLD
    } else {
        LONG_HOLD
    };
    Ok(low + u8::try_from(below(rand, usize::from(high - low) + 1)?)?)
}

pub fn change_one_control(rand: &mut RomuDuoJrRand, previous: u8) -> Result<u8, Box<dyn Error>> {
    match below(rand, 4)? {
        0 => {
            let direction = previous & DIRECTION_BITS;
            let others: Vec<u8> = DIRECTIONS
                .into_iter()
                .filter(|&mask| mask != direction)
                .collect();
            Ok((previous & !DIRECTION_BITS) | others[below(rand, others.len())?])
        }
        1 => Ok(previous ^ A),
        2 => Ok(previous ^ B),
        _ => Ok(previous),
    }
}

impl CampaignTypes for HeldWorkload {
    type Target = Target;
    type Action = Chord;
    type Key = HeldKey;
    type Milestones = bool;
    type Progress = bool;
    type Snapshot = State;
    type Observations = Observation;
    type Evidence = Evidence;
    type ArchiveReport = Report;
    type Run = ();
}

impl Reporting for HeldWorkload {
    fn stream_format(&self) -> &'static str {
        "tiny-held-world-v1"
    }
    fn checkpoint_format(&self) -> &'static str {
        "tiny-held-world-checkpoint-v1"
    }
    fn workload_identity_sha256(&self) -> String {
        postcard_value_sha256(&self.course).expect("serializable course")
    }
    fn action_cost_unit(&self) -> &'static str {
        "frames"
    }
    fn execution_work_unit(&self) -> &'static str {
        "frames"
    }
    fn result_sha256(&self, result: &CampaignJobResult<Self>) -> Result<String, Box<dyn Error>> {
        postcard_value_sha256(result)
    }
    fn archive_report(&self, evidence: &Evidence, state: ArchiveReportState<Self>) -> Report {
        Report {
            entries: state.entries,
            evidence: evidence.clone(),
        }
    }
}

impl InputPolicy for HeldWorkload {
    fn max_action_cost(&self) -> u64 {
        u64::from(MAX_HOLD)
    }
    fn policies(&self, _: &()) -> WorkloadPolicies {
        [
            ("held_chord_draw", self.chord_identifier()),
            ("held_duration", "stratified"),
        ]
        .into_iter()
        .map(|(field, value)| (field.to_owned(), value.to_owned()))
        .collect()
    }
    fn resolve_recorded(&self, policies: &WorkloadPolicies) -> Result<(), Box<dyn Error>> {
        if *policies != self.policies(&()) {
            return Err("unknown held-world policy".into());
        }
        Ok(())
    }
    fn sample_alphabet(
        &self,
        _: &(),
        previous: Option<&Chord>,
        rand: &mut RomuDuoJrRand,
    ) -> Result<Chord, Box<dyn Error>> {
        let buttons = match (self.chords, previous) {
            (ChordDraw::ChangeOneControl, Some(previous)) => {
                change_one_control(rand, previous.buttons)?
            }
            _ => DIRECTIONS[below(rand, DIRECTIONS.len())?] | u8::try_from(below(rand, 4)?)?,
        };
        Ok(Chord {
            buttons,
            hold: stratified_hold(rand)?,
        })
    }
}

impl TargetExecution for HeldWorkload {
    fn new_target(&self) -> Result<Target, String> {
        self.course.validate()?;
        Ok(Target {
            state: State::default(),
            work: 0,
            observation: None,
        })
    }
    fn reset(&self, target: &mut Target) {
        target.state = State::default();
        target.observation = None;
    }
    fn restore(&self, target: &mut Target, snapshot: &State) -> Result<(), Box<dyn Error>> {
        target.state = *snapshot;
        target.observation = None;
        Ok(())
    }
    fn execution_work(&self, target: &Target) -> u64 {
        target.work
    }
    fn action_cost_fn(&self) -> fn(&Chord) -> u64 {
        |chord| u64::from(chord.hold)
    }
    fn snapshot_memory_charge(_: &State) -> usize {
        std::mem::size_of::<State>()
    }
    fn apply_action(
        &self,
        target: &mut Target,
        action: &Chord,
        milestones: &mut bool,
    ) -> Result<(), Box<dyn Error>> {
        for _ in 0..action.hold {
            target.state = self.course.step(target.state, action.buttons);
        }
        target.work += u64::from(action.hold);
        target.observation = Some(Observation {
            after: target.state,
            work_in_job: target.work,
            job_work: None,
        });
        *milestones |= target.state.goal;
        Ok(())
    }
    fn rollout_observations(&self, target: &Target) -> Vec<Observation> {
        target.observation.iter().cloned().collect()
    }
    #[allow(clippy::too_many_arguments)]
    fn execute_job(
        &self,
        run: &(),
        target: &mut Target,
        origin_snapshot: &State,
        replay: &[Chord],
        parent_milestones: bool,
        suffix: &[Chord],
        retention: RetentionPolicy,
        stop_rollout_on_objective: bool,
    ) -> Result<CampaignJobResult<Self>, Box<dyn Error>> {
        let start_work = target.work;
        let mut result = searcher::search::rollout::execute_job(
            self,
            run,
            target,
            origin_snapshot,
            replay,
            parent_milestones,
            suffix,
            retention,
            stop_rollout_on_objective,
        )?;
        let work = target.work - start_work;
        for observation in result
            .actions
            .iter_mut()
            .flat_map(|action| action.observations.iter_mut())
        {
            observation.work_in_job -= start_work;
        }
        if let Some(observation) = result
            .actions
            .first_mut()
            .and_then(|action| action.observations.first_mut())
        {
            observation.job_work = Some(work);
        }
        Ok(result)
    }
    fn snapshot(&self, target: &mut Target) -> Result<State, Box<dyn Error>> {
        Ok(target.state)
    }
}

impl Evaluation for HeldWorkload {
    fn execution_disposition(&self, _: &Target) -> ExecutionDisposition {
        ExecutionDisposition::Runnable
    }
    fn objective_reached(&self, _: &(), target: &Target) -> Result<bool, Box<dyn Error>> {
        Ok(target.state.goal)
    }
    fn current_key(&self, target: &Target) -> Result<HeldKey, Box<dyn Error>> {
        Ok(self.course.key(target.state))
    }
    fn complete_candidate_key(&self, key: HeldKey, _: &State) -> Result<HeldKey, Box<dyn Error>> {
        Ok(key)
    }
    fn merge_milestones(&self, into: &mut bool, from: bool) {
        *into |= from;
    }
    fn aggregate_milestones(evidence: &Evidence) -> bool {
        evidence.objectives > 0
    }
    fn aggregate_progress(evidence: &Evidence) -> bool {
        evidence.objectives > 0
    }
    fn merge_origin_evidence(&self, evidence: &mut Evidence, source: &Report) {
        *evidence = source.evidence.clone();
    }
    fn merge_snapshot_root_evidence(
        &self,
        evidence: &mut Evidence,
        target: &Target,
    ) -> Result<(), Box<dyn Error>> {
        evidence.objectives += u64::from(target.state.goal);
        Ok(())
    }
    fn merge_import_evidence(&self, evidence: &mut Evidence, milestones: bool, _: &Input<Chord>) {
        evidence.objectives += u64::from(milestones);
    }
    fn merge_action_evidence<F>(
        &self,
        evidence: &mut Evidence,
        action: &CampaignActionResult<Self>,
        _sequence: u64,
        _input: F,
    ) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> Result<Input<Chord>, Box<dyn Error>>,
    {
        for observation in &action.observations {
            if let Some(work) = observation.job_work {
                evidence.job_start_work = evidence.admitted_job_work;
                evidence.admitted_job_work += work;
            }
            let section = self.course.section_of(observation.after.x / SUBPIXELS)
                + usize::from(observation.after.goal);
            evidence
                .first_section_work
                .resize(self.course.pits_px.len() + 2, None);
            for reached in &mut evidence.first_section_work[..=section] {
                reached.get_or_insert(evidence.job_start_work + observation.work_in_job);
            }
            evidence.falls = evidence.falls.max(u64::from(observation.after.falls));
        }
        evidence.objectives += u64::from(action.outcome.objective_reached);
        Ok(())
    }
    fn source_entries<'a>(
        &self,
        source: &'a Report,
    ) -> &'a [ArchiveEntryReport<Chord, HeldKey, bool>] {
        &source.entries
    }
    fn resume_input(&self, _: &Report) -> Result<Input<Chord>, Box<dyn Error>> {
        Err("held-world resume is not supported".into())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub course: Course,
    pub seed: u64,
    pub work_budget: u64,
    pub workers: u32,
    pub suffix: String,
    pub mixture: String,
    #[serde(default)]
    pub chords: ChordDraw,
    #[serde(default)]
    pub verify: bool,
}

#[derive(Default)]
struct StreamCounts {
    line: Vec<u8>,
    kept: Option<Vec<u8>>,
    header: bool,
    jobs: u64,
    skips: u64,
    decisions: [u64; 4],
}

impl StreamCounts {
    fn count(&mut self, line: &[u8]) -> Result<(), Box<dyn Error>> {
        if !self.header {
            serde_json::from_slice::<CampaignStreamHeader<serde_json::Value>>(line)?;
            self.header = true;
            return Ok(());
        }
        match serde_json::from_slice::<CampaignStreamRecord<serde_json::Value, serde_json::Value>>(
            line,
        )? {
            CampaignStreamRecord::Job(job) => {
                self.jobs += 1;
                for decision in &job.decisions {
                    self.decisions[match decision {
                        CampaignAdmissionDecision::Retained { .. } => 0,
                        CampaignAdmissionDecision::Duplicate { .. } => 1,
                        CampaignAdmissionDecision::Rejected
                        | CampaignAdmissionDecision::ProbeRefused => 2,
                        CampaignAdmissionDecision::Objective => 3,
                    }] += 1;
                }
            }
            CampaignStreamRecord::Skip(_) => self.skips += 1,
        }
        Ok(())
    }
}

impl std::io::Write for StreamCounts {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Some(kept) = &mut self.kept {
            kept.extend_from_slice(buf);
        }
        for &byte in buf {
            if byte != b'\n' {
                self.line.push(byte);
                continue;
            }
            let line = std::mem::take(&mut self.line);
            if !line.is_empty() {
                self.count(&line)
                    .map_err(|error| std::io::Error::other(error.to_string()))?;
            }
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[allow(clippy::disallowed_methods)]
fn telemetry_now() -> std::time::Instant {
    std::time::Instant::now()
}

pub fn run(request: &Request) -> Result<serde_json::Value, Box<dyn Error>> {
    request.course.validate()?;
    if request.work_budget == 0 || request.work_budget > MAX_WORK_BUDGET {
        return Err(format!("work budget must be 1..={MAX_WORK_BUDGET}").into());
    }
    if request.verify && request.work_budget > MAX_VERIFIED_WORK_BUDGET {
        return Err(format!(
            "verified runs keep the stream, so their budget is at most {MAX_VERIFIED_WORK_BUDGET}"
        )
        .into());
    }
    let workload = HeldWorkload {
        course: request.course.clone(),
        chords: request.chords,
    };
    let config = CampaignConfig::<HeldWorkload> {
        campaign_seed: request.seed,
        workers: request.workers,
        execution_budget: request.work_budget,
        host: "tiny-held-world".into(),
        wall_budget: None,
        stop_rollout_on_objective: true,
        stop_campaign_on_objective: true,
        archive_entry_limit: MAX_ARCHIVE_ENTRIES,
        window: default_window(request.workers),
        memory_budget_mib: Some(256),
        materialize_final_artifacts: true,
        run: (),
        suffix: suffix_shape_from_identifier(&request.suffix)?,
        mixture: draw_mixture_from_identifier(&request.mixture)?,
        retention: RetentionPolicy::Unprobed,
        objective_witness_path: None,
    };
    let options = || CampaignExecutionOptions {
        work_budget: Some(request.work_budget),
        ..Default::default()
    };
    let mut stream = StreamCounts {
        kept: request.verify.then(Vec::new),
        ..StreamCounts::default()
    };
    let started = telemetry_now();
    let live = run_campaign_checkpointed_with_options(
        &workload,
        &config,
        &CampaignOrigin::Genesis,
        &mut stream,
        None,
        options(),
    )?;
    let elapsed = started.elapsed().as_secs_f64();
    if let Some(kept) = &stream.kept {
        let replay = replay_campaign_checkpointed(&workload, kept, None, None)?;
        if replay != live {
            return Err("held-world replay mismatch".into());
        }
    }
    let report = live.0;
    if let Some(witness) = &report.objective_witness {
        let mut state = State::default();
        for chord in &witness.actions {
            for _ in 0..chord.hold {
                state = workload.course.step(state, chord.buttons);
            }
        }
        if !state.goal {
            return Err("invalid objective witness".into());
        }
    }
    let StreamCounts {
        jobs,
        skips,
        decisions,
        ..
    } = stream;
    let per_thousand = |count: u64| count as f64 * 1000.0 / report.execution_work.max(1) as f64;
    Ok(serde_json::json!({
        "seed": request.seed,
        "suffix": request.suffix,
        "mixture": request.mixture,
        "chords": request.chords,
        "work_budget": request.work_budget,
        "work": report.execution_work,
        "first_objective_work": report.work_to_first_objective,
        "success": report.work_to_first_objective.is_some(),
        "first_section_work": report.archive.evidence.first_section_work,
        "falls": report.archive.evidence.falls,
        "jobs": jobs,
        "skips": skips,
        "retained": decisions[0],
        "duplicate": decisions[1],
        "rejected": decisions[2],
        "retained_per_thousand": per_thousand(decisions[0]),
        "rejected_per_thousand": per_thousand(decisions[2]),
        "live_entries": report.live_entries,
        "input_index_nodes": report.input_index_nodes,
        "resident_memory_bytes": report.resident_memory_bytes,
        "history_memory_bytes": report.history_memory_bytes,
        "witness_actions": report.objective_witness.as_ref().map(|w| w.actions.len()),
        "elapsed_seconds": elapsed,
        "verified": request.verify,
    }))
}

#[cfg(test)]
mod tests {
    use super::{A, B, ChordDraw, Course, RIGHT, Request, State, change_one_control, run};
    use searcher::search::rand::RomuDuoJrRand;

    fn course() -> Course {
        Course {
            ground_px: 160,
            pits_px: vec![16, 32, 48, 64, 80, 96, 104, 112],
        }
    }

    fn hold(course: &Course, mut state: State, buttons: u8, frames: u32) -> State {
        for _ in 0..frames {
            state = course.step(state, buttons);
        }
        state
    }

    #[test]
    fn a_running_jump_clears_a_pit_that_a_walking_jump_falls_into() {
        let course = Course {
            ground_px: 160,
            pits_px: vec![96],
        };
        let walked = hold(&course, State::default(), RIGHT, 100);
        let walked = hold(&course, walked, RIGHT | A, 60);
        assert_eq!(walked.falls, 1);
        let ran = hold(&course, State::default(), RIGHT | B, 79);
        let ran = hold(&course, ran, RIGHT | B | A, 60);
        assert_eq!(ran.falls, 0);
        assert!(ran.x / 16 > 256);
    }

    #[test]
    fn the_widest_course_is_finishable() {
        let course = course();
        let mut state = State::default();
        for section in 0..course.pits_px.len() {
            let pit = course.section_px(section) + i32::from(course.ground_px);
            while state.x / 16 + 12 < pit {
                state = course.step(state, RIGHT | B);
            }
            state = hold(&course, state, RIGHT | B | A, 64);
            state = hold(&course, state, RIGHT | B, 1);
            assert_eq!(state.falls, 0, "section {section}");
        }
        let state = hold(&course, state, RIGHT | B, 200);
        assert!(state.goal);
    }

    #[test]
    fn a_new_jump_needs_a_new_press() {
        let course = course();
        let state = hold(&course, State::default(), A, 120);
        assert_eq!(state.y, 0);
        let state = hold(&course, state, 0, 1);
        let state = hold(&course, state, A, 1);
        assert!(state.y > 0);
    }

    #[test]
    fn a_control_change_moves_one_control() {
        let mut rand = RomuDuoJrRand::with_seed(7);
        for previous in [0x00, 0x83, 0x51, 0xa2] {
            for _ in 0..200 {
                let next = change_one_control(&mut rand, previous).expect("change");
                let direction = next & 0xf0 != previous & 0xf0;
                let buttons = (next ^ previous) & 0x0f;
                assert!(
                    (direction && buttons == 0) || (!direction && [0, A, B].contains(&buttons))
                );
            }
        }
    }

    #[test]
    fn both_chord_draws_replay_their_own_streams() {
        for chords in [ChordDraw::ChangeOneControl, ChordDraw::Fresh] {
            let report = run(&Request {
                course: Course {
                    ground_px: 64,
                    pits_px: vec![16, 32],
                },
                seed: 11,
                work_budget: 200_000,
                workers: 2,
                suffix: "one_to_six_within_3_max_action_cost_full_hold".into(),
                mixture: "energy_splice:6".into(),
                chords,
                verify: true,
            })
            .expect("a verified run replays");
            assert_eq!(report["verified"], true);
            assert!(report["jobs"].as_u64().is_some_and(|jobs| jobs > 0));
        }
    }
}
