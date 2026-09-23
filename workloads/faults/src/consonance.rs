// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    cell::RefCell,
    collections::BTreeMap,
    error::Error,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use consonance_client::{
    cache::{CacheError, CacheIndex, Lease, Namespace},
    session::{
        SearchSession, Session, SessionConfig, SessionError, UmlLaunch, WorkerLauncher,
        WorkerSession, identity_with_config,
    },
};
use control_proto::{SnapId, StopReason};
use environment::{
    Moment,
    channel::{Answer as ChannelAnswer, ChannelError, Question, ServiceHandler, ServiceResponse},
    input_spec::{ServiceConfig, ServiceFactory, nominal_factory},
};
use fault_policy::{STANDING_NAMESPACE, StandingWindow, encode_standing, encode_windows};
use searcher::target::ExitKind;
use sha2::{Digest, Sha256};

use crate::chain::{Chain, Hit, Point, Shared};
use crate::target::{
    ActionWindows, FaultAction, FaultObservations, FaultSnapshot, FaultStop, actions_key,
    decode_sdk_events, standing_windows,
};

pub const DEFAULT_RAM_MIB: u32 = 1024;
pub const SERVICE_IDENTITY: &[u8] = b"faults-standing-coverage-v2";
pub const SESSION_SERVICE: &str = "faults";
const SEED: u64 = 0x4661_756c_744c_6162;
const SETUP_BUDGET: u64 = 120_000_000_000;
const WALL_LIMIT: Duration = Duration::from_secs(60);
const CONSOLE_TAIL: usize = 1_500;
const WATCHDOG_CUTOFF: &str = "fault-guest-watchdog-cutoff: ";

#[cfg(target_arch = "x86_64")]
const CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t tsc=reliable \
    no_timer_check lpj=4000000 random.trust_cpu=off nokaslr nosmp maxcpus=1 \
    nox2apic hpet=disable noxsaveopt noxsaves LD_BIND_NOW=1 rdinit=/init";
#[cfg(target_arch = "aarch64")]
const CMDLINE: &str = "console=ttyAMA0 earlycon=pl011,0x09000000 nohlt rdinit=/init";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FaultConfig {
    pub knobs: Vec<String>,
    pub ram_mib: u32,
    pub backend: GuestBackend,
    pub root: Option<Arc<Vec<u8>>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum GuestBackend {
    #[default]
    Vm,
    Uml(UmlGuest),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UmlGuest {
    pub profile: PathBuf,
    pub identity: String,
    pub work_parent: PathBuf,
}

impl UmlGuest {
    pub fn load(profile: &Path, work_parent: PathBuf) -> Result<Self, String> {
        let verified = uml::Profile::load(profile)
            .map_err(|error| format!("User-mode Linux profile: {error}"))?;
        let host = uml::HostIdentity::current()
            .map_err(|error| format!("User-mode Linux host identity: {error}"))?;
        let identity = format!(
            "profile={};architecture={};cpu={};features={:x}",
            verified.identity_sha256,
            host.architecture,
            host.cpu_model,
            Sha256::digest(host.cpu_features.as_bytes())
        );
        Ok(Self {
            profile: profile.to_path_buf(),
            identity,
            work_parent,
        })
    }

    pub fn executable(profile: &Path) -> Result<PathBuf, String> {
        uml::Profile::load(profile)
            .map(|verified| verified.executable())
            .map_err(|error| format!("User-mode Linux profile: {error}"))
    }

    fn kernel_arguments(knobs: &[String]) -> Vec<String> {
        let mut arguments = knobs.to_vec();
        arguments.push("rdinit=/usr/lib/harmony/init".to_owned());
        arguments
    }
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
    pub fn uml(&self) -> Option<&UmlGuest> {
        match &self.backend {
            GuestBackend::Vm => None,
            GuestBackend::Uml(guest) => Some(guest),
        }
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

const IDENTITY_TAG: &str = "faults-consonance-execution-v6";

type CoverageWindow = (u64, u64, std::num::NonZeroU16);
const COVERAGE_WINDOW_BYTES: usize = 18;

fn encode_configuration(standing: &[u8], coverage: &[CoverageWindow]) -> Result<Vec<u8>, String> {
    let length = u32::try_from(standing.len()).map_err(|_| "standing windows too large")?;
    let mut bytes = Vec::with_capacity(4 + standing.len() + coverage.len() * COVERAGE_WINDOW_BYTES);
    bytes.extend_from_slice(&length.to_le_bytes());
    bytes.extend_from_slice(standing);
    for (start, end, quantum) in coverage {
        bytes.extend_from_slice(&start.to_le_bytes());
        bytes.extend_from_slice(&end.to_le_bytes());
        bytes.extend_from_slice(&quantum.get().to_le_bytes());
    }
    Ok(bytes)
}

fn decode_configuration(bytes: &[u8]) -> Option<(&[u8], Vec<CoverageWindow>)> {
    let (length, rest) = bytes.split_first_chunk::<4>()?;
    let length = usize::try_from(u32::from_le_bytes(*length)).ok()?;
    let (standing, rest) = rest.split_at_checked(length)?;
    if !rest.len().is_multiple_of(COVERAGE_WINDOW_BYTES) {
        return None;
    }
    let mut coverage = Vec::with_capacity(rest.len() / COVERAGE_WINDOW_BYTES);
    let mut previous_end = None;
    for row in rest.chunks_exact(COVERAGE_WINDOW_BYTES) {
        let start = u64::from_le_bytes(row[..8].try_into().ok()?);
        let end = u64::from_le_bytes(row[8..16].try_into().ok()?);
        let quantum = std::num::NonZeroU16::new(u16::from_le_bytes(row[16..].try_into().ok()?))?;
        if start >= end || previous_end.is_some_and(|previous| previous != start) {
            return None;
        }
        previous_end = Some(end);
        coverage.push((start, end, quantum));
    }
    Some((standing, coverage))
}

#[derive(Debug)]
struct StandingPlan {
    configuration: Vec<u8>,
    windows: Vec<StandingWindow>,
    coverage: Vec<CoverageWindow>,
}

#[derive(Clone, Debug)]
struct StandingHandler(Arc<StandingPlan>);

impl StandingHandler {
    fn from_configuration(configuration: &[u8]) -> Result<Self, ChannelError> {
        let (standing, coverage) =
            decode_configuration(configuration).ok_or(ChannelError::Malformed)?;
        let windows =
            fault_policy::decode_windows(standing).map_err(|_| ChannelError::Malformed)?;
        Ok(Self(Arc::new(StandingPlan {
            configuration: configuration.to_vec(),
            windows,
            coverage,
        })))
    }
}

impl ServiceHandler for StandingHandler {
    fn identity(&self) -> &[u8] {
        SERVICE_IDENTITY
    }

    fn configuration(&self) -> &[u8] {
        &self.0.configuration
    }

    fn respond(
        &mut self,
        moment: Moment,
        question: &Question,
    ) -> Result<ServiceResponse, ChannelError> {
        if question.service() == environment::channel::SERVICE_COVERAGE_QUANTUM {
            if question.payload().len() != 8 {
                return Err(ChannelError::Malformed);
            }
            let quantum = self
                .0
                .coverage
                .iter()
                .find(|(start, end, _)| *start <= moment && moment < *end)
                .map_or(
                    environment::channel::DEFAULT_COVERAGE_QUANTUM,
                    |(_, _, quantum)| u64::from(quantum.get()),
                );
            return Ok(ServiceResponse::Answered(ChannelAnswer::data(
                quantum.to_le_bytes().to_vec(),
            )?));
        }
        if question.service() == process_proto::debug::NAMESPACE {
            return Ok(ServiceResponse::Answered(ChannelAnswer::data(Vec::new())?));
        }
        if question.service() != STANDING_NAMESPACE {
            return Err(ChannelError::Handler(
                "the fault package answers only the standing namespace".to_owned(),
            ));
        }
        let live: Vec<StandingWindow> = self
            .0
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
        if config.identity == DEBUG_IDENTITY {
            return Ok(
                Box::new(DebugHandler::decode(&config.configuration)?) as Box<dyn ServiceHandler>
            );
        }
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
        configuration: encode_configuration(
            &encode_windows(&standing_windows(windows, actions)?)?,
            &actions
                .iter()
                .enumerate()
                .map(|(index, action)| {
                    let (start, end) = windows.window(actions, index)?;
                    Ok((start, end, action.coverage_quantum))
                })
                .collect::<Result<Vec<_>, String>>()?,
        )?,
    })
}

#[derive(Debug)]
struct Config {
    key: [u8; 32],
    kernel: Vec<u8>,
    initramfs: Vec<u8>,
    session: SessionConfig,
    cache: Option<Arc<dyn CacheIndex>>,
    worker: Option<WorkerLauncher>,
    uml: Option<UmlLaunch>,
    root: Option<Arc<Vec<u8>>>,
}

fn uml_launch(
    guest: &UmlGuest,
    initramfs: &[u8],
    config: &FaultConfig,
) -> Result<UmlLaunch, String> {
    let digest = Sha256::digest(initramfs);
    std::fs::create_dir_all(&guest.work_parent)
        .map_err(|error| format!("User-mode Linux work directory: {error}"))?;
    let path = guest
        .work_parent
        .join(format!("harmony-initramfs-{digest:x}.cpio"));
    if !path.exists() {
        let mut staged = tempfile::NamedTempFile::new_in(&guest.work_parent)
            .map_err(|error| format!("stage initramfs: {error}"))?;
        std::io::Write::write_all(&mut staged, initramfs)
            .map_err(|error| format!("stage initramfs: {error}"))?;
        staged
            .persist(&path)
            .map_err(|error| format!("stage initramfs: {error}"))?;
    }
    Ok(UmlLaunch {
        profile: guest.profile.clone(),
        initramfs: path,
        memory_mib: config.ram_mib,
        kernel_arguments: UmlGuest::kernel_arguments(&config.knobs),
        work_parent: guest.work_parent.clone(),
        seed: SEED,
        setup_budget: SETUP_BUDGET,
        progress_limit: WALL_LIMIT,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RunKind {
    New,
    Rebuild,
    Replay,
}

#[derive(Clone, Copy, Debug, Default)]
struct Spent {
    count: u64,
    nanos: u64,
}

impl Spent {
    fn since(&mut self, started: Instant) {
        self.count = self.count.saturating_add(1);
        self.nanos = self
            .nanos
            .saturating_add(u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX));
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Runs {
    host: Spent,
    virtual_nanos: u64,
}

#[derive(Clone, Debug, Default)]
struct LiveTelemetry {
    boot: Spent,
    new: Runs,
    rebuild: Runs,
    replay: Runs,
    restores: Spent,
    branches: Spent,
    seals: Spent,
    observations: Spent,
    drops: Spent,
    exact_hits: u64,
    ancestor_hits: u64,
    misses: u64,
    evictions: u64,
    imports: Spent,
    publishes: Spent,
    refusals: u64,
}

struct Live {
    key: [u8; 32],
    holder: u64,
    session: Box<dyn SearchSession>,
    base_identity: [u8; 32],
    setup: SnapId,
    windows: ActionWindows,
    chain: Chain,
    horizons_run: u64,
    abandoned: bool,
    telemetry: LiveTelemetry,
}

static HOLDERS: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static LIVE: RefCell<Option<Live>> = const { RefCell::new(None) };
    static RETIRED: RefCell<BTreeMap<String, u64>> = const { RefCell::new(BTreeMap::new()) };
}

#[allow(clippy::disallowed_methods)]
fn started() -> Instant {
    Instant::now()
}

fn add_counters(into: &mut BTreeMap<String, u64>, counters: Vec<(String, u64)>) {
    for (name, value) in counters {
        let total = into.entry(name).or_default();
        *total = total.saturating_add(value);
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        if let Some(shared) = self.chain.shared() {
            shared.index.forget_store(self.holder);
        }
        let counters = self.counters(false);
        let _ = RETIRED.try_with(|retired| add_counters(&mut retired.borrow_mut(), counters));
    }
}

#[must_use]
pub fn thread_telemetry() -> BTreeMap<String, u64> {
    let mut counters = RETIRED.with(|retired| retired.borrow().clone());
    LIVE.with(|slot| {
        if let Some(live) = slot.borrow().as_ref() {
            add_counters(&mut counters, live.counters(true));
        }
    });
    counters
}

#[derive(Debug)]
pub struct FaultTarget {
    config: Arc<Config>,
    actions: Vec<FaultAction>,
    observation: FaultObservations,
    action_observations: Vec<FaultObservations>,
    failed: bool,
    watchdog_cutoffs: u64,
    execution_ticks: u64,
    guest_horizons_run: u64,
    root_seal: u64,
}

#[cfg(target_os = "macos")]
impl Drop for FaultTarget {
    fn drop(&mut self) {
        LIVE.with(|slot| slot.borrow_mut().take());
    }
}

impl FaultTarget {
    pub fn new(
        kernel: &[u8],
        initramfs: &[u8],
        config: &FaultConfig,
        cache: Option<Arc<dyn CacheIndex>>,
        worker: Option<WorkerLauncher>,
    ) -> Result<Self, String> {
        let config = Arc::new(Config {
            key: Sha256::digest(identity(kernel, initramfs, config)).into(),
            kernel: kernel.to_vec(),
            initramfs: initramfs.to_vec(),
            session: config.session_config(),
            root: config.root.clone(),
            cache,
            worker,
            uml: config
                .uml()
                .map(|guest| uml_launch(guest, initramfs, config))
                .transpose()?,
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
            execution_ticks: 0,
            guest_horizons_run: 0,
            root_seal,
        })
    }

    pub fn fresh(kernel: &[u8], initramfs: &[u8], config: &FaultConfig) -> Result<Self, String> {
        LIVE.with(|slot| slot.borrow_mut().take());
        Self::new(kernel, initramfs, config, None, None)
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
    pub fn execution_ticks(&self) -> u64 {
        self.execution_ticks
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

    pub fn console_evidence(&self) -> Result<String, String> {
        with_live(&self.config, |live| {
            live.session
                .console_tail()
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .map_err(|e| e.to_string())
        })
    }

    #[must_use]
    pub fn actions(&self) -> &[FaultAction] {
        &self.actions
    }

    pub fn debug_position(&self) -> Result<(usize, u64), String> {
        with_live(&self.config, |live| {
            let events = live.session.sdk_events().map_err(|e| e.to_string())?;
            let id = events
                .iter()
                .rev()
                .filter(|(_, id, _)| *id == process_proto::debug::STATUS_EVENT)
                .find_map(|(_, _, b)| process_proto::debug::Status::decode(b).ok())
                .map_or(0, |s| s.acknowledged);
            Ok((events.len(), id))
        })
    }
    pub fn debug_step(
        &mut self,
        message: Option<&process_proto::debug::Message>,
        cursor: &mut usize,
    ) -> Result<DebugProgress, String> {
        if self.failed || !self.observation.stop.is_continuable() {
            return Err("the selected state cannot continue; rewind further".into());
        }
        let (progress, observation) = with_live(&self.config, |live| {
            let (snapshot, at) = live.session.snapshot().map_err(|e| e.to_string())?;
            let standing = branch_config(live.windows, &self.actions).map_err(|e| e.to_string())?;
            let service =
                debug_configuration(&standing.configuration, message).map_err(|e| e.to_string())?;
            live.session
                .branch_with_service(snapshot, service, Vec::new(), Vec::new())
                .map_err(|e| e.to_string())?;
            live.session
                .drop_snapshot(snapshot)
                .map_err(|e| e.to_string())?;
            let stop = live
                .session
                .run_until(at.checked_add(10_000_000).ok_or("debug clock overflow")?)
                .map_err(|e| e.to_string())?;
            let observation = live.observe(FaultStop::from_stop_reason(&stop))?;
            let events = live.session.sdk_events().map_err(|e| e.to_string())?;
            let mut progress = DebugProgress {
                status: None,
                output: Vec::new(),
            };
            for (_, id, bytes) in events.iter().skip(*cursor) {
                if *id == process_proto::debug::OUTPUT_EVENT {
                    progress.output.extend_from_slice(bytes);
                }
                if *id == process_proto::debug::STATUS_EVENT {
                    progress.status =
                        Some(process_proto::debug::Status::decode(bytes).map_err(str::to_owned)?);
                }
            }
            *cursor = events.len();
            Ok((progress, observation))
        })?;
        self.observation = observation;
        Ok(progress)
    }

    pub fn export_root(&self) -> Result<Vec<u8>, String> {
        with_live(&self.config, |live| {
            let (snapshot, _) = live.session.snapshot().map_err(|e| e.to_string())?;
            let result = export_root(live.session.as_mut(), snapshot, live.base_identity)
                .map_err(|e| e.to_string());
            live.session
                .drop_snapshot(snapshot)
                .map_err(|e| e.to_string())?;
            result
        })
    }

    pub fn reset(&mut self) {
        let result = with_live(&self.config, |live| {
            live.fit_store(1)?;
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
            live.fit_store(1)?;
            match live.ensure_prefix(&snapshot.actions)? {
                Ok(cached) => {
                    live.replay(cached.snap)?;
                    Ok(None)
                }
                Err(observation) => Ok(Some(observation)),
            }
        });
        self.finish_restore(snapshot, rebuilt)
    }

    fn finish_restore(
        &mut self,
        snapshot: &FaultSnapshot,
        rebuilt: Result<Option<FaultObservations>, String>,
    ) -> Result<(), Box<dyn Error>> {
        self.action_observations.clear();
        self.watchdog_cutoffs = 0;
        let rebuilt = match rebuilt {
            Ok(rebuilt) => rebuilt,
            Err(error) if error.starts_with(WATCHDOG_CUTOFF) => {
                self.actions.clear();
                self.observation = FaultObservations::default();
                self.failed = true;
                self.watchdog_cutoffs = 1;
                self.action_observations.push(FaultObservations {
                    watchdog_cutoff: true,
                    ..FaultObservations::default()
                });
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
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

    pub fn apply(&mut self, action: FaultAction) {
        self.apply_as(action, RunKind::New);
    }

    pub fn apply_replayed(&mut self, action: FaultAction) {
        self.apply_as(action, RunKind::Replay);
    }

    fn apply_as(&mut self, action: FaultAction, kind: RunKind) {
        if self.failed || !self.observation.stop.is_continuable() {
            return;
        }
        self.action_observations.clear();
        let result = with_live(&self.config, |live| {
            let before = live.horizons_run;
            let observation = live.advance(&self.actions, action, kind)?;
            Ok((observation, live.horizons_run.saturating_sub(before)))
        });
        match result {
            Ok((mut observation, ran)) => {
                let since = self.observation.moment;
                observation.parks.retain(|park| park.moment > since);
                observation.park_reads.retain(|read| read.moment > since);
                self.actions.push(action);
                self.execution_ticks = self
                    .execution_ticks
                    .saturating_add(crate::target::action_ticks(&action));
                self.guest_horizons_run = self.guest_horizons_run.saturating_add(ran);
                self.observation = observation.clone();
                self.action_observations.push(observation);
            }
            Err(error) => {
                eprintln!("fault action {action:?} failed: {error}");
                if error.starts_with(WATCHDOG_CUTOFF) {
                    self.watchdog_cutoffs = self.watchdog_cutoffs.saturating_add(1);
                    let mut observation = self.observation.clone();
                    observation.parks.clear();
                    observation.park_reads.clear();
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
        if live.abandoned || live.session.abandoned() {
            *slot = None;
        }
        result
    })
}

fn stop_moment(stop: &StopReason) -> u64 {
    match stop {
        StopReason::Deadline { vtime }
        | StopReason::Quiescent { vtime }
        | StopReason::Crash { vtime, .. }
        | StopReason::Decision { vtime, .. }
        | StopReason::SnapshotPoint { vtime }
        | StopReason::Assertion { vtime, .. } => vtime.0,
    }
}

fn session_failure(what: &str, error: &(dyn Error + 'static)) -> String {
    let message = format!("{what}: {error}");
    if matches!(
        error.downcast_ref::<SessionError>(),
        Some(SessionError::Hung(_) | SessionError::Abandoned)
    ) {
        format!("{WATCHDOG_CUTOFF}{message}")
    } else {
        message
    }
}

impl Live {
    fn open(config: &Config) -> Result<Box<dyn SearchSession>, Box<dyn Error>> {
        if let Some(launch) = &config.uml {
            #[cfg(target_os = "linux")]
            return Ok(Box::new(consonance_client::session::UmlSession::boot(
                launch,
                service_factory(),
            )?));
            #[cfg(not(target_os = "linux"))]
            return Err(format!(
                "User-mode Linux runs only on Linux hosts; {} cannot boot here",
                launch.profile.display()
            )
            .into());
        }
        if let Some(launcher) = &config.worker {
            return Ok(Box::new(WorkerSession::spawn(
                launcher,
                &config.kernel,
                &config.initramfs,
                &config.session,
                &[],
                SESSION_SERVICE,
            )?));
        }
        let mut session = Session::new_with_config_and_payloads(
            &config.kernel,
            &config.initramfs,
            config.session.clone(),
            Vec::new(),
        )?;
        session.set_service_factory(service_factory());
        Ok(Box::new(session))
    }

    fn boot(config: &Config) -> Result<Self, String> {
        let boot_started = started();
        let mut session =
            Self::open(config).map_err(|error| format!("fault guest boot failed: {error}"))?;
        let base_identity = session.root_identity().map_err(|e| e.to_string())?;
        let (mut setup, mut root_seal) = session.setup_handle();
        if let Some(root) = &config.root {
            (setup, root_seal) = import_root(session.as_mut(), root).map_err(|e| e.to_string())?;
            session.replay_snapshot(setup).map_err(|e| e.to_string())?;
        }
        let shared = match &config.cache {
            Some(index) => {
                let setup_hash = session
                    .cache_identity()
                    .map_err(|error| format!("setup cache identity: {error}"))?;
                Some(Shared {
                    index: Arc::clone(index),
                    namespace: Namespace::new(&[&config.key, SERVICE_IDENTITY, &setup_hash]),
                })
            }
            None => None,
        };
        let mut telemetry = LiveTelemetry::default();
        telemetry.boot.since(boot_started);
        Ok(Self {
            key: config.key,
            holder: HOLDERS.fetch_add(1, Ordering::Relaxed),
            base_identity,
            session,
            setup,
            windows: ActionWindows { root_seal },
            chain: Chain::new(
                Point {
                    snap: setup,
                    moment: root_seal,
                },
                shared,
            ),
            horizons_run: 0,
            abandoned: false,
            telemetry,
        })
    }

    fn counters(&self, resident: bool) -> Vec<(String, u64)> {
        let t = &self.telemetry;
        let mut out = vec![
            ("boot.count".to_owned(), t.boot.count),
            ("boot.ns".to_owned(), t.boot.nanos),
        ];
        for (name, runs) in [("new", t.new), ("rebuild", t.rebuild), ("replay", t.replay)] {
            out.push((format!("run.{name}.count"), runs.host.count));
            out.push((format!("run.{name}.ns"), runs.host.nanos));
            out.push((format!("run.{name}.virtual_ns"), runs.virtual_nanos));
        }
        for (name, spent) in [
            ("restore", t.restores),
            ("branch", t.branches),
            ("seal", t.seals),
            ("observe", t.observations),
            ("drop", t.drops),
        ] {
            out.push((format!("session.{name}.count"), spent.count));
            out.push((format!("session.{name}.ns"), spent.nanos));
        }
        out.extend([
            ("cache.exact_hits".to_owned(), t.exact_hits),
            ("cache.ancestor_hits".to_owned(), t.ancestor_hits),
            ("cache.misses".to_owned(), t.misses),
            ("cache.evictions".to_owned(), t.evictions),
            ("shared.import.count".to_owned(), t.imports.count),
            ("shared.import.ns".to_owned(), t.imports.nanos),
            ("shared.publish.count".to_owned(), t.publishes.count),
            ("shared.publish.ns".to_owned(), t.publishes.nanos),
            ("shared.refusals".to_owned(), t.refusals),
        ]);
        if resident {
            let bytes = self
                .chain
                .points()
                .filter_map(|point| self.session.snapshot_owned_pages(point.snap))
                .sum::<u64>()
                .saturating_mul(4096);
            out.push(("cache.entries".to_owned(), self.chain.links() as u64));
            out.push(("cache.resident_bytes".to_owned(), bytes));
        }
        out.extend(
            self.session
                .telemetry_counters()
                .into_iter()
                .map(|(name, value)| (format!("vmm.{name}"), value)),
        );
        out
    }

    fn abandon(&mut self, what: &str, error: &(dyn Error + 'static)) -> String {
        self.abandoned = true;
        let tail = self.session.console_tail().unwrap_or_default();
        let start = tail.len().saturating_sub(CONSOLE_TAIL);
        eprintln!(
            "fault guest abandoned during {what}: {error}\nconsole tail:\n{}",
            String::from_utf8_lossy(&tail[start..])
        );
        session_failure(what, error)
    }

    fn replay(&mut self, snapshot: SnapId) -> Result<(), String> {
        let restore_started = started();
        let result = self
            .session
            .replay_snapshot(snapshot)
            .map_err(|error| format!("replay: {error}"));
        self.telemetry.restores.since(restore_started);
        result
    }

    fn drop_snapshots(&mut self, snapshots: Vec<SnapId>) -> Result<(), String> {
        for snapshot in snapshots {
            let drop_started = started();
            self.session
                .drop_snapshot(snapshot)
                .map_err(|error| format!("drop snapshot: {error}"))?;
            self.telemetry.drops.since(drop_started);
            self.telemetry.evictions = self.telemetry.evictions.saturating_add(1);
        }
        Ok(())
    }

    fn fit_store(&mut self, keep: usize) -> Result<(), String> {
        let Some(index) = self.chain.shared().map(|shared| Arc::clone(&shared.index)) else {
            return Ok(());
        };
        loop {
            let Some(bytes) = self.session.store_bytes() else {
                return Ok(());
            };
            if !index.report_store(self.holder, bytes) {
                return Ok(());
            }
            let dropped = self.chain.shed(keep);
            if dropped.is_empty() {
                return Ok(());
            }
            self.drop_snapshots(dropped)?;
        }
    }

    fn push_link(
        &mut self,
        actions: &[FaultAction],
        point: Point,
        lease: Option<Lease>,
    ) -> Result<Point, String> {
        let evicted = self.chain.push(actions, point, lease);
        self.drop_snapshots(evicted.into_iter().collect())?;
        Ok(point)
    }

    fn publish(
        &mut self,
        actions: &[FaultAction],
        snap: SnapId,
        cost: u64,
    ) -> Result<Option<Lease>, String> {
        let Some(shared) = self.chain.shared() else {
            return Ok(None);
        };
        let publish_started = started();
        let published = self.session.publish_snapshot(
            shared.index.as_ref(),
            shared.namespace,
            &actions_key(actions),
            self.chain.parent_for(actions.len()),
            snap,
            cost,
        );
        self.telemetry.publishes.since(publish_started);
        match published {
            Ok(lease) => Ok(Some(lease)),
            Err(error)
                if matches!(
                    error.downcast_ref::<CacheError>(),
                    Some(CacheError::Refused { .. })
                ) =>
            {
                self.telemetry.refusals = self.telemetry.refusals.saturating_add(1);
                Ok(None)
            }
            Err(error) => Err(format!("publish snapshot: {error}")),
        }
    }

    fn seal_link(
        &mut self,
        actions: &[FaultAction],
        snap: SnapId,
        moment: u64,
        from: u64,
    ) -> Result<Point, String> {
        let lease = self.publish(actions, snap, moment.saturating_sub(from))?;
        let point = self.push_link(actions, Point { snap, moment }, lease)?;
        self.fit_store(2)?;
        Ok(point)
    }

    fn find(&mut self, actions: &[FaultAction]) -> Result<usize, String> {
        let at = self.chain.local(actions);
        let local = self.chain.depth(at);
        if local == actions.len() {
            return Ok(at);
        }
        let Some(lease) = self.chain.deeper_shared(actions, local) else {
            return Ok(at);
        };
        let index = self
            .chain
            .shared()
            .map(|shared| Arc::clone(&shared.index))
            .ok_or("a shared lease without a shared cache")?;
        let import_started = started();
        let near = self.chain.point(at).snap;
        let imported = self.session.import_cached(index.as_ref(), &lease, near);
        self.telemetry.imports.since(import_started);
        let (snap, moment) = match imported {
            Ok(imported) => imported,
            Err(error) => {
                self.chain.release(lease);
                return Err(format!("import snapshot: {error}"));
            }
        };
        let dropped = self.chain.truncate(at.saturating_add(1));
        self.drop_snapshots(dropped)?;
        let len = Chain::actions_in(&lease);
        self.push_link(&actions[..len], Point { snap, moment }, Some(lease))?;
        Ok(self.chain.links() - 1)
    }

    fn ensure_prefix(
        &mut self,
        actions: &[FaultAction],
    ) -> Result<Result<Point, FaultObservations>, String> {
        let at = self.find(actions)?;
        let start = self.chain.depth(at);
        let counter = match Hit::of(start, actions.len()) {
            Hit::Exact => &mut self.telemetry.exact_hits,
            Hit::Ancestor => &mut self.telemetry.ancestor_hits,
            Hit::Miss => &mut self.telemetry.misses,
        };
        *counter = counter.saturating_add(1);
        let dropped = self.chain.truncate(at.saturating_add(1));
        self.drop_snapshots(dropped)?;
        let mut last = self.chain.point(at);
        for index in start..actions.len() {
            self.branch(last, &actions[..=index])?;
            let (observation, sealed) =
                self.run_action(actions, index, last.moment, RunKind::Rebuild)?;
            let Some((snap, moment)) = sealed else {
                return Ok(Err(observation));
            };
            last = self.seal_link(&actions[..=index], snap, moment, last.moment)?;
        }
        Ok(Ok(last))
    }

    fn advance(
        &mut self,
        prefix: &[FaultAction],
        action: FaultAction,
        kind: RunKind,
    ) -> Result<FaultObservations, String> {
        let mut next = prefix.to_vec();
        next.push(action);
        let at = self.find(&next)?;
        if self.chain.depth(at) == next.len() {
            self.telemetry.exact_hits = self.telemetry.exact_hits.saturating_add(1);
            let snap = self.chain.point(at).snap;
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
        let (observation, sealed) = self.run_action(&next, prefix.len(), parent.moment, kind)?;
        if let Some((snap, moment)) = sealed {
            self.seal_link(&next, snap, moment, parent.moment)?;
        }
        Ok(observation)
    }

    fn branch(&mut self, parent: Point, actions: &[FaultAction]) -> Result<(), String> {
        let config = branch_config(self.windows, actions)
            .map_err(|error| format!("branch configuration: {error}"))?;
        let branch_started = started();
        let result = self
            .session
            .branch_with_service(parent.snap, config, Vec::new(), Vec::new())
            .map_err(|error| format!("branch: {error}"));
        self.telemetry.branches.since(branch_started);
        result
    }

    fn run_action(
        &mut self,
        actions: &[FaultAction],
        index: usize,
        from: u64,
        kind: RunKind,
    ) -> Result<(FaultObservations, Option<(SnapId, u64)>), String> {
        let (_, deadline) = self.windows.window(actions, index)?;
        self.horizons_run = self.horizons_run.saturating_add(1);
        let run_started = started();
        let stop = match self.session.run_until(deadline) {
            Ok(stop) => stop,
            Err(error) => return Err(self.abandon("run", error.as_ref())),
        };
        let runs = match kind {
            RunKind::New => &mut self.telemetry.new,
            RunKind::Rebuild => &mut self.telemetry.rebuild,
            RunKind::Replay => &mut self.telemetry.replay,
        };
        runs.host.since(run_started);
        runs.virtual_nanos = runs
            .virtual_nanos
            .saturating_add(stop_moment(&stop).saturating_sub(from));
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
            let seal_started = started();
            let (snapshot, at) = self
                .session
                .snapshot()
                .map_err(|error| self.abandon("snapshot", error.as_ref()))?;
            self.telemetry.seals.since(seal_started);
            Some((snapshot, at))
        } else {
            None
        };
        let observation = match self.observe(stop) {
            Ok(observation) => observation,
            Err(error) => {
                let error: Box<dyn Error> = error.into();
                return Err(self.abandon("observe", error.as_ref()));
            }
        };
        Ok((observation, snap))
    }

    fn observe(&mut self, stop: FaultStop) -> Result<FaultObservations, String> {
        let observe_started = started();
        let events = self
            .session
            .sdk_events()
            .map_err(|error| format!("SDK events: {error}"));
        self.telemetry.observations.since(observe_started);
        let events = events?;
        let moment = events.last().map_or(0, |(moment, _, _)| *moment);
        let capture = decode_sdk_events(&events)?;
        capture.check_infrastructure_status()?;
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
                .assertions
                .0
                .iter()
                .map(|(id, outcome)| {
                    size_of::<String>()
                        + id.len()
                        + outcome.message.len()
                        + outcome.location.len()
                        + size_of::<crate::assertion::AssertionOutcome>()
                })
                .sum::<usize>(),
        )
        .saturating_add(snapshot.observation.check.as_ref().map_or(0, |check| {
            check
                .points
                .iter()
                .map(|point| size_of::<String>() + point.capacity())
                .sum::<usize>()
        }))
        .saturating_add(
            snapshot
                .observation
                .parks
                .capacity()
                .saturating_mul(size_of::<crate::target::ParkLanding>()),
        )
        .saturating_add(
            snapshot
                .observation
                .park_reads
                .capacity()
                .saturating_mul(size_of::<crate::target::ParkRead>()),
        )
}

#[must_use]
pub fn identity(kernel: &[u8], initramfs: &[u8], config: &FaultConfig) -> String {
    let base = base_identity(kernel, initramfs, config);
    match &config.root {
        Some(root) => format!("{base};root={:x}", Sha256::digest(root.as_slice())),
        None => base,
    }
}

fn base_identity(kernel: &[u8], initramfs: &[u8], config: &FaultConfig) -> String {
    if let Some(guest) = config.uml() {
        return format!(
            "faults-uml-v1;{};executable={:x};initramfs={:x};memory_mib={};arguments={};\
             seed={SEED};setup_budget={SETUP_BUDGET};tag={IDENTITY_TAG};\
             action=standing-fault-delta-v3;snapshot=uml-checkpoint-v1",
            guest.identity,
            Sha256::digest(kernel),
            Sha256::digest(initramfs),
            config.ram_mib,
            UmlGuest::kernel_arguments(&config.knobs).join(" "),
        );
    }
    format!(
        "faults-consonance-whole-vm-v2;session={};\
         action=standing-fault-delta-v3;snapshot=portable-prefix-to-vm-snapshot-v1",
        identity_with_config(kernel, initramfs, &config.session_config()),
    )
}

pub fn from_paths(
    kernel: &Path,
    initramfs: &Path,
    config: &FaultConfig,
) -> Result<FaultTarget, String> {
    let kernel = std::fs::read(kernel).map_err(|error| format!("read kernel: {error}"))?;
    let initramfs = std::fs::read(initramfs).map_err(|error| format!("read initramfs: {error}"))?;
    FaultTarget::new(&kernel, &initramfs, config, None, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::FaultOperation;

    fn unbooted_target() -> FaultTarget {
        FaultTarget {
            config: Arc::new(Config {
                root: None,
                key: [0; 32],
                kernel: Vec::new(),
                initramfs: Vec::new(),
                session: FaultConfig {
                    knobs: Vec::new(),
                    ram_mib: DEFAULT_RAM_MIB,
                    backend: GuestBackend::Vm,
                    root: None,
                }
                .session_config(),
                cache: None,
                worker: None,
                uml: None,
            }),
            actions: Vec::new(),
            observation: FaultObservations::default(),
            action_observations: Vec::new(),
            failed: false,
            watchdog_cutoffs: 0,
            execution_ticks: 17,
            guest_horizons_run: 0,
            root_seal: 0,
        }
    }

    #[test]
    fn debug_service_is_portable_and_root_identity_includes_saved_state() {
        let standing = branch_config(ActionWindows { root_seal: 1000 }, &[]).unwrap();
        let message = process_proto::debug::Message {
            id: 9,
            operation: process_proto::debug::Operation::Input,
            data: b"echo debug\n".to_vec(),
        };
        let service = debug_configuration(&standing.configuration, Some(&message)).unwrap();
        let decoded = DebugHandler::decode(&service.configuration).unwrap();
        assert_eq!(decoded.message, message.encode().unwrap());
        assert!(service_factory()(&service).is_ok());
        assert!(DebugHandler::decode(&[255, 255, 255, 255]).is_err());
        let mut config = FaultConfig {
            knobs: Vec::new(),
            ram_mib: DEFAULT_RAM_MIB,
            backend: GuestBackend::Vm,
            root: None,
        };
        let base = identity(b"kernel", b"initramfs", &config);
        config.root = Some(Arc::new(vec![1]));
        let first = identity(b"kernel", b"initramfs", &config);
        config.root = Some(Arc::new(vec![2]));
        assert_ne!(first, base);
        assert_ne!(first, identity(b"kernel", b"initramfs", &config));
    }

    #[test]
    fn reconstruction_cutoff_discards_stale_evidence_and_allows_a_later_restore() {
        let mut target = unbooted_target();
        let snapshot = FaultSnapshot {
            actions: vec![FaultOperation::Wait(std::num::NonZeroU16::new(32768).unwrap()).into()],
            observation: FaultObservations {
                ticks: 123,
                ..FaultObservations::default()
            },
            failed: false,
        };
        target.observation.stop = FaultStop::Crash;
        target.action_observations.push(target.observation.clone());
        target
            .finish_restore(&snapshot, Err(format!("{WATCHDOG_CUTOFF}run: expired")))
            .unwrap();
        assert!(target.failed());
        assert!(!target.found_bug());
        assert!(target.snapshot().is_none());
        assert!(target.actions.is_empty());
        assert_eq!(target.watchdog_cutoffs(), 1);
        target.apply(FaultOperation::Kill(0, std::num::NonZeroU16::new(50).unwrap()).into());
        assert_eq!(target.execution_ticks(), 17);
        assert_eq!(target.last_action_observations().len(), 1);
        assert!(target.last_action_observations()[0].watchdog_cutoff);
        assert!(!target.last_action_observations()[0].is_bug());
        assert_eq!(target.last_action_observations()[0].ticks, 0);
        target.finish_restore(&snapshot, Ok(None)).unwrap();
        assert!(!target.failed());
        assert_eq!(target.watchdog_cutoffs(), 0);
        assert_eq!(target.observation(), &snapshot.observation);
        assert_eq!(target.snapshot().unwrap().actions, snapshot.actions);
        assert!(!target.last_action_observations()[0].watchdog_cutoff);
    }

    #[test]
    fn run_and_snapshot_watchdogs_share_the_recoverable_classification() {
        let snapshot = FaultSnapshot {
            actions: Vec::new(),
            observation: FaultObservations::default(),
            failed: false,
        };
        for phase in ["run", "snapshot"] {
            for error in [SessionError::Hung(WALL_LIMIT), SessionError::Abandoned] {
                let error: Box<dyn Error> = Box::new(error);
                let mut target = unbooted_target();
                target
                    .finish_restore(&snapshot, Err(session_failure(phase, error.as_ref())))
                    .unwrap();
                assert!(target.failed());
                assert_eq!(target.watchdog_cutoffs(), 1);
                assert!(target.last_action_observations()[0].watchdog_cutoff);
            }
            assert!(
                !session_failure(phase, &SessionError::Unboundable).starts_with(WATCHDOG_CUTOFF)
            );
        }
    }

    #[test]
    fn observation_strings_do_not_become_typed_watchdog_errors() {
        let error: Box<dyn Error> = SessionError::Hung(WALL_LIMIT).to_string().into();
        assert!(!session_failure("observe", error.as_ref()).starts_with(WATCHDOG_CUTOFF));
    }

    #[test]
    fn other_reconstruction_errors_remain_fatal() {
        let mut target = unbooted_target();
        let snapshot = FaultSnapshot {
            actions: Vec::new(),
            observation: FaultObservations::default(),
            failed: false,
        };
        let error = target
            .finish_restore(&snapshot, Err("replay: invalid snapshot".into()))
            .unwrap_err();
        assert_eq!(error.to_string(), "replay: invalid snapshot");
        assert_eq!(target.watchdog_cutoffs(), 0);
    }

    #[test]
    fn coverage_windows_select_the_recorded_action_quantum_and_reject_gaps() {
        let actions = [
            FaultAction::new(
                FaultOperation::Wait(std::num::NonZeroU16::new(2).unwrap()),
                std::num::NonZeroU16::new(64).unwrap(),
            ),
            FaultAction::new(
                FaultOperation::Wait(std::num::NonZeroU16::new(3).unwrap()),
                std::num::NonZeroU16::new(8).unwrap(),
            ),
        ];
        let windows = ActionWindows { root_seal: 1_000 };
        let configuration = branch_config(windows, &actions).unwrap();
        let mut handler =
            StandingHandler::from_configuration(&configuration.configuration).unwrap();
        let question = Question::with_request_id(
            7,
            environment::channel::SERVICE_COVERAGE_QUANTUM,
            1_u64.to_le_bytes().to_vec(),
        )
        .unwrap();
        for (moment, expected) in [
            (999, environment::channel::DEFAULT_COVERAGE_QUANTUM),
            (1_000, 64),
            (20_001_000, 8),
            (50_001_000, environment::channel::DEFAULT_COVERAGE_QUANTUM),
        ] {
            assert_eq!(
                handler.respond(moment, &question).unwrap(),
                ServiceResponse::Answered(ChannelAnswer::Data(expected.to_le_bytes().to_vec()))
            );
        }
        let (standing, mut gap) = decode_configuration(&configuration.configuration).unwrap();
        gap[1].0 += 1;
        let gap = encode_configuration(standing, &gap).unwrap();
        assert!(StandingHandler::from_configuration(&gap).is_err());
        let truncated = &configuration.configuration[..configuration.configuration.len() - 1];
        assert!(StandingHandler::from_configuration(truncated).is_err());
    }

    #[test]
    fn the_factory_serves_the_nominal_service_the_setup_point_was_sealed_under() {
        let factory = service_factory();
        let handler = factory(&ServiceConfig::default()).expect("nominal service");
        assert_eq!(handler.identity(), ServiceConfig::default().identity);
        let own = factory(&ServiceConfig {
            identity: SERVICE_IDENTITY.to_vec(),
            configuration: encode_configuration(
                &encode_windows(&[]).expect("an empty standing list encodes"),
                &[],
            )
            .unwrap(),
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

fn export_root(
    session: &mut dyn SearchSession,
    snapshot: SnapId,
    identity: [u8; 32],
) -> Result<Vec<u8>, Box<dyn Error>> {
    use consonance_client::cache::LocalIndex;
    let index = LocalIndex::new(usize::MAX);
    let lease = session.publish_root(&index, Namespace::new(&[&identity]), snapshot)?;
    let chain = index.chain(&lease)?;
    if chain.len() != 1 {
        return Err("branch root must be a complete snapshot extent".into());
    }
    let mut bytes = b"HMROOT01".to_vec();
    bytes.extend_from_slice(&identity);
    bytes.extend_from_slice(chain[0].bytes());
    Ok(bytes)
}
fn import_root(
    session: &mut dyn SearchSession,
    bytes: &[u8],
) -> Result<(SnapId, u64), Box<dyn Error>> {
    use consonance_client::cache::LocalIndex;
    if bytes.len() < 40 || &bytes[..8] != b"HMROOT01" {
        return Err("invalid branch snapshot header".into());
    }
    if bytes[8..40] != session.root_identity()? {
        return Err("branch snapshot setup identity differs".into());
    }
    let index = LocalIndex::new(usize::MAX);
    let mut extent = index.extent(bytes.len() - 40)?;
    extent.bytes_mut().copy_from_slice(&bytes[40..]);
    let lease = index.publish(Namespace::new(&[&bytes[8..40]]), b"root", None, extent, 1)?;
    session.import_root(&index, &lease)
}

#[derive(Debug)]
pub struct DebugProgress {
    pub status: Option<process_proto::debug::Status>,
    pub output: Vec<u8>,
}
const DEBUG_IDENTITY: &[u8] = b"faults-debug-terminal-v1";
#[derive(Clone, Debug)]
struct DebugHandler {
    configuration: Vec<u8>,
    standing: StandingHandler,
    message: Vec<u8>,
}
impl DebugHandler {
    fn decode(bytes: &[u8]) -> Result<Self, ChannelError> {
        let (size, rest) = bytes
            .split_first_chunk::<4>()
            .ok_or(ChannelError::Malformed)?;
        let n = u32::from_le_bytes(*size) as usize;
        let (standing, message) = rest.split_at_checked(n).ok_or(ChannelError::Malformed)?;
        if !message.is_empty() {
            process_proto::debug::Message::decode(message).map_err(|_| ChannelError::Malformed)?;
        }
        Ok(Self {
            configuration: bytes.to_vec(),
            standing: StandingHandler::from_configuration(standing)?,
            message: message.to_vec(),
        })
    }
}
impl ServiceHandler for DebugHandler {
    fn identity(&self) -> &[u8] {
        DEBUG_IDENTITY
    }
    fn configuration(&self) -> &[u8] {
        &self.configuration
    }
    fn respond(
        &mut self,
        moment: Moment,
        question: &Question,
    ) -> Result<ServiceResponse, ChannelError> {
        if question.service() == process_proto::debug::NAMESPACE {
            process_proto::debug::Status::decode(question.payload())
                .map_err(|_| ChannelError::Malformed)?;
            return Ok(ServiceResponse::Answered(ChannelAnswer::data(
                self.message.clone(),
            )?));
        }
        self.standing.respond(moment, question)
    }
    fn snapshot_state(&self) -> Result<Vec<u8>, ChannelError> {
        Ok(Vec::new())
    }
    fn restore_state(&mut self, state: &[u8]) -> Result<(), ChannelError> {
        if state.is_empty() {
            Ok(())
        } else {
            Err(ChannelError::Malformed)
        }
    }
    fn clone_box(&self) -> Box<dyn ServiceHandler> {
        Box::new(self.clone())
    }
}
fn debug_configuration(
    standing: &[u8],
    message: Option<&process_proto::debug::Message>,
) -> Result<ServiceConfig, Box<dyn Error>> {
    let mut configuration = u32::try_from(standing.len())?.to_le_bytes().to_vec();
    configuration.extend_from_slice(standing);
    if let Some(message) = message {
        configuration.extend(message.encode()?);
    }
    Ok(ServiceConfig {
        identity: DEBUG_IDENTITY.to_vec(),
        configuration,
    })
}
