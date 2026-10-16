// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::BTreeMap,
    error::Error,
    time::{Duration, Instant},
};

use control_proto::{
    CrashInfo, CrashKind, DecisionId, EventRef, Moment, Resolution, SnapId, StopReason,
};
use environment::{
    channel::{CoverageSchedule, Effect, HandlerFingerprint},
    input_spec::{ServiceConfig, ServiceFactory},
};
use sha2::{Digest, Sha256};
use uml::{Bridge, Capture, Checkpoint, Checkpoints, Launch, Stop, VerifiedProfile};

use super::{SdkEvent, SearchSession, SessionError, UmlLaunch};
use crate::cache::extent::{extent_len, read_extent, resolve, write_extent};
use crate::cache::{ANCHOR_DEPTH, CacheIndex, CommittedExtent, Lease, Namespace};

const CONSOLE_TAIL: usize = 64 << 10;
const CACHE_SIDECAR_MAGIC: [u8; 8] = *b"HUMLCA01";

fn cache_sidecar(identity: [u8; 32], raw: &[u8]) -> Result<Vec<u8>, Box<dyn Error>> {
    let len = CACHE_SIDECAR_MAGIC
        .len()
        .checked_add(identity.len())
        .and_then(|len| len.checked_add(raw.len()))
        .ok_or_else(|| SessionError::Portable("snapshot delta size overflows".into()))?;
    let mut sidecar = Vec::with_capacity(len);
    sidecar.extend_from_slice(&CACHE_SIDECAR_MAGIC);
    sidecar.extend_from_slice(&identity);
    sidecar.extend_from_slice(raw);
    Ok(sidecar)
}

fn target_commitment(identity: [u8; 32], target: &Checkpoint) -> Result<[u8; 32], Box<dyn Error>> {
    let mut digest = Sha256::new();
    digest.update(b"consonance-cache-uml-target-v1\0");
    digest.update(CACHE_SIDECAR_MAGIC);
    digest.update(identity);
    digest.update(target.digest()?);
    Ok(digest.finalize().into())
}

fn checked_cache_sidecar(identity: [u8; 32], sidecar: &[u8]) -> Result<&[u8], Box<dyn Error>> {
    if sidecar.len() < CACHE_SIDECAR_MAGIC.len() + identity.len()
        || sidecar[..CACHE_SIDECAR_MAGIC.len()] != CACHE_SIDECAR_MAGIC
    {
        return Err(SessionError::Portable("cached snapshot sidecar differs".into()).into());
    }
    let identity_start = CACHE_SIDECAR_MAGIC.len();
    let identity_end = identity_start + identity.len();
    if sidecar[identity_start..identity_end] != identity {
        return Err(SessionError::Portable("cached snapshot identity differs".into()).into());
    }
    Ok(&sidecar[identity_end..])
}

fn root_identity(
    config: &UmlLaunch,
    profile_identity: &str,
    host: &uml::HostIdentity,
    initramfs: &[u8],
    loaded_executable: &str,
) -> [u8; 32] {
    let mut identity = Sha256::new();
    identity.update(b"harmony-uml-root-v6\0");
    identity.update((loaded_executable.len() as u64).to_le_bytes());
    identity.update(loaded_executable.as_bytes());
    identity.update(profile_identity.as_bytes());
    identity.update(host.identity_sha256().as_bytes());
    identity.update(config.seed.to_le_bytes());
    identity.update(config.memory_mib.to_le_bytes());
    identity.update(config.setup_budget.to_le_bytes());
    identity.update((config.kernel_arguments.len() as u64).to_le_bytes());
    for argument in &config.kernel_arguments {
        identity.update((argument.len() as u64).to_le_bytes());
        identity.update(argument.as_bytes());
    }
    identity.update((initramfs.len() as u64).to_le_bytes());
    identity.update(initramfs);
    identity.finalize().into()
}

fn require_profile_identity(actual: &str, expected: &str) -> Result<(), Box<dyn Error>> {
    if actual != expected {
        return Err(SessionError::Control(
            "the UML profile identity changed after workload admission".into(),
        )
        .into());
    }
    Ok(())
}

fn admitted_initramfs(
    launch: &Launch,
    expected_sha256: [u8; 32],
) -> Result<Vec<u8>, Box<dyn Error>> {
    let initramfs = launch
        .initramfs_bytes()?
        .ok_or_else(|| SessionError::Control("the pinned initramfs is missing".into()))?;
    let actual_sha256: [u8; 32] = Sha256::digest(&initramfs).into();
    if actual_sha256 != expected_sha256 {
        return Err(SessionError::Control(
            "the pinned UML initramfs changed after workload admission".into(),
        )
        .into());
    }
    Ok(initramfs)
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
struct Counters {
    captures: Spent,
    restores: Spent,
    fresh_restores: Spent,
    imports: Spent,
}

pub struct UmlSession {
    session: Option<uml::Session>,
    loaded_image_guard: crate::image_identity::LoadedImageGuard,
    profile: Option<VerifiedProfile>,
    launch: Launch,
    host: uml::HostIdentity,
    seed: u64,
    root_identity: [u8; 32],
    cache_domain: [u8; 32],
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
        require_profile_identity(
            profile.identity_sha256(),
            &config.expected_profile_identity_sha256,
        )?;
        let mut launch = Launch::new(config.work_parent.clone());
        launch.memory_mib = config.memory_mib;
        launch.set_initramfs(&config.initramfs)?;
        launch.kernel_arguments.clone_from(&config.kernel_arguments);
        launch.bridge = Some(Bridge::new(config.seed));
        launch.wall_limit = Duration::MAX;
        launch.console_limit_bytes = u64::MAX;
        launch.console_tail_bytes = CONSOLE_TAIL;
        launch.bind_expected_host(config.expected_host.clone())?;
        let host = launch.host_identity()?;
        let initramfs = admitted_initramfs(&launch, config.expected_initramfs_sha256)?;
        let (loaded_executable, loaded_image_guard) =
            crate::image_identity::LoadedImageGuard::identity()?;
        let root_identity = root_identity(
            config,
            profile.identity_sha256(),
            &host,
            &initramfs,
            &loaded_executable,
        );
        let checkpoints = Checkpoints::new();
        let mut session = uml::Session::spawn(
            &launch,
            &profile,
            &checkpoints,
            None,
            Some(config.progress_limit),
        )?;
        session.branch(
            config.seed,
            handler(&factory, &ServiceConfig::default())?,
            Vec::new(),
            None,
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
            loaded_image_guard,
            profile: Some(profile),
            launch,
            host,
            seed: config.seed,
            root_identity,
            cache_domain: [0; 32],
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
        let mut cache_domain = Sha256::new();
        cache_domain.update(b"harmony-uml-cache-session-v1\0");
        cache_domain.update(this.root_identity);
        let setup_checkpoint = this
            .snapshots
            .get(&setup)
            .ok_or_else(|| SessionError::Control("the setup snapshot is gone".into()))?;
        cache_domain.update(setup_checkpoint.digest()?);
        this.cache_domain = cache_domain.finalize().into();
        this.loaded_image_guard.check()?;
        Ok(this)
    }

    fn live(&mut self) -> Result<&mut uml::Session, Box<dyn Error>> {
        self.loaded_image_guard.check()?;
        if self.abandoned {
            return Err(SessionError::Abandoned.into());
        }
        self.session
            .as_mut()
            .ok_or_else(|| SessionError::Control("the guest is not running".into()).into())
    }

    fn live_service_fingerprint(&mut self) -> Result<HandlerFingerprint, Box<dyn Error>> {
        let result = self.live()?.service_handler_fingerprint();
        result.map_err(|error| self.abandon(error))
    }

    fn import_service_fingerprint(&mut self) -> Result<Option<HandlerFingerprint>, Box<dyn Error>> {
        if self.abandoned {
            return Err(SessionError::Abandoned.into());
        }
        if self.session.is_some() {
            self.live_service_fingerprint().map(Some)
        } else {
            Ok(None)
        }
    }

    fn verify_live_service_fingerprint(
        &mut self,
        before: Option<&HandlerFingerprint>,
    ) -> Result<(), Box<dyn Error>> {
        if let Some(before) = before {
            let result = self.live()?.verify_service_handler_unchanged(before);
            result.map_err(|error| self.abandon(error))?;
        }
        Ok(())
    }

    fn capture(&mut self) -> Result<(SnapId, u64), Box<dyn Error>> {
        self.loaded_image_guard.check()?;
        let begun = started();
        let checkpoint = self
            .live()?
            .capture(Capture::default())
            .map_err(|error| self.abandon(error))?;
        self.counters.captures.since(begun);
        let id = SnapId(self.next);
        self.next += 1;
        let at = checkpoint.moment();
        self.snapshots.insert(id, checkpoint);
        self.loaded_image_guard.check()?;
        Ok((id, at))
    }

    fn restore(&mut self, snapshot: SnapId) -> Result<(), Box<dyn Error>> {
        self.loaded_image_guard.check()?;
        if self.abandoned {
            return Err(SessionError::Abandoned.into());
        }
        if self
            .session
            .as_ref()
            .is_none_or(|session| !session.paused())
            && self.profile.is_none()
        {
            return Err(
                SessionError::Control("the UML session has no admitted profile".into()).into(),
            );
        }
        let checkpoint = self
            .snapshots
            .get(&snapshot)
            .ok_or_else(|| SessionError::Control(format!("unknown snapshot {snapshot:?}")))?;
        if self.launch.host_identity()? != self.host {
            return Err(SessionError::Control(
                "the UML launch host identity changed before restore".into(),
            )
            .into());
        }
        let begun = started();
        if let Some(session) = self.session.as_mut().filter(|session| session.paused()) {
            session
                .restore(checkpoint)
                .map_err(|error| self.abandon(error))?;
            self.counters.restores.since(begun);
            self.loaded_image_guard.check()?;
            return Ok(());
        }
        self.session = None;
        let profile = self.profile.as_ref().ok_or_else(|| {
            SessionError::Control("the UML session has no admitted profile".into())
        })?;
        let session = uml::Session::spawn(
            &self.launch,
            profile,
            &self.checkpoints,
            Some(checkpoint),
            Some(self.progress_limit),
        )
        .map_err(|error| self.abandon(error))?;
        self.session = Some(session);
        self.counters.fresh_restores.since(begun);
        self.loaded_image_guard.check()?;
        Ok(())
    }

    fn crash(&mut self, moment: u64) -> Result<StopReason, Box<dyn Error>> {
        let limit = self.progress_limit;
        let terminal_reason = match self.live()?.settle_terminal_console(limit) {
            Ok(reason) => reason,
            Err(error) => return Err(self.abandon(error)),
        };
        let detail = self
            .session
            .as_ref()
            .map(uml::Session::console_tail)
            .unwrap_or_default();
        let kind = if terminal_reason == uml::ExitReason::GuestPanic {
            CrashKind::Panic
        } else {
            CrashKind::Shutdown
        };
        Ok(StopReason::Crash {
            vtime: Moment(moment),
            info: CrashInfo {
                kind,
                detail: format!("{terminal_reason:?}\n{detail}").into_bytes(),
            },
        })
    }

    fn finish_run(
        &mut self,
        result: Result<Stop, uml::SessionError>,
    ) -> Result<StopReason, Box<dyn Error>> {
        let latest = self.session.as_ref().map_or(0, uml::Session::moment);
        let outcome = match result {
            Ok(Stop::Deadline(moment)) => Ok(StopReason::Deadline {
                vtime: Moment(moment),
            }),
            Ok(Stop::Decision { moment, question }) => {
                let mut ctx = question.service().to_le_bytes().to_vec();
                ctx.extend(question.payload());
                Ok(StopReason::Decision {
                    vtime: Moment(moment),
                    id: DecisionId(question.request_id()),
                    ctx,
                })
            }
            Ok(Stop::Violation { moment, id, data }) => Ok(StopReason::Assertion {
                vtime: Moment(moment),
                ev: EventRef { id, data },
            }),
            Ok(Stop::Exhausted(moment)) => Ok(StopReason::Quiescent {
                vtime: Moment(moment),
            }),
            Ok(Stop::Closed) => self.crash(latest),
            Ok(stop) => Err(self.abandon(uml::SessionError::Protocol(format!(
                "the guest stopped at {stop:?}"
            )))),
            Err(error @ uml::SessionError::BadResolution(_)) => Err(error.into()),
            Err(error) => Err(self.abandon(error)),
        };
        self.loaded_image_guard.check()?;
        outcome
    }

    fn abandon(&mut self, error: uml::SessionError) -> Box<dyn Error> {
        self.abandoned = true;
        let tail = self
            .session
            .take()
            .map(|session| session.console_tail())
            .unwrap_or_default();
        if let uml::SessionError::Boot {
            error: boot_error,
            console,
        } = &error
            && matches!(boot_error.as_ref(), uml::SessionError::Hung(_))
            && !console.is_empty()
        {
            eprintln!("UML restore boot watchdog console:\n{console}");
        }
        let hung = match &error {
            uml::SessionError::Hung(limit) => Some(*limit),
            uml::SessionError::Boot { error, .. } => match error.as_ref() {
                uml::SessionError::Hung(limit) => Some(*limit),
                _ => None,
            },
            _ => None,
        };
        match hung {
            Some(limit) => SessionError::Hung(limit).into(),
            None => SessionError::Control(format!("{error}\n{tail}")).into(),
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
        self.loaded_image_guard.check()?;
        let state = self.live()?.bridge_state_digest()?;
        let setup = self
            .snapshots
            .get(&self.setup.0)
            .ok_or_else(|| SessionError::Control("the setup snapshot is gone".into()))?
            .digest()?;
        let mut digest = Sha256::new();
        digest.update(b"harmony-uml-cache-identity-v2\0");
        digest.update(setup);
        digest.update(state);
        let identity = digest.finalize().into();
        self.loaded_image_guard.check()?;
        Ok(identity)
    }

    fn root_identity(&mut self) -> Result<[u8; 32], Box<dyn Error>> {
        self.loaded_image_guard.check()?;
        Ok(self.root_identity)
    }

    fn publish_root(
        &self,
        index: &dyn CacheIndex,
        namespace: Namespace,
        target: SnapId,
    ) -> Result<Lease, Box<dyn Error>> {
        self.loaded_image_guard.check()?;
        let target = self
            .snapshots
            .get(&target)
            .ok_or_else(|| SessionError::Control("unknown root snapshot".into()))?;
        let target_commitment = target_commitment(self.root_identity, target)?;
        let sidecar = cache_sidecar(self.root_identity, &target.sidecar()?)?;
        let extent = self
            .checkpoints
            .full(target, |pages| -> Result<_, Box<dyn Error>> {
                let len = extent_len(pages.len(), 0, sidecar.len())
                    .ok_or_else(|| SessionError::Portable("root snapshot size overflows".into()))?;
                let mut extent = index.extent(len)?;
                write_extent(extent.bytes_mut(), pages, &[], &sidecar)?;
                Ok(extent)
            })??;
        self.loaded_image_guard.check()?;
        let lease = index.publish(namespace, b"root", None, extent, target_commitment, 1)?;
        Ok(self.loaded_image_guard.check_published(index, lease)?)
    }

    fn import_root(
        &mut self,
        index: &dyn CacheIndex,
        lease: &Lease,
    ) -> Result<(SnapId, u64), Box<dyn Error>> {
        self.loaded_image_guard.check()?;
        let chain = index.chain(lease)?;
        if chain.len() != 1 {
            return Err("root snapshot must have exactly one extent".into());
        }
        let delta = read_extent(chain[0].bytes())?;
        let deltas = [delta];
        let resolved = resolve(&deltas)?;
        let sidecar = checked_cache_sidecar(self.root_identity, resolved.sidecar)?;
        let before = self.import_service_fingerprint()?;
        let near = self
            .snapshots
            .get(&self.setup.0)
            .ok_or_else(|| SessionError::Control("the setup snapshot is gone".into()))?;
        let imported = self
            .checkpoints
            .import_full(near, &resolved.pages, sidecar, &self.factory);
        self.verify_live_service_fingerprint(before.as_ref())?;
        let checkpoint = imported?;
        let id = SnapId(self.next);
        self.next += 1;
        let at = checkpoint.moment();
        self.snapshots.insert(id, checkpoint);
        self.loaded_image_guard.check()?;
        Ok((id, at))
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
        self.loaded_image_guard.check()?;
        let parent = parent.filter(|(_, lease)| lease.depth() + 1 < ANCHOR_DEPTH);
        let checkpoint = |id: SnapId| -> Result<&Checkpoint, Box<dyn Error>> {
            self.snapshots
                .get(&id)
                .ok_or_else(|| SessionError::Control(format!("unknown snapshot {id:?}")).into())
        };
        let setup = checkpoint(self.setup.0)?;
        let parent_checkpoint = parent.map(|(id, _)| checkpoint(id)).transpose()?;
        let target_checkpoint = checkpoint(target)?;
        let target_commitment = target_commitment(self.cache_domain, target_checkpoint)?;
        let sidecar = cache_sidecar(self.cache_domain, &target_checkpoint.sidecar()?)?;
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
        self.loaded_image_guard.check()?;
        let lease = index.publish(
            namespace,
            key,
            parent.map(|(_, lease)| lease),
            extent,
            target_commitment,
            cost,
        )?;
        Ok(self.loaded_image_guard.check_published(index, lease)?)
    }

    fn import_cached(
        &mut self,
        index: &dyn CacheIndex,
        lease: &Lease,
        near: SnapId,
    ) -> Result<(SnapId, u64), Box<dyn Error>> {
        self.loaded_image_guard.check()?;
        let begun = started();
        let chain = index.chain(lease)?;
        let mut deltas = chain
            .iter()
            .map(|extent| read_extent(CommittedExtent::bytes(extent)))
            .collect::<Result<Vec<_>, _>>()?;
        for delta in &mut deltas {
            delta.sidecar = checked_cache_sidecar(self.cache_domain, delta.sidecar)?;
        }
        let resolved = resolve(&deltas)?;
        let before = self.import_service_fingerprint()?;
        let setup = self
            .snapshots
            .get(&self.setup.0)
            .ok_or_else(|| SessionError::Control("the setup snapshot is gone".into()))?;
        let near = self.snapshots.get(&near).unwrap_or(setup);
        let imported = self.checkpoints.import(
            setup,
            near,
            &resolved.pages,
            resolved.sidecar,
            &self.factory,
        );
        self.verify_live_service_fingerprint(before.as_ref())?;
        let checkpoint = imported?;
        let id = SnapId(self.next);
        self.next += 1;
        let at = checkpoint.moment();
        self.snapshots.insert(id, checkpoint);
        self.counters.imports.since(begun);
        self.loaded_image_guard.check()?;
        Ok((id, at))
    }

    fn replay_snapshot(&mut self, snapshot: SnapId) -> Result<(), Box<dyn Error>> {
        self.restore(snapshot)
    }

    fn drop_snapshot(&mut self, snapshot: SnapId) -> Result<(), Box<dyn Error>> {
        self.loaded_image_guard.check()?;
        self.snapshots.remove(&snapshot);
        Ok(())
    }

    fn branch_with_service(
        &mut self,
        snapshot: SnapId,
        config: ServiceConfig,
        payloads: Vec<Vec<u8>>,
        effects: Vec<(u64, Effect)>,
        coverage_schedule: Option<CoverageSchedule>,
    ) -> Result<(), Box<dyn Error>> {
        self.loaded_image_guard.check()?;
        if !effects.is_empty() {
            return Err(SessionError::Control(
                "User-mode Linux cannot apply host memory writes or interrupts".into(),
            )
            .into());
        }
        let before = self.live_service_fingerprint()?;
        let prepared = handler(&self.factory, &config);
        self.verify_live_service_fingerprint(Some(&before))?;
        let handler = prepared?;
        self.restore(snapshot)?;
        let seed = self.seed;
        self.live()?
            .branch(seed, handler, payloads, coverage_schedule)
            .map_err(|error| self.abandon(error))?;
        self.loaded_image_guard.check()?;
        Ok(())
    }

    fn run_until(&mut self, deadline: u64) -> Result<StopReason, Box<dyn Error>> {
        let result = self.live()?.run_until(deadline);
        self.finish_run(result)
    }

    fn run_resolving(
        &mut self,
        deadline: u64,
        resolution: Resolution,
    ) -> Result<StopReason, Box<dyn Error>> {
        let result = self.live()?.resolve_and_run(
            deadline,
            resolution.vtime.0,
            resolution.service,
            resolution.id.0,
            &resolution.answer.0,
        );
        self.finish_run(result)
    }

    fn snapshot(&mut self) -> Result<(SnapId, u64), Box<dyn Error>> {
        self.capture()
    }

    fn sdk_events(&mut self) -> Result<Vec<SdkEvent>, Box<dyn Error>> {
        self.loaded_image_guard.check()?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::LocalIndex;
    use environment::input_spec::nominal_factory;
    use std::path::PathBuf;

    fn host() -> uml::HostIdentity {
        uml::HostIdentity {
            architecture: "x86_64".into(),
            processor: 0,
            cpu_model: "model".into(),
            cpu_features: "features".into(),
            boot_cpuinfo: "flags: features\n".into(),
            auxv: uml::HostAuxv::default(),
        }
    }

    fn unbooted_session() -> UmlSession {
        UmlSession {
            session: None,
            loaded_image_guard: crate::image_identity::LoadedImageGuard::capture().unwrap(),
            profile: None,
            launch: Launch::new(PathBuf::new()),
            host: host(),
            seed: 7,
            root_identity: [7; 32],
            cache_domain: [9; 32],
            progress_limit: Duration::ZERO,
            factory: nominal_factory(),
            checkpoints: Checkpoints::new(),
            snapshots: BTreeMap::new(),
            next: 1,
            setup: (SnapId(1), 0),
            abandoned: false,
            counters: Counters::default(),
        }
    }

    #[cfg(target_os = "linux")]
    fn terminal_session(script: &str, parent: &tempfile::TempDir) -> UmlSession {
        use std::os::unix::process::CommandExt;
        use std::process::Command;

        let mut launch = Launch::new(parent.path().to_path_buf());
        launch.bridge = Some(Bridge::new(7));
        launch.wall_limit = Duration::from_secs(5);
        let work = launch.work_directory().unwrap();
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]).process_group(0);
        let checkpoints = Checkpoints::new();
        let guest = uml::Session::start(
            command,
            work,
            &launch,
            &checkpoints,
            None,
            Some(Duration::from_secs(5)),
        )
        .unwrap();
        let mut session = unbooted_session();
        session.session = Some(guest);
        session.checkpoints = checkpoints;
        session.progress_limit = Duration::from_secs(5);
        session
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn host_signal_abandons_instead_of_reporting_guest_shutdown() {
        let parent = tempfile::tempdir().unwrap();
        let mut session = terminal_session("kill -KILL $$", &parent);
        let error = session.run_until(1).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<SessionError>(),
            Some(SessionError::Control(message)) if message.contains("Signaled(9)")
        ));
        assert!(session.abandoned());
        assert!(matches!(
            session
                .run_until(1)
                .unwrap_err()
                .downcast_ref::<SessionError>(),
            Some(SessionError::Abandoned)
        ));
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn host_clone_status_abandons_instead_of_reporting_guest_shutdown() {
        let parent = tempfile::tempdir().unwrap();
        let mut session = terminal_session("exit 87", &parent);
        let error = session.run_until(1).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<SessionError>(),
            Some(SessionError::Control(message)) if message.contains("HostFailure")
        ));
        assert!(session.abandoned());
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn clean_guest_exit_remains_a_terminal_shutdown() {
        let parent = tempfile::tempdir().unwrap();
        let mut session = terminal_session("exit 0", &parent);
        let stop = session.run_until(1).unwrap();
        let StopReason::Crash { info, .. } = stop else {
            panic!("expected a terminal guest stop");
        };
        assert_eq!(info.kind, CrashKind::Shutdown);
        assert!(String::from_utf8_lossy(&info.detail).contains("Exited(0)"));
        assert!(!session.abandoned());
    }

    #[test]
    fn fresh_restore_timeout_abandons_with_a_typed_watchdog_error() {
        let limit = Duration::from_millis(25);
        let mut session = unbooted_session();
        let error = session.abandon(uml::SessionError::Boot {
            error: Box::new(uml::SessionError::Hung(limit)),
            console: "restore wait".into(),
        });
        assert!(matches!(
            error.downcast_ref::<SessionError>(),
            Some(SessionError::Hung(got)) if *got == limit
        ));
        assert!(session.abandoned);
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn capture_timeout_abandons_with_a_typed_watchdog_error() {
        use std::io::{Read, Write};
        use std::os::fd::FromRawFd;
        use std::os::unix::process::CommandExt;
        use std::process::Command;

        if std::env::var_os("HARMONY_UML_CAPTURE_CHILD").is_some() {
            // SAFETY: the UML launcher passes the owned bridge socket at fd 3.
            let mut socket = unsafe { std::fs::File::from_raw_fd(3) };
            socket.write_all(&100_u64.to_le_bytes()).unwrap();
            let mut control = [0_u8; 16];
            socket.read_exact(&mut control).unwrap();
            assert_eq!(u32::from_le_bytes(control[4..8].try_into().unwrap()), 1);
            std::thread::sleep(Duration::from_secs(30));
            return;
        }

        let directory = tempfile::tempdir().unwrap();
        let mut launch = Launch::new(directory.path().to_path_buf());
        launch.bridge = Some(Bridge::new(7));
        launch.wall_limit = Duration::MAX;
        let work = launch.work_directory().unwrap();
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("exec \"$0\" --exact session::uml::tests::capture_timeout_abandons_with_a_typed_watchdog_error --nocapture")
            .arg(std::env::current_exe().unwrap())
            .env("HARMONY_UML_CAPTURE_CHILD", "1")
            .process_group(0);
        let checkpoints = Checkpoints::new();
        let limit = Duration::from_secs(2);
        let mut guest =
            uml::Session::start(command, work, &launch, &checkpoints, None, Some(limit)).unwrap();
        assert!(matches!(guest.run_until(100), Ok(Stop::Deadline(100))));
        let mut session = unbooted_session();
        session.session = Some(guest);
        session.checkpoints = checkpoints;
        let error = session.snapshot().unwrap_err();
        assert!(matches!(
            error.downcast_ref::<SessionError>(),
            Some(SessionError::Hung(got)) if *got == limit
        ));
        assert!(session.abandoned());
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn paused_restore_timeout_abandons_with_a_typed_watchdog_error() {
        use std::io::{Read, Write};
        use std::os::fd::FromRawFd;
        use std::os::unix::process::CommandExt;
        use std::process::Command;

        if std::env::var_os("HARMONY_UML_RESTORE_CHILD").is_some() {
            // SAFETY: the UML launcher passes distinct owned bridge and memory descriptors.
            let (mut socket, memory) =
                unsafe { (std::fs::File::from_raw_fd(3), std::fs::File::from_raw_fd(4)) };
            memory.set_len(4 * 4096).unwrap();
            socket.write_all(&100_u64.to_le_bytes()).unwrap();
            let mut control = [0_u8; 16];
            socket.read_exact(&mut control).unwrap();
            assert_eq!(u32::from_le_bytes(control[4..8].try_into().unwrap()), 1);
            let ack = |command: u32, result: i64| {
                let mut frame = [0_u8; 16];
                frame[..4].copy_from_slice(&0x3143_5548_u32.to_le_bytes());
                frame[4..8].copy_from_slice(&command.to_le_bytes());
                frame[8..].copy_from_slice(&result.to_le_bytes());
                frame
            };
            socket.write_all(&ack(4, 1)).unwrap();
            assert_eq!(socket.read(&mut control).unwrap(), 16);
            assert_eq!(u32::from_le_bytes(control[4..8].try_into().unwrap()), 5);
            socket.write_all(&ack(1, 0)).unwrap();
            assert_eq!(socket.read(&mut control).unwrap(), 16);
            assert_eq!(u32::from_le_bytes(control[4..8].try_into().unwrap()), 2);
            std::thread::sleep(Duration::from_secs(30));
            return;
        }

        let directory = tempfile::tempdir().unwrap();
        let mut launch = Launch::new(directory.path().to_path_buf());
        launch.bridge = Some(Bridge::new(7));
        launch.wall_limit = Duration::MAX;
        let work = launch.work_directory().unwrap();
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("exec \"$0\" --exact session::uml::tests::paused_restore_timeout_abandons_with_a_typed_watchdog_error --nocapture")
            .arg(std::env::current_exe().unwrap())
            .env("HARMONY_UML_RESTORE_CHILD", "1")
            .process_group(0);
        let checkpoints = Checkpoints::new();
        let limit = Duration::from_secs(2);
        let mut guest =
            uml::Session::start(command, work, &launch, &checkpoints, None, Some(limit)).unwrap();
        assert!(matches!(guest.run_until(100), Ok(Stop::Deadline(100))));
        let checkpoint = guest.capture(Capture::default()).unwrap();
        assert!(guest.paused());

        let mut session = unbooted_session();
        session.host = launch.host_identity().unwrap();
        session.launch = launch;
        session.checkpoints = checkpoints;
        session.session = Some(guest);
        session.snapshots.insert(SnapId(1), checkpoint);
        let error = session.restore(SnapId(1)).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<SessionError>(),
            Some(SessionError::Hung(got)) if *got == limit
        ));
        assert!(session.abandoned());
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn rejected_service_branch_cannot_leave_a_usable_rewound_uml_session() {
        use std::io::{Read, Write};
        use std::os::fd::FromRawFd;
        use std::os::unix::process::CommandExt;
        use std::process::Command;

        if std::env::var_os("HARMONY_UML_FAILED_BRANCH_CHILD").is_some() {
            // SAFETY: the UML launcher passes distinct owned bridge and memory descriptors.
            let (mut socket, memory) =
                unsafe { (std::fs::File::from_raw_fd(3), std::fs::File::from_raw_fd(4)) };
            memory.set_len(4 * 4096).unwrap();
            socket.write_all(&100_u64.to_le_bytes()).unwrap();
            let ack = |command: u32, result: i64| {
                let mut frame = [0_u8; 16];
                frame[..4].copy_from_slice(&0x3143_5548_u32.to_le_bytes());
                frame[4..8].copy_from_slice(&command.to_le_bytes());
                frame[8..].copy_from_slice(&result.to_le_bytes());
                frame
            };
            let mut control = [0_u8; 16];
            assert_eq!(socket.read(&mut control).unwrap(), 16);
            assert_eq!(u32::from_le_bytes(control[4..8].try_into().unwrap()), 1);
            socket.write_all(&ack(4, 1)).unwrap();
            assert_eq!(socket.read(&mut control).unwrap(), 16);
            assert_eq!(u32::from_le_bytes(control[4..8].try_into().unwrap()), 5);
            socket.write_all(&ack(1, 0)).unwrap();
            for _ in 0..2 {
                socket.read_exact(&mut control).unwrap();
                assert_eq!(u32::from_le_bytes(control[4..8].try_into().unwrap()), 2);
                socket.write_all(&ack(4, 1)).unwrap();
                assert_eq!(socket.read(&mut control).unwrap(), 16);
                assert_eq!(u32::from_le_bytes(control[4..8].try_into().unwrap()), 5);
                socket.write_all(&ack(2, 0)).unwrap();
            }
            std::thread::sleep(Duration::from_secs(30));
            return;
        }

        let directory = tempfile::tempdir().unwrap();
        let mut launch = Launch::new(directory.path().to_path_buf());
        launch.bridge = Some(Bridge::new(7));
        launch.wall_limit = Duration::MAX;
        let work = launch.work_directory().unwrap();
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("exec \"$0\" --exact session::uml::tests::rejected_service_branch_cannot_leave_a_usable_rewound_uml_session --nocapture")
            .arg(std::env::current_exe().unwrap())
            .env("HARMONY_UML_FAILED_BRANCH_CHILD", "1")
            .process_group(0);
        let checkpoints = Checkpoints::new();
        let mut guest = uml::Session::start(
            command,
            work,
            &launch,
            &checkpoints,
            None,
            Some(Duration::from_secs(2)),
        )
        .unwrap();
        assert!(matches!(guest.run_until(100), Ok(Stop::Deadline(100))));
        let setup = guest.capture(Capture::default()).unwrap();

        let mut session = unbooted_session();
        session.host = launch.host_identity().unwrap();
        session.launch = launch;
        session.checkpoints = checkpoints;
        session.session = Some(guest);
        session.snapshots.insert(SnapId(1), setup);
        session.setup = (SnapId(1), 100);

        let before_setup_branch = session.cache_identity().unwrap();
        session
            .branch_with_service(
                SnapId(1),
                ServiceConfig::default(),
                Vec::new(),
                Vec::new(),
                None,
            )
            .unwrap();
        let before_rejected_branch = session.cache_identity().unwrap();
        assert_ne!(before_rejected_branch, before_setup_branch);

        let error = session
            .branch_with_service(
                SnapId(1),
                ServiceConfig::default(),
                vec![vec![0; environment::channel::MAX_CHANNEL_BYTES + 1]],
                Vec::new(),
                None,
            )
            .unwrap_err();
        assert!(error.to_string().contains("payload tape"), "{error}");
        assert!(session.abandoned());
        assert!(matches!(
            session
                .cache_identity()
                .unwrap_err()
                .downcast_ref::<SessionError>(),
            Some(SessionError::Abandoned)
        ));
        assert!(matches!(
            session
                .run_until(101)
                .unwrap_err()
                .downcast_ref::<SessionError>(),
            Some(SessionError::Abandoned)
        ));
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn service_factory_aliasing_the_live_handler_abandons_the_session() {
        use std::io::{Read, Write};
        use std::os::fd::FromRawFd;
        use std::os::unix::process::CommandExt;
        use std::process::Command;
        use std::sync::{
            Arc,
            atomic::{AtomicU8, Ordering},
        };

        use environment::channel::{
            Answer, ChannelError, Question, ServiceHandler, ServiceResponse,
        };

        #[derive(Clone)]
        struct AliasedHandler(Arc<AtomicU8>);

        impl ServiceHandler for AliasedHandler {
            fn identity(&self) -> &[u8] {
                b"aliased-uml-handler"
            }

            fn configuration(&self) -> &[u8] {
                &[]
            }

            fn respond(&mut self, _: u64, _: &Question) -> Result<ServiceResponse, ChannelError> {
                Ok(ServiceResponse::Answered(Answer::Data(vec![
                    self.0.load(Ordering::SeqCst),
                ])))
            }

            fn snapshot_state(&self) -> Result<Vec<u8>, ChannelError> {
                Ok(vec![self.0.load(Ordering::SeqCst)])
            }

            fn restore_state(&mut self, state: &[u8]) -> Result<(), ChannelError> {
                let [value] = state else {
                    return Err(ChannelError::Malformed);
                };
                self.0.store(*value, Ordering::SeqCst);
                Ok(())
            }

            fn reseed(&mut self, seed: u64) {
                self.0.store(seed as u8, Ordering::SeqCst);
            }

            fn clone_box(&self) -> Box<dyn ServiceHandler> {
                Box::new(self.clone())
            }
        }

        if std::env::var_os("HARMONY_UML_ALIASED_SERVICE_CHILD").is_some() {
            // SAFETY: the UML launcher passes distinct owned bridge and memory descriptors.
            let (mut socket, memory) =
                unsafe { (std::fs::File::from_raw_fd(3), std::fs::File::from_raw_fd(4)) };
            memory.set_len(4 * 4096).unwrap();
            socket.write_all(&100_u64.to_le_bytes()).unwrap();
            let ack = |command: u32, result: i64| {
                let mut frame = [0_u8; 16];
                frame[..4].copy_from_slice(&0x3143_5548_u32.to_le_bytes());
                frame[4..8].copy_from_slice(&command.to_le_bytes());
                frame[8..].copy_from_slice(&result.to_le_bytes());
                frame
            };
            let mut control = [0_u8; 16];
            socket.read_exact(&mut control).unwrap();
            assert_eq!(u32::from_le_bytes(control[4..8].try_into().unwrap()), 1);
            socket.write_all(&ack(4, 1)).unwrap();
            socket.read_exact(&mut control).unwrap();
            assert_eq!(u32::from_le_bytes(control[4..8].try_into().unwrap()), 5);
            socket.write_all(&ack(1, 0)).unwrap();
            std::thread::sleep(Duration::from_secs(30));
            return;
        }

        for operation in ["root", "cached", "branch"] {
            let directory = tempfile::tempdir().unwrap();
            let mut launch = Launch::new(directory.path().to_path_buf());
            launch.bridge = Some(Bridge::new(7));
            launch.wall_limit = Duration::MAX;
            let work = launch.work_directory().unwrap();
            let mut command = Command::new("/bin/sh");
            command
                .arg("-c")
                .arg("exec \"$0\" --exact session::uml::tests::service_factory_aliasing_the_live_handler_abandons_the_session --nocapture")
                .arg(std::env::current_exe().unwrap())
                .env("HARMONY_UML_ALIASED_SERVICE_CHILD", "1")
                .process_group(0);
            let checkpoints = Checkpoints::new();
            let mut guest = uml::Session::start(
                command,
                work,
                &launch,
                &checkpoints,
                None,
                Some(Duration::from_secs(2)),
            )
            .unwrap();
            assert!(matches!(guest.run_until(100), Ok(Stop::Deadline(100))));
            let old_state = Arc::new(AtomicU8::new(0));
            guest
                .branch(3, Box::new(AliasedHandler(old_state)), Vec::new(), None)
                .unwrap();
            let setup = guest.capture(Capture::default()).unwrap();

            let mut session = unbooted_session();
            session.host = launch.host_identity().unwrap();
            session.launch = launch;
            session.checkpoints = checkpoints;
            session.session = Some(guest);
            session.snapshots.insert(SnapId(1), setup);
            session.setup = (SnapId(1), 100);
            let index = LocalIndex::new(1 << 20);
            let namespace = Namespace::new(&[b"alias"]);
            let lease = match operation {
                "root" => Some(session.publish_root(&index, namespace, SnapId(1)).unwrap()),
                "cached" => Some(
                    session
                        .publish_snapshot(&index, namespace, b"saved", None, SnapId(1), 1)
                        .unwrap(),
                ),
                _ => None,
            };

            let live_state = Arc::new(AtomicU8::new(0));
            session
                .live()
                .unwrap()
                .branch(
                    7,
                    Box::new(AliasedHandler(Arc::clone(&live_state))),
                    Vec::new(),
                    None,
                )
                .unwrap();
            assert_eq!(live_state.load(Ordering::SeqCst), 7);
            let factory_state = Arc::clone(&live_state);
            let result = if operation == "branch" {
                session.factory = Arc::new(move |_| {
                    factory_state.store(9, Ordering::SeqCst);
                    Err(ChannelError::Malformed)
                });
                session.branch_with_service(
                    SnapId(1),
                    ServiceConfig {
                        identity: b"aliased-uml-handler".to_vec(),
                        configuration: Vec::new(),
                    },
                    Vec::new(),
                    Vec::new(),
                    None,
                )
            } else {
                session.factory = Arc::new(move |config| {
                    if config.identity != b"aliased-uml-handler" || !config.configuration.is_empty()
                    {
                        return Err(ChannelError::Malformed);
                    }
                    Ok(Box::new(AliasedHandler(Arc::clone(&factory_state))))
                });
                let lease = lease.as_ref().unwrap();
                if operation == "root" {
                    session.import_root(&index, lease).map(|_| ())
                } else {
                    session.import_cached(&index, lease, SnapId(1)).map(|_| ())
                }
            };
            assert!(result.is_err());
            assert_eq!(
                live_state.load(Ordering::SeqCst),
                if operation == "branch" { 9 } else { 3 }
            );
            assert!(session.abandoned());
            assert!(matches!(
                session
                    .cache_identity()
                    .unwrap_err()
                    .downcast_ref::<SessionError>(),
                Some(SessionError::Abandoned)
            ));
        }
    }

    fn extent(index: &LocalIndex, key: &[u8], identity: [u8; 32]) -> Lease {
        let sidecar = cache_sidecar(identity, b"checkpoint").unwrap();
        let len = extent_len(0, 0, sidecar.len()).unwrap();
        let mut extent = index.extent(len).unwrap();
        write_extent(extent.bytes_mut(), &[], &[], &sidecar).unwrap();
        index
            .publish(Namespace::new(&[b"test"]), key, None, extent, [0; 32], 1)
            .unwrap()
    }

    #[test]
    fn profile_changed_after_admission_rejects_before_guest_boot() {
        let error = require_profile_identity("changed", "admitted").unwrap_err();
        assert!(error.to_string().contains("profile identity changed"));
        require_profile_identity("admitted", "admitted").unwrap();
    }

    #[test]
    fn initramfs_changed_after_admission_rejects_before_guest_boot() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("override.cpio");
        std::fs::write(&path, b"changed initramfs").unwrap();
        let mut launch = Launch::new(directory.path().to_path_buf());
        launch.set_initramfs(&path).unwrap();
        let expected = Sha256::digest(b"admitted initramfs").into();
        let error = admitted_initramfs(&launch, expected).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("initramfs changed after workload admission")
        );
        assert_eq!(
            admitted_initramfs(&launch, Sha256::digest(b"changed initramfs").into()).unwrap(),
            b"changed initramfs"
        );
    }

    #[test]
    fn missing_admitted_profile_cannot_restore_a_fresh_guest() {
        let mut session = unbooted_session();
        let error = session.restore(SnapId(1)).unwrap_err();
        assert!(error.to_string().contains("has no admitted profile"));
        assert!(session.session.is_none());
    }

    #[test]
    fn foreign_uml_root_and_delta_leases_reject_before_checkpoint_lookup() {
        let index = LocalIndex::new(1 << 20);
        let mut session = unbooted_session();
        let root = extent(&index, b"root", [8; 32]);
        let error = session.import_root(&index, &root).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cached snapshot identity differs")
        );
        index.release(root);

        let delta = extent(&index, b"delta", [8; 32]);
        let error = session
            .import_cached(&index, &delta, SnapId(1))
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cached snapshot identity differs")
        );
        assert_eq!(session.snapshots.len(), 0);
        assert_eq!(session.next, 1);
        index.release(delta);

        let valid = cache_sidecar([7; 32], b"checkpoint").unwrap();
        assert_eq!(
            checked_cache_sidecar([7; 32], &valid).unwrap(),
            b"checkpoint"
        );
        assert!(checked_cache_sidecar([7; 32], &valid[..8]).is_err());
    }

    #[test]
    fn root_identity_refuses_a_different_admitted_guest_processor() {
        let profile_identity = "admitted-profile";
        let host = host();
        let config = UmlLaunch {
            profile: PathBuf::new(),
            expected_profile_identity_sha256: profile_identity.into(),
            expected_host: host.clone(),
            initramfs: PathBuf::new(),
            expected_initramfs_sha256: Sha256::digest(b"initramfs").into(),
            memory_mib: 128,
            kernel_arguments: vec!["rdinit=/init".into()],
            work_parent: PathBuf::new(),
            seed: 7,
            setup_budget: 100,
            progress_limit: Duration::from_secs(1),
        };
        let local = root_identity(&config, profile_identity, &host, b"initramfs", "loaded-a");
        let mut foreign_host = host.clone();
        foreign_host.processor ^= 1;
        let foreign = root_identity(
            &config,
            profile_identity,
            &foreign_host,
            b"initramfs",
            "loaded-a",
        );
        assert_ne!(local, foreign);
        let mut foreign_boot = host.clone();
        foreign_boot.boot_cpuinfo.push_str("cache_alignment: 128\n");
        assert_ne!(
            local,
            root_identity(
                &config,
                profile_identity,
                &foreign_boot,
                b"initramfs",
                "loaded-a"
            )
        );
        let foreign_executable =
            root_identity(&config, profile_identity, &host, b"initramfs", "loaded-b");
        assert_ne!(local, foreign_executable);

        let index = LocalIndex::new(1 << 20);
        let mut session = unbooted_session();
        session.root_identity = local;
        let root = extent(&index, b"foreign-cpu-root", foreign);
        let error = session.import_root(&index, &root).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cached snapshot identity differs")
        );
        assert_eq!(session.snapshots.len(), 0);
        assert_eq!(session.next, 1);
        index.release(root);

        let foreign = extent(&index, b"foreign-executable-root", foreign_executable);
        let error = session.import_root(&index, &foreign).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cached snapshot identity differs")
        );
        assert_eq!(session.snapshots.len(), 0);
        index.release(foreign);
    }

    #[test]
    fn root_identity_separates_boot_arguments_from_initramfs_bytes() {
        let profile_identity = "admitted-profile";
        let host = host();
        let mut config = UmlLaunch {
            profile: PathBuf::new(),
            expected_profile_identity_sha256: profile_identity.into(),
            expected_host: host.clone(),
            initramfs: PathBuf::new(),
            expected_initramfs_sha256: Sha256::digest(b"070701").into(),
            memory_mib: 128,
            kernel_arguments: vec![String::new()],
            work_parent: PathBuf::new(),
            seed: 7,
            setup_budget: 100,
            progress_limit: Duration::from_secs(1),
        };
        let with_empty_argument =
            root_identity(&config, profile_identity, &host, b"070701", "loaded-a");
        config.kernel_arguments.clear();
        let mut padded_initramfs = [0_u8; 8].to_vec();
        padded_initramfs.extend_from_slice(b"070701");
        let with_padding = root_identity(
            &config,
            profile_identity,
            &host,
            &padded_initramfs,
            "loaded-a",
        );
        assert_ne!(with_empty_argument, with_padding);
    }
}
