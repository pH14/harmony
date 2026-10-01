// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::BTreeMap,
    error::Error,
    time::{Duration, Instant},
};

use control_proto::{CrashInfo, CrashKind, EventRef, Moment, SnapId, StopReason};
use environment::{
    channel::Effect,
    input_spec::{ServiceConfig, ServiceFactory},
};
use uml::{Bridge, Capture, Checkpoint, ExitReason, Launch, Stop, VerifiedProfile};

use super::{SdkEvent, SearchSession, SessionError, UmlLaunch};
use crate::cache::{CacheIndex, Lease, Namespace};

const CONSOLE_TAIL: usize = 64 << 10;

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
struct Counters {
    captures: Spent,
    restores: Spent,
    fresh_restores: Spent,
    imports: Spent,
}

pub struct UmlSession {
    session: Option<uml::Session>,
    profile: VerifiedProfile,
    launch: Launch,
    seed: u64,
    progress_limit: Duration,
    factory: ServiceFactory,
    snapshots: BTreeMap<SnapId, Checkpoint>,
    next: u64,
    setup: (SnapId, u64),
    abandoned: bool,
    counters: Counters,
}

impl std::fmt::Debug for UmlSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UmlSession")
            .field("snapshots", &self.snapshots.len())
            .field("setup", &self.setup)
            .field("abandoned", &self.abandoned)
            .finish_non_exhaustive()
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "telemetry reports host time spent in captures and restores"
)]
fn started() -> Instant {
    Instant::now()
}

impl UmlSession {
    pub fn boot(config: &UmlLaunch, factory: ServiceFactory) -> Result<Self, Box<dyn Error>> {
        let profile = uml::Profile::load(&config.profile)?;
        let mut launch = Launch::new(config.work_parent.clone());
        launch.memory_mib = config.memory_mib;
        launch.initramfs = Some(config.initramfs.clone());
        launch.kernel_arguments.clone_from(&config.kernel_arguments);
        launch.bridge = Some(Bridge::new(config.seed));
        launch.wall_limit = Duration::MAX;
        launch.console_limit_bytes = u64::MAX;
        launch.console_tail_bytes = CONSOLE_TAIL;
        let mut session = uml::Session::spawn(&launch, &profile, None)?;
        session.set_progress_limit(Some(config.progress_limit));
        session.branch(
            config.seed,
            handler(&factory, &ServiceConfig::default())?,
            Vec::new(),
        )?;
        let moment = match session.run_to_snapshot_point(config.setup_budget) {
            Ok(Stop::SnapshotPoint(moment)) => moment,
            Ok(stop) => {
                return Err(SessionError::Control(format!(
                    "the guest stopped at {stop:?} before the setup snapshot point\n{}",
                    session.console_tail()
                ))
                .into());
            }
            Err(error) => {
                return Err(SessionError::Control(format!(
                    "setup: {error}\n{}",
                    session.console_tail()
                ))
                .into());
            }
        };
        let mut this = Self {
            session: Some(session),
            profile,
            launch,
            seed: config.seed,
            progress_limit: config.progress_limit,
            factory,
            snapshots: BTreeMap::new(),
            next: 1,
            setup: (SnapId(0), moment),
            abandoned: false,
            counters: Counters::default(),
        };
        let (setup, at) = this.capture()?;
        this.setup = (setup, at);
        Ok(this)
    }

    fn live(&mut self) -> Result<&mut uml::Session, Box<dyn Error>> {
        if self.abandoned {
            return Err(SessionError::Abandoned.into());
        }
        self.session
            .as_mut()
            .ok_or_else(|| SessionError::Control("the guest is not running".into()).into())
    }

    fn capture(&mut self) -> Result<(SnapId, u64), Box<dyn Error>> {
        let begun = started();
        let checkpoint = self.live()?.capture(Capture::default())?;
        self.counters.captures.since(begun);
        let id = SnapId(self.next);
        self.next += 1;
        let at = checkpoint.moment();
        self.snapshots.insert(id, checkpoint);
        Ok((id, at))
    }

    fn restore(&mut self, snapshot: SnapId) -> Result<(), Box<dyn Error>> {
        if self.abandoned {
            return Err(SessionError::Abandoned.into());
        }
        let checkpoint = self
            .snapshots
            .get(&snapshot)
            .ok_or_else(|| SessionError::Control(format!("unknown snapshot {snapshot:?}")))?;
        let begun = started();
        if let Some(session) = self.session.as_mut().filter(|session| session.paused()) {
            session.restore(checkpoint)?;
            self.counters.restores.since(begun);
            return Ok(());
        }
        self.session = None;
        let mut session = uml::Session::spawn(&self.launch, &self.profile, Some(checkpoint))?;
        session.set_progress_limit(Some(self.progress_limit));
        self.session = Some(session);
        self.counters.fresh_restores.since(begun);
        Ok(())
    }

    fn crash(&mut self, moment: u64, reason: Option<ExitReason>) -> StopReason {
        let detail = self
            .session
            .as_ref()
            .map(uml::Session::console_tail)
            .unwrap_or_default();
        let kind = if detail.contains("Kernel panic") {
            CrashKind::Panic
        } else {
            CrashKind::Shutdown
        };
        StopReason::Crash {
            vtime: Moment(moment),
            info: CrashInfo {
                kind,
                detail: format!("{reason:?}\n{detail}").into_bytes(),
            },
        }
    }

    fn abandon(&mut self, error: uml::SessionError) -> Box<dyn Error> {
        self.abandoned = true;
        let tail = self
            .session
            .take()
            .map(|session| session.console_tail())
            .unwrap_or_default();
        match error {
            uml::SessionError::Hung(limit) => SessionError::Hung(limit).into(),
            error => SessionError::Control(format!("{error}\n{tail}")).into(),
        }
    }
}

fn handler(
    factory: &ServiceFactory,
    config: &ServiceConfig,
) -> Result<Box<dyn environment::channel::ServiceHandler>, Box<dyn Error>> {
    let handler = factory(config)?;
    if handler.identity() != config.identity || handler.configuration() != config.configuration {
        return Err(SessionError::Control(
            "the service factory built a handler for another configuration".into(),
        )
        .into());
    }
    Ok(handler)
}

impl SearchSession for UmlSession {
    fn setup_handle(&self) -> (SnapId, u64) {
        self.setup
    }

    fn state_hash(&mut self) -> Result<[u8; 32], Box<dyn Error>> {
        Ok(self.live()?.state_hash()?)
    }

    fn console_tail(&mut self) -> Result<Vec<u8>, Box<dyn Error>> {
        Ok(self
            .session
            .as_ref()
            .map(|session| session.console_tail().into_bytes())
            .unwrap_or_default())
    }

    fn telemetry_counters(&self) -> Vec<(String, u64)> {
        let c = &self.counters;
        let mut out = Vec::new();
        for (name, spent) in [
            ("uml.capture", c.captures),
            ("uml.restore", c.restores),
            ("uml.fresh_restore", c.fresh_restores),
            ("uml.import", c.imports),
        ] {
            out.push((format!("{name}.count"), spent.count));
            out.push((format!("{name}.ns"), spent.nanos));
        }
        out
    }

    fn snapshot_owned_pages(&self, snapshot: SnapId) -> Option<u64> {
        self.snapshots
            .get(&snapshot)
            .and_then(|checkpoint| checkpoint.allocated_bytes().ok())
            .map(|bytes| bytes.div_ceil(4096))
    }

    fn store_bytes(&self) -> Option<u64> {
        Some(
            self.snapshots
                .values()
                .map(|checkpoint| checkpoint.allocated_bytes().unwrap_or(checkpoint.bytes()))
                .sum(),
        )
    }

    fn publish_snapshot(
        &self,
        index: &dyn CacheIndex,
        namespace: Namespace,
        key: &[u8],
        _parent: Option<(SnapId, &Lease)>,
        target: SnapId,
        cost: u64,
    ) -> Result<Lease, Box<dyn Error>> {
        let checkpoint = self
            .snapshots
            .get(&target)
            .ok_or_else(|| SessionError::Control(format!("unknown snapshot {target:?}")))?;
        let export = checkpoint.export()?;
        let mut extent = index.extent(export.len())?;
        export.write(&mut extent.bytes_mut()[..export.len()])?;
        Ok(index.publish(namespace, key, None, extent, cost)?)
    }

    fn import_cached(
        &mut self,
        index: &dyn CacheIndex,
        lease: &Lease,
        _near: SnapId,
    ) -> Result<(SnapId, u64), Box<dyn Error>> {
        let begun = started();
        let chain = index.chain(lease)?;
        let [extent] = chain.as_slice() else {
            return Err(SessionError::Portable(format!(
                "a User-mode Linux checkpoint is one extent, the lease names {}",
                chain.len()
            ))
            .into());
        };
        let checkpoint = Checkpoint::import(extent.bytes(), &self.factory)?;
        let id = SnapId(self.next);
        self.next += 1;
        let at = checkpoint.moment();
        self.snapshots.insert(id, checkpoint);
        self.counters.imports.since(begun);
        Ok((id, at))
    }

    fn replay_snapshot(&mut self, snapshot: SnapId) -> Result<(), Box<dyn Error>> {
        self.restore(snapshot)
    }

    fn drop_snapshot(&mut self, snapshot: SnapId) -> Result<(), Box<dyn Error>> {
        self.snapshots.remove(&snapshot);
        Ok(())
    }

    fn branch_with_service(
        &mut self,
        snapshot: SnapId,
        config: ServiceConfig,
        payloads: Vec<Vec<u8>>,
        effects: Vec<(u64, Effect)>,
    ) -> Result<(), Box<dyn Error>> {
        if !effects.is_empty() {
            return Err(SessionError::Control(
                "User-mode Linux cannot apply host memory writes or interrupts".into(),
            )
            .into());
        }
        let handler = handler(&self.factory, &config)?;
        self.restore(snapshot)?;
        let seed = self.seed;
        self.live()?.branch(seed, handler, payloads)?;
        Ok(())
    }

    fn run_until(&mut self, deadline: u64) -> Result<StopReason, Box<dyn Error>> {
        let result = self.live()?.run_until(deadline);
        let latest = self.session.as_ref().map_or(0, uml::Session::moment);
        match result {
            Ok(Stop::Deadline(moment)) => Ok(StopReason::Deadline {
                vtime: Moment(moment),
            }),
            Ok(Stop::Violation { moment, id, data }) => Ok(StopReason::Assertion {
                vtime: Moment(moment),
                ev: EventRef { id, data },
            }),
            Ok(Stop::Exhausted(moment)) => Ok(StopReason::Quiescent {
                vtime: Moment(moment),
            }),
            Ok(Stop::Stopped(reason @ (ExitReason::Exited(_) | ExitReason::Signaled(_)))) => {
                Ok(self.crash(latest, Some(reason)))
            }
            Ok(Stop::Closed) => Ok(self.crash(latest, None)),
            Ok(stop) => Err(self.abandon(uml::SessionError::Protocol(format!(
                "the guest stopped at {stop:?}"
            )))),
            Err(error) => Err(self.abandon(error)),
        }
    }

    fn snapshot(&mut self) -> Result<(SnapId, u64), Box<dyn Error>> {
        self.capture()
    }

    fn sdk_events(&mut self) -> Result<Vec<SdkEvent>, Box<dyn Error>> {
        Ok(self
            .session
            .as_ref()
            .map(|session| {
                session
                    .events()
                    .iter()
                    .map(|event| (event.moment, event.id, event.data.clone()))
                    .collect()
            })
            .unwrap_or_default())
    }

    fn abandoned(&self) -> bool {
        self.abandoned
    }
}
