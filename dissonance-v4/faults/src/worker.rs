// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{Receiver, Sender, channel},
    },
    thread::{self, JoinHandle},
};

use consonance_client::{cache::CacheIndex, session::WorkerLauncher};
use engine::target::{Action, JobId, Observation, Position, Probe, Want, Worker};
use faults_workload::{
    consonance::{FaultConfig, FaultTarget},
    target::{FaultAction, FaultObservations, FaultSnapshot},
};
use serde::{Deserialize, Serialize};

use crate::{layout::Layout, translate::Translator};

pub struct Boot {
    pub kernel: Vec<u8>,
    pub initramfs: Vec<u8>,
    pub config: FaultConfig,
    pub cache: Option<Arc<dyn CacheIndex>>,
    pub launcher: Option<WorkerLauncher>,
}

pub struct Genesis {
    pub snapshot: FaultSnapshot,
    pub root_seal: u64,
}

enum Cmd {
    Restore(Arc<Entry>),
    Apply(FaultAction),
    Actions,
}

pub struct Applied {
    pub observation: FaultObservations,
    pub snapshot: Option<FaultSnapshot>,
    pub failed: bool,
    pub hung: bool,
    pub infrastructure: bool,
}

enum Reply {
    Booted(Result<Genesis, String>),
    Restored(Result<Option<FaultObservations>, String>),
    Applied(Box<Applied>),
    Actions(Vec<FaultAction>),
}

pub struct Actor {
    tx: Option<Sender<Cmd>>,
    rx: Receiver<Reply>,
    thread: Option<JoinHandle<()>>,
}

fn serve(boot: Arc<Boot>, rx: Receiver<Cmd>, tx: Sender<Reply>) {
    let booted = FaultTarget::new(
        &boot.kernel,
        &boot.initramfs,
        &boot.config,
        boot.cache.clone(),
        boot.launcher.clone(),
    )
    .and_then(|target| {
        let snapshot = target
            .snapshot()?
            .ok_or("the setup endpoint cannot be captured")?;
        Ok((target, snapshot))
    });
    let mut target = match booted {
        Ok((target, snapshot)) => {
            let root_seal = target.root_seal();
            if tx
                .send(Reply::Booted(Ok(Genesis {
                    snapshot,
                    root_seal,
                })))
                .is_err()
            {
                return;
            }
            target
        }
        Err(e) => {
            let _ = tx.send(Reply::Booted(Err(e)));
            return;
        }
    };
    for cmd in rx {
        let reply = match cmd {
            Cmd::Restore(entry) => Reply::Restored(
                target
                    .restore(&entry.snapshot)
                    .map_err(|e| e.to_string())
                    .and_then(|()| {
                        if target.infrastructure_failure() {
                            Err("host infrastructure failure during a restore".into())
                        } else if target.failed() {
                            Ok(None)
                        } else {
                            Ok(Some(target.observation().clone()))
                        }
                    }),
            ),
            Cmd::Apply(action) => {
                let snapshot = target.apply_and_snapshot(action);
                let infrastructure = target.infrastructure_failure()
                    || target.verify_integrity().is_err()
                    || snapshot.is_err();
                Reply::Applied(Box::new(Applied {
                    observation: target.observation().clone(),
                    snapshot: snapshot.ok().flatten(),
                    failed: target.failed(),
                    hung: target.watchdog_cutoffs() != 0,
                    infrastructure,
                }))
            }
            Cmd::Actions => Reply::Actions(target.actions().to_vec()),
        };
        if tx.send(reply).is_err() {
            return;
        }
    }
}

impl Actor {
    pub fn spawn(boot: Arc<Boot>) -> Result<Self, String> {
        let (cmd_tx, cmd_rx) = channel();
        let (reply_tx, reply_rx) = channel();
        let thread = thread::Builder::new()
            .name("fault-target".into())
            .spawn(move || serve(boot, cmd_rx, reply_tx))
            .map_err(|e| e.to_string())?;
        Ok(Actor {
            tx: Some(cmd_tx),
            rx: reply_rx,
            thread: Some(thread),
        })
    }

    pub fn genesis(&self) -> Result<Genesis, String> {
        match self
            .rx
            .recv()
            .map_err(|_| "fault target thread stopped".to_string())?
        {
            Reply::Booted(genesis) => genesis,
            _ => Err("fault target answered out of order".into()),
        }
    }

    fn call(&self, cmd: Cmd) -> Result<Reply, String> {
        self.tx
            .as_ref()
            .and_then(|tx| tx.send(cmd).ok())
            .ok_or("fault target thread stopped")?;
        self.rx
            .recv()
            .map_err(|_| "fault target thread stopped".to_string())
    }

    pub fn restore(&self, entry: Arc<Entry>) -> Result<Option<FaultObservations>, String> {
        match self.call(Cmd::Restore(entry))? {
            Reply::Restored(r) => r,
            _ => Err("fault target answered out of order".into()),
        }
    }

    pub fn apply(&self, action: FaultAction) -> Result<Applied, String> {
        match self.call(Cmd::Apply(action))? {
            Reply::Applied(a) => Ok(*a),
            _ => Err("fault target answered out of order".into()),
        }
    }

    pub fn actions(&self) -> Result<Vec<FaultAction>, String> {
        match self.call(Cmd::Actions)? {
            Reply::Actions(a) => Ok(a),
            _ => Err("fault target answered out of order".into()),
        }
    }
}

impl Drop for Actor {
    fn drop(&mut self) {
        self.tx = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub struct Entry {
    pub snapshot: FaultSnapshot,
    pub parent: Option<u64>,
    pub segment: Vec<Action>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Finding {
    pub actions: Vec<FaultAction>,
    pub violations: Vec<String>,
    pub observation: FaultObservations,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Pass {
    pub route: Vec<Action>,
    pub fault_route: Vec<FaultAction>,
    pub scalars: Vec<u64>,
    pub batch: u64,
    pub job_in_batch: u64,
    pub execution: u64,
    pub work: u64,
    #[serde(skip)]
    order: (u64, u64),
}

#[derive(Default)]
struct Passes {
    first: BTreeMap<usize, Pass>,
    pending: BTreeMap<usize, Pass>,
    jobs: Vec<(JobId, u64)>,
    batches: u64,
    executions: u64,
    work: u64,
}

pub struct Shared {
    pub translator: Translator,
    pub layout: Layout,
    table: Mutex<Vec<Arc<Entry>>>,
    pub findings: Mutex<Vec<Finding>>,
    passes: Mutex<Passes>,
    passing_jobs: Mutex<Vec<u64>>,
    pub executions: AtomicU64,
    pub hangs: AtomicU64,
}

impl Shared {
    pub fn new(translator: Translator, layout: Layout, root: FaultSnapshot) -> Self {
        let sometimes = layout.sometimes.len();
        Self {
            translator,
            layout,
            table: Mutex::new(vec![Arc::new(Entry {
                snapshot: root,
                parent: None,
                segment: Vec::new(),
            })]),
            findings: Mutex::new(Vec::new()),
            passes: Mutex::new(Passes::default()),
            passing_jobs: Mutex::new(vec![0; sometimes]),
            executions: AtomicU64::new(0),
            hangs: AtomicU64::new(0),
        }
    }

    pub fn entry(&self, handle: u64) -> Result<Arc<Entry>, String> {
        self.table
            .lock()
            .map_err(|_| "snapshot table lock")?
            .get(handle as usize)
            .cloned()
            .ok_or_else(|| format!("unknown snapshot handle {handle}"))
    }

    fn insert(&self, entry: Entry) -> Result<u64, String> {
        let mut table = self.table.lock().map_err(|_| "snapshot table lock")?;
        let handle = table.len() as u64;
        table.push(Arc::new(entry));
        Ok(handle)
    }

    pub fn entries(&self) -> usize {
        self.table.lock().map_or(0, |t| t.len())
    }

    pub fn route_of(&self, handle: u64) -> Result<Vec<Action>, String> {
        let mut segments = Vec::new();
        let mut at = Some(handle);
        while let Some(h) = at {
            let entry = self.entry(h)?;
            at = entry.parent;
            segments.push(entry);
        }
        Ok(segments
            .iter()
            .rev()
            .flat_map(|e| e.segment.iter().copied())
            .collect())
    }

    fn wants_pass(&self, index: usize, order: (u64, u64)) -> bool {
        self.passes.lock().is_ok_and(|p| {
            !p.first.contains_key(&index)
                && p.pending.get(&index).is_none_or(|held| order < held.order)
        })
    }

    fn offer_pass(&self, index: usize, pass: Pass) -> Result<(), String> {
        let mut passes = self.passes.lock().map_err(|_| "pass lock")?;
        if !passes.first.contains_key(&index)
            && passes
                .pending
                .get(&index)
                .is_none_or(|held| pass.order < held.order)
        {
            passes.pending.insert(index, pass);
        }
        Ok(())
    }

    fn finish_job(&self, job: JobId, work: u64, passed: &[usize]) -> Result<(), String> {
        self.passes
            .lock()
            .map_err(|_| "pass lock")?
            .jobs
            .push((job, work));
        let mut counts = self.passing_jobs.lock().map_err(|_| "pass lock")?;
        for &i in passed {
            counts[i] += 1;
        }
        Ok(())
    }

    pub fn close_batch(&self, engine_jobs: u64, engine_work: u64) -> Result<(), String> {
        let mut passes = self.passes.lock().map_err(|_| "pass lock")?;
        let batch = passes.batches;
        let mut jobs = std::mem::take(&mut passes.jobs);
        jobs.sort_unstable();
        if jobs.iter().enumerate().any(|(j, (id, _))| {
            *id != JobId {
                batch,
                index: j as u64,
            }
        }) {
            return Err(format!("batch {batch} did not run each of its jobs once"));
        }
        let admitted = engine_jobs
            .checked_sub(passes.executions)
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n <= jobs.len())
            .ok_or("the engine admitted more jobs than ran")?;
        let mut starts = Vec::with_capacity(admitted);
        let mut work = passes.work;
        for (_, w) in &jobs[..admitted] {
            starts.push(work);
            work += w;
        }
        if work != engine_work {
            return Err(format!(
                "job order check failed: the batch's jobs add to work {work}, the engine counted {engine_work}"
            ));
        }
        let before = passes.executions;
        let pending = std::mem::take(&mut passes.pending);
        for (index, mut pass) in pending {
            let j = pass.order.0 as usize;
            if j >= jobs.len() {
                return Err("a pass names a job that did not run".into());
            }
            if j >= admitted {
                continue;
            }
            pass.batch = batch;
            pass.job_in_batch = j as u64;
            pass.execution = before + j as u64 + 1;
            pass.work = starts[j] + pass.order.1;
            passes.first.insert(index, pass);
        }
        passes.batches += 1;
        passes.executions = engine_jobs;
        passes.work = engine_work;
        Ok(())
    }

    pub fn first_passes(&self) -> BTreeMap<usize, Pass> {
        self.passes
            .lock()
            .map(|p| p.first.clone())
            .unwrap_or_default()
    }

    pub fn passing_jobs(&self) -> Vec<u64> {
        self.passing_jobs
            .lock()
            .map(|c| c.clone())
            .unwrap_or_default()
    }
}

pub fn handle(start: &[u8]) -> Result<u64, String> {
    Ok(u64::from_le_bytes(
        start
            .try_into()
            .map_err(|_| "a snapshot handle is eight bytes")?,
    ))
}

pub struct FaultWorker {
    pub actor: Actor,
    pub shared: Arc<Shared>,
    scalars: Vec<u64>,
    array: Vec<u8>,
}

impl FaultWorker {
    pub fn new(actor: Actor, shared: Arc<Shared>) -> Self {
        Self {
            actor,
            shared,
            scalars: Vec::new(),
            array: Vec::new(),
        }
    }
}

impl FaultWorker {
    fn record_passes(
        &self,
        start: u64,
        job: JobId,
        done: &[Action],
        newly: &mut [bool],
        at_start: &[bool],
    ) -> Result<(), String> {
        let order = (job.index, done.len() as u64);
        let mut prefix: Option<Vec<Action>> = None;
        for index in 0..self.shared.layout.sometimes.len() {
            if self.scalars[self.shared.layout.sometimes_index(index)] == 0 {
                continue;
            }
            if !at_start[index] {
                newly[index] = true;
            }
            if !self.shared.wants_pass(index, order) {
                continue;
            }
            let mut route = match &prefix {
                Some(p) => p.clone(),
                None => {
                    let p = self.shared.route_of(start)?;
                    prefix = Some(p.clone());
                    p
                }
            };
            route.extend_from_slice(done);
            self.shared.offer_pass(
                index,
                Pass {
                    route,
                    fault_route: self.actor.actions()?,
                    scalars: self.scalars.clone(),
                    batch: 0,
                    job_in_batch: 0,
                    execution: 0,
                    work: 0,
                    order,
                },
            )?;
        }
        Ok(())
    }
}

impl Worker for FaultWorker {
    fn observe_start(&mut self, start: &[u8]) -> Result<Observation, String> {
        let o = self
            .actor
            .restore(self.shared.entry(handle(start)?)?)?
            .ok_or("the start state did not rebuild")?;
        let mut scalars = Vec::new();
        let mut array = Vec::new();
        self.shared.layout.scalars(&o, &mut scalars);
        self.shared.layout.array(&o, &mut array);
        Ok(Observation {
            scalars,
            array,
            milestone: false,
        })
    }

    fn run(
        &mut self,
        job: JobId,
        start: &[u8],
        actions: &[Action],
        probe: &mut dyn Probe,
    ) -> Result<u64, String> {
        let start = handle(start)?;
        let entry = self.shared.entry(start)?;
        let Some(mut current) = self.actor.restore(entry)? else {
            if start == 0 {
                return Err("the setup state did not rebuild".into());
            }
            self.shared.hangs.fetch_add(1, Ordering::Relaxed);
            self.shared.executions.fetch_add(1, Ordering::Relaxed);
            self.shared.finish_job(job, 0, &[])?;
            return Ok(0);
        };
        self.shared.layout.scalars(&current, &mut self.scalars);
        let at_start: Vec<bool> = (0..self.shared.layout.sometimes.len())
            .map(|i| self.scalars[self.shared.layout.sometimes_index(i)] != 0)
            .collect();
        let mut newly = vec![false; at_start.len()];
        let mut work = 0u64;
        for (i, a) in actions.iter().enumerate() {
            let fault = self.shared.translator.action(a, current.event_ready)?;
            let applied = self.actor.apply(fault)?;
            if applied.infrastructure {
                return Err("host infrastructure failure during a fault action".into());
            }
            work += 1;
            let o = &applied.observation;
            let stopped = applied.failed || applied.hung || !o.stop.is_continuable();
            let finding = !applied.failed && !applied.hung && o.is_bug();
            self.shared.layout.scalars(o, &mut self.scalars);
            self.shared.layout.array(o, &mut self.array);
            let want = probe.observe(
                Position { action: i, step: 0 },
                work,
                &self.scalars,
                &self.array,
                false,
            );
            if !applied.failed && !applied.hung {
                self.record_passes(start, job, &actions[..=i], &mut newly, &at_start)?;
            }
            if want == Want::Capture
                && !finding
                && !stopped
                && let Some(snapshot) = applied.snapshot
            {
                let h = self.shared.insert(Entry {
                    snapshot,
                    parent: Some(start),
                    segment: actions[..=i].to_vec(),
                })?;
                probe.keep(h.to_le_bytes().to_vec());
            }
            if applied.hung {
                self.shared.hangs.fetch_add(1, Ordering::Relaxed);
            }
            if finding {
                let fault_actions = self.actor.actions()?;
                self.shared
                    .findings
                    .lock()
                    .map_err(|_| "findings lock")?
                    .push(Finding {
                        actions: fault_actions,
                        violations: o.violations().into_iter().collect(),
                        observation: applied.observation.clone(),
                    });
            }
            if want == Want::Stop || finding || stopped {
                break;
            }
            current = applied.observation;
        }
        self.shared.executions.fetch_add(1, Ordering::Relaxed);
        let passed: Vec<usize> = (0..newly.len()).filter(|&i| newly[i]).collect();
        self.shared.finish_job(job, work, &passed)?;
        Ok(work)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::{
        archive::Archive,
        campaign::{Campaign, Config},
        target::{Kind, Vocabulary},
    };

    type Captures = Arc<Mutex<Vec<(JobId, u64, u64)>>>;

    struct Counter {
        captures: Captures,
        jobs: Arc<Mutex<Vec<(JobId, u64)>>>,
    }

    impl Worker for Counter {
        fn observe_start(&mut self, _: &[u8]) -> Result<Observation, String> {
            Ok(Observation {
                scalars: vec![0],
                array: Vec::new(),
                milestone: false,
            })
        }

        fn run(
            &mut self,
            job: JobId,
            start: &[u8],
            actions: &[Action],
            probe: &mut dyn Probe,
        ) -> Result<u64, String> {
            let mut v = u64::from_le_bytes(start.try_into().map_err(|_| "start")?);
            let mut work = 0;
            for (i, a) in actions.iter().enumerate() {
                v = (v * 7 + u64::from(a.input) + 1) % 997;
                work += 1;
                let at = Position { action: i, step: 0 };
                if probe.observe(at, work, &[v], &[], false) == Want::Capture {
                    probe.keep(v.to_le_bytes().to_vec());
                    self.captures
                        .lock()
                        .map_err(|_| "lock")?
                        .push((job, work, v));
                }
            }
            self.jobs.lock().map_err(|_| "lock")?.push((job, work));
            Ok(work)
        }
    }

    #[test]
    fn job_ids_reproduce_the_engine_work_of_every_admitted_state() {
        let captures: Captures = Arc::default();
        let jobs = Arc::new(Mutex::new(Vec::new()));
        let mut workers: Vec<Box<dyn Worker>> = (0..3)
            .map(|_| {
                Box::new(Counter {
                    captures: captures.clone(),
                    jobs: jobs.clone(),
                }) as Box<dyn Worker>
            })
            .collect();
        let vocab = Vocabulary {
            inputs: 4,
            holds: false,
            base_steps: 8,
        };
        let mut campaign = Campaign::new(
            vec![Kind::Identity],
            vocab,
            Config::new(5, 6000),
            0u64.to_le_bytes().to_vec(),
            vec![0],
        );
        let mut ends = vec![(0u64, 0u64)];
        let mut seen = Vec::new();
        let mut record = |states: &[engine::archive::StateRec]| {
            for state in &states[seen.len()..] {
                seen.push((state.found, state.scalars.first().copied()));
            }
        };
        campaign.run(&mut workers, &mut |_: &Archive| true, &mut |c| {
            ends.push((c.jobs, c.work));
            record(&c.archive.states);
        });
        ends.push((campaign.jobs, campaign.work));
        record(&campaign.archive.states);
        let mut jobs = jobs.lock().expect("lock").clone();
        jobs.sort_unstable();
        let mut predicted = BTreeMap::new();
        for b in 0..ends.len() as u64 - 1 {
            let batch_jobs: Vec<(JobId, u64)> =
                jobs.iter().copied().filter(|j| j.0.batch == b).collect();
            for (j, (id, _)) in batch_jobs.iter().enumerate() {
                assert_eq!(id.index, j as u64);
            }
            let (jobs_before, mut work) = ends[b as usize];
            let admitted = (ends[b as usize + 1].0 - jobs_before) as usize;
            let mut starts = BTreeMap::new();
            for (id, w) in &batch_jobs[..admitted] {
                starts.insert(*id, work);
                work += w;
            }
            assert_eq!(work, ends[b as usize + 1].1);
            for c in captures
                .lock()
                .expect("lock")
                .iter()
                .filter(|c| c.0.batch == b)
            {
                if let Some(s) = starts.get(&c.0) {
                    predicted.insert((s + c.1, c.2), ());
                }
            }
        }
        assert!(campaign.batches > 16);
        assert!(campaign.archive.states.iter().any(|s| s.scalars.is_empty()));
        assert!(seen.len() > 50);
        for (found, scalar) in &seen[1..] {
            let scalar = scalar.expect("every state has its scalars when progress sees it");
            assert!(predicted.contains_key(&(*found, scalar)));
        }
    }
}
