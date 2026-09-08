// SPDX-License-Identifier: AGPL-3.0-or-later

//! Consonance whole-VM backend for the fault package.
//!
//! Each evaluator thread lazily owns one Consonance client session running the
//! workload image under the in-guest fault agent. A portable action prefix maps
//! to a real whole-VM snapshot: the session branches the sealed setup point
//! with the prefix's standing-fault window list, stages any host-plane
//! perturbation, and runs to each action's horizon deadline. The generic search
//! coordinator never learns a workload, a node name, or a hook.
//!
//! The guest reaches its faults through the package's own service handler. Its
//! configuration bytes are the encoded window list of the whole input; every
//! poll is answered with the current moment and the windows containing it, so
//! the handler holds no dynamic state and a branch is fully described by its
//! configuration.

use std::{
    cell::RefCell, collections::BTreeMap, error::Error, path::Path, sync::Arc, time::Duration,
};

use consonance_client::session::{PortableSnapshot, Session, SessionConfig, identity_with_config};
use control_proto::{Moment, SnapId, StopReason};
use environment::{
    channel::{Answer as ChannelAnswer, ChannelError, Question, ServiceHandler, ServiceResponse},
    input_spec::{ServiceConfig, ServiceFactory},
};
use fault_policy::{STANDING_NAMESPACE, StandingWindow, encode_standing, encode_windows};
use searcher::target::ExitKind;
use sha2::{Digest, Sha256};

use crate::target::{
    ActionWindows, FaultAction, FaultObservations, FaultSnapshot, FaultStop, action_delta,
    decode_sdk_events, standing_windows,
};

/// Guest RAM when a campaign names none. The workloads are real database
/// servers, so the image needs more than a toy guest.
pub const DEFAULT_RAM_MIB: u32 = 1024;
/// Handler identity the branch configuration is recorded under.
pub const SERVICE_IDENTITY: &[u8] = b"faults-standing-v1";
const SEED: u64 = 0x4661_756c_744c_6162;
/// Virtual-time bound on reaching the fault agent's `setup_complete`.
const SETUP_BUDGET: u64 = 120_000_000_000;
/// Wall-clock limit on one guest run. A guest spinning on a frozen clock never
/// exits and never reaches its deadline, so only host time can notice it.
const WALL_LIMIT: Duration = Duration::from_secs(60);
/// Prefix snapshots one evaluator keeps resident besides the sealed setup
/// point. Each holds the pages its run dirtied, and a campaign creates one per
/// action, so an unbounded cache grows without limit across a long run; an
/// evicted prefix is rebuilt from its longest cached ancestor instead.
const SNAPSHOT_CACHE_LIMIT: usize = 96;
/// Guest time run past a horizon deadline when the session refuses to seal the
/// endpoint. The refusal is a point the virtual clock cannot seal, such as an
/// exit still in flight, so a short step forward finds a sealable one.
const SETTLE_STEP_NANOS: u64 = 100_000;
/// Seal attempts per endpoint before it counts as having no successor.
const SETTLE_ATTEMPTS: u32 = 16;
/// Serial console bytes kept when a guest is abandoned: the workload's own
/// account of what it was doing when it stopped exiting.
const CONSOLE_TAIL: usize = 1_500;

/// The guest command line, which boots the package's own init. The init the
/// image preparation writes chroots into the workload rootfs and execs the
/// fault agent.
#[cfg(target_arch = "x86_64")]
const CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t tsc=reliable \
    no_timer_check lpj=4000000 random.trust_cpu=off nokaslr nosmp maxcpus=1 \
    nox2apic hpet=disable harmony_pvclock rdinit=/init";
#[cfg(target_arch = "aarch64")]
const CMDLINE: &str = "console=ttyAMA0 earlycon=pl011,0x09000000 nohlt rdinit=/init";

/// How a campaign boots and drives one workload image.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FaultConfig {
    /// Extra `key=value` command-line words the workload scripts read as
    /// knobs, so a campaign can widen or narrow a bug's window without
    /// rebuilding the image.
    pub knobs: Vec<String>,
    /// Virtual nanoseconds one action runs for.
    pub horizon_nanos: u64,
    /// Guest RAM in MiB. Every worker holds its guest's touched pages plus
    /// the snapshots of them, so the size bounds how many workers fit a box.
    pub ram_mib: u32,
}

impl FaultConfig {
    /// The guest command line this configuration boots with.
    #[must_use]
    pub fn cmdline(&self) -> String {
        let mut cmdline = CMDLINE.to_owned();
        for knob in &self.knobs {
            cmdline.push(' ');
            cmdline.push_str(knob);
        }
        cmdline
    }

    /// The neutral session settings this configuration launches with.
    #[must_use]
    pub fn session_config(&self) -> SessionConfig {
        let ram = usize::try_from(self.ram_mib)
            .unwrap_or(usize::MAX / (1024 * 1024))
            .saturating_mul(1024 * 1024);
        SessionConfig::new(ram, SEED, SETUP_BUDGET, self.cmdline())
            .with_identity_tag(IDENTITY_TAG)
            .with_wall_limit(WALL_LIMIT)
    }
}

/// Portable identity domain of this package's sessions.
const IDENTITY_TAG: &str = "faults-consonance-execution-v1";

/// The package's standing-fault service. Its configuration is the whole
/// input's window list; a poll is answered with the windows whose half-open
/// span contains the polling moment.
#[derive(Clone, Debug)]
struct StandingHandler {
    configuration: Vec<u8>,
    windows: Vec<StandingWindow>,
}

impl StandingHandler {
    fn from_configuration(configuration: &[u8]) -> Result<Self, ChannelError> {
        let windows =
            fault_policy::decode_windows(configuration).map_err(|_| ChannelError::Malformed)?;
        Ok(Self {
            configuration: configuration.to_vec(),
            windows,
        })
    }
}

impl ServiceHandler for StandingHandler {
    fn identity(&self) -> &[u8] {
        SERVICE_IDENTITY
    }

    fn configuration(&self) -> &[u8] {
        &self.configuration
    }

    fn respond(
        &mut self,
        moment: Moment,
        question: &Question,
    ) -> Result<ServiceResponse, ChannelError> {
        if question.service() != STANDING_NAMESPACE {
            return Err(ChannelError::Handler(
                "the fault package answers only the standing namespace".to_owned(),
            ));
        }
        let live: Vec<StandingWindow> = self
            .windows
            .iter()
            .filter(|window| window.contains(moment))
            .cloned()
            .collect();
        let bytes = encode_standing(moment, &live).map_err(|_| ChannelError::TooLarge)?;
        Ok(ServiceResponse::Answered(ChannelAnswer::data(bytes)?))
    }

    // The answer is a pure function of the configuration and the polling
    // moment, so there is no dynamic state to carry across a snapshot.
    fn snapshot_state(&self) -> Result<Vec<u8>, ChannelError> {
        Ok(Vec::new())
    }

    fn restore_state(&mut self, state: &[u8]) -> Result<(), ChannelError> {
        if state.is_empty() {
            Ok(())
        } else {
            Err(ChannelError::Handler(
                "the fault standing service carries no state".to_owned(),
            ))
        }
    }

    fn clone_box(&self) -> Box<dyn ServiceHandler> {
        Box::new(self.clone())
    }
}

/// The factory that materializes this package's service from a branch's
/// recorded configuration.
#[must_use]
pub fn service_factory() -> ServiceFactory {
    Arc::new(|config: &ServiceConfig| {
        if config.identity != SERVICE_IDENTITY {
            return Err(ChannelError::Handler(
                "the branch names a service this package does not own".to_owned(),
            ));
        }
        Ok(
            Box::new(StandingHandler::from_configuration(&config.configuration)?)
                as Box<dyn ServiceHandler>,
        )
    })
}

/// The branch configuration that installs `actions`' standing faults.
fn branch_config(
    windows: ActionWindows,
    actions: &[FaultAction],
) -> Result<ServiceConfig, Box<dyn Error>> {
    Ok(ServiceConfig {
        identity: SERVICE_IDENTITY.to_vec(),
        configuration: encode_windows(&standing_windows(windows, actions))?,
    })
}

#[derive(Debug)]
struct Config {
    key: [u8; 32],
    kernel: Vec<u8>,
    initramfs: Vec<u8>,
    session: SessionConfig,
    horizon_nanos: u64,
}

struct Live {
    key: [u8; 32],
    session: Session,
    setup: SnapId,
    windows: ActionWindows,
    /// Cached prefix endpoints with the use stamp that orders eviction.
    snapshots: BTreeMap<Vec<FaultAction>, (SnapId, u64)>,
    uses: u64,
    /// Set when a guest stopped answering and must not be resumed.
    abandoned: bool,
}

thread_local! {
    static LIVE: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// One workload-aware target handle; the live session stays thread-local.
#[derive(Debug)]
pub struct FaultTarget {
    config: Arc<Config>,
    actions: Vec<FaultAction>,
    observation: FaultObservations,
    action_observations: Vec<FaultObservations>,
    failed: bool,
    horizons_clocked: u64,
    root_seal: u64,
}

impl FaultTarget {
    /// Boot or join the current evaluator thread's session.
    ///
    /// # Errors
    ///
    /// Returns an error when the guest cannot boot or never reaches the fault
    /// agent's setup point.
    pub fn new(kernel: &[u8], initramfs: &[u8], config: &FaultConfig) -> Result<Self, String> {
        let config = Arc::new(Config {
            key: Sha256::digest(identity(kernel, initramfs, config)).into(),
            kernel: kernel.to_vec(),
            initramfs: initramfs.to_vec(),
            session: config.session_config(),
            horizon_nanos: config.horizon_nanos,
        });
        let (observation, root_seal) = with_live(&config, |live| {
            let observation = live.observe(FaultStop::Deadline)?;
            Ok((observation, live.windows.root_seal))
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

    /// The whole-VM state hash of the current endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error when the session cannot hash its state.
    pub fn state_hash(&self) -> Result<[u8; 32], String> {
        with_live(&self.config, |live| {
            live.session
                .state_hash()
                .map_err(|error| format!("state hash: {error}"))
        })
    }

    /// The last bytes the guest wrote to its serial console.
    #[must_use]
    pub fn console_tail(&self) -> String {
        LIVE.with(|cell| {
            cell.borrow_mut().as_mut().map_or_else(String::new, |live| {
                let bytes = live.session.console_tail().unwrap_or_default();
                let start = bytes.len().saturating_sub(CONSOLE_TAIL);
                String::from_utf8_lossy(&bytes[start..]).into_owned()
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
        let result = with_live(&self.config, |live| {
            let setup = live.setup;
            live.replay(setup)?;
            live.observe(FaultStop::Deadline)
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
    pub fn restore(&mut self, snapshot: &FaultSnapshot) -> Result<(), Box<dyn Error>> {
        let rebuilt = with_live(&self.config, |live| {
            match live.ensure_prefix(&snapshot.actions)? {
                Ok(snap) => {
                    live.replay(snap)?;
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
                    "fault prefix {:?} first stopped at {:?} and stopped at {:?} when rebuilt",
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
    pub fn snapshot(&self) -> Option<FaultSnapshot> {
        (!self.failed && self.observation.stop.is_continuable()).then(|| FaultSnapshot {
            actions: self.actions.clone(),
            observation: self.observation.clone(),
            failed: false,
        })
    }

    /// Export the current endpoint as a portable whole-VM snapshot, which is
    /// what a replay of one recorded input hands back to a fresh session.
    ///
    /// # Errors
    ///
    /// Returns an error when the session cannot export it.
    pub fn portable_snapshot(&self) -> Result<PortableSnapshot, String> {
        with_live(&self.config, |live| {
            live.session
                .setup_snapshot()
                .map_err(|error| format!("portable snapshot: {error}"))
        })
    }

    /// Apply one action: stage its environment delta, run to the horizon
    /// deadline, and observe the endpoint.
    pub fn apply(&mut self, action: FaultAction) {
        self.action_observations.clear();
        if self.failed || !self.observation.stop.is_continuable() {
            return;
        }
        let result = with_live(&self.config, |live| live.advance(&self.actions, action));
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

fn with_live<T>(
    config: &Arc<Config>,
    operation: impl FnOnce(&mut Live) -> Result<T, String>,
) -> Result<T, String> {
    LIVE.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.as_ref().is_none_or(|live| live.key != config.key) {
            *slot = Some(Live::boot(config)?);
        }
        let live = slot.as_mut().ok_or("the session was not initialized")?;
        let result = operation(live);
        // A guest the session gave up on must not be resumed, so the next
        // operation on this thread boots a fresh one.
        if live.abandoned {
            *slot = None;
        }
        result
    })
}

impl Live {
    fn boot(config: &Config) -> Result<Self, String> {
        let mut session = Session::new_with_config_and_payloads(
            &config.kernel,
            &config.initramfs,
            config.session.clone(),
            Vec::new(),
        )
        .map_err(|error| format!("fault guest boot failed: {error}"))?;
        // A campaign never encodes the virtual-time trace, so the sparse
        // checkpoint hash over all guest RAM would only slow every execution.
        session
            .defer_virtual_time_checkpoint_hashes()
            .map_err(|error| format!("defer virtual-time hashes: {error}"))?;
        session.set_service_factory(service_factory());
        // The client boots to the fault agent's `setup_complete`, which the
        // agent publishes once every node is up and the readiness command has
        // passed, and seals it.
        let (setup, root_seal) = session.setup_handle();
        let mut snapshots = BTreeMap::new();
        snapshots.insert(Vec::new(), (setup, 0));
        Ok(Self {
            key: config.key,
            session,
            setup,
            windows: ActionWindows {
                root_seal,
                horizon_nanos: config.horizon_nanos,
            },
            snapshots,
            uses: 0,
            abandoned: false,
        })
    }

    /// Note a failure that leaves the guest unusable, so the thread reboots.
    fn abandon(&mut self, what: &str, error: &dyn std::fmt::Display) -> String {
        self.abandoned = true;
        let tail = self.session.console_tail().unwrap_or_default();
        let start = tail.len().saturating_sub(CONSOLE_TAIL);
        eprintln!(
            "fault guest abandoned during {what}: {error}\nconsole tail:\n{}",
            String::from_utf8_lossy(&tail[start..])
        );
        format!("{what}: {error}")
    }

    fn replay(&mut self, snapshot: SnapId) -> Result<(), String> {
        self.session
            .replay_snapshot(snapshot)
            .map_err(|error| format!("replay: {error}"))
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
                self.session
                    .drop_snapshot(snap)
                    .map_err(|error| format!("drop snapshot: {error}"))?;
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
            .ok_or("the fault setup snapshot is missing")?;
        for index in start..actions.len() {
            // Each endpoint is reached the way its first run reached it:
            // branched from its parent's snapshot under the standing list of
            // that shorter input. Running the actions back to back under the
            // whole input's list gives the guest agent a longer list to
            // reconcile, and that shifts the guest's timing enough for a
            // rebuilt endpoint to differ from the one first observed.
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
            self.replay(snap)?;
            return self.observe(FaultStop::Deadline);
        }
        let parent = match self.ensure_prefix(prefix)? {
            Ok(parent) => parent,
            Err(observation) => {
                eprintln!(
                    "fault prefix {prefix:?} stopped at {:?} when rebuilt",
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
        let config = branch_config(self.windows, actions)
            .map_err(|error| format!("branch configuration: {error}"))?;
        self.session
            .branch_with_service(parent, config, Vec::new())
            .map_err(|error| format!("branch: {error}"))
    }

    /// Run `action` to its horizon and seal the endpoint. An endpoint the
    /// session cannot seal is run a little further and retried; one that never
    /// seals is observed with no snapshot, so the search never branches from
    /// it.
    fn run_action(
        &mut self,
        action: FaultAction,
        index: usize,
    ) -> Result<(FaultObservations, Option<SnapId>), String> {
        let window = self.windows.window(index);
        if let Some(perturb) = action_delta(action, window).perturb {
            let fault = fault_policy::HostFault::decode(&perturb.fault)
                .map_err(|error| format!("staged host fault: {error}"))?;
            let effect = fault_policy::consonance::effect(&fault)
                .map_err(|error| format!("staged host fault: {error}"))?;
            self.session
                .stage_effect(perturb.at, &effect)
                .map_err(|error| format!("stage effect: {error}"))?;
        }
        let deadline = self.windows.deadline(index);
        let stop = match self.session.run_until(deadline) {
            Ok(stop) => stop,
            Err(error) => return Err(self.abandon("run", &error)),
        };
        if let StopReason::Crash { vtime, info } = &stop {
            eprintln!(
                "fault guest crashed at {vtime:?}: {:?} {}",
                info.kind,
                String::from_utf8_lossy(&info.detail)
            );
        }
        let mut stop = FaultStop::from_stop_reason(&stop);
        let mut snap = None;
        if stop.is_continuable() {
            match self.session.seal(SETTLE_STEP_NANOS, SETTLE_ATTEMPTS) {
                Ok((sealed, _at, settled)) => {
                    stop = FaultStop::from_stop_reason(&settled);
                    if stop.is_continuable() {
                        snap = Some(sealed);
                    }
                }
                Err(error) => return Err(self.abandon("seal", &error)),
            }
        }
        if snap.is_none() && stop.is_continuable() {
            stop = FaultStop::Unexpected;
        }
        Ok((self.observe(stop)?, snap))
    }

    fn observe(&mut self, stop: FaultStop) -> Result<FaultObservations, String> {
        let events = self
            .session
            .sdk_events()
            .map_err(|error| format!("SDK events: {error}"))?;
        let moment = events.last().map_or(0, |(moment, _, _)| *moment);
        let capture = decode_sdk_events(&events)?;
        Ok(FaultObservations::new(moment, &capture, stop))
    }
}

/// Deterministic memory charge for one resident snapshot.
#[must_use]
pub fn snapshot_memory_charge(snapshot: &FaultSnapshot) -> usize {
    size_of::<FaultSnapshot>()
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

/// Stable identity string for one image and the way it boots.
#[must_use]
pub fn identity(kernel: &[u8], initramfs: &[u8], config: &FaultConfig) -> String {
    format!(
        "faults-consonance-whole-vm-v1;session={};horizon-nanos={};\
         action=standing-fault-delta-v1;snapshot=portable-prefix-to-vm-snapshot-v1",
        identity_with_config(kernel, initramfs, &config.session_config()),
        config.horizon_nanos,
    )
}

/// Read a target from the two guest image paths.
///
/// # Errors
///
/// Returns an error when an image cannot be read or the guest cannot boot.
pub fn from_paths(
    kernel: &Path,
    initramfs: &Path,
    config: &FaultConfig,
) -> Result<FaultTarget, String> {
    let kernel = std::fs::read(kernel).map_err(|error| format!("read kernel: {error}"))?;
    let initramfs = std::fs::read(initramfs).map_err(|error| format!("read initramfs: {error}"))?;
    FaultTarget::new(&kernel, &initramfs, config)
}
