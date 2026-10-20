// SPDX-License-Identifier: AGPL-3.0-or-later

use super::{investigate, runs::Manifest};
use crate::{adapters::Request, config::Result};
use faults_workload::{FaultAction, FaultOperation, package::Report};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    num::NonZeroU16,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Attempt {
    pub replaced: Vec<usize>,
    pub reproduced: bool,
    pub violations: Vec<String>,
}

#[derive(Clone)]
pub struct Trial<T> {
    pub reproduced: bool,
    pub violations: Vec<String>,
    pub value: T,
}

pub struct Reduction<T> {
    pub reproduced: bool,
    pub replaced: BTreeSet<usize>,
    pub attempts: Vec<Attempt>,
    pub values: Vec<T>,
    pub value: T,
    pub replays: usize,
}

pub struct Probe<'a> {
    cancel: &'a AtomicBool,
    reproduced: &'a dyn Fn(),
}

impl Probe<'_> {
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    pub fn reproduced(&self) {
        (self.reproduced)();
    }
}

pub fn as_wait(action: FaultAction) -> FaultAction {
    let ticks = u16::try_from(action.ticks())
        .ok()
        .and_then(NonZeroU16::new)
        .unwrap_or(NonZeroU16::MIN);
    FaultAction {
        operation: FaultOperation::Wait(ticks),
        ..action
    }
}

pub fn candidate(actions: &[FaultAction], replaced: &BTreeSet<usize>) -> Vec<FaultAction> {
    actions
        .iter()
        .enumerate()
        .map(|(index, action)| {
            if replaced.contains(&index) {
                as_wait(*action)
            } else {
                *action
            }
        })
        .collect()
}

type Set = BTreeSet<usize>;

struct Job {
    id: usize,
    replaced: Set,
    cancel: Arc<AtomicBool>,
}

enum Event<T> {
    Reproduced(usize),
    Done(usize, std::result::Result<Option<Trial<T>>, String>),
}

struct Status<T> {
    reproduced: bool,
    finished: Option<std::result::Result<Trial<T>, String>>,
}

enum Outcome {
    Unknown,
    Reproduced,
    Failed,
    Error(String),
}

#[derive(Clone)]
struct Ddmin {
    kept: Vec<usize>,
    replaced: Set,
    parts: usize,
    cursor: usize,
}

impl Ddmin {
    fn round(&self, parts: usize) -> (usize, usize, Vec<Vec<usize>>) {
        let parts = parts.min(self.kept.len());
        let size = self.kept.len().div_ceil(parts);
        let chunks = self.kept.chunks(size).map(<[usize]>::to_vec).collect();
        (parts, size, chunks)
    }

    fn with(&self, chunk: &[usize]) -> Set {
        let mut set = self.replaced.clone();
        set.extend(chunk);
        set
    }
}

struct Coordinator<T> {
    statuses: BTreeMap<Set, Status<T>>,
    running: BTreeMap<Set, (usize, Arc<AtomicBool>)>,
    dispatched: BTreeMap<usize, Set>,
    flags: Vec<Arc<AtomicBool>>,
    consumed: Vec<Set>,
    decided: Vec<bool>,
    idle: usize,
    replays: usize,
}

impl<T> Coordinator<T> {
    fn outcome(&self, set: &Set) -> Outcome {
        match self.statuses.get(set) {
            None => Outcome::Unknown,
            Some(Status {
                finished: Some(Err(error)),
                ..
            }) => Outcome::Error(error.clone()),
            Some(Status {
                reproduced: true, ..
            }) => Outcome::Reproduced,
            Some(Status {
                finished: Some(Ok(_)),
                ..
            }) => Outcome::Failed,
            Some(_) => Outcome::Unknown,
        }
    }

    fn plan(&self, search: &Ddmin) -> Vec<Set> {
        let mut search = search.clone();
        let mut plan = Vec::new();
        while !search.kept.is_empty() {
            let (parts, size, chunks) = search.round(search.parts);
            let sets = chunks[search.cursor..]
                .iter()
                .map(|chunk| search.with(chunk))
                .collect::<Vec<_>>();
            let Some(accepted) = sets
                .iter()
                .position(|set| matches!(self.outcome(set), Outcome::Reproduced))
            else {
                if size == 1
                    || sets
                        .iter()
                        .any(|set| matches!(self.outcome(set), Outcome::Unknown))
                {
                    plan.extend(sets);
                    break;
                }
                search.parts = (parts * 2).min(search.kept.len());
                search.cursor = 0;
                continue;
            };
            plan.extend(sets[..=accepted].iter().cloned());
            let chunk = &chunks[search.cursor + accepted];
            search.kept.retain(|index| !chunk.contains(index));
            search.replaced = sets[accepted].clone();
            search.parts = parts.saturating_sub(1).max(2);
            search.cursor = 0;
        }
        plan
    }

    fn consume(&mut self, set: Set, reproduced: bool) {
        self.consumed.push(set);
        self.decided.push(reproduced);
    }

    fn receive(&mut self, event: Event<T>) {
        match event {
            Event::Reproduced(id) => {
                if let Some(set) = self.dispatched.get(&id)
                    && self
                        .running
                        .get(set)
                        .is_some_and(|(running, _)| *running == id)
                    && let Some(status) = self.statuses.get_mut(set)
                {
                    status.reproduced = true;
                }
            }
            Event::Done(id, result) => {
                self.idle += 1;
                let Some(set) = self.dispatched.remove(&id) else {
                    return;
                };
                let current = self
                    .running
                    .get(&set)
                    .is_some_and(|(running, _)| *running == id);
                if current {
                    self.running.remove(&set);
                }
                match result {
                    Ok(Some(trial)) => {
                        if let Some((_, cancel)) = self.running.remove(&set) {
                            cancel.store(true, Ordering::Relaxed);
                        }
                        let status = self.statuses.entry(set).or_insert(Status {
                            reproduced: false,
                            finished: None,
                        });
                        status.reproduced |= trial.reproduced;
                        status.finished = Some(Ok(trial));
                    }
                    Err(error) if current => {
                        if let Some(status) = self.statuses.get_mut(&set) {
                            status.finished = Some(Err(error));
                        }
                    }
                    _ => {
                        if current
                            && self
                                .statuses
                                .get(&set)
                                .is_some_and(|status| status.finished.is_none())
                        {
                            self.statuses.remove(&set);
                        }
                    }
                }
            }
        }
    }

    fn settle(&mut self, plan: &[Set], jobs: &mpsc::Sender<Job>) {
        let stale = self
            .running
            .keys()
            .filter(|set| !plan.contains(set) && !self.consumed.contains(set))
            .cloned()
            .collect::<Vec<_>>();
        for set in stale {
            if let Some((_, cancel)) = self.running.remove(&set) {
                cancel.store(true, Ordering::Relaxed);
            }
            if self
                .statuses
                .get(&set)
                .is_some_and(|status| status.finished.is_none())
            {
                self.statuses.remove(&set);
            }
        }
        for set in plan {
            if self.idle == 0 {
                break;
            }
            if self.statuses.contains_key(set) {
                continue;
            }
            let id = self.replays;
            let cancel = Arc::new(AtomicBool::new(false));
            if jobs
                .send(Job {
                    id,
                    replaced: set.clone(),
                    cancel: Arc::clone(&cancel),
                })
                .is_err()
            {
                return;
            }
            self.replays += 1;
            self.idle -= 1;
            self.flags.push(Arc::clone(&cancel));
            self.dispatched.insert(id, set.clone());
            self.running.insert(set.clone(), (id, cancel));
            self.statuses.insert(
                set.clone(),
                Status {
                    reproduced: false,
                    finished: None,
                },
            );
        }
    }
}

fn coordinate<T: Clone>(
    actions: &[FaultAction],
    coordinator: &mut Coordinator<T>,
    jobs: &mpsc::Sender<Job>,
    events: &mpsc::Receiver<Event<T>>,
) -> Result<Reduction<T>> {
    let mut search = Ddmin {
        kept: actions
            .iter()
            .enumerate()
            .filter(|(_, action)| !matches!(action.operation, FaultOperation::Wait(_)))
            .map(|(index, _)| index)
            .collect(),
        replaced: Set::new(),
        parts: 2,
        cursor: 0,
    };
    let baseline = Set::new();
    let mut started = false;
    let mut done = false;
    loop {
        while !done {
            if !started {
                match coordinator.outcome(&baseline) {
                    Outcome::Unknown => break,
                    Outcome::Error(error) => return Err(error.into()),
                    Outcome::Failed => {
                        coordinator.consume(baseline.clone(), false);
                        done = true;
                        break;
                    }
                    Outcome::Reproduced => {
                        coordinator.consume(baseline.clone(), true);
                        started = true;
                    }
                }
            }
            if search.kept.is_empty() {
                done = true;
                break;
            }
            let (parts, size, chunks) = search.round(search.parts);
            let mut waiting = false;
            let mut accepted = None;
            while search.cursor < chunks.len() {
                let set = search.with(&chunks[search.cursor]);
                match coordinator.outcome(&set) {
                    Outcome::Unknown => {
                        waiting = true;
                        break;
                    }
                    Outcome::Error(error) => return Err(error.into()),
                    Outcome::Failed => {
                        coordinator.consume(set, false);
                        search.cursor += 1;
                    }
                    Outcome::Reproduced => {
                        coordinator.consume(set.clone(), true);
                        accepted = Some((chunks[search.cursor].clone(), set));
                        break;
                    }
                }
            }
            if waiting {
                break;
            }
            search.cursor = 0;
            match accepted {
                Some((chunk, set)) => {
                    search.replaced = set;
                    search.kept.retain(|index| !chunk.contains(index));
                    search.parts = parts.saturating_sub(1).max(2);
                }
                None if size == 1 => done = true,
                None => search.parts = (parts * 2).min(search.kept.len()),
            }
        }
        let plan = match (done, started) {
            (true, _) => Vec::new(),
            (false, true) => coordinator.plan(&search),
            (false, false) => std::iter::once(baseline.clone())
                .chain(coordinator.plan(&search))
                .collect(),
        };
        coordinator.settle(&plan, jobs);
        let pending = coordinator.consumed.iter().any(|set| {
            !matches!(
                coordinator.statuses.get(set),
                Some(Status {
                    finished: Some(_),
                    ..
                })
            )
        });
        if done && !pending {
            break;
        }
        let event = events
            .recv()
            .map_err(|_| "every shrink worker stopped before the reduction finished")?;
        coordinator.receive(event);
    }
    let mut attempts = Vec::new();
    let mut values = Vec::new();
    let mut value = None;
    for (index, (set, decided)) in coordinator
        .consumed
        .iter()
        .zip(&coordinator.decided)
        .enumerate()
    {
        let trial = match coordinator
            .statuses
            .get(set)
            .and_then(|status| status.finished.as_ref())
        {
            Some(Ok(trial)) => trial,
            Some(Err(error)) => return Err(error.clone().into()),
            None => return Err("a decided shrink candidate has no finished replay".into()),
        };
        if trial.reproduced != *decided {
            return Err(format!(
                "the replay replacing steps {set:?} reported the violation and then lost it"
            )
            .into());
        }
        if trial.reproduced {
            value = Some(trial.value.clone());
        }
        if index != 0 {
            attempts.push(Attempt {
                replaced: set.iter().copied().collect(),
                reproduced: trial.reproduced,
                violations: trial.violations.clone(),
            });
        }
        values.push(trial.value.clone());
    }
    let reproduced = coordinator.decided.first().copied().unwrap_or(false);
    let value = match value {
        Some(value) => value,
        None => values
            .first()
            .cloned()
            .ok_or("the shrink finished without replaying the recorded actions")?,
    };
    Ok(Reduction {
        reproduced,
        replaced: search.replaced,
        attempts,
        values,
        value,
        replays: coordinator.replays,
    })
}

pub fn reduce<T: Clone + Send, W>(
    actions: &[FaultAction],
    jobs: usize,
    worker: impl Fn() -> std::result::Result<W, String> + Sync,
    evaluate: impl Fn(&mut W, &[FaultAction], &Probe) -> std::result::Result<Option<Trial<T>>, String>
    + Sync,
) -> Result<Reduction<T>> {
    let jobs = jobs.max(1);
    let (job_sender, job_receiver) = mpsc::channel::<Job>();
    let job_receiver = Mutex::new(job_receiver);
    let (event_sender, event_receiver) = mpsc::channel::<Event<T>>();
    std::thread::scope(|scope| {
        for _ in 0..jobs {
            let events = event_sender.clone();
            let (job_receiver, worker, evaluate) = (&job_receiver, &worker, &evaluate);
            scope.spawn(move || {
                let mut state = None;
                let next = || job_receiver.lock().ok()?.recv().ok();
                while let Some(job) = next() {
                    let input = candidate(actions, &job.replaced);
                    let signal = || {
                        let _ = events.send(Event::Reproduced(job.id));
                    };
                    let probe = Probe {
                        cancel: &job.cancel,
                        reproduced: &signal,
                    };
                    let result = catch_unwind(AssertUnwindSafe(|| {
                        if state.is_none() {
                            state = Some(worker()?);
                        }
                        match state.as_mut() {
                            Some(state) => evaluate(state, &input, &probe),
                            None => Err("the shrink worker could not start".into()),
                        }
                    }));
                    let result = result.unwrap_or_else(|_| {
                        state = None;
                        Err("a shrink attempt panicked".into())
                    });
                    if events.send(Event::Done(job.id, result)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(event_sender);
        let mut coordinator = Coordinator {
            statuses: BTreeMap::new(),
            running: BTreeMap::new(),
            dispatched: BTreeMap::new(),
            flags: Vec::new(),
            consumed: Vec::new(),
            decided: Vec::new(),
            idle: jobs,
            replays: 0,
        };
        let result = coordinate(actions, &mut coordinator, &job_sender, &event_receiver);
        for flag in &coordinator.flags {
            flag.store(true, Ordering::Relaxed);
        }
        drop(job_sender);
        result
    })
}

fn reproduces(report: &Report, target: &[String]) -> (bool, Vec<String>) {
    let violations = report
        .replays
        .first()
        .map(|replay| replay.violations.clone())
        .unwrap_or_default();
    let reproduced = report.replays.first().is_some_and(|replay| replay.bug)
        && violations
            .iter()
            .any(|violation| target.contains(violation));
    (reproduced, violations)
}

#[derive(Serialize)]
struct Summary<'a> {
    finding: Option<usize>,
    violations: &'a [String],
    faults_before: usize,
    faults_after: usize,
    replaced: Vec<usize>,
    replays: usize,
    attempts: &'a [Attempt],
    actions: &'a [FaultAction],
}

pub struct Shrunk {
    pub report: Report,
    pub actions: Vec<FaultAction>,
    pub candidates: usize,
    pub replays: usize,
    pub faults_before: usize,
}

pub fn faults(actions: &[FaultAction]) -> usize {
    actions
        .iter()
        .filter(|action| !matches!(action.operation, FaultOperation::Wait(_)))
        .count()
}

pub struct Source {
    pub manifest: Manifest,
    pub path: PathBuf,
    pub actions: Vec<FaultAction>,
    pub violations: Vec<String>,
}

pub fn source(saved: crate::runs::Saved, finding: Option<usize>, verb: &str) -> Result<Source> {
    let path = saved.path;
    let manifest = Manifest::from_record(saved.manifest)?;
    manifest.verify(&path)?;
    if manifest.bundle.is_none() {
        return Err(format!("{verb} requires a supervised application scenario").into());
    }
    let recorded = investigate::recorded_witness(&manifest, finding)?;
    if !recorded.bug {
        return Err(format!("the selected execution has no violation to {verb}").into());
    }
    let (mut actions, _, anchor) = investigate::trajectory(&recorded, true);
    actions.truncate(anchor);
    Ok(Source {
        manifest,
        path,
        actions,
        violations: recorded.violations,
    })
}

pub fn shrink_into(
    out: &Path,
    manifest: &Manifest,
    source: &Source,
    finding: Option<usize>,
    jobs: usize,
) -> Result<Shrunk> {
    let actions = &source.actions;
    let target = &source.violations;
    let artifacts = manifest.fault_artifacts(out)?;
    let options = manifest.options(out)?;
    let replayer = faults_workload::package::Replayer::new(
        &artifacts,
        &options,
        source.manifest.settle,
        jobs.max(1),
    )?;
    let reduction = reduce(
        actions,
        replayer.workers(),
        || replayer.worker().map_err(|error| error.to_string()),
        |worker, input, probe| {
            let mut signalled = false;
            let report = worker
                .replay(input, |observation| {
                    if !signalled
                        && observation.is_bug()
                        && observation
                            .violations()
                            .iter()
                            .any(|violation| target.contains(violation))
                    {
                        signalled = true;
                        probe.reproduced();
                    }
                    !probe.cancelled()
                })
                .map_err(|error| error.to_string())?;
            Ok(report.map(|report| {
                let (reproduced, violations) = reproduces(&report, target);
                Trial {
                    reproduced,
                    violations,
                    value: report,
                }
            }))
        },
    )?;
    for (index, report) in reduction.values.iter().enumerate() {
        report.write(&attempt_dir(out, index))?;
    }
    if !reduction.reproduced {
        return Err(format!(
            "the recorded actions no longer produce {target:?}; see {}",
            attempt_dir(out, 0).display()
        )
        .into());
    }
    let shrunk = candidate(actions, &reduction.replaced);
    let faults_before = faults(actions);
    fs::write(
        out.join("shrink.json"),
        serde_json::to_vec_pretty(&Summary {
            finding,
            violations: target,
            faults_before,
            faults_after: faults_before - reduction.replaced.len(),
            replaced: reduction.replaced.iter().copied().collect(),
            replays: reduction.replays,
            attempts: &reduction.attempts,
            actions: &shrunk,
        })?,
    )?;
    Ok(Shrunk {
        report: reduction.value,
        actions: shrunk,
        candidates: reduction.values.len(),
        replays: reduction.replays,
        faults_before,
    })
}

#[cfg(feature = "frontend")]
pub fn shrink(request: Request) -> Result<u8> {
    let saved = crate::runs::Saved::read(
        &request
            .selection
            .as_ref()
            .ok_or("select a search or branch")?
            .run,
    )?;
    shrink_saved(request, saved)
}

pub fn shrink_saved(request: Request, saved: crate::runs::Saved) -> Result<u8> {
    let selection = request.selection.ok_or("select a search or branch")?;
    let source = source(saved, selection.finding, "shrink")?;
    let out = request.destination.create()?;
    let mut manifest = source.manifest.inherit(&source.path, &out, "replay")?;
    manifest.actions = source.actions.clone();
    manifest.save(&out)?;
    let result = (|| -> Result<u8> {
        let shrunk = shrink_into(&out, &manifest, &source, selection.finding, request.jobs)?;
        manifest.actions = shrunk.actions.clone();
        shrunk.report.write(&out)?;
        manifest.pin_outcome(&shrunk.report)?;
        println!(
            "kept {} of {} faults after {} candidates and {} replays; violation: {}",
            faults(&shrunk.actions),
            shrunk.faults_before,
            shrunk.candidates,
            shrunk.replays,
            source.violations.join("; ")
        );
        for (index, action) in shrunk.actions.iter().enumerate() {
            if !matches!(action.operation, FaultOperation::Wait(_)) {
                println!("  step {}: {:?}", index + 1, action.operation);
            }
        }
        Ok(0)
    })();
    super::workflow::finish(&out, &mut manifest, result)
}

fn attempt_dir(out: &Path, index: usize) -> PathBuf {
    out.join("attempts").join(index.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn action(operation: FaultOperation, choice: u64) -> FaultAction {
        FaultAction::new(operation, NonZeroU16::MIN).with_choice(choice)
    }

    fn ticks(value: u16) -> NonZeroU16 {
        NonZeroU16::new(value).unwrap()
    }

    fn schedule() -> Vec<FaultAction> {
        vec![
            action(FaultOperation::Kill(1, ticks(256)), 11),
            action(FaultOperation::Wait(ticks(256)), 12),
            action(FaultOperation::Pause(2, ticks(128)), 13),
            action(FaultOperation::KillAll(ticks(256)), 14),
            action(FaultOperation::Restart(0, ticks(64)), 15),
            action(FaultOperation::Hook(1, ticks(256)), 16),
            action(FaultOperation::Wait(ticks(256)), 17),
        ]
    }

    fn waits(input: &[FaultAction]) -> Vec<usize> {
        input
            .iter()
            .enumerate()
            .filter(|(_, action)| matches!(action.operation, FaultOperation::Wait(_)))
            .map(|(index, _)| index)
            .collect()
    }

    fn reproduces_with(required: &[usize], input: &[FaultAction]) -> bool {
        required
            .iter()
            .all(|step| !matches!(input[*step].operation, FaultOperation::Wait(_)))
    }

    type Evaluation = std::result::Result<Option<Trial<Vec<usize>>>, String>;

    fn needs(
        required: &'static [usize],
    ) -> impl Fn(&mut (), &[FaultAction], &Probe) -> Evaluation + Sync {
        move |_, input, _| {
            let reproduced = reproduces_with(required, input);
            Ok(Some(Trial {
                reproduced,
                violations: if reproduced {
                    vec!["lost".into()]
                } else {
                    Vec::new()
                },
                value: waits(input),
            }))
        }
    }

    fn run(
        jobs: usize,
        evaluate: impl Fn(&mut (), &[FaultAction], &Probe) -> Evaluation + Sync,
    ) -> Result<Reduction<Vec<usize>>> {
        reduce(&schedule(), jobs, || Ok(()), evaluate)
    }

    #[test]
    fn replaced_faults_keep_their_length_and_choice() {
        let actions = schedule();
        let shrunk = candidate(&actions, &BTreeSet::from([0, 3]));
        assert_eq!(shrunk[0].operation, FaultOperation::Wait(ticks(256)));
        assert_eq!(shrunk[0].choice, 11);
        assert_eq!(shrunk[0].coverage_quantum, actions[0].coverage_quantum);
        assert_eq!(shrunk[3].operation, FaultOperation::Wait(ticks(256)));
        assert_eq!(shrunk[2], actions[2]);
        assert_eq!(
            shrunk.iter().map(FaultAction::ticks).sum::<u64>(),
            actions.iter().map(FaultAction::ticks).sum::<u64>()
        );
    }

    #[test]
    fn reduction_keeps_only_the_faults_the_violation_needs() {
        for jobs in [1, 2, 8] {
            let reduction = run(jobs, needs(&[0, 3])).unwrap();
            assert!(reduction.reproduced);
            assert_eq!(reduction.replaced, BTreeSet::from([2, 4, 5]), "jobs {jobs}");
            assert_eq!(reduction.value, vec![1, 2, 4, 5, 6]);
            assert!(reduction.attempts.iter().all(|attempt| {
                !attempt.replaced.contains(&1) && !attempt.replaced.contains(&6)
            }));
        }
    }

    #[test]
    fn every_job_count_checks_the_same_candidates_in_the_same_order() {
        for required in [&[4][..], &[0, 3], &[], &[0, 2, 3, 4, 5]] {
            let single = run(1, needs(required)).unwrap();
            for jobs in [2, 3, 8] {
                let parallel = run(jobs, needs(required)).unwrap();
                assert_eq!(
                    parallel.replaced, single.replaced,
                    "{required:?} jobs {jobs}"
                );
                assert_eq!(
                    parallel.attempts, single.attempts,
                    "{required:?} jobs {jobs}"
                );
                assert_eq!(parallel.values, single.values, "{required:?} jobs {jobs}");
                assert_eq!(parallel.value, single.value, "{required:?} jobs {jobs}");
            }
        }
        assert_eq!(
            run(1, needs(&[4])).unwrap().replaced,
            BTreeSet::from([0, 2, 3, 5])
        );
    }

    #[test]
    fn a_violation_without_faults_replaces_every_fault() {
        let reduction = run(3, needs(&[])).unwrap();
        assert_eq!(reduction.replaced, BTreeSet::from([0, 2, 3, 4, 5]));
    }

    #[test]
    fn a_candidate_seen_before_is_not_replayed_again() {
        let seen = Mutex::new(BTreeMap::<Vec<usize>, usize>::new());
        let evaluate = needs(&[0, 3]);
        let reduction = run(1, |state, input, probe| {
            *seen.lock().unwrap().entry(waits(input)).or_default() += 1;
            evaluate(state, input, probe)
        })
        .unwrap();
        let seen = seen.into_inner().unwrap();
        assert!(seen.values().all(|count| *count == 1), "{seen:?}");
        assert_eq!(reduction.replays, seen.len());
        assert_eq!(reduction.values.len(), reduction.attempts.len() + 1);
    }

    fn wait_until(done: impl Fn() -> bool) -> bool {
        for _ in 0..10_000 {
            if done() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        done()
    }

    #[test]
    fn an_early_report_lets_the_next_round_start_before_the_replay_ends() {
        let next_round = AtomicBool::new(false);
        let timed_out = AtomicBool::new(false);
        let reduction = run(4, |_, input, probe| {
            let replaced = waits(input);
            if [0, 2, 3].iter().all(|step| replaced.contains(step)) && replaced.len() > 5 {
                next_round.store(true, Ordering::Relaxed);
            }
            let reproduced = reproduces_with(&[4], input);
            if reproduced {
                probe.reproduced();
                if !wait_until(|| next_round.load(Ordering::Relaxed) || probe.cancelled()) {
                    timed_out.store(true, Ordering::Relaxed);
                }
            }
            Ok(Some(Trial {
                reproduced,
                violations: Vec::new(),
                value: replaced,
            }))
        })
        .unwrap();
        assert!(!timed_out.load(Ordering::Relaxed));
        assert_eq!(reduction.replaced, BTreeSet::from([0, 2, 3, 5]));
        assert_eq!(reduction.value, vec![0, 1, 2, 3, 5, 6]);
    }

    #[test]
    fn a_reproducing_candidate_starts_its_next_round_while_earlier_candidates_run() {
        let next_round = AtomicBool::new(false);
        let timed_out = AtomicBool::new(false);
        let seen = Mutex::new(BTreeMap::<Vec<usize>, usize>::new());
        let reduction = run(4, |_, input, probe| {
            let replaced = waits(input);
            *seen.lock().unwrap().entry(replaced.clone()).or_default() += 1;
            if replaced == vec![0, 1, 2, 4, 5, 6] || replaced == vec![1, 3, 4, 5, 6] {
                next_round.store(true, Ordering::Relaxed);
            }
            if replaced == vec![1, 4, 5, 6] {
                probe.reproduced();
            }
            if (replaced == vec![0, 1, 2, 3, 6] || replaced == vec![1, 4, 5, 6])
                && !wait_until(|| next_round.load(Ordering::Relaxed) || probe.cancelled())
            {
                timed_out.store(true, Ordering::Relaxed);
            }
            if probe.cancelled() {
                return Ok(None);
            }
            needs(&[0])(&mut (), input, probe)
        })
        .unwrap();
        let single = run(1, needs(&[0])).unwrap();
        assert!(!timed_out.load(Ordering::Relaxed));
        assert!(seen.into_inner().unwrap().values().all(|count| *count == 1));
        assert_eq!(reduction.replaced, single.replaced);
        assert_eq!(reduction.attempts, single.attempts);
        assert_eq!(reduction.replaced, BTreeSet::from([2, 3, 4, 5]));
    }

    #[test]
    fn speculative_replays_stop_when_an_earlier_candidate_reproduces() {
        let single = run(1, needs(&[4])).unwrap();
        let consumed = single.values.clone();
        let finished = AtomicUsize::new(0);
        let late = Mutex::new(Vec::new());
        let reduction = run(8, |_, input, probe| {
            let replaced = waits(input);
            if !consumed.contains(&replaced) {
                if !wait_until(|| probe.cancelled())
                    || finished.load(Ordering::Relaxed) == consumed.len()
                {
                    late.lock().unwrap().push(replaced);
                }
                return Ok(None);
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
            finished.fetch_add(1, Ordering::Relaxed);
            needs(&[4])(&mut (), input, probe)
        })
        .unwrap();
        assert_eq!(reduction.replaced, single.replaced);
        assert_eq!(reduction.attempts, single.attempts);
        assert!(late.into_inner().unwrap().is_empty());
    }

    #[test]
    fn a_baseline_without_the_violation_stops_before_any_candidate_counts() {
        let reduction = run(4, |_, input, _| {
            Ok(Some(Trial {
                reproduced: false,
                violations: Vec::new(),
                value: waits(input),
            }))
        })
        .unwrap();
        assert!(!reduction.reproduced);
        assert!(reduction.attempts.is_empty());
        assert!(reduction.replaced.is_empty());
        assert_eq!(reduction.values, vec![vec![1, 6]]);
    }

    #[test]
    fn evaluation_errors_stop_the_reduction() {
        let error = run(2, |_, _, _| -> Evaluation { Err("guest failed".into()) })
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("guest failed"), "{error}");
    }

    #[test]
    fn errors_in_candidates_the_reduction_never_reaches_are_ignored() {
        let evaluate = needs(&[4]);
        let reduction = run(8, |state, input, probe| {
            if waits(input) == vec![1, 4, 5, 6] {
                return Err("unused candidate failed".into());
            }
            evaluate(state, input, probe)
        })
        .unwrap();
        assert_eq!(reduction.replaced, BTreeSet::from([0, 2, 3, 5]));
    }

    #[test]
    fn a_lost_violation_is_an_error() {
        let error = run(1, |_, input, probe| {
            probe.reproduced();
            Ok(Some(Trial {
                reproduced: input.iter().all(|action| {
                    !matches!(action.operation, FaultOperation::Wait(_))
                        || action.choice == 12
                        || action.choice == 17
                }),
                violations: Vec::new(),
                value: waits(input),
            }))
        })
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("lost it"), "{error}");
    }

    #[test]
    fn a_panicking_worker_reports_an_error() {
        let error = run(2, |_, _, _| -> Evaluation { panic!("worker bug") })
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("panicked"), "{error}");
    }
}
