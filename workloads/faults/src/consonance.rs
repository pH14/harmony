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
use control_proto::{Resolution, SnapId, StopReason};
use environment::{
    Moment,
    channel::{
        Answer as ChannelAnswer, ChannelError, CoverageSchedule, Question, ServiceHandler,
        ServiceResponse,
    },
    input_spec::{ServiceConfig, ServiceFactory, nominal_factory},
};
use fault_policy::{STANDING_NAMESPACE, StandingWindow, encode_standing, encode_windows};
use searcher::target::ExitKind;
use sha2::{Digest, Sha256};

use crate::chain::{Chain, Hit, Point, Shared};
use crate::target::{
    ActionWindows, FaultAction, FaultEndpointCertificate, FaultObservations, FaultSnapshot,
    FaultStop, actions_key, decode_sdk_events, standing_windows,
};

pub const DEFAULT_RAM_MIB: u32 = 1024;
pub const SERVICE_IDENTITY: &[u8] = b"faults-standing-coverage-v2";
pub const SESSION_SERVICE: &str = "faults";
const SEED: u64 = 0x4661_756c_744c_6162;
const SETUP_BUDGET: u64 = 120_000_000_000;
const WALL_LIMIT: Duration = Duration::from_secs(60);
const CONSOLE_TAIL: usize = 1_500;
const WATCHDOG_CUTOFF: &str = "fault-guest-watchdog-cutoff: ";

pub fn replay_endpoint_certificate(
    hash: [u8; 32],
    observation: &FaultObservations,
    failed: bool,
) -> Result<[u8; 32], String> {
    let encoded = postcard::to_allocvec(&("fault-replay-endpoint-v1", hash, observation, failed))
        .map_err(|error| format!("encode fault replay endpoint: {error}"))?;
    Ok(Sha256::digest(encoded).into())
}

pub(crate) fn validate_snapshot_prefix_certificates(
    snapshot: &FaultSnapshot,
    uml: bool,
) -> Result<(), String> {
    if snapshot.prefix_certificates.len() != snapshot.actions.len() {
        return Err("fault snapshot prefix certificate count does not match its actions".into());
    }
    if uml {
        if snapshot.endpoint != Some(FaultEndpointCertificate::UmlWholeStateUnavailable) {
            return Err("UML snapshot lacks its backend endpoint certificate".into());
        }
        if snapshot.prefix_certificates.iter().any(Option::is_some) {
            return Err("UML snapshot unexpectedly carries KVM prefix certificates".into());
        }
        return Ok(());
    }
    let Some(FaultEndpointCertificate::KvmWholeState(hash)) = snapshot.endpoint else {
        return Err("KVM snapshot lacks its whole-state endpoint certificate".into());
    };
    if snapshot.prefix_certificates.iter().any(Option::is_none) {
        return Err("KVM snapshot prefix lacks an endpoint certificate".into());
    }
    if let Some(&Some(actual)) = snapshot.prefix_certificates.last() {
        let expected = replay_endpoint_certificate(hash, &snapshot.observation, snapshot.failed)?;
        if actual != expected {
            return Err("KVM snapshot final prefix differs from its endpoint certificate".into());
        }
    }
    Ok(())
}

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
    Uml(Box<UmlGuest>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UmlGuest {
    pub(crate) profile: PathBuf,
    pub(crate) profile_identity_sha256: String,
    pub(crate) host: uml::HostIdentity,
    pub(crate) identity: String,
    pub(crate) work_parent: PathBuf,
}

impl UmlGuest {
    pub fn load(profile: &Path, work_parent: PathBuf) -> Result<Self, String> {
        let verified = uml::Profile::load(profile)
            .map_err(|error| format!("User-mode Linux profile: {error}"))?;
        let host = Self::default_host()?;
        let executable = consonance_client::image_identity::loaded_executable_identity_self()
            .map_err(|error| format!("loaded Harmony executable: {error}"))?;
        let identity = Self::identity(verified.identity_sha256(), &host, &executable);
        Ok(Self {
            profile: profile.to_path_buf(),
            profile_identity_sha256: verified.identity_sha256().to_owned(),
            host,
            identity,
            work_parent,
        })
    }

    fn default_host() -> Result<uml::HostIdentity, String> {
        #[cfg(target_os = "linux")]
        {
            let pool = consonance_client::placement::CorePool::detect()?;
            let processor = pool
                .plan(1)?
                .coordinator
                .ok_or("User-mode Linux needs a coordinator processor")?;
            uml::HostIdentity::for_processor(processor)
                .map_err(|error| format!("User-mode Linux host identity: {error}"))
        }
        #[cfg(not(target_os = "linux"))]
        {
            uml::HostIdentity::current()
                .map_err(|error| format!("User-mode Linux host identity: {error}"))
        }
    }

    fn identity(profile: &str, host: &uml::HostIdentity, executable: &str) -> String {
        format!(
            "profile={profile};host={};host_executable={executable}",
            host.identity_sha256()
        )
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn verify_processor(&self, processor: usize) -> Result<(), String> {
        let current = uml::HostIdentity::for_processor(processor)
            .map_err(|error| format!("User-mode Linux host processor {processor}: {error}"))?;
        if current != self.host {
            return Err(format!(
                "User-mode Linux host identity changed after admission: expected {:?}, current {current:?}",
                self.host
            ));
        }
        Ok(())
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
            GuestBackend::Uml(guest) => Some(guest.as_ref()),
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

const IDENTITY_TAG: &str = "faults-consonance-execution-v8";

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
    inherited: &[StandingWindow],
    actions: &[FaultAction],
) -> Result<ServiceConfig, Box<dyn Error>> {
    let mut standing = inherited.to_vec();
    standing.extend(standing_windows(windows, actions)?);
    Ok(ServiceConfig {
        identity: SERVICE_IDENTITY.to_vec(),
        configuration: encode_configuration(
            &encode_windows(&standing)?,
            &coverage_windows(windows, actions)?,
        )?,
    })
}

fn coverage_windows(
    windows: ActionWindows,
    actions: &[FaultAction],
) -> Result<Vec<CoverageWindow>, String> {
    actions
        .iter()
        .enumerate()
        .map(|(index, action)| {
            let (start, end) = windows.window(actions, index)?;
            Ok((start, end, action.coverage_quantum))
        })
        .collect()
}

fn branch_coverage_schedule(
    windows: ActionWindows,
    actions: &[FaultAction],
) -> Result<CoverageSchedule, String> {
    let rows = coverage_windows(windows, actions)?
        .into_iter()
        .map(|(start, end, quantum)| (start, end, u64::from(quantum.get())))
        .collect();
    CoverageSchedule::new(environment::channel::DEFAULT_COVERAGE_QUANTUM, rows)
        .map_err(|error| error.to_string())
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
    if path.exists() {
        let staged =
            std::fs::read(&path).map_err(|error| format!("read staged initramfs: {error}"))?;
        if staged != initramfs {
            return Err("staged initramfs differs from the requested image".into());
        }
    } else {
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
        expected_profile_identity_sha256: guest.profile_identity_sha256.clone(),
        expected_host: guest.host.clone(),
        initramfs: path,
        expected_initramfs_sha256: digest.into(),
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
    inherited: Vec<StandingWindow>,
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
    prefix_certificates: Vec<Option<[u8; 32]>>,
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
            key: Sha256::digest(identity(kernel, initramfs, config)?).into(),
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
            prefix_certificates: Vec::new(),
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

    pub fn state_hash_current(&self) -> Result<[u8; 32], String> {
        LIVE.with(|slot| {
            let mut slot = slot.borrow_mut();
            let live = slot
                .as_mut()
                .ok_or("the current job session is unavailable")?;
            if live.key != self.config.key {
                return Err("the current job session has a different identity".to_owned());
            }
            if live.abandoned || live.session.abandoned() {
                return Err("the current job session was abandoned".to_owned());
            }
            let result = live
                .session
                .state_hash()
                .map_err(|error| format!("current job state hash: {error}"));
            if live.abandoned || live.session.abandoned() {
                *slot = None;
                return Err("the current job session was abandoned while hashing".to_owned());
            }
            result
        })
    }

    pub fn verify_replayed_endpoint(&self, certificate: Option<[u8; 32]>) -> Result<(), String> {
        if self.failed || !self.observation.stop.is_continuable() {
            return Err("replayed fault prefix stopped before its archived endpoint".to_owned());
        }
        if self.config.uml.is_some() {
            if certificate.is_some() {
                return Err("UML replay unexpectedly carries a KVM endpoint certificate".to_owned());
            }
            return Ok(());
        }
        let expected = certificate.ok_or("KVM replay prefix lacks an endpoint certificate")?;
        let actual = replay_endpoint_certificate(
            self.state_hash_current()?,
            &self.observation,
            self.failed,
        )?;
        if actual != expected {
            return Err(
                "replayed KVM prefix differs from its archived endpoint certificate".to_owned(),
            );
        }
        Ok(())
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
            let standing = branch_config(live.windows, &live.inherited, &self.actions)
                .map_err(|e| e.to_string())?;
            let service =
                debug_configuration(&standing.configuration, message).map_err(|e| e.to_string())?;
            let mut handler =
                DebugHandler::decode(&service.configuration).map_err(|error| error.to_string())?;
            live.session
                .branch_with_service(
                    snapshot,
                    service,
                    Vec::new(),
                    Vec::new(),
                    Some(branch_coverage_schedule(live.windows, &self.actions)?),
                )
                .map_err(|e| e.to_string())?;
            live.session
                .drop_snapshot(snapshot)
                .map_err(|e| e.to_string())?;
            let stop = run_with_service_decisions(
                live.session.as_mut(),
                at.checked_add(10_000_000).ok_or("debug clock overflow")?,
                &mut handler,
            )
            .map_err(|error| error.to_string())?;
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
            let (snapshot, moment) = live.session.snapshot().map_err(|e| e.to_string())?;
            let result = active_root_windows(&live.inherited, live.windows, &self.actions, moment)
                .and_then(|standing| {
                    export_root(
                        live.session.as_mut(),
                        snapshot,
                        live.base_identity,
                        &standing,
                    )
                    .map_err(|e| e.to_string())
                });
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
                self.prefix_certificates.clear();
                self.guest_horizons_run = 0;
                self.observation = observation.clone();
                self.action_observations = vec![observation];
                self.failed = false;
                self.watchdog_cutoffs = 0;
            }
            Err(error) => {
                eprintln!("fault target reset failed: {error}");
                self.prefix_certificates.clear();
                self.failed = true;
                self.watchdog_cutoffs = u64::from(error.starts_with(WATCHDOG_CUTOFF));
            }
        }
    }

    pub fn verify_integrity(&self) -> Result<(), CacheError> {
        if let Some(cache) = &self.config.cache {
            cache.verify_integrity()?;
        }
        Ok(())
    }

    pub fn restore(&mut self, snapshot: &FaultSnapshot) -> Result<(), Box<dyn Error>> {
        self.verify_integrity()?;
        if snapshot.failed || !snapshot.observation.stop.is_continuable() {
            return Err("the fault snapshot is not a runnable endpoint".into());
        }
        let expected = match (self.config.uml.is_some(), snapshot.endpoint) {
            (false, Some(FaultEndpointCertificate::KvmWholeState(hash))) => Some(hash),
            (true, Some(FaultEndpointCertificate::UmlWholeStateUnavailable)) => None,
            _ => return Err("the fault snapshot lacks a certificate for its backend".into()),
        };
        validate_snapshot_prefix_certificates(snapshot, self.config.uml.is_some())?;
        let rebuilt = with_live(&self.config, |live| {
            live.fit_store(1)?;
            if expected.is_some() && snapshot.actions.len() > 1 {
                let root = live.chain.point(live.chain.local(&[]));
                live.replay(root.snap)?;
                let mut previous_moment = live.observe(FaultStop::Deadline)?.moment;
                for index in 0..snapshot.actions.len() - 1 {
                    let prefix = &snapshot.actions[..=index];
                    let at = live.chain.local(prefix);
                    let point = if live.chain.depth(at) == prefix.len() {
                        live.chain.point(at)
                    } else {
                        live.ensure_prefix(prefix)?.map_err(
                            |_| "restored fault prefix stopped before its snapshot certificate",
                        )?
                    };
                    live.replay(point.snap)?;
                    let hash = live
                        .session
                        .state_hash()
                        .map_err(|error| format!("restored fault prefix state hash: {error}"))?;
                    let mut observation = live.observe(FaultStop::Deadline)?;
                    observation
                        .parks
                        .retain(|park| park.moment > previous_moment);
                    observation
                        .park_reads
                        .retain(|read| read.moment > previous_moment);
                    let actual = replay_endpoint_certificate(hash, &observation, false)?;
                    if Some(actual) != snapshot.prefix_certificates[index] {
                        return Err(
                            "restored fault prefix differs from its snapshot certificate".into(),
                        );
                    }
                    previous_moment = observation.moment;
                }
            }
            let previous_moment = if snapshot.actions.is_empty() {
                None
            } else {
                let prefix = &snapshot.actions[..snapshot.actions.len() - 1];
                let at = live.chain.local(prefix);
                let previous = if live.chain.depth(at) == prefix.len() {
                    live.chain.point(at)
                } else {
                    live.ensure_prefix(prefix)?.map_err(
                        |_| "restored fault prefix stopped before its snapshot certificate",
                    )?
                };
                live.replay(previous.snap)?;
                Some(live.observe(FaultStop::Deadline)?.moment)
            };
            match live.ensure_prefix(&snapshot.actions)? {
                Ok(cached) => {
                    live.replay(cached.snap)?;
                    if let Some(expected) = expected {
                        let actual = live.session.state_hash().map_err(|error| {
                            format!("restored fault endpoint state hash: {error}")
                        })?;
                        if actual != expected {
                            return Err(
                                "restored fault endpoint differs from its snapshot certificate"
                                    .into(),
                            );
                        }
                    }
                    let mut actual = live.observe(FaultStop::Deadline)?;
                    if let Some(previous_moment) = previous_moment {
                        actual.parks.retain(|park| park.moment > previous_moment);
                        actual
                            .park_reads
                            .retain(|read| read.moment > previous_moment);
                    }
                    if actual != snapshot.observation {
                        return Err(
                            "restored fault endpoint observation differs from its snapshot".into(),
                        );
                    }
                    Ok(())
                }
                Err(_) => {
                    Err("restored fault endpoint stopped before its snapshot certificate".into())
                }
            }
        });
        self.finish_restore(snapshot, rebuilt)
    }

    fn finish_restore(
        &mut self,
        snapshot: &FaultSnapshot,
        rebuilt: Result<(), String>,
    ) -> Result<(), Box<dyn Error>> {
        self.action_observations.clear();
        self.watchdog_cutoffs = 0;
        match rebuilt {
            Ok(()) => {}
            Err(error) if error.starts_with(WATCHDOG_CUTOFF) => {
                self.actions.clear();
                self.prefix_certificates.clear();
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
        }
        self.actions.clone_from(&snapshot.actions);
        self.prefix_certificates
            .clone_from(&snapshot.prefix_certificates);
        self.observation = snapshot.observation.clone();
        self.action_observations = vec![self.observation.clone()];
        self.failed = snapshot.failed;
        self.watchdog_cutoffs = 0;
        Ok(())
    }

    pub fn snapshot(&self) -> Result<Option<FaultSnapshot>, String> {
        self.verify_integrity().map_err(|error| error.to_string())?;
        if self.failed || !self.observation.stop.is_continuable() {
            return Ok(None);
        }
        let endpoint = if self.config.uml.is_some() {
            FaultEndpointCertificate::UmlWholeStateUnavailable
        } else {
            FaultEndpointCertificate::KvmWholeState(self.state_hash_current()?)
        };
        let snapshot = FaultSnapshot {
            actions: self.actions.clone(),
            prefix_certificates: self.prefix_certificates.clone(),
            observation: self.observation.clone(),
            failed: false,
            endpoint: Some(endpoint),
        };
        validate_snapshot_prefix_certificates(&snapshot, self.config.uml.is_some())?;
        Ok(Some(snapshot))
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
        let previous_moment = self.observation.moment;
        let kvm = self.config.uml.is_none();
        let result = with_live(&self.config, |live| {
            let before = live.horizons_run;
            let mut observation = live.advance(&self.actions, action, kind)?;
            observation
                .parks
                .retain(|park| park.moment > previous_moment);
            observation
                .park_reads
                .retain(|read| read.moment > previous_moment);
            let certificate = if kvm && observation.stop.is_continuable() {
                let hash = live
                    .session
                    .state_hash()
                    .map_err(|error| format!("fault action state hash: {error}"))?;
                Some(replay_endpoint_certificate(hash, &observation, false)?)
            } else {
                None
            };
            Ok((
                observation,
                live.horizons_run.saturating_sub(before),
                certificate,
            ))
        });
        match result {
            Ok((observation, ran, certificate)) => {
                self.actions.push(action);
                self.prefix_certificates.push(certificate);
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
    if let Some(index) = &config.cache {
        index
            .verify_integrity()
            .map_err(|error| error.to_string())?;
    }
    let result = LIVE.with(|slot| {
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
    });
    if let Some(index) = &config.cache {
        index
            .verify_integrity()
            .map_err(|error| error.to_string())?;
    }
    result
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

fn resolution_for_stop(
    stop: &StopReason,
    handler: &mut dyn ServiceHandler,
) -> Result<Resolution, Box<dyn Error>> {
    let StopReason::Decision { vtime, id, ctx } = stop else {
        return Err("a service resolution requires a decision stop".into());
    };
    let (service, payload) = ctx
        .split_first_chunk::<2>()
        .ok_or("service decision context is missing its service id")?;
    let service = u16::from_le_bytes(*service);
    let question = Question::with_request_id(id.0, service, payload.to_vec())?;
    let answer = match handler.respond(vtime.0, &question)? {
        ServiceResponse::Answered(answer) => answer,
        ServiceResponse::External => {
            return Err(format!("service {service} did not answer the explicit decision").into());
        }
    };
    Ok(Resolution {
        vtime: *vtime,
        service,
        id: *id,
        answer: control_proto::Answer(answer.encode()),
    })
}

fn run_with_service_decisions(
    session: &mut dyn SearchSession,
    deadline: u64,
    handler: &mut dyn ServiceHandler,
) -> Result<StopReason, Box<dyn Error>> {
    let mut stop = session.run_until(deadline)?;
    while matches!(stop, StopReason::Decision { .. }) {
        if stop_moment(&stop) >= deadline {
            return Ok(StopReason::Deadline {
                vtime: control_proto::Moment(stop_moment(&stop)),
            });
        }
        let resolution = resolution_for_stop(&stop, handler)?;
        stop = session.run_resolving(deadline, resolution)?;
    }
    Ok(stop)
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
        let mut inherited = Vec::new();
        if let Some(root) = &config.root {
            (setup, root_seal, inherited) =
                import_root(session.as_mut(), root).map_err(|e| e.to_string())?;
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
            inherited,
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
        let result = self.session.replay_snapshot(snapshot);
        self.telemetry.restores.since(restore_started);
        result.map_err(|error| self.abandon("replay", error.as_ref()))
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
        if let Some(shared) = self.chain.shared() {
            shared
                .index
                .verify_namespace(shared.namespace)
                .map_err(|error| error.to_string())?;
        }
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
                return Err(self.abandon("import snapshot", error.as_ref()));
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
            let prefix = &actions[..=index];
            self.branch(last, prefix)?;
            let (observation, sealed) =
                self.run_action(prefix, index, last.moment, RunKind::Rebuild)?;
            let Some((snap, moment)) = sealed else {
                return Ok(Err(observation));
            };
            last = self.seal_link(prefix, snap, moment, last.moment)?;
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
        let config = branch_config(self.windows, &self.inherited, actions)
            .map_err(|error| format!("branch configuration: {error}"))?;
        let coverage_schedule = branch_coverage_schedule(self.windows, actions)
            .map_err(|error| format!("branch coverage schedule: {error}"))?;
        let branch_started = started();
        let result = self.session.branch_with_service(
            parent.snap,
            config,
            Vec::new(),
            Vec::new(),
            Some(coverage_schedule),
        );
        self.telemetry.branches.since(branch_started);
        result.map_err(|error| self.abandon("branch", error.as_ref()))
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
        let config = branch_config(self.windows, &self.inherited, actions)
            .map_err(|error| format!("decision handler configuration: {error}"))?;
        let mut handler = StandingHandler::from_configuration(&config.configuration)
            .map_err(|error| format!("decision handler: {error}"))?;
        let stop = match run_with_service_decisions(self.session.as_mut(), deadline, &mut handler) {
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
                .prefix_certificates
                .capacity()
                .saturating_mul(size_of::<Option<[u8; 32]>>()),
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

pub fn identity(kernel: &[u8], initramfs: &[u8], config: &FaultConfig) -> Result<String, String> {
    let base = base_identity(kernel, initramfs, config)?;
    Ok(identity_with_root(
        &base,
        config.root.as_deref().map(Vec::as_slice),
    ))
}

fn identity_with_root(base: &str, root: Option<&[u8]>) -> String {
    match root {
        Some(root) => format!("{base};root={:x}", Sha256::digest(root)),
        None => base.to_owned(),
    }
}

fn base_identity(kernel: &[u8], initramfs: &[u8], config: &FaultConfig) -> Result<String, String> {
    if let Some(guest) = config.uml() {
        let arguments = UmlGuest::kernel_arguments(&config.knobs);
        let mut arguments_digest = Sha256::new();
        arguments_digest.update(b"faults-uml-kernel-arguments-v1\0");
        arguments_digest.update((arguments.len() as u64).to_le_bytes());
        for argument in arguments {
            arguments_digest.update((argument.len() as u64).to_le_bytes());
            arguments_digest.update(argument.as_bytes());
        }
        return Ok(format!(
            "faults-uml-v2;{};executable={:x};initramfs={:x};memory_mib={};arguments-sha256={:x};\
             seed={SEED};setup_budget={SETUP_BUDGET};tag={IDENTITY_TAG};\
             action=standing-fault-delta-v4;snapshot=uml-checkpoint-v3",
            guest.identity,
            Sha256::digest(kernel),
            Sha256::digest(initramfs),
            config.ram_mib,
            arguments_digest.finalize(),
        ));
    }
    Ok(format!(
        "faults-consonance-whole-vm-v2;session={};\
         action=standing-fault-delta-v4;snapshot=portable-prefix-to-vm-snapshot-v3",
        identity_with_config(kernel, initramfs, &config.session_config())?,
    ))
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
    use consonance_client::cache::{
        LocalIndex,
        extent::{extent_len, read_extent, write_extent},
    };

    #[test]
    fn uml_launch_carries_the_admitted_profile_identity() {
        let directory = tempfile::tempdir().unwrap();
        let guest = UmlGuest {
            profile: directory.path().join("profile"),
            profile_identity_sha256: "admitted-profile".into(),
            host: uml::HostIdentity {
                architecture: "x86_64".into(),
                processor: 0,
                cpu_model: "model".into(),
                cpu_features: "features".into(),
                boot_cpuinfo: "flags: features\n".into(),
                auxv: uml::HostAuxv::default(),
            },
            identity: "admitted-host".into(),
            work_parent: directory.path().to_path_buf(),
        };
        let config = FaultConfig {
            knobs: Vec::new(),
            ram_mib: DEFAULT_RAM_MIB,
            backend: GuestBackend::Uml(Box::new(guest.clone())),
            root: None,
        };
        let launch = uml_launch(&guest, b"initramfs", &config).unwrap();
        assert_eq!(launch.expected_profile_identity_sha256, "admitted-profile");
        assert_eq!(launch.expected_host, guest.host);
        assert_eq!(std::fs::read(&launch.initramfs).unwrap(), b"initramfs");
        std::fs::write(&launch.initramfs, b"stale initramfs").unwrap();
        assert!(uml_launch(&guest, b"initramfs", &config).is_err());
        let mut foreign_boot = guest.host.clone();
        foreign_boot.boot_cpuinfo.push_str("cache_alignment: 128\n");
        assert_ne!(
            UmlGuest::identity("admitted-profile", &guest.host, "loaded-a"),
            UmlGuest::identity("admitted-profile", &foreign_boot, "loaded-a")
        );
        assert_ne!(
            UmlGuest::identity("admitted-profile", &guest.host, "loaded-a"),
            UmlGuest::identity("admitted-profile", &guest.host, "loaded-b")
        );
    }

    #[test]
    fn uml_identity_distinguishes_kernel_argument_boundaries() {
        let guest = UmlGuest {
            profile: PathBuf::from("profile"),
            profile_identity_sha256: "admitted-profile".into(),
            host: uml::HostIdentity {
                architecture: "x86_64".into(),
                processor: 0,
                cpu_model: "model".into(),
                cpu_features: "features".into(),
                boot_cpuinfo: "flags: features\n".into(),
                auxv: uml::HostAuxv::default(),
            },
            identity: "admitted-host".into(),
            work_parent: PathBuf::from("work"),
        };
        let mut joined = FaultConfig {
            knobs: vec!["foo=bar root=/dev/ram0".into()],
            ram_mib: DEFAULT_RAM_MIB,
            backend: GuestBackend::Uml(Box::new(guest)),
            root: None,
        };
        let split = FaultConfig {
            knobs: vec!["foo=bar".into(), "root=/dev/ram0".into()],
            ..joined.clone()
        };
        assert_eq!(
            UmlGuest::kernel_arguments(&joined.knobs).join(" "),
            UmlGuest::kernel_arguments(&split.knobs).join(" ")
        );
        assert_ne!(
            identity(b"kernel", b"initramfs", &joined),
            identity(b"kernel", b"initramfs", &split)
        );
        joined.knobs = split.knobs.clone();
        assert_eq!(
            identity(b"kernel", b"initramfs", &joined),
            identity(b"kernel", b"initramfs", &split)
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn uml_replay_and_search_admit_the_same_coordinator_processor() {
        let pool = consonance_client::placement::CorePool::detect().unwrap();
        let replay = UmlGuest::default_host().unwrap();
        for workers in 1..=pool.max_workers() {
            let planned = pool.plan(workers as u32).unwrap();
            assert_eq!(planned.coordinator, Some(replay.processor));
        }
    }

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
            prefix_certificates: Vec::new(),
            observation: FaultObservations::default(),
            action_observations: Vec::new(),
            failed: false,
            watchdog_cutoffs: 0,
            execution_ticks: 17,
            guest_horizons_run: 0,
            root_seal: 0,
        }
    }

    #[derive(Debug)]
    struct TimedOutRestore {
        hash: Option<[u8; 32]>,
        events: BTreeMap<u64, Vec<consonance_client::session::SdkEvent>>,
        current: u64,
        run_hashes: Option<Vec<[u8; 32]>>,
        run_hung: bool,
        snapshots: BTreeMap<u64, [u8; 32]>,
        moment: u64,
        next_snap: u64,
    }

    impl SearchSession for TimedOutRestore {
        fn setup_handle(&self) -> (SnapId, u64) {
            (SnapId(1), 0)
        }

        fn state_hash(&mut self) -> Result<[u8; 32], Box<dyn Error>> {
            self.hash.ok_or_else(|| "state hash unavailable".into())
        }

        fn console_tail(&mut self) -> Result<Vec<u8>, Box<dyn Error>> {
            Ok(Vec::new())
        }

        fn telemetry_counters(&self) -> Vec<(String, u64)> {
            Vec::new()
        }

        fn snapshot_owned_pages(&self, _: SnapId) -> Option<u64> {
            None
        }

        fn store_bytes(&self) -> Option<u64> {
            None
        }

        fn publish_snapshot(
            &self,
            index: &dyn CacheIndex,
            namespace: Namespace,
            key: &[u8],
            parent: Option<(SnapId, &Lease)>,
            target: SnapId,
            cost: u64,
        ) -> Result<Lease, Box<dyn Error>> {
            let target_commitment = *self.snapshots.get(&target.0).ok_or("unknown snapshot")?;
            let sidecar = target.0.to_le_bytes();
            let mut extent = index.extent(extent_len(0, 0, sidecar.len()).unwrap())?;
            write_extent(extent.bytes_mut(), &[], &[], &sidecar)?;
            Ok(index.publish(
                namespace,
                key,
                parent.map(|(_, lease)| lease),
                extent,
                target_commitment,
                cost,
            )?)
        }

        fn import_cached(
            &mut self,
            _: &dyn CacheIndex,
            _: &Lease,
            _: SnapId,
        ) -> Result<(SnapId, u64), Box<dyn Error>> {
            Err(SessionError::Hung(WALL_LIMIT).into())
        }

        fn replay_snapshot(&mut self, snapshot: SnapId) -> Result<(), Box<dyn Error>> {
            if self.hash.is_some() {
                if self.run_hashes.is_some() {
                    self.hash = self.snapshots.get(&snapshot.0).copied();
                }
                self.current = snapshot.0;
                Ok(())
            } else {
                Err(SessionError::Hung(WALL_LIMIT).into())
            }
        }

        fn drop_snapshot(&mut self, snapshot: SnapId) -> Result<(), Box<dyn Error>> {
            self.snapshots.remove(&snapshot.0);
            Ok(())
        }

        fn branch_with_service(
            &mut self,
            _: SnapId,
            _: ServiceConfig,
            _: Vec<Vec<u8>>,
            _: Vec<(u64, environment::channel::Effect)>,
            _: Option<CoverageSchedule>,
        ) -> Result<(), Box<dyn Error>> {
            if self.hash.is_some() {
                Ok(())
            } else {
                Err(SessionError::Hung(WALL_LIMIT).into())
            }
        }

        fn run_until(&mut self, deadline: u64) -> Result<StopReason, Box<dyn Error>> {
            if self.run_hung && self.run_hashes.as_ref().is_none_or(Vec::is_empty) {
                return Err(SessionError::Hung(WALL_LIMIT).into());
            }
            if let Some(hashes) = &mut self.run_hashes {
                if hashes.is_empty() {
                    return Err("scripted run has no state hash".into());
                }
                self.hash = Some(hashes.remove(0));
                self.moment = deadline;
                return Ok(StopReason::Deadline {
                    vtime: control_proto::Moment(deadline),
                });
            }
            Ok(StopReason::Quiescent {
                vtime: control_proto::Moment(0),
            })
        }

        fn snapshot(&mut self) -> Result<(SnapId, u64), Box<dyn Error>> {
            self.next_snap += 1;
            self.snapshots.insert(self.next_snap, self.hash.unwrap());
            Ok((SnapId(self.next_snap), self.moment))
        }

        fn sdk_events(
            &mut self,
        ) -> Result<Vec<consonance_client::session::SdkEvent>, Box<dyn Error>> {
            Ok(self.events.get(&self.current).cloned().unwrap_or_default())
        }

        fn abandoned(&self) -> bool {
            false
        }
    }

    fn timed_out_live() -> Live {
        Live {
            key: [0; 32],
            holder: 0,
            session: Box::new(TimedOutRestore {
                hash: None,
                events: BTreeMap::new(),
                current: 1,
                run_hashes: None,
                run_hung: false,
                snapshots: BTreeMap::new(),
                moment: 0,
                next_snap: 1,
            }),
            base_identity: [0; 32],
            setup: SnapId(1),
            windows: ActionWindows { root_seal: 0 },
            inherited: Vec::new(),
            chain: Chain::new(
                Point {
                    snap: SnapId(1),
                    moment: 0,
                },
                None,
            ),
            horizons_run: 0,
            abandoned: false,
            telemetry: LiveTelemetry::default(),
        }
    }

    fn publishing_live(
        index: &Arc<LocalIndex>,
        namespace: Namespace,
        target: SnapId,
        commitment: [u8; 32],
    ) -> Live {
        let mut live = timed_out_live();
        live.session = Box::new(TimedOutRestore {
            hash: Some(commitment),
            events: BTreeMap::new(),
            current: target.0,
            run_hashes: None,
            run_hung: false,
            snapshots: BTreeMap::from([(target.0, commitment)]),
            moment: 0,
            next_snap: target.0,
        });
        live.chain = Chain::new(
            Point {
                snap: SnapId(1),
                moment: 0,
            },
            Some(Shared {
                index: Arc::clone(index) as Arc<dyn CacheIndex>,
                namespace,
            }),
        );
        live
    }

    #[test]
    fn divergent_publication_fails_before_sealing_a_campaign_link() {
        use std::num::NonZeroU16;

        let index = Arc::new(LocalIndex::new(1 << 20));
        let namespace = Namespace::new(&[b"fault-campaign"]);
        let actions = [FaultOperation::Wait(NonZeroU16::MIN).into()];
        let mut first = publishing_live(&index, namespace, SnapId(2), [0x11; 32]);
        let mut second = publishing_live(&index, namespace, SnapId(3), [0x22; 32]);
        assert_eq!(
            first.seal_link(&actions, SnapId(2), 10, 0).unwrap().snap,
            SnapId(2)
        );
        let error = second.seal_link(&actions, SnapId(3), 10, 0).unwrap_err();
        assert!(error.contains("publish snapshot: snapshot cache key"));
        assert!(error.contains(&format!("{namespace:?}")));
        assert!(error.contains(&format!("{:?}", actions_key(&actions))));
        assert_eq!(second.chain.links(), 1);
        assert_eq!(index.stats().entries, 1);
        assert_eq!(index.stats().duplicates, 0);
        assert_eq!(second.session.state_hash().unwrap(), [0x22; 32]);
        assert!(first.find(&actions).unwrap_err().contains("quarantined"));
        assert!(index.lookup(namespace, &actions_key(&actions)).is_none());
        assert!(matches!(
            index.verify_integrity(),
            Err(CacheError::Quarantined { .. })
        ));
        let evidence = index.divergences();
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].existing.target_commitment, [0x11; 32]);
        assert_eq!(evidence[0].published.target_commitment, [0x22; 32]);
        assert_eq!(
            read_extent(evidence[0].existing.chain[0].bytes())
                .unwrap()
                .sidecar,
            &2u64.to_le_bytes()
        );
        assert_eq!(
            read_extent(evidence[0].published.chain[0].bytes())
                .unwrap()
                .sidecar,
            &3u64.to_le_bytes()
        );
    }

    #[test]
    fn equal_publication_with_distinct_extents_seals_a_campaign_link() {
        use std::num::NonZeroU16;

        let index = Arc::new(LocalIndex::new(1 << 20));
        let namespace = Namespace::new(&[b"fault-campaign"]);
        let actions = [FaultOperation::Wait(NonZeroU16::MIN).into()];
        let mut first = publishing_live(&index, namespace, SnapId(2), [0x11; 32]);
        let mut second = publishing_live(&index, namespace, SnapId(3), [0x11; 32]);
        first.seal_link(&actions, SnapId(2), 10, 0).unwrap();
        assert_eq!(
            second.seal_link(&actions, SnapId(3), 10, 0).unwrap().snap,
            SnapId(3)
        );
        assert_eq!(second.chain.links(), 2);
        assert_eq!(index.stats().entries, 1);
        assert_eq!(index.stats().duplicates, 1);
        let held = index.lookup(namespace, &actions_key(&actions)).unwrap();
        let extents = index.chain(&held).unwrap();
        assert_eq!(
            read_extent(extents[0].bytes()).unwrap().sidecar,
            &2u64.to_le_bytes()
        );
        index.release(held);
    }

    #[test]
    fn shared_cache_import_preserves_typed_watchdog_failure() {
        let index = Arc::new(LocalIndex::new(1 << 20));
        let namespace = Namespace::new(&[b"fault-import-timeout"]);
        let actions = [FaultOperation::Wait(std::num::NonZeroU16::MIN).into()];
        let mut publisher = publishing_live(&index, namespace, SnapId(2), [17; 32]);
        publisher.seal_link(&actions, SnapId(2), 10, 0).unwrap();
        let mut importer = timed_out_live();
        importer.chain = Chain::new(
            Point {
                snap: SnapId(1),
                moment: 0,
            },
            Some(Shared {
                index: index.clone(),
                namespace,
            }),
        );
        let error = importer.find(&actions).unwrap_err();
        assert!(error.starts_with(WATCHDOG_CUTOFF), "{error}");
        assert!(error.contains("import snapshot"), "{error}");
        assert!(importer.abandoned);
        assert_eq!(importer.chain.links(), 1);
    }

    #[test]
    fn bootstrap_cutoffs_remain_incomplete_through_evaluation_and_certification() {
        use searcher::search::campaign::{
            CampaignJobResult, Evaluation, Reporting, TargetExecution,
        };
        let game = crate::campaign::fixture_workload(
            b"kernel",
            b"initramfs",
            &FaultConfig {
                knobs: Vec::new(),
                ram_mib: DEFAULT_RAM_MIB,
                backend: GuestBackend::Vm,
                root: None,
            },
        );
        let run = crate::campaign::FaultCampaignRun {
            vocabulary: crate::bundle::FaultVocabulary::new(1, vec![1]).unwrap(),
        };
        let mut target = unbooted_target();
        let snapshot = FaultSnapshot {
            endpoint: Some(FaultEndpointCertificate::KvmWholeState([17; 32])),
            ..FaultSnapshot::default()
        };
        LIVE.with(|slot| *slot.borrow_mut() = Some(timed_out_live()));
        let error = game.restore(&mut target, &snapshot).unwrap_err();
        assert!(error.to_string().contains("campaign incomplete"), "{error}");
        let result = CampaignJobResult {
            preparation_failure: None,
            actions: Vec::new(),
        };
        assert!(
            game.certified_result_sha256(&result, &target)
                .unwrap_err()
                .to_string()
                .contains("campaign incomplete")
        );
        assert!(
            game.rollout_outcome(&run, &target)
                .unwrap_err()
                .to_string()
                .contains("campaign incomplete")
        );
        LIVE.with(|slot| *slot.borrow_mut() = Some(timed_out_live()));
        target.reset();
        assert_eq!(target.watchdog_cutoffs(), 1);
        assert!(
            game.rollout_outcome(&run, &target)
                .unwrap_err()
                .to_string()
                .contains("campaign incomplete")
        );
        LIVE.with(|slot| slot.borrow_mut().take());
    }

    #[derive(Debug)]
    struct BoundaryDecisionSession {
        resolutions: Arc<std::sync::Mutex<Vec<(ServiceConfig, Resolution)>>>,
        branch: Option<ServiceConfig>,
        deadline: u64,
        pending_moment: Option<u64>,
        next_snapshot: u64,
    }

    impl SearchSession for BoundaryDecisionSession {
        fn setup_handle(&self) -> (SnapId, u64) {
            (SnapId(1), 0)
        }

        fn state_hash(&mut self) -> Result<[u8; 32], Box<dyn Error>> {
            Ok([0; 32])
        }

        fn console_tail(&mut self) -> Result<Vec<u8>, Box<dyn Error>> {
            Ok(Vec::new())
        }

        fn telemetry_counters(&self) -> Vec<(String, u64)> {
            Vec::new()
        }

        fn snapshot_owned_pages(&self, _: SnapId) -> Option<u64> {
            None
        }

        fn store_bytes(&self) -> Option<u64> {
            None
        }

        fn publish_snapshot(
            &self,
            _: &dyn CacheIndex,
            _: Namespace,
            _: &[u8],
            _: Option<(SnapId, &Lease)>,
            _: SnapId,
            _: u64,
        ) -> Result<Lease, Box<dyn Error>> {
            unreachable!()
        }

        fn import_cached(
            &mut self,
            _: &dyn CacheIndex,
            _: &Lease,
            _: SnapId,
        ) -> Result<(SnapId, u64), Box<dyn Error>> {
            unreachable!()
        }

        fn replay_snapshot(&mut self, _: SnapId) -> Result<(), Box<dyn Error>> {
            Ok(())
        }

        fn drop_snapshot(&mut self, _: SnapId) -> Result<(), Box<dyn Error>> {
            Ok(())
        }

        fn branch_with_service(
            &mut self,
            _: SnapId,
            config: ServiceConfig,
            _: Vec<Vec<u8>>,
            _: Vec<(u64, environment::channel::Effect)>,
            _: Option<CoverageSchedule>,
        ) -> Result<(), Box<dyn Error>> {
            self.branch = Some(config);
            Ok(())
        }

        fn run_until(&mut self, deadline: u64) -> Result<StopReason, Box<dyn Error>> {
            self.deadline = deadline;
            let moment = *self.pending_moment.get_or_insert(deadline);
            Ok(StopReason::Decision {
                vtime: control_proto::Moment(moment),
                id: control_proto::DecisionId(7),
                ctx: STANDING_NAMESPACE.to_le_bytes().to_vec(),
            })
        }

        fn run_resolving(
            &mut self,
            deadline: u64,
            resolution: Resolution,
        ) -> Result<StopReason, Box<dyn Error>> {
            assert!(resolution.vtime.0 < deadline);
            self.pending_moment = None;
            self.resolutions
                .lock()
                .unwrap()
                .push((self.branch.clone().unwrap(), resolution));
            Ok(StopReason::Deadline {
                vtime: control_proto::Moment(deadline),
            })
        }

        fn snapshot(&mut self) -> Result<(SnapId, u64), Box<dyn Error>> {
            self.next_snapshot += 1;
            Ok((SnapId(self.next_snapshot), self.deadline))
        }

        fn sdk_events(
            &mut self,
        ) -> Result<Vec<consonance_client::session::SdkEvent>, Box<dyn Error>> {
            Ok(Vec::new())
        }

        fn abandoned(&self) -> bool {
            false
        }
    }

    #[test]
    fn a_boundary_decision_waits_for_the_next_action_service_windows() {
        use std::num::NonZeroU16;

        let tick = NonZeroU16::MIN;
        let actions = [
            FaultOperation::Wait(tick).into(),
            FaultOperation::Kill(3, tick).into(),
        ];
        let resolutions = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut live = timed_out_live();
        live.session = Box::new(BoundaryDecisionSession {
            resolutions: Arc::clone(&resolutions),
            branch: None,
            deadline: 0,
            pending_moment: None,
            next_snapshot: 1,
        });
        assert!(live.ensure_prefix(&actions).unwrap().is_ok());
        let resolutions = resolutions.lock().unwrap();
        assert_eq!(resolutions.len(), 1);
        let (installed, first) = &resolutions[0];
        assert_eq!(first.vtime.0, crate::target::SUPERVISOR_TICK_NANOS);
        assert_eq!(
            installed,
            &branch_config(live.windows, &[], &actions).unwrap()
        );
        let question = Question::with_request_id(7, STANDING_NAMESPACE, Vec::new()).unwrap();
        let expected = StandingHandler::from_configuration(&installed.configuration)
            .unwrap()
            .respond(first.vtime.0, &question)
            .unwrap();
        let ServiceResponse::Answered(expected) = expected else {
            panic!("the installed standing service must answer");
        };
        assert_eq!(first.answer.0, expected.encode());
        let previous = branch_config(live.windows, &[], &actions[..1]).unwrap();
        let previous = StandingHandler::from_configuration(&previous.configuration)
            .unwrap()
            .respond(first.vtime.0, &question)
            .unwrap();
        let ServiceResponse::Answered(previous) = previous else {
            panic!("the previous standing service must answer");
        };
        assert_ne!(first.answer.0, previous.encode());
    }

    fn hashed_live(hash: [u8; 32]) -> Live {
        let mut live = timed_out_live();
        live.session = Box::new(TimedOutRestore {
            hash: Some(hash),
            events: BTreeMap::new(),
            current: 1,
            run_hashes: None,
            run_hung: false,
            snapshots: BTreeMap::new(),
            moment: 0,
            next_snap: 1,
        });
        live
    }

    #[test]
    fn runnable_snapshot_identifies_the_current_kvm_endpoint() {
        let target = unbooted_target();
        LIVE.with(|slot| *slot.borrow_mut() = Some(hashed_live([17; 32])));
        let first = postcard::to_allocvec(&target.snapshot().unwrap().unwrap()).unwrap();
        LIVE.with(|slot| *slot.borrow_mut() = Some(hashed_live([23; 32])));
        let second = postcard::to_allocvec(&target.snapshot().unwrap().unwrap()).unwrap();
        LIVE.with(|slot| slot.borrow_mut().take());
        assert_ne!(
            first, second,
            "a runnable snapshot omitted its live endpoint"
        );
        assert!(target.snapshot().unwrap_err().contains("unavailable"));
    }

    #[test]
    fn restoring_a_certified_snapshot_rejects_a_changed_kvm_endpoint() {
        let mut target = unbooted_target();
        LIVE.with(|slot| *slot.borrow_mut() = Some(hashed_live([17; 32])));
        let snapshot = target.snapshot().unwrap().unwrap();
        assert_eq!(
            snapshot.endpoint,
            Some(FaultEndpointCertificate::KvmWholeState([17; 32]))
        );
        target.restore(&snapshot).unwrap();
        LIVE.with(|slot| *slot.borrow_mut() = Some(hashed_live([23; 32])));
        let error = target.restore(&snapshot).unwrap_err().to_string();
        assert!(
            error.contains("differs from its snapshot certificate"),
            "{error}"
        );
        let missing = FaultSnapshot {
            endpoint: None,
            ..snapshot
        };
        let error = target.restore(&missing).unwrap_err().to_string();
        assert!(error.contains("lacks a certificate"), "{error}");
        LIVE.with(|slot| slot.borrow_mut().take());
    }

    #[test]
    fn restoring_a_certified_snapshot_rejects_forged_logical_evidence() {
        let mut target = unbooted_target();
        LIVE.with(|slot| *slot.borrow_mut() = Some(hashed_live([17; 32])));
        let snapshot = target.snapshot().unwrap().unwrap();

        let mut forged = snapshot.clone();
        forged.observation.stop = FaultStop::Assertion { point: 7 };
        assert!(target.restore(&forged).is_err());

        forged.observation = snapshot.observation.clone();
        forged.observation.ticks = 1;
        assert!(target.restore(&forged).is_err());

        forged.observation = snapshot.observation.clone();
        forged.observation.parks.push(crate::target::ParkLanding {
            moment: 1,
            site: 2,
            edges: 3,
        });
        assert!(target.restore(&forged).is_err());

        forged.observation = snapshot.observation;
        forged.failed = true;
        assert!(target.restore(&forged).is_err());
        LIVE.with(|slot| slot.borrow_mut().take());
    }

    #[test]
    fn restoring_a_certified_action_preserves_its_local_park_reports() {
        let action = FaultOperation::Wait(std::num::NonZeroU16::new(1).unwrap()).into();
        let first = (10, 0, br#"{"harmony_park":{"site":1,"edges":2}}"#.to_vec());
        let second = (20, 0, br#"{"harmony_park":{"site":3,"edges":4}}"#.to_vec());
        let read = (
            30,
            0,
            br#"{"harmony_park_read":{"site":5,"edges":6}}"#.to_vec(),
        );
        let events = vec![first.clone(), second, read];
        let mut expected = FaultObservations::new(
            30,
            &decode_sdk_events(&events).unwrap(),
            FaultStop::Deadline,
        );
        expected.parks.retain(|park| park.moment > 10);
        expected.park_reads.retain(|read| read.moment > 10);
        let snapshot = FaultSnapshot {
            actions: vec![action],
            prefix_certificates: vec![Some(
                replay_endpoint_certificate([17; 32], &expected, false).unwrap(),
            )],
            observation: expected,
            failed: false,
            endpoint: Some(FaultEndpointCertificate::KvmWholeState([17; 32])),
        };
        let mut target = unbooted_target();
        let mut live = hashed_live([17; 32]);
        live.chain.push(
            &[action],
            Point {
                snap: SnapId(2),
                moment: 1_000_000,
            },
            None,
        );
        live.session = Box::new(TimedOutRestore {
            hash: Some([17; 32]),
            events: BTreeMap::from([(1, vec![first]), (2, events)]),
            current: 1,
            run_hashes: None,
            run_hung: false,
            snapshots: BTreeMap::new(),
            moment: 0,
            next_snap: 2,
        });
        LIVE.with(|slot| *slot.borrow_mut() = Some(live));
        target.restore(&snapshot).unwrap();
        assert_eq!(target.observation(), &snapshot.observation);
        LIVE.with(|slot| slot.borrow_mut().take());
    }

    #[test]
    fn replayed_prefix_state_and_observation_must_be_checked_before_suffix_convergence() {
        let first_action = FaultOperation::Wait(std::num::NonZeroU16::new(1).unwrap()).into();
        let second_action = FaultOperation::Wait(std::num::NonZeroU16::new(2).unwrap()).into();
        let run = |first_hash,
                   forged_observation,
                   expected|
         -> Result<([u8; 32], FaultObservations, FaultSnapshot), String> {
            let mut target = unbooted_target();
            let mut live = hashed_live([17; 32]);
            live.session = Box::new(TimedOutRestore {
                hash: Some([17; 32]),
                events: BTreeMap::new(),
                current: 1,
                run_hashes: Some(vec![first_hash, [17; 32]]),
                run_hung: false,
                snapshots: BTreeMap::from([(1, [17; 32])]),
                moment: 0,
                next_snap: 1,
            });
            LIVE.with(|slot| *slot.borrow_mut() = Some(live));
            let keyframe = target.snapshot().unwrap().unwrap();
            target.restore(&keyframe).unwrap();
            target.apply_replayed(first_action);
            assert!(!target.failed());
            let replayed_parent = target.state_hash_current().unwrap();
            if forged_observation {
                target.observation.edge_digest ^= 1;
            }
            let replayed_observation = target.observation().clone();
            if let Some(expected) = expected {
                let checked = target.verify_replayed_endpoint(Some(expected));
                if let Err(error) = checked {
                    LIVE.with(|slot| slot.borrow_mut().take());
                    return Err(error);
                }
            }
            target.apply(second_action);
            assert!(!target.failed());
            let final_snapshot = target.snapshot().unwrap().unwrap();
            LIVE.with(|slot| slot.borrow_mut().take());
            Ok((replayed_parent, replayed_observation, final_snapshot))
        };
        let baseline = run([17; 32], false, None).unwrap();
        let certificate = replay_endpoint_certificate(baseline.0, &baseline.1, false).unwrap();
        let verified_baseline = run([17; 32], false, Some(certificate)).unwrap();
        let divergent = run([23; 32], false, None).unwrap();
        let forged = run([17; 32], true, None).unwrap();
        let archived_parent = FaultEndpointCertificate::KvmWholeState([17; 32]);
        assert_eq!(baseline.0, [17; 32]);
        assert_ne!(divergent.0, [17; 32]);
        assert_eq!(verified_baseline.2, baseline.2);
        assert_eq!(
            archived_parent,
            FaultEndpointCertificate::KvmWholeState(baseline.0)
        );
        assert_eq!(baseline.2.actions, divergent.2.actions);
        assert_eq!(baseline.2.observation, divergent.2.observation);
        assert_eq!(baseline.2.endpoint, divergent.2.endpoint);
        assert_ne!(
            baseline.2.prefix_certificates,
            divergent.2.prefix_certificates
        );
        assert_eq!(baseline.2, forged.2);
        assert_eq!(forged.0, baseline.0);
        assert_ne!(forged.1, baseline.1);
        let error = run([23; 32], false, Some(certificate)).unwrap_err();
        assert!(
            error.contains("differs from its archived endpoint certificate"),
            "{error}"
        );
        let error = run([17; 32], true, Some(certificate)).unwrap_err();
        assert!(
            error.contains("differs from its archived endpoint certificate"),
            "{error}"
        );
    }

    #[test]
    fn campaign_job_rejects_a_divergent_replayed_prefix_before_its_suffix() {
        let first_action = FaultOperation::Wait(std::num::NonZeroU16::new(1).unwrap()).into();
        let second_action = FaultOperation::Wait(std::num::NonZeroU16::new(2).unwrap()).into();
        let mut baseline = unbooted_target();
        let mut baseline_live = hashed_live([17; 32]);
        baseline_live.session = Box::new(TimedOutRestore {
            hash: Some([17; 32]),
            events: BTreeMap::new(),
            current: 1,
            run_hashes: Some(vec![[17; 32], [17; 32]]),
            run_hung: false,
            snapshots: BTreeMap::from([(1, [17; 32])]),
            moment: 0,
            next_snap: 1,
        });
        LIVE.with(|slot| *slot.borrow_mut() = Some(baseline_live));
        let baseline_keyframe = baseline.snapshot().unwrap().unwrap();
        baseline.restore(&baseline_keyframe).unwrap();
        baseline.apply_replayed(first_action);
        let expected = replay_endpoint_certificate(
            baseline.state_hash_current().unwrap(),
            baseline.observation(),
            baseline.failed(),
        )
        .unwrap();
        LIVE.with(|slot| slot.borrow_mut().take());

        let mut target = unbooted_target();
        let mut live = hashed_live([17; 32]);
        live.session = Box::new(TimedOutRestore {
            hash: Some([17; 32]),
            events: BTreeMap::new(),
            current: 1,
            run_hashes: Some(vec![[23; 32], [17; 32]]),
            run_hung: false,
            snapshots: BTreeMap::from([(1, [17; 32])]),
            moment: 0,
            next_snap: 1,
        });
        LIVE.with(|slot| *slot.borrow_mut() = Some(live));
        let keyframe = target.snapshot().unwrap().unwrap();
        let error = crate::campaign::execute_job(
            &mut target,
            &keyframe,
            &[first_action],
            &[Some(expected)],
            crate::archive::FaultMilestones::default(),
            &[second_action],
            false,
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("differs from its archived endpoint certificate"),
            "{error}"
        );
        LIVE.with(|slot| slot.borrow_mut().take());
    }

    #[test]
    fn campaign_job_reconstruction_cutoff_is_incomplete() {
        let mut target = unbooted_target();
        let snapshot = FaultSnapshot {
            actions: Vec::new(),
            prefix_certificates: Vec::new(),
            observation: FaultObservations::default(),
            failed: false,
            endpoint: Some(FaultEndpointCertificate::KvmWholeState([17; 32])),
        };
        LIVE.with(|slot| *slot.borrow_mut() = Some(timed_out_live()));
        let result = crate::campaign::execute_job(
            &mut target,
            &snapshot,
            &[FaultOperation::Wait(std::num::NonZeroU16::new(1).unwrap()).into()],
            &[Some([17; 32])],
            crate::archive::FaultMilestones::default(),
            &[],
            false,
        )
        .unwrap_err();
        assert!(
            result.to_string().contains("campaign incomplete"),
            "{result}"
        );
        assert!(
            result.to_string().contains("parent reconstruction"),
            "{result}"
        );
        assert_eq!(target.watchdog_cutoffs(), 1);
        assert!(!target.found_bug());
        assert!(target.snapshot().unwrap().is_none());
        LIVE.with(|slot| slot.borrow_mut().take());
    }

    #[test]
    fn campaign_job_replayed_action_cutoff_is_incomplete() {
        let mut target = unbooted_target();
        let snapshot = FaultSnapshot {
            actions: Vec::new(),
            prefix_certificates: Vec::new(),
            observation: FaultObservations::default(),
            failed: false,
            endpoint: Some(FaultEndpointCertificate::KvmWholeState([17; 32])),
        };
        let mut live = hashed_live([17; 32]);
        live.session = Box::new(TimedOutRestore {
            hash: Some([17; 32]),
            events: BTreeMap::new(),
            current: 1,
            run_hashes: None,
            run_hung: true,
            snapshots: BTreeMap::from([(1, [17; 32])]),
            moment: 0,
            next_snap: 1,
        });
        LIVE.with(|slot| *slot.borrow_mut() = Some(live));
        let result = crate::campaign::execute_job(
            &mut target,
            &snapshot,
            &[FaultOperation::Wait(std::num::NonZeroU16::new(1).unwrap()).into()],
            &[Some([17; 32])],
            crate::archive::FaultMilestones::default(),
            &[],
            false,
        )
        .unwrap_err();
        assert!(
            result.to_string().contains("campaign incomplete"),
            "{result}"
        );
        assert!(result.to_string().contains("parent replay"), "{result}");
        assert_eq!(target.watchdog_cutoffs(), 1);
        assert!(!target.found_bug());
        assert!(target.snapshot().unwrap().is_none());
        LIVE.with(|slot| slot.borrow_mut().take());
    }

    #[test]
    fn campaign_job_replay_non_watchdog_failure_remains_fatal() {
        let mut target = unbooted_target();
        let snapshot = FaultSnapshot {
            actions: Vec::new(),
            prefix_certificates: Vec::new(),
            observation: FaultObservations::default(),
            failed: false,
            endpoint: Some(FaultEndpointCertificate::KvmWholeState([17; 32])),
        };
        let mut live = hashed_live([17; 32]);
        live.session = Box::new(TimedOutRestore {
            hash: Some([17; 32]),
            events: BTreeMap::new(),
            current: 1,
            run_hashes: Some(Vec::new()),
            run_hung: false,
            snapshots: BTreeMap::from([(1, [17; 32])]),
            moment: 0,
            next_snap: 1,
        });
        LIVE.with(|slot| *slot.borrow_mut() = Some(live));
        let error = crate::campaign::execute_job(
            &mut target,
            &snapshot,
            &[FaultOperation::Wait(std::num::NonZeroU16::new(1).unwrap()).into()],
            &[Some([17; 32])],
            crate::archive::FaultMilestones::default(),
            &[],
            false,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("replayed fault prefix stopped"), "{error}");
        LIVE.with(|slot| slot.borrow_mut().take());
    }

    #[test]
    fn campaign_suffix_cutoff_discards_the_entire_unadmitted_job() {
        let first = FaultOperation::Wait(std::num::NonZeroU16::new(1).unwrap()).into();
        let second = FaultOperation::Wait(std::num::NonZeroU16::new(2).unwrap()).into();
        let mut target = unbooted_target();
        let mut live = hashed_live([17; 32]);
        live.session = Box::new(TimedOutRestore {
            hash: Some([17; 32]),
            events: BTreeMap::new(),
            current: 1,
            run_hashes: Some(vec![[17; 32]]),
            run_hung: true,
            snapshots: BTreeMap::from([(1, [17; 32])]),
            moment: 0,
            next_snap: 1,
        });
        LIVE.with(|slot| *slot.borrow_mut() = Some(live));
        let origin = target.snapshot().unwrap().unwrap();
        let result = crate::campaign::execute_job(
            &mut target,
            &origin,
            &[],
            &[],
            crate::archive::FaultMilestones::default(),
            &[first, second],
            false,
        );
        let error = result.expect_err("no partial job may be certified or admitted");
        assert!(error.to_string().contains("campaign incomplete"), "{error}");
        assert!(error.to_string().contains("suffix execution"), "{error}");
        assert_eq!(target.actions(), &[first]);
        assert_eq!(target.watchdog_cutoffs(), 1);
        assert!(target.snapshot().unwrap().is_none());
        assert!(!target.found_bug());
        LIVE.with(|slot| slot.borrow_mut().take());
    }

    #[test]
    fn restoring_a_cached_certified_path_does_not_rebuild_it() {
        let first = FaultOperation::Wait(std::num::NonZeroU16::new(1).unwrap()).into();
        let second = FaultOperation::Wait(std::num::NonZeroU16::new(2).unwrap()).into();
        let mut target = unbooted_target();
        let mut live = hashed_live([17; 32]);
        live.session = Box::new(TimedOutRestore {
            hash: Some([17; 32]),
            events: BTreeMap::new(),
            current: 1,
            run_hashes: Some(vec![[17; 32], [17; 32]]),
            run_hung: false,
            snapshots: BTreeMap::from([(1, [17; 32])]),
            moment: 0,
            next_snap: 1,
        });
        LIVE.with(|slot| *slot.borrow_mut() = Some(live));
        target.apply(first);
        target.apply(second);
        let snapshot = target.snapshot().unwrap().unwrap();
        assert_eq!(snapshot.actions, vec![first, second]);
        let links = LIVE.with(|slot| slot.borrow().as_ref().unwrap().chain.links());
        assert_eq!(links, 3);
        target.restore(&snapshot).unwrap();
        assert_eq!(target.observation(), &snapshot.observation);
        assert_eq!(
            LIVE.with(|slot| slot.borrow().as_ref().unwrap().chain.links()),
            links
        );
        LIVE.with(|slot| slot.borrow_mut().take());
    }

    #[test]
    fn campaign_job_rejects_a_divergent_prefix_inside_a_resident_keyframe() {
        let first_action = FaultOperation::Wait(std::num::NonZeroU16::new(1).unwrap()).into();
        let second_action = FaultOperation::Wait(std::num::NonZeroU16::new(2).unwrap()).into();
        let mut baseline = unbooted_target();
        let mut baseline_live = hashed_live([17; 32]);
        baseline_live.session = Box::new(TimedOutRestore {
            hash: Some([17; 32]),
            events: BTreeMap::new(),
            current: 1,
            run_hashes: Some(vec![[17; 32], [17; 32]]),
            run_hung: false,
            snapshots: BTreeMap::from([(1, [17; 32])]),
            moment: 0,
            next_snap: 1,
        });
        LIVE.with(|slot| *slot.borrow_mut() = Some(baseline_live));
        baseline.apply(first_action);
        baseline.apply(second_action);
        assert!(!baseline.failed());
        let keyframe = baseline.snapshot().unwrap().unwrap();
        assert_eq!(keyframe.actions, vec![first_action, second_action]);
        LIVE.with(|slot| slot.borrow_mut().take());

        for cached in [false, true] {
            let mut target = unbooted_target();
            let mut live = hashed_live([17; 32]);
            live.session = Box::new(TimedOutRestore {
                hash: Some([17; 32]),
                events: BTreeMap::new(),
                current: 1,
                run_hashes: Some(vec![[23; 32], [17; 32]]),
                run_hung: false,
                snapshots: BTreeMap::from([(1, [17; 32])]),
                moment: 0,
                next_snap: 1,
            });
            LIVE.with(|slot| *slot.borrow_mut() = Some(live));
            if cached {
                target.apply(first_action);
                target.apply(second_action);
                assert_eq!(
                    target.snapshot().unwrap().unwrap().endpoint,
                    keyframe.endpoint
                );
            }
            let result = crate::campaign::execute_job(
                &mut target,
                &keyframe,
                &[],
                &[],
                crate::archive::FaultMilestones::default(),
                &[],
                false,
            );
            LIVE.with(|slot| slot.borrow_mut().take());
            let error = result.expect_err("the first keyframe prefix must be checked");
            assert!(error.to_string().contains("prefix"), "{error}");
        }
    }

    #[test]
    fn restore_rejects_a_prefix_that_stops_before_its_certified_endpoint() {
        let mut target = unbooted_target();
        let snapshot = FaultSnapshot {
            actions: vec![FaultOperation::Wait(std::num::NonZeroU16::new(1).unwrap()).into()],
            prefix_certificates: vec![Some(
                replay_endpoint_certificate([17; 32], &FaultObservations::default(), false)
                    .unwrap(),
            )],
            endpoint: Some(FaultEndpointCertificate::KvmWholeState([17; 32])),
            ..FaultSnapshot::default()
        };
        LIVE.with(|slot| *slot.borrow_mut() = Some(hashed_live([17; 32])));
        let error = target.restore(&snapshot).unwrap_err().to_string();
        assert!(
            error.contains("stopped before its snapshot certificate"),
            "{error}"
        );
        LIVE.with(|slot| slot.borrow_mut().take());
    }

    #[test]
    fn current_job_state_hash_never_boots_a_missing_or_foreign_session() {
        let target = unbooted_target();
        LIVE.with(|slot| slot.borrow_mut().take());
        assert!(
            target
                .state_hash_current()
                .unwrap_err()
                .contains("unavailable")
        );
        LIVE.with(|slot| *slot.borrow_mut() = Some(timed_out_live()));
        LIVE.with(|slot| slot.borrow_mut().as_mut().unwrap().key = [1; 32]);
        assert!(
            target
                .state_hash_current()
                .unwrap_err()
                .contains("different identity")
        );
        LIVE.with(|slot| {
            assert_eq!(slot.borrow().as_ref().unwrap().key, [1; 32]);
            slot.borrow_mut().as_mut().unwrap().abandoned = true;
        });
        LIVE.with(|slot| slot.borrow_mut().as_mut().unwrap().key = [0; 32]);
        assert!(
            target
                .state_hash_current()
                .unwrap_err()
                .contains("abandoned")
        );
        LIVE.with(|slot| slot.borrow_mut().take());
    }

    #[test]
    fn replay_timeout_is_a_recoverable_watchdog_cutoff() {
        let mut live = timed_out_live();
        let error = live.replay(SnapId(1)).unwrap_err();
        assert!(error.starts_with(WATCHDOG_CUTOFF));
        assert!(live.abandoned);
    }

    #[test]
    fn branch_restore_timeout_is_a_recoverable_watchdog_cutoff() {
        let mut live = timed_out_live();
        let error = live
            .branch(
                Point {
                    snap: SnapId(1),
                    moment: 0,
                },
                &[],
            )
            .unwrap_err();
        assert!(error.starts_with(WATCHDOG_CUTOFF));
        assert!(live.abandoned);

        let mut target = unbooted_target();
        let snapshot = FaultSnapshot {
            actions: Vec::new(),
            prefix_certificates: Vec::new(),
            observation: FaultObservations::default(),
            failed: false,
            endpoint: None,
        };
        target.finish_restore(&snapshot, Err(error)).unwrap();
        assert_eq!(target.watchdog_cutoffs(), 1);
        assert!(target.last_action_observations()[0].watchdog_cutoff);
    }

    #[test]
    fn debug_service_is_portable_and_root_identity_includes_saved_state() {
        let standing = branch_config(ActionWindows { root_seal: 1000 }, &[], &[]).unwrap();
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
        let base = identity_with_root("test-admitted-session", None);
        let first = identity_with_root("test-admitted-session", Some(&[1]));
        assert_ne!(first, base);
        assert_ne!(
            first,
            identity_with_root("test-admitted-session", Some(&[2]))
        );
    }

    #[test]
    fn reconstruction_cutoff_discards_stale_evidence_and_allows_a_later_restore() {
        let mut target = unbooted_target();
        let snapshot = FaultSnapshot {
            actions: vec![FaultOperation::Wait(std::num::NonZeroU16::new(32768).unwrap()).into()],
            prefix_certificates: Vec::new(),
            observation: FaultObservations {
                ticks: 123,
                ..FaultObservations::default()
            },
            failed: false,
            endpoint: None,
        };
        target.observation.stop = FaultStop::Crash;
        target.action_observations.push(target.observation.clone());
        target
            .finish_restore(&snapshot, Err(format!("{WATCHDOG_CUTOFF}run: expired")))
            .unwrap();
        assert!(target.failed());
        assert!(!target.found_bug());
        assert!(target.snapshot().unwrap().is_none());
        assert!(target.actions.is_empty());
        assert_eq!(target.watchdog_cutoffs(), 1);
        target.apply(FaultOperation::Kill(0, std::num::NonZeroU16::new(50).unwrap()).into());
        assert_eq!(target.execution_ticks(), 17);
        assert_eq!(target.last_action_observations().len(), 1);
        assert!(target.last_action_observations()[0].watchdog_cutoff);
        assert!(!target.last_action_observations()[0].is_bug());
        assert_eq!(target.last_action_observations()[0].ticks, 0);
        target.finish_restore(&snapshot, Ok(())).unwrap();
        assert!(!target.failed());
        assert_eq!(target.watchdog_cutoffs(), 0);
        assert_eq!(target.observation(), &snapshot.observation);
        assert_eq!(target.actions(), snapshot.actions);
        assert!(!target.last_action_observations()[0].watchdog_cutoff);
    }

    #[test]
    fn run_and_snapshot_watchdogs_share_the_recoverable_classification() {
        let snapshot = FaultSnapshot {
            actions: Vec::new(),
            prefix_certificates: Vec::new(),
            observation: FaultObservations::default(),
            failed: false,
            endpoint: None,
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
            prefix_certificates: Vec::new(),
            observation: FaultObservations::default(),
            failed: false,
            endpoint: None,
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
        let configuration = branch_config(windows, &[], &actions).unwrap();
        let schedule = branch_coverage_schedule(windows, &actions).unwrap();
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
            assert_eq!(schedule.quantum_at(moment), expected);
            assert_eq!(
                handler.respond(moment, &question).unwrap(),
                ServiceResponse::Answered(ChannelAnswer::Data(expected.to_le_bytes().to_vec()))
            );
        }
        let mut ctx = environment::channel::SERVICE_COVERAGE_QUANTUM
            .to_le_bytes()
            .to_vec();
        ctx.extend_from_slice(&1_u64.to_le_bytes());
        let stop = StopReason::Decision {
            vtime: control_proto::Moment(1_000),
            id: control_proto::DecisionId(7),
            ctx,
        };
        assert_eq!(
            resolution_for_stop(&stop, &mut handler).unwrap(),
            Resolution {
                vtime: control_proto::Moment(1_000),
                service: environment::channel::SERVICE_COVERAGE_QUANTUM,
                id: control_proto::DecisionId(7),
                answer: control_proto::Answer(
                    ChannelAnswer::Data(64_u64.to_le_bytes().to_vec()).encode()
                ),
            }
        );
        assert!(
            resolution_for_stop(
                &StopReason::Decision {
                    vtime: control_proto::Moment(1_000),
                    id: control_proto::DecisionId(7),
                    ctx: vec![1],
                },
                &mut handler,
            )
            .is_err()
        );
        let (standing, mut gap) = decode_configuration(&configuration.configuration).unwrap();
        gap[1].0 += 1;
        let gap = encode_configuration(standing, &gap).unwrap();
        assert!(StandingHandler::from_configuration(&gap).is_err());
        let truncated = &configuration.configuration[..configuration.configuration.len() - 1];
        assert!(StandingHandler::from_configuration(truncated).is_err());
    }

    #[test]
    fn root_continuation_keeps_an_unfired_event_kill_window() {
        let root = ActionWindows { root_seal: 1_000 };
        let prefix = [FaultAction::from(FaultOperation::EventKill {
            node: 0,
            rarity: 1,
            ticks: std::num::NonZeroU16::new(2).unwrap(),
        })];
        let moment = root.window(&prefix, 0).unwrap().1;
        let original = branch_config(root, &[], &prefix).unwrap();
        let active = active_root_windows(&[], root, &prefix, moment).unwrap();
        let payload = encode_root_payload([7; 32], &active, b"snapshot").unwrap();
        let decoded = decode_root_payload(&payload, [7; 32]).unwrap();
        assert_eq!(decoded.snapshot, b"snapshot");
        assert_eq!(root_standing(&payload).unwrap(), active);
        let continuation =
            branch_config(ActionWindows { root_seal: moment }, &decoded.standing, &[]).unwrap();
        let mut original = StandingHandler::from_configuration(&original.configuration).unwrap();
        let mut continuation =
            StandingHandler::from_configuration(&continuation.configuration).unwrap();
        let question = Question::with_request_id(1, STANDING_NAMESPACE, Vec::new()).unwrap();
        assert_eq!(
            original.respond(moment, &question).unwrap(),
            continuation.respond(moment, &question).unwrap()
        );
        let rooted = ActionWindows { root_seal: moment };
        let suffix = [FaultAction::from(FaultOperation::Wait(
            std::num::NonZeroU16::MIN,
        ))];
        let nested_moment = rooted.window(&suffix, 0).unwrap().1;
        let nested_active =
            active_root_windows(&decoded.standing, rooted, &suffix, nested_moment).unwrap();
        let nested_payload = encode_root_payload([7; 32], &nested_active, b"nested").unwrap();
        let nested = decode_root_payload(&nested_payload, [7; 32]).unwrap();
        let nested_config = branch_config(
            ActionWindows {
                root_seal: nested_moment,
            },
            &nested.standing,
            &[],
        )
        .unwrap();
        let mut nested = StandingHandler::from_configuration(&nested_config.configuration).unwrap();
        assert_eq!(
            original.respond(nested_moment, &question).unwrap(),
            nested.respond(nested_moment, &question).unwrap()
        );
        assert!(decode_root_payload(&payload, [8; 32]).is_err());
        assert!(decode_root_payload(&payload[..payload.len() - 8], [7; 32]).is_err());
        assert!(decode_root_payload(&payload[..payload.len() - 9], [7; 32]).is_err());
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
    standing: &[StandingWindow],
) -> Result<Vec<u8>, Box<dyn Error>> {
    use consonance_client::cache::LocalIndex;
    let index = LocalIndex::new(usize::MAX);
    let lease = session.publish_root(&index, Namespace::new(&[&identity]), snapshot)?;
    let chain = index.chain(&lease)?;
    if chain.len() != 1 {
        return Err("branch root must be a complete snapshot extent".into());
    }
    encode_root_payload(identity, standing, chain[0].bytes())
}

fn active_root_windows(
    inherited: &[StandingWindow],
    windows: ActionWindows,
    actions: &[FaultAction],
    moment: u64,
) -> Result<Vec<StandingWindow>, String> {
    let mut standing = inherited.to_vec();
    standing.extend(standing_windows(windows, actions)?);
    standing.retain(|window| window.contains(moment));
    Ok(standing)
}

fn import_root(
    session: &mut dyn SearchSession,
    bytes: &[u8],
) -> Result<(SnapId, u64, Vec<StandingWindow>), Box<dyn Error>> {
    use consonance_client::cache::LocalIndex;
    let identity = session.root_identity()?;
    let RootPayload { standing, snapshot } = decode_root_payload(bytes, identity)?;
    let index = LocalIndex::new(usize::MAX);
    let mut extent = index.extent(snapshot.len())?;
    extent.bytes_mut().copy_from_slice(snapshot);
    let target_commitment = Sha256::digest(extent.bytes_mut()).into();
    let lease = index.publish(
        Namespace::new(&[&identity]),
        b"root",
        None,
        extent,
        target_commitment,
        1,
    )?;
    let (setup, moment) = session.import_root(&index, &lease)?;
    if standing.iter().any(|window| !window.contains(moment)) {
        return Err("branch root standing window is inactive at its snapshot moment".into());
    }
    Ok((setup, moment, standing))
}

fn encode_root_payload(
    identity: [u8; 32],
    standing: &[StandingWindow],
    snapshot: &[u8],
) -> Result<Vec<u8>, Box<dyn Error>> {
    let encoded = encode_windows(standing)?;
    let length =
        u32::try_from(encoded.len()).map_err(|_| "branch root standing list is too large")?;
    let mut bytes = b"HMROOT02".to_vec();
    bytes.extend_from_slice(&identity);
    bytes.extend_from_slice(&length.to_le_bytes());
    bytes.extend_from_slice(&encoded);
    bytes.extend_from_slice(snapshot);
    Ok(bytes)
}

pub(crate) fn root_standing(root: &[u8]) -> Result<Vec<StandingWindow>, Box<dyn Error>> {
    let identity = root
        .get(8..40)
        .ok_or("invalid branch snapshot header")?
        .try_into()?;
    Ok(decode_root_payload(root, identity)?.standing)
}

struct RootPayload<'a> {
    standing: Vec<StandingWindow>,
    snapshot: &'a [u8],
}

fn decode_root_payload(
    bytes: &[u8],
    identity: [u8; 32],
) -> Result<RootPayload<'_>, Box<dyn Error>> {
    if bytes.len() < 44 || &bytes[..8] != b"HMROOT02" {
        return Err("invalid branch snapshot header".into());
    }
    if bytes[8..40] != identity {
        return Err("branch snapshot setup identity differs".into());
    }
    let length = usize::try_from(u32::from_le_bytes(bytes[40..44].try_into()?))?;
    let end = 44_usize
        .checked_add(length)
        .ok_or("branch root standing length overflows")?;
    let standing = fault_policy::decode_windows(
        bytes
            .get(44..end)
            .ok_or("truncated branch root standing list")?,
    )?;
    let snapshot = bytes.get(end..).ok_or("missing branch root snapshot")?;
    if snapshot.is_empty() {
        return Err("missing branch root snapshot".into());
    }
    Ok(RootPayload { standing, snapshot })
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
