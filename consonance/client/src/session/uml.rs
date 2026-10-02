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
use sha2::{Digest, Sha256};
use uml::{Bridge, Capture, Checkpoint, Checkpoints, ExitReason, Launch, Stop, VerifiedProfile};

use super::{SdkEvent, SearchSession, SessionError, UmlLaunch};
use crate::cache::extent::{extent_len, read_extent, resolve, write_extent};
use crate::cache::{ANCHOR_DEPTH, CacheIndex, CommittedExtent, Lease, Namespace};

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
    checkpoints: Checkpoints,
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
        let checkpoints = Checkpoints::new();
        let mut session = uml::Session::spawn(&launch, &profile, &checkpoints, None)?;
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
            checkpoints,
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
        let mut session = uml::Session::spawn(
            &self.launch,
            &self.profile,
            &self.checkpoints,
            Some(checkpoint),
        )?;
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

    fn cache_identity(&mut self) -> Result<[u8; 32], Box<dyn Error>> {
        let state = self.live()?.state_hash()?;
        let setup = self
            .snapshots
            .get(&self.setup.0)
            .ok_or_else(|| SessionError::Control("the setup snapshot is gone".into()))?
            .digest()?;
        let mut digest = Sha256::new();
        digest.update(b"harmony-uml-cache-identity-v1\0");
        digest.update(setup);
        digest.update(state);
        Ok(digest.finalize().into())
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
        self.snapshots.get(&snapshot).map(Checkpoint::owned_pages)
    }

    fn store_bytes(&self) -> Option<u64> {
        self.checkpoints.stats().map(|stats| stats.bytes_resident)
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
        let parent = parent.filter(|(_, lease)| lease.depth() + 1 < ANCHOR_DEPTH);
        let checkpoint = |id: SnapId| -> Result<&Checkpoint, Box<dyn Error>> {
            self.snapshots
                .get(&id)
                .ok_or_else(|| SessionError::Control(format!("unknown snapshot {id:?}")).into())
        };
        let setup = checkpoint(self.setup.0)?;
        let parent_checkpoint = parent.map(|(id, _)| checkpoint(id)).transpose()?;
        let target_checkpoint = checkpoint(target)?;
        let sidecar = target_checkpoint.sidecar()?;
        let extent = self.checkpoints.delta(
            setup,
            parent_checkpoint,
            target_checkpoint,
            |pages, reverted| -> Result<_, Box<dyn Error>> {
                let len =
                    extent_len(pages.len(), reverted.len(), sidecar.len()).ok_or_else(|| {
                        SessionError::Portable("snapshot delta size overflows".into())
                    })?;
                let mut extent = index.extent(len)?;
                write_extent(extent.bytes_mut(), pages, reverted, &sidecar)?;
                Ok(extent)
            },
        )??;
        Ok(index.publish(namespace, key, parent.map(|(_, lease)| lease), extent, cost)?)
    }

    fn import_cached(
        &mut self,
        index: &dyn CacheIndex,
        lease: &Lease,
        near: SnapId,
    ) -> Result<(SnapId, u64), Box<dyn Error>> {
        let begun = started();
        let chain = index.chain(lease)?;
        let deltas = chain
            .iter()
            .map(|extent| read_extent(CommittedExtent::bytes(extent)))
            .collect::<Result<Vec<_>, _>>()?;
        let resolved = resolve(&deltas)?;
        let setup = self
            .snapshots
            .get(&self.setup.0)
            .ok_or_else(|| SessionError::Control("the setup snapshot is gone".into()))?;
        let near = self.snapshots.get(&near).unwrap_or(setup);
        let checkpoint = self.checkpoints.import(
            setup,
            near,
            &resolved.pages,
            resolved.sidecar,
            &self.factory,
        )?;
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
                    .map(|event| (event.moment, event.id, event.data.to_vec()))
                    .collect()
            })
            .unwrap_or_default())
    }

    fn abandoned(&self) -> bool {
        self.abandoned
    }
}
