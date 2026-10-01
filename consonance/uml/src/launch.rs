// SPDX-License-Identifier: AGPL-3.0-or-later

use std::ffi::OsString;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tempfile::TempDir;

use crate::bridge::{self, Bridge, BridgeLog, Event};
use crate::console::ConsoleTail;
use crate::process::group_members;
use crate::profile::VerifiedProfile;

const POLL: Duration = Duration::from_millis(5);
const SWEEP_LIMIT: Duration = Duration::from_secs(5);
const BRIDGE_FD: i32 = 3;

#[derive(Clone, Debug)]
pub struct Launch {
    pub memory_mib: u32,
    pub console_tail_bytes: usize,
    pub console_limit_bytes: u64,
    pub wall_limit: Duration,
    pub kernel_arguments: Vec<String>,
    pub work_parent: PathBuf,
    pub bridge: Option<Bridge>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitReason {
    Exited(i32),
    Signaled(i32),
    WallLimit,
    ConsoleLimit,
    Killed,
    EventCut,
    BridgeFailure,
}

#[derive(Debug)]
pub struct Exit {
    pub reason: ExitReason,
    pub console: Vec<u8>,
    pub console_bytes: u64,
    pub leftovers: Vec<i32>,
    pub work_removed: bool,
    pub wall: Duration,
    pub events: Vec<Event>,
    pub bridge_failure: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error("{0} cannot appear on a User-mode Linux command line")]
    Path(PathBuf),
    #[error(transparent)]
    Io(#[from] io::Error),
}

pub struct Guest {
    child: Child,
    pid: i32,
    work: Option<TempDir>,
    console: Arc<Mutex<ConsoleTail>>,
    over_limit: Arc<AtomicBool>,
    reader: Option<JoinHandle<io::PipeReader>>,
    bridge_state: Arc<AtomicU8>,
    bridge: Option<JoinHandle<BridgeLog>>,
    started: Instant,
    wall_limit: Duration,
    finished: bool,
}

impl Launch {
    pub fn new(work_parent: PathBuf) -> Self {
        Self {
            memory_mib: 128,
            console_tail_bytes: 64 << 10,
            console_limit_bytes: 16 << 20,
            wall_limit: Duration::from_secs(60),
            kernel_arguments: Vec::new(),
            work_parent,
            bridge: None,
        }
    }

    pub fn command(&self, profile: &VerifiedProfile, work: &Path) -> Result<Command, LaunchError> {
        let mut command = Command::new(profile.executable());
        command
            .arg(format!("mem={}M", self.memory_mib))
            .arg(prefixed("initrd=", &profile.rootfs())?)
            .args([
                "seccomp=on",
                "time-travel=inf-cpu",
                "time-travel-start=0",
                "con0=null,fd:1",
                "con=none",
                "ssl=none",
                "umid=harmony",
            ])
            .arg(prefixed("uml_dir=", work)?)
            .args(self.bridge.iter().flat_map(|bridge| {
                [
                    format!("harmony_fd={BRIDGE_FD}"),
                    format!("harmony_seed={}", bridge.boot_seed()),
                ]
            }))
            .args(&self.kernel_arguments)
            .env_clear()
            .env("TMPDIR", work)
            .env("GLIBC_TUNABLES", "glibc.pthread.rseq=0")
            .stdin(Stdio::null())
            .process_group(0);
        Ok(command)
    }

    pub fn work_directory(&self) -> io::Result<TempDir> {
        tempfile::Builder::new()
            .prefix("harmony-uml-")
            .tempdir_in(&self.work_parent)
    }
}

fn prefixed(prefix: &str, path: &Path) -> Result<OsString, LaunchError> {
    match path.to_str() {
        Some(text) if !text.is_empty() && !text.contains(char::is_whitespace) => {
            Ok(OsString::from(format!("{prefix}{text}")))
        }
        _ => Err(LaunchError::Path(path.to_path_buf())),
    }
}

impl Guest {
    pub fn spawn(launch: &Launch, profile: &VerifiedProfile) -> Result<Self, LaunchError> {
        let work = launch.work_directory()?;
        let command = launch.command(profile, work.path())?;
        Self::start(command, work, launch)
    }

    pub fn start(command: Command, work: TempDir, launch: &Launch) -> Result<Self, LaunchError> {
        Self::start_bridged(command, work, launch, true).map(|(guest, _)| guest)
    }

    pub(crate) fn start_bridged(
        mut command: Command,
        work: TempDir,
        launch: &Launch,
        serve: bool,
    ) -> Result<(Self, Option<OwnedFd>), LaunchError> {
        let (mut reader, writer) = io::pipe()?;
        command.stdout(writer.try_clone()?).stderr(writer);
        set_parent_death_signal(&mut command);
        let host_end = match launch.bridge {
            Some(_) => {
                let (host, guest) = bridge::socket_pair()?;
                pass_bridge(&mut command, guest);
                Some(host)
            }
            None => None,
        };
        let child = command.spawn()?;
        drop(command);
        let pid = i32::try_from(child.id()).map_err(io::Error::other)?;
        let bridge_state = Arc::new(AtomicU8::new(bridge::RUNNING));
        let (bridge, held) = match (host_end, launch.bridge) {
            (Some(host), Some(config)) if serve => {
                let state = Arc::clone(&bridge_state);
                let thread = std::thread::Builder::new()
                    .name(format!("uml-bridge-{pid}"))
                    .spawn(move || bridge::serve(host, config, state))?;
                (Some(thread), None)
            }
            (host, _) => (None, host),
        };
        let console = Arc::new(Mutex::new(ConsoleTail::new(launch.console_tail_bytes)));
        let over_limit = Arc::new(AtomicBool::new(false));
        let limit = launch.console_limit_bytes;
        let reader = {
            let console = Arc::clone(&console);
            let over_limit = Arc::clone(&over_limit);
            std::thread::Builder::new()
                .name(format!("uml-console-{pid}"))
                .spawn(move || {
                    let mut block = vec![0_u8; 64 << 10];
                    loop {
                        let read = match reader.read(&mut block) {
                            Ok(0) => return reader,
                            Ok(read) => read,
                            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                            Err(_) => return reader,
                        };
                        let mut tail = console.lock().unwrap_or_else(PoisonError::into_inner);
                        tail.push(&block[..read]);
                        if tail.total() > limit {
                            over_limit.store(true, Ordering::Release);
                            return reader;
                        }
                    }
                })?
        };
        let guest = Self {
            child,
            pid,
            work: Some(work),
            console,
            over_limit,
            reader: Some(reader),
            bridge_state,
            bridge,
            started: now(),
            wall_limit: launch.wall_limit,
            finished: false,
        };
        Ok((guest, held))
    }

    pub fn pid(&self) -> i32 {
        self.pid
    }

    pub fn console_tail(&self) -> Vec<u8> {
        self.console
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .bytes()
    }

    pub fn console_contains(&self, needle: &str) -> bool {
        self.console
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(needle.as_bytes())
    }

    pub fn wait_for(&mut self, needle: &str, timeout: Duration) -> io::Result<bool> {
        let deadline = now() + timeout;
        loop {
            if self.console_contains(needle) {
                return Ok(true);
            }
            if now() >= deadline
                || self.over_limit.load(Ordering::Acquire)
                || self.child.try_wait()?.is_some()
            {
                return Ok(self.console_contains(needle));
            }
            std::thread::sleep(POLL);
        }
    }

    pub fn wait(mut self) -> io::Result<Exit> {
        let reason = loop {
            if self.over_limit.load(Ordering::Acquire) {
                break ExitReason::ConsoleLimit;
            }
            match self.bridge_state.load(Ordering::Acquire) {
                bridge::CUT => break ExitReason::EventCut,
                bridge::FAILED => break ExitReason::BridgeFailure,
                _ => {}
            }
            if let Some(reason) = self.stopped()? {
                break reason;
            }
            std::thread::sleep(POLL);
        };
        self.finish(reason)
    }

    pub(crate) fn stopped(&mut self) -> io::Result<Option<ExitReason>> {
        if self.over_limit.load(Ordering::Acquire) {
            return Ok(Some(ExitReason::ConsoleLimit));
        }
        if let Some(status) = self.child.try_wait()? {
            return Ok(Some(exit_reason(status)));
        }
        if self.started.elapsed() >= self.wall_limit {
            return Ok(Some(ExitReason::WallLimit));
        }
        Ok(None)
    }

    pub fn kill(mut self) -> io::Result<Exit> {
        self.finish(ExitReason::Killed)
    }

    pub(crate) fn finish(&mut self, reason: ExitReason) -> io::Result<Exit> {
        self.finished = true;
        let wall = self.started.elapsed();
        kill_group(self.pid);
        self.child.wait()?;
        let leftovers = sweep(self.pid);
        if leftovers.is_empty()
            && let Some(reader) = self.reader.take()
        {
            let _ = reader.join();
        }
        let (events, bridge_failure) = match self.bridge.take() {
            Some(bridge) if leftovers.is_empty() => match bridge.join() {
                Ok(log) => {
                    drop(log.socket);
                    (log.events, log.failure)
                }
                Err(_) => (Vec::new(), Some("bridge thread panicked".to_owned())),
            },
            Some(_) => (Vec::new(), Some("bridge left running".to_owned())),
            None => (Vec::new(), None),
        };
        let tail = self.console.lock().unwrap_or_else(PoisonError::into_inner);
        let (console, console_bytes) = (tail.bytes(), tail.total());
        drop(tail);
        let work_removed = match self.work.take() {
            Some(work) => {
                let path = work.path().to_path_buf();
                work.close().is_ok() && !path.exists()
            }
            None => true,
        };
        Ok(Exit {
            reason,
            console,
            console_bytes,
            leftovers,
            work_removed,
            wall,
            events,
            bridge_failure,
        })
    }
}

impl Drop for Guest {
    fn drop(&mut self) {
        if !self.finished {
            kill_group(self.pid);
            let _ = self.child.wait();
            sweep(self.pid);
        }
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "launcher limits are host wall-clock budgets outside the guest"
)]
fn now() -> Instant {
    Instant::now()
}

fn exit_reason(status: ExitStatus) -> ExitReason {
    match (status.code(), status.signal()) {
        (Some(code), _) => ExitReason::Exited(code),
        (None, Some(signal)) => ExitReason::Signaled(signal),
        (None, None) => ExitReason::Signaled(0),
    }
}

fn sweep(group: i32) -> Vec<i32> {
    let deadline = now() + SWEEP_LIMIT;
    loop {
        reap_group(group);
        let members = group_members(group);
        if members.is_empty() || now() >= deadline {
            return members;
        }
        kill_group(group);
        for member in &members {
            // SAFETY: kill(2) takes plain integers and has no memory effects; a
            // stale pid can at worst return ESRCH, which is ignored.
            unsafe {
                libc::kill(*member, libc::SIGKILL);
            }
        }
        std::thread::sleep(POLL);
    }
}

fn kill_group(group: i32) {
    // SAFETY: killpg(2) takes plain integers and has no memory effects. The
    // group id is the launched child's pid, which leads its own process group.
    unsafe {
        libc::killpg(group, libc::SIGKILL);
    }
}

fn reap_group(group: i32) {
    loop {
        let mut status = 0;
        // SAFETY: `status` is a live, writable c_int for the duration of the call.
        let reaped = unsafe { libc::waitpid(-group, &mut status, libc::WNOHANG) };
        if reaped <= 0 {
            return;
        }
    }
}

#[cfg(target_os = "linux")]
fn set_parent_death_signal(command: &mut Command) {
    // SAFETY: the closure runs in the forked child before exec and calls only
    // prctl(2), which is async-signal-safe and touches no shared memory.
    unsafe {
        command.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(target_os = "linux"))]
fn set_parent_death_signal(_command: &mut Command) {}

fn pass_bridge(command: &mut Command, guest: OwnedFd) {
    // SAFETY: the closure runs in the forked child before exec and calls only
    // dup2(2) and fcntl(2), which are async-signal-safe. `guest` stays open
    // until the closure is dropped with the command after spawn.
    unsafe {
        command.pre_exec(move || {
            let fd = guest.as_raw_fd();
            let result = if fd == BRIDGE_FD {
                libc::fcntl(fd, libc::F_SETFD, 0)
            } else {
                libc::dup2(fd, BRIDGE_FD)
            };
            if result < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_paths_reject_whitespace() {
        assert_eq!(
            prefixed("initrd=", Path::new("/a/b")).unwrap(),
            OsString::from("initrd=/a/b")
        );
        assert!(matches!(
            prefixed("initrd=", Path::new("/a b")),
            Err(LaunchError::Path(_))
        ));
    }

    #[cfg(target_os = "linux")]
    fn launch(work: &Path) -> Launch {
        let mut launch = Launch::new(work.to_path_buf());
        launch.console_tail_bytes = 16;
        launch.console_limit_bytes = 1 << 16;
        launch.wall_limit = Duration::from_secs(10);
        launch
    }

    #[cfg(target_os = "linux")]
    fn shell(launch: &Launch, script: &str) -> Guest {
        let work = launch.work_directory().unwrap();
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]).process_group(0);
        Guest::start(command, work, launch).unwrap()
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn wait_reports_exit_and_bounded_console() {
        let parent = tempfile::tempdir().unwrap();
        let launch = launch(parent.path());
        let exit = shell(&launch, "echo 0123456789abcdefXYZ; exit 7")
            .wait()
            .unwrap();
        assert_eq!(exit.reason, ExitReason::Exited(7));
        assert_eq!(exit.console, b"456789abcdefXYZ\n");
        assert_eq!(exit.console_bytes, 20);
        assert!(exit.leftovers.is_empty());
        assert!(exit.work_removed);
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn limits_stop_the_whole_group() {
        let parent = tempfile::tempdir().unwrap();
        let mut launch = launch(parent.path());
        launch.wall_limit = Duration::from_millis(300);
        let mut guest = shell(&launch, "sleep 30 & sleep 30 & echo ready; wait");
        assert!(guest.wait_for("ready", Duration::from_secs(5)).unwrap());
        assert!(group_members(guest.pid()).len() >= 3);
        let exit = guest.wait().unwrap();
        assert_eq!(exit.reason, ExitReason::WallLimit);
        assert!(exit.leftovers.is_empty());

        let exit = shell(&launch, "yes flood").wait().unwrap();
        assert_eq!(exit.reason, ExitReason::ConsoleLimit);
        assert!(exit.console.len() <= 16);
        assert!(exit.leftovers.is_empty());
    }
}
