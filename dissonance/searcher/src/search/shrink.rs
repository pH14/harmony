// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    error::Error,
    fs::File,
    io::{self, BufWriter, Write},
    path::PathBuf,
    sync::Mutex,
};

use serde::{Deserialize, Serialize};

pub mod window;

use super::{
    archive::{Input, MAX_ARCHIVE_ENTRIES, RetentionPolicy},
    campaign::{
        CampaignConfig, CampaignCoordinator, CampaignExecutionOptions, CampaignOrigin,
        CampaignWatch, JobOutcome, JobSpec, MinimizeConfig, MinimizeReport, WatchedJob, Workload,
        default_window, run_campaign_watched,
    },
    cost::ShrinkCost,
    draw::DrawMixture,
    rollout::classify,
};

pub trait ShrinkPolicy: Workload {
    fn window_actions(&self, run: &Self::Run) -> Vec<Self::Action> {
        let _ = run;
        Vec::new()
    }

    fn window_fingerprint(&self, target: &Self::Target) -> Option<u64> {
        let _ = target;
        None
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Shortcut,
    Ordinary,
    Continuation,
}

impl JobKind {
    pub(crate) const ALL: [Self; 3] = [Self::Shortcut, Self::Ordinary, Self::Continuation];

    pub(crate) const fn index(self) -> usize {
        match self {
            Self::Shortcut => 0,
            Self::Ordinary => 1,
            Self::Continuation => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct KindCounts {
    pub shortcut: u64,
    pub ordinary: u64,
    pub continuation: u64,
}

impl KindCounts {
    pub(crate) fn add(&mut self, kind: JobKind) {
        let count = match kind {
            JobKind::Shortcut => &mut self.shortcut,
            JobKind::Ordinary => &mut self.ordinary,
            JobKind::Continuation => &mut self.continuation,
        };
        *count = count.saturating_add(1);
    }

    #[must_use]
    pub const fn total(&self) -> u64 {
        self.shortcut
            .saturating_add(self.ordinary)
            .saturating_add(self.continuation)
    }
}

pub(crate) const CONTINUATION_QUEUE_LIMIT: usize = 4_096;

#[derive(Clone, Debug)]
pub struct ShrinkConfig<R> {
    pub run: R,
    pub seed: u64,
    pub root: usize,
    pub workers: usize,
    pub window: Option<usize>,
    pub energy_scale: u64,
    pub work_budget: Option<u64>,
    pub job_budget: Option<u64>,
    pub until_shortcuts_exhausted: bool,
    pub primary_shortcuts: bool,
    pub ordinary: bool,
    pub ordinary_length: Option<u8>,
    pub shortcut_snapshots: bool,
    pub mixture: DrawMixture,
    pub archive_entry_limit: usize,
    pub memory_budget_mib: Option<usize>,
    pub stream: Option<PathBuf>,
}

impl<R> ShrinkConfig<R> {
    pub const fn new(run: R, seed: u64) -> Self {
        Self {
            run,
            seed,
            root: 0,
            workers: 1,
            window: None,
            energy_scale: 6,
            work_budget: None,
            job_budget: None,
            until_shortcuts_exhausted: false,
            primary_shortcuts: false,
            ordinary: true,
            ordinary_length: Some(64),
            shortcut_snapshots: false,
            mixture: DrawMixture::AlphabetOnly,
            archive_entry_limit: MAX_ARCHIVE_ENTRIES,
            memory_budget_mib: None,
            stream: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Improvement {
    pub stage: Option<u32>,
    pub job: u64,
    pub kind: JobKind,
    pub cost: ShrinkCost,
    pub actions: usize,
    pub execution_work: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShrinkStop {
    WorkBudget,
    JobBudget,
    ShortcutsExhausted,
    LowerBound,
    NoArrival,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ShrinkProgress {
    pub jobs: KindCounts,
    pub accepted: KindCounts,
    pub cost: Option<ShrinkCost>,
    pub initial_cost: Option<ShrinkCost>,
    pub lower_bound: ShrinkCost,
    pub execution_work: u64,
    pub pending_continuations: usize,
    pub shortcut_window: usize,
    pub shortcut_position: usize,
}

impl ShrinkProgress {
    fn of(
        report: Option<&MinimizeReport>,
        executions: u64,
        cost: Option<ShrinkCost>,
        initial_cost: Option<ShrinkCost>,
        execution_work: u64,
    ) -> Self {
        let mut jobs = report.map(|report| report.jobs).unwrap_or_default();
        jobs.ordinary = executions
            .saturating_sub(jobs.shortcut)
            .saturating_sub(jobs.continuation);
        Self {
            jobs,
            accepted: report.map(|report| report.accepted).unwrap_or_default(),
            cost,
            initial_cost,
            lower_bound: report.map(|report| report.floor).unwrap_or_default(),
            execution_work,
            pending_continuations: report.map_or(0, |report| report.pending_continuations),
            shortcut_window: report.map_or(0, |report| report.shortcut_window),
            shortcut_position: report.map_or(0, |report| report.shortcut_position),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ShrinkOutcome<A: Ord> {
    pub input: Input<A>,
    pub progress: ShrinkProgress,
    pub improvements: Vec<Improvement>,
    pub stop: ShrinkStop,
}

pub enum ShrinkEvent<'a, A: Ord> {
    Improved {
        input: &'a Input<A>,
        improvement: &'a Improvement,
    },
    Progress {
        progress: &'a ShrinkProgress,
    },
}

struct Observed<F> {
    observe: F,
    initial_cost: Option<ShrinkCost>,
    improvements: Vec<Improvement>,
}

struct ShrinkWatch<F> {
    observed: Mutex<Observed<F>>,
    stage: Option<u32>,
    work_offset: u64,
}

impl<F> ShrinkWatch<F> {
    fn observed(&self) -> Result<std::sync::MutexGuard<'_, Observed<F>>, Box<dyn Error>> {
        self.observed
            .lock()
            .map_err(|_| "the shrink observer lock was poisoned".into())
    }
}

impl<G, F> CampaignWatch<G> for ShrinkWatch<F>
where
    G: Workload,
    G::ArchiveReport: Serialize,
    F: FnMut(ShrinkEvent<'_, G::Action>) -> Result<(), Box<dyn Error>> + Send,
{
    type Note = ();

    fn run(
        &self,
        _job: &JobSpec<G>,
        run: impl FnOnce() -> Result<JobOutcome<G>, Box<dyn Error>>,
    ) -> WatchedJob<G, ()> {
        Ok((run()?, ()))
    }

    fn start(&self, campaign: &mut CampaignCoordinator<'_, G>) -> Result<(), Box<dyn Error>> {
        campaign.watch_incumbents();
        self.observed()?.initial_cost = campaign.incumbent_cost();
        Ok(())
    }

    fn admitted(
        &self,
        (): (),
        campaign: &mut CampaignCoordinator<'_, G>,
    ) -> Result<(), Box<dyn Error>> {
        let mut observed = self.observed()?;
        let observed = &mut *observed;
        for incumbent in campaign.take_incumbents() {
            let improvement = Improvement {
                stage: self.stage,
                job: incumbent.sequence,
                kind: incumbent.kind,
                cost: incumbent.record.cost,
                actions: incumbent.record.actions,
                execution_work: incumbent.execution_work.saturating_add(self.work_offset),
            };
            observed.improvements.push(improvement);
            (observed.observe)(ShrinkEvent::Improved {
                input: &incumbent.input,
                improvement: &improvement,
            })?;
        }
        let progress = ShrinkProgress::of(
            campaign.minimize_progress().as_ref(),
            campaign.executions(),
            campaign.incumbent_cost(),
            observed.initial_cost,
            campaign.execution_work().saturating_add(self.work_offset),
        );
        (observed.observe)(ShrinkEvent::Progress {
            progress: &progress,
        })
    }
}

struct Walk {
    stages: Vec<Option<u32>>,
    costs: Vec<ShrinkCost>,
    arrival: Option<usize>,
}

impl Walk {
    fn end(&self) -> usize {
        self.arrival.unwrap_or(self.costs.len() - 1)
    }
}

fn walk<G: Workload>(
    workload: &G,
    run: &G::Run,
    target: &mut G::Target,
    tape: &Input<G::Action>,
) -> Result<Walk, Box<dyn Error>> {
    workload.reset(target);
    let mut milestones = G::Milestones::default();
    let mut walk = Walk {
        stages: vec![workload.stage(run, target)],
        costs: vec![ShrinkCost::ZERO],
        arrival: None,
    };
    let mut cost = ShrinkCost::ZERO;
    for action in &tape.actions {
        workload.apply_action(target, action, &mut milestones)?;
        cost = cost + workload.shrink_cost(action, target, &workload.rollout_observations(target));
        walk.stages.push(workload.stage(run, target));
        walk.costs.push(cost);
        let outcome = classify(workload, run, target)?;
        if outcome.objective_reached {
            walk.arrival = Some(walk.costs.len() - 1);
            break;
        }
        if outcome.disposition.is_terminal() {
            break;
        }
    }
    Ok(walk)
}

fn share(budget: Option<u64>, part: u64, whole: u64) -> Option<u64> {
    budget.map(|budget| {
        if whole == 0 || part >= whole {
            return budget;
        }
        u64::try_from(u128::from(budget) * u128::from(part) / u128::from(whole))
            .unwrap_or(budget)
            .max(budget.min(1))
    })
}

fn segments_left<G: Workload>(workload: &G, run: &G::Run, stage: Option<u32>) -> u64 {
    let Some(stage) = stage else {
        return 1;
    };
    let later = (stage.saturating_add(1)..)
        .take_while(|&later| workload.stage_run(run, later).is_some())
        .count();
    u64::try_from(later).unwrap_or(u64::MAX).saturating_add(1)
}

fn segment_stream(stream: Option<&PathBuf>, stage: Option<u32>) -> Option<PathBuf> {
    let stream = stream?;
    let Some(stage) = stage else {
        return Some(stream.clone());
    };
    let mut name = stream.file_stem().unwrap_or_default().to_os_string();
    name.push(format!("-stage-{stage}"));
    if let Some(extension) = stream.extension() {
        name.push(".");
        name.push(extension);
    }
    Some(stream.with_file_name(name))
}

pub fn shrink<G>(
    workload: &G,
    config: &ShrinkConfig<G::Run>,
    tape: &Input<G::Action>,
    mut observe: impl FnMut(ShrinkEvent<'_, G::Action>) -> Result<(), Box<dyn Error>> + Send,
) -> Result<ShrinkOutcome<G::Action>, Box<dyn Error>>
where
    G: ShrinkPolicy,
    G::ArchiveReport: Serialize,
{
    if workload.stage_run(&config.run, 0).is_none() {
        let segment = SegmentConfig {
            run: &config.run,
            root: config.root,
            stage: None,
            work_budget: config.work_budget,
            job_budget: config.job_budget,
            stream: config.stream.clone(),
            work_offset: 0,
        };
        return shrink_segment(workload, config, tape, &segment, &mut observe);
    }
    let mut target = workload.new_target()?;
    let original = tape;
    let mut tape = tape.clone();
    let mut root = config.root;
    let first = walk(workload, &config.run, &mut target, &tape)?;
    let initial_cost = first.arrival.map(|arrival| first.costs[arrival]);
    let mut work_budget = config.work_budget;
    let mut job_budget = config.job_budget;
    let mut jobs = KindCounts::default();
    let mut accepted = KindCounts::default();
    let mut improvements = Vec::new();
    let mut execution_work = 0_u64;
    let mut last: Option<(ShrinkStop, ShrinkProgress)> = None;
    loop {
        let walked = walk(workload, &config.run, &mut target, &tape)?;
        if let Some((last_stop, last_progress)) = &last {
            let stop = if walked.arrival.is_some_and(|arrival| root >= arrival) {
                Some(*last_stop)
            } else if work_budget == Some(0) {
                Some(ShrinkStop::WorkBudget)
            } else if job_budget == Some(0) {
                Some(ShrinkStop::JobBudget)
            } else {
                None
            };
            if let Some(stop) = stop {
                let cost = walked.arrival.map(|arrival| walked.costs[arrival]);
                let regressed =
                    initial_cost.is_some_and(|initial| cost.is_none_or(|cost| cost > initial));
                let progress = ShrinkProgress {
                    jobs,
                    accepted,
                    initial_cost,
                    execution_work,
                    cost: if regressed { initial_cost } else { cost },
                    ..last_progress.clone()
                };
                return Ok(ShrinkOutcome {
                    input: if regressed { original.clone() } else { tape },
                    progress,
                    improvements,
                    stop,
                });
            }
        }
        if root > walked.end() {
            return Err("the shrink root is past the tape's objective, death or end".into());
        }
        let stage = walked.stages[root];
        let next = match stage.and_then(|stage| workload.stage_run(&config.run, stage + 1)) {
            Some(run) => {
                let position = walk(workload, &run, &mut target, &tape)?
                    .arrival
                    .filter(|&position| position > root && position <= walked.end());
                Some((position, run))
            }
            None => None,
        };
        let whole = walked.costs[walked.end()].primary - walked.costs[root].primary;
        let (run, part, whole) = match &next {
            Some((Some(position), run)) if walked.arrival.is_some() => (
                run.clone(),
                walked.costs[*position].primary - walked.costs[root].primary,
                whole,
            ),
            Some((_, run)) => (run.clone(), 1, segments_left(workload, &config.run, stage)),
            None => (config.run.clone(), 1, 1),
        };
        let segment = SegmentConfig {
            run: &run,
            root,
            stage,
            work_budget: share(work_budget, part, whole),
            job_budget: share(job_budget, part, whole),
            stream: segment_stream(config.stream.as_ref(), stage),
            work_offset: execution_work,
        };
        let outcome = shrink_segment(workload, config, &tape, &segment, &mut observe)?;
        jobs.shortcut += outcome.progress.jobs.shortcut;
        jobs.ordinary += outcome.progress.jobs.ordinary;
        jobs.continuation += outcome.progress.jobs.continuation;
        accepted.shortcut += outcome.progress.accepted.shortcut;
        accepted.ordinary += outcome.progress.accepted.ordinary;
        accepted.continuation += outcome.progress.accepted.continuation;
        let spent = outcome.progress.execution_work;
        execution_work = execution_work.saturating_add(spent);
        work_budget = work_budget.map(|budget| budget.saturating_sub(spent));
        job_budget = job_budget.map(|budget| budget.saturating_sub(outcome.progress.jobs.total()));
        improvements.extend_from_slice(&outcome.improvements);
        last = Some((outcome.stop, outcome.progress.clone()));
        let Some(&(position, _)) = next
            .as_ref()
            .filter(|_| outcome.stop != ShrinkStop::NoArrival)
        else {
            let input = if next.is_some() { tape } else { outcome.input };
            let regressed = initial_cost
                .is_some_and(|initial| outcome.progress.cost.is_none_or(|cost| cost > initial));
            let progress = ShrinkProgress {
                jobs,
                accepted,
                initial_cost,
                execution_work,
                cost: if regressed {
                    initial_cost
                } else {
                    outcome.progress.cost
                },
                ..outcome.progress
            };
            return Ok(ShrinkOutcome {
                input: if regressed { original.clone() } else { input },
                progress,
                improvements,
                stop: outcome.stop,
            });
        };
        root = outcome.input.actions.len();
        let mut actions = outcome.input.actions;
        if let Some(position) = position {
            actions.extend_from_slice(&tape.actions[position..]);
        }
        tape = Input { actions };
    }
}

struct SegmentConfig<'a, R> {
    run: &'a R,
    root: usize,
    stage: Option<u32>,
    work_budget: Option<u64>,
    job_budget: Option<u64>,
    stream: Option<PathBuf>,
    work_offset: u64,
}

fn shrink_segment<G>(
    workload: &G,
    config: &ShrinkConfig<G::Run>,
    tape: &Input<G::Action>,
    segment: &SegmentConfig<'_, G::Run>,
    observe: impl FnMut(ShrinkEvent<'_, G::Action>) -> Result<(), Box<dyn Error>> + Send,
) -> Result<ShrinkOutcome<G::Action>, Box<dyn Error>>
where
    G: ShrinkPolicy,
    G::ArchiveReport: Serialize,
{
    let workers = u32::try_from(config.workers.max(1))?;
    let campaign = CampaignConfig {
        campaign_seed: config.seed,
        workers,
        execution_budget: segment.job_budget.unwrap_or(u64::MAX),
        host: "shrink".to_owned(),
        wall_budget: None,
        stop_rollout_on_objective: true,
        stop_campaign_on_objective: true,
        archive_entry_limit: config.archive_entry_limit,
        window: config
            .window
            .unwrap_or_else(|| default_window(workers))
            .max(1),
        memory_budget_mib: config.memory_budget_mib,
        materialize_final_artifacts: false,
        run: segment.run.clone(),
        mixture: config.mixture,
        retention: RetentionPolicy::Unprobed,
        objective_witness_path: None,
        minimize: Some(MinimizeConfig {
            energy_scale: config.energy_scale,
            ordinary: config.ordinary,
            primary_shortcuts: config.primary_shortcuts,
            shortcut_snapshots: config.shortcut_snapshots,
            until_shortcuts_exhausted: config.until_shortcuts_exhausted,
            ordinary_length: config.ordinary_length,
        }),
    };
    let origin = CampaignOrigin::Tape {
        input: tape.clone(),
        root: segment.root,
    };
    let watch = ShrinkWatch {
        observed: Mutex::new(Observed {
            observe,
            initial_cost: None,
            improvements: Vec::new(),
        }),
        stage: segment.stage,
        work_offset: segment.work_offset,
    };
    let mut stream: Box<dyn Write> = match &segment.stream {
        Some(path) => Box::new(BufWriter::new(File::create(path)?)),
        None => Box::new(io::sink()),
    };
    let (report, _) = run_campaign_watched(
        workload,
        &campaign,
        &origin,
        &mut *stream,
        None,
        CampaignExecutionOptions {
            work_budget: segment.work_budget,
            ..CampaignExecutionOptions::default()
        },
        &watch,
    )?;
    stream.flush()?;
    let Observed {
        initial_cost,
        improvements,
        ..
    } = watch
        .observed
        .into_inner()
        .map_err(|_| "the shrink observer lock was poisoned")?;
    let stop = match report.incumbent_cost {
        None => ShrinkStop::NoArrival,
        Some(_) => report
            .shrink_stop
            .ok_or("the shrink campaign ended without a stop")?,
    };
    let progress = ShrinkProgress::of(
        report.minimize_progress.as_ref(),
        report.executions_completed,
        report.incumbent_cost,
        initial_cost,
        report.execution_work,
    );
    Ok(ShrinkOutcome {
        input: report.objective_witness.unwrap_or_else(|| tape.clone()),
        progress,
        improvements,
        stop,
    })
}
