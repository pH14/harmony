// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    cmp::Reverse,
    collections::{BTreeMap, BinaryHeap},
    error::Error,
    fmt,
    sync::{Arc, Condvar, Mutex, mpsc},
    thread,
    time::Duration,
};

use crate::search::locality::{LocalityKey, WorkerLocality};
use crate::search::telemetry::{
    TargetCounters, WorkerTelemetry, nanos_since, now, thread_schedstat,
};

fn profiled_execute<Output>(
    profile: &mut Option<crate::search::telemetry::WorkerExecuteCpuTelemetry>,
    mut clock: impl FnMut() -> Option<u128>,
    execute: impl FnOnce() -> Result<Output, String>,
) -> std::thread::Result<Result<Output, String>> {
    let before = profile.as_ref().and_then(|_| clock());
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(execute));
    let after = profile.as_ref().and_then(|_| clock());
    if let Some(profile) = profile {
        profile.record(
            before,
            after,
            matches!(&outcome, Ok(Err(_))),
            outcome.is_err(),
        );
    }
    outcome
}

pub(crate) struct WorkerReply<Key, Output, O = ()> {
    pub(crate) worker: u32,
    pub(crate) key: Key,
    pub(crate) outcome: Result<Output, String>,
    pub(crate) observation: crate::search::job_observation::ObservationDelivery<O>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WorkerPoolError {
    QueueClosed,
    DuplicateJob,
    WorkersExited,
    RepliesClosed,
    QueueCapacity,
}

impl fmt::Display for WorkerPoolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::QueueClosed => "campaign job queue is already closed",
            Self::DuplicateJob => "campaign job queue already holds a job with this order key",
            Self::WorkersExited => "every campaign worker exited before taking the next job",
            Self::RepliesClosed => "every campaign worker exited while a reply was expected",
            Self::QueueCapacity => "campaign worker queue capacity cannot be allocated",
        })
    }
}

impl Error for WorkerPoolError {}

struct IdleWorkers {
    values: Vec<Option<WorkerLocality>>,
    identifiers: Vec<u32>,
    bits: Vec<u64>,
    len: usize,
}

impl IdleWorkers {
    fn new(workers: usize) -> Self {
        Self {
            values: std::iter::repeat_with(|| None).take(workers).collect(),
            identifiers: (0..workers)
                .map(|worker| u32::try_from(worker).expect("worker identifier"))
                .collect(),
            bits: vec![0; workers.div_ceil(64)],
            len: 0,
        }
    }

    fn insert(&mut self, worker: u32, locality: WorkerLocality) {
        let index = worker as usize;
        if self.values[index].replace(locality).is_none() {
            self.len += 1;
        }
        self.bits[index / 64] |= 1_u64 << (index % 64);
    }

    fn remove(&mut self, worker: &u32) {
        let index = *worker as usize;
        if self.values[index].take().is_some() {
            self.len -= 1;
        }
        self.bits[index / 64] &= !(1_u64 << (index % 64));
    }

    fn iter(&self) -> impl Iterator<Item = (&u32, &WorkerLocality)> {
        self.bits.iter().enumerate().flat_map(move |(word, &bits)| {
            let mut remaining = bits;
            std::iter::from_fn(move || {
                if remaining == 0 {
                    return None;
                }
                let bit = remaining.trailing_zeros() as usize;
                remaining &= remaining - 1;
                let index = word * 64 + bit;
                Some((
                    &self.identifiers[index],
                    self.values[index].as_ref().expect("idle worker bit"),
                ))
            })
        })
    }

    fn first_key_value(&self) -> Option<(&u32, &WorkerLocality)> {
        self.iter().next()
    }

    #[cfg(test)]
    fn contains_key(&self, worker: &u32) -> bool {
        self.values
            .get(*worker as usize)
            .is_some_and(Option::is_some)
    }
    #[cfg(test)]
    fn keys(&self) -> impl Iterator<Item = &u32> {
        self.iter().map(|(worker, _)| worker)
    }
    #[cfg(test)]
    fn len(&self) -> usize {
        self.len
    }
    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.len == 0
    }
}

struct JobQueue<Key, Job> {
    jobs: BinaryHeap<Reverse<(Key, usize)>>,
    slots: Vec<Option<QueuedSlot<Key, Job>>>,
    free: Vec<usize>,
    keys: BTreeMap<Key, usize>,
    slot_index: Option<fn(&Key) -> usize>,
    idle: IdleWorkers,
    assigned: Vec<Option<usize>>,
    open: bool,
    workers: usize,
    failed_job: bool,
}

struct QueuedJob<Job> {
    job: Job,
    locality: Option<LocalityKey>,
}

struct QueuedSlot<Key, Job> {
    key: Key,
    queued: QueuedJob<Job>,
}

impl<Key: Ord + Clone, Job> JobQueue<Key, Job> {
    fn enqueue(
        &mut self,
        key: Key,
        job: Job,
        locality: Option<LocalityKey>,
    ) -> Result<(), WorkerPoolError> {
        let index = if let Some(slot_index) = self.slot_index {
            let index = slot_index(&key);
            if self.slots.get(index).is_none_or(Option::is_some) {
                return Err(WorkerPoolError::DuplicateJob);
            }
            index
        } else {
            if self.keys.contains_key(&key) {
                return Err(WorkerPoolError::DuplicateJob);
            }
            let index = self.free.pop().unwrap_or_else(|| {
                self.slots.push(None);
                self.slots.len() - 1
            });
            self.keys.insert(key.clone(), index);
            index
        };
        self.jobs.push(Reverse((key.clone(), index)));
        self.slots[index] = Some(QueuedSlot {
            key,
            queued: QueuedJob { job, locality },
        });
        Ok(())
    }

    fn release(&mut self, index: usize) -> (Key, QueuedJob<Job>) {
        let slot = self.slots[index].take().expect("assigned job slot");
        if self.slot_index.is_none() {
            assert!(self.keys.remove(&slot.key).is_some());
            self.free.push(index);
        }
        (slot.key, slot.queued)
    }

    fn assign(&mut self, mut notify: impl FnMut(u32)) -> bool {
        let mut assigned = false;
        while let Some(Reverse((_, index))) = self.jobs.peek() {
            let first = &self.slots[*index].as_ref().expect("queued job slot").queued;
            let worker = if first.locality.is_none() {
                self.idle.first_key_value().map(|(&worker, _)| worker)
            } else {
                self.idle
                    .iter()
                    .max_by_key(|(worker, locality)| {
                        (locality.rank(first.locality), std::cmp::Reverse(**worker))
                    })
                    .map(|(&worker, _)| worker)
            };
            let Some(worker) = worker else {
                break;
            };
            let Reverse((_, index)) = self.jobs.pop().expect("queued job head");
            self.idle.remove(&worker);
            let slot = &mut self.assigned[worker as usize];
            assert!(slot.is_none());
            *slot = Some(index);
            notify(worker);
            assigned = true;
        }
        assigned
    }

    fn take_assigned(&mut self, worker: u32) -> Option<(Key, Job)> {
        let index = self.assigned[worker as usize].take()?;
        let (key, queued) = self.release(index);
        Some((key, queued.job))
    }

    #[cfg(test)]
    fn assigned_keys(&self) -> Vec<Key> {
        let mut keys = self
            .assigned
            .iter()
            .flatten()
            .map(|index| self.slots[*index].as_ref().unwrap().key.clone())
            .collect::<Vec<_>>();
        keys.sort();
        keys
    }

    #[cfg(test)]
    fn queued_keys(&self) -> Vec<Key> {
        let mut keys = self
            .jobs
            .iter()
            .map(|Reverse((key, _))| key.clone())
            .collect::<Vec<_>>();
        keys.sort();
        keys
    }

    #[cfg(test)]
    fn pop_queued(&mut self) -> Option<(Key, QueuedJob<Job>)> {
        let Reverse((_, index)) = self.jobs.pop()?;
        Some(self.release(index))
    }

    #[cfg(test)]
    fn from_parts(
        jobs: BTreeMap<Key, QueuedJob<Job>>,
        idle: BTreeMap<u32, WorkerLocality>,
        workers: usize,
        open: bool,
    ) -> Self {
        let mut queue = Self {
            jobs: BinaryHeap::new(),
            slots: Vec::new(),
            free: Vec::new(),
            keys: BTreeMap::new(),
            slot_index: None,
            idle: IdleWorkers::new(workers),
            assigned: vec![None; workers],
            open,
            workers,
            failed_job: false,
        };
        for (worker, locality) in idle {
            queue.idle.insert(worker, locality);
        }
        for (key, queued) in jobs {
            queue.enqueue(key, queued.job, queued.locality).unwrap();
        }
        queue
    }
}

struct SharedJobs<Key, Job> {
    queue: Mutex<JobQueue<Key, Job>>,
    ready: Vec<Condvar>,
    #[cfg(test)]
    state_changed: Condvar,
}

impl<Key, Job> SharedJobs<Key, Job> {
    fn wake_all(&self) {
        for ready in &self.ready {
            ready.notify_all();
        }
        #[cfg(test)]
        self.state_changed.notify_all();
    }
}

impl<Key: Ord + Clone, Job> SharedJobs<Key, Job> {
    fn new(workers: usize) -> Self {
        Self {
            queue: Mutex::new(JobQueue {
                jobs: BinaryHeap::new(),
                slots: Vec::new(),
                free: Vec::new(),
                keys: BTreeMap::new(),
                slot_index: None,
                idle: IdleWorkers::new(workers),
                assigned: std::iter::repeat_with(|| None).take(workers).collect(),
                open: true,
                workers,
                failed_job: false,
            }),
            ready: std::iter::repeat_with(Condvar::new).take(workers).collect(),
            #[cfg(test)]
            state_changed: Condvar::new(),
        }
    }

    fn next(&self, worker: u32, locality: WorkerLocality) -> Option<(Key, Job)> {
        let mut queue = self.queue.lock().ok()?;
        queue.idle.insert(worker, locality);
        #[cfg(test)]
        self.state_changed.notify_all();
        loop {
            queue.assign(|assigned| {
                if assigned != worker {
                    self.ready[assigned as usize].notify_one();
                }
            });
            if let Some(job) = queue.take_assigned(worker) {
                return Some(job);
            }
            if !queue.open {
                queue.idle.remove(&worker);
                return None;
            }
            queue = self.ready[worker as usize].wait(queue).ok()?;
        }
    }

    fn close(&self) {
        if let Ok(mut queue) = self.queue.lock() {
            queue.open = false;
        }
        self.wake_all();
    }

    fn note_failed_job(&self) {
        if let Ok(mut queue) = self.queue.lock() {
            queue.failed_job = true;
        }
    }
}

struct WorkerExit<'a, Key, Job> {
    jobs: &'a SharedJobs<Key, Job>,
    worker: u32,
}

impl<Key, Job> Drop for WorkerExit<'_, Key, Job> {
    fn drop(&mut self) {
        let wake = match self.jobs.queue.lock() {
            Ok(mut queue) => {
                queue.workers = queue.workers.saturating_sub(1);
                queue.idle.remove(&self.worker);
                queue.open
            }
            Err(_) => true,
        };
        if wake {
            self.jobs.wake_all();
        } else {
            #[cfg(test)]
            self.jobs.state_changed.notify_all();
        }
    }
}

pub(crate) struct WorkerPool<Key: Ord + Clone, Job, Output, O = ()> {
    jobs: Option<Arc<SharedJobs<Key, Job>>>,
    reply_receiver: mpsc::Receiver<WorkerReply<Key, Output, O>>,
    startup_receiver: mpsc::Receiver<(u32, Result<(), String>)>,
}

impl<Key: Ord + Clone, Job, Output, O> WorkerPool<Key, Job, Output, O> {
    pub(crate) fn configure_indexed(
        &mut self,
        capacity: usize,
        slot_index: fn(&Key) -> usize,
    ) -> Result<(), WorkerPoolError> {
        let shared = self.jobs.as_ref().ok_or(WorkerPoolError::QueueClosed)?;
        let mut queue = shared
            .queue
            .lock()
            .map_err(|_| WorkerPoolError::WorkersExited)?;
        if !queue.jobs.is_empty() || queue.assigned.iter().any(Option::is_some) {
            return Err(WorkerPoolError::DuplicateJob);
        }
        queue
            .jobs
            .try_reserve(capacity)
            .map_err(|_| WorkerPoolError::QueueCapacity)?;
        let additional = capacity.saturating_sub(queue.slots.len());
        queue
            .slots
            .try_reserve_exact(additional)
            .map_err(|_| WorkerPoolError::QueueCapacity)?;
        queue.slots.resize_with(capacity, || None);
        queue.free.clear();
        queue.keys.clear();
        queue.slot_index = Some(slot_index);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn send(&self, key: Key, job: Job) -> Result<(), WorkerPoolError> {
        self.send_local(key, job, None)
    }

    fn send_local(
        &self,
        key: Key,
        job: Job,
        locality: Option<LocalityKey>,
    ) -> Result<(), WorkerPoolError> {
        let shared = self.jobs.as_ref().ok_or(WorkerPoolError::QueueClosed)?;
        let mut queue = shared
            .queue
            .lock()
            .map_err(|_| WorkerPoolError::WorkersExited)?;
        if queue.workers == 0 && !queue.failed_job {
            return Err(WorkerPoolError::WorkersExited);
        }
        queue.enqueue(key, job, locality)?;
        queue.assign(|assigned| shared.ready[assigned as usize].notify_one());
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn dispatch(&self, key: Key, job: Job) -> Result<(), Box<dyn Error>> {
        self.send(key, job).map_err(Into::into)
    }

    pub(crate) fn dispatch_local(
        &self,
        key: Key,
        job: Job,
        locality: Option<LocalityKey>,
    ) -> Result<(), Box<dyn Error>> {
        self.send_local(key, job, locality).map_err(Into::into)
    }

    pub(crate) fn close(&mut self) {
        if let Some(shared) = self.jobs.take() {
            shared.close();
        }
    }

    #[cfg(test)]
    #[allow(clippy::disallowed_methods)]
    pub(crate) fn wait_idle(&self, workers: &[u32]) -> Result<(), String> {
        let shared = self.jobs.as_ref().ok_or("test pool is closed")?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut queue = shared.queue.lock().map_err(|_| "test queue poisoned")?;
        while !workers.iter().all(|worker| queue.idle.contains_key(worker)) {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(format!(
                    "workers {workers:?} did not become idle; idle={:?}, assigned={:?}",
                    queue.idle.keys().collect::<Vec<_>>(),
                    queue
                        .assigned
                        .iter()
                        .enumerate()
                        .filter_map(|(worker, job)| job.as_ref().map(|_| worker))
                        .collect::<Vec<_>>()
                ));
            }
            queue = shared
                .state_changed
                .wait_timeout(queue, remaining)
                .map_err(|_| "test queue poisoned while waiting")?
                .0;
        }
        Ok(())
    }

    pub(crate) fn wait_ready(&self, workers: u32) -> Result<(), String> {
        let mut statuses = std::iter::repeat_with(|| None)
            .take(workers as usize)
            .collect::<Vec<Option<Result<(), String>>>>();
        for _ in 0..workers {
            let Ok((worker, status)) = self.startup_receiver.recv() else {
                break;
            };
            let slot = statuses
                .get_mut(worker as usize)
                .ok_or("campaign worker startup reported an unknown identifier")?;
            if slot.replace(status).is_some() {
                return Err("campaign worker startup reported one identifier twice".to_owned());
            }
        }
        for (worker, status) in statuses.into_iter().enumerate() {
            match status {
                Some(Ok(())) => {}
                Some(Err(error)) => {
                    return Err(format!("campaign worker {worker} failed to start: {error}"));
                }
                None => {
                    return Err(format!(
                        "campaign worker {worker} exited before reporting startup"
                    ));
                }
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn receive(&self) -> Result<WorkerReply<Key, Output, O>, WorkerPoolError> {
        self.reply_receiver
            .recv()
            .map_err(|_| WorkerPoolError::RepliesClosed)
    }

    pub(crate) fn receive_timeout(
        &self,
        timeout: Duration,
    ) -> Result<Option<WorkerReply<Key, Output, O>>, WorkerPoolError> {
        match self.reply_receiver.recv_timeout(timeout) {
            Ok(reply) => Ok(Some(reply)),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(WorkerPoolError::RepliesClosed),
        }
    }

    pub(crate) fn try_receive(
        &self,
    ) -> Result<Option<WorkerReply<Key, Output, O>>, WorkerPoolError> {
        match self.reply_receiver.try_recv() {
            Ok(reply) => Ok(Some(reply)),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err(WorkerPoolError::RepliesClosed),
        }
    }
}

impl<Key: Ord + Clone, Job, Output, O> Drop for WorkerPool<Key, Job, Output, O> {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
pub(crate) fn with_worker_pool<State, Key, Job, Output, ResultValue, CoordinatorError>(
    workers: u32,
    initialize: impl Fn(u32) -> Result<State, String> + Sync,
    recover: impl Fn(u32) -> Result<State, String> + Sync,
    execute: impl Fn(&mut State, Job) -> Result<Output, String> + Sync,
    finish: impl Fn(&State) -> TargetCounters + Sync,
    coordinate: impl FnOnce(&mut WorkerPool<Key, Job, Output>) -> Result<ResultValue, CoordinatorError>,
) -> Result<(ResultValue, Vec<WorkerTelemetry>), CoordinatorError>
where
    Key: Ord + Clone + Send,
    Job: Send,
    Output: Send,
    CoordinatorError: From<String>,
{
    with_worker_pool_observed(
        workers,
        initialize,
        recover,
        execute,
        finish,
        || {},
        || None::<()>,
        None,
        coordinate,
    )
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub(crate) fn with_worker_pool_observed<O, State, Key, Job, Output, ResultValue, CoordinatorError>(
    workers: u32,
    initialize: impl Fn(u32) -> Result<State, String> + Sync,
    recover: impl Fn(u32) -> Result<State, String> + Sync,
    execute: impl Fn(&mut State, Job) -> Result<Output, String> + Sync,
    finish: impl Fn(&State) -> TargetCounters + Sync,
    begin: impl Fn() + Sync,
    drain: impl Fn() -> Option<O> + Sync,
    diagnostics: Option<crate::search::job_observation::ObservationDiagnostics>,
    coordinate: impl FnOnce(
        &mut WorkerPool<Key, Job, Output, O>,
    ) -> Result<ResultValue, CoordinatorError>,
) -> Result<(ResultValue, Vec<WorkerTelemetry>), CoordinatorError>
where
    Key: Ord + Clone + Send,
    Job: Send,
    Output: Send,
    O: Copy + Send + 'static,
    CoordinatorError: From<String>,
{
    with_worker_pool_routed(
        workers,
        initialize,
        recover,
        execute,
        finish,
        |_| WorkerLocality::default(),
        begin,
        drain,
        diagnostics,
        coordinate,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn with_worker_pool_routed<O, State, Key, Job, Output, ResultValue, CoordinatorError>(
    workers: u32,
    initialize: impl Fn(u32) -> Result<State, String> + Sync,
    recover: impl Fn(u32) -> Result<State, String> + Sync,
    execute: impl Fn(&mut State, Job) -> Result<Output, String> + Sync,
    finish: impl Fn(&State) -> TargetCounters + Sync,
    locality: impl Fn(&State) -> WorkerLocality + Sync,
    begin: impl Fn() + Sync,
    drain: impl Fn() -> Option<O> + Sync,
    diagnostics: Option<crate::search::job_observation::ObservationDiagnostics>,
    coordinate: impl FnOnce(
        &mut WorkerPool<Key, Job, Output, O>,
    ) -> Result<ResultValue, CoordinatorError>,
) -> Result<(ResultValue, Vec<WorkerTelemetry>), CoordinatorError>
where
    Key: Ord + Clone + Send,
    Job: Send,
    Output: Send,
    O: Copy + Send + 'static,
    CoordinatorError: From<String>,
{
    crate::search::job_observation::bounded::<O>();
    thread::scope(|scope| {
        let (reply_sender, reply_receiver) = mpsc::channel::<WorkerReply<Key, Output, O>>();
        let (startup_sender, startup_receiver) = mpsc::channel();
        let jobs = Arc::new(SharedJobs::<Key, Job>::new(workers as usize));
        let mut handles = Vec::with_capacity(workers as usize);
        for worker in 0..workers {
            let reply_sender = reply_sender.clone();
            let startup_sender = startup_sender.clone();
            let jobs = Arc::clone(&jobs);
            let initialize = &initialize;
            let recover = &recover;
            let execute = &execute;
            let finish = &finish;
            let locality = &locality;
            let begin = &begin;
            let drain = &drain;
            let diagnostics = diagnostics.clone();
            handles.push(scope.spawn(move || {
                let _exit = WorkerExit {
                    jobs: &jobs,
                    worker,
                };
                let schedstat_before = thread_schedstat();
                let booted = now();
                let mut telemetry = WorkerTelemetry {
                    profile_mode: crate::search::telemetry::ProfileMode::from_flags(
                        super::campaign::profile_clock_enabled(),
                        super::campaign::worker_cpu_profile_enabled(),
                    ),
                    execute_cpu: super::campaign::worker_cpu_profile_enabled()
                        .then(crate::search::telemetry::WorkerExecuteCpuTelemetry::new),
                    ..WorkerTelemetry::default()
                };
                let initialized =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| initialize(worker)));
                let mut state = Some(match initialized {
                    Ok(Ok(state)) => {
                        let _ = startup_sender.send((worker, Ok(())));
                        state
                    }
                    Ok(Err(error)) => {
                        let _ = startup_sender.send((worker, Err(error.clone())));
                        return Err(error);
                    }
                    Err(_) => {
                        let error = "worker panicked during startup".to_owned();
                        let _ = startup_sender.send((worker, Err(error.clone())));
                        return Err(error);
                    }
                });
                drop(startup_sender);
                telemetry.boot_ns = nanos_since(booted);
                let mut worker_failure = None;
                loop {
                    let waiting = now();
                    let hints = state
                        .as_ref()
                        .map_or_else(WorkerLocality::default, locality);
                    let Some((key, job)) = jobs.next(worker, hints) else {
                        break;
                    };
                    telemetry.idle_ns = telemetry.idle_ns.saturating_add(nanos_since(waiting));
                    let started = now();
                    use crate::search::job_observation::{
                        CollectionStatus, ObservationDelivery, ObservationReceipt,
                    };
                    let mut status = CollectionStatus::Disabled;
                    if let Some(counts) = &diagnostics {
                        counts.add(crate::search::job_observation::Counter::Eligible);
                        status = if std::panic::catch_unwind(std::panic::AssertUnwindSafe(begin))
                            .is_err()
                        {
                            counts.add(crate::search::job_observation::Counter::BeginPanics);
                            CollectionStatus::BeginPanicked
                        } else {
                            CollectionStatus::Unavailable
                        };
                    }
                    let (outcome, panicked) = match state.as_mut() {
                        Some(target) => {
                            match profiled_execute(
                                &mut telemetry.execute_cpu,
                                crate::search::telemetry::thread_cpu_time_ns,
                                || execute(target, job),
                            ) {
                                Ok(outcome) => (outcome, false),
                                Err(_) => {
                                    (Err("worker panicked during execution".to_owned()), true)
                                }
                            }
                        }
                        None => (
                            Err("worker target unavailable after execution panic".to_owned()),
                            false,
                        ),
                    };
                    telemetry.busy_ns = telemetry.busy_ns.saturating_add(nanos_since(started));
                    telemetry.jobs = telemetry.jobs.saturating_add(1);
                    let failed = outcome.is_err();
                    if failed {
                        jobs.note_failed_job();
                    }
                    let payload = if diagnostics.is_some() {
                        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(drain)) {
                            Ok(payload) if status != CollectionStatus::BeginPanicked => {
                                if let Some(counts) = &diagnostics {
                                    counts.add(if payload.is_some() {
                                        crate::search::job_observation::Counter::Collected
                                    } else {
                                        crate::search::job_observation::Counter::Unavailable
                                    });
                                }
                                status = if payload.is_some() {
                                    CollectionStatus::Collected
                                } else {
                                    CollectionStatus::Unavailable
                                };
                                payload
                            }
                            Ok(_) => None,
                            Err(_) => {
                                if let Some(counts) = &diagnostics {
                                    counts
                                        .add(crate::search::job_observation::Counter::DrainPanics);
                                }
                                status = CollectionStatus::DrainPanicked;
                                None
                            }
                        }
                    } else {
                        None
                    };
                    let sent = reply_sender.send(WorkerReply {
                        worker,
                        key,
                        outcome,
                        observation: ObservationDelivery {
                            receipt: ObservationReceipt { status, payload },
                            diagnostics: diagnostics.clone(),
                        },
                    });
                    if panicked {
                        worker_failure = Some("worker panicked during execution".to_owned());
                        if sent.is_err() {
                            return Err("worker panicked during execution".to_owned());
                        }
                        let discarded =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                drop(state.take());
                            }));
                        if discarded.is_err() {
                            return Err(
                                "worker target could not be discarded after panic".to_owned()
                            );
                        }
                        state = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            recover(worker)
                        }))
                        .ok()
                        .and_then(Result::ok);
                        if state.is_none() {
                            return Err(
                                "worker target could not be recreated after panic".to_owned()
                            );
                        }
                    }
                    if sent.is_err() || (failed && !panicked) {
                        break;
                    }
                }
                if let Some(profile) = telemetry.execute_cpu.as_mut() {
                    profile.finish(telemetry.jobs);
                }
                if let Some(target) = state.as_ref() {
                    telemetry.target =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| finish(target)))
                            .map_err(|_| {
                            format!("campaign worker {worker} panicked during finalization")
                        })?;
                }
                if let (Some((cpu_before, wait_before)), Some((cpu_after, wait_after))) =
                    (schedstat_before, thread_schedstat())
                {
                    telemetry.cpu_ns = Some(cpu_after.saturating_sub(cpu_before));
                    telemetry.cpu_wait_ns = Some(wait_after.saturating_sub(wait_before));
                }
                match worker_failure {
                    Some(error) => Err(error),
                    None => Ok(telemetry),
                }
            }));
        }
        drop(reply_sender);
        drop(startup_sender);
        let mut pool = WorkerPool {
            jobs: Some(jobs),
            reply_receiver,
            startup_receiver,
        };
        let result = coordinate(&mut pool);
        drop(pool);
        let mut telemetry = Vec::with_capacity(handles.len());
        let mut worker_error = None;
        for (worker, handle) in handles.into_iter().enumerate() {
            match handle.join() {
                Ok(Ok(worker)) => telemetry.push(worker),
                Ok(Err(error)) => {
                    worker_error.get_or_insert(error);
                    telemetry.push(WorkerTelemetry::default());
                }
                Err(_) => {
                    worker_error.get_or_insert(format!(
                        "campaign worker {worker} panicked outside the handled worker phases"
                    ));
                    telemetry.push(WorkerTelemetry::default());
                }
            }
        }
        let value = result?;
        match worker_error {
            Some(error) => Err(error.into()),
            None => Ok((value, telemetry)),
        }
    })
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc, sync::mpsc, time::Duration};

    use super::{WorkerPool, WorkerPoolError, WorkerReply, profiled_execute, with_worker_pool};
    use crate::search::telemetry::TargetCounters;

    #[test]
    fn worker_execute_cpu_disabled_does_not_read_clock() {
        let result = profiled_execute(&mut None, || panic!("disabled clock read"), || Ok(17));
        assert_eq!(result.expect("execute did not panic"), Ok(17));
    }

    #[test]
    fn worker_execute_cpu_classifies_actual_calls_and_panic() {
        let mut profile = Some(crate::search::telemetry::WorkerExecuteCpuTelemetry::new());
        profile.as_mut().expect("profile").supported = true;
        profile.as_mut().expect("profile").total_cpu_ns = Some(0);
        let mut tick = 0;
        let mut clock = || {
            tick += 7;
            Some(tick)
        };
        assert_eq!(
            profiled_execute(&mut profile, &mut clock, || Ok(11)).expect("success"),
            Ok(11)
        );
        assert_eq!(
            profiled_execute(&mut profile, &mut clock, || Err::<u8, _>(
                "failure".to_owned()
            ))
            .expect("ordinary error"),
            Err("failure".to_owned())
        );
        assert!(
            profiled_execute(&mut profile, &mut clock, || -> Result<u8, String> {
                panic!("execute panic")
            })
            .is_err()
        );
        let value = profile.as_mut().expect("profile");
        value.finish(3);
        assert_eq!(
            (value.calls, value.samples, value.errors, value.panics),
            (3, 3, 1, 1)
        );
        assert_eq!(value.total_cpu_ns, Some(21));
        assert!(value.complete);
        value.finish(4);
        assert!(!value.complete);
    }

    #[test]
    fn worker_execute_cpu_missing_backwards_and_overflow_are_not_zero() {
        use crate::search::telemetry::WorkerExecuteCpuTelemetry;
        for (before, after) in [(None, Some(4)), (Some(4), None), (Some(9), Some(4))] {
            let mut value = WorkerExecuteCpuTelemetry::new();
            value.supported = true;
            value.total_cpu_ns = Some(0);
            value.record(before, after, false, false);
            value.record(Some(10), Some(11), false, false);
            value.finish(2);
            assert_eq!((value.calls, value.samples), (2, 1));
            assert_eq!(value.total_cpu_ns, None);
            assert!(!value.complete);
        }
        let mut value = WorkerExecuteCpuTelemetry::new();
        value.total_cpu_ns = Some(u128::MAX);
        value.record(Some(0), Some(1), false, false);
        assert!(value.counter_overflow);
        assert_eq!(value.total_cpu_ns, None);
        for field in 0..4 {
            let mut value = WorkerExecuteCpuTelemetry::new();
            value.total_cpu_ns = Some(0);
            match field {
                0 => value.calls = u64::MAX,
                1 => value.samples = u64::MAX,
                2 => value.errors = u64::MAX,
                _ => value.panics = u64::MAX,
            }
            value.record(Some(0), Some(1), field == 2, field == 3);
            value.finish(1);
            assert!(value.counter_overflow);
            assert_eq!(value.total_cpu_ns, None);
            assert!(!value.complete);
        }
    }

    #[test]
    fn worker_execute_cpu_pool_jobs_match_enabled_coverage() {
        let (values, workers) = with_worker_pool(
            1,
            |_| Ok::<_, String>(10_u64),
            |_| Ok::<_, String>(10_u64),
            |state, job: u64| Ok::<_, String>(std::hint::black_box(*state + job)),
            |_| TargetCounters::new(),
            |pool| -> Result<Vec<u64>, Box<dyn std::error::Error>> {
                pool.send(0, 1)?;
                pool.send(1, 2)?;
                let values = (0..2)
                    .map(|_| Ok(pool.receive()?.outcome?))
                    .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
                pool.close();
                Ok(values)
            },
        )
        .expect("worker pool");
        assert_eq!(values, vec![11, 12]);
        assert_eq!(workers[0].jobs, 2);
        if super::super::campaign::worker_cpu_profile_enabled() {
            let value = workers[0]
                .execute_cpu
                .as_ref()
                .expect("enabled execute CPU");
            assert_eq!(value.calls, 2);
            assert_eq!((value.errors, value.panics), (0, 0));
            if cfg!(target_os = "linux") {
                assert!(value.complete);
                assert_eq!(value.samples, 2);
                assert!(value.total_cpu_ns.expect("CPU total") > 0);
            } else {
                assert!(!value.supported);
                assert!(!value.complete);
                assert_eq!(value.total_cpu_ns, None);
            }
        } else {
            assert!(workers[0].execute_cpu.is_none());
        }
    }

    #[test]
    fn idle_bitmap_preserves_order_across_words_and_replaces_existing_hints() {
        let mut idle = super::IdleWorkers::new(130);
        for worker in [129, 64, 0] {
            idle.insert(worker, Default::default());
        }
        assert_eq!(idle.keys().copied().collect::<Vec<_>>(), [0, 64, 129]);
        idle.remove(&0);
        idle.insert(64, Default::default());
        assert_eq!(idle.len(), 2);
        assert_eq!(idle.first_key_value().unwrap().0, &64);
        idle.remove(&64);
        idle.remove(&64);
        assert_eq!(idle.first_key_value().unwrap().0, &129);
        idle.remove(&129);
        assert!(idle.is_empty());
        assert!(idle.first_key_value().is_none());
    }

    #[test]
    fn indexed_queue_reuses_fixed_payload_slots_without_assigned_key_allocations() {
        let mut queue = super::JobQueue::from_parts(
            std::collections::BTreeMap::new(),
            std::collections::BTreeMap::new(),
            2,
            true,
        );
        queue.slots.resize_with(2, || None);
        queue.jobs.reserve(2);
        queue.slot_index = Some(|key: &(u64, usize)| key.1);
        let capacities = (queue.slots.capacity(), queue.jobs.capacity());
        for round in 0..1_000 {
            queue.enqueue((round, 1), round, None).unwrap();
            let address = std::ptr::from_ref(&queue.slots[1].as_ref().unwrap().queued.job);
            assert_eq!(
                queue.enqueue((round, 1), 0, None),
                Err(WorkerPoolError::DuplicateJob)
            );
            queue.idle.insert(0, Default::default());
            assert!(queue.assign(|worker| assert_eq!(worker, 0)));
            assert_eq!(
                std::ptr::from_ref(&queue.slots[1].as_ref().unwrap().queued.job),
                address
            );
            assert_eq!(
                queue.enqueue((round + 1, 1), 0, None),
                Err(WorkerPoolError::DuplicateJob)
            );
            assert_eq!(queue.take_assigned(0), Some(((round, 1), round)));
            assert!(queue.keys.is_empty());
            assert_eq!((queue.slots.capacity(), queue.jobs.capacity()), capacities);
        }
        assert_eq!(
            queue.enqueue((0, 2), 0, None),
            Err(WorkerPoolError::DuplicateJob)
        );
        assert!(queue.slots.iter().all(Option::is_none));
    }

    #[test]
    fn indexed_pool_impossible_capacity_is_a_normal_error() {
        with_worker_pool(
            1,
            |_| Ok::<_, String>(()),
            |_| Ok::<_, String>(()),
            |(), job: u8| Ok::<_, String>(job),
            |_| TargetCounters::new(),
            |pool| -> Result<(), Box<dyn std::error::Error>> {
                pool.wait_ready(1)?;
                assert_eq!(
                    pool.configure_indexed(usize::MAX, |key: &usize| *key),
                    Err(WorkerPoolError::QueueCapacity)
                );
                Ok(())
            },
        )
        .unwrap();
    }

    #[test]
    fn timed_receive_distinguishes_waiting_ready_and_closed_workers() {
        with_worker_pool(
            1,
            |_| Ok::<_, String>(()),
            |_| Ok::<_, String>(()),
            |(), value: u8| Ok::<_, String>(value + 1),
            |_| TargetCounters::new(),
            |pool| -> Result<(), Box<dyn std::error::Error>> {
                pool.wait_ready(1)?;
                assert!(pool.receive_timeout(Duration::from_millis(10))?.is_none());
                pool.send(7_u8, 10)?;
                let reply = pool.receive_timeout(Duration::from_secs(5))?.unwrap();
                assert_eq!(reply.key, 7);
                assert_eq!(reply.outcome, Ok(11));
                pool.close();
                assert!(matches!(
                    pool.receive_timeout(Duration::from_secs(5)),
                    Err(WorkerPoolError::RepliesClosed)
                ));
                Ok(())
            },
        )
        .unwrap();
    }

    #[test]
    fn parked_workers_take_a_burst_and_wake_again_after_reparking() {
        type Job = (u8, mpsc::Sender<u8>, mpsc::Receiver<()>);
        let (started, arrivals) = mpsc::channel();
        let (_, workers) = with_worker_pool(
            3,
            |_| Ok::<_, String>(()),
            |_| Ok::<_, String>(()),
            |(), (value, started, release): Job| {
                started.send(value).map_err(|error| error.to_string())?;
                release
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(|error| error.to_string())?;
                Ok::<_, String>(value)
            },
            |_| TargetCounters::new(),
            |pool| -> Result<(), Box<dyn std::error::Error>> {
                pool.wait_ready(3)?;
                for round in 0..2 {
                    pool.wait_idle(&[0, 1, 2])?;
                    let mut release = Vec::new();
                    for value in round * 3..round * 3 + 3 {
                        let (ready, blocked) = mpsc::channel();
                        release.push(ready);
                        pool.send(value, (value, started.clone(), blocked))?;
                    }
                    let mut observed = (0..3)
                        .map(|_| arrivals.recv_timeout(Duration::from_secs(5)))
                        .collect::<Result<Vec<_>, _>>()?;
                    observed.sort_unstable();
                    assert_eq!(observed, (round * 3..round * 3 + 3).collect::<Vec<_>>());
                    for ready in release {
                        ready.send(())?;
                    }
                    let mut replies = (0..3)
                        .map(|_| -> Result<_, Box<dyn std::error::Error>> {
                            Ok(pool
                                .receive_timeout(Duration::from_secs(5))?
                                .ok_or("missing reply")?
                                .outcome?)
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    replies.sort_unstable();
                    assert_eq!(replies, observed);
                }
                pool.close();
                Ok(())
            },
        )
        .expect("parked workers must consume both bursts");
        assert_eq!(workers.iter().map(|worker| worker.jobs).sum::<u64>(), 6);
    }

    #[test]
    fn routed_workers_receive_owned_jobs_and_drain_after_close() {
        use crate::search::locality::{LocalityKey, WorkerLocality};

        let key = LocalityKey([4; 32]);
        let result = super::with_worker_pool_routed(
            3,
            Ok::<_, String>,
            Ok::<_, String>,
            |worker, job: u8| Ok::<_, String>((*worker, job)),
            |_| TargetCounters::new(),
            |worker| WorkerLocality::new([(key, u8::from(*worker == 2))]),
            || {},
            || None::<()>,
            None,
            |pool| -> Result<_, Box<dyn std::error::Error>> {
                pool.wait_ready(3)?;
                let shared = pool.jobs.as_ref().unwrap();
                let mut queue = shared.queue.lock().unwrap();
                while queue.idle.len() != 3 {
                    queue = shared.state_changed.wait(queue).unwrap();
                }
                drop(queue);
                pool.dispatch_local(0, 9, Some(key))?;
                pool.close();
                let reply = pool.receive()?;
                assert_eq!(reply.worker, 2);
                Ok(reply.outcome?)
            },
        )
        .unwrap();
        assert_eq!(result.0, (2, 9));
        assert_eq!(result.1.iter().map(|worker| worker.jobs).sum::<u64>(), 1);
    }

    #[test]
    fn a_large_parked_pool_repeatedly_wakes_the_chosen_owner() {
        use crate::search::locality::{LocalityKey, WorkerLocality};

        let ids = (0..128).collect::<Vec<u32>>();
        let (_, workers) = super::with_worker_pool_routed(
            128,
            Ok::<_, String>,
            Ok::<_, String>,
            |worker, job: u32| Ok::<_, String>((*worker, job)),
            |_| TargetCounters::new(),
            |worker| WorkerLocality::new([(LocalityKey([*worker as u8; 32]), 1)]),
            || {},
            || None::<()>,
            None,
            |pool| -> Result<(), Box<dyn std::error::Error>> {
                pool.wait_ready(128)?;
                for (reservation, owner) in
                    [127_u32, 64, 1, 126, 0, 63, 125, 2].into_iter().enumerate()
                {
                    pool.wait_idle(&ids)?;
                    pool.dispatch_local(reservation, owner, Some(LocalityKey([owner as u8; 32])))?;
                    let reply = pool
                        .receive_timeout(Duration::from_secs(5))?
                        .ok_or("chosen owner did not wake")?;
                    assert_eq!(reply.worker, owner);
                    assert_eq!(reply.key, reservation);
                    assert_eq!(reply.outcome, Ok((owner, owner)));
                }
                pool.close();
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(workers.iter().map(|worker| worker.jobs).sum::<u64>(), 8);
    }

    #[test]
    fn duplicate_keys_are_rejected_while_assigned_and_released_when_claimed() {
        use crate::search::locality::WorkerLocality;
        use std::sync::Arc;

        let shared = Arc::new(super::SharedJobs::new(2));
        shared
            .queue
            .lock()
            .unwrap()
            .idle
            .insert(1, WorkerLocality::default());
        let (_, reply_receiver) = mpsc::channel();
        let (_, startup_receiver) = mpsc::channel();
        let pool = super::WorkerPool::<u8, u8, ()> {
            jobs: Some(Arc::clone(&shared)),
            reply_receiver,
            startup_receiver,
        };
        pool.send(7, 10).unwrap();
        assert_eq!(pool.send(7, 99), Err(WorkerPoolError::DuplicateJob));
        pool.send(8, 11).unwrap();
        assert_eq!(pool.send(8, 99), Err(WorkerPoolError::DuplicateJob));
        {
            let mut queue = shared.queue.lock().unwrap();
            assert_eq!(queue.take_assigned(1), Some((7, 10)));
            assert!(queue.assigned_keys().is_empty());
            queue.idle.insert(1, WorkerLocality::default());
        }
        pool.send(7, 12).unwrap();
        let mut queue = shared.queue.lock().unwrap();
        assert_eq!(queue.take_assigned(1), Some((7, 12)));
        assert_eq!(
            queue.pop_queued().map(|(key, value)| (key, value.job)),
            Some((8, 11))
        );
        assert!(queue.assigned_keys().is_empty());
        drop(queue);
    }

    #[test]
    fn assignment_chooses_the_best_idle_owner_for_the_earliest_job() {
        use super::{JobQueue, QueuedJob};
        use crate::search::locality::{LocalityKey, WorkerLocality};
        use std::collections::BTreeMap;

        let parent = LocalityKey([1; 32]);
        let other = LocalityKey([2; 32]);
        let mut queue = JobQueue::from_parts(
            BTreeMap::from([
                (
                    10,
                    QueuedJob {
                        job: 10,
                        locality: Some(parent),
                    },
                ),
                (
                    20,
                    QueuedJob {
                        job: 20,
                        locality: Some(other),
                    },
                ),
                (
                    30,
                    QueuedJob {
                        job: 30,
                        locality: Some(other),
                    },
                ),
            ]),
            BTreeMap::from([
                (0, WorkerLocality::new([(other, 1)])),
                (1, WorkerLocality::new([(parent, 0)])),
                (2, WorkerLocality::new([(parent, 1)])),
            ]),
            3,
            true,
        );
        let mut notified = Vec::new();
        assert!(queue.assign(|worker| notified.push(worker)));
        assert_eq!(notified, [2, 0, 1]);
        assert_eq!(queue.assigned_keys(), [10, 20, 30]);
        assert_eq!(
            [
                queue.take_assigned(0),
                queue.take_assigned(1),
                queue.take_assigned(2)
            ],
            [Some((20, 20)), Some((30, 30)), Some((10, 10))]
        );
        assert!(queue.jobs.is_empty());
        assert!(queue.idle.is_empty());
        assert!(!queue.assign(|_| panic!("assignment notified without work")));
    }

    #[test]
    fn a_busy_owner_never_blocks_the_head_or_gives_a_later_local_job_priority() {
        use super::{JobQueue, QueuedJob};
        use crate::search::locality::{LocalityKey, WorkerLocality};
        use std::collections::BTreeMap;

        let busy_parent = LocalityKey([1; 32]);
        let local_parent = LocalityKey([2; 32]);
        let mut queue = JobQueue::from_parts(
            BTreeMap::from([
                (
                    10,
                    QueuedJob {
                        job: 10,
                        locality: Some(busy_parent),
                    },
                ),
                (
                    20,
                    QueuedJob {
                        job: 20,
                        locality: Some(local_parent),
                    },
                ),
            ]),
            BTreeMap::from([(1, WorkerLocality::new([(local_parent, 9)]))]),
            2,
            false,
        );
        assert!(queue.assign(|_| {}));
        assert!(!queue.assign(|_| panic!("backlog woke a worker without an assignment")));
        assert_eq!(queue.take_assigned(1), Some((10, 10)));
        assert!(queue.assigned_keys().is_empty());
        assert_eq!(queue.queued_keys(), [20]);
        queue.idle.insert(1, WorkerLocality::default());
        assert!(queue.assign(|_| {}));
        assert_eq!(queue.take_assigned(1), Some((20, 20)));
        assert!(queue.assigned_keys().is_empty());
    }

    #[test]
    fn assignment_without_a_match_breaks_idle_worker_ties_by_identifier() {
        use super::{JobQueue, QueuedJob};
        use crate::search::locality::WorkerLocality;
        use std::collections::BTreeMap;

        let mut queue = JobQueue::from_parts(
            BTreeMap::from([(
                7,
                QueuedJob {
                    job: (),
                    locality: None,
                },
            )]),
            BTreeMap::from([
                (5, WorkerLocality::default()),
                (2, WorkerLocality::default()),
            ]),
            6,
            true,
        );
        assert!(queue.assign(|_| {}));
        assert_eq!(queue.take_assigned(2), Some((7, ())));
        assert_eq!(queue.idle.keys().copied().collect::<Vec<_>>(), [5]);
    }

    #[test]
    fn idle_workers_take_queued_jobs_while_one_worker_is_blocked() {
        let (release, blocked) = mpsc::channel();
        let replies = with_worker_pool(
            3,
            |_| Ok::<_, String>(()),
            |_| Ok::<_, String>(()),
            |_, (release, value): (Option<mpsc::Receiver<()>>, u8)| {
                if let Some(release) = release {
                    release.recv().map_err(|e| e.to_string())?;
                }
                Ok::<_, String>(value)
            },
            |_| TargetCounters::new(),
            move |pool| -> Result<Vec<u8>, Box<dyn std::error::Error>> {
                pool.send(0, (Some(blocked), 0))?;
                for value in 1..=6 {
                    pool.send(value, (None, value))?;
                }
                let mut values = (0..6)
                    .map(|_| Ok(pool.receive()?.outcome?))
                    .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
                values.sort_unstable();
                release.send(())?;
                values.push(pool.receive()?.outcome?);
                pool.close();
                Ok(values)
            },
        )
        .expect("queued jobs run around a blocked worker");
        assert_eq!(replies.0, vec![1, 2, 3, 4, 5, 6, 0]);
        assert_eq!(replies.1.iter().map(|worker| worker.jobs).sum::<u64>(), 7);
    }

    #[test]
    fn a_free_worker_takes_the_queued_job_with_the_smallest_key() {
        let (release, blocked) = mpsc::channel();
        let replies = with_worker_pool(
            1,
            |_| Ok::<_, String>(()),
            |_| Ok::<_, String>(()),
            |_, (release, value): (Option<mpsc::Receiver<()>>, u8)| {
                if let Some(release) = release {
                    release.recv().map_err(|e| e.to_string())?;
                }
                Ok::<_, String>(value)
            },
            |_| TargetCounters::new(),
            move |pool| -> Result<Vec<u8>, Box<dyn std::error::Error>> {
                pool.send(0, (Some(blocked), 0))?;
                for value in [30, 10, 20] {
                    pool.send(value, (None, value))?;
                }
                assert_eq!(
                    pool.send(10, (None, 10)),
                    Err(WorkerPoolError::DuplicateJob)
                );
                release.send(())?;
                let values = (0..4)
                    .map(|_| Ok(pool.receive()?.outcome?))
                    .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
                pool.close();
                Ok(values)
            },
        )
        .expect("a single worker runs queued jobs in key order");
        assert_eq!(replies.0, vec![0, 10, 20, 30]);
    }

    #[test]
    fn try_receive_distinguishes_ready_empty_and_disconnected() {
        let (sender, receiver) = mpsc::channel();
        let (_startup_sender, startup_receiver) = mpsc::channel();
        let pool = WorkerPool::<u8, (), u64> {
            jobs: None,
            reply_receiver: receiver,
            startup_receiver,
        };
        assert!(
            pool.try_receive()
                .expect("empty channel remains open")
                .is_none()
        );
        sender
            .send(WorkerReply {
                worker: 7,
                key: 0,
                outcome: Ok(11),
                observation: Default::default(),
            })
            .expect("queue reply");
        let reply = pool
            .try_receive()
            .expect("ready channel")
            .expect("queued reply");
        assert_eq!((reply.worker, reply.outcome), (7, Ok(11)));
        drop(sender);
        assert!(matches!(
            pool.try_receive(),
            Err(WorkerPoolError::RepliesClosed)
        ));
        assert_eq!(pool.send(0, ()), Err(WorkerPoolError::QueueClosed));
    }

    #[test]
    fn one_worker_uses_the_same_pool_path() {
        let replies = with_worker_pool(
            1,
            |_| Ok::<_, String>(10_u64),
            |_| Ok::<_, String>(10_u64),
            |state, job: u64| Ok::<_, String>(*state + job),
            |state| TargetCounters::from([("state".to_owned(), *state)]),
            |pool| -> Result<Vec<(u32, u64)>, Box<dyn std::error::Error>> {
                pool.send(0, 7)?;
                let reply = pool.receive()?;
                pool.close();
                Ok(vec![(reply.worker, reply.outcome?)])
            },
        )
        .expect("coordinate one worker");
        let (replies, telemetry) = replies;
        assert_eq!(replies, vec![(0, 17)]);
        assert_eq!(telemetry.len(), 1);
        assert_eq!(telemetry[0].jobs, 1);
        assert_eq!(telemetry[0].target["state"], 10);
    }

    #[test]
    fn worker_state_can_remain_on_the_thread_that_constructed_it() {
        let value = with_worker_pool(
            1,
            |_| Ok::<_, String>(Rc::new(Cell::new(4_u64))),
            |_| Ok::<_, String>(Rc::new(Cell::new(4_u64))),
            |state, increment: u64| {
                state.set(state.get().saturating_add(increment));
                Ok::<_, String>(state.get())
            },
            |_| TargetCounters::new(),
            |pool| -> Result<u64, Box<dyn std::error::Error>> {
                pool.send(0, 3)?;
                Ok(pool.receive()?.outcome?)
            },
        )
        .expect("coordinate worker-local state")
        .0;
        assert_eq!(value, 7);
    }

    #[test]
    fn a_dispatch_after_every_worker_failed_reports_the_worker_error() {
        let error = with_worker_pool(
            2,
            |_| Err::<(), _>("boot failed".to_owned()),
            |_| Ok::<_, String>(()),
            |(), job: u64| Ok::<_, String>(job),
            |()| TargetCounters::new(),
            |pool| -> Result<(), Box<dyn std::error::Error>> {
                let mut key = 0_u64;
                while pool.dispatch(key, 1).is_ok() {
                    key += 1;
                    std::thread::yield_now();
                }
                pool.dispatch(key + 1, 1)
            },
        )
        .unwrap_err();
        assert!(
            [
                "boot failed",
                "every campaign worker exited before taking the next job"
            ]
            .contains(&error.to_string().as_str()),
            "{error}"
        );
    }

    #[test]
    fn a_worker_that_fails_to_start_after_the_last_admission_fails_the_pool() {
        let (started, wait_for_start) = mpsc::channel();
        let wait_for_start = std::sync::Mutex::new(wait_for_start);
        let error = with_worker_pool(
            2,
            |worker| {
                if worker == 1 {
                    wait_for_start.lock().unwrap().recv().unwrap();
                    return Err("late boot failed".to_owned());
                }
                Ok(())
            },
            |_| Ok::<_, String>(()),
            |(), job: u64| Ok::<_, String>(job),
            |()| TargetCounters::new(),
            |pool| -> Result<u64, Box<dyn std::error::Error>> {
                pool.send(0, 5)?;
                let value = pool.receive()?.outcome?;
                started.send(()).unwrap();
                Ok(value)
            },
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "late boot failed");
    }

    #[test]
    fn startup_panic_has_a_worker_key_and_does_not_hold_the_barrier() {
        let error = with_worker_pool(
            2,
            |worker| -> Result<(), String> {
                if worker == 0 {
                    panic!("injected startup panic");
                }
                Ok(())
            },
            |_| Ok::<_, String>(()),
            |(), (): ()| Ok::<_, String>(()),
            |()| TargetCounters::new(),
            |pool: &mut WorkerPool<u8, (), ()>| -> Result<(), Box<dyn std::error::Error>> {
                pool.wait_ready(2)?;
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "campaign worker 0 failed to start: worker panicked during startup"
        );
    }

    #[test]
    fn startup_barrier_reports_a_worker_that_never_sent_status() {
        let (_reply_sender, reply_receiver) = mpsc::channel();
        let (startup_sender, startup_receiver) = mpsc::channel();
        drop(startup_sender);
        let pool = WorkerPool::<u8, (), ()> {
            jobs: None,
            reply_receiver,
            startup_receiver,
        };
        assert_eq!(
            pool.wait_ready(1).unwrap_err(),
            "campaign worker 0 exited before reporting startup"
        );
    }

    #[test]
    fn execution_panic_replies_with_its_key_and_replaces_the_target() {
        for workers in [1, 2] {
            let error = with_worker_pool(
                workers,
                |_| Ok::<_, String>(()),
                |_| Ok::<_, String>(()),
                |(), panic: bool| -> Result<(), String> {
                    if panic {
                        panic!("injected execution panic");
                    }
                    Ok(())
                },
                |()| TargetCounters::new(),
                |pool| -> Result<(), Box<dyn std::error::Error>> {
                    pool.wait_ready(workers)?;
                    pool.send(0_u8, true)?;
                    let reply = pool.receive()?;
                    assert_eq!(reply.key, 0);
                    assert_eq!(
                        reply.outcome,
                        Err("worker panicked during execution".to_owned())
                    );
                    pool.send(1_u8, false)?;
                    let recovered = pool.receive()?;
                    assert_eq!(recovered.key, 1);
                    assert_eq!(recovered.outcome, Ok(()));
                    pool.close();
                    Ok(())
                },
            )
            .unwrap_err();
            assert_eq!(error.to_string(), "worker panicked during execution");
        }
    }

    #[test]
    fn an_earlier_key_queued_after_a_panic_still_gets_a_reply() {
        let (started, entered) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let error = with_worker_pool(
            1,
            |_| Ok::<_, String>(()),
            |_| Ok::<_, String>(()),
            move |(), (block, value): (Option<mpsc::Receiver<()>>, u8)| {
                if let Some(block) = block {
                    started.send(()).unwrap();
                    block.recv().unwrap();
                    panic!("later-planned job panicked");
                }
                Ok::<_, String>(value)
            },
            |()| TargetCounters::new(),
            |pool| -> Result<(), Box<dyn std::error::Error>> {
                pool.wait_ready(1)?;
                pool.send(20, (Some(blocked), 20))?;
                entered.recv_timeout(Duration::from_secs(5))?;
                pool.send(10, (None, 10))?;
                release.send(())?;
                let failed = pool.receive()?;
                assert_eq!(failed.key, 20);
                assert_eq!(
                    failed.outcome,
                    Err("worker panicked during execution".to_owned())
                );
                let earlier = pool.receive()?;
                assert_eq!(earlier.key, 10);
                assert_eq!(earlier.outcome, Ok(10));
                pool.close();
                Ok(())
            },
        );
        assert_eq!(
            error.unwrap_err().to_string(),
            "worker panicked during execution"
        );
    }

    #[test]
    fn failed_recovery_disconnects_after_the_keyed_panic_reply() {
        let (started, entered) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let error = with_worker_pool(
            1,
            |_| Ok::<_, String>(()),
            |_| Err::<(), _>("target recreation failed".to_owned()),
            move |(), (block, value): (Option<mpsc::Receiver<()>>, u8)| {
                if let Some(block) = block {
                    started.send(()).unwrap();
                    block.recv().unwrap();
                    panic!("later-planned job panicked");
                }
                Ok::<_, String>(value)
            },
            |()| TargetCounters::new(),
            |pool| -> Result<(), Box<dyn std::error::Error>> {
                pool.wait_ready(1)?;
                pool.send(20, (Some(blocked), 20))?;
                entered.recv_timeout(Duration::from_secs(5))?;
                pool.send(10, (None, 10))?;
                release.send(())?;
                let failed = pool.receive()?;
                assert_eq!(failed.key, 20);
                assert_eq!(
                    failed.outcome,
                    Err("worker panicked during execution".to_owned())
                );
                assert!(matches!(
                    pool.receive(),
                    Err(WorkerPoolError::RepliesClosed)
                ));
                pool.close();
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "worker target could not be recreated after panic"
        );
    }

    #[test]
    fn failed_recovery_retires_without_taking_work_from_a_healthy_worker() {
        let (started, entered) = mpsc::channel();
        let (release_panic, blocked_panic) = mpsc::channel();
        let (release_healthy, blocked_healthy) = mpsc::channel();
        let error = with_worker_pool(
            2,
            |_| Ok::<_, String>(()),
            |_| Err::<(), _>("target recreation failed".to_owned()),
            move |(), (release, value): (Option<mpsc::Receiver<()>>, u8)| {
                if let Some(release) = release {
                    started.send(value).unwrap();
                    release.recv().unwrap();
                    if value == 20 {
                        panic!("later-planned job panicked");
                    }
                }
                Ok::<_, String>(value)
            },
            |()| TargetCounters::new(),
            |pool| -> Result<(), Box<dyn std::error::Error>> {
                pool.wait_ready(2)?;
                pool.send(20, (Some(blocked_panic), 20))?;
                pool.send(30, (Some(blocked_healthy), 30))?;
                let first_claim = entered.recv_timeout(Duration::from_secs(5));
                let second_claim = entered.recv_timeout(Duration::from_secs(5));
                if first_claim.is_err() || second_claim.is_err() {
                    let _ = release_panic.send(());
                    let _ = release_healthy.send(());
                }
                let mut claimed = [first_claim?, second_claim?];
                claimed.sort_unstable();
                if claimed != [20, 30] {
                    let _ = release_panic.send(());
                    let _ = release_healthy.send(());
                }
                assert_eq!(claimed, [20, 30]);
                pool.send(10, (None, 10))?;
                release_panic.send(())?;
                let shared = pool.jobs.as_ref().unwrap();
                let queue = shared.queue.lock().unwrap();
                let queue = shared
                    .state_changed
                    .wait_timeout_while(queue, Duration::from_secs(5), |queue| queue.workers != 1)
                    .unwrap()
                    .0;
                let survivors = queue.workers;
                drop(queue);
                release_healthy.send(())?;
                assert_eq!(survivors, 1);
                let mut replies = (0..3)
                    .map(|_| pool.receive().map(|reply| (reply.key, reply.outcome)))
                    .collect::<Result<Vec<_>, _>>()?;
                replies.sort_by_key(|(key, _)| *key);
                assert_eq!(replies[0], (10, Ok(10)));
                assert_eq!(
                    replies[1],
                    (20, Err("worker panicked during execution".to_owned()))
                );
                assert_eq!(replies[2], (30, Ok(30)));
                pool.close();
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "worker target could not be recreated after panic"
        );
    }

    #[test]
    fn dispatch_after_all_workers_exit_preserves_buffered_replies() {
        let error = with_worker_pool(
            1,
            |_| Ok::<_, String>(()),
            |_| Err::<(), _>("target recreation failed".to_owned()),
            |(), panic: bool| -> Result<u8, String> {
                if panic {
                    panic!("injected execution panic");
                }
                Ok(10)
            },
            |()| TargetCounters::new(),
            |pool| -> Result<(), Box<dyn std::error::Error>> {
                pool.wait_ready(1)?;
                pool.send(10_u8, false)?;
                pool.send(20_u8, true)?;
                let shared = pool.jobs.as_ref().unwrap();
                let queue = shared.queue.lock().unwrap();
                let queue = shared
                    .state_changed
                    .wait_timeout_while(queue, Duration::from_secs(5), |queue| queue.workers != 0)
                    .unwrap()
                    .0;
                assert_eq!(queue.workers, 0);
                drop(queue);
                pool.dispatch(15, false)?;
                let success = pool.receive()?;
                assert_eq!(success.key, 10);
                assert_eq!(success.outcome, Ok(10));
                let failure = pool.receive()?;
                assert_eq!(failure.key, 20);
                assert_eq!(
                    failure.outcome,
                    Err("worker panicked during execution".to_owned())
                );
                assert!(matches!(
                    pool.receive(),
                    Err(WorkerPoolError::RepliesClosed)
                ));
                pool.close();
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "worker target could not be recreated after panic"
        );
    }

    #[test]
    fn finalization_panic_fails_the_pool_after_a_clean_coordinator_return() {
        let error = with_worker_pool(
            1,
            |_| Ok::<_, String>(()),
            |_| Ok::<_, String>(()),
            |(), (): ()| Ok::<_, String>(()),
            |()| panic!("injected finalization panic"),
            |pool: &mut WorkerPool<u8, (), ()>| -> Result<(), Box<dyn std::error::Error>> {
                pool.wait_ready(1)?;
                pool.close();
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "campaign worker 0 panicked during finalization"
        );
    }

    #[test]
    fn uncaught_worker_panic_fails_the_pool_after_a_clean_coordinator_return() {
        struct PanicsOnDrop;

        impl Drop for PanicsOnDrop {
            fn drop(&mut self) {
                panic!("injected target drop panic");
            }
        }

        let error = with_worker_pool(
            1,
            |_| Ok::<_, String>(PanicsOnDrop),
            |_| Ok::<_, String>(PanicsOnDrop),
            |_, (): ()| Ok::<_, String>(()),
            |_| TargetCounters::new(),
            |pool: &mut WorkerPool<u8, (), ()>| -> Result<(), Box<dyn std::error::Error>> {
                pool.wait_ready(1)?;
                pool.close();
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "campaign worker 0 panicked outside the handled worker phases"
        );
    }
    #[test]
    fn observed_queued_replies_are_accounted_after_coordinator_error_or_unwind() {
        for unwind in [false, true] {
            let diagnostics = crate::search::job_observation::ObservationDiagnostics::default();
            let (sent, received) = mpsc::sync_channel(0);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                super::with_worker_pool_observed(
                    1, |_| Ok::<_,String>(()), |_| Ok::<_,String>(()),
                    |_, _: ()| { sent.send(()).unwrap();Ok::<_,String>(()) },
                    |_| Default::default(), || {}, || Some([17u64;2]), Some(diagnostics.clone()),
                    |pool: &mut WorkerPool<u8,(),(),[u64;2]>| -> Result<(),Box<dyn std::error::Error>> {
                        pool.wait_ready(1)?;pool.dispatch(0,())?;
                        received.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
                        if unwind { panic!("coordinator unwound"); }
                        Err("primary coordinator error".into())
                    },
                )
            }));
            if unwind {
                assert!(result.is_err());
            } else {
                assert_eq!(
                    result.unwrap().unwrap_err().to_string(),
                    "primary coordinator error"
                );
            }
            let counts = diagnostics.snapshot();
            assert_eq!(counts.eligible, 1);
            assert_eq!(counts.collected, 1);
            assert_eq!(counts.delivered, 0);
            assert_eq!(counts.discarded, 1);
        }
    }

    #[test]
    fn observed_execution_panic_drains_before_target_recovery() {
        thread_local! { static VALUE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) }; }
        let diagnostics = crate::search::job_observation::ObservationDiagnostics::default();
        let result = super::with_worker_pool_observed(
            1,
            |_| Ok::<_, String>(()),
            |_| {
                VALUE.with(|v| v.set(99));
                Ok::<_, String>(())
            },
            |_, _: ()| -> Result<(), String> {
                VALUE.with(|v| v.set(7));
                panic!("execution");
            },
            |_| Default::default(),
            || VALUE.with(|v| v.set(1)),
            || Some(VALUE.with(|v| v.get())),
            Some(diagnostics.clone()),
            |pool: &mut WorkerPool<u8, (), (), u64>| -> Result<(), Box<dyn std::error::Error>> {
                pool.wait_ready(1)?;
                pool.dispatch(0, ())?;
                let reply = pool.receive()?;
                assert_eq!(reply.observation.receipt.payload, Some(7));
                assert_eq!(
                    reply.observation.receipt.status,
                    crate::search::job_observation::CollectionStatus::Collected
                );
                assert!(reply.outcome.is_err());
                pool.close();
                Ok(())
            },
        );
        assert!(result.is_err());
        let counts = diagnostics.snapshot();
        assert_eq!(counts.collected, 1);
        assert_eq!(counts.discarded, 1);
    }
    #[test]
    fn observation_hook_panics_are_visible_and_do_not_replace_primary_errors() {
        for (begin_panics, drain_panics) in [(true, false), (false, true), (true, true)] {
            let diagnostics = crate::search::job_observation::ObservationDiagnostics::default();
            let result = super::with_worker_pool_observed(
                1,
                |_| Ok::<_, String>(()),
                |_| Ok::<_, String>(()),
                |_, _: ()| Err::<(), _>("primary execution error".to_owned()),
                |_| Default::default(),
                || {
                    if begin_panics {
                        panic!("begin");
                    }
                },
                || -> Option<u64> {
                    if drain_panics {
                        panic!("drain");
                    }
                    Some(9)
                },
                Some(diagnostics.clone()),
                |pool: &mut WorkerPool<u8, (), (), u64>| -> Result<(), Box<dyn std::error::Error>> {
                    pool.wait_ready(1)?;
                    pool.dispatch(0, ())?;
                    let reply = pool.receive()?;
                    assert_eq!(reply.outcome, Err("primary execution error".to_owned()));
                    assert!(reply.observation.receipt.payload.is_none());
                    assert_eq!(
                        reply.observation.receipt.status,
                        if drain_panics {
                            crate::search::job_observation::CollectionStatus::DrainPanicked
                        } else {
                            crate::search::job_observation::CollectionStatus::BeginPanicked
                        }
                    );
                    Err("primary coordinator error".into())
                },
            );
            assert_eq!(result.unwrap_err().to_string(), "primary coordinator error");
            let counts = diagnostics.snapshot();
            assert_eq!(counts.eligible, 1);
            assert_eq!(counts.begin_panics, u64::from(begin_panics));
            assert_eq!(counts.drain_panics, u64::from(drain_panics));
            assert_eq!(counts.collected, 0);
        }
    }
    #[test]
    fn observed_receipts_keep_the_actual_executing_worker_identity() {
        thread_local! { static MARK: std::cell::Cell<u32> = const { std::cell::Cell::new(u32::MAX) }; }
        let diagnostics = crate::search::job_observation::ObservationDiagnostics::default();
        let (entered, entries) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let released = std::sync::Mutex::new(released);
        super::with_worker_pool_observed(
            2,
            Ok::<_, String>,
            Ok::<_, String>,
            |worker, _: ()| {
                MARK.with(|v| v.set(*worker));
                entered.send(*worker).unwrap();
                released.lock().unwrap().recv().unwrap();
                Ok::<_, String>(*worker)
            },
            |_| Default::default(),
            || {},
            || Some(MARK.with(|v| v.get())),
            Some(diagnostics.clone()),
            |pool: &mut WorkerPool<u8, (), u32, u32>| -> Result<(), Box<dyn std::error::Error>> {
                pool.wait_ready(2)?;
                pool.dispatch(0, ())?;
                pool.dispatch(1, ())?;
                let first = entries.recv_timeout(std::time::Duration::from_secs(5));
                let second = entries.recv_timeout(std::time::Duration::from_secs(5));
                let _ = release.send(());
                let _ = release.send(());
                pool.close();
                let first = first.unwrap();
                let second = second.unwrap();
                assert_ne!(first, second);
                let mut seen = std::collections::BTreeSet::new();
                for _ in 0..2 {
                    let reply = pool.receive()?;
                    let actual = reply.outcome.unwrap();
                    assert_eq!(reply.worker, actual);
                    assert_eq!(reply.observation.receipt.payload, Some(actual));
                    seen.insert(actual);
                }
                assert_eq!(seen, std::collections::BTreeSet::from([0, 1]));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(diagnostics.snapshot().collected, 2);
        assert_eq!(diagnostics.snapshot().discarded, 2);
    }
}
