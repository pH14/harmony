// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    cell::RefCell, collections::BTreeMap, error::Error, path::Path, sync::Arc, time::Duration,
};

use consonance_client::session::{
    PortableSnapshot, Session, SessionConfig, SessionError, identity_with_config,
};
use control_proto::{SnapId, StopReason};
use environment::{
    Moment,
    channel::{
        Answer as ChannelAnswer, ChannelError, Effect, Question, ServiceHandler, ServiceResponse,
    },
    input_spec::{ServiceConfig, ServiceFactory, nominal_factory},
};
use fault_policy::{STANDING_NAMESPACE, StandingWindow, encode_standing, encode_windows};
use searcher::target::ExitKind;
use sha2::{Digest, Sha256};

use crate::target::{
    ActionWindows, FaultAction, FaultObservations, FaultSnapshot, FaultStop, action_delta,
    decode_sdk_events, standing_windows,
};

pub const DEFAULT_RAM_MIB: u32 = 1024;
pub const SERVICE_IDENTITY: &[u8] = b"faults-standing-v1";
const SEED: u64 = 0x4661_756c_744c_6162;
const SETUP_BUDGET: u64 = 120_000_000_000;
const WALL_LIMIT: Duration = Duration::from_secs(60);
const SNAPSHOT_CACHE_LIMIT: usize = 96;
const CONSOLE_TAIL: usize = 1_500;
const WATCHDOG_CUTOFF: &str = "fault-guest-watchdog-cutoff: ";

#[cfg(target_arch = "x86_64")]
const CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t tsc=reliable \
    no_timer_check lpj=4000000 random.trust_cpu=off nokaslr nosmp maxcpus=1 \
    nox2apic hpet=disable harmony_pvclock rdinit=/init";
#[cfg(target_arch = "aarch64")]
const CMDLINE: &str = "console=ttyAMA0 earlycon=pl011,0x09000000 nohlt rdinit=/init";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FaultConfig {
    pub knobs: Vec<String>,
    pub horizon_nanos: u64,
    pub ram_mib: u32,
}

impl FaultConfig {
    #[must_use]
    pub fn cmdline(&self) -> String {
        let mut cmdline = CMDLINE.to_owned();
        for knob in &self.knobs {
            cmdline.push(' ');
            cmdline.push_str(knob);
        }
        cmdline
    }

    #[must_use]
    pub fn session_config(&self) -> SessionConfig {
        let ram = usize::try_from(self.ram_mib)
            .unwrap_or(usize::MAX / (1024 * 1024))
            .saturating_mul(1024 * 1024);
        SessionConfig::new(ram, SEED, SETUP_BUDGET, self.cmdline())
            .with_identity_tag(IDENTITY_TAG)
            .with_wall_limit(WALL_LIMIT)
            .with_deferred_virtual_time_checkpoint_hashes()
    }
}

const IDENTITY_TAG: &str = "faults-consonance-execution-v1";

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

#[must_use]
pub fn service_factory() -> ServiceFactory {
    let nominal = nominal_factory();
    Arc::new(move |config: &ServiceConfig| {
        if config.identity != SERVICE_IDENTITY {
            return nominal(config);
        }
        Ok(
            Box::new(StandingHandler::from_configuration(&config.configuration)?)
                as Box<dyn ServiceHandler>,
        )
    })
}

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

#[derive(Clone, Copy, Debug)]
struct Cached {
    snap: SnapId,
    moment: u64,
    stamp: u64,
}

struct Live {
    key: [u8; 32],
    session: Session,
    setup: SnapId,
    windows: ActionWindows,
    snapshots: BTreeMap<Vec<FaultAction>, Cached>,
    uses: u64,
    horizons_run: u64,
    abandoned: bool,
}

thread_local! {
    static LIVE: RefCell<Option<Live>> = const { RefCell::new(None) };
}

#[derive(Debug)]
pub struct FaultTarget {
    config: Arc<Config>,
    actions: Vec<FaultAction>,
    observation: FaultObservations,
    action_observations: Vec<FaultObservations>,
    failed: bool,
    watchdog_cutoffs: u64,
    horizons_clocked: u64,
    guest_horizons_run: u64,
    root_seal: u64,
}

impl FaultTarget {
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
            watchdog_cutoffs: 0,
            horizons_clocked: 0,
            guest_horizons_run: 0,
            root_seal,
        })
    }

    pub fn fresh(kernel: &[u8], initramfs: &[u8], config: &FaultConfig) -> Result<Self, String> {
        LIVE.with(|slot| slot.borrow_mut().take());
        Self::new(kernel, initramfs, config)
    }

    #[must_use]
    pub fn root_seal(&self) -> u64 {
        self.root_seal
    }

    #[must_use]
    pub fn observation(&self) -> &FaultObservations {
        &self.observation
    }

    #[must_use]
    pub fn last_action_observations(&self) -> &[FaultObservations] {
        &self.action_observations
    }

    #[must_use]
    pub fn found_bug(&self) -> bool {
        self.observation.is_bug()
    }

    #[must_use]
    pub fn failed(&self) -> bool {
        self.failed
    }

    #[must_use]
    pub fn watchdog_cutoffs(&self) -> u64 {
        self.watchdog_cutoffs
    }

    #[must_use]
    pub fn horizons_clocked(&self) -> u64 {
        self.horizons_clocked
    }

    #[must_use]
    pub fn guest_horizons_run(&self) -> u64 {
        self.guest_horizons_run
    }

    pub fn state_hash(&self) -> Result<[u8; 32], String> {
        with_live(&self.config, |live| {
            live.session
                .state_hash()
                .map_err(|error| format!("state hash: {error}"))
        })
    }

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

    #[must_use]
    pub fn actions(&self) -> &[FaultAction] {
        &self.actions
    }

    pub fn reset(&mut self) {
        let result = with_live(&self.config, |live| {
            let setup = live.setup;
            live.replay(setup)?;
            live.observe(FaultStop::Deadline)
        });
        match result {
            Ok(observation) => {
                self.actions.clear();
                self.guest_horizons_run = 0;
                self.observation = observation.clone();
                self.action_observations = vec![observation];
                self.failed = false;
                self.watchdog_cutoffs = 0;
            }
            Err(error) => {
                eprintln!("fault target reset failed: {error}");
                self.failed = true;
            }
        }
    }

    pub fn restore(&mut self, snapshot: &FaultSnapshot) -> Result<(), Box<dyn Error>> {
        let rebuilt = with_live(&self.config, |live| {
            match live.ensure_prefix(&snapshot.actions)? {
                Ok(cached) => {
                    live.replay(cached.snap)?;
                    Ok(None)
                }
                Err(observation) => Ok(Some(observation)),
            }
        })?;
        self.actions.clone_from(&snapshot.actions);
        self.observation = match rebuilt {
            None => snapshot.observation.clone(),
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
        self.watchdog_cutoffs = 0;
        Ok(())
    }

    #[must_use]
    pub fn snapshot(&self) -> Option<FaultSnapshot> {
        (!self.failed && self.observation.stop.is_continuable()).then(|| FaultSnapshot {
            actions: self.actions.clone(),
            observation: self.observation.clone(),
            failed: false,
        })
    }

    pub fn portable_snapshot(&self) -> Result<PortableSnapshot, String> {
        with_live(&self.config, |live| {
            live.session
                .setup_snapshot()
                .map_err(|error| format!("portable snapshot: {error}"))
        })
    }

    pub fn apply(&mut self, action: FaultAction) {
        self.action_observations.clear();
        if self.failed || !self.observation.stop.is_continuable() {
            return;
        }
        let result = with_live(&self.config, |live| {
            let before = live.horizons_run;
            let observation = live.advance(&self.actions, action)?;
            Ok((observation, live.horizons_run.saturating_sub(before)))
        });
        match result {
            Ok((observation, ran)) => {
                self.actions.push(action);
                self.horizons_clocked = self.horizons_clocked.saturating_add(1);
                self.guest_horizons_run = self.guest_horizons_run.saturating_add(ran);
                self.observation = observation.clone();
                self.action_observations.push(observation);
            }
            Err(error) => {
                eprintln!("fault action {action:?} failed: {error}");
                if error.starts_with(WATCHDOG_CUTOFF) {
                    self.watchdog_cutoffs = self.watchdog_cutoffs.saturating_add(1);
                    let mut observation = self.observation.clone();
                    observation.watchdog_cutoff = true;
                    self.action_observations.push(observation);
                }
                self.failed = true;
            }
        }
    }

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
        session.set_service_factory(service_factory());
        let (setup, root_seal) = session.setup_handle();
        let mut snapshots = BTreeMap::new();
        snapshots.insert(
            Vec::new(),
            Cached {
                snap: setup,
                moment: root_seal,
                stamp: 0,
            },
        );
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
            horizons_run: 0,
            abandoned: false,
        })
    }

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

    fn cached(&mut self, actions: &[FaultAction]) -> Option<Cached> {
        self.uses = self.uses.saturating_add(1);
        let uses = self.uses;
        self.snapshots.get_mut(actions).map(|cached| {
            cached.stamp = uses;
            *cached
        })
    }

    fn remember(
        &mut self,
        actions: Vec<FaultAction>,
        snap: SnapId,
        moment: u64,
    ) -> Result<Cached, String> {
        self.uses = self.uses.saturating_add(1);
        let cached = Cached {
            snap,
            moment,
            stamp: self.uses,
        };
        self.snapshots.insert(actions, cached);
        while self.snapshots.len() > SNAPSHOT_CACHE_LIMIT.saturating_add(1) {
            let Some(victim) = self
                .snapshots
                .iter()
                .filter(|(prefix, _)| !prefix.is_empty())
                .min_by_key(|(_, cached)| cached.stamp)
                .map(|(prefix, _)| prefix.clone())
            else {
                break;
            };
            if let Some(victim) = self.snapshots.remove(&victim) {
                self.session
                    .drop_snapshot(victim.snap)
                    .map_err(|error| format!("drop snapshot: {error}"))?;
            }
        }
        Ok(cached)
    }

    fn ensure_prefix(
        &mut self,
        actions: &[FaultAction],
    ) -> Result<Result<Cached, FaultObservations>, String> {
        if let Some(cached) = self.cached(actions) {
            return Ok(Ok(cached));
        }
        let start = (0..actions.len())
            .rev()
            .find(|length| self.snapshots.contains_key(&actions[..*length]))
            .unwrap_or(0);
        let mut last = self
            .cached(&actions[..start])
            .ok_or("the fault setup snapshot is missing")?;
        for index in start..actions.len() {
            self.branch(last, &actions[..=index])?;
            let (observation, sealed) = self.run_action(index)?;
            let Some((snap, moment)) = sealed else {
                return Ok(Err(observation));
            };
            last = self.remember(actions[..=index].to_vec(), snap, moment)?;
        }
        Ok(Ok(last))
    }

    fn advance(
        &mut self,
        prefix: &[FaultAction],
        action: FaultAction,
    ) -> Result<FaultObservations, String> {
        let mut next = prefix.to_vec();
        next.push(action);
        if let Some(cached) = self.cached(&next) {
            self.replay(cached.snap)?;
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
        let (observation, sealed) = self.run_action(prefix.len())?;
        if let Some((snap, moment)) = sealed {
            self.remember(next, snap, moment)?;
        }
        Ok(observation)
    }

    fn branch(&mut self, parent: Cached, actions: &[FaultAction]) -> Result<(), String> {
        let config = branch_config(self.windows, actions)
            .map_err(|error| format!("branch configuration: {error}"))?;
        let effects = self.staged_effects(actions, parent.moment)?;
        self.session
            .branch_with_service(parent.snap, config, Vec::new(), effects)
            .map_err(|error| format!("branch: {error}"))
    }

    fn staged_effects(
        &self,
        actions: &[FaultAction],
        floor: u64,
    ) -> Result<Vec<(u64, Effect)>, String> {
        let Some(index) = actions.len().checked_sub(1) else {
            return Ok(Vec::new());
        };
        let Some(perturb) = action_delta(actions[index], self.windows.window(index)).perturb else {
            return Ok(Vec::new());
        };
        let fault = fault_policy::HostFault::decode(&perturb.fault)
            .map_err(|error| format!("staged host fault: {error}"))?;
        let effect = fault_policy::consonance::effect(&fault)
            .map_err(|error| format!("staged host fault: {error}"))?;
        Ok(vec![(perturb.at.max(floor), effect)])
    }

    fn run_action(
        &mut self,
        index: usize,
    ) -> Result<(FaultObservations, Option<(SnapId, u64)>), String> {
        let deadline = self.windows.deadline(index);
        self.horizons_run = self.horizons_run.saturating_add(1);
        let stop = match self.session.run_until(deadline) {
            Ok(stop) => stop,
            Err(error) => {
                let watchdog = matches!(
                    error.downcast_ref::<SessionError>(),
                    Some(SessionError::Hung(_) | SessionError::Abandoned)
                );
                let message = self.abandon("run", &error);
                return Err(if watchdog {
                    format!("{WATCHDOG_CUTOFF}{message}")
                } else {
                    message
                });
            }
        };
        if let StopReason::Crash { vtime, info } = &stop {
            let tail = self.session.console_tail().unwrap_or_default();
            let start = tail.len().saturating_sub(CONSOLE_TAIL);
            eprintln!(
                "fault guest crashed at {vtime:?}: {:?} {}\nconsole tail:\n{}",
                info.kind,
                String::from_utf8_lossy(&info.detail),
                String::from_utf8_lossy(&tail[start..])
            );
        }
        let stop = FaultStop::from_stop_reason(&stop);
        let snap = if stop.is_continuable() {
            let (snapshot, at) = self
                .session
                .snapshot()
                .map_err(|error| self.abandon("snapshot", &error))?;
            Some((snapshot, at))
        } else {
            None
        };
        let observation = match self.observe(stop) {
            Ok(observation) => observation,
            Err(error) => return Err(self.abandon("observe", &error)),
        };
        Ok((observation, snap))
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

#[must_use]
pub fn identity(kernel: &[u8], initramfs: &[u8], config: &FaultConfig) -> String {
    format!(
        "faults-consonance-whole-vm-v1;session={};horizon-nanos={};\
         action=standing-fault-delta-v1;snapshot=portable-prefix-to-vm-snapshot-v1",
        identity_with_config(kernel, initramfs, &config.session_config()),
        config.horizon_nanos,
    )
}

pub fn from_paths(
    kernel: &Path,
    initramfs: &Path,
    config: &FaultConfig,
) -> Result<FaultTarget, String> {
    let kernel = std::fs::read(kernel).map_err(|error| format!("read kernel: {error}"))?;
    let initramfs = std::fs::read(initramfs).map_err(|error| format!("read initramfs: {error}"))?;
    FaultTarget::new(&kernel, &initramfs, config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_factory_serves_the_nominal_service_the_setup_point_was_sealed_under() {
        let factory = service_factory();
        let handler = factory(&ServiceConfig::default()).expect("nominal service");
        assert_eq!(handler.identity(), ServiceConfig::default().identity);
        let own = factory(&ServiceConfig {
            identity: SERVICE_IDENTITY.to_vec(),
            configuration: encode_windows(&[]).expect("an empty standing list encodes"),
        })
        .expect("an empty standing list");
        assert_eq!(own.identity(), SERVICE_IDENTITY);
        assert!(
            factory(&ServiceConfig {
                identity: b"another-package".to_vec(),
                configuration: Vec::new(),
            })
            .is_err()
        );
    }
}
