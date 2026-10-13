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
#[cfg(target_os = "linux")]
use crate::profile::PinnedImage;
use crate::profile::VerifiedProfile;

const POLL: Duration = Duration::from_millis(5);
const SWEEP_LIMIT: Duration = Duration::from_secs(5);
const BRIDGE_FD: i32 = 3;
pub(crate) const PHYSMEM_FD: i32 = 4;
const HIGH_FD: i32 = 10;

#[cfg(target_os = "linux")]
fn fixed_host_layout(command: &mut Command) {
    // SAFETY: the child calls personality(2) only on itself before exec, without
    // allocation or shared-memory access.
    unsafe {
        command.pre_exec(|| {
            let _ = libc::personality(libc::ADDR_NO_RANDOMIZE as libc::c_ulong);
            Ok(())
        });
    }
}

#[derive(Clone, Debug)]
pub struct Launch {
    pub memory_mib: u32,
    pub console_tail_bytes: usize,
    pub console_limit_bytes: u64,
    pub wall_limit: Duration,
    pub kernel_arguments: Vec<String>,
    pub work_parent: PathBuf,
    pub bridge: Option<Bridge>,
    #[cfg(target_os = "linux")]
    initramfs: Option<Arc<PinnedImage>>,
    #[cfg(not(target_os = "linux"))]
    initramfs: Option<PathBuf>,
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
    #[error("kernel argument {0} sets a host resource that the launcher owns")]
    Reserved(String),
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
            initramfs: None,
        }
    }

    pub fn set_initramfs(&mut self, path: &Path) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            self.initramfs = Some(Arc::new(PinnedImage::from_path(
                path,
                c"harmony-uml-initramfs",
                false,
            )?));
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.initramfs = Some(path.to_path_buf());
        }
        Ok(())
    }

    pub fn has_initramfs_override(&self) -> bool {
        self.initramfs.is_some()
    }

    pub fn initramfs_bytes(&self) -> io::Result<Option<Vec<u8>>> {
        #[cfg(target_os = "linux")]
        {
            self.initramfs
                .as_ref()
                .map(|image| image.bytes())
                .transpose()
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.initramfs.as_ref().map(std::fs::read).transpose()
        }
    }

    pub fn command(&self, profile: &VerifiedProfile, work: &Path) -> Result<Command, LaunchError> {
        if let Some(argument) = self
            .kernel_arguments
            .iter()
            .find(|argument| reserved(argument))
        {
            return Err(LaunchError::Reserved(argument.clone()));
        }
        #[cfg(target_os = "linux")]
        let initramfs = self
            .initramfs
            .as_ref()
            .map_or_else(|| profile.rootfs(), |image| image.path());
        #[cfg(not(target_os = "linux"))]
        let initramfs = self.initramfs.clone().unwrap_or_else(|| profile.rootfs());
        let mut command = Command::new(profile.executable());
        command
            .arg(format!("mem={}M", self.memory_mib))
            .arg(prefixed("initrd=", &initramfs)?)
            .args([
                "seccomp=on",
                "time-travel=inf-cpu",
                "time-travel-start=0",
                "con0=null,fd:1",
                "con=null",
                "ssl=null",
                "umid=harmony",
                "noreboot",
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
        #[cfg(target_os = "linux")]
        {
            fixed_host_layout(&mut command);
            profile.pass_artifacts(&mut command);
            if let Some(image) = &self.initramfs {
                image.pass(&mut command);
            }
        }
        Ok(command)
    }

    pub fn work_directory(&self) -> io::Result<TempDir> {
        tempfile::Builder::new()
            .prefix("harmony-uml-")
            .tempdir_in(&self.work_parent)
    }
}

const RESERVED: [&str; 12] = [
    "mem",
    "initrd",
    "seccomp",
    "umid",
    "noreboot",
    "uml_dir",
    "harmony_fd",
    "harmony_seed",
    "hostfs",
    "iomem",
    "mconsole",
    "xterm",
];

const RESERVED_NUMBERED: [&str; 4] = ["con", "ssl", "eth", "vec"];

fn reserved(argument: &str) -> bool {
    let name = argument.split_once('=').map_or(argument, |(name, _)| name);
    RESERVED.contains(&name)
        || name.starts_with("time-travel")
        || name.starts_with("ubd")
        || name.starts_with("virtio_uml")
        || RESERVED_NUMBERED.iter().any(|prefix| {
            name.strip_prefix(prefix)
                .is_some_and(|rest| rest.bytes().all(|byte| byte.is_ascii_digit()))
        })
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
        Self::start_bridged(command, work, launch, true, None).map(|(guest, _)| guest)
    }

    pub(crate) fn start_bridged(
        mut command: Command,
        work: TempDir,
        launch: &Launch,
        serve: bool,
        physmem: Option<OwnedFd>,
    ) -> Result<(Self, Option<OwnedFd>), LaunchError> {
        let (mut reader, writer) = io::pipe()?;
        command.stdout(writer.try_clone()?).stderr(writer);
        set_parent_death_signal(&mut command);
        let host_end = match launch.bridge {
            Some(_) => {
                let (host, guest) = bridge::socket_pair()?;
                pass_bridge(&mut command, guest, physmem);
                Some(host)
            }
            None => None,
        };
        let child = command.spawn()?;
        drop(command);
        let pid = i32::try_from(child.id()).map_err(io::Error::other)?;
        let bridge_state = Arc::new(AtomicU8::new(
            if serve && launch.bridge.is_some_and(|bridge| bridge.cut == Some(0)) {
                bridge::CUT
            } else {
                bridge::RUNNING
            },
        ));
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

fn pass_bridge(command: &mut Command, guest: OwnedFd, physmem: Option<OwnedFd>) {
    // SAFETY: the closure runs in the forked child before exec and calls only
    // fcntl(2), dup2(2) and close(2), which are async-signal-safe. Both
    // descriptors stay open until the closure is dropped with the command
    // after spawn. Each source is first copied above the targets so that a
    // source already at one target is not overwritten by the other.
    unsafe {
        command.pre_exec(move || {
            let passed = [
                Some((&guest, BRIDGE_FD)),
                physmem.as_ref().map(|fd| (fd, PHYSMEM_FD)),
            ];
            let mut copies = [-1; 2];
            for (copy, (fd, _)) in copies.iter_mut().zip(passed.iter().flatten()) {
                *copy = libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD, HIGH_FD);
                if *copy < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            for (copy, (_, target)) in copies.iter().zip(passed.iter().flatten()) {
                if libc::dup2(*copy, *target) < 0 {
                    return Err(io::Error::last_os_error());
                }
                libc::close(*copy);
            }
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn a_pinned_kernel_requests_fixed_layout_before_exec() {
        let mut probe = Command::new("sh");
        probe.arg("-c").arg("true");
        // SAFETY: this test changes only the forked child's personality before
        // exec; the syscall touches no shared memory or Rust allocator state.
        unsafe {
            probe.pre_exec(|| {
                if libc::personality(libc::ADDR_NO_RANDOMIZE as libc::c_ulong) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        match probe.status() {
            Ok(status) => assert!(status.success()),
            Err(error) if error.raw_os_error() == Some(libc::EPERM) => return,
            Err(error) => panic!("fixed layout probe failed: {error}"),
        }

        use std::os::unix::fs::PermissionsExt;

        use crate::profile::{Artifact, Profile, VirtualTimeCosts};

        let directory = tempfile::tempdir().unwrap();
        let kernel = directory.path().join("kernel");
        std::fs::write(&kernel, b"#!/bin/sh\ncat /proc/self/personality\n").unwrap();
        std::fs::set_permissions(&kernel, std::fs::Permissions::from_mode(0o755)).unwrap();
        let artifact = |name: &str| Artifact {
            name: name.to_owned(),
            sha256: String::new(),
        };
        let profile = VerifiedProfile::for_test(
            directory.path().to_path_buf(),
            Profile {
                schema: 2,
                architecture: std::env::consts::ARCH.to_owned(),
                kernel_version: String::new(),
                userspace: "seccomp".to_owned(),
                patch_series_sha256: String::new(),
                host_libraries: Vec::new(),
                executable: artifact("kernel"),
                config: artifact("config"),
                rootfs: artifact("rootfs"),
                virtual_time: VirtualTimeCosts {
                    syscall_vns: 0,
                    clock_read_vns: 0,
                },
            },
            String::new(),
        );
        let launch = Launch::new(directory.path().to_path_buf());
        let mut command = launch.command(&profile, directory.path()).unwrap();
        let output = command.output().unwrap();
        assert!(output.status.success());
        let flags =
            u32::from_str_radix(std::str::from_utf8(&output.stdout).unwrap().trim(), 16).unwrap();
        assert_ne!(flags & libc::ADDR_NO_RANDOMIZE as u32, 0);
    }

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

    #[test]
    fn launcher_owned_kernel_arguments_are_reserved() {
        for argument in [
            "initrd=/tmp/other.cpio",
            "mem=4096M",
            "uml_dir=/tmp",
            "time-travel=off",
            "con=fd:0",
            "con1=pts",
            "ssl0=null",
            "ubd0=/tmp/disk",
            "ubdbrc=/tmp/disk",
            "eth0=tuntap",
            "hostfs=/",
            "harmony_seed=1",
            "noreboot",
        ] {
            assert!(reserved(argument), "{argument}");
        }
        for argument in [
            "harmony_fixture=values",
            "rdinit=/init",
            "console=ttyS0",
            "quiet",
            "memblock=debug",
            "faults.knob=1",
        ] {
            assert!(!reserved(argument), "{argument}");
        }
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
    fn zero_event_cut_stops_a_live_guest_before_any_event() {
        let parent = tempfile::tempdir().unwrap();
        let mut launch = launch(parent.path());
        launch.bridge = Some(Bridge {
            seed: 7,
            cut: Some(0),
        });
        launch.wall_limit = Duration::from_millis(500);
        let exit = shell(&launch, "sleep 30").wait().unwrap();
        assert_eq!(exit.reason, ExitReason::EventCut);
        assert!(exit.events.is_empty());
        assert!(exit.leftovers.is_empty());
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

        launch.wall_limit = Duration::from_secs(10);
        let exit = shell(&launch, "yes flood").wait().unwrap();
        assert_eq!(exit.reason, ExitReason::ConsoleLimit);
        assert!(exit.console.len() <= 16);
        assert!(exit.leftovers.is_empty());
    }
}
