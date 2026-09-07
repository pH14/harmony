// SPDX-License-Identifier: AGPL-3.0-or-later

//! Consonance whole-VM backend for the fault-library adapter.
//!
//! Each evaluator thread lazily owns one Consonance VM session running the
//! workload image under the guest fault agent. A portable action prefix maps to
//! a real whole-VM snapshot: the session branches the sealed setup point with
//! the prefix's standing-fault list, stages any host-plane perturbation, and
//! runs to each action's horizon deadline. The generic search coordinator never
//! learns a workload, a node name, or a hook.

use std::{
    cell::RefCell,
    collections::BTreeMap,
    error::Error,
    mem::size_of,
    path::Path,
    sync::{
        Arc, Once,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use control_proto::{
    ControlError, HostFault, Moment, RegsView, Reply, Request, SnapId, StopConditions, StopMask,
    StopReason, class_bit,
};
use sha2::{Digest, Sha256};
use vmm_backend::{Backend, X86};
pub use vmm_core::vendor::x86::bringup::BackendKind;
use vmm_core::{
    control::{ControlServer, RestoreMode, VmmFactory, server_caps},
    vendor::x86::bringup::{
        boot_linux_selected, boot_linux_stock_virtual_time,
        compose_patched_virtual_time_restore_target, compose_stock_virtual_time_restore_target,
    },
    vmm::Vmm,
};

use crate::{
    faultlab::target::{
        ActionWindows, FaultAction, FaultObservations, FaultStop, FaultlabSnapshot, action_delta,
        decode_sdk_events, reproducer,
    },
    target::ExitKind,
};

type Server = ControlServer<Box<dyn Backend<A = X86>>>;

/// Guest RAM when a campaign names none. The workloads are real database
/// servers, so the image needs more than a toy guest.
pub const DEFAULT_RAM_MIB: u32 = 1024;
const SEED: u64 = 0x4661_756c_744c_6162;
/// Virtual-time bound on reaching the fault agent's `setup_complete`.
const SETUP_DEADLINE: Moment = Moment(120_000_000_000);
/// Prefix snapshots one evaluator keeps resident besides the sealed setup
/// point. Each holds the pages its run dirtied, and a campaign creates one per
/// action, so an unbounded cache grows without limit across a long run; an
/// evicted prefix is rebuilt from its longest cached ancestor instead.
const SNAPSHOT_CACHE_LIMIT: usize = 96;

/// Guest time run past a horizon deadline when the server refuses to snapshot
/// the endpoint. The refusal is a point the virtual clock cannot seal, such as
/// an exit still in flight, so a short step forward finds a sealable one.
const SETTLE_STEP_NANOS: u64 = 100_000;

/// Snapshot attempts per endpoint before it counts as having no successor.
const SETTLE_ATTEMPTS: u32 = 16;
/// Wall-clock limit on one control request before its guest is abandoned.
const STALL_LIMIT: Duration = Duration::from_secs(60);
/// How often an abandoned guest's vCPU thread is interrupted again until the
/// request returns.
const STALL_NUDGE: Duration = Duration::from_millis(100);
const CMDLINE_PREFIX: &str = "console=ttyS0 panic=-1 reboot=t tsc=reliable \
    no_timer_check lpj=4000000 random.trust_cpu=off nokaslr nosmp maxcpus=1 \
    nox2apic hpet=disable rdinit=";

/// The guest kernel's request for the paravirtual clock page.
const PVCLOCK_WORD: &str = "harmony_pvclock";

/// How a campaign boots and drives one workload image.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FaultlabConfig {
    /// The workload init the guest kernel starts, its `rdinit=` word.
    pub rdinit: String,
    /// Extra `key=value` command-line words the workload scripts read as
    /// knobs, so a campaign can widen or narrow a bug's window without
    /// rebuilding the image.
    pub knobs: Vec<String>,
    /// Which KVM the guest runs under. Only the patched backend traps the
    /// timestamp counter, so only it makes two runs of one input identical.
    pub backend: BackendKind,
    /// Virtual nanoseconds one action runs for.
    pub horizon_nanos: u64,
    /// Guest RAM in MiB. Every worker holds its guest's touched pages plus
    /// the snapshots of them, so the size bounds how many workers fit a box.
    pub ram_mib: u32,
    /// Whether the guest kernel keeps time from the paravirtual clock page.
    /// The page is a plain memory read, so a kernel loop that waits for time
    /// to pass without leaving the guest never sees it move and never gives
    /// the host a chance to deliver the timer interrupt that would end the
    /// loop. Without the page every clock read traps to the host instead,
    /// which is slower but always makes progress.
    pub pvclock: bool,
}

impl FaultlabConfig {
    fn cmdline(&self) -> String {
        let mut cmdline = format!("{CMDLINE_PREFIX}{}", self.rdinit);
        if self.pvclock {
            cmdline.push(' ');
            cmdline.push_str(PVCLOCK_WORD);
        }
        for knob in &self.knobs {
            cmdline.push(' ');
            cmdline.push_str(knob);
        }
        cmdline
    }
}

fn backend_label(backend: BackendKind) -> &'static str {
    match backend {
        BackendKind::Stock => "stock",
        BackendKind::Patched => "patched",
    }
}

#[derive(Debug)]
struct Config {
    key: [u8; 32],
    kernel: Vec<u8>,
    initramfs: Vec<u8>,
    cmdline: String,
    backend: BackendKind,
    pvclock: bool,
    horizon_nanos: u64,
    /// Guest RAM in bytes.
    ram: usize,
}

struct Session {
    key: [u8; 32],
    server: Server,
    watchdog: Watchdog,
    setup: SnapId,
    windows: ActionWindows,
    /// Cached prefix endpoints with the use stamp that orders eviction.
    snapshots: BTreeMap<Vec<FaultAction>, (SnapId, u64)>,
    uses: u64,
}

thread_local! {
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
}

/// One workload-aware target handle; the live KVM session stays thread-local.
#[derive(Debug)]
pub struct FaultlabTarget {
    config: Arc<Config>,
    actions: Vec<FaultAction>,
    observation: FaultObservations,
    action_observations: Vec<FaultObservations>,
    failed: bool,
    horizons_clocked: u64,
    root_seal: u64,
}

impl FaultlabTarget {
    /// Boot or join the current evaluator thread's fault-library session.
    ///
    /// # Errors
    ///
    /// Returns an error when the guest cannot boot or never reaches the fault
    /// agent's setup point.
    pub fn new(kernel: &[u8], initramfs: &[u8], config: &FaultlabConfig) -> Result<Self, String> {
        let config = Arc::new(Config {
            key: Sha256::digest(identity(kernel, initramfs, config)).into(),
            kernel: kernel.to_vec(),
            initramfs: initramfs.to_vec(),
            cmdline: config.cmdline(),
            backend: config.backend,
            pvclock: config.pvclock,
            horizon_nanos: config.horizon_nanos,
            ram: usize::try_from(config.ram_mib)
                .ok()
                .and_then(|mib| mib.checked_mul(1024 * 1024))
                .ok_or("guest RAM does not fit this host")?,
        });
        let (observation, root_seal) = with_session(&config, |session| {
            let observation = session.observe(FaultStop::Deadline)?;
            Ok((observation, session.windows.root_seal))
        })?;
        Ok(Self {
            config,
            actions: Vec::new(),
            action_observations: vec![observation.clone()],
            observation,
            failed: false,
            horizons_clocked: 0,
            root_seal,
        })
    }

    /// The sealed setup `Moment` every action window is measured from.
    #[must_use]
    pub fn root_seal(&self) -> u64 {
        self.root_seal
    }

    /// The current endpoint's observations.
    #[must_use]
    pub fn observation(&self) -> &FaultObservations {
        &self.observation
    }

    /// Endpoint observations emitted by the most recent action.
    #[must_use]
    pub fn last_action_observations(&self) -> &[FaultObservations] {
        &self.action_observations
    }

    /// Whether the current endpoint found a bug.
    #[must_use]
    pub fn found_bug(&self) -> bool {
        self.observation.is_bug()
    }

    /// Horizons this handle has run.
    #[must_use]
    pub fn horizons_clocked(&self) -> u64 {
        self.horizons_clocked
    }

    /// Everything the guest has written to its serial console so far.
    #[must_use]
    pub fn console(&self) -> String {
        SESSION.with(|cell| {
            cell.borrow_mut()
                .as_mut()
                .map_or_else(String::new, |session| {
                    console_text(&mut session.server, usize::MAX)
                })
        })
    }

    /// The action list that reaches the current endpoint.
    #[must_use]
    pub fn actions(&self) -> &[FaultAction] {
        &self.actions
    }

    /// Restore the sealed setup point.
    pub fn reset(&mut self) {
        let result = with_session(&self.config, |session| {
            expect_unit(
                session.drive(&Request::Replay(session.setup))?,
                "setup replay",
            )?;
            session.observe(FaultStop::Deadline)
        });
        match result {
            Ok(observation) => {
                self.actions.clear();
                self.observation = observation.clone();
                self.action_observations = vec![observation];
                self.failed = false;
            }
            Err(_) => self.failed = true,
        }
    }

    /// Restore one portable prefix through this thread's snapshot cache.
    ///
    /// # Errors
    ///
    /// Returns an error when the prefix cannot be rebuilt.
    pub fn restore(&mut self, snapshot: &FaultlabSnapshot) -> Result<(), Box<dyn Error>> {
        let rebuilt = with_session(&self.config, |session| {
            match session.ensure_prefix(&snapshot.actions)? {
                Ok(snap) => {
                    expect_unit(session.drive(&Request::Replay(snap))?, "replay")?;
                    Ok(None)
                }
                Err(observation) => Ok(Some(observation)),
            }
        })?;
        self.actions.clone_from(&snapshot.actions);
        self.observation = match rebuilt {
            None => snapshot.observation.clone(),
            // The prefix ended at its horizon when first run and somewhere
            // else now, so the two runs of one input differed. The endpoint
            // is recorded as having no successor rather than as a bug or a
            // dead worker, and the mismatch goes to the campaign log.
            Some(observation) => {
                eprintln!(
                    "fault-library prefix {:?} first stopped at {:?} and stopped at {:?} when rebuilt",
                    snapshot.actions, snapshot.observation.stop, observation.stop
                );
                FaultObservations {
                    stop: FaultStop::Unexpected,
                    ..observation
                }
            }
        };
        self.action_observations = vec![self.observation.clone()];
        self.failed = snapshot.failed;
        Ok(())
    }

    /// Capture a portable reference to the current whole-VM endpoint.
    #[must_use]
    pub fn snapshot(&self) -> Option<FaultlabSnapshot> {
        (!self.failed && self.observation.stop.is_continuable()).then(|| FaultlabSnapshot {
            actions: self.actions.clone(),
            observation: self.observation.clone(),
            failed: false,
        })
    }

    /// Apply one action: stage its environment delta, run to the horizon
    /// deadline, and observe the endpoint.
    pub fn apply(&mut self, action: FaultAction) {
        self.action_observations.clear();
        if self.failed || !self.observation.stop.is_continuable() {
            return;
        }
        let result = with_session(&self.config, |session| {
            session.advance(&self.actions, action)
        });
        match result {
            Ok(observation) => {
                self.actions.push(action);
                self.horizons_clocked = self.horizons_clocked.saturating_add(1);
                self.observation = observation.clone();
                self.action_observations.push(observation);
            }
            Err(_) => self.failed = true,
        }
    }

    /// Generic target exit classification.
    #[must_use]
    pub fn exit_kind(&self) -> ExitKind {
        if self.failed {
            ExitKind::Crash
        } else {
            self.observation.exit_kind()
        }
    }
}

fn with_session<T>(
    config: &Arc<Config>,
    operation: impl FnOnce(&mut Session) -> Result<T, String>,
) -> Result<T, String> {
    SESSION.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot
            .as_ref()
            .is_none_or(|session| session.key != config.key)
        {
            *slot = Some(Session::boot(config)?);
        }
        let session = slot
            .as_mut()
            .ok_or("Consonance session was not initialized")?;
        let result = operation(session);
        // A fatal server error or a canceled run leaves a VM that must not be
        // resumed, so the next operation on this thread boots a fresh one.
        if session.server.vmm().is_none() || session.watchdog.fired() {
            *slot = None;
        }
        result
    })
}

enum Command {
    Arm(Option<Arc<AtomicBool>>),
    Disarm,
}

/// Abandons a guest whose vCPU thread stops returning to the host.
///
/// The VMM injects the virtual-time timer only at exits, so a guest spinning
/// on a frozen clock never exits and never reaches its deadline. Past the
/// wall-clock limit the watchdog sets the backend's cancellation latch and
/// interrupts the vCPU thread with a signal, which makes the pending run
/// return an error. Every session runs its guest on the thread that created
/// the watchdog.
struct Watchdog {
    commands: Sender<Command>,
    fired: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Watchdog {
    fn start() -> Self {
        install_wakeup_handler();
        let (commands, receiver) = mpsc::channel();
        let fired = Arc::new(AtomicBool::new(false));
        let watched = fired.clone();
        // SAFETY: `pthread_self` has no preconditions.
        let vcpu = unsafe { libc::pthread_self() };
        let thread = std::thread::spawn(move || watch(&receiver, vcpu, &watched));
        Self {
            commands,
            fired,
            thread: Some(thread),
        }
    }

    fn arm(&self, flag: Option<Arc<AtomicBool>>) {
        let _ = self.commands.send(Command::Arm(flag));
    }

    fn disarm(&self) {
        let _ = self.commands.send(Command::Disarm);
    }

    fn fired(&self) -> bool {
        self.fired.load(Ordering::Acquire)
    }
}

impl Drop for Watchdog {
    // The watcher must stop signalling before the watched thread can exit.
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Disarm);
        let (sender, _) = mpsc::channel();
        drop(std::mem::replace(&mut self.commands, sender));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

// The watchdog measures host time on purpose: a guest that stalls advances
// no virtual time, so only the wall clock can notice it.
#[allow(clippy::disallowed_methods)]
fn watch(commands: &Receiver<Command>, vcpu: libc::pthread_t, fired: &AtomicBool) {
    let mut armed: Option<(Arc<AtomicBool>, Instant)> = None;
    loop {
        let command = match &armed {
            None => commands.recv().map_err(|_| RecvTimeoutError::Disconnected),
            Some((flag, since)) if since.elapsed() >= STALL_LIMIT => {
                fired.store(true, Ordering::Release);
                flag.store(true, Ordering::Release);
                // SAFETY: `vcpu` is the thread that owns this watchdog, and
                // that thread joins this one before it can exit, so the
                // handle is live. The signal has a no-op handler installed.
                unsafe { libc::pthread_kill(vcpu, libc::SIGUSR1) };
                commands.recv_timeout(STALL_NUDGE)
            }
            Some((_, since)) => commands.recv_timeout(STALL_LIMIT.saturating_sub(since.elapsed())),
        };
        match command {
            Ok(Command::Arm(Some(flag))) => armed = Some((flag, Instant::now())),
            Ok(Command::Arm(None) | Command::Disarm) => armed = None,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

extern "C" fn wakeup(_signal: libc::c_int) {}

/// A handled signal makes `KVM_RUN` return `EINTR`; an ignored one does not.
fn install_wakeup_handler() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        // SAFETY: an all-zero `sigaction` is a valid value, and the handler
        // is an `extern "C"` function that touches no shared state.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = wakeup as extern "C" fn(libc::c_int) as usize;
            libc::sigemptyset(&raw mut action.sa_mask);
            libc::sigaction(libc::SIGUSR1, &raw const action, std::ptr::null_mut());
        }
    });
}

impl Session {
    fn boot(config: &Config) -> Result<Self, String> {
        let cmdline = config.cmdline.clone();
        let backend = config.backend;
        let pvclock = config.pvclock;
        let ram = config.ram;
        let boot = move |kernel: &[u8], initramfs: &[u8]| {
            let mut vmm = match backend {
                BackendKind::Stock => {
                    boot_linux_stock_virtual_time(kernel, initramfs, ram, &cmdline, SEED)?
                }
                BackendKind::Patched => {
                    let mut vmm = boot_linux_selected(
                        BackendKind::Patched,
                        kernel,
                        initramfs,
                        ram,
                        &cmdline,
                        SEED,
                    )?;
                    // The stock composition offers the clock page itself; the
                    // patched one leaves that to its caller, and the guest's
                    // `harmony_pvclock` token expects the page to be there.
                    if pvclock {
                        vmm.enable_pvclock();
                    }
                    vmm
                }
            };
            // A campaign never encodes the virtual-time trace, so the sparse
            // checkpoint hash (a SHA-256 over all guest RAM every 256 events)
            // would only slow every execution down.
            vmm.defer_virtual_time_checkpoint_hashes()?;
            vmm.wire_snapshot_hashing();
            Ok(vmm)
        };
        let live = boot(&config.kernel, &config.initramfs)
            .map_err(|error| format!("fault-library boot compose failed: {error:?}"))?;
        let factory_kernel = config.kernel.clone();
        let factory_initramfs = config.initramfs.clone();
        let factory: VmmFactory<Box<dyn Backend<A = X86>>> =
            Box::new(move || boot(&factory_kernel, &factory_initramfs));
        let mut server = ControlServer::new(live, factory);
        // A restore that copies the whole image into a fresh VM faults every
        // page of it, and with many workers in one process those faults
        // serialize on the process's memory-map lock. A VM composed around
        // the snapshot mapping faults only the pages the run touches.
        server.set_remap_factory(Box::new(move |mapping| {
            let mut vmm = match backend {
                BackendKind::Stock => compose_stock_virtual_time_restore_target(mapping, SEED)?,
                BackendKind::Patched => {
                    let mut vmm = compose_patched_virtual_time_restore_target(mapping, SEED, true)?;
                    if pvclock {
                        vmm.enable_pvclock();
                    }
                    vmm
                }
            };
            vmm.defer_virtual_time_checkpoint_hashes()?;
            vmm.wire_snapshot_hashing();
            Ok(vmm)
        }));
        // Most restores patch the live VM: only the pages that differ from the
        // target image are written, and no VM is torn down or composed. A
        // restore the live VM cannot take falls back to the remap factory.
        server.set_restore_mode(RestoreMode::InPlace);
        let watchdog = Watchdog::start();
        match drive(&watchdog, &mut server, &Request::Hello(server_caps()))? {
            Reply::Hello(caps) if caps == server_caps() => {}
            other => return Err(format!("fault-library hello returned {other:?}")),
        }
        // The fault agent seals at `setup_complete`, once every node is up and
        // the workload's readiness command has passed.
        let root_seal = match drive(
            &watchdog,
            &mut server,
            &Request::Run {
                until: StopConditions {
                    deadline: Some(SETUP_DEADLINE),
                    on: StopMask::NONE.arm(class_bit::SNAPSHOT_POINT),
                },
                resolve: None,
            },
        )? {
            Reply::Stop(StopReason::SnapshotPoint { vtime }) => vtime.0,
            other => {
                return Err(format!(
                    "the fault agent never reached setup_complete: {other:?}"
                ));
            }
        };
        let setup =
            snapshot(&mut server)?.ok_or("the fault-library setup point is not snapshottable")?;
        let mut snapshots = BTreeMap::new();
        snapshots.insert(Vec::new(), (setup, 0));
        Ok(Self {
            key: config.key,
            server,
            watchdog,
            setup,
            windows: ActionWindows {
                root_seal,
                horizon_nanos: config.horizon_nanos,
            },
            snapshots,
            uses: 0,
        })
    }

    fn drive(&mut self, request: &Request) -> Result<Reply, String> {
        drive(&self.watchdog, &mut self.server, request)
    }

    /// The cached snapshot of `actions`, if any, marked as just used.
    fn cached(&mut self, actions: &[FaultAction]) -> Option<SnapId> {
        self.uses = self.uses.saturating_add(1);
        let uses = self.uses;
        self.snapshots.get_mut(actions).map(|(snap, stamp)| {
            *stamp = uses;
            *snap
        })
    }

    /// Cache the snapshot of `actions`, evicting the least recently used
    /// prefix once the cache is full. The setup point is never evicted.
    fn remember(&mut self, actions: Vec<FaultAction>, snap: SnapId) -> Result<(), String> {
        self.uses = self.uses.saturating_add(1);
        self.snapshots.insert(actions, (snap, self.uses));
        while self.snapshots.len() > SNAPSHOT_CACHE_LIMIT.saturating_add(1) {
            let Some(victim) = self
                .snapshots
                .iter()
                .filter(|(prefix, _)| !prefix.is_empty())
                .min_by_key(|(_, (_, stamp))| *stamp)
                .map(|(prefix, _)| prefix.clone())
            else {
                break;
            };
            if let Some((snap, _)) = self.snapshots.remove(&victim) {
                expect_unit(self.drive(&Request::Drop(snap))?, "drop")?;
            }
        }
        Ok(())
    }

    /// Rebuild every uncached endpoint on the way to `actions`, returning the
    /// snapshot of the last one, or the observation of the first endpoint
    /// that no longer stops at its horizon.
    fn ensure_prefix(
        &mut self,
        actions: &[FaultAction],
    ) -> Result<Result<SnapId, FaultObservations>, String> {
        if let Some(snap) = self.cached(actions) {
            return Ok(Ok(snap));
        }
        let start = (0..actions.len())
            .rev()
            .find(|length| self.snapshots.contains_key(&actions[..*length]))
            .unwrap_or(0);
        let mut last = self
            .cached(&actions[..start])
            .ok_or("the fault-library setup snapshot is missing")?;
        for index in start..actions.len() {
            // Each endpoint is reached the way its first run reached it:
            // restored from its parent's snapshot under the standing-fault
            // list of that shorter input. Running the actions back to back
            // under the whole input's list gives the guest agent a longer
            // list to reconcile, and that shifts the guest's timing enough
            // for a rebuilt endpoint to differ from the one first observed.
            self.branch(last, &actions[..=index])?;
            let (observation, snap) = self.run_action(actions[index], index)?;
            let Some(snap) = snap else {
                return Ok(Err(observation));
            };
            last = snap;
            self.remember(actions[..=index].to_vec(), last)?;
        }
        Ok(Ok(last))
    }

    /// Run one more action past `prefix` and observe its endpoint. A terminal
    /// endpoint is observed but never cached: it has no successor.
    fn advance(
        &mut self,
        prefix: &[FaultAction],
        action: FaultAction,
    ) -> Result<FaultObservations, String> {
        let mut next = prefix.to_vec();
        next.push(action);
        if let Some(snap) = self.cached(&next) {
            expect_unit(self.drive(&Request::Replay(snap))?, "cached replay")?;
            return self.observe(FaultStop::Deadline);
        }
        let parent = match self.ensure_prefix(prefix)? {
            Ok(parent) => parent,
            Err(observation) => {
                eprintln!(
                    "fault-library prefix {prefix:?} stopped at {:?} when rebuilt",
                    observation.stop
                );
                return Ok(FaultObservations {
                    stop: FaultStop::Unexpected,
                    ..observation
                });
            }
        };
        self.branch(parent, &next)?;
        let (observation, snap) = self.run_action(action, prefix.len())?;
        if let Some(snap) = snap {
            self.remember(next, snap)?;
        }
        Ok(observation)
    }

    /// Restore `parent` under the whole input's standing-fault list. Windows
    /// already behind the parent's seal are inert, so one branch carries the
    /// entire input.
    fn branch(&mut self, parent: SnapId, actions: &[FaultAction]) -> Result<(), String> {
        let env = reproducer(self.windows, actions);
        expect_unit(
            self.drive(&Request::Branch { snap: parent, env })?,
            "branch",
        )
    }

    /// Run `action` to its horizon and seal the endpoint. An endpoint the
    /// server cannot seal is run a little further and retried; one that never
    /// seals is observed with no snapshot, so the search never branches from
    /// it.
    fn run_action(
        &mut self,
        action: FaultAction,
        index: usize,
    ) -> Result<(FaultObservations, Option<SnapId>), String> {
        let window = self.windows.window(index);
        if let Some(perturb) = action_delta(action, window).perturb {
            expect_unit(
                self.drive(&Request::Perturb {
                    fault: HostFault(perturb.fault),
                    at: perturb.at,
                })?,
                "perturb",
            )?;
        }
        let mut deadline = self.windows.deadline(index);
        let mut stop = self.run_until(deadline)?;
        let mut snap = None;
        for _ in 0..SETTLE_ATTEMPTS {
            if !stop.is_continuable() {
                break;
            }
            snap = snapshot(&mut self.server)?;
            if snap.is_some() {
                break;
            }
            deadline = Moment(deadline.0.saturating_add(SETTLE_STEP_NANOS));
            stop = self.run_until(deadline)?;
        }
        if snap.is_none() && stop.is_continuable() {
            stop = FaultStop::Unexpected;
        }
        // The server keeps every finished run's virtual-time trace until it is
        // taken. A campaign writes no trace file, and one 250 ms run leaves
        // about half a megabyte behind, so the buffer is drained here.
        drop(self.server.take_session_virtual_time_trace());
        Ok((self.observe(stop)?, snap))
    }

    fn run_until(&mut self, deadline: Moment) -> Result<FaultStop, String> {
        match self.drive(&Request::Run {
            until: StopConditions {
                deadline: Some(deadline),
                on: StopMask::NONE.arm(class_bit::ASSERTION),
            },
            resolve: None,
        })? {
            Reply::Stop(reason) => {
                if let StopReason::Crash { vtime, info } = &reason {
                    eprintln!(
                        "fault-library guest crashed at {vtime:?}: {:?} {}",
                        info.kind,
                        String::from_utf8_lossy(&info.detail)
                    );
                }
                Ok(FaultStop::from_stop_reason(&reason))
            }
            other => Err(format!("fault-library run returned {other:?}")),
        }
    }

    fn observe(&mut self, stop: FaultStop) -> Result<FaultObservations, String> {
        let mut events = Vec::new();
        let mut offset = 0_u32;
        loop {
            let page = match self.drive(&Request::SdkEvents { offset })? {
                Reply::SdkEvents(page) => page,
                other => return Err(format!("SDK event fetch returned {other:?}")),
            };
            if page.is_empty() {
                break;
            }
            offset = offset
                .checked_add(u32::try_from(page.len()).map_err(|_| "SDK event page is too large")?)
                .ok_or("SDK event offset overflow")?;
            events.extend(page);
        }
        let moment = events.last().map_or(0, |(moment, _, _)| *moment);
        let capture = decode_sdk_events(&events)?;
        Ok(FaultObservations::new(moment, &capture, stop))
    }
}

/// The last bytes an abandoned guest wrote to its serial console: the
/// workload's own account of what it was doing when it stopped exiting.
fn console_tail(server: &mut Server) -> String {
    console_text(server, 1500)
}

/// The last `tail` bytes of the guest's serial console.
fn console_text(server: &mut Server, tail: usize) -> String {
    let mut bytes = Vec::new();
    let mut offset = 0u32;
    while let Ok(Ok(Reply::Console { chunk, .. })) = server.handle(&Request::Console { offset }) {
        if chunk.is_empty() {
            break;
        }
        offset = offset.saturating_add(u32::try_from(chunk.len()).unwrap_or(u32::MAX));
        bytes.extend(chunk);
    }
    let start = bytes.len().saturating_sub(tail);
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

/// The words on an abandoned guest's kernel stack that point into kernel
/// text, as `offset:value` pairs from the stack pointer up to the top of
/// the 16 KiB stack. Read against the kernel's symbol map they name the
/// call chain a spin sits in. The stack is in the vmalloc range, so each
/// page is found through the guest's own page tables from `cr3`; a page
/// that cannot be resolved ends the dump.
fn kernel_stack_words(server: &mut Server, view: &RegsView) -> String {
    const STACK_MASK: u64 = 0x3fff;
    const TEXT: std::ops::Range<u64> = 0xffff_ffff_8000_0000..0xffff_ffff_c000_0000;
    let rsp = view.gpr[7];
    if !TEXT.contains(&view.rip) && rsp < 0xffff_8000_0000_0000 {
        return "(user mode)".to_string();
    }
    let read = |server: &mut Server, gpa: u64, len: u32| -> Option<Vec<u8>> {
        match server.handle(&Request::Read { gpa, len }) {
            Ok(Ok(Reply::Bytes(bytes))) => Some(bytes),
            _ => None,
        }
    };
    let translate = |server: &mut Server, va: u64| -> Option<u64> {
        let mut table = view.cr3 & 0x000f_ffff_ffff_f000;
        for level in (0..4).rev() {
            let index = (va >> (12 + 9 * level)) & 0x1ff;
            let bytes = read(server, table + index * 8, 8)?;
            let entry = u64::from_le_bytes(bytes.try_into().ok()?);
            if entry & 1 == 0 {
                return None;
            }
            let large = level > 0 && entry & (1 << 7) != 0;
            if level == 0 || large {
                let shift = 12 + 9 * level;
                let frame = entry & 0x000f_ffff_ffff_f000 & !((1u64 << shift) - 1);
                return Some(frame + (va & ((1u64 << shift) - 1)));
            }
            table = entry & 0x000f_ffff_ffff_f000;
        }
        None
    };
    let mut out = String::new();
    let mut va = rsp & !7;
    let top = (rsp | STACK_MASK) + 1;
    while va < top {
        let page_end = (va | 0xfff) + 1;
        let Some(gpa) = translate(server, va) else {
            out.push_str(" (unmapped)");
            break;
        };
        let len = (page_end.min(top) - va) as u32;
        let Some(bytes) = read(server, gpa, len) else {
            out.push_str(" (unreadable)");
            break;
        };
        for (i, word) in bytes.chunks_exact(8).enumerate() {
            let value = u64::from_le_bytes(word.try_into().expect("eight bytes"));
            if TEXT.contains(&value) {
                out.push_str(&format!(" +{:x}:{value:x}", va + i as u64 * 8 - rsp));
            }
        }
        va = page_end;
    }
    out
}

fn drive(watchdog: &Watchdog, server: &mut Server, request: &Request) -> Result<Reply, String> {
    watchdog.arm(server.vmm().and_then(Vmm::cancellation_flag));
    let reply = server.handle(request);
    watchdog.disarm();
    if watchdog.fired() {
        eprintln!("fault-library guest abandoned: {request:?} ran for more than {STALL_LIMIT:?}");
        // The register view says whether the guest was spinning in user or
        // kernel mode and with interrupts on, which is what a stall diagnosis
        // needs and what nothing else records.
        if let Ok(Ok(Reply::Regs(view))) = server.handle(&Request::Regs) {
            eprintln!("abandoned guest registers: {view:?}");
            eprintln!(
                "abandoned guest stack: {}",
                kernel_stack_words(server, &view)
            );
        }
        eprintln!("abandoned guest console tail:\n{}", console_tail(server));
        return Err(format!("{request:?} ran for more than {STALL_LIMIT:?}"));
    }
    match reply {
        Ok(Ok(reply)) => Ok(reply),
        Ok(Err(error)) => Err(format!("{request:?} returned {error:?}")),
        Err(error) => Err(format!("{request:?} ended the session: {error:?}")),
    }
}

fn expect_unit(reply: Reply, operation: &str) -> Result<(), String> {
    match reply {
        Reply::Unit => Ok(()),
        other => Err(format!("{operation} returned {other:?}")),
    }
}

/// Seal the current point, or `None` when the server cannot seal it yet.
fn snapshot(server: &mut Server) -> Result<Option<SnapId>, String> {
    match server.handle(&Request::Snapshot) {
        Ok(Ok(Reply::Snapshot {
            id, tainted: false, ..
        })) => Ok(Some(id)),
        Ok(Ok(Reply::Snapshot { tainted: true, .. })) => {
            Err("the fault-library timeline is tainted".to_owned())
        }
        Ok(Ok(other)) => Err(format!("snapshot returned {other:?}")),
        Ok(Err(ControlError::NotQuiescent)) => Ok(None),
        Ok(Err(error)) => Err(format!("snapshot returned {error:?}")),
        Err(error) => Err(format!("snapshot ended the session: {error:?}")),
    }
}

/// Deterministic memory charge for one resident snapshot.
#[must_use]
pub fn snapshot_memory_charge(snapshot: &FaultlabSnapshot) -> usize {
    size_of::<FaultlabSnapshot>()
        .saturating_add(
            snapshot
                .actions
                .len()
                .saturating_mul(size_of::<FaultAction>()),
        )
        .saturating_add(
            snapshot
                .observation
                .sometimes
                .len()
                .saturating_mul(size_of::<u32>()),
        )
}

/// Stable identity string for one fault-library image and the way it boots.
#[must_use]
pub fn identity(kernel: &[u8], initramfs: &[u8], config: &FaultlabConfig) -> String {
    format!(
        "faultlab-consonance-whole-vm-v1;kernel-sha256={:x};initramfs-sha256={:x};rdinit={};\
         knobs={};backend={};pvclock={};ram-mib={};action=standing-fault-delta-v1;\
         snapshot=portable-prefix-to-vm-snapshot-v1",
        Sha256::digest(kernel),
        Sha256::digest(initramfs),
        config.rdinit,
        config.knobs.join(","),
        backend_label(config.backend),
        config.pvclock,
        config.ram_mib,
    )
}

/// Read a fault-library target from the two guest image paths.
///
/// # Errors
///
/// Returns an error when an image cannot be read or the guest cannot boot.
pub fn from_paths(
    kernel: &Path,
    initramfs: &Path,
    config: &FaultlabConfig,
) -> Result<FaultlabTarget, String> {
    let kernel = std::fs::read(kernel).map_err(|error| format!("read kernel: {error}"))?;
    let initramfs = std::fs::read(initramfs).map_err(|error| format!("read initramfs: {error}"))?;
    FaultlabTarget::new(&kernel, &initramfs, config)
}
