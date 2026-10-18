// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::cache::WritableExtent;
#[cfg(feature = "in-process")]
use std::sync::OnceLock;
use std::{
    collections::VecDeque,
    io,
    process::Child,
    sync::{Arc, Mutex},
};

pub(crate) struct ExportDestination<'a> {
    pub(crate) extent: Option<WritableExtent>,
    pub(crate) exposed: bool,
    retained: &'a std::cell::RefCell<Vec<WritableExtent>>,
}
impl<'a> ExportDestination<'a> {
    pub(crate) fn new(retained: &'a std::cell::RefCell<Vec<WritableExtent>>) -> Self {
        Self {
            extent: None,
            exposed: false,
            retained,
        }
    }
}
impl Drop for ExportDestination<'_> {
    fn drop(&mut self) {
        if self.exposed
            && let Some(extent) = self.extent.take()
        {
            self.retained.borrow_mut().push(extent);
        }
    }
}

pub(crate) trait ExitOwner: Send + std::fmt::Debug {
    fn try_exited(&mut self) -> io::Result<bool>;
    fn terminate(&mut self) -> io::Result<()>;
}

impl ExitOwner for Child {
    fn try_exited(&mut self) -> io::Result<bool> {
        self.try_wait().map(|status| status.is_some())
    }
    fn terminate(&mut self) -> io::Result<()> {
        self.kill()
    }
}

#[derive(Debug)]
struct UnknownOwner;
impl ExitOwner for UnknownOwner {
    fn try_exited(&mut self) -> io::Result<bool> {
        Err(io::Error::other("worker exit ownership is unavailable"))
    }
    fn terminate(&mut self) -> io::Result<()> {
        Err(io::Error::other("worker exit ownership is unavailable"))
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WorkerQuarantineStats {
    pub retained_workers: u64,
    pub retained_extents: u64,
    pub retained_bytes: u64,
    pub ambiguous_workers: u64,
    pub signals_sent: u64,
    pub wait_errors: u64,
    pub signal_errors: u64,
    pub confirmed_exits: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExitPoll {
    Exited,
    Running,
    Ambiguous,
}

#[derive(Clone, Debug)]
pub(crate) struct WorkerId(Arc<()>);

#[derive(Debug)]
struct RetainedWorker {
    id: WorkerId,
    owner: Box<dyn ExitOwner>,
    extents: Vec<WritableExtent>,
    ambiguous: bool,
    signalled: bool,
    signal_eligible: bool,
}

#[derive(Debug, Default)]
struct Quarantine {
    workers: VecDeque<RetainedWorker>,
    stats: WorkerQuarantineStats,
}

impl Quarantine {
    fn insert(
        &mut self,
        owner: Option<Box<dyn ExitOwner>>,
        extents: Vec<WritableExtent>,
    ) -> WorkerId {
        let id = WorkerId(Arc::new(()));
        self.stats.retained_workers = self.stats.retained_workers.saturating_add(1);
        self.stats.retained_extents = self
            .stats
            .retained_extents
            .saturating_add(extents.len() as u64);
        for extent in &extents {
            self.stats.retained_bytes = self
                .stats
                .retained_bytes
                .saturating_add(extent.len() as u64);
        }
        self.workers.push_back(RetainedWorker {
            id: id.clone(),
            owner: owner.unwrap_or_else(|| Box::new(UnknownOwner)),
            extents,
            ambiguous: false,
            signalled: false,
            signal_eligible: false,
        });
        id
    }

    fn poll(&mut self, id: &WorkerId, signal: bool) -> ExitPoll {
        let Some(position) = self
            .workers
            .iter()
            .position(|worker| Arc::ptr_eq(&worker.id.0, &id.0))
        else {
            return ExitPoll::Exited;
        };
        let worker = &mut self.workers[position];
        if worker.ambiguous {
            return ExitPoll::Ambiguous;
        }
        worker.ambiguous = true;
        self.stats.ambiguous_workers = self.stats.ambiguous_workers.saturating_add(1);
        match worker.owner.try_exited() {
            Ok(true) => {
                let worker = self
                    .workers
                    .remove(position)
                    .expect("retained worker position");
                self.stats.retained_workers -= 1;
                self.stats.retained_extents -= worker.extents.len() as u64;
                for extent in &worker.extents {
                    self.stats.retained_bytes -= extent.len() as u64;
                }
                self.stats.ambiguous_workers -= u64::from(worker.ambiguous);
                self.stats.confirmed_exits = self.stats.confirmed_exits.saturating_add(1);
                drop(worker);
                ExitPoll::Exited
            }
            Err(_) => {
                self.stats.wait_errors = self.stats.wait_errors.saturating_add(1);
                ExitPoll::Ambiguous
            }
            Ok(false) => {
                worker.ambiguous = false;
                self.stats.ambiguous_workers -= 1;
                if signal && worker.signal_eligible && !worker.signalled {
                    worker.ambiguous = true;
                    self.stats.ambiguous_workers = self.stats.ambiguous_workers.saturating_add(1);
                    match worker.owner.terminate() {
                        Ok(()) => {
                            worker.ambiguous = false;
                            self.stats.ambiguous_workers -= 1;
                            worker.signalled = true;
                            self.stats.signals_sent = self.stats.signals_sent.saturating_add(1);
                        }
                        Err(_) => {
                            self.stats.signal_errors = self.stats.signal_errors.saturating_add(1);
                        }
                    }
                }
                if worker.ambiguous {
                    ExitPoll::Ambiguous
                } else {
                    ExitPoll::Running
                }
            }
        }
    }

    fn allow_signal(&mut self, id: &WorkerId) {
        if let Some(worker) = self
            .workers
            .iter_mut()
            .find(|worker| Arc::ptr_eq(&worker.id.0, &id.0))
        {
            worker.signal_eligible = true;
        }
    }

    fn reap(&mut self, limit: usize) {
        for _ in 0..limit.min(64).min(self.workers.len()) {
            let id = self.workers.front().expect("retained worker").id.clone();
            if self.poll(&id, true) != ExitPoll::Exited {
                self.workers.rotate_left(1);
            }
        }
    }
}

#[cfg(feature = "in-process")]
fn registry() -> &'static Mutex<Quarantine> {
    static REGISTRY: OnceLock<Mutex<Quarantine>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Quarantine::default()))
}

#[cfg(feature = "in-process")]
fn locked() -> std::sync::MutexGuard<'static, Quarantine> {
    registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(feature = "in-process")]
pub(crate) fn retain_worker(
    owner: Option<Box<dyn ExitOwner>>,
    extents: Vec<WritableExtent>,
) -> WorkerId {
    locked().insert(owner, extents)
}
#[cfg(feature = "in-process")]
pub(crate) fn poll_worker(id: &WorkerId, signal: bool) -> ExitPoll {
    locked().poll(id, signal)
}

#[cfg(feature = "in-process")]
pub(crate) fn allow_worker_signal(id: &WorkerId) {
    locked().allow_signal(id);
}

#[cfg(all(test, feature = "in-process"))]
pub(crate) fn retained_worker(id: &WorkerId) -> Option<(usize, usize, bool)> {
    locked()
        .workers
        .iter()
        .find(|worker| Arc::ptr_eq(&worker.id.0, &id.0))
        .map(|worker| {
            (
                worker.extents.len(),
                worker.extents.iter().map(WritableExtent::len).sum(),
                worker.ambiguous,
            )
        })
}

#[cfg(feature = "in-process")]
pub fn worker_quarantine_stats() -> WorkerQuarantineStats {
    locked().stats
}
#[cfg(feature = "in-process")]
pub fn reap_worker_quarantine(limit: usize) -> WorkerQuarantineStats {
    let mut registry = locked();
    registry.reap(limit);
    registry.stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{CacheIndex, LocalIndex};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    #[derive(Clone, Copy, Debug)]
    enum Step {
        Running,
        Exited,
        Error,
        Panic,
    }
    #[derive(Debug)]
    struct ScriptedOwner {
        steps: VecDeque<Step>,
        polls: Arc<AtomicUsize>,
        signals: Arc<AtomicUsize>,
        signal_error: bool,
    }
    impl ExitOwner for ScriptedOwner {
        fn try_exited(&mut self) -> io::Result<bool> {
            self.polls.fetch_add(1, Ordering::SeqCst);
            match self.steps.pop_front().unwrap_or(Step::Running) {
                Step::Running => Ok(false),
                Step::Exited => Ok(true),
                Step::Error => Err(io::Error::other("ambiguous wait")),
                Step::Panic => panic!("injected owner panic"),
            }
        }
        fn terminate(&mut self) -> io::Result<()> {
            self.signals.fetch_add(1, Ordering::SeqCst);
            if self.signal_error {
                Err(io::Error::other("ambiguous signal"))
            } else {
                Ok(())
            }
        }
    }
    fn owner(
        steps: &[Step],
        signal_error: bool,
    ) -> (Box<dyn ExitOwner>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let polls = Arc::new(AtomicUsize::new(0));
        let signals = Arc::new(AtomicUsize::new(0));
        (
            Box::new(ScriptedOwner {
                steps: steps.iter().copied().collect(),
                polls: polls.clone(),
                signals: signals.clone(),
                signal_error,
            }),
            polls,
            signals,
        )
    }
    #[test]
    fn unwinding_after_descriptor_exposure_retains_charge_until_proven_exit() {
        let index = LocalIndex::new(4096);
        let retained = std::cell::RefCell::new(Vec::new());
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut destination = ExportDestination::new(&retained);
                destination.extent = Some(index.extent(4096).unwrap());
                destination.exposed = true;
                panic!("after descriptor exposure");
            }))
            .is_err()
        );
        assert_eq!(index.stats().charged, 4096);
        assert_eq!(retained.borrow().len(), 1);
        let (owner, _, _) = owner(&[Step::Exited], false);
        let mut registry = Quarantine::default();
        let id = registry.insert(Some(owner), retained.take());
        assert_eq!(registry.poll(&id, false), ExitPoll::Exited);
        assert_eq!(index.stats().charged, 0);
    }

    #[test]
    fn signalling_does_not_release_a_destination_until_exit_is_confirmed() {
        let index = LocalIndex::new(8192);
        let extent = index.extent(8192).unwrap();
        let (owner, _, signals) = owner(&[Step::Running, Step::Exited], false);
        let mut registry = Quarantine::default();
        let id = registry.insert(Some(owner), vec![extent]);
        registry.allow_signal(&id);
        assert_eq!(registry.poll(&id, true), ExitPoll::Running);
        assert_eq!(signals.load(Ordering::SeqCst), 1);
        assert_eq!(
            registry.stats,
            WorkerQuarantineStats {
                retained_workers: 1,
                retained_extents: 1,
                retained_bytes: 8192,
                ambiguous_workers: 0,
                signals_sent: 1,
                wait_errors: 0,
                signal_errors: 0,
                confirmed_exits: 0,
            }
        );
        assert_eq!(index.stats().charged, 8192);
        assert!(index.extent(1).is_err());
        assert_eq!(registry.poll(&id, true), ExitPoll::Exited);
        assert_eq!(
            registry.stats,
            WorkerQuarantineStats {
                retained_workers: 0,
                retained_extents: 0,
                retained_bytes: 0,
                ambiguous_workers: 0,
                signals_sent: 1,
                wait_errors: 0,
                signal_errors: 0,
                confirmed_exits: 1,
            }
        );
        assert_eq!(index.stats().charged, 0);
        assert_eq!(registry.poll(&id, true), ExitPoll::Exited);
        assert_eq!(registry.stats.confirmed_exits, 1);
        assert_eq!(signals.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn ambiguous_wait_permanently_forbids_further_waits_or_signals() {
        let index = LocalIndex::new(4096);
        let (owner, polls, signals) = owner(&[Step::Error, Step::Exited], false);
        let mut registry = Quarantine::default();
        let id = registry.insert(Some(owner), vec![index.extent(4096).unwrap()]);
        registry.allow_signal(&id);
        for _ in 0..3 {
            assert_eq!(registry.poll(&id, true), ExitPoll::Ambiguous);
            registry.reap(64);
        }
        assert_eq!(polls.load(Ordering::SeqCst), 1);
        assert_eq!(signals.load(Ordering::SeqCst), 0);
        assert_eq!(index.stats().charged, 4096);
        assert_eq!(registry.stats.ambiguous_workers, 1);
        assert_eq!(registry.stats.wait_errors, 1);
        assert_eq!(registry.stats.confirmed_exits, 0);
        assert_eq!(registry.stats.retained_bytes, 4096);
    }
    #[test]
    fn failed_signal_permanently_forbids_further_waits_or_signals() {
        let index = LocalIndex::new(4096);
        let (owner, polls, signals) = owner(&[Step::Running, Step::Exited], true);
        let mut registry = Quarantine::default();
        let id = registry.insert(Some(owner), vec![index.extent(4096).unwrap()]);
        registry.allow_signal(&id);
        for _ in 0..3 {
            assert_eq!(registry.poll(&id, true), ExitPoll::Ambiguous);
        }
        assert_eq!(index.stats().charged, 4096);
        assert_eq!(polls.load(Ordering::SeqCst), 1);
        assert_eq!(signals.load(Ordering::SeqCst), 1);
        assert_eq!(registry.stats.signal_errors, 1);
        assert_eq!(registry.stats.signals_sent, 0);
        assert_eq!(registry.stats.confirmed_exits, 0);
    }
    #[test]
    fn reap_work_is_capped_and_round_robin() {
        let mut registry = Quarantine::default();
        let mut polls = Vec::new();
        let mut signals = Vec::new();
        for _ in 0..100 {
            let (owner, p, s) = owner(&[], false);
            let id = registry.insert(Some(owner), Vec::new());
            registry.allow_signal(&id);
            polls.push(p);
            signals.push(s);
        }
        registry.reap(usize::MAX);
        assert_eq!(
            polls
                .iter()
                .map(|n| n.load(Ordering::SeqCst))
                .sum::<usize>(),
            64
        );
        registry.reap(usize::MAX);
        assert_eq!(
            polls
                .iter()
                .map(|n| n.load(Ordering::SeqCst))
                .sum::<usize>(),
            128
        );
        assert!(polls.iter().all(|n| n.load(Ordering::SeqCst) > 0));
        assert_eq!(
            signals
                .iter()
                .map(|n| n.load(Ordering::SeqCst))
                .sum::<usize>(),
            100
        );
        assert_eq!(registry.stats.retained_workers, 100);
    }
    #[test]
    fn an_owner_panic_does_not_take_storage_out_of_the_registry() {
        let index = LocalIndex::new(4096);
        let (owner, polls, signals) = owner(&[Step::Panic, Step::Exited], false);
        let mut registry = Quarantine::default();
        let id = registry.insert(Some(owner), vec![index.extent(4096).unwrap()]);
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| registry.poll(&id, false)))
                .is_err()
        );
        assert_eq!(index.stats().charged, 4096);
        assert_eq!(registry.stats.retained_extents, 1);
        assert_eq!(registry.poll(&id, true), ExitPoll::Ambiguous);
        assert_eq!(signals.load(Ordering::SeqCst), 0);
        assert_eq!(registry.poll(&id, false), ExitPoll::Ambiguous);
        assert_eq!(polls.load(Ordering::SeqCst), 1);
        assert_eq!(registry.stats.confirmed_exits, 0);
        assert_eq!(index.stats().charged, 4096);
    }
    #[test]
    fn unknown_ownership_is_observable_and_never_treated_as_exit() {
        let mut registry = Quarantine::default();
        let id = registry.insert(None, Vec::new());
        assert_eq!(registry.poll(&id, true), ExitPoll::Ambiguous);
        assert_eq!(registry.poll(&id, true), ExitPoll::Ambiguous);
        assert_eq!(registry.stats.retained_workers, 1);
        assert_eq!(registry.stats.signals_sent, 0);
        assert_eq!(registry.stats.wait_errors, 1);
        assert_eq!(registry.stats.ambiguous_workers, 1);
    }
    #[test]
    fn concurrent_background_reaping_cannot_signal_until_grace_is_released() {
        let index = LocalIndex::new(4096);
        let (owner, polls, signals) = owner(
            &[
                Step::Running,
                Step::Running,
                Step::Running,
                Step::Running,
                Step::Exited,
            ],
            false,
        );
        let mut registry = Quarantine::default();
        let id = registry.insert(Some(owner), vec![index.extent(4096).unwrap()]);
        let registry = Arc::new(Mutex::new(registry));
        let (continue_tx, continue_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let reaper = {
            let registry = registry.clone();
            std::thread::spawn(move || {
                registry.lock().unwrap().reap(64);
                registry.lock().unwrap().reap(64);
                registry.lock().unwrap().reap(64);
                done_tx.send(()).unwrap();
                if continue_rx
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .is_ok()
                {
                    registry.lock().unwrap().reap(64);
                    registry.lock().unwrap().reap(64);
                }
            })
        };
        let reached = done_rx.recv_timeout(std::time::Duration::from_secs(2));
        let before = (
            polls.load(Ordering::SeqCst),
            signals.load(Ordering::SeqCst),
            index.stats().charged,
        );
        registry.lock().unwrap().allow_signal(&id);
        let _ = continue_tx.send(());
        let joined = reaper.join();
        assert!(joined.is_ok());
        assert!(reached.is_ok());
        assert_eq!(before, (3, 0, 4096));
        assert_eq!(polls.load(Ordering::SeqCst), 5);
        assert_eq!(signals.load(Ordering::SeqCst), 1);
        assert_eq!(index.stats().charged, 0);
        assert_eq!(registry.lock().unwrap().stats.confirmed_exits, 1);
    }
}
