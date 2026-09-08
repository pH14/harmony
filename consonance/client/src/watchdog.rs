// SPDX-License-Identifier: AGPL-3.0-or-later
//! Host-only wall-clock timeout for a guest that stops returning to the host.
//!
//! A guest spinning on a frozen virtual clock takes no exit, so it never
//! reaches its virtual-time deadline and only host time can notice it. SIGUSR1
//! interrupts the owning thread, and the backend's cancellation latch prevents
//! EINTR from re-entering the guest. The guard cannot leave that thread and
//! joins the sender before it exits. SIGUSR1 is reserved process-wide for this
//! purpose, so every composition that arms a timeout shares this one guard.
//!
//! One guard covers one request, so the caller must call [`Watchdog::claim`]
//! the moment its request returns: after that the guard sends no signal, and a
//! signal already in flight can still land on the caller's thread and return
//! EINTR from an unrelated system call before the guard is dropped. Callers
//! that issue other system calls in that window handle EINTR themselves.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, Ordering},
    mpsc,
};
use std::time::Duration;

/// The request and the expiring guard race for the run. Whichever reaches the
/// outcome first decides it, so a reply that arrives just before expiry is
/// still delivered and an expiry just before the reply still cancels the VM.
const RUNNING: u8 = 0;
const CLAIMED: u8 = 1;
const EXPIRED: u8 = 2;

pub struct Watchdog {
    done: mpsc::Sender<()>,
    thread: Option<std::thread::JoinHandle<()>>,
    outcome: Arc<AtomicU8>,
    _owner: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl Watchdog {
    #[cfg(not(miri))]
    pub fn start(budget: Duration, cancel: Arc<AtomicBool>) -> std::io::Result<Self> {
        install_signal()?;
        // SAFETY: pthread_self returns the calling thread's live identifier.
        // The non-Send guard joins the only user of it before this thread exits.
        let owner = unsafe { libc::pthread_self() };
        let (done, receiver) = mpsc::channel();
        let outcome = Arc::new(AtomicU8::new(RUNNING));
        let watched = Arc::clone(&outcome);
        let thread = std::thread::Builder::new()
            .name("harmony-timeout".into())
            .spawn(move || {
                watch(receiver, budget, &watched, &cancel, || {
                    // SAFETY: owner stays alive until this sender has been joined.
                    // SIGUSR1 has a no-op handler and is unblocked on that thread.
                    let _ = unsafe { libc::pthread_kill(owner, libc::SIGUSR1) };
                });
            })?;
        Ok(Self {
            done,
            thread: Some(thread),
            outcome,
            _owner: std::marker::PhantomData,
        })
    }

    /// Claim the run for the request that just returned. `true` when the
    /// request won and its reply stands; `false` when the guard expired first
    /// and the VM has been canceled.
    pub fn claim(&self) -> bool {
        self.outcome
            .compare_exchange(RUNNING, CLAIMED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        let _ = self.done.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn watch(
    done: mpsc::Receiver<()>,
    budget: Duration,
    outcome: &AtomicU8,
    cancel: &AtomicBool,
    mut kick: impl FnMut(),
) {
    if done.recv_timeout(budget) != Err(mpsc::RecvTimeoutError::Timeout) {
        return;
    }
    if outcome
        .compare_exchange(RUNNING, EXPIRED, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    cancel.store(true, Ordering::Release);
    loop {
        kick();
        // Repeat after expiry to close the signal-before-KVM_RUN race. No
        // signals are sent before expiry, and the VM is abandoned afterward.
        if done.recv_timeout(Duration::from_millis(10)) != Err(mpsc::RecvTimeoutError::Timeout) {
            break;
        }
    }
}

extern "C" fn interrupt(_signal: libc::c_int) {}

#[cfg(not(miri))]
fn install_signal() -> std::io::Result<()> {
    static INSTALLED: std::sync::OnceLock<Result<(), i32>> = std::sync::OnceLock::new();
    let result = INSTALLED.get_or_init(|| {
        // SAFETY: both sigactions are fully zero-initialized C records. The
        // kernel writes old, then reads action, whose mask is initialized and
        // whose handler has the required ABI and performs no operations.
        unsafe {
            let mut old: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(libc::SIGUSR1, std::ptr::null(), &raw mut old) != 0 {
                return Err(std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EIO));
            }
            if old.sa_sigaction != libc::SIG_DFL && old.sa_sigaction != libc::SIG_IGN {
                return Err(libc::EBUSY);
            }
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = interrupt as *const () as usize;
            libc::sigemptyset(&raw mut action.sa_mask);
            if libc::sigaction(libc::SIGUSR1, &raw const action, std::ptr::null_mut()) != 0 {
                return Err(std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EIO));
            }
        }
        Ok(())
    });
    result.map_err(std::io::Error::from_raw_os_error)?;
    // SAFETY: set is initialized before use; pthread_sigmask changes only the
    // caller's mask. SIGUSR1 is reserved process-wide for canceling a run.
    let rc = unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&raw mut set);
        libc::sigaddset(&raw mut set, libc::SIGUSR1);
        libc::pthread_sigmask(libc::SIG_UNBLOCK, &raw const set, std::ptr::null_mut())
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::from_raw_os_error(rc))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_and_disconnect_do_not_cancel_or_kick() {
        for complete in [true, false] {
            let (tx, rx) = mpsc::channel();
            if complete {
                tx.send(()).unwrap();
            }
            drop(tx);
            let cancel = AtomicBool::new(false);
            let outcome = AtomicU8::new(RUNNING);
            watch(rx, Duration::ZERO, &outcome, &cancel, || {
                panic!("normal completion sent a signal")
            });
            assert!(!cancel.load(Ordering::Acquire));
            assert_eq!(outcome.load(Ordering::Acquire), RUNNING);
        }
    }

    #[test]
    fn expiry_publishes_cancellation_before_signaling() {
        let (tx, rx) = mpsc::channel();
        let cancel = AtomicBool::new(false);
        let outcome = AtomicU8::new(RUNNING);
        let mut signals = 0;
        watch(rx, Duration::ZERO, &outcome, &cancel, || {
            assert!(cancel.load(Ordering::Acquire));
            signals += 1;
            tx.send(()).unwrap();
        });
        assert_eq!(signals, 1);
        assert_eq!(outcome.load(Ordering::Acquire), EXPIRED);
        interrupt(libc::SIGUSR1);
    }

    #[test]
    fn a_request_that_returns_first_keeps_its_reply_and_the_vm() {
        let (_tx, rx) = mpsc::channel();
        let cancel = AtomicBool::new(false);
        let outcome = AtomicU8::new(CLAIMED);
        watch(rx, Duration::ZERO, &outcome, &cancel, || {
            panic!("a claimed run was signaled")
        });
        assert!(!cancel.load(Ordering::Acquire));
        assert_eq!(outcome.load(Ordering::Acquire), CLAIMED);
    }

    #[test]
    #[cfg(not(miri))]
    fn only_the_first_of_the_request_and_the_expiry_claims_the_run() {
        let cancel = Arc::new(AtomicBool::new(false));
        let unexpired = Watchdog::start(Duration::from_secs(60), Arc::clone(&cancel)).unwrap();
        assert!(unexpired.claim(), "the request returned first");
        assert!(!unexpired.claim(), "the run is claimed only once");
        drop(unexpired);
        assert!(!cancel.load(Ordering::Acquire));

        let expired = Watchdog::start(Duration::ZERO, Arc::clone(&cancel)).unwrap();
        for _ in 0..1000 {
            if cancel.load(Ordering::Acquire) {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!expired.claim(), "the guard expired first");
        drop(expired);
    }

    #[test]
    #[cfg(not(miri))]
    fn real_signal_cancellation_and_guard_join() {
        let cancel = Arc::new(AtomicBool::new(false));
        let guard = Watchdog::start(Duration::ZERO, Arc::clone(&cancel)).unwrap();
        for _ in 0..1000 {
            if cancel.load(Ordering::Acquire) {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(cancel.load(Ordering::Acquire));
        drop(guard);
        let canceled = Arc::new(AtomicBool::new(false));
        drop(Watchdog::start(Duration::from_secs(60), Arc::clone(&canceled)).unwrap());
        assert!(!canceled.load(Ordering::Acquire));
    }
    #[test]
    fn drop_joins_the_sender_before_returning() {
        let (done, rx) = mpsc::channel();
        let joined = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&joined);
        let guard = Watchdog {
            done,
            thread: Some(std::thread::spawn(move || {
                let _ = rx.recv();
                // Without join(), guard destruction returns before this write.
                std::thread::sleep(Duration::from_millis(20));
                flag.store(true, Ordering::Release);
            })),
            outcome: Arc::new(AtomicU8::new(RUNNING)),
            _owner: std::marker::PhantomData,
        };
        drop(guard);
        assert!(joined.load(Ordering::Acquire));
    }

    #[test]
    #[cfg(not(miri))]
    fn installs_for_default_and_ignored_but_rejects_owned_signal() {
        const ENV: &str = "HARMONY_WATCHDOG_SIGNAL_TEST";
        if let Ok(mode) = std::env::var(ENV) {
            let handler = match mode.as_str() {
                "default" => libc::SIG_DFL,
                "ignored" => libc::SIG_IGN,
                _ => interrupt as *const () as usize,
            };
            // SAFETY: isolated child process; the handler has the C signal ABI.
            unsafe {
                libc::signal(libc::SIGUSR1, handler);
            }
            let result = install_signal();
            if mode == "owned" {
                assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::EBUSY));
            } else {
                result.unwrap();
                // SAFETY: old is a fully initialized record written by sigaction.
                let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
                assert_eq!(
                    unsafe { libc::sigaction(libc::SIGUSR1, std::ptr::null(), &raw mut old) },
                    0
                );
                assert_eq!(old.sa_sigaction, interrupt as *const () as usize);
            }
            return;
        }
        for mode in ["default", "ignored", "owned"] {
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "watchdog::tests::installs_for_default_and_ignored_but_rejects_owned_signal",
                ])
                .env(ENV, mode)
                .output()
                .unwrap();
            let report = String::from_utf8_lossy(&child.stdout).into_owned();
            assert!(child.status.success(), "{mode}: {report}");
            // A filter that matches nothing also exits zero, so require the
            // child to report the case it was spawned to run.
            assert!(report.contains("1 passed"), "{mode}: {report}");
        }
    }
}
