// SPDX-License-Identifier: AGPL-3.0-or-later

//! Game-neutral scoped worker pool for campaign job execution.

use std::{collections::VecDeque, error::Error, fmt, sync::mpsc, thread};

/// Credits cover both executing and completed-but-unadmitted jobs. Receiving a
/// result does not release a credit: only its ordered admission does.
pub(crate) struct ResultSlots {
    outstanding: Vec<usize>,
    available: VecDeque<u32>,
    limit: usize,
}

impl ResultSlots {
    pub(crate) fn new(workers: u32, limit: usize) -> Self {
        assert!((1..=2).contains(&limit));
        Self {
            outstanding: vec![0; workers as usize],
            available: (0..limit).flat_map(|_| 0..workers).collect(),
            limit,
        }
    }

    pub(crate) fn reserve(&mut self) -> Option<u32> {
        let worker = self.available.pop_front()?;
        let count = &mut self.outstanding[worker as usize];
        assert!(*count < self.limit);
        *count += 1;
        Some(worker)
    }

    pub(crate) fn admit(&mut self, worker: u32) -> Result<(), &'static str> {
        let count = self
            .outstanding
            .get_mut(worker as usize)
            .ok_or("admitted result has an unknown physical worker")?;
        *count = count
            .checked_sub(1)
            .ok_or("admitted result has no reserved result slot")?;
        self.available.push_back(worker);
        Ok(())
    }

    pub(crate) fn available(&self) -> usize {
        self.available.len()
    }
}

/// One worker's completed output or deterministic failure text.
#[derive(Debug)]
pub struct WorkerReply<Output> {
    /// Stable zero-based worker identifier.
    pub worker: u32,
    /// Executed output, or a failure produced while initializing or running the worker.
    pub outcome: Result<Output, String>,
}

/// Failure to communicate with a campaign worker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerPoolError {
    /// The requested worker identifier is outside the configured pool.
    UnknownWorker,
    /// The requested worker has already been closed.
    WorkerClosed,
    /// A worker exited before accepting its next job.
    WorkerExited,
    /// Every worker reply sender closed while a result was still expected.
    RepliesClosed,
}

impl fmt::Display for WorkerPoolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnknownWorker => "campaign worker identifier is outside the pool",
            Self::WorkerClosed => "campaign worker channel is already closed",
            Self::WorkerExited => "campaign worker exited before accepting its next job",
            Self::RepliesClosed => "every campaign worker exited while a reply was expected",
        })
    }
}

impl Error for WorkerPoolError {}

/// Coordinator-facing channels for a scoped worker set.
pub struct WorkerPool<Job, Output> {
    job_senders: Vec<Option<mpsc::Sender<Job>>>,
    reply_receiver: mpsc::Receiver<WorkerReply<Output>>,
}

impl<Job, Output> WorkerPool<Job, Output> {
    /// Send one job to a specific open worker.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown or closed worker, or when the worker exited.
    pub fn send(&self, worker: u32, job: Job) -> Result<(), WorkerPoolError> {
        self.job_senders
            .get(usize::try_from(worker).map_err(|_| WorkerPoolError::UnknownWorker)?)
            .ok_or(WorkerPoolError::UnknownWorker)?
            .as_ref()
            .ok_or(WorkerPoolError::WorkerClosed)?
            .send(job)
            .map_err(|_| WorkerPoolError::WorkerExited)
    }

    /// Stop assigning work to one worker. Its scoped thread exits after its current job.
    ///
    /// # Errors
    ///
    /// Returns an error when the worker identifier is outside the pool.
    pub fn close(&mut self, worker: u32) -> Result<(), WorkerPoolError> {
        let sender = self
            .job_senders
            .get_mut(usize::try_from(worker).map_err(|_| WorkerPoolError::UnknownWorker)?)
            .ok_or(WorkerPoolError::UnknownWorker)?;
        *sender = None;
        Ok(())
    }

    /// Wait for the next completed worker output.
    ///
    /// # Errors
    ///
    /// Returns an error if every reply sender closes before another result arrives.
    pub fn receive(&self) -> Result<WorkerReply<Output>, WorkerPoolError> {
        self.reply_receiver
            .recv()
            .map_err(|_| WorkerPoolError::RepliesClosed)
    }

    /// Take one completed worker output without waiting.
    ///
    /// This lets a coordinator refill every physical executor whose reply is
    /// already queued before it performs deterministic ordered admission.
    ///
    /// # Errors
    ///
    /// Returns an error if every reply sender has closed.
    pub(crate) fn try_receive(&self) -> Result<Option<WorkerReply<Output>>, WorkerPoolError> {
        match self.reply_receiver.try_recv() {
            Ok(reply) => Ok(Some(reply)),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err(WorkerPoolError::RepliesClosed),
        }
    }
}

/// Run a coordinator against a scoped, game-neutral worker pool.
///
/// Worker identifiers, job assignment, selection, admission, randomness, and recording remain
/// coordinator concerns. The pool owns only target initialization, job execution, and channels.
/// Setting `workers` to one uses this exact path with a one-element pool.
///
/// # Errors
///
/// Returns any error produced by the coordinator callback.
pub fn with_worker_pool<State, Job, Output, ResultValue, CoordinatorError>(
    workers: u32,
    initialize: impl Fn(u32) -> Result<State, String> + Sync,
    execute: impl Fn(&mut State, Job) -> Result<Output, String> + Sync,
    coordinate: impl FnOnce(&mut WorkerPool<Job, Output>) -> Result<ResultValue, CoordinatorError>,
) -> Result<ResultValue, CoordinatorError>
where
    Job: Send,
    Output: Send,
{
    thread::scope(|scope| {
        let (reply_sender, reply_receiver) = mpsc::channel::<WorkerReply<Output>>();
        let mut job_senders = Vec::with_capacity(workers as usize);
        for worker in 0..workers {
            let (job_sender, job_receiver) = mpsc::channel::<Job>();
            let reply_sender = reply_sender.clone();
            let initialize = &initialize;
            let execute = &execute;
            scope.spawn(move || {
                let mut state = match initialize(worker) {
                    Ok(state) => state,
                    Err(error) => {
                        let _ = reply_sender.send(WorkerReply {
                            worker,
                            outcome: Err(error),
                        });
                        return;
                    }
                };
                while let Ok(job) = job_receiver.recv() {
                    let outcome = execute(&mut state, job);
                    let failed = outcome.is_err();
                    if reply_sender.send(WorkerReply { worker, outcome }).is_err() || failed {
                        break;
                    }
                }
            });
            job_senders.push(Some(job_sender));
        }
        drop(reply_sender);
        let mut pool = WorkerPool {
            job_senders,
            reply_receiver,
        };
        coordinate(&mut pool)
    })
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc, sync::mpsc};

    use super::{ResultSlots, WorkerPool, WorkerPoolError, WorkerReply, with_worker_pool};

    #[test]
    fn result_slots_bound_every_executor_until_admission() {
        for limit in [1, 2] {
            let mut slots = ResultSlots::new(2, limit);
            for _ in 0..limit {
                assert_eq!(slots.reserve(), Some(0));
                assert_eq!(slots.reserve(), Some(1));
            }
            assert_eq!(slots.reserve(), None);
            slots.admit(1).unwrap();
            assert_eq!(slots.reserve(), Some(1));
            assert_eq!(slots.reserve(), None);
            assert!(slots.admit(2).is_err());
            for _ in 0..limit {
                slots.admit(0).unwrap();
            }
            assert!(slots.admit(0).is_err());
            assert_eq!(slots.available(), limit);
        }
    }

    #[test]
    fn a_second_reserved_job_runs_while_an_earlier_worker_is_blocked() {
        let (release, blocked) = mpsc::channel();
        with_worker_pool(
            2,
            |_| Ok::<_, String>(()),
            |_, (gate, value): (Option<mpsc::Receiver<()>>, u8)| {
                if let Some(gate) = gate {
                    gate.recv().map_err(|e| e.to_string())?;
                }
                Ok::<_, String>(value)
            },
            move |pool| -> Result<(), Box<dyn std::error::Error>> {
                let mut slots = ResultSlots::new(2, 2);
                for job in [(Some(blocked), 0), (None, 1), (None, 2), (None, 3)] {
                    pool.send(slots.reserve().expect("bounded prefill"), job)?;
                }
                // Both replies must come from worker 1, before worker 0 is
                // released and before any result credit is returned.
                for value in [1, 3] {
                    let reply = pool.receive()?;
                    assert_eq!(reply.worker, 1);
                    assert_eq!(reply.outcome?, value);
                    assert_eq!(slots.reserve(), None);
                }
                release.send(())?;
                for value in [0, 2] {
                    let reply = pool.receive()?;
                    assert_eq!(reply.worker, 0);
                    assert_eq!(reply.outcome?, value);
                }
                Ok(())
            },
        )
        .expect("bounded physical overlap");
    }

    #[test]
    fn try_receive_distinguishes_ready_empty_and_disconnected() {
        let (sender, receiver) = mpsc::channel();
        let pool = WorkerPool::<(), u64> {
            job_senders: Vec::new(),
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
    }

    #[test]
    fn one_worker_uses_the_same_pool_path() {
        let replies = with_worker_pool(
            1,
            |_| Ok::<_, String>(10_u64),
            |state, job: u64| Ok::<_, String>(*state + job),
            |pool| -> Result<Vec<(u32, u64)>, Box<dyn std::error::Error>> {
                pool.send(0, 7)?;
                let reply = pool.receive()?;
                pool.close(0)?;
                Ok(vec![(reply.worker, reply.outcome?)])
            },
        )
        .expect("coordinate one worker");
        assert_eq!(replies, vec![(0, 17)]);
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
            |pool| -> Result<u64, Box<dyn std::error::Error>> {
                pool.send(0, 3)?;
                Ok(pool.receive()?.outcome?)
            },
        )
        .expect("coordinate worker-local state");
        assert_eq!(value, 7);
    }

    #[test]
    fn a_completed_worker_can_be_reissued_while_another_is_busy() {
        let (release_sender, release_receiver) = mpsc::channel();
        let replies = with_worker_pool(
            2,
            |_| Ok::<_, String>(()),
            |_, job: (Option<mpsc::Receiver<()>>, u64)| {
                if let Some(release) = job.0 {
                    release.recv().map_err(|error| error.to_string())?;
                }
                Ok::<_, String>(job.1)
            },
            |pool| -> Result<Vec<(u32, u64)>, Box<dyn std::error::Error>> {
                pool.send(0, (Some(release_receiver), 10))?;
                pool.send(1, (None, 11))?;
                let first = pool.receive()?;
                pool.send(first.worker, (None, 12))?;
                let second = pool.receive()?;
                release_sender.send(())?;
                let third = pool.receive()?;
                Ok(vec![
                    (first.worker, first.outcome?),
                    (second.worker, second.outcome?),
                    (third.worker, third.outcome?),
                ])
            },
        )
        .expect("coordinate two workers");
        assert_eq!(replies, vec![(1, 11), (1, 12), (0, 10)]);
    }
}
