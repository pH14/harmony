// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    cell::RefCell,
    collections::BTreeMap,
    error::Error,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use consonance_client::{
    cache::{CacheError, CacheIndex, Lease, Namespace},
    session::{PortableSnapshot, Session, SessionConfig, SessionError, identity_with_config},
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

use crate::chain::{Chain, Hit, Point, Shared};
use crate::target::{
    ActionWindows, FaultAction, FaultObservations, FaultSnapshot, FaultStop, action_delta,
    actions_key, decode_sdk_events, standing_windows,
};

pub const DEFAULT_RAM_MIB: u32 = 1024;
pub const SERVICE_IDENTITY: &[u8] = b"faults-standing-v1";
const SEED: u64 = 0x4661_756c_744c_6162;
const SETUP_BUDGET: u64 = 120_000_000_000;
const WALL_LIMIT: Duration = Duration::from_secs(60);
const CONSOLE_TAIL: usize = 1_500;
const WATCHDOG_CUTOFF: &str = "fault-guest-watchdog-cutoff: ";

#[cfg(target_arch = "x86_64")]
const CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t tsc=reliable \
    no_timer_check lpj=4000000 random.trust_cpu=off nokaslr nosmp maxcpus=1 \
    nox2apic hpet=disable harmony_pvclock noxsaveopt noxsaves LD_BIND_NOW=1 rdinit=/init";
#[cfg(target_arch = "aarch64")]
const CMDLINE: &str = "console=ttyAMA0 earlycon=pl011,0x09000000 nohlt rdinit=/init";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FaultConfig {
    pub knobs: Vec<String>,
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

const IDENTITY_TAG: &str = "faults-consonance-execution-v5";

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
        configuration: encode_windows(&standing_windows(windows, actions)?)?,
    })
}

#[derive(Debug)]
struct Config {
    key: [u8; 32],
    kernel: Vec<u8>,
    initramfs: Vec<u8>,
    session: SessionConfig,
    cache: Option<Arc<dyn CacheIndex>>,
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
    session: Session,
    setup: SnapId,
    windows: ActionWindows,
    chain: Chain,
    horizons_run: u64,
    abandoned: bool,
    telemetry: LiveTelemetry,
}

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
    ) -> Result<Self, String> {
        let config = Arc::new(Config {
            key: Sha256::digest(identity(kernel, initramfs, config)).into(),
            kernel: kernel.to_vec(),
            initramfs: initramfs.to_vec(),
            session: config.session_config(),
            cache,
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
        Self::new(kernel, initramfs, config, None)
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

    pub fn portable_snapshot(&self) -> Result<PortableSnapshot, String> {
        with_live(&self.config, |live| {
            live.session
                .setup_snapshot()
                .map_err(|error| format!("portable snapshot: {error}"))
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
            Ok((observation, ran)) => {
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
    fn boot(config: &Config) -> Result<Self, String> {
        let boot_started = started();
        let mut session = Session::new_with_config_and_payloads(
            &config.kernel,
            &config.initramfs,
            config.session.clone(),
            Vec::new(),
        )
        .map_err(|error| format!("fault guest boot failed: {error}"))?;
        session.set_service_factory(service_factory());
        let (setup, root_seal) = session.setup_handle();
        let shared = match &config.cache {
            Some(index) => {
                let setup_hash = session
                    .state_hash()
                    .map_err(|error| format!("setup state hash: {error}"))?;
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
            session,
            setup,
            windows: ActionWindows {
                root_seal,
                horizon_nanos: crate::target::DEFAULT_HORIZON_NANOS,
            },
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

    fn publish(&mut self, actions: &[FaultAction], snap: SnapId) -> Result<Option<Lease>, String> {
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
    ) -> Result<Point, String> {
        let lease = self.publish(actions, snap)?;
        self.push_link(actions, Point { snap, moment }, lease)
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
            last = self.seal_link(&actions[..=index], snap, moment)?;
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
            self.seal_link(&next, snap, moment)?;
        }
        Ok(observation)
    }

    fn branch(&mut self, parent: Point, actions: &[FaultAction]) -> Result<(), String> {
        let config = branch_config(self.windows, actions)
            .map_err(|error| format!("branch configuration: {error}"))?;
        let effects = self.staged_effects(actions, parent.moment)?;
        let branch_started = started();
        let result = self
            .session
            .branch_with_service(parent.snap, config, Vec::new(), effects)
            .map_err(|error| format!("branch: {error}"));
        self.telemetry.branches.since(branch_started);
        result
    }

    fn staged_effects(
        &self,
        actions: &[FaultAction],
        floor: u64,
    ) -> Result<Vec<(u64, Effect)>, String> {
        let Some(index) = actions.len().checked_sub(1) else {
            return Ok(Vec::new());
        };
        let Some(perturb) =
            action_delta(actions[index], self.windows.window(actions, index)?).perturb
        else {
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
                .sometimes
                .len()
                .saturating_mul(size_of::<u32>()),
        )
        .saturating_add(snapshot.observation.check.as_ref().map_or(0, |check| {
            check.points.capacity().saturating_mul(size_of::<u32>())
        }))
}

#[must_use]
pub fn identity(kernel: &[u8], initramfs: &[u8], config: &FaultConfig) -> String {
    format!(
        "faults-consonance-whole-vm-v2;session={};horizon-nanos={};\
         action=standing-fault-delta-v2;snapshot=portable-prefix-to-vm-snapshot-v1",
        identity_with_config(kernel, initramfs, &config.session_config()),
        crate::target::DEFAULT_HORIZON_NANOS,
    )
}

pub fn from_paths(
    kernel: &Path,
    initramfs: &Path,
    config: &FaultConfig,
) -> Result<FaultTarget, String> {
    let kernel = std::fs::read(kernel).map_err(|error| format!("read kernel: {error}"))?;
    let initramfs = std::fs::read(initramfs).map_err(|error| format!("read initramfs: {error}"))?;
    FaultTarget::new(&kernel, &initramfs, config, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unbooted_target() -> FaultTarget {
        FaultTarget {
            config: Arc::new(Config {
                key: [0; 32],
                kernel: Vec::new(),
                initramfs: Vec::new(),
                session: FaultConfig {
                    knobs: Vec::new(),
                    ram_mib: DEFAULT_RAM_MIB,
                }
                .session_config(),
                cache: None,
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
    fn reconstruction_cutoff_discards_stale_evidence_and_allows_a_later_restore() {
        let mut target = unbooted_target();
        let snapshot = FaultSnapshot {
            actions: vec![FaultAction::Wait(std::num::NonZeroU16::new(32768).unwrap())],
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
        target.apply(FaultAction::Kill(0));
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
