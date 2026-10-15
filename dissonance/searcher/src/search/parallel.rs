// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::BTreeMap,
    error::Error,
    fmt,
    sync::{Arc, Condvar, Mutex, mpsc},
    thread,
};

use crate::search::telemetry::{
    TargetCounters, WorkerTelemetry, nanos_since, now, thread_schedstat,
};

#[derive(Debug)]
pub(crate) struct WorkerReply<Key, Output> {
    pub(crate) worker: u32,
    pub(crate) key: Key,
    pub(crate) outcome: Result<Output, String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WorkerPoolError {
    QueueClosed,
    DuplicateJob,
    WorkersExited,
    RepliesClosed,
}

impl fmt::Display for WorkerPoolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::QueueClosed => "campaign job queue is already closed",
            Self::DuplicateJob => "campaign job queue already holds a job with this order key",
            Self::WorkersExited => "every campaign worker exited before taking the next job",
            Self::RepliesClosed => "every campaign worker exited while a reply was expected",
        })
    }
}

impl Error for WorkerPoolError {}

struct JobQueue<Key, Job> {
    jobs: BTreeMap<Key, Job>,
    open: bool,
    workers: usize,
    failed_job: bool,
}

struct SharedJobs<Key, Job> {
    queue: Mutex<JobQueue<Key, Job>>,
    ready: Condvar,
}

impl<Key: Ord, Job> SharedJobs<Key, Job> {
    fn new(workers: usize) -> Self {
        Self {
            queue: Mutex::new(JobQueue {
                jobs: BTreeMap::new(),
                open: true,
                workers,
                failed_job: false,
            }),
            ready: Condvar::new(),
        }
    }

    fn next(&self) -> Option<(Key, Job)> {
        let mut queue = self.queue.lock().ok()?;
        loop {
            if let Some(job) = queue.jobs.pop_first() {
                return Some(job);
            }
            if !queue.open {
                return None;
            }
            queue = self.ready.wait(queue).ok()?;
        }
    }

    fn close(&self) {
        if let Ok(mut queue) = self.queue.lock() {
            queue.open = false;
        }
        self.ready.notify_all();
    }

    fn note_failed_job(&self) {
        if let Ok(mut queue) = self.queue.lock() {
            queue.failed_job = true;
        }
    }
}

struct WorkerExit<'a, Key, Job>(&'a SharedJobs<Key, Job>);

impl<Key, Job> Drop for WorkerExit<'_, Key, Job> {
    fn drop(&mut self) {
        if let Ok(mut queue) = self.0.queue.lock() {
            queue.workers = queue.workers.saturating_sub(1);
        }
        self.0.ready.notify_all();
    }
}

pub(crate) struct WorkerPool<Key: Ord, Job, Output> {
    jobs: Option<Arc<SharedJobs<Key, Job>>>,
    reply_receiver: mpsc::Receiver<WorkerReply<Key, Output>>,
    startup_receiver: mpsc::Receiver<(u32, Result<(), String>)>,
}

impl<Key: Ord, Job, Output> WorkerPool<Key, Job, Output> {
    pub(crate) fn send(&self, key: Key, job: Job) -> Result<(), WorkerPoolError> {
        let shared = self.jobs.as_ref().ok_or(WorkerPoolError::QueueClosed)?;
        let mut queue = shared
            .queue
            .lock()
            .map_err(|_| WorkerPoolError::WorkersExited)?;
        if queue.workers == 0 && !queue.failed_job {
            return Err(WorkerPoolError::WorkersExited);
        }
        if queue.jobs.contains_key(&key) {
            return Err(WorkerPoolError::DuplicateJob);
        }
        queue.jobs.insert(key, job);
        drop(queue);
        shared.ready.notify_one();
        Ok(())
    }

    pub(crate) fn dispatch(&self, key: Key, job: Job) -> Result<(), Box<dyn Error>> {
        self.send(key, job).map_err(Into::into)
    }

    pub(crate) fn close(&mut self) {
        if let Some(shared) = self.jobs.take() {
            shared.close();
        }
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

    pub(crate) fn receive(&self) -> Result<WorkerReply<Key, Output>, WorkerPoolError> {
        self.reply_receiver
            .recv()
            .map_err(|_| WorkerPoolError::RepliesClosed)
    }

    pub(crate) fn try_receive(&self) -> Result<Option<WorkerReply<Key, Output>>, WorkerPoolError> {
        match self.reply_receiver.try_recv() {
            Ok(reply) => Ok(Some(reply)),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err(WorkerPoolError::RepliesClosed),
        }
    }
}

impl<Key: Ord, Job, Output> Drop for WorkerPool<Key, Job, Output> {
    fn drop(&mut self) {
        self.close();
    }
}

pub(crate) fn with_worker_pool<State, Key, Job, Output, ResultValue, CoordinatorError>(
    workers: u32,
    initialize: impl Fn(u32) -> Result<State, String> + Sync,
    recover: impl Fn(u32) -> Result<State, String> + Sync,
    execute: impl Fn(&mut State, Job) -> Result<Output, String> + Sync,
    finish: impl Fn(&State) -> TargetCounters + Sync,
    coordinate: impl FnOnce(&mut WorkerPool<Key, Job, Output>) -> Result<ResultValue, CoordinatorError>,
) -> Result<(ResultValue, Vec<WorkerTelemetry>), CoordinatorError>
where
    Key: Ord + Send,
    Job: Send,
    Output: Send,
    CoordinatorError: From<String>,
{
    thread::scope(|scope| {
        let (reply_sender, reply_receiver) = mpsc::channel::<WorkerReply<Key, Output>>();
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
            handles.push(scope.spawn(move || {
                let _exit = WorkerExit(&jobs);
                let schedstat_before = thread_schedstat();
                let booted = now();
                let mut telemetry = WorkerTelemetry::default();
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
                    let Some((key, job)) = jobs.next() else {
                        break;
                    };
                    telemetry.idle_ns = telemetry.idle_ns.saturating_add(nanos_since(waiting));
                    let started = now();
                    let (outcome, panicked) = match state.as_mut() {
                        Some(target) => {
                            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                execute(target, job)
                            })) {
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
                    let sent = reply_sender.send(WorkerReply {
                        worker,
                        key,
                        outcome,
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
                if let Some(target) = state.as_ref() {
                    telemetry.target = finish(target);
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
        for handle in handles {
            match handle.join() {
                Ok(Ok(worker)) => telemetry.push(worker),
                Ok(Err(error)) => {
                    worker_error.get_or_insert(error);
                    telemetry.push(WorkerTelemetry::default());
                }
                Err(_) => telemetry.push(WorkerTelemetry::default()),
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

    use super::{WorkerPool, WorkerPoolError, WorkerReply, with_worker_pool};
    use crate::search::telemetry::TargetCounters;

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
                    .ready
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
                    .ready
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
}
