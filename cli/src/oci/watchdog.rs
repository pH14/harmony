// SPDX-License-Identifier: AGPL-3.0-or-later
//! Host-only KVM timeout. SIGUSR1 interrupts the owning thread, and the
//! backend's cancellation latch prevents EINTR from re-entering the guest.
//! The guard cannot leave that thread and joins the sender before it exits.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;

pub(super) struct Watchdog {
    done: mpsc::Sender<()>,
    thread: Option<std::thread::JoinHandle<()>>,
    _owner: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl Watchdog {
    #[cfg(not(miri))]
    pub(super) fn start(budget: Duration, cancel: Arc<AtomicBool>) -> std::io::Result<Self> {
        install_signal()?;
        // SAFETY: pthread_self returns the calling thread's live identifier.
        // The non-Send guard joins the only user of it before this thread exits.
        let owner = unsafe { libc::pthread_self() };
        let (done, receiver) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("kvm-timeout".into())
            .spawn(move || {
                watch(receiver, budget, &cancel, || {
                    // SAFETY: owner stays alive until this sender has been joined.
                    // SIGUSR1 has a no-op handler and is unblocked on that thread.
                    let _ = unsafe { libc::pthread_kill(owner, libc::SIGUSR1) };
                });
            })?;
        Ok(Self {
            done,
            thread: Some(thread),
            _owner: std::marker::PhantomData,
        })
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

fn watch(done: mpsc::Receiver<()>, budget: Duration, cancel: &AtomicBool, mut kick: impl FnMut()) {
    if done.recv_timeout(budget) != Err(mpsc::RecvTimeoutError::Timeout) {
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
    // caller's mask. SIGUSR1 is reserved by this CLI for canceling its own VM.
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
            watch(rx, Duration::ZERO, &cancel, || {
                panic!("normal completion sent a signal")
            });
            assert!(!cancel.load(Ordering::Acquire));
        }
    }

    #[test]
    fn expiry_publishes_cancellation_before_signaling() {
        let (tx, rx) = mpsc::channel();
        let cancel = AtomicBool::new(false);
        let mut signals = 0;
        watch(rx, Duration::ZERO, &cancel, || {
            assert!(cancel.load(Ordering::Acquire));
            signals += 1;
            tx.send(()).unwrap();
        });
        assert_eq!(signals, 1);
        interrupt(libc::SIGUSR1);
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
            assert!(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "oci::watchdog::tests::installs_for_default_and_ignored_but_rejects_owned_signal"])
                .env(ENV, mode).status().unwrap().success());
        }
    }
}
