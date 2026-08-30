// SPDX-License-Identifier: AGPL-3.0-or-later

//! Game-neutral scoped worker pool for campaign job execution.

use std::{error::Error, fmt, sync::mpsc, thread};

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
    /// One worker replied twice before the coordinator consumed its first reply.
    DuplicateReply,
}

impl fmt::Display for WorkerPoolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnknownWorker => "campaign worker identifier is outside the pool",
            Self::WorkerClosed => "campaign worker channel is already closed",
            Self::WorkerExited => "campaign worker exited before accepting its next job",
            Self::RepliesClosed => "every campaign worker exited while a reply was expected",
            Self::DuplicateReply => "campaign worker replied twice before admission",
        })
    }
}

impl Error for WorkerPoolError {}

/// Coordinator-facing channels for a scoped worker set.
pub struct WorkerPool<Job, Output> {
    job_senders: Vec<Option<mpsc::Sender<Job>>>,
    reply_receiver: mpsc::Receiver<WorkerReply<Output>>,
    buffered_replies: Vec<Option<WorkerReply<Output>>>,
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

    /// Wait for one named worker's reply, buffering replies that finish ahead
    /// of it. This lets a coordinator make admission order independent of host
    /// thread scheduling while workers continue executing concurrently.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown worker, a duplicate buffered reply, or
    /// if every reply sender closes before the named worker replies.
    pub fn receive_for(&mut self, worker: u32) -> Result<WorkerReply<Output>, WorkerPoolError> {
        let expected = usize::try_from(worker).map_err(|_| WorkerPoolError::UnknownWorker)?;
        if expected >= self.buffered_replies.len() {
            return Err(WorkerPoolError::UnknownWorker);
        }
        if let Some(reply) = self.buffered_replies[expected].take() {
            return Ok(reply);
        }
        loop {
            let reply = self.receive()?;
            if reply.worker == worker {
                return Ok(reply);
            }
            let index =
                usize::try_from(reply.worker).map_err(|_| WorkerPoolError::UnknownWorker)?;
            let slot = self
                .buffered_replies
                .get_mut(index)
                .ok_or(WorkerPoolError::UnknownWorker)?;
            if slot.replace(reply).is_some() {
                return Err(WorkerPoolError::DuplicateReply);
            }
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
    State: Send,
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
            buffered_replies: (0..workers).map(|_| None).collect(),
        };
        coordinate(&mut pool)
    })
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::{WorkerPool, WorkerReply, with_worker_pool};

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
    fn a_named_receive_buffers_out_of_order_replies() {
        let (reply_sender, reply_receiver) = mpsc::channel();
        reply_sender
            .send(WorkerReply {
                worker: 0,
                outcome: Ok::<_, String>(10_u64),
            })
            .expect("send worker zero");
        reply_sender
            .send(WorkerReply {
                worker: 1,
                outcome: Ok::<_, String>(11_u64),
            })
            .expect("send worker one");
        let mut pool = WorkerPool::<(), u64> {
            job_senders: vec![None, None],
            reply_receiver,
            buffered_replies: vec![None, None],
        };
        let one = pool.receive_for(1).expect("receive worker one");
        let zero = pool.receive_for(0).expect("receive buffered worker zero");
        assert_eq!(one.outcome.expect("worker one outcome"), 11);
        assert_eq!(zero.outcome.expect("worker zero outcome"), 10);
    }
}
