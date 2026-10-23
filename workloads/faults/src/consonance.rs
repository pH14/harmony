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
    endpoint::{
        EndpointBranch, EndpointProof, EndpointProofRequest, EndpointReceipt, EndpointStops,
        ExecutionEndpoint, ExecutionRequest, ExecutionStart,
    },
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
    input_spec::{InputSpec, ServiceConfig, ServiceFactory, nominal_factory},
};
use fault_policy::{STANDING_NAMESPACE, StandingWindow, encode_standing, encode_windows};
use searcher::target::ExitKind;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::chain::{Chain, Hit, Point, Shared};
use crate::history::{FaultHistory, HistoryReceipt};
use crate::target::{
    ActionWindows, FaultAction, FaultEndpointCertificate, FaultObservations, FaultSnapshot,
    FaultStop, ObservationCheckpoint, SdkDecoder, action_delta, standing_windows,
};

pub const DEFAULT_RAM_MIB: u32 = 1024;
pub const SERVICE_IDENTITY: &[u8] = b"faults-standing-coverage-v5";
const LOCAL_SERVICES: [u16; 2] = [STANDING_NAMESPACE, process_proto::debug::NAMESPACE];
pub const SESSION_SERVICE: &str = "faults";
const SEED: u64 = 0x4661_756c_744c_6162;
const SETUP_BUDGET: u64 = 120_000_000_000;
const WALL_LIMIT: Duration = Duration::from_secs(60);
const CONSOLE_TAIL: usize = 1_500;
const WATCHDOG_CUTOFF: &str = "fault-guest-watchdog-cutoff: ";
const INFRASTRUCTURE_FAILURE: &str = "fault-host-infrastructure: ";

pub fn replay_endpoint_certificate(
    hash: [u8; 32],
    history: (u64, [u8; 32]),
    observation: &FaultObservations,
    failed: bool,
) -> Result<[u8; 32], String> {
    crate::history::replay_certificate(hash, history, observation, failed)
}

pub(crate) fn validate_snapshot_prefix_certificates(
    snapshot: &FaultSnapshot,
    uml: bool,
) -> Result<(), String> {
    if !uml && snapshot.event_history.is_none() {
        return Err("KVM snapshot lacks ordered event history".to_owned());
    }
    if uml && snapshot.event_history.is_some() {
        return Err("UML snapshot unexpectedly carries a normalized event history".to_owned());
    }
    let receipt = snapshot.history.receipt();
    receipt.validate()?;
    let endpoint = snapshot
        .endpoint
        .ok_or("fault snapshot lacks an endpoint")?;
    if receipt.endpoint() != endpoint
        || receipt.event_history() != snapshot.event_history
        || receipt.certificate()
            != crate::history::endpoint_certificate(
                endpoint,
                snapshot.event_history,
                &snapshot.observation,
            )?
        || !receipt.matches_observation(&snapshot.observation)?
        || receipt.moment() != snapshot.observation.moment
        || matches!(endpoint, FaultEndpointCertificate::UmlWholeStateUnavailable) != uml
    {
        return Err("fault snapshot final prefix differs from its endpoint certificate".to_owned());
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
    pub boot_cache: Option<PathBuf>,
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
        let config = SessionConfig::new(ram, SEED, SETUP_BUDGET, self.cmdline())
            .with_identity_tag(IDENTITY_TAG)
            .with_wall_limit(WALL_LIMIT)
            .with_deferred_virtual_time_checkpoint_hashes();
        match &self.boot_cache {
            Some(dir) => config.with_boot_cache(dir),
            None => config,
        }
    }
}

const IDENTITY_TAG: &str = "faults-consonance-execution-v11";

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
    fn local_services(&self) -> &[u16] {
        &LOCAL_SERVICES
    }
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
            validate_debug_status(question)?;
            return Ok(ServiceResponse::Answered(ChannelAnswer::data(Vec::new())?));
        }
        if question.service() != STANDING_NAMESPACE {
            return Err(ChannelError::Handler(
                "the fault package answers only the standing namespace".to_owned(),
            ));
        }
        let after = process_proto::decode_standing_question(question.payload())
            .map_err(|_| ChannelError::Malformed)?
            .map_or(moment, |after| after.min(moment));
        let answer: Vec<StandingWindow> = self
            .0
            .windows
            .iter()
            .filter(|window| window.start <= moment && after < window.end)
            .cloned()
            .collect();
        let bytes = encode_standing(moment, &answer).map_err(|_| ChannelError::TooLarge)?;
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
            if !config.local_services.is_empty() {
                return Err(ChannelError::Malformed);
            }
            return Ok(
                Box::new(DebugHandler::decode(&config.configuration)?) as Box<dyn ServiceHandler>
            );
        }
        if config.identity != SERVICE_IDENTITY {
            return nominal(config);
        }
        if config.local_services != LOCAL_SERVICES {
            return Err(ChannelError::Malformed);
        }
        Ok(
            Box::new(StandingHandler::from_configuration(&config.configuration)?)
                as Box<dyn ServiceHandler>,
        )
    })
}

struct ActionPlan {
    handler: StandingHandler,
    coverage_schedule: CoverageSchedule,
    deadline: u64,
    entropy_seed: u64,
}

impl ActionPlan {
    fn compile(
        windows: ActionWindows,
        inherited: &[StandingWindow],
        actions: &[FaultAction],
    ) -> Result<Self, Box<dyn Error>> {
        let mut standing = inherited.to_vec();
        let mut coverage = Vec::with_capacity(actions.len());
        let mut deadline = windows.root_seal;
        for (action, window) in actions.iter().zip(windows.iter(actions)) {
            let (start, end) = window?;
            standing.extend(action_delta(*action, (start, end)).standing);
            coverage.push((start, end, action.coverage_quantum));
            deadline = end;
        }
        let configuration = encode_configuration(&encode_windows(&standing)?, &coverage)?;
        let rows = coverage
            .iter()
            .map(|&(start, end, quantum)| (start, end, u64::from(quantum.get())))
            .collect();
        let coverage_schedule =
            CoverageSchedule::new(environment::channel::DEFAULT_COVERAGE_QUANTUM, rows)?;
        Ok(Self {
            handler: StandingHandler(Arc::new(StandingPlan {
                configuration,
                windows: standing,
                coverage,
            })),
            coverage_schedule,
            deadline,
            entropy_seed: action_entropy_seed(actions),
        })
    }

    fn service_config(&self) -> ServiceConfig {
        ServiceConfig {
            local_services: LOCAL_SERVICES.to_vec(),
            identity: SERVICE_IDENTITY.to_vec(),
            configuration: self.handler.0.configuration.clone(),
        }
    }
}

fn action_entropy_seed(actions: &[FaultAction]) -> u64 {
    actions
        .last()
        .map_or(SEED, |action| action.entropy_seed(SEED))
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
        qualification: execution_contract::Requirement::Ordinary,
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
    observation_decode: Spent,
    observation_projection: Spent,
    observation_events: u64,
    observation_payload_bytes: u64,
    observation_decoded_events: u64,
    observation_rebuilds: u64,
    observation_checkpoint_hits: u64,
    observation_checkpoint_evictions: u64,
    observation_checkpoint_rebuild_events: u64,
    observation_checkpoint_rebuild_payload_bytes: u64,
    observation_checkpoint_rebuilds: Spent,
    drops: Spent,
    exact_hits: u64,
    ancestor_hits: u64,
    misses: u64,
    evictions: u64,
    imports: Spent,
    publishes: Spent,
    refusals: u64,
}

const OBSERVATION_CHECKPOINT_LIMIT: usize = 8 * 1024 * 1024;
const PUBLISH_SPACING_WARMUP: u64 = 64;
const MAX_PUBLISH_SPACING: usize = 16;
const OBSERVATION_ATTACHMENT_DOMAIN: &[u8] = b"fault-observations-v1";

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CapturedObservations {
    receipt: HistoryReceipt,
    observations: ObservationCheckpoint,
}

#[derive(Default)]
struct ObservationCursor {
    decoder: Option<SdkDecoder>,
    after: Option<u64>,
}

impl ObservationCursor {
    fn invalidate(&mut self) {
        self.decoder = None;
        self.after = None;
    }
    fn begin_action(&mut self, after: u64) {
        self.after = Some(after);
        if let Some(decoder) = &mut self.decoder {
            decoder.begin_action(after);
        }
    }
}

struct Live {
    key: [u8; 32],
    holder: u64,
    session: Box<dyn SearchSession>,
    final_counters: Option<Vec<(String, u64)>>,
    base_identity: [u8; 32],
    setup: SnapId,
    windows: ActionWindows,
    inherited: Vec<StandingWindow>,
    chain: Chain,
    horizons_run: u64,
    abandoned: bool,
    telemetry: LiveTelemetry,
    cursor: ObservationCursor,
}

static HOLDERS: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static COORDINATOR_SESSION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
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
        let counters = self
            .final_counters
            .take()
            .unwrap_or_else(|| self.work_counters());
        let _ = RETIRED.try_with(|retired| add_counters(&mut retired.borrow_mut(), counters));
    }
}

#[must_use]
pub fn thread_work_telemetry() -> BTreeMap<String, u64> {
    let mut counters = RETIRED.with(|retired| retired.borrow().clone());
    LIVE.with(|slot| {
        if let Some(live) = slot.borrow().as_ref() {
            add_counters(&mut counters, live.work_counters());
        }
    });
    counters
}

pub fn coordinator_retired_work_telemetry() -> Result<BTreeMap<String, u64>, String> {
    LIVE.with(|slot| {
        if slot.borrow().is_some() {
            return Err("coordinator telemetry requires no live session".to_owned());
        }
        Ok(RETIRED.with(|retired| retired.borrow().clone()))
    })
}

pub fn begin_coordinator_session() -> Result<(), Box<dyn Error>> {
    COORDINATOR_SESSION.with(|active| {
        if active.replace(true) {
            Err("a coordinator session scope is already active".into())
        } else {
            Ok(())
        }
    })
}
#[cfg(any(target_os = "macos", test))]
fn drop_target_session() {
    if !COORDINATOR_SESSION.with(|active| active.get()) {
        LIVE.with(|slot| slot.borrow_mut().take());
    }
}
pub fn finish_coordinator_session() -> Result<(), Box<dyn Error>> {
    COORDINATOR_SESSION.with(|active| active.set(false));
    let live = LIVE.with(|slot| slot.borrow_mut().take());
    if let Some(mut live) = live {
        live.final_counters = Some(Vec::new());
        let counters =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| live.work_counters()));
        let telemetry_failed = match counters {
            Ok(counters) => {
                live.final_counters = Some(counters);
                false
            }
            Err(_) => true,
        };
        live.session.shutdown()?;
        if telemetry_failed {
            return Err(
                "coordinator retirement telemetry panicked after cleanup was attempted".into(),
            );
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ThreadResidencyDiagnostics {
    pub entries: usize,
    pub resident_bytes: Option<u64>,
    pub observation_checkpoint_bytes: usize,
    pub charged_bytes: Option<u64>,
}

pub(crate) fn take_job_observation() -> Option<crate::job_observation::FaultJobObservation> {
    let generation = LIVE.with(|slot| {
        slot.borrow()
            .as_ref()
            .and_then(|_| crate::job_observation::generation())
    });
    crate::job_observation::update(|value| value.end_generation = generation);
    crate::job_observation::take()
}

#[must_use]
pub fn thread_residency_diagnostics() -> Option<ThreadResidencyDiagnostics> {
    LIVE.with(|slot| slot.borrow().as_ref().map(Live::residency_diagnostics))
}

#[derive(Debug)]
pub struct FaultTarget {
    config: Arc<Config>,
    actions: std::sync::OnceLock<Vec<FaultAction>>,
    history: FaultHistory,
    observation: FaultObservations,
    action_observations: Vec<FaultObservations>,
    failed: bool,
    watchdog_cutoffs: u64,
    infrastructure_failure: bool,
    execution_ticks: u64,
    guest_horizons_run: u64,
    root_seal: u64,
}

#[cfg(target_os = "macos")]
impl Drop for FaultTarget {
    fn drop(&mut self) {
        drop_target_session();
    }
}

impl FaultTarget {
    pub(crate) fn worker_locality(&self) -> searcher::search::locality::WorkerLocality {
        use searcher::search::locality::{LocalityKey, WorkerLocality};

        LIVE.with(|slot| {
            let slot = slot.borrow();
            let Some(live) = slot.as_ref().filter(|live| {
                live.key == self.config.key && !live.abandoned && !live.session.abandoned()
            }) else {
                return WorkerLocality::default();
            };
            WorkerLocality::new((0..live.chain.links()).filter_map(|at| {
                let receipt = live.chain.provenance(at).ok()?;
                let rank = u8::from(live.chain.observation_checkpoint(at).is_some());
                Some((LocalityKey(receipt.root()), rank))
            }))
        })
    }

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
        let (observation, root_seal, history) = with_live(&config, |live| {
            let observation = live.observe(FaultStop::Deadline)?;
            let history = FaultHistory::genesis(live.chain.provenance(0)?.clone())?;
            Ok((observation, live.windows.root_seal, history))
        })?;
        Ok(Self {
            config,
            actions: std::sync::OnceLock::new(),
            history,
            action_observations: vec![observation.clone()],
            observation,
            failed: false,
            watchdog_cutoffs: 0,
            infrastructure_failure: false,
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
    pub fn infrastructure_failure(&self) -> bool {
        self.infrastructure_failure
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
                .map_err(|error| session_failure("state hash", error.as_ref()))
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
            let result = live.state_hash_current();
            if live.abandoned || live.session.abandoned() {
                *slot = None;
            }
            result
        })
    }

    pub fn event_history_current(&self) -> Result<(u64, [u8; 32]), String> {
        LIVE.with(|cell| {
            let mut slot = cell.borrow_mut();
            let live = slot.as_mut().ok_or("the current job has no live session")?;
            if live.key != self.config.key {
                return Err("the current job session has a different identity".into());
            }
            if live.abandoned || live.session.abandoned() {
                *slot = None;
                return Err(format!("{INFRASTRUCTURE_FAILURE}the current job session was abandoned while reading ordered event history"));
            }
            let result = live.ordered_history();
            if live.abandoned || live.session.abandoned() {
                *slot = None;
            }
            result
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
        self.actions.get_or_init(|| self.history.actions())
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
            let plan = ActionPlan::compile(live.windows, &live.inherited, self.actions())
                .map_err(|e| e.to_string())?;
            let service = debug_configuration(plan.handler.configuration(), message)
                .map_err(|e| e.to_string())?;
            let mut handler =
                DebugHandler::decode(&service.configuration).map_err(|error| error.to_string())?;
            live.cursor.invalidate();
            live.session
                .branch_with_service(
                    snapshot,
                    action_entropy_seed(self.actions()),
                    service,
                    Vec::new(),
                    Vec::new(),
                    Some(plan.coverage_schedule),
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

    #[must_use]
    pub fn endpoint_certificate(&self) -> Option<[u8; 32]> {
        self.history.receipt().certificate()
    }

    pub fn probe<T>(&mut self, inspect: impl FnOnce(&mut Self) -> T) -> T {
        let observation = self.observation.clone();
        let result = inspect(self);
        self.observation = observation;
        result
    }

    pub fn export_root(&self) -> Result<Vec<u8>, String> {
        with_live(&self.config, |live| {
            let (snapshot, moment) = live.session.snapshot().map_err(|e| e.to_string())?;
            let result = active_root_windows(&live.inherited, live.windows, self.actions(), moment)
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
            let observation = live.observe(FaultStop::Deadline)?;
            let history = FaultHistory::genesis(live.chain.provenance(0)?.clone())?;
            Ok((observation, history))
        });
        match result {
            Ok((observation, history)) => {
                self.actions.take();
                self.history = history;
                self.guest_horizons_run = 0;
                self.observation = observation.clone();
                self.action_observations = vec![observation];
                self.failed = false;
                self.watchdog_cutoffs = 0;
                self.infrastructure_failure = false;
            }
            Err(error) => {
                eprintln!("fault target reset failed: {error}");
                self.history = FaultHistory::default();
                self.failed = true;
                self.watchdog_cutoffs = u64::from(error.starts_with(WATCHDOG_CUTOFF));
                self.infrastructure_failure = error.starts_with(INFRASTRUCTURE_FAILURE);
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
        crate::job_observation::origin_start(snapshot.history.len());
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
        crate::job_observation::stage(crate::job_observation::RestoreStage::Session);
        let rebuilt = with_live(&self.config, |live| {
            live.fit_store(1)?;
            if snapshot.history.receipt().domain() != live.key {
                return Err("fault history execution domain differs".to_owned());
            }
            crate::job_observation::stage(crate::job_observation::RestoreStage::Lookup);
            let cached = live
                .ensure_certified_prefix(&snapshot.history, true)?
                .map_err(|_| "restored fault endpoint stopped before its snapshot certificate")?;
            let at = live
                .chain
                .at_snapshot(cached.snap)
                .ok_or("restored endpoint link missing")?;
            let receipt = live.chain.provenance(at)?.clone();
            if &receipt != snapshot.history.receipt() {
                return Err(
                    "restored fault history differs from its snapshot certificate".to_owned(),
                );
            }
            crate::job_observation::stage(crate::job_observation::RestoreStage::Replay);
            live.replay(cached.snap)?;
            crate::job_observation::stage(crate::job_observation::RestoreStage::Certificate);
            if let Some(expected) = expected
                && (live.state_hash_current()? != expected
                    || Some(live.ordered_history()?) != snapshot.event_history)
            {
                return Err(
                    "restored fault endpoint differs from its snapshot certificate".to_owned(),
                );
            }
            crate::job_observation::stage(crate::job_observation::RestoreStage::Observation);
            let mut actual = live.observe(FaultStop::Deadline)?;
            if !snapshot.history.is_empty() {
                actual
                    .parks
                    .retain(|park| park.moment > receipt.previous_moment());
                actual
                    .park_reads
                    .retain(|read| read.moment > receipt.previous_moment());
            }
            if actual != snapshot.observation {
                return Err(
                    "restored fault endpoint observation differs from its snapshot".to_owned(),
                );
            }
            Ok(())
        });
        self.finish_restore(snapshot, rebuilt)
    }

    fn finish_restore(
        &mut self,
        snapshot: &FaultSnapshot,
        rebuilt: Result<(), String>,
    ) -> Result<(), Box<dyn Error>> {
        crate::job_observation::origin_finish(match &rebuilt {
            Ok(()) => crate::job_observation::RestoreOutcome::Succeeded,
            Err(error) if error.starts_with(WATCHDOG_CUTOFF) => {
                crate::job_observation::RestoreOutcome::Cutoff
            }
            Err(_) => crate::job_observation::RestoreOutcome::Failed,
        });
        self.action_observations.clear();
        self.watchdog_cutoffs = 0;
        self.infrastructure_failure = false;
        match rebuilt {
            Ok(()) => {}
            Err(error)
                if error.starts_with(WATCHDOG_CUTOFF)
                    || error.starts_with(INFRASTRUCTURE_FAILURE) =>
            {
                self.actions.take();
                self.history = FaultHistory::default();
                self.observation = FaultObservations::default();
                self.failed = true;
                self.watchdog_cutoffs = u64::from(error.starts_with(WATCHDOG_CUTOFF));
                self.infrastructure_failure = error.starts_with(INFRASTRUCTURE_FAILURE);
                self.action_observations.push(FaultObservations {
                    watchdog_cutoff: self.watchdog_cutoffs != 0,
                    infrastructure_failure: self.infrastructure_failure,
                    ..FaultObservations::default()
                });
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        }
        self.actions.take();
        self.history = snapshot.history.clone();
        self.observation = snapshot.observation.clone();
        self.action_observations = vec![self.observation.clone()];
        self.failed = snapshot.failed;
        self.watchdog_cutoffs = 0;
        self.infrastructure_failure = false;
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
        let event_history = if self.config.uml.is_some() {
            None
        } else {
            Some(self.event_history_current()?)
        };
        self.snapshot_with_endpoint(endpoint, event_history)
    }

    fn snapshot_with_endpoint(
        &self,
        endpoint: FaultEndpointCertificate,
        event_history: Option<(u64, [u8; 32])>,
    ) -> Result<Option<FaultSnapshot>, String> {
        let snapshot = FaultSnapshot {
            history: self.history.clone(),
            observation: self.observation.clone(),
            failed: false,
            endpoint: Some(endpoint),
            event_history,
        };
        validate_snapshot_prefix_certificates(&snapshot, self.config.uml.is_some())?;
        Ok(Some(snapshot))
    }

    pub fn apply(&mut self, action: FaultAction) {
        self.apply_as(action, RunKind::New);
    }

    pub fn apply_and_snapshot(
        &mut self,
        action: FaultAction,
    ) -> Result<Option<FaultSnapshot>, String> {
        self.apply_and_snapshot_as(action, RunKind::New)
    }

    pub fn apply_replayed_and_snapshot(
        &mut self,
        action: FaultAction,
    ) -> Result<Option<FaultSnapshot>, String> {
        self.apply_and_snapshot_as(action, RunKind::Replay)
    }

    fn apply_and_snapshot_as(
        &mut self,
        action: FaultAction,
        kind: RunKind,
    ) -> Result<Option<FaultSnapshot>, String> {
        let endpoint = self.apply_as(action, kind);
        self.verify_integrity().map_err(|error| error.to_string())?;
        match endpoint {
            Some(endpoint) => {
                self.snapshot_with_endpoint(endpoint, self.history.receipt().event_history())
            }
            None => Ok(None),
        }
    }

    pub fn apply_replayed(&mut self, action: FaultAction) {
        self.apply_as(action, RunKind::Replay);
    }

    pub fn apply_replayed_certified(
        &mut self,
        action: FaultAction,
        expected: Option<[u8; 32]>,
    ) -> Result<(), String> {
        let endpoint = self
            .apply_as(action, RunKind::Replay)
            .ok_or("replayed fault prefix stopped before its archived endpoint")?;
        match (self.config.uml.is_some(), endpoint) {
            (true, FaultEndpointCertificate::UmlWholeStateUnavailable) => {
                if expected.is_some() {
                    return Err("UML replay unexpectedly carries a KVM endpoint certificate".into());
                }
            }
            (false, FaultEndpointCertificate::KvmWholeState(_)) => {
                let expected = expected.ok_or("KVM replay prefix lacks an endpoint certificate")?;
                let actual = self
                    .history
                    .receipt()
                    .certificate()
                    .ok_or("captured KVM replay prefix lacks an endpoint certificate")?;
                if actual != expected {
                    return Err(
                        "replayed KVM prefix differs from its archived endpoint certificate".into(),
                    );
                }
            }
            _ => return Err("captured replay endpoint differs from its backend".into()),
        }
        Ok(())
    }

    fn apply_as(&mut self, action: FaultAction, kind: RunKind) -> Option<FaultEndpointCertificate> {
        if self.failed || !self.observation.stop.is_continuable() {
            return None;
        }
        self.action_observations.clear();
        let result = with_live(&self.config, |live| {
            let before = live.horizons_run;
            let (mut observation, receipt) = live.advance(&self.history, action, kind)?;
            observation
                .parks
                .retain(|park| park.moment > self.history.receipt().moment());
            observation
                .park_reads
                .retain(|read| read.moment > self.history.receipt().moment());
            Ok((
                observation,
                live.horizons_run.saturating_sub(before),
                receipt,
            ))
        });
        match result {
            Ok((observation, ran, receipt)) => {
                let endpoint = receipt.as_ref().map(HistoryReceipt::endpoint);
                if let Some(receipt) = receipt {
                    match self.history.push(receipt) {
                        Ok(history) => self.history = history,
                        Err(error) => {
                            eprintln!("fault history extension failed: {error}");
                            self.failed = true;
                            return None;
                        }
                    }
                }
                if endpoint.is_none() {
                    let mut actions = self.actions().to_vec();
                    actions.push(action);
                    self.actions = std::sync::OnceLock::from(actions);
                } else {
                    self.actions.take();
                }
                self.execution_ticks = self
                    .execution_ticks
                    .saturating_add(crate::target::action_ticks(&action));
                self.guest_horizons_run = self.guest_horizons_run.saturating_add(ran);
                self.observation = observation.clone();
                self.action_observations.push(observation);
                endpoint
            }
            Err(error) => {
                eprintln!("fault action {action:?} failed: {error}");
                if error.starts_with(WATCHDOG_CUTOFF) || error.starts_with(INFRASTRUCTURE_FAILURE) {
                    self.watchdog_cutoffs = self
                        .watchdog_cutoffs
                        .saturating_add(u64::from(error.starts_with(WATCHDOG_CUTOFF)));
                    self.infrastructure_failure = error.starts_with(INFRASTRUCTURE_FAILURE);
                    let mut observation = self.observation.clone();
                    observation.parks.clear();
                    observation.park_reads.clear();
                    observation.watchdog_cutoff = error.starts_with(WATCHDOG_CUTOFF);
                    observation.infrastructure_failure = self.infrastructure_failure;
                    self.action_observations.push(observation);
                }
                self.failed = true;
                None
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
            crate::job_observation::new_generation();
        }
        let live = slot.as_mut().ok_or("the session was not initialized")?;
        let result = operation(live);
        if result.is_err() {
            live.cursor.invalidate();
        }
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

struct ExecutedAction {
    observation: FaultObservations,
    snapshot: Option<(SnapId, u64)>,
    proof: EndpointProof,
}

struct ObservedEndpoint {
    receipt: EndpointReceipt,
    decoder: SdkDecoder,
    events: u64,
    payload_bytes: u64,
    decoded: u64,
}

fn execute_observed_endpoint(
    session: &mut dyn SearchSession,
    request: ExecutionRequest,
    handler: &mut dyn ServiceHandler,
    mut decoder: SdkDecoder,
    stopped: &mut Option<StopReason>,
) -> Result<ObservedEndpoint, Box<dyn Error>> {
    let deadline = request.until.deadline.ok_or("endpoint deadline missing")?.0;
    let mut start = ExecutionEndpoint::execute(session, request)?;
    let mut endpoint = loop {
        start = match start {
            ExecutionStart::Decision(decision) => {
                if stop_moment(decision.stop()) >= deadline {
                    break decision.finalize(true)?;
                }
                let resolution = resolution_for_stop(decision.stop(), handler)?;
                decision.resume(resolution)?
            }
            ExecutionStart::Endpoint(endpoint) => break endpoint,
        };
    };
    *stopped = Some(endpoint.page().binding.stop.clone());
    let mut events = 0u64;
    let mut payload_bytes = 0u64;
    let mut decoded = 0u64;
    loop {
        let page = endpoint.page();
        events += page.events.len() as u64;
        payload_bytes += page
            .events
            .iter()
            .map(|event| event.2.len() as u64)
            .sum::<u64>();
        match decoder.append(&page.events) {
            Ok(count) => decoded += count as u64,
            Err(error) => {
                endpoint.abandon();
                return Err(error.into());
            }
        }
        if !endpoint.next_page()? {
            break;
        }
    }
    if let Err(error) = decoder.capture().check_infrastructure_status() {
        endpoint.abandon();
        return Err(error.into());
    }
    let receipt = endpoint.commit()?;
    Ok(ObservedEndpoint {
        receipt,
        decoder,
        events,
        payload_bytes,
        decoded,
    })
}

fn session_failure(what: &str, error: &(dyn Error + 'static)) -> String {
    let message = format!("{what}: {error}");
    match error.downcast_ref::<SessionError>() {
        Some(SessionError::Hung(_)) => format!("{WATCHDOG_CUTOFF}{message}"),
        Some(SessionError::Infrastructure(_) | SessionError::Abandoned) => {
            format!("{INFRASTRUCTURE_FAILURE}{message}")
        }
        _ => message,
    }
}

impl Live {
    fn state_hash_current(&mut self) -> Result<[u8; 32], String> {
        if self.abandoned || self.session.abandoned() {
            return Err(format!(
                "{INFRASTRUCTURE_FAILURE}the current job session was abandoned"
            ));
        }
        let hash = self
            .session
            .state_hash()
            .map_err(|error| session_failure("current job state hash", error.as_ref()))?;
        if self.abandoned || self.session.abandoned() {
            return Err(format!(
                "{INFRASTRUCTURE_FAILURE}the current job session was abandoned while hashing"
            ));
        }
        Ok(hash)
    }

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
        session.set_service_factory(service_factory())?;
        Ok(Box::new(session))
    }

    fn boot(config: &Config) -> Result<Self, String> {
        let boot_started = started();
        let mut session = Self::open(config)
            .map_err(|error| session_failure("fault guest boot", error.as_ref()))?;
        let base_identity = session
            .root_identity()
            .map_err(|error| session_failure("root identity", error.as_ref()))?;
        let (mut setup, mut root_seal) = session.setup_handle();
        let mut inherited = Vec::new();
        if let Some(root) = &config.root {
            (setup, root_seal, inherited) = import_root(session.as_mut(), root)
                .map_err(|error| session_failure("import root", error.as_ref()))?;
            session
                .replay_snapshot(setup)
                .map_err(|error| session_failure("replay imported root", error.as_ref()))?;
        }
        let shared = match &config.cache {
            Some(index) => {
                let setup_hash = session
                    .cache_identity()
                    .map_err(|error| session_failure("setup cache identity", error.as_ref()))?;
                Some(Shared {
                    index: Arc::clone(index),
                    namespace: Namespace::exact(&[
                        &config.key,
                        SERVICE_IDENTITY,
                        &setup_hash,
                        OBSERVATION_ATTACHMENT_DOMAIN,
                    ]),
                })
            }
            None => None,
        };
        let mut telemetry = LiveTelemetry::default();
        telemetry.boot.since(boot_started);
        let mut live = Self {
            final_counters: None,
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
            cursor: ObservationCursor::default(),
        };
        let observation = live.observe(FaultStop::Deadline)?;
        let endpoint = if config.uml.is_some() {
            FaultEndpointCertificate::UmlWholeStateUnavailable
        } else {
            FaultEndpointCertificate::KvmWholeState(live.state_hash_current()?)
        };
        let event_history = if config.uml.is_some() {
            None
        } else {
            Some(live.ordered_history()?)
        };
        let receipt = HistoryReceipt::genesis(config.key, endpoint, event_history, &observation)?;
        live.chain.certify(0, receipt.clone())?;
        let checkpoint = live.capture_observation_checkpoint(&receipt)?;
        live.chain.set_observation_checkpoint(0, checkpoint)?;
        live.fit_store(1)?;
        Ok(live)
    }

    fn work_counters(&self) -> Vec<(String, u64)> {
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
            ("observe_decode", t.observation_decode),
            ("observe_projection", t.observation_projection),
            (
                "observe_checkpoint_rebuild",
                t.observation_checkpoint_rebuilds,
            ),
            ("drop", t.drops),
        ] {
            out.push((format!("session.{name}.count"), spent.count));
            out.push((format!("session.{name}.ns"), spent.nanos));
        }
        out.extend([
            ("observe.events".to_owned(), t.observation_events),
            (
                "observe.payload_bytes".to_owned(),
                t.observation_payload_bytes,
            ),
            (
                "observe.decoded_events".to_owned(),
                t.observation_decoded_events,
            ),
            ("observe.full_rebuilds".to_owned(), t.observation_rebuilds),
            (
                "observe.checkpoint_hits".to_owned(),
                t.observation_checkpoint_hits,
            ),
            (
                "observe.checkpoint_evictions".to_owned(),
                t.observation_checkpoint_evictions,
            ),
            (
                "observe.checkpoint_rebuild_events".to_owned(),
                t.observation_checkpoint_rebuild_events,
            ),
            (
                "observe.checkpoint_rebuild_payload_bytes".to_owned(),
                t.observation_checkpoint_rebuild_payload_bytes,
            ),
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
        out.extend(
            self.session
                .telemetry_counters()
                .into_iter()
                .map(|(name, value)| (format!("vmm.{name}"), value)),
        );
        out
    }

    fn residency_diagnostics(&self) -> ThreadResidencyDiagnostics {
        let mut resident_bytes = Some(0u64);
        for point in self.chain.points() {
            let pages = self.session.snapshot_owned_pages(point.snap);
            resident_bytes = resident_bytes
                .zip(pages)
                .and_then(|(bytes, pages)| bytes.checked_add(pages.checked_mul(4096)?));
        }
        let observation_checkpoint_bytes = self.chain.observation_checkpoint_bytes();
        ThreadResidencyDiagnostics {
            entries: self.chain.links(),
            resident_bytes,
            observation_checkpoint_bytes,
            charged_bytes: resident_bytes
                .and_then(|bytes| bytes.checked_add(observation_checkpoint_bytes as u64)),
        }
    }

    fn ordered_history(&mut self) -> Result<(u64, [u8; 32]), String> {
        self.session
            .event_history()
            .map(|history| (history.count, history.digest))
            .map_err(|error| self.abandon("ordered event history", error.as_ref()))
    }

    fn abandon(&mut self, what: &str, error: &(dyn Error + 'static)) -> String {
        self.abandoned = true;
        self.cursor.invalidate();
        let tail = self.session.console_tail().unwrap_or_default();
        let start = tail.len().saturating_sub(CONSOLE_TAIL);
        eprintln!(
            "fault guest abandoned during {what}: {error}\nconsole tail:\n{}",
            String::from_utf8_lossy(&tail[start..])
        );
        session_failure(what, error)
    }

    fn restore_observation_cursor(&mut self, snapshot: SnapId) -> Result<(), String> {
        self.restore_observation_cursor_with_limit(snapshot, OBSERVATION_CHECKPOINT_LIMIT)
    }

    fn restore_observation_cursor_with_limit(
        &mut self,
        snapshot: SnapId,
        limit: usize,
    ) -> Result<(), String> {
        let Some(at) = self.chain.at_snapshot(snapshot) else {
            return Ok(());
        };
        let receipt = self.chain.provenance(at)?.clone();
        self.cursor.after = (receipt.depth() != 0).then(|| receipt.previous_moment());
        if let Some(checkpoint) = self.chain.observation_checkpoint(at) {
            checkpoint.validate_binding(&receipt)?;
            self.cursor.decoder = Some(checkpoint.decoder().clone());
            self.telemetry.observation_checkpoint_hits += 1;
            return Ok(());
        }
        let pending = self.chain.pending_observation_checkpoint(at).cloned();
        let rebuilt = (|| {
            let verify_started = started();
            let events = self
                .session
                .sdk_events_since(0)
                .map_err(|error| session_failure("restored SDK events", error.as_ref()))?;
            self.telemetry.observation_checkpoint_rebuild_events += events.len() as u64;
            self.telemetry.observation_checkpoint_rebuild_payload_bytes +=
                events.iter().map(|event| event.2.len() as u64).sum::<u64>();
            let mut decoder = SdkDecoder::default();
            if let Some(after) = self.cursor.after {
                decoder.begin_action(after);
            }
            decoder.append(&events)?;
            decoder.capture().check_infrastructure_status()?;
            self.telemetry
                .observation_checkpoint_rebuilds
                .since(verify_started);
            if let Some(expected) = pending.as_ref()
                && &decoder != expected.decoder()
            {
                return Err(
                    "imported observation checkpoint differs from actual SDK history".to_owned(),
                );
            }
            ObservationCheckpoint::capture(&receipt, decoder)
        })();
        match rebuilt {
            Ok(checkpoint) => {
                self.cursor.decoder = Some(checkpoint.decoder().clone());
                self.chain.set_observation_checkpoint(at, checkpoint)?;
                self.fit_observation_store(limit);
                Ok(())
            }
            Err(error) => {
                self.cursor.invalidate();
                if pending.is_some()
                    && let Some(snapshot) = self.chain.remove_snapshot(snapshot)
                {
                    let _ = self.session.drop_snapshot(snapshot);
                }
                Err(error)
            }
        }
    }

    fn replay(&mut self, snapshot: SnapId) -> Result<(), String> {
        self.cursor.invalidate();
        let restore_started = started();
        let result = self.session.replay_snapshot(snapshot);
        self.telemetry.restores.since(restore_started);
        result.map_err(|error| self.abandon("replay", error.as_ref()))?;
        self.restore_observation_cursor(snapshot)
    }

    fn drop_snapshots(&mut self, snapshots: Vec<SnapId>) -> Result<(), String> {
        for snapshot in snapshots {
            let drop_started = started();
            self.session
                .drop_snapshot(snapshot)
                .map_err(|error| session_failure("drop snapshot", error.as_ref()))?;
            self.telemetry.drops.since(drop_started);
            self.telemetry.evictions = self.telemetry.evictions.saturating_add(1);
        }
        Ok(())
    }

    fn fit_observation_store(&mut self, limit: usize) -> bool {
        self.telemetry.observation_checkpoint_evictions +=
            self.chain.discard_observation_checkpoints(limit) as u64;
        let Some(index) = self.chain.shared().map(|shared| Arc::clone(&shared.index)) else {
            return false;
        };
        let report = |live: &Self| {
            index.report_store(
                live.holder,
                live.session
                    .store_bytes()
                    .unwrap_or(0)
                    .saturating_add(live.chain.observation_checkpoint_bytes() as u64),
            )
        };
        if !report(self) {
            return false;
        }
        let discarded = self.chain.discard_observation_checkpoints(0);
        self.telemetry.observation_checkpoint_evictions += discarded as u64;
        if discarded != 0 {
            return report(self);
        }
        true
    }

    fn fit_store(&mut self, keep: usize) -> Result<(), String> {
        while self.fit_observation_store(OBSERVATION_CHECKPOINT_LIMIT) {
            let dropped = self.chain.shed(keep);
            if dropped.is_empty() {
                return Ok(());
            }
            self.drop_snapshots(dropped)?;
        }
        Ok(())
    }

    fn capture_observation_checkpoint(
        &self,
        receipt: &HistoryReceipt,
    ) -> Result<ObservationCheckpoint, String> {
        let decoder = self
            .cursor
            .decoder
            .as_ref()
            .ok_or("captured endpoint lacks actual observation decoder")?;
        ObservationCheckpoint::capture(receipt, decoder.clone())
    }

    fn publish_spacing(&self) -> usize {
        let telemetry = &self.telemetry;
        let captures = telemetry
            .new
            .host
            .count
            .saturating_add(telemetry.rebuild.host.count);
        if captures < PUBLISH_SPACING_WARMUP || telemetry.publishes.count == 0 {
            return 1;
        }
        let publish = u128::from(telemetry.publishes.nanos / telemetry.publishes.count);
        let action = u128::from(
            telemetry
                .new
                .host
                .nanos
                .saturating_add(telemetry.rebuild.host.nanos)
                / captures,
        )
        .max(1);
        let lookups = u128::from(
            telemetry
                .imports
                .count
                .saturating_add(telemetry.misses)
                .saturating_add(1),
        );
        let bound = 2 * publish * u128::from(captures) / (lookups * action);
        let mut spacing = 1_usize;
        while spacing < MAX_PUBLISH_SPACING && ((2 * spacing) as u128).pow(2) <= bound {
            spacing *= 2;
        }
        spacing
    }

    fn publish(
        &mut self,
        parent: Point,
        snap: SnapId,
        moment: u64,
        receipt: &HistoryReceipt,
        checkpoint: &ObservationCheckpoint,
    ) -> Result<Option<Lease>, String> {
        let Some(shared) = self.chain.shared() else {
            return Ok(None);
        };
        let ancestor = self.chain.parent_for(parent);
        let cost = moment
            .saturating_sub(ancestor.map_or(self.chain.point(0).moment, |(point, _)| point.moment));
        let publish_started = started();
        let attachment = consonance_client::session::SnapshotAttachment {
            domain: OBSERVATION_ATTACHMENT_DOMAIN.to_vec(),
            bytes: postcard::to_allocvec(&CapturedObservations {
                receipt: receipt.clone(),
                observations: checkpoint.clone(),
            })
            .map_err(|error| error.to_string())?,
        };
        let published = self.session.publish_snapshot_attached(
            shared.index.as_ref(),
            shared.namespace,
            &receipt.path(),
            ancestor.map(|(point, lease)| (point.snap, lease)),
            snap,
            &attachment,
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
            Err(error) => Err(session_failure("publish snapshot", error.as_ref())),
        }
    }

    fn seal_captured_link(
        &mut self,
        parent: Point,
        snap: SnapId,
        moment: u64,
        receipt: HistoryReceipt,
    ) -> Result<Point, String> {
        let spacing = self.publish_spacing();
        let captured = self
            .capture_observation_checkpoint(&receipt)
            .and_then(|checkpoint| {
                if !receipt.depth().is_multiple_of(spacing) {
                    return Ok((None, checkpoint));
                }
                self.publish(parent, snap, moment, &receipt, &checkpoint)
                    .map(|lease| (lease, checkpoint))
            });
        let (lease, checkpoint) = match captured {
            Ok(captured) => captured,
            Err(error) => {
                self.cursor.invalidate();
                let _ = self.session.drop_snapshot(snap);
                return Err(error);
            }
        };
        let point = Point { snap, moment };
        let dropped = self.chain.push_history(point, lease, receipt)?;
        let at = self
            .chain
            .at_snapshot(snap)
            .ok_or("captured observation link missing")?;
        self.chain.set_observation_checkpoint(at, checkpoint)?;
        self.drop_snapshots(dropped)?;
        self.fit_store(2)?;
        Ok(point)
    }

    #[cfg(test)]
    fn find_path(&mut self, path: [u8; 32], depth: usize) -> Result<Option<usize>, String> {
        Ok(self
            .find_path_selected(path, depth, crate::job_observation::LookupStage::Child)?
            .map(|(at, _)| at))
    }

    fn find_path_selected(
        &mut self,
        path: [u8; 32],
        depth: usize,
        stage: crate::job_observation::LookupStage,
    ) -> Result<Option<(usize, bool)>, String> {
        crate::job_observation::lookup(stage, crate::job_observation::LookupEvent::Attempt);
        if let Some(shared) = self.chain.shared() {
            shared
                .index
                .verify_namespace(shared.namespace)
                .map_err(|error| error.to_string())?;
        }
        if let Some(at) = self.chain.by_path(path) {
            return Ok(Some((at, false)));
        }
        let Some(shared) = self.chain.shared() else {
            return Ok(None);
        };
        let Some(lease) = shared.index.lookup(shared.namespace, &path) else {
            return Ok(None);
        };
        let index = Arc::clone(&shared.index);
        let near = self.chain.point(self.chain.links() - 1).snap;
        let import_started = started();
        crate::job_observation::lookup(stage, crate::job_observation::LookupEvent::ImportAttempt);
        let imported = self
            .session
            .import_cached_receipt(index.as_ref(), &lease, near);
        self.telemetry.imports.since(import_started);
        let imported = match imported {
            Ok(imported) => imported,
            Err(error) => {
                self.chain.release(lease);
                return Err(self.abandon("import snapshot", error.as_ref()));
            }
        };
        let validated = (|| {
            if imported.attachment.domain != OBSERVATION_ATTACHMENT_DOMAIN {
                return Err("imported snapshot lacks captured fault observations".to_owned());
            }
            let captured: CapturedObservations =
                postcard::from_bytes(&imported.attachment.bytes)
                    .map_err(|error| format!("imported fault observations: {error}"))?;
            let receipt = captured.receipt;
            receipt.validate()?;
            if receipt.domain() != self.key || receipt.depth() != depth || receipt.path() != path {
                return Err("imported fault history domain, path or depth differs".to_owned());
            }
            captured.observations.validate_binding(&receipt)?;
            let checkpoint = captured.observations;
            Ok((receipt, checkpoint))
        })();
        let (receipt, checkpoint) = match validated {
            Ok(captured) => captured,
            Err(error) => {
                self.cursor.invalidate();
                self.chain.release(lease);
                let _ = self.session.drop_snapshot(imported.snapshot);
                return Err(error);
            }
        };
        let point = Point {
            snap: imported.snapshot,
            moment: imported.moment,
        };
        let dropped = self.chain.push_history(point, Some(lease), receipt)?;
        let at = self
            .chain
            .at_snapshot(point.snap)
            .ok_or("imported observation link missing")?;
        self.chain
            .set_pending_observation_checkpoint(at, checkpoint)?;
        self.drop_snapshots(dropped)?;
        self.fit_store(2)?;
        crate::job_observation::lookup(stage, crate::job_observation::LookupEvent::ImportCompleted);
        Ok(self.chain.at_snapshot(point.snap).map(|at| (at, true)))
    }

    fn find_history(
        &mut self,
        history: &FaultHistory,
        stage: crate::job_observation::LookupStage,
    ) -> Result<(usize, bool), String> {
        if let Some(at) = self.find_path_selected(history.receipt().path(), history.len(), stage)? {
            return Ok(at);
        }
        for receipt in history.receipts().into_iter().rev().skip(1) {
            if let Some(at) = self.find_path_selected(receipt.path(), receipt.depth(), stage)? {
                return Ok(at);
            }
        }
        Err("captured fault setup history is unavailable".to_owned())
    }

    fn capture_receipt(
        &mut self,
        parent: &HistoryReceipt,
        action: FaultAction,
        observation: &mut FaultObservations,
    ) -> Result<HistoryReceipt, String> {
        self.capture_receipt_with_proof(parent, action, observation, None)
    }

    fn capture_receipt_with_proof(
        &mut self,
        parent: &HistoryReceipt,
        action: FaultAction,
        observation: &mut FaultObservations,
        proof: Option<EndpointProof>,
    ) -> Result<HistoryReceipt, String> {
        if let Some(EndpointProof::WholeStateAndHistory {
            assurance_identity, ..
        }) = &proof
            && *assurance_identity != parent.assurance_identity()
        {
            return Err("execution endpoint assurance differs from parent history".to_owned());
        }
        self.cursor.begin_action(parent.moment());
        observation
            .parks
            .retain(|park| park.moment > parent.moment());
        observation
            .park_reads
            .retain(|read| read.moment > parent.moment());
        let (endpoint, event_history) = match (parent.endpoint(), proof) {
            (
                FaultEndpointCertificate::KvmWholeState(_),
                Some(EndpointProof::WholeStateAndHistory {
                    state_hash,
                    history,
                    ..
                }),
            ) => (
                FaultEndpointCertificate::KvmWholeState(state_hash),
                Some((history.count, history.digest)),
            ),
            (FaultEndpointCertificate::KvmWholeState(_), None) => (
                FaultEndpointCertificate::KvmWholeState(self.state_hash_current()?),
                Some(self.ordered_history()?),
            ),
            (
                FaultEndpointCertificate::UmlWholeStateUnavailable,
                None | Some(EndpointProof::Unavailable),
            ) => (FaultEndpointCertificate::UmlWholeStateUnavailable, None),
            (FaultEndpointCertificate::KvmUnavailableAfterFailure, _) => {
                return Err("uncertified parent endpoint".to_owned());
            }
            _ => return Err("execution endpoint evidence differs from parent backend".to_owned()),
        };
        parent.extend(action, endpoint, event_history, observation)
    }

    fn capture_sealed_receipt(
        &mut self,
        snap: SnapId,
        parent: &HistoryReceipt,
        action: FaultAction,
        observation: &mut FaultObservations,
        proof: EndpointProof,
    ) -> Result<HistoryReceipt, String> {
        match self.capture_receipt_with_proof(parent, action, observation, Some(proof)) {
            Ok(receipt) => Ok(receipt),
            Err(error) => {
                self.cursor.invalidate();
                let _ = self.session.drop_snapshot(snap);
                Err(error)
            }
        }
    }

    fn ensure_certified_prefix(
        &mut self,
        history: &FaultHistory,
        origin: bool,
    ) -> Result<Result<Point, FaultObservations>, String> {
        if let Some(at) = self.chain.certified(history.receipt()) {
            if origin {
                crate::job_observation::selected(false, self.chain.depth(at));
            } else {
                crate::job_observation::source(
                    crate::job_observation::LookupStage::Parent,
                    false,
                    self.chain.depth(at),
                    history.len(),
                );
            }
            self.telemetry.exact_hits = self.telemetry.exact_hits.saturating_add(1);
            return Ok(Ok(self.chain.point(at)));
        }
        let actions = history.actions();
        let (at, imported) = self.find_history(
            history,
            if origin {
                crate::job_observation::LookupStage::Origin
            } else {
                crate::job_observation::LookupStage::Parent
            },
        )?;
        let start = self.chain.depth(at);
        let expected_parent = history
            .ancestor(start)
            .ok_or("fault history ancestor missing")?;
        let mut receipt = self.chain.provenance(at)?.clone();
        if &receipt != expected_parent.receipt() {
            return Err("restored fault prefix differs from its snapshot certificate".to_owned());
        }
        if origin {
            crate::job_observation::selected(imported, start);
        }
        let counter = match Hit::of(start, actions.len()) {
            Hit::Exact => &mut self.telemetry.exact_hits,
            Hit::Ancestor => &mut self.telemetry.ancestor_hits,
            Hit::Miss => &mut self.telemetry.misses,
        };
        *counter = counter.saturating_add(1);
        let dropped = self.chain.truncate(at.saturating_add(1));
        self.drop_snapshots(dropped)?;
        let mut last = self.chain.point(at);
        if origin && start < history.len() {
            crate::job_observation::stage(crate::job_observation::RestoreStage::Rebuild);
        }
        for (offset, expected) in history
            .receipts_after(start)
            .ok_or("fault history suffix missing")?
            .enumerate()
        {
            let index = start + offset;
            let prefix = &actions[..=index];
            if origin {
                crate::job_observation::update(|v| {
                    crate::job_observation::increment(
                        &mut v.origin.rebuild_attempted,
                        &mut v.overflow,
                    )
                });
            }
            let plan = ActionPlan::compile(self.windows, &self.inherited, prefix)
                .map_err(|error| format!("branch configuration: {error}"))?;
            let ExecutedAction {
                mut observation,
                snapshot: sealed,
                proof,
            } = self.run_action(&plan, last, RunKind::Rebuild)?;
            let Some((snap, moment)) = sealed else {
                return Ok(Err(observation));
            };
            let actual = self.capture_sealed_receipt(
                snap,
                &receipt,
                actions[index],
                &mut observation,
                proof,
            )?;
            if &actual != expected {
                let _ = self.session.drop_snapshot(snap);
                return Err(
                    "restored fault prefix differs from its snapshot certificate".to_owned(),
                );
            }
            last = self.seal_captured_link(last, snap, moment, actual.clone())?;
            receipt = actual;
            if origin {
                crate::job_observation::update(|v| {
                    crate::job_observation::increment(
                        &mut v.origin.rebuild_completed,
                        &mut v.overflow,
                    )
                });
            }
        }
        if !origin {
            crate::job_observation::source(
                crate::job_observation::LookupStage::Parent,
                imported,
                start,
                history.len(),
            );
        }
        Ok(Ok(last))
    }

    fn advance(
        &mut self,
        history: &FaultHistory,
        action: FaultAction,
        kind: RunKind,
    ) -> Result<(FaultObservations, Option<HistoryReceipt>), String> {
        if let Some((at, imported)) = self.find_path_selected(
            history.receipt().next_path(action)?,
            history.len() + 1,
            crate::job_observation::LookupStage::Child,
        )? {
            let receipt = self.chain.provenance(at)?.clone();
            if !receipt.follows(history.receipt()) || receipt.action() != Some(action) {
                return Err("cached fault prefix history differs from its parent".to_owned());
            }
            self.telemetry.exact_hits = self.telemetry.exact_hits.saturating_add(1);
            self.replay(self.chain.point(at).snap)?;
            let mut observation = self.observe(FaultStop::Deadline)?;
            if self.capture_receipt(history.receipt(), action, &mut observation)? != receipt {
                return Err("cached fault endpoint differs from captured history".to_owned());
            }
            crate::job_observation::source(
                crate::job_observation::LookupStage::Child,
                imported,
                self.chain.depth(at),
                history.len() + 1,
            );
            return Ok((observation, Some(receipt)));
        }
        let parent = match self.ensure_certified_prefix(history, false)? {
            Ok(parent) => parent,
            Err(mut observation) => {
                observation.stop = FaultStop::Unexpected;
                return Ok((observation, None));
            }
        };
        let mut next = history.actions();
        next.push(action);
        let plan = ActionPlan::compile(self.windows, &self.inherited, &next)
            .map_err(|error| format!("branch configuration: {error}"))?;
        let ExecutedAction {
            mut observation,
            snapshot: sealed,
            proof,
        } = self.run_action(&plan, parent, kind)?;
        let receipt = if let Some((snap, moment)) = sealed {
            let receipt = self.capture_sealed_receipt(
                snap,
                history.receipt(),
                action,
                &mut observation,
                proof,
            )?;
            self.seal_captured_link(parent, snap, moment, receipt.clone())?;
            Some(receipt)
        } else {
            None
        };
        Ok((observation, receipt))
    }

    fn branch(&mut self, parent: Point, plan: &ActionPlan) -> Result<(), String> {
        let config = plan.service_config();
        let coverage_schedule = plan.coverage_schedule.clone();
        let branch_started = started();
        self.cursor.invalidate();
        let result = self.session.branch_with_service(
            parent.snap,
            plan.entropy_seed,
            config,
            Vec::new(),
            Vec::new(),
            Some(coverage_schedule),
        );
        self.telemetry.branches.since(branch_started);
        result.map_err(|error| self.abandon("branch", error.as_ref()))?;
        self.restore_observation_cursor(parent.snap)?;
        let at = self
            .chain
            .at_snapshot(parent.snap)
            .ok_or("branch observation parent missing")?;
        self.cursor
            .begin_action(self.chain.provenance(at)?.moment());
        Ok(())
    }

    fn run_action(
        &mut self,
        plan: &ActionPlan,
        parent: Point,
        kind: RunKind,
    ) -> Result<ExecutedAction, String> {
        self.horizons_run = self.horizons_run.saturating_add(1);
        let at = self
            .chain
            .at_snapshot(parent.snap)
            .ok_or("branch observation parent missing")?;
        let parent_receipt = self.chain.provenance(at)?.clone();
        let branch = if let Some(checkpoint) = self.chain.observation_checkpoint(at) {
            checkpoint.validate_binding(&parent_receipt)?;
            self.cursor.decoder = Some(checkpoint.decoder().clone());
            self.telemetry.observation_checkpoint_hits += 1;
            let mut input = InputSpec::seeded(plan.entropy_seed);
            input.set_config(plan.service_config());
            input.set_payloads(Some(Vec::new()));
            input.set_coverage_schedule(Some(plan.coverage_schedule.clone()));
            Some(
                EndpointBranch::new(parent.snap, &input)
                    .map_err(|error| session_failure("endpoint branch input", error.as_ref()))?,
            )
        } else {
            self.branch(parent, plan)?;
            None
        };
        self.cursor.begin_action(parent_receipt.moment());
        let decoder = self.cursor.decoder.clone().unwrap_or_default();
        let proof = match parent_receipt.endpoint() {
            FaultEndpointCertificate::KvmWholeState(_) => {
                EndpointProofRequest::CapturedWholeStateAndHistory
            }
            FaultEndpointCertificate::UmlWholeStateUnavailable => EndpointProofRequest::Omit,
            FaultEndpointCertificate::KvmUnavailableAfterFailure => {
                return Err("uncertified parent endpoint".to_owned());
            }
        };
        let request = ExecutionRequest {
            branch,
            until: control_proto::StopConditions {
                deadline: Some(control_proto::Moment(plan.deadline)),
                on: control_proto::StopMask::NONE.arm(control_proto::class_bit::ASSERTION),
            },
            resolve: None,
            sdk_cursor: decoder.offset(),
            snapshot_on: EndpointStops::DEADLINE,
            proof,
        };
        let combined_branch = request.branch.is_some();
        let run_started = started();
        let mut handler = plan.handler.clone();
        let mut stopped = None;
        let outcome = execute_observed_endpoint(
            self.session.as_mut(),
            request,
            &mut handler,
            decoder,
            &mut stopped,
        );
        if let Some(stop) = stopped {
            if combined_branch {
                self.telemetry.branches.count += 1;
            }
            if matches!(
                stop,
                StopReason::Deadline { .. } | StopReason::Decision { .. }
            ) {
                self.telemetry.seals.count += 1;
            }
            let runs = match kind {
                RunKind::New => &mut self.telemetry.new,
                RunKind::Rebuild => &mut self.telemetry.rebuild,
                RunKind::Replay => &mut self.telemetry.replay,
            };
            runs.host.since(run_started);
            runs.virtual_nanos = runs
                .virtual_nanos
                .saturating_add(stop_moment(&stop).saturating_sub(parent.moment));
        }
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                self.cursor.invalidate();
                return Err(self.abandon("execution endpoint", error.as_ref()));
            }
        };
        if let StopReason::Crash { vtime, info } = &outcome.receipt.binding.stop {
            let tail = self.session.console_tail().unwrap_or_default();
            let start = tail.len().saturating_sub(CONSOLE_TAIL);
            eprintln!(
                "fault guest crashed at {vtime:?}: {:?} {}\nconsole tail:\n{}",
                info.kind,
                String::from_utf8_lossy(&info.detail),
                String::from_utf8_lossy(&tail[start..])
            );
        }
        let stop = match &outcome.receipt.binding.stop {
            StopReason::Decision { vtime, .. } if vtime.0 >= plan.deadline => FaultStop::Deadline,
            actual => FaultStop::from_stop_reason(actual),
        };
        self.telemetry.observation_events += outcome.events;
        self.telemetry.observation_payload_bytes += outcome.payload_bytes;
        self.telemetry.observation_decoded_events += outcome.decoded;
        let observation =
            FaultObservations::new(outcome.decoder.moment(), outcome.decoder.capture(), stop);
        self.cursor.decoder = Some(outcome.decoder);
        Ok(ExecutedAction {
            observation,
            snapshot: outcome.receipt.snapshot,
            proof: outcome.receipt.proof,
        })
    }

    fn observe(&mut self, stop: FaultStop) -> Result<FaultObservations, String> {
        let result = (|| {
            let mut decoder = self.cursor.decoder.clone().unwrap_or_default();
            if self.cursor.decoder.is_none() {
                self.telemetry.observation_rebuilds += 1;
                if let Some(after) = self.cursor.after {
                    decoder.begin_action(after);
                }
            }
            let fetch_started = started();
            let events = self
                .session
                .sdk_events_since(decoder.offset())
                .map_err(|error| session_failure("SDK events", error.as_ref()));
            self.telemetry.observations.since(fetch_started);
            let events = events?;
            self.telemetry.observation_events += events.len() as u64;
            self.telemetry.observation_payload_bytes +=
                events.iter().map(|event| event.2.len() as u64).sum::<u64>();
            let decode_started = started();
            self.telemetry.observation_decoded_events += decoder.append(&events)? as u64;
            decoder.capture().check_infrastructure_status()?;
            self.telemetry.observation_decode.since(decode_started);
            let projection_started = started();
            let observation = FaultObservations::new(decoder.moment(), decoder.capture(), stop);
            self.telemetry
                .observation_projection
                .since(projection_started);
            self.cursor.decoder = Some(decoder);
            Ok(observation)
        })();
        if result.is_err() {
            self.cursor.invalidate();
        }
        result
    }
}

#[must_use]
pub fn snapshot_memory_charge(snapshot: &FaultSnapshot) -> usize {
    size_of::<FaultSnapshot>()
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
             action=standing-fault-delta-v5;snapshot=uml-checkpoint-v3",
            guest.identity,
            Sha256::digest(kernel),
            Sha256::digest(initramfs),
            config.ram_mib,
            arguments_digest.finalize(),
        ));
    }
    Ok(format!(
        "faults-consonance-whole-vm-v2;session={};\
         action=standing-fault-delta-v5;snapshot=portable-prefix-to-vm-snapshot-v3",
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
    use crate::target::{FaultOperation, decode_sdk_events};
    use consonance_client::cache::{
        LocalIndex,
        extent::{encode, read_extent},
    };

    fn test_branch_config(
        windows: ActionWindows,
        inherited: &[StandingWindow],
        actions: &[FaultAction],
    ) -> Result<ServiceConfig, Box<dyn Error>> {
        Ok(ActionPlan::compile(windows, inherited, actions)?.service_config())
    }

    fn test_coverage_schedule(
        windows: ActionWindows,
        actions: &[FaultAction],
    ) -> Result<CoverageSchedule, Box<dyn Error>> {
        Ok(ActionPlan::compile(windows, &[], actions)?.coverage_schedule)
    }

    #[test]
    fn snapshot_memory_charge_counts_owned_assertions_beside_shared_history() {
        use crate::assertion::{AssertionKind, AssertionOutcome};
        use crate::history::FaultHistoryRetention;
        use searcher::search::archive::{SharedSnapshotMemory, SnapshotRetention};

        let empty = FaultSnapshot::default();
        let mut snapshot = empty.clone();
        let mut string_bytes = 0;
        for index in 0..3 {
            let id = format!("{index}{}", "i".repeat(4096));
            let outcome = AssertionOutcome {
                kind: AssertionKind::Always,
                message: "m".repeat(8192),
                location: "l".repeat(16384),
                passed: true,
                failed: false,
            };
            string_bytes += id.len() + outcome.message.len() + outcome.location.len();
            snapshot.observation.assertions.record(id, outcome);
        }
        snapshot.endpoint = Some(FaultEndpointCertificate::KvmWholeState([17; 32]));
        snapshot.event_history = Some((0, [0; 32]));
        snapshot.history =
            FaultHistory::fixture(&[], &[], snapshot.endpoint, &snapshot.observation);
        validate_snapshot_prefix_certificates(&snapshot, false).unwrap();
        let exclusive = snapshot_memory_charge(&snapshot);
        assert!(exclusive >= snapshot_memory_charge(&empty) + string_bytes);
        let mut retention = FaultHistoryRetention::default();
        let mut memory = SharedSnapshotMemory::default();
        retention.canonicalize(&mut snapshot).unwrap();
        retention.retain(&snapshot, &mut memory);
        let shared = memory.bytes() + retention.metadata_bytes();
        let mut cloned = snapshot.clone();
        retention.canonicalize(&mut cloned).unwrap();
        retention.retain(&cloned, &mut memory);
        assert_eq!(memory.bytes() + retention.metadata_bytes(), shared);
        assert_eq!(snapshot_memory_charge(&cloned), exclusive);
        let budget = shared + snapshot_memory_charge(&empty) + string_bytes / 2;
        assert!(shared + snapshot_memory_charge(&empty) <= budget);
        assert!(shared + exclusive > budget);
        let two_snapshot_budget = shared + 2 * snapshot_memory_charge(&empty) + string_bytes;
        assert!(shared + 2 * snapshot_memory_charge(&empty) <= two_snapshot_budget);
        assert!(shared + exclusive + snapshot_memory_charge(&cloned) > two_snapshot_budget);
        retention.release(&snapshot, &mut memory);
        retention.release(&cloned, &mut memory);
        assert_eq!(memory.bytes(), 0);
    }

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
            boot_cache: None,
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
            boot_cache: None,
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
                    boot_cache: None,
                }
                .session_config(),
                cache: None,
                worker: None,
                uml: None,
            }),
            actions: std::sync::OnceLock::new(),
            history: FaultHistory::fixture(
                &[],
                &[],
                Some(FaultEndpointCertificate::KvmWholeState([17; 32])),
                &FaultObservations::default(),
            ),
            observation: FaultObservations::default(),
            action_observations: Vec::new(),
            failed: false,
            watchdog_cutoffs: 0,
            infrastructure_failure: false,
            execution_ticks: 17,
            guest_horizons_run: 0,
            root_seal: 0,
        }
    }

    fn fixture_genesis() -> HistoryReceipt {
        HistoryReceipt::genesis(
            [0; 32],
            FaultEndpointCertificate::KvmWholeState([17; 32]),
            crate::history::fixture_event_history(FaultEndpointCertificate::KvmWholeState(
                [17; 32],
            )),
            &FaultObservations::default(),
        )
        .unwrap()
    }

    #[test]
    fn endpoint_assurance_is_checked_before_history_or_observation_changes() {
        let mut live = hashed_live([17; 32]);
        let parent = fixture_genesis();
        let action = FaultOperation::Wait(std::num::NonZeroU16::MIN).into();
        let mut observation = FaultObservations::default();
        let before = observation.clone();
        let proof = EndpointProof::WholeStateAndHistory {
            assurance_identity: [31; 32],
            state_hash: [17; 32],
            history: control_proto::EventHistory::default(),
        };
        let error = live
            .capture_receipt_with_proof(&parent, action, &mut observation, Some(proof))
            .unwrap_err();
        assert!(error.contains("assurance differs from parent"));
        assert_eq!(observation, before);
        assert_eq!(live.cursor.after, None);
        parent.validate().unwrap();
    }

    impl Live {
        fn seal_link(
            &mut self,
            actions: &[FaultAction],
            snap: SnapId,
            moment: u64,
            from: u64,
        ) -> Result<Point, String> {
            let parent = self.chain.provenance(0)?.clone();
            let action = *actions.last().ok_or("fixture action missing")?;
            let mut observation = self.observe(FaultStop::Deadline)?;
            let receipt = self.capture_receipt(&parent, action, &mut observation)?;
            self.seal_captured_link(
                Point {
                    snap: self.setup,
                    moment: from,
                },
                snap,
                moment,
                receipt,
            )
        }
        fn find(&mut self, actions: &[FaultAction]) -> Result<usize, String> {
            let mut path = self.chain.provenance(0)?.clone();
            for action in actions {
                path = path.extend(
                    *action,
                    path.endpoint(),
                    crate::history::fixture_event_history(path.endpoint()),
                    &FaultObservations::default(),
                )?;
            }
            self.find_path(path.path(), actions.len())?
                .ok_or_else(|| "fixture path missing".to_owned())
        }
        fn ensure_prefix(
            &mut self,
            actions: &[FaultAction],
        ) -> Result<Result<Point, FaultObservations>, String> {
            let mut history = FaultHistory::genesis(self.chain.provenance(0)?.clone())?;
            for action in actions {
                let (observation, receipt) = self.advance(&history, *action, RunKind::Rebuild)?;
                let Some(receipt) = receipt else {
                    return Ok(Err(observation));
                };
                history = history.push(receipt)?;
            }
            let at = self
                .chain
                .certified(history.receipt())
                .ok_or("fixture endpoint missing")?;
            Ok(Ok(self.chain.point(at)))
        }
    }

    #[derive(Default)]
    struct ResidencyQueries {
        owned: Vec<SnapId>,
        stores: usize,
        missing: Option<SnapId>,
        oversized: bool,
    }

    struct ResidencyFixture;

    impl Drop for ResidencyFixture {
        fn drop(&mut self) {
            LIVE.with(|slot| slot.borrow_mut().take());
            RESIDENCY_QUERIES.with(|slot| slot.borrow_mut().take());
            RETIRED.with(|retired| retired.borrow_mut().clear());
        }
    }

    thread_local! {
        static RESIDENCY_QUERIES: RefCell<Option<ResidencyQueries>> = const { RefCell::new(None) };
    }

    thread_local! {
        static AFTER_RUN_HASH: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
    }

    type PlannedBranch = (u64, ServiceConfig, Option<CoverageSchedule>);

    thread_local! {
        static PLANNED_BRANCHES: RefCell<Option<Vec<PlannedBranch>>> = const { RefCell::new(None) };
    }

    #[derive(Debug, Default)]
    struct TimedOutRestore {
        endpoint_state: consonance_client::endpoint::EndpointState,
        endpoint_abandoned: bool,
        hash: Option<[u8; 32]>,
        hash_calls: u64,
        replay_calls: u64,
        branch_calls: u64,
        sdk_offsets: Vec<u32>,
        sdk_event_count: u64,
        run_events: Option<Vec<consonance_client::session::SdkEvent>>,
        abandon_on_hash: bool,
        hash_failure: bool,
        hash_error: Option<fn() -> SessionError>,
        history: control_proto::EventHistory,
        history_failure: Option<fn() -> SessionError>,
        sdk_failure: Option<fn() -> SessionError>,
        histories: BTreeMap<u64, control_proto::EventHistory>,
        events: BTreeMap<u64, Vec<consonance_client::session::SdkEvent>>,
        current: u64,
        run_hashes: Option<Vec<[u8; 32]>>,
        run_hung: bool,
        terminal_when_exhausted: bool,
        snapshots: BTreeMap<u64, [u8; 32]>,
        moment: u64,
        next_snap: u64,
    }

    impl consonance_client::endpoint::EndpointDriver for TimedOutRestore {
        fn ensure_active(&self) -> Result<(), Box<dyn Error>> {
            if self.abandoned() {
                Err(SessionError::Abandoned.into())
            } else {
                Ok(())
            }
        }
        fn supports_proof(&self) -> bool {
            true
        }
        fn event_count(&self, parent: Option<SnapId>) -> Result<Option<u32>, Box<dyn Error>> {
            Ok(Some(u32::try_from(
                self.events
                    .get(&parent.map_or(self.current, |snap| snap.0))
                    .map_or(0, Vec::len),
            )?))
        }
        fn branch(&mut self, parent: SnapId, input: &InputSpec) -> Result<(), Box<dyn Error>> {
            self.branch_with_service(
                parent,
                input.seed(),
                input.config().clone(),
                input.payloads().unwrap_or_default().to_vec(),
                Vec::new(),
                input.coverage_schedule().cloned(),
            )
        }
        fn run(
            &mut self,
            until: control_proto::StopConditions,
            resolve: Option<Resolution>,
        ) -> Result<StopReason, Box<dyn Error>> {
            assert_eq!(
                until.on,
                control_proto::StopMask::NONE.arm(control_proto::class_bit::ASSERTION)
            );
            let deadline = until.deadline.unwrap().0;
            match resolve {
                Some(resolution) => self.run_resolving(deadline, resolution),
                None => self.run_until(deadline),
            }
        }
        fn snapshot(&mut self) -> Result<(SnapId, u64), Box<dyn Error>> {
            SearchSession::snapshot(self)
        }
        fn page(
            &mut self,
            cursor: u32,
            _end: u32,
        ) -> Result<Vec<consonance_client::endpoint::EndpointEvent>, Box<dyn Error>> {
            self.sdk_events_since(cursor)
        }
        fn proof(&mut self) -> Result<EndpointProof, Box<dyn Error>> {
            Ok(EndpointProof::WholeStateAndHistory {
                assurance_identity: execution_contract::Assurance::ordinary().identity(),
                state_hash: self.state_hash()?,
                history: self.event_history()?,
            })
        }
        fn drop_snapshot(&mut self, snapshot: SnapId) -> Result<(), Box<dyn Error>> {
            SearchSession::drop_snapshot(self, snapshot)
        }
        fn abandon(&mut self) {
            self.endpoint_abandoned = true;
            self.snapshots.clear();
            self.events.clear();
            self.histories.clear();
        }
    }

    impl SearchSession for TimedOutRestore {
        fn execution_endpoint(
            &mut self,
            request: consonance_client::endpoint::EndpointRequest,
        ) -> Result<consonance_client::endpoint::EndpointReply, Box<dyn Error>> {
            let mut state = std::mem::take(&mut self.endpoint_state);
            let result = state.exchange(self, request);
            self.endpoint_state = state;
            result
        }
        fn abandon_execution_endpoint(&mut self) {
            consonance_client::endpoint::EndpointDriver::abandon(self);
        }

        fn shutdown(&mut self) -> Result<(), Box<dyn Error>> {
            if TRACK_SHUTDOWN_TELEMETRY.with(|track| track.get()) {
                SHUTDOWN_CLOSED.with(|closed| closed.set(true));
            }
            BOOTSTRAP_SHUTDOWNS.with(|calls| calls.set(calls.get() + 1));
            if BOOTSTRAP_SHUTDOWN_FAIL.with(|fail| fail.get()) {
                Err("fixture shutdown unconfirmed".into())
            } else {
                Ok(())
            }
        }
        fn setup_handle(&self) -> (SnapId, u64) {
            (SnapId(1), 0)
        }

        fn state_hash(&mut self) -> Result<[u8; 32], Box<dyn Error>> {
            self.hash_calls += 1;
            if let Some(error) = self.hash_error {
                return Err(error().into());
            }
            if self.moment > 0 {
                AFTER_RUN_HASH.with(|hook| {
                    if let Some(hook) = hook.borrow_mut().take() {
                        hook();
                    }
                });
            }
            if self.hash_failure {
                return Err("ordinary hash failure".into());
            }
            self.hash.ok_or_else(|| "state hash unavailable".into())
        }

        fn event_history(&mut self) -> Result<control_proto::EventHistory, Box<dyn Error>> {
            if let Some(failure) = self.history_failure {
                return Err(failure().into());
            }
            Ok(self.history)
        }

        fn console_tail(&mut self) -> Result<Vec<u8>, Box<dyn Error>> {
            Ok(Vec::new())
        }

        fn telemetry_counters(&self) -> Vec<(String, u64)> {
            if TRACK_SHUTDOWN_TELEMETRY.with(|track| track.get()) {
                if SHUTDOWN_TELEMETRY_PANIC.with(|value| value.get()) {
                    panic!("injected retirement telemetry failure");
                }
                assert!(
                    !SHUTDOWN_CLOSED.with(|closed| closed.get()),
                    "telemetry queried after shutdown"
                );
                SHUTDOWN_TELEMETRY_CALLS.with(|calls| calls.set(calls.get() + 1));
            }
            let mut counters = vec![
                ("test.hash_calls".to_owned(), self.hash_calls),
                ("test.replay_calls".to_owned(), self.replay_calls),
                ("test.branch_calls".to_owned(), self.branch_calls),
                ("test.sdk_calls".to_owned(), self.sdk_offsets.len() as u64),
                (
                    "test.sdk_last_offset".to_owned(),
                    self.sdk_offsets.last().copied().unwrap_or(0) as u64,
                ),
                ("test.sdk_events".to_owned(), self.sdk_event_count),
                ("test.snapshots".to_owned(), self.snapshots.len() as u64),
            ];
            if TRACK_SHUTDOWN_TELEMETRY.with(|track| track.get()) {
                counters.push(("test.retirement_units".into(), 1));
            }
            counters
        }

        fn snapshot_owned_pages(&self, snapshot: SnapId) -> Option<u64> {
            RESIDENCY_QUERIES.with(|slot| {
                let mut slot = slot.borrow_mut();
                let queries = slot.as_mut()?;
                queries.owned.push(snapshot);
                (queries.missing != Some(snapshot)).then_some(if queries.oversized {
                    u64::MAX
                } else {
                    1
                })
            })
        }

        fn store_bytes(&self) -> Option<u64> {
            RESIDENCY_QUERIES.with(|slot| {
                let mut slot = slot.borrow_mut();
                let queries = slot.as_mut()?;
                queries.stores += 1;
                Some(8192)
            })
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
            let encoded = encode(&[], &[], &sidecar)?;
            let mut extent = index.extent(encoded.len())?;
            encoded.write(extent.bytes_mut())?;
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

        fn publish_root(
            &self,
            index: &dyn CacheIndex,
            namespace: Namespace,
            target: SnapId,
        ) -> Result<Lease, Box<dyn Error>> {
            self.publish_snapshot_attached(
                index,
                namespace,
                b"root",
                None,
                target,
                &consonance_client::session::SnapshotAttachment::default(),
                1,
            )
        }
        fn import_root(
            &mut self,
            index: &dyn CacheIndex,
            lease: &Lease,
        ) -> Result<(SnapId, u64), Box<dyn Error>> {
            let receipt = self.import_cached_receipt(index, lease, SnapId(1))?;
            Ok((receipt.snapshot, receipt.moment))
        }

        fn publish_snapshot_attached(
            &self,
            index: &dyn CacheIndex,
            namespace: Namespace,
            key: &[u8],
            parent: Option<(SnapId, &Lease)>,
            target: SnapId,
            attachment: &consonance_client::session::SnapshotAttachment,
            cost: u64,
        ) -> Result<Lease, Box<dyn Error>> {
            let hash = *self.snapshots.get(&target.0).ok_or("unknown snapshot")?;
            let history = self
                .histories
                .get(&target.0)
                .copied()
                .unwrap_or(self.history);
            let history = (history.count, history.digest);
            let sidecar = postcard::to_allocvec(&(
                target.0,
                hash,
                history,
                self.moment,
                &attachment.domain,
                &attachment.bytes,
                self.events.get(&target.0).cloned().unwrap_or_default(),
            ))?;
            let commitment = Sha256::digest(postcard::to_allocvec(&(
                hash,
                history,
                &attachment.domain,
                &attachment.bytes,
            ))?)
            .into();
            let encoded = encode(&[], &[], &sidecar)?;
            let mut extent = index.extent(encoded.len())?;
            encoded.write(extent.bytes_mut())?;
            Ok(index.publish(
                namespace,
                key,
                parent.map(|(_, lease)| lease),
                extent,
                commitment,
                cost,
            )?)
        }
        fn import_cached_receipt(
            &mut self,
            index: &dyn CacheIndex,
            lease: &Lease,
            _: SnapId,
        ) -> Result<consonance_client::session::CachedSnapshotReceipt, Box<dyn Error>> {
            if self.hash.is_none() {
                return Err(SessionError::Hung(WALL_LIMIT).into());
            }
            let extents = index.chain(lease)?;
            let delta = read_extent(extents.last().ok_or("missing extent")?.bytes())?;
            type CachedFixture = (
                u64,
                [u8; 32],
                (u64, [u8; 32]),
                u64,
                Vec<u8>,
                Vec<u8>,
                Vec<consonance_client::session::SdkEvent>,
            );
            let (_, hash, history, moment, domain, bytes, events): CachedFixture =
                postcard::from_bytes(delta.sidecar)?;
            let object_commitment =
                Sha256::digest(postcard::to_allocvec(&(hash, history, &domain, &bytes))?).into();
            if index.target_commitment(lease)? != object_commitment {
                return Err("fixture cache commitment differs".into());
            }
            self.next_snap += 1;
            let snapshot = SnapId(self.next_snap);
            self.snapshots.insert(snapshot.0, hash);
            self.events.insert(snapshot.0, events);
            self.histories.insert(
                snapshot.0,
                control_proto::EventHistory {
                    count: history.0,
                    digest: history.1,
                },
            );
            Ok(consonance_client::session::CachedSnapshotReceipt {
                snapshot,
                moment,
                machine_commitment: hash,
                attachment: consonance_client::session::SnapshotAttachment { domain, bytes },
                object_commitment,
            })
        }

        fn replay_snapshot(&mut self, snapshot: SnapId) -> Result<(), Box<dyn Error>> {
            self.replay_calls += 1;
            if self.hash.is_some() {
                if self.run_hashes.is_some() {
                    self.hash = self.snapshots.get(&snapshot.0).copied();
                }
                self.current = snapshot.0;
                if let Some(history) = self.histories.get(&snapshot.0) {
                    self.history = *history;
                }
                Ok(())
            } else {
                Err(SessionError::Hung(WALL_LIMIT).into())
            }
        }

        fn drop_snapshot(&mut self, snapshot: SnapId) -> Result<(), Box<dyn Error>> {
            self.snapshots.remove(&snapshot.0);
            self.histories.remove(&snapshot.0);
            Ok(())
        }

        fn branch_with_service(
            &mut self,
            snapshot: SnapId,
            seed: u64,
            config: ServiceConfig,
            _: Vec<Vec<u8>>,
            _: Vec<(u64, environment::channel::Effect)>,
            coverage: Option<CoverageSchedule>,
        ) -> Result<(), Box<dyn Error>> {
            self.branch_calls += 1;
            PLANNED_BRANCHES.with(|branches| {
                if let Some(branches) = branches.borrow_mut().as_mut() {
                    branches.push((seed, config, coverage));
                }
            });
            if self.hash.is_some() {
                self.current = snapshot.0;
                if let Some(history) = self.histories.get(&snapshot.0) {
                    self.history = *history;
                }
                Ok(())
            } else {
                Err(SessionError::Hung(WALL_LIMIT).into())
            }
        }

        fn run_until(&mut self, deadline: u64) -> Result<StopReason, Box<dyn Error>> {
            if let Some(events) = self.run_events.take() {
                self.events.insert(self.current, events);
            }
            if self.run_hung && self.run_hashes.as_ref().is_none_or(Vec::is_empty) {
                return Err(SessionError::Hung(WALL_LIMIT).into());
            }
            if let Some(hashes) = &mut self.run_hashes {
                if hashes.is_empty() {
                    if self.terminal_when_exhausted {
                        return Ok(StopReason::Quiescent {
                            vtime: control_proto::Moment(self.moment),
                        });
                    }
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
            self.histories.insert(self.next_snap, self.history);
            Ok((SnapId(self.next_snap), self.moment))
        }

        fn sdk_events(
            &mut self,
        ) -> Result<Vec<consonance_client::session::SdkEvent>, Box<dyn Error>> {
            self.sdk_events_since(0)
        }

        fn sdk_events_since(
            &mut self,
            offset: u32,
        ) -> Result<Vec<consonance_client::session::SdkEvent>, Box<dyn Error>> {
            self.sdk_offsets.push(offset);
            if let Some(failure) = self.sdk_failure {
                return Err(failure().into());
            }
            let events: Vec<_> = self
                .events
                .get(&self.current)
                .into_iter()
                .flatten()
                .skip(offset as usize)
                .cloned()
                .collect();
            self.sdk_event_count += events.len() as u64;
            Ok(events)
        }

        fn abandoned(&self) -> bool {
            self.endpoint_abandoned || (self.abandon_on_hash && self.hash_calls != 0)
        }
    }

    thread_local! {
        static SHUTDOWN_TELEMETRY_PANIC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        static TRACK_SHUTDOWN_TELEMETRY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        static SHUTDOWN_CLOSED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        static SHUTDOWN_TELEMETRY_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
        static BOOTSTRAP_SHUTDOWNS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
        static BOOTSTRAP_SHUTDOWN_FAIL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    #[test]
    fn bootstrap_finalization_takes_tls_once_and_reports_shutdown_failure() {
        for fail in [false, true] {
            BOOTSTRAP_SHUTDOWNS.with(|calls| calls.set(0));
            BOOTSTRAP_SHUTDOWN_FAIL.with(|value| value.set(fail));
            let live = timed_out_live();
            LIVE.with(|slot| *slot.borrow_mut() = Some(live));
            let result = finish_coordinator_session();
            assert_eq!(result.is_err(), fail);
            LIVE.with(|slot| assert!(slot.borrow().is_none()));
            finish_coordinator_session().unwrap();
            assert_eq!(BOOTSTRAP_SHUTDOWNS.with(|calls| calls.get()), 1);
        }
        BOOTSTRAP_SHUTDOWN_FAIL.with(|value| value.set(false));
    }

    #[test]
    fn scoped_target_drop_defers_owned_shutdown_and_retires_telemetry_once() {
        let previous = RETIRED.with(|retired| std::mem::take(&mut *retired.borrow_mut()));
        TRACK_SHUTDOWN_TELEMETRY.with(|track| track.set(true));
        SHUTDOWN_CLOSED.with(|closed| closed.set(false));
        SHUTDOWN_TELEMETRY_CALLS.with(|calls| calls.set(0));
        BOOTSTRAP_SHUTDOWN_FAIL.with(|fail| fail.set(true));
        begin_coordinator_session().unwrap();
        assert!(begin_coordinator_session().is_err());
        LIVE.with(|slot| *slot.borrow_mut() = Some(timed_out_live()));
        drop_target_session();
        LIVE.with(|slot| assert!(slot.borrow().is_some()));
        assert!(finish_coordinator_session().is_err());
        assert!(!COORDINATOR_SESSION.with(|active| active.get()));
        LIVE.with(|slot| assert!(slot.borrow().is_none()));
        finish_coordinator_session().unwrap();
        assert_eq!(SHUTDOWN_TELEMETRY_CALLS.with(|calls| calls.get()), 1);
        assert_eq!(
            RETIRED.with(|retired| retired.borrow()["vmm.test.retirement_units"]),
            1
        );
        TRACK_SHUTDOWN_TELEMETRY.with(|track| track.set(false));
        BOOTSTRAP_SHUTDOWN_FAIL.with(|fail| fail.set(false));
        RETIRED.with(|retired| *retired.borrow_mut() = previous);
    }

    #[test]
    fn retirement_telemetry_failure_does_not_prevent_owned_shutdown() {
        TRACK_SHUTDOWN_TELEMETRY.with(|track| track.set(true));
        SHUTDOWN_TELEMETRY_PANIC.with(|value| value.set(true));
        BOOTSTRAP_SHUTDOWNS.with(|calls| calls.set(0));
        BOOTSTRAP_SHUTDOWN_FAIL.with(|value| value.set(false));
        begin_coordinator_session().unwrap();
        LIVE.with(|slot| *slot.borrow_mut() = Some(timed_out_live()));
        let result = finish_coordinator_session();
        SHUTDOWN_TELEMETRY_PANIC.with(|value| value.set(false));
        TRACK_SHUTDOWN_TELEMETRY.with(|track| track.set(false));
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("retirement telemetry panicked")
        );
        assert_eq!(BOOTSTRAP_SHUTDOWNS.with(|calls| calls.get()), 1);
        LIVE.with(|slot| assert!(slot.borrow().is_none()));
        assert!(!COORDINATOR_SESSION.with(|active| active.get()));
    }

    fn timed_out_live() -> Live {
        let mut live = Live {
            final_counters: None,
            key: [0; 32],
            holder: 0,
            session: Box::new(TimedOutRestore {
                endpoint_state: consonance_client::endpoint::EndpointState::default(),
                endpoint_abandoned: false,
                histories: BTreeMap::new(),

                history_failure: None,

                sdk_failure: None,
                history: control_proto::EventHistory::default(),
                hash: None,
                hash_calls: 0,
                replay_calls: 0,
                branch_calls: 0,
                sdk_offsets: Vec::new(),
                sdk_event_count: 0,
                run_events: None,
                abandon_on_hash: false,
                hash_failure: false,
                hash_error: None,
                events: BTreeMap::new(),
                current: 1,
                run_hashes: None,
                run_hung: false,
                terminal_when_exhausted: false,
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
            cursor: ObservationCursor::default(),
        };
        live.chain.certify(0, fixture_genesis()).unwrap();
        live
    }

    fn publishing_live(
        index: &Arc<LocalIndex>,
        namespace: Namespace,
        target: SnapId,
        commitment: [u8; 32],
    ) -> Live {
        let mut live = timed_out_live();
        live.session = Box::new(TimedOutRestore {
            endpoint_state: consonance_client::endpoint::EndpointState::default(),
            endpoint_abandoned: false,
            histories: BTreeMap::new(),

            history_failure: None,

            sdk_failure: None,
            history: control_proto::EventHistory::default(),
            hash: Some(commitment),
            hash_calls: 0,
            replay_calls: 0,
            branch_calls: 0,
            sdk_offsets: Vec::new(),
            sdk_event_count: 0,
            run_events: None,
            abandon_on_hash: false,
            hash_failure: false,
            hash_error: None,
            events: BTreeMap::new(),
            current: target.0,
            run_hashes: None,
            run_hung: false,
            terminal_when_exhausted: false,
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
        live.chain.certify(0, fixture_genesis()).unwrap();
        live
    }

    #[test]
    fn divergent_publication_fails_before_sealing_a_campaign_link() {
        use std::num::NonZeroU16;

        let index = Arc::new(LocalIndex::new(1 << 20));
        let namespace = Namespace::exact(&[b"fault-campaign"]);
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
        assert!(error.contains("snapshot cache key"));
        assert_eq!(second.chain.links(), 1);
        assert_eq!(index.stats().entries, 1);
        assert_eq!(index.stats().duplicates, 0);
        assert_eq!(second.session.state_hash().unwrap(), [0x22; 32]);
        assert!(first.find(&actions).unwrap_err().contains("quarantined"));
        assert!(
            index
                .lookup(namespace, &fixture_genesis().next_path(actions[0]).unwrap())
                .is_none()
        );
        assert!(matches!(
            index.verify_integrity(),
            Err(CacheError::Quarantined { .. })
        ));
        let evidence = index.divergences();
        assert_eq!(evidence.len(), 1);
        assert_ne!(
            evidence[0].existing.target_commitment,
            evidence[0].published.target_commitment
        );
        assert_ne!(
            read_extent(evidence[0].existing.chain[0].bytes())
                .unwrap()
                .sidecar,
            read_extent(evidence[0].published.chain[0].bytes())
                .unwrap()
                .sidecar
        );
    }

    #[test]
    fn equal_publication_with_distinct_extents_seals_a_campaign_link() {
        use std::num::NonZeroU16;

        let index = Arc::new(LocalIndex::new(1 << 20));
        let namespace = Namespace::exact(&[b"fault-campaign"]);
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
        let held = index
            .lookup(namespace, &fixture_genesis().next_path(actions[0]).unwrap())
            .unwrap();
        let extents = index.chain(&held).unwrap();
        assert!(!read_extent(extents[0].bytes()).unwrap().sidecar.is_empty());
        index.release(held);
    }

    #[test]
    fn shared_cache_import_preserves_typed_watchdog_failure() {
        let index = Arc::new(LocalIndex::new(1 << 20));
        let namespace = Namespace::exact(&[b"fault-import-timeout"]);
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
        importer.chain.certify(0, fixture_genesis()).unwrap();
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
                boot_cache: None,
            },
        );
        let run = crate::campaign::FaultCampaignRun {
            vocabulary: crate::bundle::FaultVocabulary::new(1, vec![1]).unwrap(),
        };
        let mut target = unbooted_target();
        let snapshot = FaultSnapshot {
            history: FaultHistory::genesis(fixture_genesis()).unwrap(),
            endpoint: Some(FaultEndpointCertificate::KvmWholeState([17; 32])),
            event_history: Some((0, [0; 32])),
            ..FaultSnapshot::default()
        };
        LIVE.with(|slot| *slot.borrow_mut() = Some(timed_out_live()));
        let error = game.restore(&mut target, &snapshot).unwrap_err();
        assert!(error.to_string().contains("campaign incomplete"), "{error}");
        let result = CampaignJobResult {
            preparation_failure: None,
            hang: None,
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
        endpoint_state: consonance_client::endpoint::EndpointState,
        endpoint_abandoned: bool,
        resolutions: Arc<std::sync::Mutex<Vec<(ServiceConfig, Resolution)>>>,
        branch: Option<ServiceConfig>,
        deadline: u64,
        pending_moment: Option<u64>,
        next_snapshot: u64,
    }

    impl consonance_client::endpoint::EndpointDriver for BoundaryDecisionSession {
        fn ensure_active(&self) -> Result<(), Box<dyn Error>> {
            if self.endpoint_abandoned {
                Err(SessionError::Abandoned.into())
            } else {
                Ok(())
            }
        }
        fn supports_proof(&self) -> bool {
            true
        }
        fn event_count(&self, _parent: Option<SnapId>) -> Result<Option<u32>, Box<dyn Error>> {
            Ok(Some(0))
        }
        fn branch(&mut self, parent: SnapId, input: &InputSpec) -> Result<(), Box<dyn Error>> {
            self.branch_with_service(
                parent,
                input.seed(),
                input.config().clone(),
                Vec::new(),
                Vec::new(),
                input.coverage_schedule().cloned(),
            )
        }
        fn run(
            &mut self,
            until: control_proto::StopConditions,
            resolve: Option<Resolution>,
        ) -> Result<StopReason, Box<dyn Error>> {
            assert_eq!(
                until.on,
                control_proto::StopMask::NONE.arm(control_proto::class_bit::ASSERTION)
            );
            let deadline = until.deadline.unwrap().0;
            match resolve {
                Some(resolution) => self.run_resolving(deadline, resolution),
                None => self.run_until(deadline),
            }
        }
        fn snapshot(&mut self) -> Result<(SnapId, u64), Box<dyn Error>> {
            SearchSession::snapshot(self)
        }
        fn page(
            &mut self,
            _cursor: u32,
            _end: u32,
        ) -> Result<Vec<consonance_client::endpoint::EndpointEvent>, Box<dyn Error>> {
            Ok(Vec::new())
        }
        fn proof(&mut self) -> Result<EndpointProof, Box<dyn Error>> {
            Ok(EndpointProof::WholeStateAndHistory {
                assurance_identity: execution_contract::Assurance::ordinary().identity(),
                state_hash: self.state_hash()?,
                history: self.event_history()?,
            })
        }
        fn drop_snapshot(&mut self, snapshot: SnapId) -> Result<(), Box<dyn Error>> {
            SearchSession::drop_snapshot(self, snapshot)
        }
        fn abandon(&mut self) {
            self.endpoint_abandoned = true;
        }
    }

    impl SearchSession for BoundaryDecisionSession {
        fn execution_endpoint(
            &mut self,
            request: consonance_client::endpoint::EndpointRequest,
        ) -> Result<consonance_client::endpoint::EndpointReply, Box<dyn Error>> {
            let mut state = std::mem::take(&mut self.endpoint_state);
            let result = state.exchange(self, request);
            self.endpoint_state = state;
            result
        }
        fn abandon_execution_endpoint(&mut self) {
            consonance_client::endpoint::EndpointDriver::abandon(self);
        }

        fn setup_handle(&self) -> (SnapId, u64) {
            (SnapId(1), 0)
        }

        fn state_hash(&mut self) -> Result<[u8; 32], Box<dyn Error>> {
            Ok([0; 32])
        }

        fn event_history(&mut self) -> Result<control_proto::EventHistory, Box<dyn Error>> {
            Ok(control_proto::EventHistory::default())
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
            _: u64,
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
            self.endpoint_abandoned
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
            endpoint_state: consonance_client::endpoint::EndpointState::default(),
            endpoint_abandoned: false,
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
            &test_branch_config(live.windows, &[], &actions).unwrap()
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
        let previous = test_branch_config(live.windows, &[], &actions[..1]).unwrap();
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
            endpoint_state: consonance_client::endpoint::EndpointState::default(),
            endpoint_abandoned: false,
            histories: BTreeMap::new(),

            history_failure: None,

            sdk_failure: None,
            history: control_proto::EventHistory::default(),
            hash: Some(hash),
            hash_calls: 0,
            replay_calls: 0,
            branch_calls: 0,
            sdk_offsets: Vec::new(),
            sdk_event_count: 0,
            run_events: None,
            abandon_on_hash: false,
            hash_failure: false,
            hash_error: None,
            events: BTreeMap::new(),
            current: 1,
            run_hashes: None,
            run_hung: false,
            terminal_when_exhausted: false,
            snapshots: BTreeMap::new(),
            moment: 0,
            next_snap: 1,
        });
        live
    }

    #[test]
    fn coordinator_retired_telemetry_refuses_live_before_session_query() {
        let _fixture = ResidencyFixture;
        let counters =
            BTreeMap::from([("vmm.sdk_dispatch.event.response_guard_calls".to_owned(), 17)]);
        RETIRED.with(|retired| *retired.borrow_mut() = counters.clone());
        assert_eq!(coordinator_retired_work_telemetry().unwrap(), counters);
        LIVE.with(|slot| *slot.borrow_mut() = Some(timed_out_live()));
        TRACK_SHUTDOWN_TELEMETRY.with(|track| track.set(true));
        SHUTDOWN_TELEMETRY_PANIC.with(|value| value.set(true));
        let result = std::panic::catch_unwind(coordinator_retired_work_telemetry);
        SHUTDOWN_TELEMETRY_PANIC.with(|value| value.set(false));
        TRACK_SHUTDOWN_TELEMETRY.with(|track| track.set(false));
        LIVE.with(|slot| {
            let mut live = slot.borrow_mut().take().unwrap();
            live.final_counters = Some(Vec::new());
        });
        assert!(result.unwrap().unwrap_err().contains("no live session"));
        assert_eq!(coordinator_retired_work_telemetry().unwrap(), counters);
        let mut owned = coordinator_retired_work_telemetry().unwrap();
        owned.clear();
        assert_eq!(coordinator_retired_work_telemetry().unwrap(), counters);
    }

    #[test]
    fn work_telemetry_never_queries_residency_across_depths_and_pruning() {
        let _fixture = ResidencyFixture;
        for depth in [0, 1, 8, 32, 64] {
            LIVE.with(|slot| slot.borrow_mut().take());
            RETIRED.with(|retired| retired.borrow_mut().clear());
            RESIDENCY_QUERIES.with(|slot| *slot.borrow_mut() = Some(ResidencyQueries::default()));
            let mut live = hashed_live([17; 32]);
            let mut receipt = fixture_genesis();
            for step in 1..=depth {
                receipt = receipt
                    .extend(
                        FaultOperation::Wait(std::num::NonZeroU16::MIN).into(),
                        receipt.endpoint(),
                        crate::history::fixture_event_history(receipt.endpoint()),
                        &FaultObservations::default(),
                    )
                    .unwrap();
                live.chain
                    .push_history(
                        Point {
                            snap: SnapId(step as u64 + 1),
                            moment: step as u64,
                        },
                        None,
                        receipt.clone(),
                    )
                    .unwrap();
            }
            let points: Vec<_> = live.chain.points().map(|point| point.snap).collect();
            LIVE.with(|slot| *slot.borrow_mut() = Some(live));
            let work = thread_work_telemetry();
            assert!(work.contains_key("vmm.test.hash_calls"));
            assert!(!work.contains_key("cache.entries"));
            RESIDENCY_QUERIES.with(|slot| {
                let queries = slot.borrow();
                let queries = queries.as_ref().unwrap();
                assert!(queries.owned.is_empty());
                assert_eq!(queries.stores, 0);
            });
            let diagnostics = thread_residency_diagnostics().unwrap();
            assert_eq!(diagnostics.entries, points.len());
            assert_eq!(diagnostics.resident_bytes, Some(points.len() as u64 * 4096));
            assert_eq!(
                diagnostics.charged_bytes,
                diagnostics
                    .resident_bytes
                    .map(|bytes| bytes + diagnostics.observation_checkpoint_bytes as u64)
            );
            RESIDENCY_QUERIES.with(|slot| {
                let mut queries = slot.borrow_mut();
                let queries = queries.as_mut().unwrap();
                assert_eq!(queries.owned, points);
                assert_eq!(queries.stores, 0);
                queries.owned.clear();
            });
            LIVE.with(|slot| {
                let mut slot = slot.borrow_mut();
                let live = slot.as_mut().unwrap();
                let dropped = live.chain.truncate(1);
                live.drop_snapshots(dropped).unwrap();
            });
            let _ = thread_work_telemetry();
            RESIDENCY_QUERIES
                .with(|slot| assert!(slot.borrow().as_ref().unwrap().owned.is_empty()));
            assert_eq!(thread_residency_diagnostics().unwrap().entries, 1);
            RESIDENCY_QUERIES.with(|slot| {
                let mut queries = slot.borrow_mut();
                let queries = queries.as_mut().unwrap();
                assert_eq!(queries.owned, [SnapId(1)]);
                queries.owned.clear();
                queries.missing = Some(SnapId(1));
            });
            let missing = thread_residency_diagnostics().unwrap();
            assert_eq!(missing.resident_bytes, None);
            assert_eq!(missing.charged_bytes, None);
            RESIDENCY_QUERIES.with(|slot| {
                let mut slot = slot.borrow_mut();
                let queries = slot.as_mut().unwrap();
                queries.owned.clear();
                queries.missing = None;
                queries.oversized = true;
            });
            let oversized = thread_residency_diagnostics().unwrap();
            assert_eq!(oversized.resident_bytes, None);
            assert_eq!(oversized.charged_bytes, None);
            LIVE.with(|slot| slot.borrow_mut().take());
            assert_eq!(thread_residency_diagnostics(), None);
            let retired = thread_work_telemetry();
            assert!(retired.contains_key("vmm.test.hash_calls"));
            assert!(!retired.contains_key("cache.entries"));
            RESIDENCY_QUERIES.with(|slot| {
                let queries = slot.borrow();
                assert_eq!(queries.as_ref().unwrap().owned, [SnapId(1)]);
                assert_eq!(queries.as_ref().unwrap().stores, 0);
            });
            RESIDENCY_QUERIES.with(|slot| *slot.borrow_mut() = None);
        }
    }

    #[test]
    fn budget_reporting_remains_separate_from_work_and_residency() {
        let _fixture = ResidencyFixture;
        RESIDENCY_QUERIES.with(|slot| *slot.borrow_mut() = Some(ResidencyQueries::default()));
        let index = Arc::new(consonance_client::cache::LocalIndex::new(usize::MAX));
        let mut live = hashed_live([17; 32]);
        live.holder = 91;
        live.chain = Chain::new(
            Point {
                snap: SnapId(1),
                moment: 0,
            },
            Some(Shared {
                index: index.clone(),
                namespace: Namespace::exact(&[b"telemetry-budget"]),
            }),
        );
        let decoder = SdkDecoder::default();
        let observation =
            FaultObservations::new(decoder.moment(), decoder.capture(), FaultStop::Deadline);
        let endpoint = FaultEndpointCertificate::KvmWholeState([17; 32]);
        let receipt = HistoryReceipt::genesis(
            [0; 32],
            endpoint,
            crate::history::fixture_event_history(endpoint),
            &observation,
        )
        .unwrap();
        live.chain.certify(0, receipt.clone()).unwrap();
        live.chain
            .set_observation_checkpoint(
                0,
                ObservationCheckpoint::capture(&receipt, decoder).unwrap(),
            )
            .unwrap();
        let checkpoints = live.chain.observation_checkpoint_bytes();
        assert!(checkpoints > 0);
        let _ = live.work_counters();
        RESIDENCY_QUERIES.with(|slot| assert_eq!(slot.borrow().as_ref().unwrap().stores, 0));
        live.fit_store(1).unwrap();
        assert_eq!(index.stats().stored, 8192 + checkpoints);
        RESIDENCY_QUERIES.with(|slot| {
            let slot = slot.borrow();
            assert_eq!(slot.as_ref().unwrap().stores, 1);
            assert!(slot.as_ref().unwrap().owned.is_empty());
        });
        let diagnostics = live.residency_diagnostics();
        assert_eq!(diagnostics.resident_bytes, Some(4096));
        assert_eq!(diagnostics.observation_checkpoint_bytes, checkpoints);
        assert_eq!(diagnostics.charged_bytes, Some(4096 + checkpoints as u64));
        drop(live);
        assert_eq!(index.stats().stored, 0);
        RESIDENCY_QUERIES.with(|slot| *slot.borrow_mut() = None);
    }

    fn scripted_capture_live(hashes: Vec<[u8; 32]>, abandon_on_hash: bool) -> Live {
        scripted_capture_live_event_history(
            hashes,
            abandon_on_hash,
            control_proto::EventHistory::default(),
        )
    }

    fn fixture_session() -> TimedOutRestore {
        TimedOutRestore {
            endpoint_state: consonance_client::endpoint::EndpointState::default(),
            endpoint_abandoned: false,
            histories: BTreeMap::new(),
            history: control_proto::EventHistory::default(),
            history_failure: None,
            sdk_failure: None,
            hash: Some([17; 32]),
            hash_calls: 0,
            replay_calls: 0,
            branch_calls: 0,
            sdk_offsets: Vec::new(),
            sdk_event_count: 0,
            run_events: None,
            abandon_on_hash: false,
            hash_failure: false,
            hash_error: None,
            events: BTreeMap::new(),
            current: 1,
            run_hashes: None,
            run_hung: false,
            terminal_when_exhausted: false,
            snapshots: BTreeMap::from([(1, [17; 32])]),
            moment: 0,
            next_snap: 1,
        }
    }

    fn scripted_capture_live_event_history(
        hashes: Vec<[u8; 32]>,
        abandon_on_hash: bool,
        history: control_proto::EventHistory,
    ) -> Live {
        let mut live = hashed_live([17; 32]);
        live.session = Box::new(TimedOutRestore {
            endpoint_state: consonance_client::endpoint::EndpointState::default(),
            endpoint_abandoned: false,
            histories: BTreeMap::new(),

            history,
            history_failure: None,
            sdk_failure: None,
            hash: Some([17; 32]),
            hash_calls: 0,
            replay_calls: 0,
            branch_calls: 0,
            sdk_offsets: Vec::new(),
            sdk_event_count: 0,
            run_events: None,
            abandon_on_hash,
            hash_failure: false,
            hash_error: None,
            events: BTreeMap::new(),
            current: 1,
            run_hashes: Some(hashes),
            run_hung: false,
            terminal_when_exhausted: false,
            snapshots: BTreeMap::from([(1, [17; 32])]),
            moment: 0,
            next_snap: 1,
        });
        live
    }

    fn capture_hash_calls() -> u64 {
        thread_work_telemetry()["vmm.test.hash_calls"]
    }

    fn capture_fixture_workload(target: &FaultTarget) -> crate::campaign::FaultWorkload {
        let backend = target
            .config
            .uml
            .as_ref()
            .map_or(GuestBackend::Vm, |launch| {
                GuestBackend::Uml(Box::new(UmlGuest {
                    profile: launch.profile.clone(),
                    profile_identity_sha256: launch.expected_profile_identity_sha256.clone(),
                    host: launch.expected_host.clone(),
                    identity: "fixture-uml".into(),
                    work_parent: PathBuf::new(),
                }))
            });
        crate::campaign::fixture_workload(
            &[],
            &[],
            &FaultConfig {
                knobs: Vec::new(),
                ram_mib: DEFAULT_RAM_MIB,
                backend,
                root: None,
                boot_cache: None,
            },
        )
    }

    #[test]
    fn import_capture_hashes_every_actual_prefix_once_and_keeps_certificates() {
        use searcher::search::campaign::TargetExecution;
        let action = FaultOperation::Wait(std::num::NonZeroU16::MIN).into();
        for depth in [1, 3] {
            let mut target = unbooted_target();
            let workload = capture_fixture_workload(&target);
            let hashes = (0..depth).map(|at| [17 + at as u8; 32]).collect::<Vec<_>>();
            LIVE.with(|slot| {
                *slot.borrow_mut() = Some(scripted_capture_live_event_history(
                    hashes.clone(),
                    false,
                    control_proto::EventHistory {
                        count: 3,
                        digest: [11; 32],
                    },
                ))
            });
            let before = capture_hash_calls();
            let mut milestones = crate::archive::FaultMilestones::default();
            let mut certificates = Vec::new();
            let mut endpoint = None;
            for (at, hash) in hashes.iter().enumerate() {
                let snapshot = workload
                    .apply_action_and_snapshot(&mut target, &action, &mut milestones)
                    .unwrap()
                    .unwrap();
                assert_eq!(capture_hash_calls() - before, at as u64 + 1);
                assert_eq!(snapshot.history.len(), at + 1);
                assert_eq!(snapshot.event_history, Some((3, [11; 32])));
                assert_eq!(
                    snapshot.endpoint,
                    Some(FaultEndpointCertificate::KvmWholeState(*hash))
                );
                let certificate = workload
                    .candidate_replay_certificate(&snapshot)
                    .unwrap()
                    .unwrap();
                assert_eq!(certificate, snapshot.history.certificates()[at].unwrap());
                certificates.push(Some(certificate));
                assert_eq!(snapshot.history.certificates(), certificates);
                endpoint = Some(snapshot);
            }
            assert_eq!(capture_hash_calls() - before, depth as u64);
            assert_eq!(Some(workload.snapshot(&mut target).unwrap()), endpoint);
            assert_eq!(capture_hash_calls() - before, depth as u64 + 1);
            LIVE.with(|slot| slot.borrow_mut().take());
        }
    }

    #[test]
    fn bounded_import_and_startup_capture_refuse_stale_success_after_stops() {
        use searcher::search::campaign::TargetExecution;
        let action = FaultOperation::Wait(std::num::NonZeroU16::MIN).into();
        for startup in [false, true] {
            for failure in ["terminal", "ordinary", "watchdog"] {
                let mut target = unbooted_target();
                let workload = capture_fixture_workload(&target);
                let mut live = hashed_live([17; 32]);
                live.session = Box::new(TimedOutRestore {
                    endpoint_state: consonance_client::endpoint::EndpointState::default(),
                    endpoint_abandoned: false,
                    run_hashes: Some(vec![[17; 32]]),
                    run_hung: failure == "watchdog",
                    terminal_when_exhausted: failure == "terminal",
                    ..fixture_session()
                });
                LIVE.with(|slot| *slot.borrow_mut() = Some(live));
                let mut milestones = crate::archive::FaultMilestones::default();
                if startup {
                    let result =
                        crate::campaign::capture_replayed_prefix(&mut target, &[action, action]);
                    assert!(result.is_err(), "startup {failure}");
                    if failure == "watchdog" {
                        assert!(
                            result
                                .unwrap_err()
                                .to_string()
                                .contains("campaign incomplete")
                        );
                    }
                } else {
                    let first = workload
                        .apply_action_and_snapshot(&mut target, &action, &mut milestones)
                        .unwrap()
                        .unwrap();
                    assert_eq!(first.history.len(), 1);
                    let result =
                        workload.apply_action_and_snapshot(&mut target, &action, &mut milestones);
                    if failure == "watchdog" {
                        assert!(
                            result
                                .unwrap_err()
                                .to_string()
                                .contains("campaign incomplete")
                        );
                    } else {
                        assert!(result.unwrap().is_none(), "import {failure}");
                    }
                }
                assert_eq!(target.history.len(), 1);
                assert_eq!(target.watchdog_cutoffs() != 0, failure == "watchdog");
                assert_eq!(target.failed(), failure != "terminal");
                assert!(target.snapshot().unwrap().is_none());
                LIVE.with(|slot| slot.borrow_mut().take());
            }
        }
    }

    #[test]
    fn startup_capture_rejects_early_terminal_and_unavailable_empty_endpoint() {
        let action = FaultOperation::Wait(std::num::NonZeroU16::MIN).into();
        let mut target = unbooted_target();
        LIVE.with(|slot| *slot.borrow_mut() = Some(hashed_live([17; 32])));
        assert!(
            crate::campaign::capture_replayed_prefix(&mut target, &[action, action])
                .unwrap_err()
                .to_string()
                .contains("terminated before")
        );
        assert_eq!(target.actions().len(), 1);
        assert!(!target.failed());
        LIVE.with(|slot| slot.borrow_mut().take());
        let mut target = unbooted_target();
        assert!(
            crate::campaign::capture_replayed_prefix(&mut target, &[])
                .unwrap_err()
                .to_string()
                .contains("unavailable")
        );
        assert_eq!(target.history.len(), 0);
        LIVE.with(|slot| slot.borrow_mut().take());
    }

    #[test]
    fn replay_and_import_capture_preserve_uml_unavailability_without_kvm_hashing() {
        use searcher::search::campaign::TargetExecution;
        let action = FaultOperation::Wait(std::num::NonZeroU16::MIN).into();
        for startup in [false, true] {
            for depth in [0, 1, 3] {
                let mut target = unbooted_uml_target();
                let workload = capture_fixture_workload(&target);
                let mut live = scripted_capture_live(vec![[17; 32]; depth], false);
                live.session = Box::new(TimedOutRestore {
                    endpoint_state: consonance_client::endpoint::EndpointState::default(),
                    endpoint_abandoned: false,
                    run_hashes: Some(vec![[17; 32]; depth]),
                    history_failure: Some(|| SessionError::Hung(WALL_LIMIT)),
                    ..fixture_session()
                });
                live.chain = Chain::new(
                    Point {
                        snap: SnapId(1),
                        moment: 0,
                    },
                    None,
                );
                live.chain
                    .certify(0, target.history.receipt().clone())
                    .unwrap();
                LIVE.with(|slot| *slot.borrow_mut() = Some(live));
                let before = capture_hash_calls();
                let snapshot = if startup {
                    crate::campaign::capture_replayed_prefix(&mut target, &vec![action; depth])
                        .unwrap()
                } else {
                    let mut milestones = crate::archive::FaultMilestones::default();
                    let mut endpoint = None;
                    for _ in 0..depth {
                        endpoint = workload
                            .apply_action_and_snapshot(&mut target, &action, &mut milestones)
                            .unwrap();
                    }
                    endpoint.unwrap_or_else(|| workload.snapshot(&mut target).unwrap())
                };
                assert_eq!(capture_hash_calls(), before);
                assert_eq!(snapshot.history.len(), depth);
                assert_eq!(
                    snapshot.endpoint,
                    Some(FaultEndpointCertificate::UmlWholeStateUnavailable)
                );
                assert_eq!(snapshot.event_history, None);
                assert_eq!(
                    workload.candidate_replay_certificate(&snapshot).unwrap(),
                    None
                );
                assert_eq!(snapshot.history.certificates(), vec![None; depth]);
                LIVE.with(|slot| slot.borrow_mut().take());
            }
        }
    }

    #[test]
    fn branch_prefix_capture_hashes_each_action_once_and_empty_setup_freshly() {
        let action = FaultOperation::Wait(std::num::NonZeroU16::MIN).into();
        for depth in [0, 1, 3] {
            let mut target = unbooted_target();
            LIVE.with(|slot| {
                *slot.borrow_mut() = Some(scripted_capture_live(vec![[17; 32]; depth], false));
            });
            let before = capture_hash_calls();
            let replay_before = thread_work_telemetry()["run.replay.count"];
            let new_before = thread_work_telemetry()["run.new.count"];
            let snapshot =
                crate::campaign::capture_replayed_prefix(&mut target, &vec![action; depth])
                    .unwrap();
            assert_eq!(capture_hash_calls() - before, depth.max(1) as u64);
            assert_eq!(snapshot.history.len(), depth);
            assert_eq!(snapshot.observation, *target.observation());
            if depth != 0 {
                assert_eq!(
                    thread_work_telemetry()["run.replay.count"] - replay_before,
                    depth as u64
                );
                assert_eq!(thread_work_telemetry()["run.new.count"] - new_before, 0);
            }
            assert_eq!(target.snapshot().unwrap(), Some(snapshot));
            assert_eq!(capture_hash_calls() - before, depth.max(1) as u64 + 1);
        }
    }

    #[test]
    fn action_capture_hashes_once_and_standalone_snapshot_reads_the_live_endpoint() {
        let mut target = unbooted_target();
        LIVE.with(|slot| *slot.borrow_mut() = Some(scripted_capture_live(vec![[17; 32]], false)));
        let before = capture_hash_calls();
        let snapshot = target
            .apply_and_snapshot(FaultOperation::Wait(std::num::NonZeroU16::MIN).into())
            .unwrap()
            .unwrap();
        assert_eq!(capture_hash_calls() - before, 1);
        assert_eq!(
            snapshot.endpoint,
            Some(FaultEndpointCertificate::KvmWholeState([17; 32]))
        );
        assert_eq!(
            snapshot.history.certificates(),
            vec![Some(
                replay_endpoint_certificate(
                    [17; 32],
                    snapshot.event_history.unwrap(),
                    &snapshot.observation,
                    false
                )
                .unwrap()
            )]
        );
        let before = capture_hash_calls();
        assert_eq!(target.snapshot().unwrap(), Some(snapshot.clone()));
        assert_eq!(capture_hash_calls() - before, 1);
        LIVE.with(|slot| *slot.borrow_mut() = Some(hashed_live([23; 32])));
        assert!(target.snapshot().unwrap_err().contains("final prefix"));
        LIVE.with(|slot| slot.borrow_mut().as_mut().unwrap().key = [1; 32]);
        assert!(
            target
                .snapshot()
                .unwrap_err()
                .contains("different identity")
        );
        LIVE.with(|slot| {
            let mut slot = slot.borrow_mut();
            let live = slot.as_mut().unwrap();
            live.key = [0; 32];
            live.abandoned = true;
        });
        assert!(target.snapshot().unwrap_err().contains("abandoned"));
        LIVE.with(|slot| assert!(slot.borrow().is_none()));
        assert!(target.snapshot().unwrap_err().contains("unavailable"));
        assert_eq!(
            snapshot.endpoint,
            Some(FaultEndpointCertificate::KvmWholeState([17; 32]))
        );
    }

    #[test]
    fn campaign_parent_replay_reports_integrity_failure_before_generic_replay_error() {
        let action = FaultOperation::Wait(std::num::NonZeroU16::MIN).into();
        let index = Arc::new(LocalIndex::new(1 << 20));
        let namespace = Namespace::exact(&[b"replay-integrity-priority"]);
        let mut first = publishing_live(&index, namespace, SnapId(2), [0x11; 32]);
        first.seal_link(&[action], SnapId(2), 10, 0).unwrap();
        let mut second = publishing_live(&index, namespace, SnapId(3), [0x22; 32]);
        let mut target = unbooted_target();
        Arc::get_mut(&mut target.config).unwrap().cache = Some(index.clone());
        LIVE.with(|slot| *slot.borrow_mut() = Some(scripted_capture_live(vec![[17; 32]], false)));
        let origin = target.snapshot().unwrap().unwrap();
        AFTER_RUN_HASH.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                assert!(second.seal_link(&[action], SnapId(3), 10, 0).is_err());
            }));
        });
        let error = crate::campaign::execute_job(
            &mut target,
            &origin,
            &[action],
            &[Some([99; 32])],
            crate::archive::FaultMilestones::default(),
            &[],
            searcher::search::rollout::RolloutStop::new(false),
        )
        .unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<CacheError>(),
                Some(CacheError::Quarantined { .. })
            ),
            "{error}"
        );
        assert_eq!(target.history.len(), 0);
        assert!(target.failed());
        LIVE.with(|slot| assert_eq!(slot.borrow().as_ref().unwrap().horizons_run, 1));
        AFTER_RUN_HASH.with(|hook| assert!(hook.borrow().is_none()));
        assert!(index.verify_integrity().is_err());
        LIVE.with(|slot| slot.borrow_mut().take());
    }

    #[test]
    fn campaign_parent_replay_verifies_each_capture_without_rehashing_it() {
        let action = FaultOperation::Wait(std::num::NonZeroU16::MIN).into();
        for hashes in [vec![[17; 32]], vec![[17; 32], [23; 32]]] {
            let mut target = unbooted_target();
            LIVE.with(|slot| {
                *slot.borrow_mut() = Some(scripted_capture_live(hashes.clone(), false))
            });
            let origin = target.snapshot().unwrap().unwrap();
            let certificates = hashes
                .iter()
                .map(|hash| {
                    Some(
                        replay_endpoint_certificate(
                            *hash,
                            (0, [0; 32]),
                            &FaultObservations::default(),
                            false,
                        )
                        .unwrap(),
                    )
                })
                .collect::<Vec<_>>();
            let before = capture_hash_calls();
            let result = crate::campaign::execute_job(
                &mut target,
                &origin,
                &vec![action; hashes.len()],
                &certificates,
                crate::archive::FaultMilestones::default(),
                &[],
                searcher::search::rollout::RolloutStop::new(false),
            )
            .unwrap();
            assert_eq!(capture_hash_calls() - before, hashes.len() as u64 + 1);
            assert_eq!(target.guest_horizons_run(), hashes.len() as u64);
            assert_eq!(target.actions().len(), hashes.len());
            assert!(result.actions.is_empty());
            assert!(!target.failed());
            LIVE.with(|slot| slot.borrow_mut().take());
        }
    }

    #[test]
    fn campaign_parent_replay_rejects_intermediate_divergence_before_convergence() {
        let action = FaultOperation::Wait(std::num::NonZeroU16::MIN).into();
        let final_certificate = replay_endpoint_certificate(
            [23; 32],
            (0, [0; 32]),
            &FaultObservations::default(),
            false,
        )
        .unwrap();
        for wrong in [
            Some(final_certificate),
            Some(
                replay_endpoint_certificate(
                    [17; 32],
                    (0, [11; 32]),
                    &FaultObservations::default(),
                    false,
                )
                .unwrap(),
            ),
            None,
        ] {
            let mut target = unbooted_target();
            LIVE.with(|slot| {
                *slot.borrow_mut() = Some(scripted_capture_live(vec![[17; 32], [23; 32]], false))
            });
            let origin = target.snapshot().unwrap().unwrap();
            let before = capture_hash_calls();
            let error = crate::campaign::execute_job(
                &mut target,
                &origin,
                &[action, action],
                &[wrong, Some(final_certificate)],
                crate::archive::FaultMilestones::default(),
                &[],
                searcher::search::rollout::RolloutStop::new(false),
            )
            .unwrap_err();
            assert!(error.to_string().contains("certificate"), "{error}");
            assert_eq!(target.guest_horizons_run(), 1);
            assert_eq!(capture_hash_calls() - before, 2);
            LIVE.with(|slot| slot.borrow_mut().take());
        }
    }

    #[test]
    fn campaign_parent_replay_watchdog_after_success_is_a_hung_job() {
        let action = FaultOperation::Wait(std::num::NonZeroU16::MIN).into();
        let mut target = unbooted_target();
        let mut live = scripted_capture_live(Vec::new(), false);
        live.session = Box::new(TimedOutRestore {
            endpoint_state: consonance_client::endpoint::EndpointState::default(),
            endpoint_abandoned: false,
            run_hashes: Some(vec![[17; 32]]),
            run_hung: true,
            ..fixture_session()
        });
        LIVE.with(|slot| *slot.borrow_mut() = Some(live));
        let origin = target.snapshot().unwrap().unwrap();
        let certificate = Some(
            replay_endpoint_certificate(
                [17; 32],
                (0, [0; 32]),
                &FaultObservations::default(),
                false,
            )
            .unwrap(),
        );
        let result = crate::campaign::execute_job(
            &mut target,
            &origin,
            &[action, action],
            &[certificate, certificate],
            crate::archive::FaultMilestones::default(),
            &[],
            searcher::search::rollout::RolloutStop::new(false),
        )
        .unwrap();
        assert_eq!(result.hang, Some(Vec::new()));
        assert!(result.actions.is_empty());
        assert!(result.preparation_failure.is_none());
        assert_eq!(target.history.len(), 1);
        assert_eq!(target.watchdog_cutoffs(), 1);
        assert!(target.failed());
        assert!(target.snapshot().unwrap().is_none());
        LIVE.with(|slot| assert!(slot.borrow().is_none()));
    }

    #[test]
    fn campaign_parent_replay_terminal_endpoint_cannot_reuse_the_origin_certificate() {
        let action = FaultOperation::Wait(std::num::NonZeroU16::MIN).into();
        let mut target = unbooted_target();
        let mut live = hashed_live([17; 32]);
        live.session = Box::new(fixture_session());
        LIVE.with(|slot| *slot.borrow_mut() = Some(live));
        let origin = target.snapshot().unwrap().unwrap();
        let error = crate::campaign::execute_job(
            &mut target,
            &origin,
            &[action],
            &[origin.history.receipt().certificate()],
            crate::archive::FaultMilestones::default(),
            &[],
            searcher::search::rollout::RolloutStop::new(false),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("replayed fault prefix stopped"),
            "{error}"
        );
        assert!(!error.to_string().contains("campaign incomplete"));
        assert!(!target.failed());
        assert!(!target.observation().stop.is_continuable());
        assert!(target.history.is_empty());
        assert!(target.snapshot().unwrap().is_none());
        LIVE.with(|slot| slot.borrow_mut().take());
    }

    #[test]
    fn certified_uml_parent_replay_never_requests_a_kvm_hash() {
        let action = FaultOperation::Wait(std::num::NonZeroU16::MIN).into();
        for expected in [None, Some([17; 32])] {
            let mut target = unbooted_uml_target();
            let mut live = scripted_capture_live(vec![[17; 32]], false);
            live.chain = Chain::new(
                Point {
                    snap: SnapId(1),
                    moment: 0,
                },
                None,
            );
            live.chain
                .certify(0, target.history.receipt().clone())
                .unwrap();
            LIVE.with(|slot| *slot.borrow_mut() = Some(live));
            let origin = target.snapshot().unwrap().unwrap();
            let before = capture_hash_calls();
            let result = crate::campaign::execute_job(
                &mut target,
                &origin,
                &[action],
                &[expected],
                crate::archive::FaultMilestones::default(),
                &[],
                searcher::search::rollout::RolloutStop::new(false),
            );
            assert_eq!(result.is_ok(), expected.is_none());
            assert_eq!(capture_hash_calls(), before);
            assert_eq!(target.history.receipt().certificate(), None);
            LIVE.with(|slot| slot.borrow_mut().take());
        }
    }

    #[test]
    fn campaign_suffix_captures_each_action_with_one_hash() {
        let mut target = unbooted_target();
        LIVE.with(|slot| {
            *slot.borrow_mut() = Some(scripted_capture_live(vec![[17; 32], [23; 32]], false))
        });
        let origin = target.snapshot().unwrap().unwrap();
        let before = capture_hash_calls();
        let action = FaultOperation::Wait(std::num::NonZeroU16::MIN).into();
        let result = crate::campaign::execute_job(
            &mut target,
            &origin,
            &[],
            &[],
            crate::archive::FaultMilestones::default(),
            &[action, action],
            searcher::search::rollout::RolloutStop::new(false),
        )
        .unwrap();
        assert_eq!(capture_hash_calls() - before, 3);
        assert_eq!(result.actions.len(), 2);
        for (index, hash) in [[17; 32], [23; 32]].into_iter().enumerate() {
            let snapshot = &result.actions[index].candidate.as_ref().unwrap().snapshot;
            assert_eq!(snapshot.history.actions().len(), index + 1);
            assert_eq!(
                snapshot.endpoint,
                Some(FaultEndpointCertificate::KvmWholeState(hash))
            );
        }
        LIVE.with(|slot| slot.borrow_mut().take());
    }

    #[test]
    fn action_capture_discards_evidence_if_hashing_abandons_the_session() {
        let mut target = unbooted_target();
        LIVE.with(|slot| *slot.borrow_mut() = Some(scripted_capture_live(vec![[17; 32]], true)));
        assert!(
            target
                .apply_and_snapshot(FaultOperation::Wait(std::num::NonZeroU16::MIN).into())
                .unwrap()
                .is_none()
        );
        assert!(target.failed());
        assert!(target.actions().is_empty());
        assert!(target.history.is_empty());
        LIVE.with(|slot| assert!(slot.borrow().is_none()));
        assert!(target.snapshot().unwrap().is_none());
        LIVE.with(|slot| assert!(slot.borrow().is_none()));
    }

    fn unbooted_uml_target() -> FaultTarget {
        let mut target = unbooted_target();
        Arc::get_mut(&mut target.config).unwrap().uml = Some(UmlLaunch {
            qualification: execution_contract::Requirement::Ordinary,
            profile: PathBuf::new(),
            expected_profile_identity_sha256: "fixture".into(),
            expected_host: uml::HostIdentity {
                architecture: "x86_64".into(),
                processor: 0,
                cpu_model: "fixture".into(),
                cpu_features: String::new(),
                boot_cpuinfo: String::new(),
                auxv: uml::HostAuxv::default(),
            },
            initramfs: PathBuf::new(),
            expected_initramfs_sha256: [0; 32],
            memory_mib: DEFAULT_RAM_MIB,
            kernel_arguments: Vec::new(),
            work_parent: PathBuf::new(),
            seed: SEED,
            setup_budget: SETUP_BUDGET,
            progress_limit: WALL_LIMIT,
        });
        target.history = FaultHistory::genesis(
            HistoryReceipt::genesis(
                [0; 32],
                FaultEndpointCertificate::UmlWholeStateUnavailable,
                crate::history::fixture_event_history(
                    FaultEndpointCertificate::UmlWholeStateUnavailable,
                ),
                &FaultObservations::default(),
            )
            .unwrap(),
        )
        .unwrap();
        target
    }

    #[test]
    fn uml_action_capture_keeps_the_unavailable_certificate_without_hashing() {
        let mut target = unbooted_uml_target();
        let mut live = scripted_capture_live(vec![[17; 32]], false);
        let receipt = HistoryReceipt::genesis(
            [0; 32],
            FaultEndpointCertificate::UmlWholeStateUnavailable,
            crate::history::fixture_event_history(
                FaultEndpointCertificate::UmlWholeStateUnavailable,
            ),
            &FaultObservations::default(),
        )
        .unwrap();
        target.history = FaultHistory::genesis(receipt.clone()).unwrap();
        live.chain = Chain::new(
            Point {
                snap: SnapId(1),
                moment: 0,
            },
            None,
        );
        live.chain.certify(0, receipt).unwrap();
        LIVE.with(|slot| *slot.borrow_mut() = Some(live));
        let before = capture_hash_calls();
        let snapshot = target
            .apply_and_snapshot(FaultOperation::Wait(std::num::NonZeroU16::MIN).into())
            .unwrap()
            .unwrap();
        assert_eq!(capture_hash_calls(), before);
        assert_eq!(
            snapshot.endpoint,
            Some(FaultEndpointCertificate::UmlWholeStateUnavailable)
        );
        assert_eq!(snapshot.history.certificates(), vec![None]);
        LIVE.with(|slot| slot.borrow_mut().take());
    }

    #[test]
    fn runnable_snapshot_identifies_the_current_kvm_endpoint() {
        let target = unbooted_target();
        LIVE.with(|slot| *slot.borrow_mut() = Some(hashed_live([17; 32])));
        let first = postcard::to_allocvec(&target.snapshot().unwrap().unwrap()).unwrap();
        LIVE.with(|slot| *slot.borrow_mut() = Some(hashed_live([23; 32])));
        assert!(target.snapshot().unwrap_err().contains("final prefix"));
        LIVE.with(|slot| slot.borrow_mut().take());
        assert!(!first.is_empty());
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
            event_history: None,
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
        let setup_observation = FaultObservations::new(
            10,
            &decode_sdk_events(std::slice::from_ref(&first)).unwrap(),
            FaultStop::Deadline,
        );
        let setup_receipt = HistoryReceipt::genesis(
            [0; 32],
            FaultEndpointCertificate::KvmWholeState([17; 32]),
            crate::history::fixture_event_history(FaultEndpointCertificate::KvmWholeState(
                [17; 32],
            )),
            &setup_observation,
        )
        .unwrap();
        let actual_receipt = setup_receipt
            .extend(
                action,
                FaultEndpointCertificate::KvmWholeState([17; 32]),
                crate::history::fixture_event_history(FaultEndpointCertificate::KvmWholeState(
                    [17; 32],
                )),
                &expected,
            )
            .unwrap();
        let snapshot = FaultSnapshot {
            history: FaultHistory::genesis(setup_receipt.clone())
                .unwrap()
                .push(actual_receipt.clone())
                .unwrap(),
            observation: expected,
            failed: false,
            endpoint: Some(FaultEndpointCertificate::KvmWholeState([17; 32])),
            event_history: Some((0, [0; 32])),
        };
        let mut target = unbooted_target();
        let mut live = hashed_live([17; 32]);
        live.chain = Chain::new(
            Point {
                snap: SnapId(1),
                moment: 0,
            },
            None,
        );
        live.chain.certify(0, setup_receipt).unwrap();
        live.chain
            .push_history(
                Point {
                    snap: SnapId(2),
                    moment: 1_000_000,
                },
                None,
                actual_receipt,
            )
            .unwrap();
        live.session = Box::new(TimedOutRestore {
            endpoint_state: consonance_client::endpoint::EndpointState::default(),
            endpoint_abandoned: false,
            histories: BTreeMap::new(),

            history_failure: None,

            sdk_failure: None,
            history: control_proto::EventHistory::default(),
            hash: Some([17; 32]),
            hash_calls: 0,
            replay_calls: 0,
            branch_calls: 0,
            sdk_offsets: Vec::new(),
            sdk_event_count: 0,
            run_events: None,
            abandon_on_hash: false,
            hash_failure: false,
            hash_error: None,
            events: BTreeMap::from([(1, vec![first]), (2, events)]),
            current: 1,
            run_hashes: None,
            run_hung: false,
            terminal_when_exhausted: false,
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
                endpoint_state: consonance_client::endpoint::EndpointState::default(),
                endpoint_abandoned: false,
                histories: BTreeMap::new(),

                history_failure: None,

                sdk_failure: None,
                history: control_proto::EventHistory::default(),
                hash: Some([17; 32]),
                hash_calls: 0,
                replay_calls: 0,
                branch_calls: 0,
                sdk_offsets: Vec::new(),
                sdk_event_count: 0,
                run_events: None,
                abandon_on_hash: false,
                hash_failure: false,
                hash_error: None,
                events: BTreeMap::new(),
                current: 1,
                run_hashes: Some(vec![first_hash, [17; 32]]),
                run_hung: false,
                terminal_when_exhausted: false,
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
                let actual = replay_endpoint_certificate(
                    target.state_hash_current()?,
                    target.event_history_current()?,
                    target.observation(),
                    target.failed(),
                )?;
                if actual != expected {
                    LIVE.with(|slot| slot.borrow_mut().take());
                    return Err(
                        "replayed KVM prefix differs from its archived endpoint certificate".into(),
                    );
                }
            }
            target.apply(second_action);
            assert!(!target.failed());
            let final_snapshot = target.snapshot().unwrap().unwrap();
            LIVE.with(|slot| slot.borrow_mut().take());
            Ok((replayed_parent, replayed_observation, final_snapshot))
        };
        let baseline = run([17; 32], false, None).unwrap();
        let certificate =
            replay_endpoint_certificate(baseline.0, (0, [0; 32]), &baseline.1, false).unwrap();
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
        assert_eq!(baseline.2.history.actions(), divergent.2.history.actions());
        assert_eq!(baseline.2.observation, divergent.2.observation);
        assert_eq!(baseline.2.endpoint, divergent.2.endpoint);
        assert_ne!(
            baseline.2.history.certificates(),
            divergent.2.history.certificates()
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
            endpoint_state: consonance_client::endpoint::EndpointState::default(),
            endpoint_abandoned: false,
            histories: BTreeMap::new(),

            history_failure: None,

            sdk_failure: None,
            history: control_proto::EventHistory::default(),
            hash: Some([17; 32]),
            hash_calls: 0,
            replay_calls: 0,
            branch_calls: 0,
            sdk_offsets: Vec::new(),
            sdk_event_count: 0,
            run_events: None,
            abandon_on_hash: false,
            hash_failure: false,
            hash_error: None,
            events: BTreeMap::new(),
            current: 1,
            run_hashes: Some(vec![[17; 32], [17; 32]]),
            run_hung: false,
            terminal_when_exhausted: false,
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
            baseline.event_history_current().unwrap(),
            baseline.observation(),
            baseline.failed(),
        )
        .unwrap();
        LIVE.with(|slot| slot.borrow_mut().take());

        let mut target = unbooted_target();
        let mut live = hashed_live([17; 32]);
        live.session = Box::new(TimedOutRestore {
            endpoint_state: consonance_client::endpoint::EndpointState::default(),
            endpoint_abandoned: false,
            histories: BTreeMap::new(),

            history_failure: None,

            sdk_failure: None,
            history: control_proto::EventHistory::default(),
            hash: Some([17; 32]),
            hash_calls: 0,
            replay_calls: 0,
            branch_calls: 0,
            sdk_offsets: Vec::new(),
            sdk_event_count: 0,
            run_events: None,
            abandon_on_hash: false,
            hash_failure: false,
            hash_error: None,
            events: BTreeMap::new(),
            current: 1,
            run_hashes: Some(vec![[23; 32], [17; 32]]),
            run_hung: false,
            terminal_when_exhausted: false,
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
            searcher::search::rollout::RolloutStop::new(false),
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
    fn campaign_job_reconstruction_cutoff_is_a_hung_job() {
        let mut target = unbooted_target();
        let snapshot = FaultSnapshot {
            history: crate::history::FaultHistory::fixture(
                &[],
                &[],
                Some(FaultEndpointCertificate::KvmWholeState([17; 32])),
                &(FaultObservations::default()),
            ),
            observation: FaultObservations::default(),
            failed: false,
            endpoint: Some(FaultEndpointCertificate::KvmWholeState([17; 32])),
            event_history: Some((0, [0; 32])),
        };
        LIVE.with(|slot| *slot.borrow_mut() = Some(timed_out_live()));
        let result = crate::campaign::execute_job(
            &mut target,
            &snapshot,
            &[FaultOperation::Wait(std::num::NonZeroU16::new(1).unwrap()).into()],
            &[Some([17; 32])],
            crate::archive::FaultMilestones::default(),
            &[],
            searcher::search::rollout::RolloutStop::new(false),
        )
        .unwrap();
        assert_eq!(result.hang, Some(Vec::new()));
        assert!(result.actions.is_empty());
        assert_eq!(target.watchdog_cutoffs(), 1);
        assert!(!target.found_bug());
        assert!(target.snapshot().unwrap().is_none());
        LIVE.with(|slot| slot.borrow_mut().take());
    }

    #[test]
    fn campaign_job_replayed_action_cutoff_is_a_hung_job() {
        let mut target = unbooted_target();
        let snapshot = FaultSnapshot {
            history: crate::history::FaultHistory::fixture(
                &[],
                &[],
                Some(FaultEndpointCertificate::KvmWholeState([17; 32])),
                &(FaultObservations::default()),
            ),
            observation: FaultObservations::default(),
            failed: false,
            endpoint: Some(FaultEndpointCertificate::KvmWholeState([17; 32])),
            event_history: Some((0, [0; 32])),
        };
        let mut live = hashed_live([17; 32]);
        live.session = Box::new(TimedOutRestore {
            endpoint_state: consonance_client::endpoint::EndpointState::default(),
            endpoint_abandoned: false,
            histories: BTreeMap::new(),

            history_failure: None,

            sdk_failure: None,
            history: control_proto::EventHistory::default(),
            hash: Some([17; 32]),
            hash_calls: 0,
            replay_calls: 0,
            branch_calls: 0,
            sdk_offsets: Vec::new(),
            sdk_event_count: 0,
            run_events: None,
            abandon_on_hash: false,
            hash_failure: false,
            hash_error: None,
            events: BTreeMap::new(),
            current: 1,
            run_hashes: None,
            run_hung: true,
            terminal_when_exhausted: false,
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
            searcher::search::rollout::RolloutStop::new(false),
        )
        .unwrap();
        assert_eq!(result.hang, Some(Vec::new()));
        assert!(result.actions.is_empty());
        assert_eq!(target.watchdog_cutoffs(), 1);
        assert!(!target.found_bug());
        assert!(target.snapshot().unwrap().is_none());
        LIVE.with(|slot| slot.borrow_mut().take());
    }

    #[test]
    fn campaign_job_replay_non_watchdog_failure_remains_fatal() {
        let mut target = unbooted_target();
        let snapshot = FaultSnapshot {
            history: crate::history::FaultHistory::fixture(
                &[],
                &[],
                Some(FaultEndpointCertificate::KvmWholeState([17; 32])),
                &(FaultObservations::default()),
            ),
            observation: FaultObservations::default(),
            failed: false,
            endpoint: Some(FaultEndpointCertificate::KvmWholeState([17; 32])),
            event_history: Some((0, [0; 32])),
        };
        let mut live = hashed_live([17; 32]);
        live.session = Box::new(TimedOutRestore {
            endpoint_state: consonance_client::endpoint::EndpointState::default(),
            endpoint_abandoned: false,
            histories: BTreeMap::new(),

            history_failure: None,

            sdk_failure: None,
            history: control_proto::EventHistory::default(),
            hash: Some([17; 32]),
            hash_calls: 0,
            replay_calls: 0,
            branch_calls: 0,
            sdk_offsets: Vec::new(),
            sdk_event_count: 0,
            run_events: None,
            abandon_on_hash: false,
            hash_failure: false,
            hash_error: None,
            events: BTreeMap::new(),
            current: 1,
            run_hashes: Some(Vec::new()),
            run_hung: false,
            terminal_when_exhausted: false,
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
            searcher::search::rollout::RolloutStop::new(false),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("replayed fault prefix stopped"), "{error}");
        LIVE.with(|slot| slot.borrow_mut().take());
    }

    #[test]
    fn campaign_suffix_cutoff_records_the_attempted_actions_as_a_hung_job() {
        let first = FaultOperation::Wait(std::num::NonZeroU16::new(1).unwrap()).into();
        let second = FaultOperation::Wait(std::num::NonZeroU16::new(2).unwrap()).into();
        let mut target = unbooted_target();
        let mut live = hashed_live([17; 32]);
        live.session = Box::new(TimedOutRestore {
            endpoint_state: consonance_client::endpoint::EndpointState::default(),
            endpoint_abandoned: false,
            histories: BTreeMap::new(),

            history_failure: None,

            sdk_failure: None,
            history: control_proto::EventHistory::default(),
            hash: Some([17; 32]),
            hash_calls: 0,
            replay_calls: 0,
            branch_calls: 0,
            sdk_offsets: Vec::new(),
            sdk_event_count: 0,
            run_events: None,
            abandon_on_hash: false,
            hash_failure: false,
            hash_error: None,
            events: BTreeMap::new(),
            current: 1,
            run_hashes: Some(vec![[17; 32]]),
            run_hung: true,
            terminal_when_exhausted: false,
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
            searcher::search::rollout::RolloutStop::new(false),
        );
        let result = result.expect("a suffix cutoff is a hung job");
        assert_eq!(result.hang, Some(vec![first, second]));
        assert!(result.actions.is_empty());
        assert!(result.preparation_failure.is_none());
        assert_eq!(target.actions(), &[first]);
        assert_eq!(target.watchdog_cutoffs(), 1);
        assert!(target.snapshot().unwrap().is_none());
        assert!(!target.found_bug());
        let game = crate::campaign::fixture_workload(
            b"kernel",
            b"initramfs",
            &FaultConfig {
                knobs: Vec::new(),
                ram_mib: DEFAULT_RAM_MIB,
                backend: GuestBackend::Vm,
                root: None,
                boot_cache: None,
            },
        );
        use searcher::search::campaign::Reporting;
        let digest = game
            .certified_result_sha256(&result, &target)
            .expect("a hung job result certifies against its failed target");
        let completed = searcher::search::campaign::CampaignJobResult {
            hang: None,
            ..result.clone()
        };
        assert!(
            game.certified_result_sha256(&completed, &target)
                .unwrap_err()
                .to_string()
                .contains("campaign incomplete")
        );
        assert_eq!(
            digest,
            game.certified_result_sha256(&result, &target).unwrap()
        );
        LIVE.with(|slot| slot.borrow_mut().take());
    }

    #[test]
    fn a_hung_job_result_cannot_name_a_target_that_did_not_hang() {
        use searcher::search::campaign::{CampaignJobResult, Reporting};
        let game = crate::campaign::fixture_workload(
            b"kernel",
            b"initramfs",
            &FaultConfig {
                knobs: Vec::new(),
                ram_mib: DEFAULT_RAM_MIB,
                backend: GuestBackend::Vm,
                root: None,
                boot_cache: None,
            },
        );
        let target = unbooted_target();
        let result = CampaignJobResult {
            preparation_failure: None,
            hang: Some(Vec::new()),
            actions: Vec::new(),
        };
        assert!(
            game.certified_result_sha256(&result, &target)
                .unwrap_err()
                .to_string()
                .contains("did not hang")
        );
    }

    #[test]
    fn imported_unavailable_machine_history_rejects_observation_divergence() {
        let target = unbooted_uml_target();
        let root = target.history.clone();
        let endpoint = FaultEndpointCertificate::UmlWholeStateUnavailable;
        let action = FaultOperation::Wait(std::num::NonZeroU16::MIN).into();
        let observation = FaultObservations::default();
        let first = root
            .receipt()
            .extend(
                action,
                endpoint,
                crate::history::fixture_event_history(endpoint),
                &observation,
            )
            .unwrap();
        let expected = root.push(first).unwrap();
        let expected = expected
            .push(
                expected
                    .receipt()
                    .extend(
                        action,
                        endpoint,
                        crate::history::fixture_event_history(endpoint),
                        &observation,
                    )
                    .unwrap(),
            )
            .unwrap();
        let mut divergent_observation = observation.clone();
        divergent_observation.alive = 7;
        let actual_first = root
            .receipt()
            .extend(
                action,
                endpoint,
                crate::history::fixture_event_history(endpoint),
                &divergent_observation,
            )
            .unwrap();
        let actual_final = actual_first
            .extend(
                action,
                endpoint,
                crate::history::fixture_event_history(endpoint),
                &observation,
            )
            .unwrap();
        assert_ne!(expected.receipt().root(), actual_final.root());
        assert_eq!(expected.receipt().moment(), actual_final.moment());
        let index = Arc::new(LocalIndex::new(1 << 20));
        let namespace = Namespace::exact(&[b"uml-observation-history"]);
        let mut publisher = publishing_live(&index, namespace, SnapId(3), [17; 32]);
        publisher.chain = Chain::new(
            Point {
                snap: SnapId(1),
                moment: 0,
            },
            Some(Shared {
                index: index.clone(),
                namespace,
            }),
        );
        publisher.chain.certify(0, root.receipt().clone()).unwrap();
        publisher.observe(FaultStop::Deadline).unwrap();
        publisher.cursor.begin_action(0);
        publisher
            .seal_captured_link(
                Point {
                    snap: SnapId(1),
                    moment: 0,
                },
                SnapId(3),
                0,
                actual_final,
            )
            .unwrap();
        let snapshot = FaultSnapshot {
            event_history: crate::history::fixture_event_history(endpoint),

            history: expected,
            observation,
            failed: false,
            endpoint: Some(endpoint),
        };
        let mut importer = scripted_capture_live(Vec::new(), false);
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
        importer.chain.certify(0, root.receipt().clone()).unwrap();
        LIVE.with(|slot| *slot.borrow_mut() = Some(importer));
        let mut target = target;
        assert!(
            target
                .restore(&snapshot)
                .unwrap_err()
                .to_string()
                .contains("prefix")
        );
        assert_eq!(capture_hash_calls(), 0);
        assert!(index.verify_integrity().is_ok());
        LIVE.with(|slot| slot.borrow_mut().take());
    }

    #[test]
    fn restored_sibling_extension_publishes_against_its_selected_parent() {
        let mut target = unbooted_target();
        let index = Arc::new(LocalIndex::new(64 << 20));
        let namespace = Namespace::exact(&[b"selected-parent-publication"]);
        let mut live = scripted_capture_live(vec![[17; 32]; 3], false);
        live.chain = Chain::new(
            Point {
                snap: SnapId(1),
                moment: 0,
            },
            Some(Shared {
                index: index.clone(),
                namespace,
            }),
        );
        live.chain.certify(0, fixture_genesis()).unwrap();
        LIVE.with(|slot| *slot.borrow_mut() = Some(live));
        target.apply(FaultOperation::Wait(std::num::NonZeroU16::new(1).unwrap()).into());
        let parent = target.snapshot().unwrap().unwrap();
        let parent_lease = index
            .lookup(namespace, &parent.history.receipt().path())
            .unwrap();
        target.reset();
        target.apply(FaultOperation::Wait(std::num::NonZeroU16::new(2).unwrap()).into());
        target.restore(&parent).unwrap();
        target.apply(FaultOperation::Wait(std::num::NonZeroU16::new(3).unwrap()).into());
        let child = target.snapshot().unwrap().unwrap();
        let child_lease = index
            .lookup(namespace, &child.history.receipt().path())
            .unwrap();
        let parent_extents = index.chain(&parent_lease).unwrap();
        let child_extents = index.chain(&child_lease).unwrap();
        assert_eq!(child_extents.len(), parent_extents.len() + 1);
        assert_eq!(child_extents[0].bytes(), parent_extents[0].bytes());
        index.release(parent_lease);
        index.release(child_lease);
        LIVE.with(|slot| slot.borrow_mut().take());
    }

    #[test]
    fn terminal_and_rebuild_stop_observations_exclude_previous_park_evidence() {
        let action = FaultOperation::Wait(std::num::NonZeroU16::MIN).into();
        let events = [
            (5, 0, br#"{"harmony_park":{"site":1,"edges":2}}"#.to_vec()),
            (
                5,
                0,
                br#"{"harmony_park_read":{"site":1,"edges":2}}"#.to_vec(),
            ),
            (20, 0, br#"{"harmony_park":{"site":3,"edges":4}}"#.to_vec()),
            (
                20,
                0,
                br#"{"harmony_park_read":{"site":3,"edges":4}}"#.to_vec(),
            ),
        ];
        for rebuilding in [false, true] {
            let mut target = unbooted_target();
            let mut prefix = events[..2].to_vec();
            prefix.push((10, u32::MAX, Vec::new()));
            let mut decoder = SdkDecoder::default();
            decoder.append(&prefix).unwrap();
            let observation = FaultObservations::new(10, decoder.capture(), FaultStop::Deadline);
            let genesis = HistoryReceipt::genesis(
                [0; 32],
                FaultEndpointCertificate::KvmWholeState([17; 32]),
                Some((0, [0; 32])),
                &observation,
            )
            .unwrap();
            target.history = FaultHistory::genesis(genesis.clone()).unwrap();
            let mut live = hashed_live([17; 32]);
            live.chain = Chain::new(
                Point {
                    snap: SnapId(1),
                    moment: 10,
                },
                None,
            );
            live.chain.certify(0, genesis).unwrap();
            if rebuilding {
                let receipt = target
                    .history
                    .receipt()
                    .extend(
                        action,
                        FaultEndpointCertificate::KvmWholeState([17; 32]),
                        Some((0, [0; 32])),
                        &observation,
                    )
                    .unwrap();
                target.history = target.history.push(receipt).unwrap();
            }
            live.session = Box::new(TimedOutRestore {
                endpoint_state: consonance_client::endpoint::EndpointState::default(),
                endpoint_abandoned: false,
                events: BTreeMap::from([(1, prefix.clone())]),
                run_events: Some(
                    prefix
                        .into_iter()
                        .chain(events[2..].iter().cloned())
                        .collect(),
                ),
                ..fixture_session()
            });
            LIVE.with(|slot| *slot.borrow_mut() = Some(live));
            target.apply(action);
            assert!(!target.failed());
            assert_eq!(
                target
                    .observation
                    .parks
                    .iter()
                    .map(|park| park.moment)
                    .collect::<Vec<_>>(),
                vec![20]
            );
            assert_eq!(
                target
                    .observation
                    .park_reads
                    .iter()
                    .map(|read| read.moment)
                    .collect::<Vec<_>>(),
                vec![20]
            );
            if rebuilding {
                assert_eq!(target.observation.stop, FaultStop::Unexpected);
            }
            LIVE.with(|slot| slot.borrow_mut().take());
        }
    }

    #[test]
    fn reset_reports_missing_actual_genesis_as_failure() {
        let mut target = unbooted_target();
        let mut live = hashed_live([17; 32]);
        live.chain = Chain::new(
            Point {
                snap: SnapId(1),
                moment: 0,
            },
            None,
        );
        LIVE.with(|slot| *slot.borrow_mut() = Some(live));
        target.reset();
        assert!(target.failed());
        LIVE.with(|slot| slot.borrow_mut().take());
    }

    #[test]
    fn valid_terminal_endpoint_does_not_publish_a_runnable_history_link() {
        let mut target = unbooted_target();
        let index = Arc::new(LocalIndex::new(1 << 20));
        let mut live = hashed_live([17; 32]);
        live.chain = Chain::new(
            Point {
                snap: SnapId(1),
                moment: 0,
            },
            Some(Shared {
                index: index.clone(),
                namespace: Namespace::exact(&[b"terminal-endpoint"]),
            }),
        );
        live.chain.certify(0, fixture_genesis()).unwrap();
        LIVE.with(|slot| *slot.borrow_mut() = Some(live));
        assert!(
            target
                .apply_and_snapshot(FaultOperation::Wait(std::num::NonZeroU16::MIN).into())
                .unwrap()
                .is_none()
        );
        assert!(!target.failed());
        assert_eq!(target.watchdog_cutoffs(), 0);
        assert_eq!(target.actions().len(), 1);
        assert!(!target.observation().stop.is_continuable());
        assert_eq!(index.stats().entries, 0);
        LIVE.with(|slot| {
            assert_eq!(slot.borrow().as_ref().unwrap().chain.links(), 1);
            slot.borrow_mut().take();
        });
    }

    #[test]
    fn ordinary_hash_failure_abandons_all_owned_snapshots_before_publication() {
        let index = Arc::new(LocalIndex::new(1 << 20));
        let mut live = scripted_capture_live(vec![[17; 32]], false);
        live.session = Box::new(TimedOutRestore {
            endpoint_state: consonance_client::endpoint::EndpointState::default(),
            endpoint_abandoned: false,
            history: control_proto::EventHistory::default(),
            history_failure: None,
            sdk_failure: None,
            histories: BTreeMap::new(),

            hash: Some([17; 32]),
            hash_calls: 0,
            replay_calls: 0,
            branch_calls: 0,
            sdk_offsets: Vec::new(),
            sdk_event_count: 0,
            run_events: None,
            abandon_on_hash: false,
            hash_failure: true,
            hash_error: None,
            events: BTreeMap::new(),
            current: 1,
            run_hashes: Some(vec![[17; 32]]),
            run_hung: false,
            terminal_when_exhausted: false,
            snapshots: BTreeMap::from([(1, [17; 32])]),
            moment: 0,
            next_snap: 1,
        });
        live.chain = Chain::new(
            Point {
                snap: SnapId(1),
                moment: 0,
            },
            Some(Shared {
                index: index.clone(),
                namespace: Namespace::exact(&[b"ordinary-hash-failure"]),
            }),
        );
        live.chain.certify(0, fixture_genesis()).unwrap();
        let history = FaultHistory::genesis(fixture_genesis()).unwrap();
        let error = live
            .advance(
                &history,
                FaultOperation::Wait(std::num::NonZeroU16::MIN).into(),
                RunKind::New,
            )
            .unwrap_err();
        assert!(error.contains("ordinary hash failure"), "{error}");
        assert!(live.abandoned);
        assert!(live.session.abandoned());
        assert_eq!(live.chain.links(), 1);
        assert!(live.cursor.decoder.is_none());
        assert_eq!(
            live.session
                .telemetry_counters()
                .into_iter()
                .collect::<BTreeMap<_, _>>()["test.snapshots"],
            0
        );
        assert_eq!(index.stats().entries, 0);
    }

    #[test]
    fn roots_preserve_the_pinned_object_commitment_and_reject_tampering() {
        let mut source = scripted_capture_live(vec![[17; 32]], false);
        let action = FaultOperation::Wait(std::num::NonZeroU16::MIN).into();
        let history = FaultHistory::genesis(fixture_genesis()).unwrap();
        source.advance(&history, action, RunKind::New).unwrap();
        let snap = source.chain.point(source.chain.links() - 1).snap;
        let bytes = export_root(source.session.as_mut(), snap, [17; 32], &[]).unwrap();
        assert_eq!(&bytes[..8], b"HMROOT03");
        let mut destination = scripted_capture_live(Vec::new(), false);
        let (imported, _, _) = import_root(destination.session.as_mut(), &bytes).unwrap();
        destination.replay(imported).unwrap();
        assert_eq!(destination.state_hash_current().unwrap(), [17; 32]);
        let mut changed = bytes.clone();
        changed[40] ^= 1;
        assert!(
            import_root(destination.session.as_mut(), &changed)
                .unwrap_err()
                .to_string()
                .contains("commitment")
        );
        let mut old = bytes;
        old[..8].copy_from_slice(b"HMROOT02");
        assert!(import_root(destination.session.as_mut(), &old).is_err());
    }

    #[test]
    fn forged_expected_history_cannot_replace_captured_history() {
        for imported in [false, true] {
            let mut target = unbooted_target();
            let index = Arc::new(LocalIndex::new(1 << 20));
            let namespace = Namespace::exact(&[b"forged-expected-history"]);
            let mut live = scripted_capture_live(vec![[17; 32], [17; 32]], false);
            if imported {
                live.chain = Chain::new(
                    Point {
                        snap: SnapId(1),
                        moment: 0,
                    },
                    Some(Shared {
                        index: index.clone(),
                        namespace,
                    }),
                );
                live.chain.certify(0, fixture_genesis()).unwrap();
            }
            LIVE.with(|slot| *slot.borrow_mut() = Some(live));
            let action = FaultOperation::Wait(std::num::NonZeroU16::MIN).into();
            target.apply(action);
            target.apply(action);
            let snapshot = target.snapshot().unwrap().unwrap();
            let mut forged = snapshot.clone();
            let mut certificates = snapshot.history.certificates();
            certificates[0] = Some([23; 32]);
            forged.history = FaultHistory::fixture(
                &snapshot.history.actions(),
                &certificates,
                snapshot.endpoint,
                &snapshot.observation,
            );
            if imported {
                let mut live = scripted_capture_live(Vec::new(), false);
                live.chain = Chain::new(
                    Point {
                        snap: SnapId(1),
                        moment: 0,
                    },
                    Some(Shared {
                        index: index.clone(),
                        namespace,
                    }),
                );
                live.chain.certify(0, fixture_genesis()).unwrap();
                LIVE.with(|slot| *slot.borrow_mut() = Some(live));
            }
            let before = thread_work_telemetry();
            assert!(
                target
                    .restore(&forged)
                    .unwrap_err()
                    .to_string()
                    .contains("prefix")
            );
            let after = thread_work_telemetry();
            assert_eq!(
                after["session.restore.count"],
                before["session.restore.count"]
            );
            assert_eq!(after["run.rebuild.count"], before["run.rebuild.count"]);
            assert_eq!(after["vmm.test.hash_calls"], before["vmm.test.hash_calls"]);
            assert!(index.verify_integrity().is_ok());
            assert!(target.restore(&snapshot).is_ok());
            LIVE.with(|slot| slot.borrow_mut().take());
        }
    }

    #[test]
    fn converged_machine_endpoints_with_different_captured_history_quarantine() {
        let index = Arc::new(LocalIndex::new(1 << 20));
        let namespace = Namespace::exact(&[b"converged-history"]);
        let action = FaultOperation::Wait(std::num::NonZeroU16::MIN).into();
        for (first_hash, expected_error) in [([17; 32], false), ([23; 32], true)] {
            let mut live = scripted_capture_live(vec![first_hash, [17; 32]], false);
            let history = FaultHistory::genesis(fixture_genesis()).unwrap();
            let (_, first) = live.advance(&history, action, RunKind::New).unwrap();
            let history = history.push(first.unwrap()).unwrap();
            let (_, second) = live.advance(&history, action, RunKind::New).unwrap();
            let receipt = second.unwrap();
            let point = live.chain.point(live.chain.links() - 1);
            live.chain = Chain::new(
                Point {
                    snap: SnapId(1),
                    moment: 0,
                },
                Some(Shared {
                    index: index.clone(),
                    namespace,
                }),
            );
            live.chain.certify(0, fixture_genesis()).unwrap();
            let result = live.seal_captured_link(
                Point {
                    snap: SnapId(1),
                    moment: 0,
                },
                point.snap,
                point.moment,
                receipt,
            );
            assert_eq!(result.is_err(), expected_error);
        }
        assert!(matches!(
            index.verify_integrity(),
            Err(CacheError::Quarantined { .. })
        ));
        let evidence = index.divergences();
        assert_eq!(evidence.len(), 1);
        assert_ne!(
            evidence[0].existing.target_commitment,
            evidence[0].published.target_commitment
        );
    }

    #[test]
    fn malformed_observation_discards_working_cursor_without_mutating_retained_checkpoint() {
        let mut live = hashed_live([17; 32]);
        live.observe(FaultStop::Deadline).unwrap();
        let checkpoint = live
            .capture_observation_checkpoint(&fixture_genesis())
            .unwrap();
        live.chain
            .set_observation_checkpoint(0, checkpoint.clone())
            .unwrap();
        let mut valid = vec![0];
        valid.extend_from_slice(&77_u64.to_le_bytes());
        live.session = Box::new(TimedOutRestore {
            endpoint_state: consonance_client::endpoint::EndpointState::default(),
            endpoint_abandoned: false,
            events: BTreeMap::from([(
                1,
                vec![(1, (2 << 24) | 1, valid), (2, (1 << 24) | 1, vec![9, 0, 0])],
            )]),
            ..fixture_session()
        });
        assert!(
            live.observe(FaultStop::Deadline)
                .unwrap_err()
                .contains("unknown disposition")
        );
        assert!(live.cursor.decoder.is_none());
        assert_eq!(live.chain.observation_checkpoint(0), Some(&checkpoint));
    }

    #[test]
    fn branch_restores_actual_checkpoint_even_for_equal_length_divergent_siblings() {
        let prefix = vec![(9, u32::MAX, vec![1]), (7, u32::MAX, vec![2])];
        let sibling = vec![(99, u32::MAX, vec![3]), (77, u32::MAX, vec![4])];
        let suffix = (8, 0, br#"{"harmony_park":{"site":3,"edges":4}}"#.to_vec());
        let mut live = hashed_live([17; 32]);
        live.session = Box::new(TimedOutRestore {
            endpoint_state: consonance_client::endpoint::EndpointState::default(),
            endpoint_abandoned: false,
            events: BTreeMap::from([(1, prefix.clone()), (2, sibling)]),
            run_events: Some(prefix.iter().cloned().chain([suffix]).collect()),
            ..fixture_session()
        });
        let actual = live.observe(FaultStop::Deadline).unwrap();
        assert_eq!(actual.moment, 7);
        let genesis = HistoryReceipt::genesis(
            [0; 32],
            FaultEndpointCertificate::KvmWholeState([17; 32]),
            Some((0, [0; 32])),
            &actual,
        )
        .unwrap();
        live.chain = Chain::new(
            Point {
                snap: SnapId(1),
                moment: 7,
            },
            None,
        );
        live.chain.certify(0, genesis.clone()).unwrap();
        let checkpoint = live.capture_observation_checkpoint(&genesis).unwrap();
        live.chain
            .set_observation_checkpoint(0, checkpoint)
            .unwrap();
        live.replay(SnapId(2)).unwrap();
        assert_eq!(live.observe(FaultStop::Deadline).unwrap().moment, 77);
        let before = live
            .session
            .telemetry_counters()
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        let action = FaultOperation::Wait(std::num::NonZeroU16::MIN).into();
        live.branch(
            Point {
                snap: SnapId(1),
                moment: 7,
            },
            &ActionPlan::compile(live.windows, &live.inherited, &[action]).unwrap(),
        )
        .unwrap();
        assert_eq!(live.cursor.decoder.as_ref().unwrap().offset(), 2);
        assert_eq!(live.cursor.decoder.as_ref().unwrap().moment(), 7);
        live.session.run_until(10).unwrap();
        let actual = live.observe(FaultStop::Deadline).unwrap();
        assert_eq!(actual.moment, 8);
        assert_eq!(actual.parks.len(), 1);
        assert_eq!(actual.parks[0].moment, 8);
        assert_eq!(actual, live.observe(FaultStop::Deadline).unwrap());
        let after = live
            .session
            .telemetry_counters()
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        assert_eq!(after["test.replay_calls"], before["test.replay_calls"]);
        assert_eq!(after["test.branch_calls"] - before["test.branch_calls"], 1);
        assert_eq!(after["test.sdk_events"] - before["test.sdk_events"], 1);
        assert_eq!(after["test.sdk_last_offset"], 3);
    }

    #[test]
    fn pending_import_is_verified_by_branch_without_an_extra_replay() {
        let action = FaultOperation::Wait(std::num::NonZeroU16::MIN).into();
        let receipt = fixture_genesis()
            .extend(
                action,
                FaultEndpointCertificate::KvmWholeState([17; 32]),
                Some((0, [0; 32])),
                &FaultObservations::default(),
            )
            .unwrap();
        let mut decoder = SdkDecoder::default();
        decoder.begin_action(0);
        decoder.append(&[(0, u32::MAX, Vec::new())]).unwrap();
        let checkpoint = ObservationCheckpoint::capture(&receipt, decoder).unwrap();
        let index = Arc::new(LocalIndex::new(1 << 20));
        let namespace = Namespace::exact(&[b"pending-branch-decoder"]);
        let mut publisher = publishing_live(&index, namespace, SnapId(3), [17; 32]);
        publisher.session = Box::new(TimedOutRestore {
            endpoint_state: consonance_client::endpoint::EndpointState::default(),
            endpoint_abandoned: false,
            current: 3,
            events: BTreeMap::from([(3, vec![(0, u32::MAX, Vec::new())])]),
            snapshots: BTreeMap::from([(3, [17; 32])]),
            ..fixture_session()
        });
        let lease = publisher
            .publish(
                Point {
                    snap: SnapId(1),
                    moment: 0,
                },
                SnapId(3),
                0,
                &receipt,
                &checkpoint,
            )
            .unwrap()
            .unwrap();
        index.release(lease);
        let mut importer = publishing_live(&index, namespace, SnapId(1), [17; 32]);
        let at = importer.find_path(receipt.path(), 1).unwrap().unwrap();
        let parent = importer.chain.point(at);
        let plan =
            ActionPlan::compile(importer.windows, &importer.inherited, &[action, action]).unwrap();
        importer.branch(parent, &plan).unwrap();
        assert!(importer.chain.pending_observation_checkpoint(at).is_none());
        assert!(importer.chain.observation_checkpoint(at).is_some());
        assert_eq!(importer.cursor.decoder.as_ref().unwrap().offset(), 1);
        assert_eq!(
            importer.observe(FaultStop::Deadline).unwrap(),
            FaultObservations::default()
        );
        let raw = importer
            .session
            .telemetry_counters()
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        assert_eq!(raw["test.replay_calls"], 0);
        assert_eq!(raw["test.branch_calls"], 1);
        assert_eq!(raw["test.sdk_events"], 1);
        assert_eq!(raw["test.sdk_last_offset"], 1);
    }

    #[test]
    fn imported_hidden_decoder_state_requires_one_actual_restore_before_trust() {
        let action = FaultOperation::Wait(std::num::NonZeroU16::MIN).into();
        let receipt = fixture_genesis()
            .extend(
                action,
                FaultEndpointCertificate::KvmWholeState([17; 32]),
                Some((0, [0; 32])),
                &FaultObservations::default(),
            )
            .unwrap();
        let mut decoder = SdkDecoder::default();
        decoder.begin_action(0);
        let mut serialized = serde_json::to_value(&decoder).unwrap();
        serialized["check_passes"] = serde_json::json!({"40": ["forged-hidden-pass"]});
        let forged: SdkDecoder = serde_json::from_value(serialized).unwrap();
        assert_ne!(decoder, forged);
        assert_eq!(decoder.capture(), forged.capture());
        let checkpoint = ObservationCheckpoint::capture(&receipt, forged).unwrap();
        let index = Arc::new(LocalIndex::new(1 << 20));
        let namespace = Namespace::exact(&[b"untrusted-hidden-decoder"]);
        let mut publisher = publishing_live(&index, namespace, SnapId(3), [17; 32]);
        let lease = publisher
            .publish(
                Point {
                    snap: SnapId(1),
                    moment: 0,
                },
                SnapId(3),
                0,
                &receipt,
                &checkpoint,
            )
            .unwrap()
            .unwrap();
        index.release(lease);
        let mut importer = publishing_live(&index, namespace, SnapId(1), [17; 32]);
        let at = importer.find_path(receipt.path(), 1).unwrap().unwrap();
        let snapshot = importer.chain.point(at).snap;
        assert!(importer.chain.observation_checkpoint(at).is_none());
        assert!(importer.chain.pending_observation_checkpoint(at).is_some());
        assert!(importer.cursor.decoder.is_none());
        assert_eq!(
            importer
                .session
                .telemetry_counters()
                .into_iter()
                .collect::<BTreeMap<_, _>>()["test.replay_calls"],
            0
        );
        let error = importer.replay(snapshot).unwrap_err();
        assert!(error.contains("differs from actual SDK history"), "{error}");
        assert_eq!(
            importer
                .session
                .telemetry_counters()
                .into_iter()
                .collect::<BTreeMap<_, _>>()["test.replay_calls"],
            1
        );
        assert!(importer.cursor.decoder.is_none());
        assert_eq!(importer.chain.links(), 1);
    }

    #[test]
    fn reconstructed_checkpoint_pool_is_bounded_without_shifting_selected_link() {
        let index = Arc::new(LocalIndex::new(1));
        let namespace = Namespace::exact(&[b"optional-decoder-pool"]);
        let mut live = publishing_live(&index, namespace, SnapId(1), [17; 32]);
        let action = FaultOperation::Wait(std::num::NonZeroU16::MIN).into();
        let mut receipt = fixture_genesis();
        for snap in [2, 3] {
            receipt = receipt
                .extend(
                    action,
                    receipt.endpoint(),
                    Some((0, [0; 32])),
                    &FaultObservations::default(),
                )
                .unwrap();
            live.chain
                .push_history(
                    Point {
                        snap: SnapId(snap),
                        moment: 0,
                    },
                    None,
                    receipt.clone(),
                )
                .unwrap();
        }
        let selected = SnapId(2);
        let at = live.chain.at_snapshot(selected).unwrap();
        assert_eq!(at, 1);
        let floor = live.chain.observation_checkpoint_bytes();
        for _ in 0..3 {
            live.cursor.invalidate();
            live.session.replay_snapshot(selected).unwrap();
            live.restore_observation_cursor_with_limit(selected, 0)
                .unwrap();
            assert_eq!(live.chain.at_snapshot(selected), Some(at));
            assert_eq!(live.chain.links(), 3);
            assert_eq!(live.chain.observation_checkpoint_bytes(), floor);
            assert_eq!(index.stats().stored, floor);
            assert!(live.chain.observation_checkpoint(at).is_none());
            assert_eq!(
                live.observe(FaultStop::Deadline).unwrap(),
                FaultObservations::default()
            );
        }
        assert_eq!(live.telemetry.observation_checkpoint_rebuilds.count, 3);
        assert_eq!(live.telemetry.observation_checkpoint_evictions, 3);
        assert!(index.stats().shrinks >= 3);
    }

    #[test]
    fn locality_drain_observes_session_retirement_outside_with_live_without_rpc() {
        for history in [false, true] {
            let target = unbooted_target();
            let mut live = hashed_live([17; 32]);
            let mut session = fixture_session();
            session.abandon_on_hash = !history;
            session.history_failure = history.then_some(|| SessionError::Hung(WALL_LIMIT));
            live.session = Box::new(session);
            LIVE.with(|slot| *slot.borrow_mut() = Some(live));
            crate::job_observation::new_generation();
            crate::job_observation::begin();
            crate::job_observation::origin_start(0);
            crate::job_observation::selected(false, 0);
            let generation = crate::job_observation::generation().unwrap();
            if history {
                let _ = target.event_history_current();
            } else {
                let _ = target.state_hash_current();
            }
            LIVE.with(|slot| assert!(slot.borrow().is_none()));
            let row = take_job_observation().unwrap();
            assert_eq!(row.origin.generation, Some(generation));
            assert_eq!(row.end_generation, None);
            assert!(take_job_observation().is_none());
        }
    }

    #[test]
    fn worker_locality_uses_only_matching_live_receipts_and_verified_decoder_hints() {
        use searcher::search::locality::{LocalityKey, WorkerLocality};

        LIVE.with(|slot| *slot.borrow_mut() = None);
        let target = unbooted_target();
        assert_eq!(target.worker_locality(), WorkerLocality::default());
        LIVE.with(|slot| assert!(slot.borrow().is_none()));
        let mut live = timed_out_live();
        let receipt = live.chain.provenance(0).unwrap().clone();
        let key = LocalityKey(receipt.root());
        let checkpoint = ObservationCheckpoint::capture(&receipt, SdkDecoder::default()).unwrap();
        live.chain
            .set_pending_observation_checkpoint(0, checkpoint.clone())
            .unwrap();
        let before = live.session.telemetry_counters();
        LIVE.with(|slot| *slot.borrow_mut() = Some(live));
        assert_eq!(target.worker_locality(), WorkerLocality::new([(key, 0)]));
        LIVE.with(|slot| {
            let mut slot = slot.borrow_mut();
            let live = slot.as_mut().unwrap();
            live.chain
                .set_observation_checkpoint(0, checkpoint)
                .unwrap();
        });
        assert_eq!(target.worker_locality(), WorkerLocality::new([(key, 1)]));
        LIVE.with(|slot| {
            let mut slot = slot.borrow_mut();
            let live = slot.as_mut().unwrap();
            assert_eq!(live.session.telemetry_counters(), before);
            live.key = [9; 32];
        });
        assert_eq!(target.worker_locality(), WorkerLocality::default());
        LIVE.with(|slot| {
            let mut slot = slot.borrow_mut();
            let live = slot.as_mut().unwrap();
            live.key = target.config.key;
            live.abandoned = true;
        });
        assert_eq!(target.worker_locality(), WorkerLocality::default());
        LIVE.with(|slot| *slot.borrow_mut() = None);
        assert_eq!(target.worker_locality(), WorkerLocality::default());
    }

    #[test]
    fn locality_parent_and_cached_child_sources_are_separate_from_origin() {
        let action = FaultOperation::Wait(std::num::NonZeroU16::MIN).into();
        for imported in [false, true] {
            let mut target = unbooted_target();
            let index = Arc::new(LocalIndex::new(64 << 20));
            let namespace = Namespace::exact(&[b"locality-child"]);
            let mut live = scripted_capture_live(vec![[17; 32]; 3], false);
            if imported {
                live.chain = Chain::new(
                    Point {
                        snap: SnapId(1),
                        moment: 0,
                    },
                    Some(Shared {
                        index: index.clone(),
                        namespace,
                    }),
                );
                live.chain.certify(0, fixture_genesis()).unwrap();
            }
            LIVE.with(|slot| *slot.borrow_mut() = Some(live));
            target.apply(action);
            let parent = target.snapshot().unwrap().unwrap();
            target.apply(action);
            if imported {
                let mut live = scripted_capture_live(vec![[17; 32]], false);
                live.chain = Chain::new(
                    Point {
                        snap: SnapId(1),
                        moment: 0,
                    },
                    Some(Shared {
                        index: index.clone(),
                        namespace,
                    }),
                );
                live.chain.certify(0, fixture_genesis()).unwrap();
                LIVE.with(|slot| *slot.borrow_mut() = Some(live));
            }
            target.restore(&parent).unwrap();
            crate::job_observation::begin();
            target.apply(action);
            assert!(!target.failed());
            let child = take_job_observation().unwrap();
            assert_eq!(
                child.origin.outcome,
                crate::job_observation::RestoreOutcome::NotAttempted
            );
            assert_eq!(child.child_sources.shared_exact, u64::from(imported));
            assert_eq!(child.child_sources.local_exact, u64::from(!imported));
            assert_eq!(child.parent_sources.local_exact, 0);
            crate::job_observation::begin();
            target.apply(action);
            assert!(!target.failed());
            let parent = take_job_observation().unwrap();
            assert_eq!(parent.parent_sources.local_exact, 1);
            assert_eq!(parent.child_sources.local_exact, 0);
            assert_eq!(parent.child_sources.shared_exact, 0);
            LIVE.with(|slot| slot.borrow_mut().take());
        }
    }

    #[test]
    fn locality_observation_distinguishes_ancestors_reconstruction_and_final_failure() {
        use crate::job_observation::{RestoreOutcome, RestoreSource};
        let action = FaultOperation::Wait(std::num::NonZeroU16::MIN).into();
        let mut target = unbooted_target();
        LIVE.with(|slot| {
            *slot.borrow_mut() = Some(scripted_capture_live(vec![[17; 32]; 2], false))
        });
        target.apply(action);
        target.apply(action);
        let snapshot = target.snapshot().unwrap().unwrap();
        LIVE.with(|slot| slot.borrow_mut().take());
        for (shared, selected_depth) in [(false, 0), (false, 1), (true, 1)] {
            let index = Arc::new(LocalIndex::new(64 << 20));
            let namespace = Namespace::exact(&[b"locality-ancestor"]);
            let mut live = scripted_capture_live(vec![[17; 32]; 2], false);
            if shared {
                live.chain = Chain::new(
                    Point {
                        snap: SnapId(1),
                        moment: 0,
                    },
                    Some(Shared {
                        index: index.clone(),
                        namespace,
                    }),
                );
                live.chain.certify(0, fixture_genesis()).unwrap();
            }
            LIVE.with(|slot| *slot.borrow_mut() = Some(live));
            let mut seed = unbooted_target();
            if selected_depth > 0 {
                seed.apply(action);
            }
            if shared {
                let mut importer = scripted_capture_live(vec![[17; 32]], false);
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
                importer.chain.certify(0, fixture_genesis()).unwrap();
                LIVE.with(|slot| *slot.borrow_mut() = Some(importer));
            }
            crate::job_observation::begin();
            target.restore(&snapshot).unwrap();
            let row = take_job_observation().unwrap();
            assert_eq!(
                row.origin.source,
                if shared {
                    RestoreSource::SharedImportAncestor
                } else {
                    RestoreSource::LocalAncestor
                }
            );
            assert_eq!(row.origin.outcome, RestoreOutcome::Succeeded);
            assert_eq!(row.origin.selected_depth, selected_depth);
            assert_eq!(row.origin.rebuild_attempted, 2 - selected_depth);
            assert_eq!(row.origin.rebuild_completed, 2 - selected_depth);
            assert_eq!(row.origin_lookups.import_completed, u64::from(shared));
            let mut wrong = snapshot.clone();
            wrong.endpoint = Some(FaultEndpointCertificate::KvmWholeState([23; 32]));
            crate::job_observation::begin();
            assert!(target.restore(&wrong).is_err());
            let refused = take_job_observation().unwrap();
            assert_ne!(refused.origin.outcome, RestoreOutcome::Succeeded);
            LIVE.with(|slot| {
                let mut slot = slot.borrow_mut();
                let live = slot.as_mut().unwrap();
                let mut corrupt = fixture_session();
                corrupt.hash = Some([23; 32]);
                for at in 0..live.chain.links() {
                    corrupt
                        .snapshots
                        .insert(live.chain.point(at).snap.0, [23; 32]);
                }
                live.session = Box::new(corrupt);
            });
            crate::job_observation::begin();
            assert!(target.restore(&snapshot).is_err());
            let failed = take_job_observation().unwrap();
            assert_eq!(failed.origin.source, RestoreSource::LocalExact);
            assert_eq!(
                failed.origin.stage,
                crate::job_observation::RestoreStage::Certificate
            );
            assert_eq!(failed.origin.outcome, RestoreOutcome::Failed);
            LIVE.with(|slot| slot.borrow_mut().take());
        }
    }

    #[test]
    fn certified_local_and_imported_restores_do_constant_endpoint_work() {
        let action = FaultOperation::Wait(std::num::NonZeroU16::MIN).into();
        for depth in [1, 8, 32, 128, 1024] {
            for imported in [false, true] {
                let mut target = unbooted_target();
                let index = Arc::new(LocalIndex::new(64 << 20));
                let namespace = Namespace::exact(&[b"certified-restore-fixture"]);
                let mut live = scripted_capture_live_event_history(
                    vec![[17; 32]; depth],
                    false,
                    control_proto::EventHistory {
                        count: 2,
                        digest: [11; 32],
                    },
                );
                if imported {
                    live.chain = Chain::new(
                        Point {
                            snap: SnapId(1),
                            moment: 0,
                        },
                        Some(Shared {
                            index: index.clone(),
                            namespace,
                        }),
                    );
                    live.chain.certify(0, fixture_genesis()).unwrap();
                }
                LIVE.with(|slot| *slot.borrow_mut() = Some(live));
                for _ in 0..depth {
                    target.apply(action);
                }
                let snapshot = target.snapshot().unwrap().unwrap();
                assert_eq!(snapshot.history.len(), depth);
                if imported {
                    let mut importer = scripted_capture_live(Vec::new(), false);
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
                    importer.chain.certify(0, fixture_genesis()).unwrap();
                    LIVE.with(|slot| *slot.borrow_mut() = Some(importer));
                }
                let before = thread_work_telemetry();
                crate::job_observation::new_generation();
                SHUTDOWN_TELEMETRY_CALLS.with(|calls| calls.set(0));
                TRACK_SHUTDOWN_TELEMETRY.with(|flag| flag.set(true));
                SHUTDOWN_CLOSED.with(|flag| flag.set(false));
                crate::job_observation::begin();
                target.restore(&snapshot).unwrap();
                assert_eq!(SHUTDOWN_TELEMETRY_CALLS.with(|calls| calls.get()), 0);
                TRACK_SHUTDOWN_TELEMETRY.with(|flag| flag.set(false));
                let captured = take_job_observation().unwrap();
                assert_eq!(
                    captured.origin.outcome,
                    crate::job_observation::RestoreOutcome::Succeeded
                );
                assert_eq!(
                    captured.origin.source,
                    if imported {
                        crate::job_observation::RestoreSource::SharedImportExact
                    } else {
                        crate::job_observation::RestoreSource::LocalExact
                    }
                );
                assert_eq!(captured.origin.requested_depth, depth as u64);
                assert_eq!(captured.origin.selected_depth, depth as u64);
                assert!(captured.origin.generation.is_some());
                assert_eq!(captured.origin_lookups.import_attempts, u64::from(imported));
                assert_eq!(
                    captured.origin_lookups.import_completed,
                    u64::from(imported)
                );
                assert_eq!(captured.origin.rebuild_attempted, 0);
                assert!(take_job_observation().is_none());
                crate::job_observation::begin();
                target.restore(&snapshot).unwrap();
                let again = take_job_observation().unwrap();
                assert_eq!(
                    again.origin.source,
                    crate::job_observation::RestoreSource::LocalExact
                );
                assert_eq!(again.origin_lookups.import_attempts, 0);
                assert!(target.actions.get().is_none());
                assert_eq!(target.observation(), &snapshot.observation);
                let after = thread_work_telemetry();
                for key in [
                    "session.restore.count",
                    "session.observe.count",
                    "vmm.test.hash_calls",
                    "vmm.test.replay_calls",
                ] {
                    assert_eq!(
                        after[key] - before[key],
                        2,
                        "{key}, depth={depth}, imported={imported}"
                    );
                }
                for key in ["run.new.count", "run.rebuild.count", "run.replay.count"] {
                    assert_eq!(
                        after[key] - before[key],
                        0,
                        "{key}, depth={depth}, imported={imported}"
                    );
                }
                if imported {
                    assert_eq!(
                        after["shared.import.count"] - before["shared.import.count"],
                        1
                    );
                }
                LIVE.with(|slot| slot.borrow_mut().take());
            }
        }
    }

    #[test]
    fn campaign_job_rejects_a_divergent_prefix_inside_a_resident_keyframe() {
        let first_action = FaultOperation::Wait(std::num::NonZeroU16::new(1).unwrap()).into();
        let second_action = FaultOperation::Wait(std::num::NonZeroU16::new(2).unwrap()).into();
        let mut baseline = unbooted_target();
        let mut baseline_live = hashed_live([17; 32]);
        baseline_live.session = Box::new(TimedOutRestore {
            endpoint_state: consonance_client::endpoint::EndpointState::default(),
            endpoint_abandoned: false,
            histories: BTreeMap::new(),

            history_failure: None,

            sdk_failure: None,
            history: control_proto::EventHistory::default(),
            hash: Some([17; 32]),
            hash_calls: 0,
            replay_calls: 0,
            branch_calls: 0,
            sdk_offsets: Vec::new(),
            sdk_event_count: 0,
            run_events: None,
            abandon_on_hash: false,
            hash_failure: false,
            hash_error: None,
            events: BTreeMap::new(),
            current: 1,
            run_hashes: Some(vec![[17; 32], [17; 32]]),
            run_hung: false,
            terminal_when_exhausted: false,
            snapshots: BTreeMap::from([(1, [17; 32])]),
            moment: 0,
            next_snap: 1,
        });
        LIVE.with(|slot| *slot.borrow_mut() = Some(baseline_live));
        baseline.apply(first_action);
        baseline.apply(second_action);
        assert!(!baseline.failed());
        let keyframe = baseline.snapshot().unwrap().unwrap();
        assert_eq!(
            keyframe.history.actions(),
            vec![first_action, second_action]
        );
        LIVE.with(|slot| slot.borrow_mut().take());

        for cached in [false, true] {
            let mut target = unbooted_target();
            let mut live = hashed_live([17; 32]);
            live.session = Box::new(TimedOutRestore {
                endpoint_state: consonance_client::endpoint::EndpointState::default(),
                endpoint_abandoned: false,
                histories: BTreeMap::new(),

                history_failure: None,

                sdk_failure: None,
                history: control_proto::EventHistory::default(),
                hash: Some([17; 32]),
                hash_calls: 0,
                replay_calls: 0,
                branch_calls: 0,
                sdk_offsets: Vec::new(),
                sdk_event_count: 0,
                run_events: None,
                abandon_on_hash: false,
                hash_failure: false,
                hash_error: None,
                events: BTreeMap::new(),
                current: 1,
                run_hashes: Some(vec![[23; 32], [17; 32]]),
                run_hung: false,
                terminal_when_exhausted: false,
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
                searcher::search::rollout::RolloutStop::new(false),
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
            history: crate::history::FaultHistory::fixture(
                &[FaultOperation::Wait(std::num::NonZeroU16::new(1).unwrap()).into()],
                &[Some(
                    replay_endpoint_certificate(
                        [17; 32],
                        (0, [0; 32]),
                        &FaultObservations::default(),
                        false,
                    )
                    .unwrap(),
                )],
                Some(FaultEndpointCertificate::KvmWholeState([17; 32])),
                &(FaultObservations::default()),
            ),
            endpoint: Some(FaultEndpointCertificate::KvmWholeState([17; 32])),
            event_history: Some((0, [0; 32])),
            ..FaultSnapshot::default()
        };
        LIVE.with(|slot| *slot.borrow_mut() = Some(hashed_live([17; 32])));
        crate::job_observation::begin();
        let error = target.restore(&snapshot).unwrap_err().to_string();
        let captured = take_job_observation().unwrap();
        assert_eq!(
            captured.origin.outcome,
            crate::job_observation::RestoreOutcome::Failed
        );
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
                &ActionPlan::compile(live.windows, &live.inherited, &[]).unwrap(),
            )
            .unwrap_err();
        assert!(error.starts_with(WATCHDOG_CUTOFF));
        assert!(live.abandoned);

        let mut target = unbooted_target();
        let snapshot = FaultSnapshot {
            history: crate::history::FaultHistory::fixture(
                &[],
                &[],
                None,
                &(FaultObservations::default()),
            ),
            observation: FaultObservations::default(),
            failed: false,
            endpoint: None,
            event_history: None,
        };
        target.finish_restore(&snapshot, Err(error)).unwrap();
        assert_eq!(target.watchdog_cutoffs(), 1);
        assert!(target.last_action_observations()[0].watchdog_cutoff);
    }

    #[test]
    fn debug_service_is_portable_and_root_identity_includes_saved_state() {
        let standing = test_branch_config(ActionWindows { root_seal: 1000 }, &[], &[]).unwrap();
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
            history: crate::history::FaultHistory::fixture(
                &[FaultOperation::Wait(std::num::NonZeroU16::new(32768).unwrap()).into()],
                &[],
                None,
                &(FaultObservations {
                    ticks: 123,
                    ..FaultObservations::default()
                }),
            ),
            observation: FaultObservations {
                ticks: 123,
                ..FaultObservations::default()
            },
            failed: false,
            endpoint: None,
            event_history: None,
        };
        target.observation.stop = FaultStop::Crash;
        target.action_observations.push(target.observation.clone());
        crate::job_observation::begin();
        crate::job_observation::origin_start(1);
        target
            .finish_restore(&snapshot, Err(format!("{WATCHDOG_CUTOFF}run: expired")))
            .unwrap();
        assert!(target.failed());
        assert!(!target.found_bug());
        assert!(target.snapshot().unwrap().is_none());
        assert!(target.actions().is_empty());
        assert_eq!(target.watchdog_cutoffs(), 1);
        assert_eq!(
            take_job_observation().unwrap().origin.outcome,
            crate::job_observation::RestoreOutcome::Cutoff
        );
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
        assert_eq!(target.actions(), snapshot.history.actions());
        assert!(!target.last_action_observations()[0].watchdog_cutoff);
    }

    #[test]
    fn typed_run_and_snapshot_host_failures_remain_distinct_and_incomplete() {
        let snapshot = FaultSnapshot {
            history: crate::history::FaultHistory::fixture(
                &[],
                &[],
                None,
                &(FaultObservations::default()),
            ),
            observation: FaultObservations::default(),
            failed: false,
            endpoint: None,
            event_history: None,
        };
        for phase in ["run", "snapshot"] {
            for error in [
                SessionError::Hung(WALL_LIMIT),
                SessionError::Abandoned,
                SessionError::Infrastructure("owned UML host failure".into()),
            ] {
                let error: Box<dyn Error> = Box::new(error);
                let mut target = unbooted_target();
                target
                    .finish_restore(&snapshot, Err(session_failure(phase, error.as_ref())))
                    .unwrap();
                assert!(target.failed());
                let watchdog = matches!(
                    error.downcast_ref::<SessionError>(),
                    Some(SessionError::Hung(_))
                );
                assert_eq!(target.watchdog_cutoffs(), u64::from(watchdog));
                assert_eq!(target.infrastructure_failure(), !watchdog);
                assert_eq!(
                    target.last_action_observations()[0].watchdog_cutoff,
                    watchdog
                );
                assert_eq!(
                    target.last_action_observations()[0].infrastructure_failure,
                    !watchdog
                );
            }
            assert!(
                !session_failure(phase, &SessionError::Unboundable).starts_with(WATCHDOG_CUTOFF)
            );
        }
    }

    #[test]
    fn observation_strings_do_not_become_typed_watchdog_errors() {
        for error in [
            SessionError::Hung(WALL_LIMIT).to_string(),
            SessionError::Infrastructure("owned host failure".into()).to_string(),
            format!("{WATCHDOG_CUTOFF}spoof"),
            format!("{INFRASTRUCTURE_FAILURE}spoof"),
        ] {
            for error in [
                Box::<dyn Error>::from(error.clone()),
                Box::new(SessionError::Control(error)),
            ] {
                let classified = session_failure("observe", error.as_ref());
                assert!(!classified.starts_with(WATCHDOG_CUTOFF));
                assert!(!classified.starts_with(INFRASTRUCTURE_FAILURE));
            }
        }
    }

    #[test]
    fn replay_certificate_v4_refuses_reused_unqualified_domains() {
        let hash = [17; 32];
        let history = (0, [0; 32]);
        let observation = FaultObservations::default();
        let certificate = replay_endpoint_certificate(hash, history, &observation, false).unwrap();
        let mut snapshot = FaultSnapshot {
            history: FaultHistory::fixture(
                &[FaultOperation::Wait(std::num::NonZeroU16::MIN).into()],
                &[Some(certificate)],
                Some(FaultEndpointCertificate::KvmWholeState(hash)),
                &observation,
            ),
            observation: observation.clone(),
            failed: false,
            endpoint: Some(FaultEndpointCertificate::KvmWholeState(hash)),
            event_history: Some(history),
        };
        validate_snapshot_prefix_certificates(&snapshot, false).unwrap();
        for previous in [
            postcard::to_allocvec(&("fault-replay-endpoint-v2", hash, &observation, false))
                .unwrap(),
            postcard::to_allocvec(&(
                "fault-replay-endpoint-v2",
                control_proto::EventHistory::VERSION,
                hash,
                history,
                &observation,
                false,
            ))
            .unwrap(),
            postcard::to_allocvec(&(
                "fault-replay-endpoint-v3",
                control_proto::EventHistory::VERSION,
                hash,
                history,
                &observation,
                false,
            ))
            .unwrap(),
        ] {
            snapshot.history = FaultHistory::fixture(
                &snapshot.history.actions(),
                &[Some(Sha256::digest(previous).into())],
                snapshot.endpoint,
                &observation,
            );
            assert!(
                validate_snapshot_prefix_certificates(&snapshot, false)
                    .unwrap_err()
                    .contains("final prefix differs")
            );
        }
    }

    #[test]
    fn retiring_hash_calls_preserve_the_returned_failure_cause() {
        for failure in [
            Some((|| SessionError::Hung(WALL_LIMIT)) as fn() -> SessionError),
            Some(
                (|| SessionError::Infrastructure("host disconnected".into()))
                    as fn() -> SessionError,
            ),
            Some(
                (|| SessionError::Control("malformed complete reply".into()))
                    as fn() -> SessionError,
            ),
            None,
        ] {
            let mut live = hashed_live([17; 32]);
            live.session = Box::new(TimedOutRestore {
                endpoint_state: consonance_client::endpoint::EndpointState::default(),
                endpoint_abandoned: false,
                hash_error: failure,
                abandon_on_hash: true,
                ..fixture_session()
            });
            LIVE.with(|slot| *slot.borrow_mut() = Some(live));
            let error = unbooted_target().state_hash_current().unwrap_err();
            let expected =
                failure.map(|failure| session_failure("current job state hash", &failure()));
            if let Some(expected) = expected {
                assert_eq!(error, expected);
            } else {
                assert!(error.starts_with(INFRASTRUCTURE_FAILURE), "{error}");
                assert!(error.contains("abandoned while hashing"), "{error}");
            }
            LIVE.with(|slot| assert!(slot.borrow().is_none()));
        }
    }

    #[test]
    fn receipt_reconstruction_and_imported_roots_preserve_host_failure_provenance() {
        let action = FaultOperation::Wait(std::num::NonZeroU16::MIN).into();
        for rooted in [false, true] {
            for history_failure in [false, true] {
                for failure in [
                    None,
                    Some((|| SessionError::Hung(WALL_LIMIT)) as fn() -> SessionError),
                    Some(
                        (|| SessionError::Infrastructure("owned worker disconnected".into()))
                            as fn() -> SessionError,
                    ),
                    Some((|| SessionError::Abandoned) as fn() -> SessionError),
                    Some(
                        (|| SessionError::Control("malformed complete reply".into()))
                            as fn() -> SessionError,
                    ),
                ] {
                    let mut source_target = unbooted_target();
                    LIVE.with(|slot| {
                        *slot.borrow_mut() = Some(scripted_capture_live(vec![[17; 32]], false))
                    });
                    let mut snapshot = source_target.apply_and_snapshot(action).unwrap().unwrap();
                    let mut source = LIVE.with(|slot| slot.borrow_mut().take().unwrap());
                    let mut session = fixture_session();
                    session.run_hashes = Some(vec![[17; 32]]);
                    let mut live = hashed_live([17; 32]);
                    if rooted {
                        let point = source.chain.point(source.chain.links() - 1);
                        let root = export_root(source.session.as_mut(), point.snap, [17; 32], &[])
                            .unwrap();
                        let (imported, moment, _) = import_root(&mut session, &root).unwrap();
                        session.replay_snapshot(imported).unwrap();
                        session.moment = moment;
                        snapshot.history = FaultHistory::genesis(
                            HistoryReceipt::genesis(
                                [0; 32],
                                snapshot.endpoint.unwrap(),
                                snapshot.event_history,
                                &snapshot.observation,
                            )
                            .unwrap(),
                        )
                        .unwrap();
                        live.chain = Chain::new(
                            Point {
                                snap: imported,
                                moment,
                            },
                            None,
                        );
                        live.chain
                            .certify(0, snapshot.history.receipt().clone())
                            .unwrap();
                        live.setup = imported;
                    }
                    drop(source);
                    session.hash_calls = 0;
                    if history_failure {
                        session.history_failure = failure;
                    } else {
                        session.hash_error = failure;
                        session.abandon_on_hash = failure.is_some();
                    }
                    live.session = Box::new(session);
                    LIVE.with(|slot| *slot.borrow_mut() = Some(live));
                    validate_snapshot_prefix_certificates(&snapshot, false).unwrap();
                    let before = thread_work_telemetry();
                    let mut target = unbooted_target();
                    let result = target.restore(&snapshot);
                    let after = thread_work_telemetry();
                    assert_eq!(
                        after["run.rebuild.count"] - before["run.rebuild.count"],
                        u64::from(!rooted)
                    );
                    assert!(after["vmm.test.hash_calls"] > before["vmm.test.hash_calls"]);
                    if let Some(failure) = failure {
                        let failure = failure();
                        let ordinary = matches!(failure, SessionError::Control(_));
                        let watchdog = matches!(failure, SessionError::Hung(_));
                        if ordinary {
                            assert!(
                                result
                                    .unwrap_err()
                                    .to_string()
                                    .contains("malformed complete reply")
                            );
                            assert!(!target.infrastructure_failure());
                            assert_eq!(target.watchdog_cutoffs(), 0);
                        } else {
                            result.unwrap();
                            assert_eq!(target.infrastructure_failure(), !watchdog);
                            assert_eq!(target.watchdog_cutoffs(), u64::from(watchdog));
                            assert!(target.actions().is_empty());
                            assert!(!target.found_bug());
                            assert!(target.snapshot().unwrap().is_none());
                            assert_eq!(target.execution_ticks(), 17);
                            assert!(
                                crate::campaign::require_completed_attempt(
                                    &target,
                                    "receipt reconstruction"
                                )
                                .unwrap_err()
                                .to_string()
                                .contains("reservation not admitted")
                            );
                        }
                    } else {
                        result.unwrap();
                        assert_eq!(target.history, snapshot.history);
                        assert!(!target.failed());
                    }
                    LIVE.with(|slot| slot.borrow_mut().take());
                }
            }
        }
    }

    #[test]
    fn ordered_history_preserves_typed_host_failure_before_admission() {
        let snapshot = FaultSnapshot {
            history: FaultHistory::genesis(fixture_genesis()).unwrap(),
            endpoint: Some(FaultEndpointCertificate::KvmWholeState([17; 32])),
            event_history: Some((0, [0; 32])),
            ..FaultSnapshot::default()
        };
        validate_snapshot_prefix_certificates(&snapshot, false).unwrap();
        for (failure, watchdog) in [
            (
                (|| SessionError::Hung(WALL_LIMIT)) as fn() -> SessionError,
                true,
            ),
            (
                (|| SessionError::Infrastructure("owned worker disconnected".into()))
                    as fn() -> SessionError,
                false,
            ),
            ((|| SessionError::Abandoned) as fn() -> SessionError, false),
        ] {
            let mut target = unbooted_target();
            let mut live = hashed_live([17; 32]);
            live.session = Box::new(TimedOutRestore {
                endpoint_state: consonance_client::endpoint::EndpointState::default(),
                endpoint_abandoned: false,
                hash: Some([17; 32]),
                history_failure: Some(failure),
                ..TimedOutRestore::default()
            });
            LIVE.with(|slot| *slot.borrow_mut() = Some(live));
            let error = target.event_history_current().unwrap_err();
            assert!(
                error.starts_with(if watchdog {
                    WATCHDOG_CUTOFF
                } else {
                    INFRASTRUCTURE_FAILURE
                }),
                "{error}"
            );
            LIVE.with(|slot| assert!(slot.borrow().is_none()));
            target.finish_restore(&snapshot, Err(error)).unwrap();
            assert_eq!(target.infrastructure_failure(), !watchdog);
            assert_eq!(target.watchdog_cutoffs(), u64::from(watchdog));
            assert_eq!(target.execution_ticks(), 17);
            assert!(target.actions().is_empty());
            assert!(!target.found_bug());
            assert!(target.snapshot().unwrap().is_none());
            let error =
                crate::campaign::require_completed_attempt(&target, "ordered history").unwrap_err();
            assert!(
                error.to_string().contains("reservation not admitted"),
                "{error}"
            );
        }
        let mut target = unbooted_target();
        let mut live = hashed_live([17; 32]);
        live.session = Box::new(TimedOutRestore {
            endpoint_state: consonance_client::endpoint::EndpointState::default(),
            endpoint_abandoned: false,
            hash: Some([17; 32]),
            history_failure: Some(|| {
                SessionError::Control(format!("{INFRASTRUCTURE_FAILURE}forged transport cause"))
            }),
            ..TimedOutRestore::default()
        });
        LIVE.with(|slot| *slot.borrow_mut() = Some(live));
        let error = target.event_history_current().unwrap_err();
        assert!(!error.starts_with(INFRASTRUCTURE_FAILURE));
        assert!(!error.starts_with(WATCHDOG_CUTOFF));
        assert_eq!(
            target
                .finish_restore(&snapshot, Err(error.clone()))
                .unwrap_err()
                .to_string(),
            error
        );
        assert!(!target.infrastructure_failure());
        assert_eq!(target.watchdog_cutoffs(), 0);
        LIVE.with(|slot| assert!(slot.borrow().is_none()));
    }

    #[test]
    fn sdk_suffix_failures_preserve_provenance_in_observation_and_cursor_rebuilds() {
        let snapshot = FaultSnapshot {
            history: FaultHistory::genesis(fixture_genesis()).unwrap(),
            endpoint: Some(FaultEndpointCertificate::KvmWholeState([17; 32])),
            event_history: Some((0, [0; 32])),
            ..FaultSnapshot::default()
        };
        validate_snapshot_prefix_certificates(&snapshot, false).unwrap();
        for rebuild in [false, true] {
            for failure in [
                None,
                Some((|| SessionError::Hung(WALL_LIMIT)) as fn() -> SessionError),
                Some(
                    (|| SessionError::Infrastructure("worker disconnected".into()))
                        as fn() -> SessionError,
                ),
                Some((|| SessionError::Abandoned) as fn() -> SessionError),
                Some(
                    (|| SessionError::Control("malformed complete reply".into()))
                        as fn() -> SessionError,
                ),
            ] {
                let mut live = hashed_live([17; 32]);
                live.session = Box::new(TimedOutRestore {
                    endpoint_state: consonance_client::endpoint::EndpointState::default(),
                    endpoint_abandoned: false,
                    sdk_failure: failure,
                    ..fixture_session()
                });
                let result = if rebuild {
                    live.replay(SnapId(1))
                } else {
                    live.observe(FaultStop::Deadline).map(|_| ())
                };
                assert_eq!(
                    live.work_counters().into_iter().collect::<BTreeMap<_, _>>()["vmm.test.sdk_calls"],
                    1
                );
                let Some(failure) = failure else {
                    result.unwrap();
                    assert!(live.cursor.decoder.is_some());
                    continue;
                };
                assert!(live.cursor.decoder.is_none());
                let error = result.unwrap_err();
                let failure = failure();
                let watchdog = matches!(failure, SessionError::Hung(_));
                let ordinary = matches!(failure, SessionError::Control(_));
                assert_eq!(error.starts_with(WATCHDOG_CUTOFF), watchdog, "{error}");
                assert_eq!(
                    error.starts_with(INFRASTRUCTURE_FAILURE),
                    !watchdog && !ordinary,
                    "{error}"
                );
                let mut target = unbooted_target();
                let result = target.finish_restore(&snapshot, Err(error));
                if ordinary {
                    assert!(
                        result
                            .unwrap_err()
                            .to_string()
                            .contains("malformed complete reply")
                    );
                    assert!(!target.infrastructure_failure());
                    assert_eq!(target.watchdog_cutoffs(), 0);
                } else {
                    result.unwrap();
                    assert_eq!(target.infrastructure_failure(), !watchdog);
                    assert_eq!(target.watchdog_cutoffs(), u64::from(watchdog));
                    assert!(!target.found_bug());
                    assert!(target.actions().is_empty());
                    assert!(target.snapshot().unwrap().is_none());
                    assert!(
                        crate::campaign::require_completed_attempt(&target, "SDK suffix")
                            .unwrap_err()
                            .to_string()
                            .contains("reservation not admitted")
                    );
                }
            }
        }
    }

    #[test]
    fn ordered_history_distinguishes_abandoned_matching_sessions_from_identity_errors() {
        let target = unbooted_target();
        for matching in [false, true] {
            let mut live = hashed_live([17; 32]);
            if !matching {
                live.key = [1; 32];
            }
            live.abandoned = true;
            LIVE.with(|slot| *slot.borrow_mut() = Some(live));
            let error = target.event_history_current().unwrap_err();
            assert_eq!(
                error.starts_with(INFRASTRUCTURE_FAILURE),
                matching,
                "{error}"
            );
            LIVE.with(|slot| assert_eq!(slot.borrow().is_none(), matching));
            LIVE.with(|slot| slot.borrow_mut().take());
        }
        let missing = target.event_history_current().unwrap_err();
        assert!(!missing.starts_with(INFRASTRUCTURE_FAILURE));
    }

    #[test]
    fn typed_worker_disconnect_refuses_campaign_evaluation_and_certification() {
        use searcher::search::campaign::{CampaignJobResult, Evaluation, Reporting};

        let config = FaultConfig {
            knobs: Vec::new(),
            ram_mib: DEFAULT_RAM_MIB,
            backend: GuestBackend::Vm,
            root: None,
            boot_cache: None,
        };
        let game = crate::campaign::fixture_workload(b"kernel", b"initramfs", &config);
        let run = crate::campaign::FaultCampaignRun {
            vocabulary: crate::bundle::FaultVocabulary::new(1, vec![1]).unwrap(),
        };
        let mut target = unbooted_target();
        let snapshot = FaultSnapshot {
            history: FaultHistory::genesis(fixture_genesis()).unwrap(),
            endpoint: Some(FaultEndpointCertificate::KvmWholeState([17; 32])),
            event_history: Some((0, [0; 32])),
            ..FaultSnapshot::default()
        };
        validate_snapshot_prefix_certificates(&snapshot, false).unwrap();
        let before_ticks = target.execution_ticks();
        let error = SessionError::Infrastructure(
            "session worker disconnected: unexpected end of file".into(),
        );
        target
            .finish_restore(&snapshot, Err(session_failure("worker replay", &error)))
            .unwrap();
        assert!(target.infrastructure_failure());
        assert_eq!(target.watchdog_cutoffs(), 0);
        assert_eq!(target.execution_ticks(), before_ticks);
        assert!(target.actions().is_empty());
        assert!(!target.found_bug());
        assert!(target.snapshot().unwrap().is_none());
        let result = CampaignJobResult {
            preparation_failure: None,
            hang: None,
            actions: Vec::new(),
        };
        for error in [
            crate::campaign::require_completed_attempt(&target, "worker replay").unwrap_err(),
            game.certified_result_sha256(&result, &target).unwrap_err(),
            game.rollout_outcome(&run, &target).unwrap_err(),
        ] {
            assert!(
                error.to_string().contains("host infrastructure failure"),
                "{error}"
            );
            assert!(
                error.to_string().contains("reservation not admitted"),
                "{error}"
            );
        }
    }

    #[test]
    fn other_reconstruction_errors_remain_fatal() {
        let mut target = unbooted_target();
        let snapshot = FaultSnapshot {
            history: crate::history::FaultHistory::fixture(
                &[],
                &[],
                None,
                &(FaultObservations::default()),
            ),
            observation: FaultObservations::default(),
            failed: false,
            endpoint: None,
            event_history: None,
        };
        let error = target
            .finish_restore(&snapshot, Err("replay: invalid snapshot".into()))
            .unwrap_err();
        assert_eq!(error.to_string(), "replay: invalid snapshot");
        assert_eq!(target.watchdog_cutoffs(), 0);
    }

    fn reference_plan(
        windows: ActionWindows,
        inherited: &[StandingWindow],
        actions: &[FaultAction],
    ) -> (ServiceConfig, CoverageSchedule) {
        let mut standing = inherited.to_vec();
        let mut rows = Vec::new();
        for (index, action) in actions.iter().enumerate() {
            let window = windows.window(actions, index).unwrap();
            standing.extend(action_delta(*action, window).standing);
            rows.push((window.0, window.1, u64::from(action.coverage_quantum.get())));
        }
        let encoded = encode_windows(&standing).unwrap();
        let mut configuration = u32::try_from(encoded.len()).unwrap().to_le_bytes().to_vec();
        configuration.extend_from_slice(&encoded);
        for &(start, end, quantum) in &rows {
            configuration.extend_from_slice(&start.to_le_bytes());
            configuration.extend_from_slice(&end.to_le_bytes());
            configuration.extend_from_slice(&u16::try_from(quantum).unwrap().to_le_bytes());
        }
        (
            ServiceConfig {
                local_services: LOCAL_SERVICES.to_vec(),
                identity: SERVICE_IDENTITY.to_vec(),
                configuration,
            },
            CoverageSchedule::new(environment::channel::DEFAULT_COVERAGE_QUANTUM, rows).unwrap(),
        )
    }

    #[test]
    fn compiled_prefix_plans_preserve_exact_service_bytes_and_quantums() {
        use crate::target::{SUPERVISOR_TICK_NANOS, TIMELINE_STEPS};
        use fault_policy::{DecisionClass, Fault, ParkTarget, Span, process_target};
        use std::num::NonZeroU16;

        let ticks = |value| NonZeroU16::new(value).unwrap();
        let root = ActionWindows { root_seal: 1_000 };
        let inherited = [StandingWindow {
            class: DecisionClass::Process.as_u16(),
            target: process_target(9, &Fault::ProcEventKill { rarity: 5 }),
            start: 17,
            end: u64::MAX,
        }];
        let actions = [
            FaultAction::new(FaultOperation::Wait(ticks(2)), ticks(64)),
            FaultAction::new(FaultOperation::Kill(3, ticks(3)), ticks(8)),
            FaultAction::new(
                FaultOperation::EventKill {
                    node: 4,
                    rarity: 7,
                    ticks: ticks(1),
                },
                ticks(32768),
            ),
            FaultAction::new(
                FaultOperation::EventPark {
                    node: 5,
                    edges: 13,
                    hold_us: 7_000,
                    ticks: ticks(4),
                    target: ParkTarget::new(8, 16),
                },
                ticks(1),
            ),
            FaultAction::new(FaultOperation::Pause(6, ticks(5)), ticks(1024)),
            FaultAction::new(FaultOperation::Restart(7, ticks(8)), ticks(2)),
            FaultAction::new(FaultOperation::Hook(99, ticks(1)), ticks(16)),
        ];
        let ends = [0, 2, 5, 6, 10, 15, 23, 24];
        let moment = |ticks| root.root_seal + ticks * SUPERVISOR_TICK_NANOS;
        for depth in 0..=actions.len() {
            TIMELINE_STEPS.with(|steps| steps.set(0));
            let plan = ActionPlan::compile(root, &inherited, &actions[..depth]).unwrap();
            assert_eq!(TIMELINE_STEPS.with(std::cell::Cell::get), depth);
            assert_eq!(plan.deadline, moment(ends[depth]));
            let (expected_config, expected_schedule) =
                reference_plan(root, &inherited, &actions[..depth]);
            assert_eq!(plan.service_config(), expected_config);
            assert_eq!(plan.coverage_schedule.encode(), expected_schedule.encode());
            let mut direct = plan.handler.clone();
            assert!(Arc::ptr_eq(&plan.handler.0, &direct.0));
            let mut decoded =
                StandingHandler::from_configuration(&expected_config.configuration).unwrap();
            let standing = Question::with_request_id(1, STANDING_NAMESPACE, Vec::new()).unwrap();
            let quantum = Question::with_request_id(
                2,
                environment::channel::SERVICE_COVERAGE_QUANTUM,
                1_u64.to_le_bytes().to_vec(),
            )
            .unwrap();
            for at in [
                999,
                1_000,
                moment(2) - 1,
                moment(2),
                moment(5),
                moment(6),
                moment(10),
                moment(15),
                moment(17),
                moment(23),
                moment(24),
            ] {
                for question in [&standing, &quantum] {
                    assert_eq!(
                        direct.respond(at, question).unwrap(),
                        decoded.respond(at, question).unwrap()
                    );
                }
                assert_eq!(
                    plan.coverage_schedule.quantum_at(at),
                    expected_schedule.quantum_at(at)
                );
            }
        }
        let full = ActionPlan::compile(root, &inherited, &actions).unwrap();
        assert_eq!(
            full.handler.0.coverage,
            [
                (moment(0), moment(2), ticks(64)),
                (moment(2), moment(5), ticks(8)),
                (moment(5), moment(6), ticks(32768)),
                (moment(6), moment(10), ticks(1)),
                (moment(10), moment(15), ticks(1024)),
                (moment(15), moment(23), ticks(2)),
                (moment(23), moment(24), ticks(16)),
            ]
        );
        let mut expected = inherited.to_vec();
        for (node, fault, start, end) in [
            (3, Fault::ProcRestart, moment(2), moment(5)),
            (4, Fault::ProcEventKill { rarity: 7 }, moment(5), u64::MAX),
            (
                5,
                Fault::ProcEventPark {
                    edges: 13,
                    hold: Span(7_000_000),
                    target: ParkTarget::new(8, 16),
                },
                moment(6),
                moment(10),
            ),
            (
                6,
                Fault::ProcPause(Span(5 * SUPERVISOR_TICK_NANOS)),
                moment(10),
                moment(15),
            ),
            (7, Fault::ProcRestart, moment(15), moment(17)),
            (0, Fault::RunHook(99), moment(23), moment(24)),
        ] {
            expected.push(StandingWindow {
                class: DecisionClass::Process.as_u16(),
                target: process_target(node, &fault),
                start,
                end,
            });
        }
        assert_eq!(full.handler.0.windows, expected);
    }

    #[test]
    fn compiled_plans_preserve_empty_maximum_roots_and_reject_overflow_and_size_limits() {
        use crate::target::SUPERVISOR_TICK_NANOS;
        let action = FaultAction::from(FaultOperation::Wait(std::num::NonZeroU16::MIN));
        let root = ActionWindows {
            root_seal: u64::MAX,
        };
        let empty = ActionPlan::compile(root, &[], &[]).unwrap();
        assert_eq!(empty.deadline, u64::MAX);
        assert_eq!(empty.service_config(), reference_plan(root, &[], &[]).0);
        assert_eq!(
            empty.coverage_schedule.quantum_at(u64::MAX),
            environment::channel::DEFAULT_COVERAGE_QUANTUM
        );
        let root = ActionWindows {
            root_seal: u64::MAX - SUPERVISOR_TICK_NANOS,
        };
        assert_eq!(
            ActionPlan::compile(root, &[], &[action]).unwrap().deadline,
            u64::MAX
        );
        assert_eq!(
            ActionPlan::compile(root, &[], &[action, action])
                .err()
                .unwrap()
                .to_string(),
            "action timeline overflows guest time"
        );
        let limit = (environment::channel::MAX_CHANNEL_BYTES - 14) / 24;
        let actions = vec![action; limit + 1];
        let root = ActionWindows { root_seal: 1000 };
        assert!(ActionPlan::compile(root, &[], &actions[..limit]).is_ok());
        assert_eq!(
            ActionPlan::compile(root, &[], &actions)
                .err()
                .unwrap()
                .to_string(),
            ChannelError::TooLarge.to_string()
        );
        let inherited = [StandingWindow {
            class: 1,
            target: vec![0; usize::from(u16::MAX) + 1],
            start: 0,
            end: u64::MAX,
        }];
        assert!(ActionPlan::compile(root, &inherited, &[]).is_err());
    }

    #[test]
    fn executed_and_rebuilt_prefixes_compile_once_and_cached_children_do_no_planning() {
        use crate::target::TIMELINE_STEPS;
        let actions = [
            FaultAction::from(FaultOperation::Wait(std::num::NonZeroU16::new(2).unwrap()))
                .with_choice(1),
            FaultAction::from(FaultOperation::Kill(
                3,
                std::num::NonZeroU16::new(3).unwrap(),
            ))
            .with_choice(2),
            FaultAction::from(FaultOperation::Wait(std::num::NonZeroU16::MIN)).with_choice(3),
        ];
        let inherited = vec![StandingWindow {
            class: 1,
            target: vec![7],
            start: 0,
            end: u64::MAX,
        }];
        let mut history = FaultHistory::genesis(fixture_genesis()).unwrap();
        let mut source = scripted_capture_live(vec![[17; 32]; actions.len()], false);
        source.inherited = inherited.clone();
        PLANNED_BRANCHES.with(|branches| *branches.borrow_mut() = Some(Vec::new()));
        for (index, action) in actions.iter().enumerate() {
            TIMELINE_STEPS.with(|steps| steps.set(0));
            let (_, receipt) = source.advance(&history, *action, RunKind::New).unwrap();
            assert_eq!(TIMELINE_STEPS.with(std::cell::Cell::get), index + 1);
            history = history.push(receipt.unwrap()).unwrap();
        }
        let original = PLANNED_BRANCHES.with(|branches| branches.borrow_mut().take().unwrap());
        for (index, (seed, config, schedule)) in original.iter().enumerate() {
            let (expected_config, expected_schedule) =
                reference_plan(source.windows, &inherited, &actions[..=index]);
            assert_eq!(*seed, actions[index].entropy_seed(SEED));
            assert_eq!(config, &expected_config);
            assert_eq!(
                schedule.as_ref().unwrap().encode(),
                expected_schedule.encode()
            );
        }
        assert_eq!(original.len(), actions.len());
        TIMELINE_STEPS.with(|steps| steps.set(0));
        let parent = history.ancestor(actions.len() - 1).unwrap();
        let (_, cached) = source
            .advance(&parent, actions[actions.len() - 1], RunKind::New)
            .unwrap();
        assert_eq!(cached.as_ref(), Some(history.receipt()));
        assert_eq!(TIMELINE_STEPS.with(std::cell::Cell::get), 0);
        let mut rebuilt = scripted_capture_live(vec![[17; 32]; actions.len()], false);
        rebuilt.inherited = inherited;
        PLANNED_BRANCHES.with(|branches| *branches.borrow_mut() = Some(Vec::new()));
        assert!(
            rebuilt
                .ensure_certified_prefix(&history, true)
                .unwrap()
                .is_ok()
        );
        let reconstruction =
            PLANNED_BRANCHES.with(|branches| branches.borrow_mut().take().unwrap());
        assert_eq!(TIMELINE_STEPS.with(std::cell::Cell::get), 6);
        assert_eq!(reconstruction, original);
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
        let configuration = test_branch_config(windows, &[], &actions).unwrap();
        let schedule = test_coverage_schedule(windows, &actions).unwrap();
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
    fn a_standing_question_with_the_previous_poll_returns_windows_closed_since() {
        let window = |start, end| StandingWindow {
            class: process_proto::PROCESS_CLASS,
            target: process_proto::encode_target(0, &process_proto::ProcessAction::Restart),
            start,
            end,
        };
        let mut handler = StandingHandler(Arc::new(StandingPlan {
            configuration: Vec::new(),
            windows: vec![
                window(10, 20),
                window(40, 80),
                window(100, 400),
                window(500, 600),
            ],
            coverage: Vec::new(),
        }));
        let mut answer = |after| {
            let question = Question::with_request_id(
                1,
                STANDING_NAMESPACE,
                process_proto::encode_standing_question(after),
            )
            .unwrap();
            let ServiceResponse::Answered(ChannelAnswer::Data(bytes)) =
                handler.respond(300, &question).unwrap()
            else {
                panic!("the standing service must answer");
            };
            let (moment, iter) = fault_policy::parse_standing(&bytes).unwrap();
            assert_eq!(moment, 300);
            iter.map(|entry| (entry.start, entry.end))
                .collect::<Vec<_>>()
        };
        assert_eq!(answer(None), [(100, 400)]);
        assert_eq!(answer(Some(15)), [(10, 20), (40, 80), (100, 400)]);
        assert_eq!(answer(Some(20)), [(40, 80), (100, 400)]);
        assert_eq!(answer(Some(900)), [(100, 400)]);
        let malformed = Question::with_request_id(2, STANDING_NAMESPACE, vec![1, 2, 3]).unwrap();
        assert!(handler.respond(300, &malformed).is_err());
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
        let original = test_branch_config(root, &[], &prefix).unwrap();
        let active = active_root_windows(&[], root, &prefix, moment).unwrap();
        let payload = encode_root_payload([7; 32], &active, b"snapshot", [9; 32]).unwrap();
        let decoded = decode_root_payload(&payload, [7; 32]).unwrap();
        assert_eq!(decoded.snapshot, b"snapshot");
        assert_eq!(root_standing(&payload).unwrap(), active);
        let continuation =
            test_branch_config(ActionWindows { root_seal: moment }, &decoded.standing, &[])
                .unwrap();
        let mut original = StandingHandler::from_configuration(&original.configuration).unwrap();
        let mut continuation =
            StandingHandler::from_configuration(&continuation.configuration).unwrap();
        let question = Question::with_request_id(1, STANDING_NAMESPACE, Vec::new()).unwrap();
        assert_eq!(
            original.respond(moment, &question).unwrap(),
            continuation.respond(moment, &question).unwrap()
        );
        let since_start = Question::with_request_id(
            2,
            STANDING_NAMESPACE,
            process_proto::encode_standing_question(Some(0)),
        )
        .unwrap();
        assert_eq!(
            original.respond(moment, &since_start).unwrap(),
            continuation.respond(moment, &since_start).unwrap()
        );
        let rooted = ActionWindows { root_seal: moment };
        let suffix = [FaultAction::from(FaultOperation::Wait(
            std::num::NonZeroU16::MIN,
        ))];
        let nested_moment = rooted.window(&suffix, 0).unwrap().1;
        let nested_active =
            active_root_windows(&decoded.standing, rooted, &suffix, nested_moment).unwrap();
        let nested_payload =
            encode_root_payload([7; 32], &nested_active, b"nested", [9; 32]).unwrap();
        let nested = decode_root_payload(&nested_payload, [7; 32]).unwrap();
        let nested_config = test_branch_config(
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
    fn inactive_debug_declares_local_but_active_empty_debug_remains_external_and_validates_status()
    {
        let config = test_branch_config(ActionWindows { root_seal: 0 }, &[], &[]).unwrap();
        let factory = service_factory();
        let mut standing = factory(&config).unwrap();
        assert_eq!(standing.local_services(), &LOCAL_SERVICES);
        let active = debug_configuration(&config.configuration, None).unwrap();
        let mut debug = factory(&active).unwrap();
        assert!(debug.local_services().is_empty());
        let mut forged_active = active.clone();
        forged_active.local_services = LOCAL_SERVICES.to_vec();
        assert!(factory(&forged_active).is_err());
        let status = process_proto::debug::Status {
            acknowledged: 3,
            running: true,
            exit_code: None,
        }
        .encode();
        let question =
            Question::with_request_id(7, process_proto::debug::NAMESPACE, status).unwrap();
        for handler in [&mut standing, &mut debug] {
            assert_eq!(
                handler.respond(17, &question).unwrap(),
                ServiceResponse::Answered(ChannelAnswer::Data(Vec::new()))
            );
            let malformed =
                Question::with_request_id(8, process_proto::debug::NAMESPACE, Vec::new()).unwrap();
            assert!(handler.respond(17, &malformed).is_err());
        }
        let message = process_proto::debug::Message {
            id: 4,
            operation: process_proto::debug::Operation::Execute,
            data: b"echo same-request".to_vec(),
        };
        let attached = debug_configuration(&config.configuration, Some(&message)).unwrap();
        let mut debug = factory(&attached).unwrap();
        assert!(debug.local_services().is_empty());
        assert_eq!(
            debug.respond(17, &question).unwrap(),
            ServiceResponse::Answered(ChannelAnswer::Data(message.encode().unwrap()))
        );
        let mut forged = config;
        forged.local_services.clear();
        assert!(factory(&forged).is_err());
    }

    #[test]
    fn the_factory_serves_the_nominal_service_the_setup_point_was_sealed_under() {
        let factory = service_factory();
        let handler = factory(&ServiceConfig::default()).expect("nominal service");
        assert_eq!(handler.identity(), ServiceConfig::default().identity);
        let own = factory(&ServiceConfig {
            local_services: LOCAL_SERVICES.to_vec(),
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
                local_services: Vec::new(),
                identity: b"another-package".to_vec(),
                configuration: Vec::new(),
            })
            .is_err()
        );
    }
    #[test]
    fn replay_and_resident_keyframes_reject_equal_endpoints_with_different_event_history() {
        let mut target = unbooted_target();
        let endpoint = [17; 32];
        let install = |digest| {
            let mut live = hashed_live(endpoint);
            live.session = Box::new(TimedOutRestore {
                endpoint_state: consonance_client::endpoint::EndpointState::default(),
                endpoint_abandoned: false,
                histories: BTreeMap::new(),
                hash_failure: false,
                hash_error: None,

                hash_calls: 0,
                replay_calls: 0,
                branch_calls: 0,
                sdk_offsets: Vec::new(),
                sdk_event_count: 0,
                run_events: None,
                abandon_on_hash: false,
                history_failure: None,
                sdk_failure: None,
                hash: Some(endpoint),
                history: control_proto::EventHistory { count: 2, digest },
                events: BTreeMap::new(),
                current: 1,
                run_hashes: Some(vec![endpoint]),
                run_hung: false,
                terminal_when_exhausted: false,
                snapshots: BTreeMap::from([(1, endpoint)]),
                moment: 0,
                next_snap: 1,
            });
            LIVE.with(|slot| *slot.borrow_mut() = Some(live));
        };
        install([11; 32]);
        target.apply_replayed(FaultAction::new(
            crate::target::FaultOperation::Wait(std::num::NonZeroU16::new(1).unwrap()),
            std::num::NonZeroU16::new(1).unwrap(),
        ));
        assert!(!target.failed());
        let snapshot = target.snapshot().unwrap().unwrap();
        let mut missing = snapshot.clone();
        missing.event_history = None;
        assert!(
            validate_snapshot_prefix_certificates(&missing, false)
                .unwrap_err()
                .contains("lacks ordered event history")
        );
        assert_eq!(snapshot.history.len(), 1);
        let certificate = snapshot.history.certificates()[0].unwrap();
        assert_eq!(
            target
                .snapshot()
                .unwrap()
                .unwrap()
                .history
                .receipt()
                .certificate(),
            Some(certificate)
        );
        let observation = target.observation().clone();
        install([12; 32]);
        assert_eq!(target.state_hash_current().unwrap(), endpoint);
        assert_eq!(target.observation(), &observation);
        assert_eq!(
            target.event_history_current().unwrap().0,
            snapshot.event_history.unwrap().0
        );
        assert!(target.snapshot().unwrap_err().contains("final prefix"));
        let mut restored = unbooted_target();
        let error = restored.restore(&snapshot).unwrap_err().to_string();
        assert!(
            error.contains("differs from its snapshot certificate"),
            "{error}"
        );
        install([11; 32]);
        restored.restore(&snapshot).unwrap();
        assert_eq!(
            restored
                .snapshot()
                .unwrap()
                .unwrap()
                .history
                .receipt()
                .certificate(),
            Some(certificate)
        );
        LIVE.with(|slot| slot.borrow_mut().take());
    }

    #[test]
    fn terminal_and_zero_action_job_certification_binds_history() {
        use searcher::search::{
            campaign::{CampaignActionResult, CampaignJobResult, Reporting},
            rollout::{ExecutionDisposition, Outcome},
        };
        let config = FaultConfig {
            knobs: Vec::new(),
            ram_mib: DEFAULT_RAM_MIB,
            backend: GuestBackend::Vm,
            root: None,
            boot_cache: None,
        };
        let game = crate::campaign::fixture_workload(b"kernel", b"initramfs", &config);
        for terminal in [false, true] {
            let mut target = unbooted_target();
            let mut result = CampaignJobResult::<crate::campaign::FaultWorkload> {
                preparation_failure: None,
                hang: None,
                actions: Vec::new(),
            };
            if terminal {
                let action = FaultAction::new(
                    crate::target::FaultOperation::Wait(std::num::NonZeroU16::new(1).unwrap()),
                    std::num::NonZeroU16::new(1).unwrap(),
                );
                target.actions = std::sync::OnceLock::from(vec![action]);
                target.observation.stop = FaultStop::Crash;
                result.actions.push(CampaignActionResult {
                    action,
                    observations: vec![target.observation.clone()],
                    milestones: Default::default(),
                    outcome: Outcome {
                        objective_reached: true,
                        disposition: ExecutionDisposition::Terminal,
                    },
                    candidate: None,
                });
            }
            let certify = |digest, history_hung: bool| {
                let mut live = hashed_live([17; 32]);
                live.session = Box::new(TimedOutRestore {
                    endpoint_state: consonance_client::endpoint::EndpointState::default(),
                    endpoint_abandoned: false,
                    histories: BTreeMap::new(),
                    hash_failure: false,
                    hash_error: None,

                    hash_calls: 0,
                    replay_calls: 0,
                    branch_calls: 0,
                    sdk_offsets: Vec::new(),
                    sdk_event_count: 0,
                    run_events: None,
                    abandon_on_hash: false,
                    history_failure: history_hung.then_some(|| SessionError::Hung(WALL_LIMIT)),
                    sdk_failure: None,
                    history: control_proto::EventHistory { count: 2, digest },
                    hash: Some([17; 32]),
                    events: BTreeMap::new(),
                    current: 1,
                    run_hashes: None,
                    run_hung: false,
                    terminal_when_exhausted: false,
                    snapshots: BTreeMap::new(),
                    moment: 0,
                    next_snap: 1,
                });
                LIVE.with(|slot| *slot.borrow_mut() = Some(live));
                assert_eq!(target.state_hash_current().unwrap(), [17; 32]);
                if !history_hung {
                    assert_eq!(target.event_history_current().unwrap().0, 2);
                }
                game.certified_result_sha256(&result, &target)
            };
            let first = certify([11; 32], false).unwrap();
            assert_ne!(first, certify([12; 32], false).unwrap());
            assert_eq!(first, certify([11; 32], false).unwrap());
            let error = certify([11; 32], true).unwrap_err().to_string();
            assert!(error.starts_with(WATCHDOG_CUTOFF), "{error}");
            LIVE.with(|slot| assert!(slot.borrow().is_none()));
            LIVE.with(|slot| slot.borrow_mut().take());
        }
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
    encode_root_payload(
        identity,
        standing,
        chain[0].bytes(),
        index.target_commitment(&lease)?,
    )
}

fn active_root_windows(
    inherited: &[StandingWindow],
    windows: ActionWindows,
    actions: &[FaultAction],
    moment: u64,
) -> Result<Vec<StandingWindow>, String> {
    let mut standing = inherited.to_vec();
    standing.extend(standing_windows(windows, actions)?);
    standing.retain(|window| window.start <= moment);
    Ok(standing)
}

fn import_root(
    session: &mut dyn SearchSession,
    bytes: &[u8],
) -> Result<(SnapId, u64, Vec<StandingWindow>), Box<dyn Error>> {
    use consonance_client::cache::LocalIndex;
    let identity = session.root_identity()?;
    let RootPayload {
        standing,
        snapshot,
        target_commitment,
    } = decode_root_payload(bytes, identity)?;
    let index = LocalIndex::new(usize::MAX);
    let mut extent = index.extent(snapshot.len())?;
    extent.bytes_mut().copy_from_slice(snapshot);
    let lease = index.publish(
        Namespace::new(&[&identity]),
        b"root",
        None,
        extent,
        target_commitment,
        1,
    )?;
    let (setup, moment) = session.import_root(&index, &lease)?;
    if standing.iter().any(|window| window.start > moment) {
        return Err("branch root standing window starts after its snapshot moment".into());
    }
    Ok((setup, moment, standing))
}

fn encode_root_payload(
    identity: [u8; 32],
    standing: &[StandingWindow],
    snapshot: &[u8],
    target_commitment: [u8; 32],
) -> Result<Vec<u8>, Box<dyn Error>> {
    let encoded = encode_windows(standing)?;
    let length =
        u32::try_from(encoded.len()).map_err(|_| "branch root standing list is too large")?;
    let mut bytes = b"HMROOT03".to_vec();
    bytes.extend_from_slice(&identity);
    bytes.extend_from_slice(&target_commitment);
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
    target_commitment: [u8; 32],
}

fn decode_root_payload(
    bytes: &[u8],
    identity: [u8; 32],
) -> Result<RootPayload<'_>, Box<dyn Error>> {
    if bytes.len() < 76 || &bytes[..8] != b"HMROOT03" {
        return Err("invalid branch snapshot header".into());
    }
    if bytes[8..40] != identity {
        return Err("branch snapshot setup identity differs".into());
    }
    let length = usize::try_from(u32::from_le_bytes(bytes[72..76].try_into()?))?;
    let end = 76_usize
        .checked_add(length)
        .ok_or("branch root standing length overflows")?;
    let standing = fault_policy::decode_windows(
        bytes
            .get(76..end)
            .ok_or("truncated branch root standing list")?,
    )?;
    let snapshot = bytes.get(end..).ok_or("missing branch root snapshot")?;
    if snapshot.is_empty() {
        return Err("missing branch root snapshot".into());
    }
    Ok(RootPayload {
        standing,
        snapshot,
        target_commitment: bytes[40..72].try_into()?,
    })
}

#[derive(Debug)]
pub struct DebugProgress {
    pub status: Option<process_proto::debug::Status>,
    pub output: Vec<u8>,
}
fn validate_debug_status(question: &Question) -> Result<(), ChannelError> {
    process_proto::debug::Status::decode(question.payload())
        .map(|_| ())
        .map_err(|_| ChannelError::Malformed)
}
const DEBUG_IDENTITY: &[u8] = b"faults-debug-terminal-v2";
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
            validate_debug_status(question)?;
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
        local_services: Vec::new(),
        identity: DEBUG_IDENTITY.to_vec(),
        configuration,
    })
}

#[cfg(all(test, target_os = "linux"))]
mod infrastructure_tests;
