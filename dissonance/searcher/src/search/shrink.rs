// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    error::Error,
    num::{NonZeroU64, NonZeroUsize},
    ops::Add,
    sync::{Arc, Mutex, mpsc},
};

use serde::{Deserialize, Serialize};

pub mod window;

use super::{
    archive::{ArchiveKey, Input, Slot, slot_of_key},
    campaign::Workload,
    draw::MixtureEnergy,
    rand::RomuDuoJrRand,
    rollout::classify,
};

#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
pub struct ShrinkCost {
    pub primary: u64,
    pub secondary: u64,
}

impl ShrinkCost {
    pub const ZERO: Self = Self {
        primary: 0,
        secondary: 0,
    };

    #[must_use]
    pub const fn new(primary: u64, secondary: u64) -> Self {
        Self { primary, secondary }
    }

    #[must_use]
    pub const fn saving_over(self, cheaper: Self) -> (u64, u64) {
        (
            self.primary.saturating_sub(cheaper.primary),
            self.secondary.saturating_sub(cheaper.secondary),
        )
    }
}

impl Add for ShrinkCost {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self {
            primary: self.primary.saturating_add(other.primary),
            secondary: self.secondary.saturating_add(other.secondary),
        }
    }
}

pub trait ShrinkPolicy: Workload {
    fn shrink_cost(&self, action: &Self::Action, target: &Self::Target) -> ShrinkCost {
        let _ = target;
        ShrinkCost::new((self.action_cost_fn())(action), 0)
    }

    fn simpler_actions(&self, run: &Self::Run, action: &Self::Action) -> Vec<Self::Action> {
        let _ = (run, action);
        Vec::new()
    }

    fn remaining_cost_bound(&self, run: &Self::Run, target: &Self::Target) -> ShrinkCost {
        let _ = (run, target);
        ShrinkCost::ZERO
    }

    fn window_actions(&self, run: &Self::Run) -> Vec<Self::Action> {
        let _ = run;
        Vec::new()
    }

    fn window_fingerprint(&self, target: &Self::Target) -> Option<u64> {
        let _ = target;
        None
    }

    fn goal_tail(&self, run: &Self::Run, last: Option<&Self::Action>) -> Vec<Self::Action> {
        let _ = (run, last);
        Vec::new()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Shortcut,
    Redraw,
    Continuation,
}

impl JobKind {
    const ALL: [Self; 3] = [Self::Shortcut, Self::Redraw, Self::Continuation];

    const fn index(self) -> usize {
        match self {
            Self::Shortcut => 0,
            Self::Redraw => 1,
            Self::Continuation => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct KindCounts {
    pub shortcut: u64,
    pub redraw: u64,
    pub continuation: u64,
}

impl KindCounts {
    fn add(&mut self, kind: JobKind) {
        let count = match kind {
            JobKind::Shortcut => &mut self.shortcut,
            JobKind::Redraw => &mut self.redraw,
            JobKind::Continuation => &mut self.continuation,
        };
        *count = count.saturating_add(1);
    }

    #[must_use]
    pub const fn total(&self) -> u64 {
        self.shortcut
            .saturating_add(self.redraw)
            .saturating_add(self.continuation)
    }
}

#[derive(Clone, Debug)]
pub struct ShrinkConfig<R> {
    pub run: R,
    pub seed: u64,
    pub workers: usize,
    pub batch: usize,
    pub redraw_length: usize,
    pub kept_limit: usize,
    pub energy_scale: u64,
    pub work_budget: Option<u64>,
    pub job_budget: Option<u64>,
    pub until_shortcuts_exhausted: bool,
    pub primary_shortcuts: bool,
    pub redraws: bool,
    pub shortcut_states: bool,
}

impl<R> ShrinkConfig<R> {
    pub const fn new(run: R, seed: u64) -> Self {
        Self {
            run,
            seed,
            workers: 1,
            batch: 32,
            redraw_length: 64,
            kept_limit: 4_096,
            energy_scale: 6,
            work_budget: None,
            job_budget: None,
            until_shortcuts_exhausted: false,
            primary_shortcuts: false,
            redraws: true,
            shortcut_states: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Improvement {
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
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ShrinkProgress {
    pub batches: u64,
    pub jobs: KindCounts,
    pub accepted: KindCounts,
    pub rebuilds: u64,
    pub cost: ShrinkCost,
    pub initial_cost: ShrinkCost,
    pub lower_bound: ShrinkCost,
    pub execution_work: u64,
    pub kept_states: usize,
    pub pending_continuations: usize,
    pub shortcut_window: usize,
    pub shortcut_position: usize,
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
    Batch {
        progress: &'a ShrinkProgress,
    },
}

type Lineage<G> = <<G as super::campaign::CampaignTypes>::Key as ArchiveKey>::Lineage;

type StartState<G> = (
    Arc<<G as super::campaign::CampaignTypes>::Snapshot>,
    ShrinkCost,
    <G as super::campaign::CampaignTypes>::Key,
    Lineage<G>,
    <G as super::campaign::CampaignTypes>::Milestones,
);

type Accepted<G> = Option<(
    Vec<<G as super::campaign::CampaignTypes>::Action>,
    Improvement,
    Option<usize>,
    Option<(usize, Vec<Endpoint<G>>)>,
)>;

struct Chain<G: Workload> {
    actions: Vec<G::Action>,
    costs: Vec<ShrinkCost>,
    snapshots: Vec<Arc<G::Snapshot>>,
    keys: Vec<G::Key>,
    lineages: Vec<Lineage<G>>,
    milestones: Vec<G::Milestones>,
    slots: BTreeMap<Slot<G::Key>, usize>,
}

impl<G: Workload> Chain<G> {
    fn cost(&self) -> ShrinkCost {
        self.costs.last().copied().unwrap_or_default()
    }

    fn truncate(&mut self, keep: usize) {
        self.actions.truncate(keep);
        self.costs.truncate(keep + 1);
        self.snapshots.truncate(keep + 1);
        self.keys.truncate(keep + 1);
        self.lineages.truncate(keep + 1);
        self.milestones.truncate(keep + 1);
        self.slots = self
            .keys
            .iter()
            .enumerate()
            .map(|(index, key)| (slot_of_key(*key), index))
            .collect();
    }

    fn push(&mut self, action: G::Action, cost: ShrinkCost, state: State<G>) {
        self.actions.push(action);
        self.costs.push(cost);
        self.push_state(state);
    }

    fn push_state(&mut self, state: State<G>) {
        let index = self.snapshots.len();
        self.slots.insert(slot_of_key(state.key), index);
        self.snapshots.push(state.snapshot);
        self.keys.push(state.key);
        self.lineages.push(state.lineage);
        self.milestones.push(state.milestones);
    }
}

struct State<G: Workload> {
    snapshot: Arc<G::Snapshot>,
    key: G::Key,
    lineage: Lineage<G>,
    milestones: G::Milestones,
}

struct Kept<G: Workload> {
    state: State<G>,
    prefix: Vec<G::Action>,
    cost: ShrinkCost,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Start {
    Chain(usize),
    Kept(usize),
}

struct JobSpec<G: Workload> {
    kind: JobKind,
    start: Start,
    snapshot: Arc<G::Snapshot>,
    cost: ShrinkCost,
    key: G::Key,
    lineage: Lineage<G>,
    milestones: G::Milestones,
    actions: Vec<G::Action>,
    snapshot_endpoints: usize,
    incumbent_cost: ShrinkCost,
}

struct Endpoint<G: Workload> {
    cost: ShrinkCost,
    key: G::Key,
    bound: ShrinkCost,
    kept: Option<(G::Snapshot, Lineage<G>, G::Milestones)>,
}

struct JobResult<G: Workload> {
    endpoints: Vec<Endpoint<G>>,
    goal: Option<(usize, ShrinkCost)>,
    work: u64,
}

fn execute<G: ShrinkPolicy>(
    workload: &G,
    run: &G::Run,
    target: &mut G::Target,
    spec: &JobSpec<G>,
) -> Result<JobResult<G>, Box<dyn Error>> {
    workload.reset(target);
    workload.restore(target, &spec.snapshot)?;
    let started = workload.execution_work(target);
    let mut cost = spec.cost;
    let mut previous = spec.key;
    let mut lineage = spec.lineage.clone();
    let mut milestones = spec.milestones;
    let mut endpoints = Vec::new();
    let mut goal = None;
    for (index, action) in spec.actions.iter().enumerate() {
        let keep = index < spec.snapshot_endpoints;
        let captured = if keep {
            workload.apply_action_and_snapshot(target, action, &mut milestones)?
        } else {
            workload.apply_action(target, action, &mut milestones)?;
            None
        };
        cost = cost + workload.shrink_cost(action, target);
        let outcome = classify(workload, run, target)?;
        if outcome.disposition.is_failed() {
            return Err("a shrink replay failed to execute".into());
        }
        if outcome.objective_reached {
            goal = Some((index, cost));
            break;
        }
        if outcome.disposition.is_terminal() {
            break;
        }
        let key = workload
            .current_key(target)?
            .complete(Some((previous, &lineage)));
        <G::Key as ArchiveKey>::record(&mut lineage, key);
        previous = key;
        let bound = workload.remaining_cost_bound(run, target);
        let kept = if keep {
            let snapshot = match captured {
                Some(snapshot) => snapshot,
                None => workload.snapshot(target)?,
            };
            Some((snapshot, lineage.clone(), milestones))
        } else {
            None
        };
        endpoints.push(Endpoint {
            cost,
            key,
            bound,
            kept,
        });
        if cost + bound >= spec.incumbent_cost {
            break;
        }
    }
    Ok(JobResult {
        endpoints,
        goal,
        work: workload.execution_work(target).saturating_sub(started),
    })
}

#[derive(Clone, Copy, Debug, Default)]
struct Schedule {
    size: usize,
    position: usize,
    variant: usize,
    accepted_in_pass: bool,
    exhausted: bool,
}

enum Fill<A> {
    Delete,
    Replace(Vec<A>),
}

fn proposal<A: Copy>(actions: &[A], indices: &[usize], fill: Fill<A>) -> Vec<A> {
    let start = indices[0];
    let stop = indices[indices.len() - 1] + 1;
    let mut replaced = match fill {
        Fill::Delete => None,
        Fill::Replace(replaced) => Some(replaced.into_iter()),
    };
    let mut proposal = Vec::with_capacity(actions.len() - start);
    let mut window = indices.iter().peekable();
    for (index, action) in actions.iter().enumerate().take(stop).skip(start) {
        if window.next_if(|&&next| next == index).is_none() {
            proposal.push(*action);
        } else if let Some(replacement) = replaced.as_mut().and_then(Iterator::next) {
            proposal.push(replacement);
        }
    }
    proposal.extend_from_slice(&actions[stop..]);
    proposal
}

impl Schedule {
    fn fills<G: ShrinkPolicy>(
        workload: &G,
        run: &G::Run,
        window: &[G::Action],
        primary: bool,
    ) -> Vec<Fill<G::Action>> {
        let mut fills = Vec::new();
        if let [action] = window
            && !primary
        {
            for simpler in workload.simpler_actions(run, action) {
                fills.push(Fill::Replace(vec![simpler]));
            }
        } else {
            let mut changed = false;
            let replaced = window
                .iter()
                .map(
                    |action| match workload.simpler_actions(run, action).first() {
                        Some(simpler) => {
                            changed = true;
                            *simpler
                        }
                        None => *action,
                    },
                )
                .collect::<Vec<_>>();
            if changed {
                fills.push(Fill::Replace(replaced));
            }
        }
        if !(primary && !fills.is_empty()) {
            fills.push(Fill::Delete);
        }
        fills
    }

    fn next<G: ShrinkPolicy>(
        &mut self,
        workload: &G,
        run: &G::Run,
        actions: &[G::Action],
        eligible: &[usize],
        primary: bool,
    ) -> Option<(usize, Vec<G::Action>)> {
        let length = eligible.len();
        loop {
            if self.exhausted || length == 0 {
                self.exhausted = true;
                return None;
            }
            if self.size == 0 {
                self.size = length.div_ceil(2).max(1);
            }
            self.size = self.size.min(length);
            if self.position >= length {
                if self.size == 1 {
                    if self.accepted_in_pass {
                        self.accepted_in_pass = false;
                        self.position = 0;
                        self.variant = 0;
                        continue;
                    }
                    self.exhausted = true;
                    return None;
                }
                self.size = self.size.div_ceil(2);
                self.position = 0;
                self.variant = 0;
                continue;
            }
            let end = (self.position + self.size).min(length);
            let indices = &eligible[self.position..end];
            let window = indices
                .iter()
                .map(|&index| actions[index])
                .collect::<Vec<_>>();
            let fills = Self::fills(workload, run, &window, primary);
            let Some(fill) = fills.into_iter().nth(self.variant) else {
                self.variant = 0;
                self.position = end;
                continue;
            };
            self.variant += 1;
            let start = indices[0];
            let proposal = proposal(actions, indices, fill);
            return Some((start, proposal));
        }
    }

    fn restart_at(&mut self, position: usize) {
        self.position = position;
        self.variant = 0;
        self.accepted_in_pass = true;
        self.exhausted = false;
    }
}

struct Continuation<A> {
    start: Start,
    actions: Vec<A>,
}

struct Shrinker<'a, G: ShrinkPolicy> {
    workload: &'a G,
    config: &'a ShrinkConfig<G::Run>,
    target: G::Target,
    chain: Chain<G>,
    kept: Vec<Kept<G>>,
    kept_slots: BTreeMap<Slot<G::Key>, usize>,
    continuations: VecDeque<Continuation<G::Action>>,
    proposed: BTreeSet<(Start, usize)>,
    schedule: Schedule,
    energy: MixtureEnergy,
    rand: RomuDuoJrRand,
    progress: ShrinkProgress,
    improvements: Vec<Improvement>,
}

const CONTINUATION_QUEUE_LIMIT: usize = 4_096;
const KEPT_PARENT_ONE_IN: usize = 4;

impl<'a, G: ShrinkPolicy> Shrinker<'a, G>
where
    Lineage<G>: Send + Sync,
{
    fn new(
        workload: &'a G,
        config: &'a ShrinkConfig<G::Run>,
        incumbent: &Input<G::Action>,
    ) -> Result<Self, Box<dyn Error>> {
        let mut target = workload.new_target()?;
        workload.reset(&mut target);
        let snapshot = workload.snapshot(&mut target)?;
        let key = workload.current_key(&target)?.complete(None);
        let mut lineage = Lineage::<G>::default();
        <G::Key as ArchiveKey>::record(&mut lineage, key);
        let lower_bound = workload.remaining_cost_bound(&config.run, &target);
        let mut chain = Chain {
            actions: Vec::new(),
            costs: vec![ShrinkCost::ZERO],
            snapshots: Vec::new(),
            keys: Vec::new(),
            lineages: Vec::new(),
            milestones: Vec::new(),
            slots: BTreeMap::new(),
        };
        chain.push_state(State {
            snapshot: Arc::new(snapshot),
            key,
            lineage,
            milestones: G::Milestones::default(),
        });
        let mut shrinker = Self {
            workload,
            config,
            target,
            chain,
            kept: Vec::new(),
            kept_slots: BTreeMap::new(),
            continuations: VecDeque::new(),
            proposed: BTreeSet::new(),
            schedule: Schedule::default(),
            energy: MixtureEnergy::default(),
            rand: RomuDuoJrRand::with_seed(config.seed),
            progress: ShrinkProgress {
                batches: 0,
                jobs: KindCounts::default(),
                accepted: KindCounts::default(),
                rebuilds: 0,
                cost: ShrinkCost::ZERO,
                initial_cost: ShrinkCost::ZERO,
                lower_bound,
                execution_work: 0,
                kept_states: 0,
                pending_continuations: 0,
                shortcut_window: 0,
                shortcut_position: 0,
            },
            improvements: Vec::new(),
        };
        let work = shrinker.extend_chain(&incumbent.actions)?;
        shrinker.progress.execution_work = work;
        shrinker.progress.cost = shrinker.chain.cost();
        shrinker.progress.initial_cost = shrinker.chain.cost();
        Ok(shrinker)
    }

    fn extend_chain(&mut self, actions: &[G::Action]) -> Result<u64, Box<dyn Error>> {
        let workload = self.workload;
        let run = &self.config.run;
        let index = self.chain.actions.len();
        workload.reset(&mut self.target);
        workload.restore(&mut self.target, &self.chain.snapshots[index])?;
        let started = workload.execution_work(&self.target);
        let mut cost = self.chain.costs[index];
        let mut previous = self.chain.keys[index];
        let mut lineage = self.chain.lineages[index].clone();
        let mut milestones = self.chain.milestones[index];
        for action in actions {
            let captured =
                workload.apply_action_and_snapshot(&mut self.target, action, &mut milestones)?;
            cost = cost + workload.shrink_cost(action, &self.target);
            let outcome = classify(workload, run, &self.target)?;
            if outcome.objective_reached {
                self.chain.actions.push(*action);
                self.chain.costs.push(cost);
                return Ok(workload
                    .execution_work(&self.target)
                    .saturating_sub(started));
            }
            if outcome.disposition.is_terminal() {
                return Err("the shrink incumbent ended before it reached the goal".into());
            }
            let snapshot = match captured {
                Some(snapshot) => snapshot,
                None => workload.snapshot(&mut self.target)?,
            };
            let key = workload
                .current_key(&self.target)?
                .complete(Some((previous, &lineage)));
            <G::Key as ArchiveKey>::record(&mut lineage, key);
            previous = key;
            self.chain.push(
                *action,
                cost,
                State {
                    snapshot: Arc::new(snapshot),
                    key,
                    lineage: lineage.clone(),
                    milestones,
                },
            );
        }
        Err("the shrink incumbent does not reach the goal".into())
    }

    fn start_state(&self, start: Start) -> StartState<G> {
        match start {
            Start::Chain(index) => (
                Arc::clone(&self.chain.snapshots[index]),
                self.chain.costs[index],
                self.chain.keys[index],
                self.chain.lineages[index].clone(),
                self.chain.milestones[index],
            ),
            Start::Kept(index) => {
                let kept = &self.kept[index];
                (
                    Arc::clone(&kept.state.snapshot),
                    kept.cost,
                    kept.state.key,
                    kept.state.lineage.clone(),
                    kept.state.milestones,
                )
            }
        }
    }

    fn spec(
        &self,
        kind: JobKind,
        start: Start,
        actions: Vec<G::Action>,
        snapshot_endpoints: usize,
    ) -> JobSpec<G> {
        let (snapshot, cost, key, lineage, milestones) = self.start_state(start);
        JobSpec {
            kind,
            start,
            snapshot,
            cost,
            key,
            lineage,
            milestones,
            actions,
            snapshot_endpoints,
            incumbent_cost: self.chain.cost(),
        }
    }

    fn draw_kind(&mut self) -> Result<JobKind, Box<dyn Error>> {
        let mut shares = self.energy.shares(self.config.energy_scale);
        if !self.config.redraws {
            shares[JobKind::Redraw.index()] = 0;
        }
        let total = shares.iter().sum::<u64>();
        let draw = self
            .rand
            .below_u64(NonZeroU64::new(total).ok_or("shrink job shares are empty")?);
        let mut floor = 0;
        for kind in JobKind::ALL {
            floor += shares[kind.index()];
            if draw < floor {
                return Ok(kind);
            }
        }
        Ok(JobKind::Redraw)
    }

    fn eligible(&self) -> Vec<usize> {
        let indices = 0..self.chain.actions.len();
        if !self.config.primary_shortcuts {
            return indices.collect();
        }
        indices
            .filter(|&index| self.chain.costs[index + 1].primary > self.chain.costs[index].primary)
            .collect()
    }

    fn shortcut(&mut self) -> Option<JobSpec<G>> {
        let workload = self.workload;
        let eligible = self.eligible();
        let (position, mut actions) = self.schedule.next(
            workload,
            &self.config.run,
            &self.chain.actions,
            &eligible,
            self.config.primary_shortcuts,
        )?;
        let tail = workload.goal_tail(&self.config.run, actions.last());
        actions.extend(tail);
        let snapshot_endpoints = if self.config.shortcut_states {
            actions.len()
        } else {
            0
        };
        Some(self.spec(
            JobKind::Shortcut,
            Start::Chain(position),
            actions,
            snapshot_endpoints,
        ))
    }

    fn redraw(&mut self) -> Result<JobSpec<G>, Box<dyn Error>> {
        let states = self.chain.actions.len();
        let start = if !self.kept.is_empty()
            && self
                .rand
                .below(NonZeroUsize::new(KEPT_PARENT_ONE_IN).ok_or("invalid kept odds")?)
                == 0
        {
            Start::Kept(
                self.rand
                    .below(NonZeroUsize::new(self.kept.len()).ok_or("no kept state")?),
            )
        } else {
            Start::Chain(
                self.rand
                    .below(NonZeroUsize::new(states).ok_or("the shrink incumbent is empty")?),
            )
        };
        let mut previous = match start {
            Start::Chain(0) => None,
            Start::Chain(index) => self.chain.actions.get(index - 1).copied(),
            Start::Kept(index) => self.kept[index].prefix.last().copied(),
        };
        let mut actions = Vec::with_capacity(self.config.redraw_length);
        for _ in 0..self.config.redraw_length.max(1) {
            let action = self.workload.sample_alphabet(
                &self.config.run,
                previous.as_ref(),
                &mut self.rand,
            )?;
            actions.push(action);
            previous = Some(action);
        }
        let length = actions.len();
        Ok(self.spec(JobKind::Redraw, start, actions, length))
    }

    fn plan_batch(&mut self) -> Result<Vec<JobSpec<G>>, Box<dyn Error>> {
        let mut batch = Vec::with_capacity(self.config.batch.max(1));
        while batch.len() < self.config.batch.max(1) {
            let kind = self.draw_kind()?;
            let spec = match kind {
                JobKind::Continuation => match self.continuations.pop_front() {
                    Some(continuation) => Some(self.spec(
                        JobKind::Continuation,
                        continuation.start,
                        continuation.actions,
                        0,
                    )),
                    None => self.shortcut(),
                },
                JobKind::Shortcut => self.shortcut(),
                JobKind::Redraw => None,
            };
            let spec = match spec {
                Some(spec) => spec,
                None if !self.config.redraws
                    || (self.config.until_shortcuts_exhausted
                        && self.schedule.exhausted
                        && self.continuations.is_empty()) =>
                {
                    break;
                }
                None => self.redraw()?,
            };
            batch.push(spec);
        }
        Ok(batch)
    }

    fn prefix(&self, start: Start) -> Vec<G::Action> {
        match start {
            Start::Chain(index) => self.chain.actions[..index].to_vec(),
            Start::Kept(index) => self.kept[index].prefix.clone(),
        }
    }

    fn admit(&mut self, spec: JobSpec<G>, mut result: JobResult<G>, accepted: &mut Accepted<G>) {
        let job = self.progress.jobs.total();
        self.progress.jobs.add(spec.kind);
        self.progress.execution_work = self.progress.execution_work.saturating_add(result.work);
        let best = accepted
            .as_ref()
            .map_or(self.chain.cost(), |(_, improvement, _, _)| improvement.cost);
        let mut productive = false;
        if let Some((index, cost)) = result.goal
            && cost < best
        {
            let mut input = self.prefix(spec.start);
            input.extend_from_slice(&spec.actions[..=index]);
            let restart = match (spec.kind, spec.start) {
                (JobKind::Shortcut, Start::Chain(position)) => Some(position),
                _ => None,
            };
            let states = match restart {
                Some(position) if spec.snapshot_endpoints > index => {
                    Some((position, std::mem::take(&mut result.endpoints)))
                }
                _ => None,
            };
            *accepted = Some((
                input,
                Improvement {
                    job,
                    kind: spec.kind,
                    cost,
                    actions: 0,
                    execution_work: self.progress.execution_work,
                },
                restart,
                states,
            ));
            productive = true;
        }
        if spec.kind == JobKind::Redraw {
            productive |= self.take_redraw(&spec, result.endpoints);
        }
        self.energy
            .record(spec.kind.index(), productive, result.work.max(1));
    }

    fn take_redraw(&mut self, spec: &JobSpec<G>, endpoints: Vec<Endpoint<G>>) -> bool {
        let incumbent = self.chain.cost();
        let mut best: Option<((u64, u64), usize, usize)> = None;
        for (index, endpoint) in endpoints.iter().enumerate() {
            if let Some(&chain_index) = self.chain.slots.get(&slot_of_key(endpoint.key)) {
                let held = self.chain.costs[chain_index];
                let saving = held.saving_over(endpoint.cost);
                if endpoint.cost < held
                    && chain_index < self.chain.actions.len()
                    && best.is_none_or(|(most, _, _)| saving > most)
                {
                    best = Some((saving, index, chain_index));
                }
            }
        }
        let mut base = None;
        for (index, endpoint) in endpoints.into_iter().enumerate() {
            let Some((snapshot, lineage, milestones)) = endpoint.kept else {
                continue;
            };
            let slot = slot_of_key(endpoint.key);
            if self
                .chain
                .slots
                .get(&slot)
                .is_some_and(|&chain_index| endpoint.cost >= self.chain.costs[chain_index])
                || endpoint.cost + endpoint.bound >= incumbent
            {
                continue;
            }
            let held = self.kept_slots.get(&slot).copied();
            match held {
                Some(held) if endpoint.cost >= self.kept[held].cost => continue,
                None if self.kept.len() >= self.config.kept_limit => continue,
                _ => {}
            }
            let base = base.get_or_insert_with(|| self.prefix(spec.start));
            let mut prefix = base.clone();
            prefix.extend_from_slice(&spec.actions[..=index]);
            let kept = Kept {
                state: State {
                    snapshot: Arc::new(snapshot),
                    key: endpoint.key,
                    lineage,
                    milestones,
                },
                prefix,
                cost: endpoint.cost,
            };
            match held {
                Some(held) => self.kept[held] = kept,
                None => {
                    self.kept_slots.insert(slot, self.kept.len());
                    self.kept.push(kept);
                }
            }
        }
        let Some((_, index, chain_index)) = best else {
            return false;
        };
        if !self.proposed.insert((spec.start, chain_index)) {
            return false;
        }
        if self.continuations.len() >= CONTINUATION_QUEUE_LIMIT {
            return false;
        }
        let mut actions = spec.actions[..=index].to_vec();
        actions.extend_from_slice(&self.chain.actions[chain_index..]);
        self.continuations.push_back(Continuation {
            start: spec.start,
            actions,
        });
        true
    }

    fn accept(
        &mut self,
        input: &[G::Action],
        improvement: &mut Improvement,
        restart: Option<usize>,
        states: Option<(usize, Vec<Endpoint<G>>)>,
    ) -> Result<(), Box<dyn Error>> {
        if let Some((start, endpoints)) = states {
            self.chain.truncate(start);
            let (goal, steps) = input[start..]
                .split_last()
                .ok_or("an accepted shrink tape is empty")?;
            if steps.len() != endpoints.len() {
                return Err("an accepted shortcut kept a state for every step but the goal".into());
            }
            for (action, endpoint) in steps.iter().zip(endpoints) {
                let (snapshot, lineage, milestones) = endpoint
                    .kept
                    .ok_or("an accepted shortcut step has no kept state")?;
                self.chain.push(
                    *action,
                    endpoint.cost,
                    State {
                        snapshot: Arc::new(snapshot),
                        key: endpoint.key,
                        lineage,
                        milestones,
                    },
                );
            }
            self.chain.actions.push(*goal);
            self.chain.costs.push(improvement.cost);
            return self.accepted(improvement, restart, start);
        }
        let common = self
            .chain
            .actions
            .iter()
            .zip(input)
            .take_while(|(left, right)| left == right)
            .count()
            .min(input.len().saturating_sub(1))
            .min(self.chain.actions.len().saturating_sub(1));
        self.chain.truncate(common);
        let work = self.extend_chain(&input[common..])?;
        self.progress.execution_work = self.progress.execution_work.saturating_add(work);
        self.progress.rebuilds += 1;
        if self.chain.cost() != improvement.cost {
            return Err("an accepted shrink tape changed its cost on replay".into());
        }
        self.accepted(improvement, restart, common)
    }

    fn accepted(
        &mut self,
        improvement: &mut Improvement,
        restart: Option<usize>,
        common: usize,
    ) -> Result<(), Box<dyn Error>> {
        improvement.actions = self.chain.actions.len();
        self.kept.clear();
        self.kept_slots.clear();
        self.continuations.clear();
        self.proposed.clear();
        let eligible = self.eligible();
        let before = |position: usize| eligible.partition_point(|&index| index < position);
        match restart {
            Some(position) => self.schedule.restart_at(before(position)),
            None => self
                .schedule
                .restart_at(self.schedule.position.min(before(common))),
        }
        self.progress.cost = self.chain.cost();
        self.progress.accepted.add(improvement.kind);
        Ok(())
    }

    fn snapshot_progress(&mut self) {
        self.progress.kept_states = self.kept.len();
        self.progress.pending_continuations = self.continuations.len();
        self.progress.shortcut_window = self.schedule.size;
        self.progress.shortcut_position = self.schedule.position;
    }

    fn budget_stop(&self) -> Option<ShrinkStop> {
        if self.progress.cost <= self.progress.lower_bound {
            return Some(ShrinkStop::LowerBound);
        }
        if self
            .config
            .work_budget
            .is_some_and(|budget| self.progress.execution_work >= budget)
        {
            return Some(ShrinkStop::WorkBudget);
        }
        if self
            .config
            .job_budget
            .is_some_and(|budget| self.progress.jobs.total() >= budget)
        {
            return Some(ShrinkStop::JobBudget);
        }
        None
    }

    fn input(&self) -> Input<G::Action> {
        Input {
            actions: self.chain.actions.clone(),
        }
    }
}

pub fn shrink<G>(
    workload: &G,
    config: &ShrinkConfig<G::Run>,
    incumbent: &Input<G::Action>,
    mut observe: impl FnMut(ShrinkEvent<'_, G::Action>) -> Result<(), Box<dyn Error>>,
) -> Result<ShrinkOutcome<G::Action>, Box<dyn Error>>
where
    G: ShrinkPolicy,
    Lineage<G>: Send + Sync,
{
    let mut shrinker = Shrinker::new(workload, config, incumbent)?;
    let workers = config.workers.max(1);
    let (job_sender, job_receiver) = mpsc::channel::<(usize, Arc<JobSpec<G>>)>();
    let job_receiver = Mutex::new(job_receiver);
    type Reply<G> = (usize, Result<JobResult<G>, String>);
    let (result_sender, result_receiver) = mpsc::channel::<Reply<G>>();
    std::thread::scope(
        |scope| -> Result<ShrinkOutcome<G::Action>, Box<dyn Error>> {
            for _ in 0..workers {
                let results = result_sender.clone();
                let jobs = &job_receiver;
                let run = &config.run;
                scope.spawn(move || {
                    let mut target = workload.new_target().ok();
                    loop {
                        let next = jobs.lock().ok().and_then(|jobs| jobs.recv().ok());
                        let Some((index, spec)) = next else {
                            break;
                        };
                        let result = match target.as_mut() {
                            Some(target) => execute(workload, run, target, &spec)
                                .map_err(|error| error.to_string()),
                            None => Err("a shrink worker could not create its target".to_owned()),
                        };
                        drop(spec);
                        if results.send((index, result)).is_err() {
                            break;
                        }
                    }
                });
            }
            drop(result_sender);
            let stop = loop {
                if let Some(stop) = shrinker.budget_stop() {
                    break stop;
                }
                let batch = shrinker.plan_batch()?;
                if batch.is_empty() {
                    break ShrinkStop::ShortcutsExhausted;
                }
                let batch = batch.into_iter().map(Arc::new).collect::<Vec<_>>();
                for (index, spec) in batch.iter().enumerate() {
                    job_sender
                        .send((index, Arc::clone(spec)))
                        .map_err(|_| "every shrink worker stopped")?;
                }
                let mut results = (0..batch.len()).map(|_| None).collect::<Vec<_>>();
                for _ in 0..batch.len() {
                    let (index, result) = result_receiver
                        .recv()
                        .map_err(|_| "every shrink worker stopped")?;
                    results[index] =
                        Some(result.map_err(|error| -> Box<dyn Error> { error.into() })?);
                }
                let mut accepted = None;
                for (spec, result) in batch.into_iter().zip(results) {
                    let spec = Arc::into_inner(spec).ok_or("a shrink job is still shared")?;
                    let result = result.ok_or("a shrink job returned no result")?;
                    shrinker.admit(spec, result, &mut accepted);
                }
                if let Some((input, mut improvement, restart, states)) = accepted {
                    shrinker.accept(&input, &mut improvement, restart, states)?;
                    shrinker.improvements.push(improvement);
                    let input = shrinker.input();
                    observe(ShrinkEvent::Improved {
                        input: &input,
                        improvement: &improvement,
                    })?;
                }
                shrinker.progress.batches += 1;
                shrinker.snapshot_progress();
                observe(ShrinkEvent::Batch {
                    progress: &shrinker.progress,
                })?;
            };
            drop(job_sender);
            shrinker.snapshot_progress();
            Ok(ShrinkOutcome {
                input: shrinker.input(),
                progress: shrinker.progress.clone(),
                improvements: shrinker.improvements.clone(),
                stop,
            })
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_contiguous_window_is_replaced_or_deleted_with_the_tail_kept() {
        let actions = [1, 2, 3, 4, 5];
        assert_eq!(
            proposal(&actions, &[1, 2], Fill::Replace(vec![20, 30])),
            vec![20, 30, 4, 5]
        );
        assert_eq!(proposal(&actions, &[1, 2], Fill::Delete), vec![4, 5]);
    }

    #[test]
    fn a_window_of_scattered_actions_keeps_the_actions_between_them() {
        let actions = [1, 2, 3, 4, 5];
        assert_eq!(
            proposal(&actions, &[1, 3], Fill::Replace(vec![20, 40])),
            vec![20, 3, 40, 5]
        );
        assert_eq!(proposal(&actions, &[1, 3], Fill::Delete), vec![3, 5]);
    }
}
