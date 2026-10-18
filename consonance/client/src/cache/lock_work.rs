// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    ops::{Deref, DerefMut},
    sync::{
        Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

#[derive(Clone, Copy, Debug)]
pub(super) enum Site {
    Divergences,
    VerifyNamespace,
    VerifyIntegrity,
    VerifyLease,
    Lookup,
    Extent,
    Publish,
    TargetCommitment,
    Chain,
    Release,
    Revoke,
    ReportStore,
    ForgetStore,
    Stats,
    Abandon,
}

impl Site {
    const ALL: [Self; 15] = [
        Self::Divergences,
        Self::VerifyNamespace,
        Self::VerifyIntegrity,
        Self::VerifyLease,
        Self::Lookup,
        Self::Extent,
        Self::Publish,
        Self::TargetCommitment,
        Self::Chain,
        Self::Release,
        Self::Revoke,
        Self::ReportStore,
        Self::ForgetStore,
        Self::Stats,
        Self::Abandon,
    ];
    fn name(self) -> &'static str {
        match self {
            Self::Divergences => "divergences",
            Self::VerifyNamespace => "verify_namespace",
            Self::VerifyIntegrity => "verify_integrity",
            Self::VerifyLease => "verify_lease",
            Self::Lookup => "lookup",
            Self::Extent => "extent",
            Self::Publish => "publish",
            Self::TargetCommitment => "target_commitment",
            Self::Chain => "chain",
            Self::Release => "release",
            Self::Revoke => "revoke",
            Self::ReportStore => "report_store",
            Self::ForgetStore => "forget_store",
            Self::Stats => "stats",
            Self::Abandon => "abandon",
        }
    }
}

pub(super) trait Clock: std::fmt::Debug {
    fn now(&self) -> u64;
}

#[derive(Debug)]
pub(super) struct WallClock(Instant);
impl Default for WallClock {
    #[allow(clippy::disallowed_methods)]
    fn default() -> Self {
        Self(Instant::now())
    }
}
impl Clock for WallClock {
    fn now(&self) -> u64 {
        self.0.elapsed().as_nanos().try_into().unwrap_or(u64::MAX)
    }
}

#[derive(Debug, Default)]
struct Work {
    attempts: AtomicU64,
    acquired: AtomicU64,
    completed: AtomicU64,
    wait_ns: AtomicU64,
    hold_ns: AtomicU64,
}
fn add(counter: &AtomicU64, value: u64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
        Some(old.saturating_add(value))
    });
}

#[derive(Debug)]
pub(super) struct LockWork<C = WallClock> {
    clock: C,
    sites: [Work; 15],
}
impl Default for LockWork {
    fn default() -> Self {
        Self::new(WallClock::default())
    }
}
impl<C: Clock> LockWork<C> {
    fn new(clock: C) -> Self {
        Self {
            clock,
            sites: std::array::from_fn(|_| Work::default()),
        }
    }
    pub(super) fn lock<'a, T>(&'a self, mutex: &'a Mutex<T>, site: Site) -> Guard<'a, T, C> {
        let work = &self.sites[site as usize];
        add(&work.attempts, 1);
        let start = self.clock.now();
        let guard = mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let acquired = self.clock.now();
        add(&work.wait_ns, acquired.saturating_sub(start));
        add(&work.acquired, 1);
        Guard {
            guard: Some(guard),
            owner: self,
            site,
            start: acquired,
        }
    }
    pub(super) fn counters(&self) -> Vec<(String, u64)> {
        let mut out = Vec::new();
        for site in Site::ALL {
            let work = &self.sites[site as usize];
            let attempts = work.attempts.load(Ordering::Relaxed);
            if attempts == 0 {
                continue;
            }
            for (name, value) in [
                ("attempts", attempts),
                ("acquired", work.acquired.load(Ordering::Relaxed)),
                ("completed", work.completed.load(Ordering::Relaxed)),
                ("wait_ns", work.wait_ns.load(Ordering::Relaxed)),
                ("hold_ns", work.hold_ns.load(Ordering::Relaxed)),
            ] {
                out.push((format!("{}.{}", site.name(), name), value));
            }
        }
        out
    }
}

pub(super) struct Guard<'a, T, C: Clock = WallClock> {
    guard: Option<MutexGuard<'a, T>>,
    owner: &'a LockWork<C>,
    site: Site,
    start: u64,
}
impl<T, C: Clock> Deref for Guard<'_, T, C> {
    type Target = T;
    fn deref(&self) -> &T {
        self.guard.as_deref().expect("live cache lock guard")
    }
}
impl<T, C: Clock> DerefMut for Guard<'_, T, C> {
    fn deref_mut(&mut self) -> &mut T {
        self.guard.as_deref_mut().expect("live cache lock guard")
    }
}
impl<T, C: Clock> Drop for Guard<'_, T, C> {
    fn drop(&mut self) {
        let held = self.owner.clock.now().saturating_sub(self.start);
        drop(self.guard.take());
        let work = &self.owner.sites[self.site as usize];
        add(&work.hold_ns, held);
        add(&work.completed, 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    #[derive(Debug, Default)]
    struct FakeClock(AtomicU64);
    impl Clock for FakeClock {
        fn now(&self) -> u64 {
            self.0.fetch_add(7, Ordering::Relaxed)
        }
    }
    #[test]
    fn exact_wait_hold_early_return_and_unwind_complete_once() {
        let owner = LockWork::new(FakeClock::default());
        let mutex = Mutex::new(0);
        let early = || {
            let _guard = owner.lock(&mutex, Site::Lookup);
            None::<()>
        };
        assert_eq!(early(), None);
        let result = std::panic::catch_unwind(|| {
            let _guard = owner.lock(&mutex, Site::Lookup);
            panic!("planted unwind")
        });
        assert!(result.is_err());
        let work = &owner.sites[Site::Lookup as usize];
        assert_eq!(work.attempts.load(Ordering::Relaxed), 2);
        assert_eq!(work.acquired.load(Ordering::Relaxed), 2);
        assert_eq!(work.completed.load(Ordering::Relaxed), 2);
        assert_eq!(work.wait_ns.load(Ordering::Relaxed), 14);
        assert_eq!(work.hold_ns.load(Ordering::Relaxed), 14);
        assert!(mutex.is_poisoned());
        drop(owner.lock(&mutex, Site::Lookup));
        assert_eq!(work.completed.load(Ordering::Relaxed), 3);
    }
    #[test]
    fn contending_attempt_is_visible_before_acquisition() {
        let owner = Arc::new(LockWork::default());
        let mutex = Arc::new(Mutex::new(()));
        let guard = owner.lock(&mutex, Site::Lookup);
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let thread = {
            let owner = owner.clone();
            let mutex = mutex.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                drop(owner.lock(&mutex, Site::Lookup));
            })
        };
        barrier.wait();
        while owner.sites[Site::Lookup as usize]
            .attempts
            .load(Ordering::Relaxed)
            != 2
        {
            std::thread::yield_now();
        }
        assert_eq!(
            owner.sites[Site::Lookup as usize]
                .acquired
                .load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            owner.sites[Site::Lookup as usize]
                .completed
                .load(Ordering::Relaxed),
            0
        );
        drop(guard);
        thread.join().unwrap();
        assert_eq!(
            owner.sites[Site::Lookup as usize]
                .completed
                .load(Ordering::Relaxed),
            2
        );
    }
}
