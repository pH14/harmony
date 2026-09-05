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
    cell::RefCell, collections::BTreeMap, error::Error, mem::size_of, path::Path, sync::Arc,
};

use control_proto::{
    HostFault, Moment, Reply, Request, SnapId, StopConditions, StopMask, StopReason, class_bit,
};
use sha2::{Digest, Sha256};
use vmm_backend::{Backend, X86};
use vmm_core::{
    control::{ControlServer, VmmFactory, server_caps},
    vendor::x86::bringup::boot_linux_stock_virtual_time,
};

use crate::{
    faultlab::target::{
        FaultAction, FaultObservations, FaultStop, FaultlabSnapshot, action_deadline, action_delta,
        action_window, decode_sdk_events, reproducer,
    },
    target::ExitKind,
};

type Server = ControlServer<Box<dyn Backend<A = X86>>>;

/// Guest RAM. The workloads are real database servers, so the image needs more
/// than a toy guest.
const RAM: usize = 1024 * 1024 * 1024;
const SEED: u64 = 0x4661_756c_744c_6162;
/// Virtual-time bound on reaching the fault agent's `setup_complete`.
const SETUP_DEADLINE: Moment = Moment(120_000_000_000);
const CMDLINE_PREFIX: &str = "console=ttyS0 panic=-1 reboot=t tsc=reliable \
    no_timer_check lpj=4000000 random.trust_cpu=off nokaslr nosmp maxcpus=1 \
    nox2apic hpet=disable harmony_pvclock rdinit=";

#[derive(Debug)]
struct Config {
    key: [u8; 32],
    kernel: Vec<u8>,
    initramfs: Vec<u8>,
    cmdline: String,
}

struct Session {
    key: [u8; 32],
    server: Server,
    setup: SnapId,
    root_seal: u64,
    snapshots: BTreeMap<Vec<FaultAction>, SnapId>,
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
    pub fn new(kernel: &[u8], initramfs: &[u8], rdinit: &str) -> Result<Self, String> {
        let config = Arc::new(Config {
            key: image_key(kernel, initramfs, rdinit),
            kernel: kernel.to_vec(),
            initramfs: initramfs.to_vec(),
            cmdline: format!("{CMDLINE_PREFIX}{rdinit}"),
        });
        let (observation, root_seal) = with_session(&config, |session| {
            let observation = session.observe(FaultStop::Deadline)?;
            Ok((observation, session.root_seal))
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
        with_session(&self.config, |session| {
            let snap = session.ensure_prefix(&snapshot.actions)?;
            expect_unit(session.drive(&Request::Replay(snap))?, "replay")
        })?;
        self.actions.clone_from(&snapshot.actions);
        self.observation = snapshot.observation.clone();
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

fn image_key(kernel: &[u8], initramfs: &[u8], rdinit: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(kernel);
    digest.update(initramfs);
    digest.update(rdinit.as_bytes());
    digest.finalize().into()
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
        operation(
            slot.as_mut()
                .ok_or("Consonance session was not initialized")?,
        )
    })
}

impl Session {
    fn boot(config: &Config) -> Result<Self, String> {
        let cmdline = config.cmdline.clone();
        let boot = move |kernel: &[u8], initramfs: &[u8]| {
            let mut vmm = boot_linux_stock_virtual_time(kernel, initramfs, RAM, &cmdline, SEED)?;
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
        match drive(&mut server, &Request::Hello(server_caps()))? {
            Reply::Hello(caps) if caps == server_caps() => {}
            other => return Err(format!("fault-library hello returned {other:?}")),
        }
        // The fault agent seals at `setup_complete`, once every node is up and
        // the workload's readiness command has passed.
        let root_seal = match drive(
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
        let setup = snapshot(&mut server)?;
        let mut snapshots = BTreeMap::new();
        snapshots.insert(Vec::new(), setup);
        Ok(Self {
            key: config.key,
            server,
            setup,
            root_seal,
            snapshots,
        })
    }

    fn drive(&mut self, request: &Request) -> Result<Reply, String> {
        drive(&mut self.server, request)
    }

    /// Rebuild every uncached endpoint on the way to `actions`, returning the
    /// snapshot of the last one.
    fn ensure_prefix(&mut self, actions: &[FaultAction]) -> Result<SnapId, String> {
        if let Some(snap) = self.snapshots.get(actions).copied() {
            return Ok(snap);
        }
        let start = (0..actions.len())
            .rev()
            .find(|length| self.snapshots.contains_key(&actions[..*length]))
            .unwrap_or(0);
        let parent = self
            .snapshots
            .get(&actions[..start])
            .copied()
            .ok_or("the fault-library setup snapshot is missing")?;
        self.branch(parent, actions)?;
        let mut last = parent;
        for index in start..actions.len() {
            let observation = self.run_action(actions[index], index)?;
            if !observation.stop.is_continuable() {
                return Err(format!(
                    "a cached fault-library prefix stopped at {:?}",
                    observation.stop
                ));
            }
            last = snapshot(&mut self.server)?;
            self.snapshots.insert(actions[..=index].to_vec(), last);
        }
        Ok(last)
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
        if let Some(snap) = self.snapshots.get(&next).copied() {
            expect_unit(self.drive(&Request::Replay(snap))?, "cached replay")?;
            return self.observe(FaultStop::Deadline);
        }
        let parent = self.ensure_prefix(prefix)?;
        self.branch(parent, &next)?;
        let observation = self.run_action(action, prefix.len())?;
        if observation.stop.is_continuable() {
            let snap = snapshot(&mut self.server)?;
            self.snapshots.insert(next, snap);
        }
        Ok(observation)
    }

    /// Restore `parent` under the whole input's standing-fault list. Windows
    /// already behind the parent's seal are inert, so one branch carries the
    /// entire input.
    fn branch(&mut self, parent: SnapId, actions: &[FaultAction]) -> Result<(), String> {
        let env = reproducer(self.root_seal, actions);
        expect_unit(
            self.drive(&Request::Branch { snap: parent, env })?,
            "branch",
        )
    }

    fn run_action(
        &mut self,
        action: FaultAction,
        index: usize,
    ) -> Result<FaultObservations, String> {
        let window = action_window(self.root_seal, index);
        if let Some(perturb) = action_delta(action, window).perturb {
            expect_unit(
                self.drive(&Request::Perturb {
                    fault: HostFault(perturb.fault),
                    at: perturb.at,
                })?,
                "perturb",
            )?;
        }
        let stop = match self.drive(&Request::Run {
            until: StopConditions {
                deadline: Some(action_deadline(self.root_seal, index)),
                on: StopMask::NONE.arm(class_bit::ASSERTION),
            },
            resolve: None,
        })? {
            Reply::Stop(reason) => FaultStop::from_stop_reason(&reason),
            other => return Err(format!("fault-library run returned {other:?}")),
        };
        self.observe(stop)
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

fn drive(server: &mut Server, request: &Request) -> Result<Reply, String> {
    match server.handle(request) {
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

fn snapshot(server: &mut Server) -> Result<SnapId, String> {
    match drive(server, &Request::Snapshot)? {
        Reply::Snapshot {
            id, tainted: false, ..
        } => Ok(id),
        Reply::Snapshot { tainted: true, .. } => {
            Err("the fault-library timeline is tainted".to_owned())
        }
        other => Err(format!("snapshot returned {other:?}")),
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

/// Stable identity string for one fault-library image.
#[must_use]
pub fn identity(kernel: &[u8], initramfs: &[u8], rdinit: &str) -> String {
    format!(
        "faultlab-consonance-whole-vm-v1;kernel-sha256={:x};initramfs-sha256={:x};rdinit={rdinit};\
         action=standing-fault-delta-v1;snapshot=portable-prefix-to-vm-snapshot-v1",
        Sha256::digest(kernel),
        Sha256::digest(initramfs),
    )
}

/// Read a fault-library target from the two guest image paths.
///
/// # Errors
///
/// Returns an error when an image cannot be read or the guest cannot boot.
pub fn from_paths(kernel: &Path, initramfs: &Path, rdinit: &str) -> Result<FaultlabTarget, String> {
    let kernel = std::fs::read(kernel).map_err(|error| format!("read kernel: {error}"))?;
    let initramfs = std::fs::read(initramfs).map_err(|error| format!("read initramfs: {error}"))?;
    FaultlabTarget::new(&kernel, &initramfs, rdinit)
}
