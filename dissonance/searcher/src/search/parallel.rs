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
pub(crate) struct WorkerReply<Output> {
    pub(crate) worker: u32,
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
            }),
            ready: Condvar::new(),
        }
    }

    fn next(&self) -> Option<Job> {
        let mut queue = self.queue.lock().ok()?;
        loop {
            if let Some((_, job)) = queue.jobs.pop_first() {
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
}

struct WorkerExit<'a, Key, Job>(&'a SharedJobs<Key, Job>);

impl<Key, Job> Drop for WorkerExit<'_, Key, Job> {
    fn drop(&mut self) {
        if let Ok(mut queue) = self.0.queue.lock() {
            queue.workers = queue.workers.saturating_sub(1);
        }
    }
}

pub(crate) struct WorkerPool<Key: Ord, Job, Output> {
    jobs: Option<Arc<SharedJobs<Key, Job>>>,
    reply_receiver: mpsc::Receiver<WorkerReply<Output>>,
}

impl<Key: Ord, Job, Output> WorkerPool<Key, Job, Output> {
    pub(crate) fn send(&self, key: Key, job: Job) -> Result<(), WorkerPoolError> {
        let shared = self.jobs.as_ref().ok_or(WorkerPoolError::QueueClosed)?;
        let mut queue = shared
            .queue
            .lock()
            .map_err(|_| WorkerPoolError::WorkersExited)?;
        if queue.workers == 0 {
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
        match self.send(key, job) {
            Err(WorkerPoolError::WorkersExited) => Err(self
                .reply_receiver
                .try_iter()
                .find_map(|reply| reply.outcome.err())
                .map_or_else(|| WorkerPoolError::WorkersExited.into(), Into::into)),
            sent => sent.map_err(Into::into),
        }
    }

    pub(crate) fn close(&mut self) {
        if let Some(shared) = self.jobs.take() {
            shared.close();
        }
    }

    pub(crate) fn receive(&self) -> Result<WorkerReply<Output>, WorkerPoolError> {
        self.reply_receiver
            .recv()
            .map_err(|_| WorkerPoolError::RepliesClosed)
    }

    pub(crate) fn try_receive(&self) -> Result<Option<WorkerReply<Output>>, WorkerPoolError> {
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
        let (reply_sender, reply_receiver) = mpsc::channel::<WorkerReply<Output>>();
        let jobs = Arc::new(SharedJobs::<Key, Job>::new(workers as usize));
        let mut handles = Vec::with_capacity(workers as usize);
        for worker in 0..workers {
            let reply_sender = reply_sender.clone();
            let jobs = Arc::clone(&jobs);
            let initialize = &initialize;
            let execute = &execute;
            let finish = &finish;
            handles.push(scope.spawn(move || {
                let _exit = WorkerExit(&jobs);
                let schedstat_before = thread_schedstat();
                let booted = now();
                let mut telemetry = WorkerTelemetry::default();
                let mut state = match initialize(worker) {
                    Ok(state) => state,
                    Err(error) => {
                        let _ = reply_sender.send(WorkerReply {
                            worker,
                            outcome: Err(error.clone()),
                        });
                        return Err(error);
                    }
                };
                telemetry.boot_ns = nanos_since(booted);
                loop {
                    let waiting = now();
                    let Some(job) = jobs.next() else {
                        break;
                    };
                    telemetry.idle_ns = telemetry.idle_ns.saturating_add(nanos_since(waiting));
                    let started = now();
                    let outcome = execute(&mut state, job);
                    telemetry.busy_ns = telemetry.busy_ns.saturating_add(nanos_since(started));
                    telemetry.jobs = telemetry.jobs.saturating_add(1);
                    let failed = outcome.is_err();
                    if reply_sender.send(WorkerReply { worker, outcome }).is_err() || failed {
                        break;
                    }
                }
                telemetry.target = finish(&state);
                if let (Some((cpu_before, wait_before)), Some((cpu_after, wait_after))) =
                    (schedstat_before, thread_schedstat())
                {
                    telemetry.cpu_ns = Some(cpu_after.saturating_sub(cpu_before));
                    telemetry.cpu_wait_ns = Some(wait_after.saturating_sub(wait_before));
                }
                Ok(telemetry)
            }));
        }
        drop(reply_sender);
        let mut pool = WorkerPool {
            jobs: Some(jobs),
            reply_receiver,
        };
        let result = coordinate(&mut pool);
        drop(pool);
        let mut telemetry = Vec::with_capacity(handles.len());
        let mut startup_error = None;
        for handle in handles {
            match handle.join() {
                Ok(Ok(worker)) => telemetry.push(worker),
                Ok(Err(error)) => {
                    startup_error.get_or_insert(error);
                    telemetry.push(WorkerTelemetry::default());
                }
                Err(_) => telemetry.push(WorkerTelemetry::default()),
            }
        }
        let value = result?;
        match startup_error {
            Some(error) => Err(error.into()),
            None => Ok((value, telemetry)),
        }
    })
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc, sync::mpsc};

    use super::{WorkerPool, WorkerPoolError, WorkerReply, with_worker_pool};
    use crate::search::telemetry::TargetCounters;

    #[test]
    fn idle_workers_take_queued_jobs_while_one_worker_is_blocked() {
        let (release, blocked) = mpsc::channel();
        let replies = with_worker_pool(
            3,
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
        let pool = WorkerPool::<u8, (), u64> {
            jobs: None,
            reply_receiver: receiver,
        };
        assert!(
            pool.try_receive()
                .expect("empty channel remains open")
                .is_none()
        );
        sender
            .send(WorkerReply {
                worker: 7,
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
}
