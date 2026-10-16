// SPDX-License-Identifier: AGPL-3.0-or-later

use std::ffi::OsString;
#[cfg(target_os = "linux")]
use std::io::Write;
use std::io::{self, Read};
#[cfg(target_os = "linux")]
use std::mem::size_of;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
#[cfg(target_os = "linux")]
use std::sync::OnceLock;
#[cfg(test)]
use std::sync::atomic::AtomicI32;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{self, Sender, SyncSender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tempfile::TempDir;

use crate::bridge::{self, Bridge, BridgeLog, Event};
use crate::console::ConsoleTail;
use crate::process::group_members;
#[cfg(target_os = "linux")]
use crate::profile::PinnedImage;
use crate::profile::{HostIdentity, VerifiedProfile};
#[cfg(target_os = "linux")]
use crate::profile::{processor_allowed, selected_processor};

const POLL: Duration = Duration::from_millis(5);
const SWEEP_LIMIT: Duration = Duration::from_secs(5);
const BRIDGE_FD: i32 = 3;
pub(crate) const PHYSMEM_FD: i32 = 4;
const HIGH_FD: i32 = 10;
const GUEST_PANIC_STATUS: i32 = 86;
const HOST_CLONE_FAILURE_STATUS: i32 = 87;

#[cfg(test)]
static TEST_EVENT_CUT_FINISH_PID: AtomicI32 = AtomicI32::new(0);
#[cfg(test)]
static TEST_EVENT_CUT_FINISH_READY: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
pub(crate) static TEST_EXIT_FINISH_PID: AtomicI32 = AtomicI32::new(0);
#[cfg(test)]
pub(crate) static TEST_EXIT_FINISH_READY: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
pub(crate) static TEST_HOLD_TERMINAL_CONSOLE: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
pub(crate) static TEST_TERMINAL_CONSOLE_PID: AtomicI32 = AtomicI32::new(0);
#[cfg(test)]
pub(crate) static TEST_TERMINAL_CONSOLE_READY: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "linux")]
fn fixed_host_layout(command: &mut Command) {
    // SAFETY: the child calls personality(2) only on itself before exec, without
    // allocation or shared-memory access.
    unsafe {
        command.pre_exec(|| {
            if libc::personality(libc::ADDR_NO_RANDOMIZE as libc::c_ulong) < 0
                && libc::personality(0) < 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(target_os = "linux")]
fn singleton_cpu_mask(cpu: usize) -> Option<libc::cpu_set_t> {
    if cpu >= libc::CPU_SETSIZE as usize {
        return None;
    }
    // SAFETY: cpu_set_t is plain data and zero is its empty set. The checked
    // index is inside its storage before CPU_SET writes the bit.
    let mut mask: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    // SAFETY: cpu is below CPU_SETSIZE and mask is a live cpu_set_t.
    unsafe { libc::CPU_SET(cpu, &mut mask) };
    Some(mask)
}

#[cfg(target_os = "linux")]
fn enforce_host_cpu(command: &mut Command, cpu: usize, shared: bool) -> Result<(), LaunchError> {
    let mask = singleton_cpu_mask(cpu).ok_or(LaunchError::HostCpuUnavailable(cpu))?;
    if !shared && !processor_allowed(cpu)? {
        return Err(LaunchError::HostCpuUnavailable(cpu));
    }
    // SAFETY: this is the final pre_exec callback installed by Guest. It only
    // changes the forked child's affinity using a captured stack-sized mask;
    // every subsequent UML process inherits that single-CPU affinity.
    unsafe {
        command.pre_exec(move || {
            if libc::sched_setaffinity(0, size_of::<libc::cpu_set_t>(), &mask) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(())
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
    admitted_processor: Arc<OnceLock<usize>>,
    #[cfg(target_os = "linux")]
    admitted_host: Arc<OnceLock<HostIdentity>>,
    #[cfg(target_os = "linux")]
    expected_host: Option<HostIdentity>,
    #[cfg(target_os = "linux")]
    shared_host: bool,
    #[cfg(target_os = "linux")]
    initramfs: Option<Arc<PinnedImage>>,
    #[cfg(not(target_os = "linux"))]
    initramfs: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitReason {
    Exited(i32),
    Signaled(i32),
    GuestPanic,
    HostFailure,
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
    #[cfg(target_os = "linux")]
    #[error("UML host processor {0} is unavailable in this process's CPU affinity mask")]
    HostCpuUnavailable(usize),
    #[cfg(target_os = "linux")]
    #[error(
        "this launch admitted UML host processor {admitted}, not requested processor {requested}"
    )]
    HostCpuAlreadyAdmitted { admitted: usize, requested: usize },
    #[cfg(target_os = "linux")]
    #[error(
        "the launch's recorded host identity {expected:?} differs from this processor {current:?}"
    )]
    HostIdentity {
        expected: Box<HostIdentity>,
        current: Box<HostIdentity>,
    },
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
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    console_reset: Sender<SyncSender<io::Result<()>>>,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    console_wake: io::PipeWriter,
    bridge_state: Arc<AtomicU8>,
    bridge: Option<JoinHandle<BridgeLog>>,
    started: Instant,
    wall_limit: Duration,
    finished: bool,
}

enum ConsoleRead {
    Bytes,
    Idle,
    Closed,
    Limit,
}

fn read_console(
    reader: &mut io::PipeReader,
    block: &mut [u8],
    console: &Mutex<ConsoleTail>,
    over_limit: &AtomicBool,
    limit: u64,
) -> io::Result<ConsoleRead> {
    loop {
        match reader.read(block) {
            Ok(0) => return Ok(ConsoleRead::Closed),
            Ok(read) => {
                let mut tail = console.lock().unwrap_or_else(PoisonError::into_inner);
                tail.push(&block[..read]);
                if tail.total() > limit {
                    over_limit.store(true, Ordering::Release);
                    return Ok(ConsoleRead::Limit);
                }
                return Ok(ConsoleRead::Bytes);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                return Ok(ConsoleRead::Idle);
            }
            Err(error) => return Err(error),
        }
    }
}

fn drain_console(
    reader: &mut io::PipeReader,
    block: &mut [u8],
    console: &Mutex<ConsoleTail>,
    over_limit: &AtomicBool,
    limit: u64,
) -> io::Result<()> {
    loop {
        match read_console(reader, block, console, over_limit, limit)? {
            ConsoleRead::Bytes => {}
            ConsoleRead::Idle => {
                console
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clear_bytes();
                return Ok(());
            }
            ConsoleRead::Closed => return Err(io::ErrorKind::BrokenPipe.into()),
            ConsoleRead::Limit => return Err(io::Error::other("console limit reached")),
        }
    }
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
            #[cfg(target_os = "linux")]
            admitted_processor: Arc::new(OnceLock::new()),
            #[cfg(target_os = "linux")]
            admitted_host: Arc::new(OnceLock::new()),
            #[cfg(target_os = "linux")]
            expected_host: None,
            #[cfg(target_os = "linux")]
            shared_host: false,
            initramfs: None,
        }
    }

    #[cfg(target_os = "linux")]
    pub fn admitted_processor(&self) -> Result<usize, LaunchError> {
        let cpu = match self.admitted_processor.get() {
            Some(cpu) => *cpu,
            None => {
                let candidate = selected_processor()?;
                *self.admitted_processor.get_or_init(|| candidate)
            }
        };
        if !self.shared_host && !processor_allowed(cpu)? {
            return Err(LaunchError::HostCpuUnavailable(cpu));
        }
        Ok(cpu)
    }

    pub fn host_identity(&self) -> Result<HostIdentity, LaunchError> {
        #[cfg(target_os = "linux")]
        {
            let current = HostIdentity::for_processor(self.admitted_processor()?)?;
            let admitted = self.admitted_host.get_or_init(|| current.clone());
            if admitted != &current {
                return Err(LaunchError::HostIdentity {
                    expected: Box::new(admitted.clone()),
                    current: Box::new(current),
                });
            }
            if let Some(expected) = &self.expected_host
                && expected != &current
            {
                return Err(LaunchError::HostIdentity {
                    expected: Box::new(expected.clone()),
                    current: Box::new(current),
                });
            }
            Ok(current)
        }
        #[cfg(not(target_os = "linux"))]
        {
            Ok(HostIdentity::current()?)
        }
    }

    pub fn bind_current_host(&mut self) -> Result<HostIdentity, LaunchError> {
        let host = self.host_identity()?;
        #[cfg(target_os = "linux")]
        {
            self.expected_host = Some(host.clone());
        }
        Ok(host)
    }

    #[cfg(target_os = "linux")]
    pub fn bind_expected_host(&mut self, host: HostIdentity) -> Result<(), LaunchError> {
        let current = HostIdentity::for_processor(host.processor)?;
        if current != host {
            return Err(LaunchError::HostIdentity {
                expected: Box::new(host),
                current: Box::new(current),
            });
        }
        let admitted = *self.admitted_processor.get_or_init(|| host.processor);
        if admitted != host.processor {
            return Err(LaunchError::HostCpuAlreadyAdmitted {
                admitted,
                requested: host.processor,
            });
        }
        let admitted_host = self.admitted_host.get_or_init(|| host.clone());
        if admitted_host != &host {
            return Err(LaunchError::HostIdentity {
                expected: Box::new(admitted_host.clone()),
                current: Box::new(host),
            });
        }
        self.expected_host = Some(host);
        self.shared_host = true;
        Ok(())
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn with_recorded_host(mut self, host: HostIdentity) -> Self {
        let _ = self.admitted_processor.set(host.processor);
        let _ = self.admitted_host.set(host.clone());
        self.expected_host = Some(host);
        self.shared_host = true;
        self
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
        let flags = rustix::fs::fcntl_getfl(&reader).map_err(io::Error::from)?;
        rustix::fs::fcntl_setfl(&reader, flags | rustix::fs::OFlags::NONBLOCK)
            .map_err(io::Error::from)?;
        let (mut wake_reader, console_wake) = io::pipe()?;
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
        #[cfg(target_os = "linux")]
        {
            let host = launch.host_identity()?;
            command.env("HARMONY_UML_CPUINFO", &host.boot_cpuinfo);
            command.env("HARMONY_UML_HWCAP", host.auxv.hwcap.to_string());
            command.env("HARMONY_UML_HWCAP2", host.auxv.hwcap2.to_string());
            command.env("HARMONY_UML_MINSIGSTKSZ", host.auxv.minsigstksz.to_string());
            command.env("HARMONY_UML_PLATFORM", &host.auxv.platform);
            enforce_host_cpu(&mut command, host.processor, launch.shared_host)?;
        }
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
        let (console_reset, resets) = mpsc::channel::<SyncSender<io::Result<()>>>();
        let reader = {
            let console = Arc::clone(&console);
            let over_limit = Arc::clone(&over_limit);
            std::thread::Builder::new()
                .name(format!("uml-console-{pid}"))
                .spawn(move || {
                    let mut block = vec![0_u8; 64 << 10];
                    loop {
                        let (console_ready, reset_ready) = {
                            let mut fds = [
                                rustix::event::PollFd::new(&reader, rustix::event::PollFlags::IN),
                                rustix::event::PollFd::new(
                                    &wake_reader,
                                    rustix::event::PollFlags::IN,
                                ),
                            ];
                            match rustix::event::poll(&mut fds, None) {
                                Ok(_) => {
                                    (!fds[0].revents().is_empty(), !fds[1].revents().is_empty())
                                }
                                Err(rustix::io::Errno::INTR) => continue,
                                Err(_) => return reader,
                            }
                        };
                        if reset_ready {
                            let mut wake = [0];
                            if wake_reader.read_exact(&mut wake).is_err() {
                                return reader;
                            }
                            let Ok(ack) = resets.recv() else {
                                return reader;
                            };
                            let result = drain_console(
                                &mut reader,
                                &mut block,
                                &console,
                                &over_limit,
                                limit,
                            );
                            let stopped = result.is_err();
                            let _ = ack.send(result);
                            if stopped {
                                return reader;
                            }
                        }
                        if console_ready {
                            #[cfg(test)]
                            if TEST_TERMINAL_CONSOLE_PID.load(Ordering::Acquire) == pid {
                                TEST_TERMINAL_CONSOLE_READY.store(true, Ordering::Release);
                                while TEST_HOLD_TERMINAL_CONSOLE.load(Ordering::Acquire) {
                                    std::thread::yield_now();
                                }
                            }
                            match read_console(
                                &mut reader,
                                &mut block,
                                &console,
                                &over_limit,
                                limit,
                            ) {
                                Ok(ConsoleRead::Bytes | ConsoleRead::Idle) => {}
                                Ok(ConsoleRead::Closed | ConsoleRead::Limit) | Err(_) => {
                                    return reader;
                                }
                            }
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
            console_reset,
            console_wake,
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

    #[cfg(target_os = "linux")]
    pub(crate) fn reset_console(&mut self) -> io::Result<()> {
        let (ack, reply) = mpsc::sync_channel(0);
        self.console_reset
            .send(ack)
            .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
        self.console_wake.write_all(&[1])?;
        reply
            .recv()
            .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?
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
            let stopped = self.stopped()?;
            if stopped == Some(ExitReason::HostFailure) {
                break ExitReason::HostFailure;
            }
            if self.over_limit.load(Ordering::Acquire) {
                break ExitReason::ConsoleLimit;
            }
            match self.bridge_state.load(Ordering::Acquire) {
                bridge::CUT => break ExitReason::EventCut,
                bridge::FAILED => break ExitReason::BridgeFailure,
                _ => {}
            }
            if let Some(reason) = stopped {
                break reason;
            }
            std::thread::sleep(POLL);
        };
        self.finish(reason)
    }

    pub(crate) fn stopped(&mut self) -> io::Result<Option<ExitReason>> {
        let status = self.child.try_wait()?;
        if status.is_some_and(|status| exit_reason(status) == ExitReason::HostFailure) {
            return Ok(Some(ExitReason::HostFailure));
        }
        if self.over_limit.load(Ordering::Acquire) {
            return Ok(Some(ExitReason::ConsoleLimit));
        }
        if let Some(status) = status {
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
        let status = self.child.wait()?;
        let reason = if exit_reason(status) == ExitReason::HostFailure {
            ExitReason::HostFailure
        } else {
            reason
        };
        let leftovers = sweep(self.pid);
        #[cfg(test)]
        if reason == ExitReason::EventCut
            && TEST_EVENT_CUT_FINISH_PID.load(Ordering::Acquire) == self.pid
        {
            TEST_EVENT_CUT_FINISH_READY.store(true, Ordering::Release);
        }
        #[cfg(test)]
        if reason == ExitReason::Exited(0)
            && TEST_EXIT_FINISH_PID.load(Ordering::Acquire) == self.pid
        {
            TEST_EXIT_FINISH_READY.store(true, Ordering::Release);
        }
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
        let reason = if reason == ExitReason::HostFailure {
            reason
        } else if reason != ExitReason::Killed && self.over_limit.load(Ordering::Acquire) {
            ExitReason::ConsoleLimit
        } else if matches!(
            reason,
            ExitReason::Exited(_) | ExitReason::Signaled(_) | ExitReason::GuestPanic
        ) {
            match self.bridge_state.load(Ordering::Acquire) {
                bridge::CUT => ExitReason::EventCut,
                bridge::FAILED => ExitReason::BridgeFailure,
                _ => reason,
            }
        } else {
            reason
        };
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
        (Some(GUEST_PANIC_STATUS), _) => ExitReason::GuestPanic,
        (Some(HOST_CLONE_FAILURE_STATUS), _) => ExitReason::HostFailure,
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
    fn kernel_statuses_are_distinct_from_guest_exit_statuses() {
        assert_eq!(
            exit_reason(ExitStatus::from_raw(GUEST_PANIC_STATUS << 8)),
            ExitReason::GuestPanic
        );
        assert_eq!(
            exit_reason(ExitStatus::from_raw(HOST_CLONE_FAILURE_STATUS << 8)),
            ExitReason::HostFailure
        );
        assert_eq!(exit_reason(ExitStatus::from_raw(0)), ExitReason::Exited(0));
        assert_eq!(
            exit_reason(ExitStatus::from_raw(7 << 8)),
            ExitReason::Exited(7)
        );
    }

    #[cfg(target_os = "linux")]
    fn deny_fixed_layout(command: &mut Command, deny_zero: bool) {
        let mut program = [
            libc::sock_filter {
                code: 0x20,
                jt: 0,
                jf: 0,
                k: 0,
            },
            libc::sock_filter {
                code: 0x15,
                jt: 0,
                jf: 5,
                k: u32::try_from(libc::SYS_personality).unwrap(),
            },
            libc::sock_filter {
                code: 0x20,
                jt: 0,
                jf: 0,
                k: 16,
            },
            libc::sock_filter {
                code: 0x15,
                jt: 2,
                jf: 0,
                k: libc::ADDR_NO_RANDOMIZE as u32,
            },
            libc::sock_filter {
                code: 0x15,
                jt: 1,
                jf: 0,
                k: if deny_zero {
                    0
                } else {
                    libc::ADDR_NO_RANDOMIZE as u32
                },
            },
            libc::sock_filter {
                code: 0x06,
                jt: 0,
                jf: 0,
                k: libc::SECCOMP_RET_ALLOW,
            },
            libc::sock_filter {
                code: 0x06,
                jt: 0,
                jf: 0,
                k: libc::SECCOMP_RET_ERRNO | libc::EPERM as u32,
            },
            libc::sock_filter {
                code: 0x06,
                jt: 0,
                jf: 0,
                k: libc::SECCOMP_RET_ALLOW,
            },
        ];
        // SAFETY: the forked child only calls personality(2) and prctl(2)
        // before exec. The initialized filter stays live until the kernel
        // copies it, and no shared memory or allocator state is touched.
        unsafe {
            command.pre_exec(move || {
                if libc::personality(0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::personality(libc::UNAME26 as libc::c_ulong) < 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
                let filter = libc::sock_fprog {
                    len: program.len() as u16,
                    filter: program.as_mut_ptr(),
                };
                if libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &filter) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if !deny_zero {
                    if libc::personality(0) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if libc::personality(libc::UNAME26 as libc::c_ulong) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn singleton_host_cpu_mask_contains_only_the_selected_cpu() {
        let mask = singleton_cpu_mask(2).unwrap();
        // SAFETY: indices 1 and 2 are below CPU_SETSIZE and mask is initialized.
        unsafe {
            assert!(!libc::CPU_ISSET(1, &mask));
            assert!(libc::CPU_ISSET(2, &mask));
        }
        assert!(singleton_cpu_mask(libc::CPU_SETSIZE as usize).is_none());
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn guest_start_passes_the_admitted_host_imports() {
        let directory = tempfile::tempdir().unwrap();
        let mut launch = Launch::new(directory.path().to_path_buf());
        let host = launch.host_identity().unwrap();
        let expected = format!(
            "{}|{}|{}|{}|{}",
            host.boot_cpuinfo,
            host.auxv.hwcap,
            host.auxv.hwcap2,
            host.auxv.minsigstksz,
            host.auxv.platform
        );
        launch.console_tail_bytes = expected.len() + 1;
        launch.console_limit_bytes = (expected.len() + 1) as u64;
        let work = launch.work_directory().unwrap();
        let mut command = Command::new("sh");
        command.arg("-c").arg(
            "printf '%s|%s|%s|%s|%s' \"$HARMONY_UML_CPUINFO\" \"$HARMONY_UML_HWCAP\" \"$HARMONY_UML_HWCAP2\" \"$HARMONY_UML_MINSIGSTKSZ\" \"$HARMONY_UML_PLATFORM\"",
        );
        let exit = Guest::start(command, work, &launch)
            .unwrap()
            .wait()
            .unwrap();
        assert_eq!(exit.reason, ExitReason::Exited(0));
        assert_eq!(exit.console, expected.as_bytes());
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn final_host_cpu_pin_overrides_an_earlier_affinity_callback() {
        let launch = Launch::new(std::env::temp_dir());
        let cpu = launch.admitted_processor().unwrap();
        // SAFETY: cpu_set_t is plain data and the kernel writes within the
        // provided object. CPU_ISSET reads only checked indices afterward.
        let mut allowed: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        // SAFETY: allowed is a live cpu_set_t of the supplied size.
        assert_eq!(
            unsafe { libc::sched_getaffinity(0, size_of::<libc::cpu_set_t>(), &mut allowed) },
            0
        );
        let other = (0..libc::CPU_SETSIZE as usize).find(|candidate| {
            // SAFETY: candidate is below CPU_SETSIZE and allowed is initialized.
            *candidate != cpu && unsafe { libc::CPU_ISSET(*candidate, &allowed) }
        });
        let Some(other) = other else {
            return;
        };
        let prior_mask = singleton_cpu_mask(other).unwrap();
        let work = launch.work_directory().unwrap();
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("grep Cpus_allowed_list /proc/self/status");
        // SAFETY: this callback changes only the forked child's affinity
        // using a live, initialized mask and no allocation or shared state.
        unsafe {
            command.pre_exec(move || {
                if libc::sched_setaffinity(0, size_of::<libc::cpu_set_t>(), &prior_mask) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let exit = match Guest::start(command, work, &launch) {
            Ok(guest) => guest.wait().unwrap(),
            Err(LaunchError::HostCpuUnavailable(_)) => return,
            Err(error) => panic!("CPU pin launch failed: {error}"),
        };
        assert_eq!(exit.reason, ExitReason::Exited(0));
        let output = String::from_utf8(exit.console).unwrap();
        assert!(output.contains(&format!("Cpus_allowed_list:\t{cpu}")));
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn launch_refuses_when_its_admitted_cpu_becomes_disallowed() {
        let launch = Launch::new(std::env::temp_dir());
        let cpu = launch.admitted_processor().unwrap();
        // SAFETY: cpu_set_t is plain data and the kernel writes no more than
        // the supplied object size into this live set.
        let mut original: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        // SAFETY: original is writable for the supplied cpu_set_t size.
        assert_eq!(
            unsafe { libc::sched_getaffinity(0, size_of::<libc::cpu_set_t>(), &mut original) },
            0
        );
        let other = (0..libc::CPU_SETSIZE as usize).find(|candidate| {
            // SAFETY: candidate is below CPU_SETSIZE and original is initialized.
            *candidate != cpu && unsafe { libc::CPU_ISSET(*candidate, &original) }
        });
        let Some(other) = other else {
            return;
        };
        let narrowed = singleton_cpu_mask(other).unwrap();
        // SAFETY: narrowed is a live cpu_set_t of the supplied size and the
        // call changes only this test thread's affinity.
        assert_eq!(
            unsafe { libc::sched_setaffinity(0, size_of::<libc::cpu_set_t>(), &narrowed) },
            0
        );
        let identity = launch.host_identity();
        let result = Guest::start(
            Command::new("true"),
            launch.work_directory().unwrap(),
            &launch,
        );
        // SAFETY: original is the affinity mask captured from this test
        // thread before narrowing, and the call restores only that thread.
        assert_eq!(
            unsafe { libc::sched_setaffinity(0, size_of::<libc::cpu_set_t>(), &original) },
            0
        );
        assert!(identity.is_err());
        assert!(matches!(result, Err(LaunchError::HostCpuUnavailable(_))));
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn independent_workers_launch_on_distinct_admitted_cpus() {
        // SAFETY: cpu_set_t is plain data and the kernel writes within the
        // supplied live object.
        let mut allowed: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        // SAFETY: allowed is writable for the supplied cpu_set_t size.
        assert_eq!(
            unsafe { libc::sched_getaffinity(0, size_of::<libc::cpu_set_t>(), &mut allowed) },
            0
        );
        let cpus = (0..libc::CPU_SETSIZE as usize)
            .filter(|cpu| {
                // SAFETY: cpu is below CPU_SETSIZE and allowed is initialized.
                unsafe { libc::CPU_ISSET(*cpu, &allowed) }
            })
            .take(2)
            .collect::<Vec<_>>();
        if cpus.len() != 2 {
            return;
        }
        let workers = cpus
            .into_iter()
            .map(|cpu| {
                std::thread::spawn(move || {
                    let mask = singleton_cpu_mask(cpu).unwrap();
                    // SAFETY: mask is a live cpu_set_t of the supplied size
                    // and the call changes only this worker thread.
                    assert_eq!(
                        unsafe { libc::sched_setaffinity(0, size_of::<libc::cpu_set_t>(), &mask) },
                        0
                    );
                    let launch = Launch::new(std::env::temp_dir());
                    let host = launch.host_identity().unwrap();
                    let work = launch.work_directory().unwrap();
                    let mut command = Command::new("sh");
                    command
                        .arg("-c")
                        .arg("grep Cpus_allowed_list /proc/self/status");
                    let exit = Guest::start(command, work, &launch)
                        .unwrap()
                        .wait()
                        .unwrap();
                    assert_eq!(exit.reason, ExitReason::Exited(0));
                    assert!(
                        String::from_utf8(exit.console)
                            .unwrap()
                            .contains(&format!("Cpus_allowed_list:\t{cpu}"))
                    );
                    (cpu, host.processor)
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            let (requested, admitted) = worker.join().unwrap();
            assert_eq!(requested, admitted);
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn disjoint_worker_threads_launch_on_one_recorded_guest_cpu() {
        // SAFETY: cpu_set_t is plain data and the kernel writes within the
        // supplied live object.
        let mut allowed: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        // SAFETY: allowed is writable for the supplied cpu_set_t size.
        assert_eq!(
            unsafe { libc::sched_getaffinity(0, size_of::<libc::cpu_set_t>(), &mut allowed) },
            0
        );
        let cpus = (0..libc::CPU_SETSIZE as usize)
            .filter(|cpu| {
                // SAFETY: cpu is below CPU_SETSIZE and allowed is initialized.
                unsafe { libc::CPU_ISSET(*cpu, &allowed) }
            })
            .take(2)
            .collect::<Vec<_>>();
        if cpus.len() != 2 {
            return;
        }
        let canonical = cpus[0];
        let host = HostIdentity::for_processor(canonical).unwrap();
        let workers = cpus
            .into_iter()
            .map(|worker_cpu| {
                let host = host.clone();
                std::thread::spawn(move || {
                    let mask = singleton_cpu_mask(worker_cpu).unwrap();
                    // SAFETY: mask is a live cpu_set_t of the supplied size
                    // and the call changes only this worker thread.
                    assert_eq!(
                        unsafe { libc::sched_setaffinity(0, size_of::<libc::cpu_set_t>(), &mask) },
                        0
                    );
                    let mut launch = Launch::new(std::env::temp_dir());
                    launch.bind_expected_host(host.clone()).unwrap();
                    assert_eq!(launch.host_identity().unwrap(), host);
                    let work = launch.work_directory().unwrap();
                    let mut command = Command::new("sh");
                    command
                        .arg("-c")
                        .arg("grep Cpus_allowed_list /proc/self/status");
                    let exit = Guest::start(command, work, &launch)
                        .unwrap()
                        .wait()
                        .unwrap();
                    assert_eq!(exit.reason, ExitReason::Exited(0));
                    assert!(
                        String::from_utf8(exit.console)
                            .unwrap()
                            .contains(&format!("Cpus_allowed_list:\t{canonical}"))
                    );
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().unwrap();
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn cloned_launch_refuses_cross_thread_cpu_migration() {
        // SAFETY: cpu_set_t is plain data and the kernel writes within the
        // supplied live object.
        let mut allowed: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        // SAFETY: allowed is writable for the supplied cpu_set_t size.
        assert_eq!(
            unsafe { libc::sched_getaffinity(0, size_of::<libc::cpu_set_t>(), &mut allowed) },
            0
        );
        let cpus = (0..libc::CPU_SETSIZE as usize)
            .filter(|cpu| {
                // SAFETY: cpu is below CPU_SETSIZE and allowed is initialized.
                unsafe { libc::CPU_ISSET(*cpu, &allowed) }
            })
            .take(2)
            .collect::<Vec<_>>();
        if cpus.len() != 2 {
            return;
        }
        let launch = Launch::new(std::env::temp_dir());
        assert_eq!(launch.admitted_processor().unwrap(), cpus[0]);
        let moved = launch.clone();
        let other = cpus[1];
        let result = std::thread::spawn(move || {
            let mask = singleton_cpu_mask(other).unwrap();
            // SAFETY: mask is a live cpu_set_t of the supplied size and the
            // call changes only this worker thread.
            assert_eq!(
                unsafe { libc::sched_setaffinity(0, size_of::<libc::cpu_set_t>(), &mask) },
                0
            );
            let work = moved.work_directory().unwrap();
            matches!(
                Guest::start(Command::new("true"), work, &moved),
                Err(LaunchError::HostCpuUnavailable(_))
            )
        })
        .join()
        .unwrap();
        assert!(result);
        assert_eq!(launch.admitted_processor().unwrap(), cpus[0]);
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn denied_fixed_layout_clears_inherited_flags() {
        let mut baseline = Command::new("/bin/cat");
        baseline.arg("/proc/self/personality");
        deny_fixed_layout(&mut baseline, false);
        let output = match baseline.output() {
            Ok(output) => output,
            Err(error) if error.raw_os_error() == Some(libc::EPERM) => return,
            Err(error) => panic!("personality filter setup failed: {error}"),
        };
        assert!(output.status.success());
        let inherited =
            u32::from_str_radix(std::str::from_utf8(&output.stdout).unwrap().trim(), 16).unwrap();
        assert_ne!(inherited & libc::UNAME26 as u32, 0);
        assert_eq!(inherited & libc::ADDR_NO_RANDOMIZE as u32, 0);

        let mut command = Command::new("/bin/cat");
        command.arg("/proc/self/personality");
        deny_fixed_layout(&mut command, false);
        fixed_host_layout(&mut command);
        let output = command.output().unwrap();
        assert!(output.status.success());
        let flags =
            u32::from_str_radix(std::str::from_utf8(&output.stdout).unwrap().trim(), 16).unwrap();
        assert_eq!(flags, 0);
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn denied_fixed_and_zero_layout_refuses_launch() {
        let mut baseline = Command::new("/bin/cat");
        baseline.arg("/proc/self/personality");
        deny_fixed_layout(&mut baseline, true);
        let output = match baseline.output() {
            Ok(output) => output,
            Err(error) if error.raw_os_error() == Some(libc::EPERM) => return,
            Err(error) => panic!("personality filter setup failed: {error}"),
        };
        assert!(output.status.success());
        let inherited =
            u32::from_str_radix(std::str::from_utf8(&output.stdout).unwrap().trim(), 16).unwrap();
        assert_eq!(inherited, libc::UNAME26 as u32);

        let mut command = Command::new("/bin/cat");
        command.arg("/proc/self/personality");
        deny_fixed_layout(&mut command, true);
        fixed_host_layout(&mut command);
        assert_eq!(
            command.output().unwrap_err().raw_os_error(),
            Some(libc::EPERM)
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn a_pinned_kernel_normalizes_host_layout_before_exec() {
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
                schema: 4,
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
        // SAFETY: personality(2) changes only the calling test thread's task
        // state, which is restored before inspecting the child output.
        let output = unsafe {
            let inherited = libc::personality(u32::MAX as libc::c_ulong);
            assert!(inherited >= 0);
            if libc::personality((inherited as libc::c_ulong) | (libc::UNAME26 as libc::c_ulong))
                < 0
            {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::EPERM) {
                    return;
                }
                panic!("inherited personality setup failed: {error}");
            }
            let output = command.output();
            assert!(libc::personality(inherited as libc::c_ulong) >= 0);
            output.unwrap()
        };
        assert!(output.status.success());
        let flags =
            u32::from_str_radix(std::str::from_utf8(&output.stdout).unwrap().trim(), 16).unwrap();
        assert_ne!(flags & libc::ADDR_NO_RANDOMIZE as u32, 0);
        assert_eq!(flags & libc::UNAME26 as u32, 0);
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
    fn reset_drains_pending_console_pipe_bytes() {
        let (mut reader, mut writer) = io::pipe().unwrap();
        let flags = rustix::fs::fcntl_getfl(&reader).unwrap();
        rustix::fs::fcntl_setfl(&reader, flags | rustix::fs::OFlags::NONBLOCK).unwrap();
        let console = Mutex::new(ConsoleTail::new(64));
        let over_limit = AtomicBool::new(false);
        console.lock().unwrap().push(b"already collected");
        writer.write_all(b"pending pipe bytes").unwrap();

        drain_console(&mut reader, &mut [0; 4], &console, &over_limit, 64).unwrap();
        assert_eq!(console.lock().unwrap().bytes(), b"");
        assert_eq!(console.lock().unwrap().total(), 35);
        assert!(!over_limit.load(Ordering::Acquire));

        writer.write_all(b"new branch").unwrap();
        assert!(matches!(
            read_console(&mut reader, &mut [0; 64], &console, &over_limit, 64,),
            Ok(ConsoleRead::Bytes)
        ));
        assert_eq!(console.lock().unwrap().bytes(), b"new branch");
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn terminal_console_waits_for_the_reader_after_child_exit() {
        let parent = tempfile::tempdir().unwrap();
        let mut launch = launch(parent.path());
        launch.console_tail_bytes = 64;
        let release_path = parent.path().join("release");
        let script = format!(
            "while [ ! -e {} ]; do sleep 0.001; done; printf 'Kernel panic: terminal marker\\n'",
            release_path.display()
        );
        let mut guest = shell(&launch, &script);
        let console = Arc::clone(&guest.console);
        let held = console.lock().unwrap();
        std::fs::write(release_path, []).unwrap();
        let reason = loop {
            if let Some(reason) = guest.stopped().unwrap() {
                break reason;
            }
            std::thread::yield_now();
        };
        assert!(!held.contains(b"Kernel panic"));
        drop(held);
        let exit = guest.finish(reason).unwrap();
        assert_eq!(exit.reason, ExitReason::Exited(0));
        assert!(
            exit.console
                .windows(12)
                .any(|window| window == b"Kernel panic")
        );
        assert!(exit.leftovers.is_empty());
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn completed_guest_over_console_limit_reports_limit_after_reader_joins() {
        let parent = tempfile::tempdir().unwrap();
        let mut launch = launch(parent.path());
        launch.console_limit_bytes = 8;
        launch.console_tail_bytes = 64;
        let release_path = parent.path().join("release");
        let script = format!(
            "while [ ! -e {} ]; do sleep 0.001; done; printf abcdefghijklmnop; exit 0",
            release_path.display()
        );
        let mut guest = shell(&launch, &script);
        let console = Arc::clone(&guest.console);
        let held = console.lock().unwrap();
        std::fs::write(release_path, []).unwrap();
        let deadline = now() + Duration::from_secs(5);
        let reason = loop {
            if let Some(reason) = guest.stopped().unwrap() {
                break reason;
            }
            assert!(now() < deadline, "guest did not exit");
            std::thread::yield_now();
        };
        assert_eq!(reason, ExitReason::Exited(0));
        drop(held);
        let exit = guest.finish(reason).unwrap();
        assert_eq!(exit.console_bytes, 16);
        assert_eq!(exit.reason, ExitReason::ConsoleLimit);
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn completed_console_overflow_precedes_event_cut() {
        struct ResetProbe;
        impl Drop for ResetProbe {
            fn drop(&mut self) {
                TEST_EVENT_CUT_FINISH_PID.store(0, Ordering::Release);
            }
        }

        let parent = tempfile::tempdir().unwrap();
        let mut launch = launch(parent.path());
        launch.console_limit_bytes = 8;
        launch.console_tail_bytes = 64;
        launch.bridge = Some(Bridge {
            seed: 7,
            cut: Some(1),
        });
        let mut request = [0_u8; hypercall_proto::MAX_FRAME];
        let length = hypercall_proto::encode_request(
            hypercall_proto::ServiceId::Event,
            1,
            7,
            b"\0\0\0\0{}",
            &mut request,
        )
        .unwrap();
        let mut frame = 1_u64.to_le_bytes().to_vec();
        frame.extend_from_slice(&request[..length]);
        let packet = parent.path().join("event-packet");
        std::fs::write(&packet, frame).unwrap();
        let release = parent.path().join("release");
        let script = format!(
            "while [ ! -e {} ]; do sleep 0.001; done; printf abcdefghijklmnop; cat {} >&3; sleep 30",
            release.display(),
            packet.display()
        );
        let guest = shell(&launch, &script);
        let console = Arc::clone(&guest.console);
        let held = console.lock().unwrap();
        TEST_EVENT_CUT_FINISH_READY.store(false, Ordering::Release);
        TEST_EVENT_CUT_FINISH_PID.store(guest.pid, Ordering::Release);
        let _reset = ResetProbe;
        std::fs::write(release, []).unwrap();
        let waiter = std::thread::spawn(move || guest.wait().unwrap());
        let deadline = now() + Duration::from_secs(5);
        while !TEST_EVENT_CUT_FINISH_READY.load(Ordering::Acquire) {
            assert!(now() < deadline, "event cut did not reach cleanup");
            std::thread::yield_now();
        }
        drop(held);
        let exit = waiter.join().unwrap();
        assert_eq!(exit.console_bytes, 16);
        assert_eq!(exit.console, b"abcdefghijklmnop");
        assert_eq!(exit.events.len(), 1);
        assert!(exit.leftovers.is_empty());
        assert!(exit.bridge_failure.is_none());
        assert_eq!(exit.reason, ExitReason::ConsoleLimit);
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn completed_event_cut_overrides_process_exit_after_bridge_joins() {
        struct ReleaseCut;
        impl Drop for ReleaseCut {
            fn drop(&mut self) {
                bridge::TEST_HOLD_CUT.store(false, Ordering::Release);
            }
        }

        let parent = tempfile::tempdir().unwrap();
        let mut launch = launch(parent.path());
        launch.bridge = Some(Bridge {
            seed: bridge::TEST_DELAY_CUT_SEED,
            cut: Some(1),
        });
        let mut request = [0_u8; hypercall_proto::MAX_FRAME];
        let length = hypercall_proto::encode_request(
            hypercall_proto::ServiceId::Event,
            1,
            7,
            b"\0\0\0\0{}",
            &mut request,
        )
        .unwrap();
        let mut frame = 1_u64.to_le_bytes().to_vec();
        frame.extend_from_slice(&request[..length]);
        let packet = parent.path().join("event-packet");
        std::fs::write(&packet, frame).unwrap();
        bridge::TEST_CUT_READY.store(false, Ordering::Release);
        bridge::TEST_HOLD_CUT.store(true, Ordering::Release);
        let _release = ReleaseCut;
        let mut guest = shell(&launch, &format!("cat {} >&3", packet.display()));
        let deadline = now() + Duration::from_secs(5);
        let reason = loop {
            if bridge::TEST_CUT_READY.load(Ordering::Acquire)
                && let Some(reason) = guest.stopped().unwrap()
            {
                break reason;
            }
            assert!(now() < deadline, "guest did not exit after cut event");
            std::thread::yield_now();
        };
        assert_eq!(reason, ExitReason::Exited(0));
        bridge::TEST_HOLD_CUT.store(false, Ordering::Release);
        let exit = guest.finish(reason).unwrap();
        assert_eq!(exit.events.len(), 1);
        assert_eq!(exit.events[0].moment, 1);
        assert_eq!(exit.events[0].id, 0);
        assert_eq!(&*exit.events[0].data, b"{}");
        assert_eq!(exit.reason, ExitReason::EventCut);
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
    fn console_text_cannot_claim_host_clone_failure() {
        let parent = tempfile::tempdir().unwrap();
        let launch = launch(parent.path());
        let exit = shell(&launch, "echo Harmony UML host clone failed; exit 0")
            .wait()
            .unwrap();
        assert_eq!(exit.reason, ExitReason::Exited(0));
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
