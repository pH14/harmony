// SPDX-License-Identifier: AGPL-3.0-or-later

use std::sync::atomic::{AtomicBool, Ordering};

static REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn requested(signal: libc::c_int) {
    if REQUESTED.swap(true, Ordering::AcqRel) {
        // SAFETY: _exit is async-signal-safe and ends the process at once
        // without running any handler, destructor or buffered writer.
        unsafe { libc::_exit(128 + signal) };
    }
}

#[cfg(not(miri))]
pub fn install() -> std::io::Result<&'static AtomicBool> {
    set(requested as *const () as libc::sighandler_t)?;
    Ok(&REQUESTED)
}

#[cfg(not(miri))]
pub fn ignore() -> std::io::Result<()> {
    set(libc::SIG_IGN)
}

#[cfg(not(miri))]
fn set(handler: libc::sighandler_t) -> std::io::Result<()> {
    for signal in [libc::SIGINT, libc::SIGTERM] {
        // SAFETY: action is a zero-initialized C record whose mask is
        // initialized by sigemptyset before sigaction reads it. The handler is
        // SIG_IGN or a C-ABI function that performs only an atomic swap and
        // the async-signal-safe _exit.
        let rc = unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = handler;
            libc::sigemptyset(&raw mut action.sa_mask);
            libc::sigaction(signal, &raw const action, std::ptr::null_mut())
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_signal_only_requests_a_stop() {
        requested(libc::SIGINT);
        assert!(REQUESTED.load(Ordering::Acquire));
    }
}
