// SPDX-License-Identifier: AGPL-3.0-or-later
//! Live in-process boot and control operations for [`super::Session`].
//!
//! This module is kept separate from the portable session/configuration types:
//! it requires a real VMM composition and is exercised by Linux/KVM lanes.

use std::{collections::BTreeMap, error::Error, fmt, path::Path, time::Duration};

use control_proto::{
    ControlError, Reply, Reproducer, Request, SnapId, StopConditions, StopMask, StopReason,
    class_bit,
};
use environment::{
    channel::Effect,
    input_spec::{InputSpec, ServiceConfig, ServiceFactory},
};
use vmm_backend::Backend;
use vmm_core::control::{ControlServer, RestoreMode, VmmFactory, server_caps};
use vmm_core::vmm::Vmm;

use crate::watchdog::Watchdog;

#[cfg(target_arch = "aarch64")]
use vmm_backend::Arm64 as HostArch;
#[cfg(target_arch = "x86_64")]
use vmm_backend::X86 as HostArch;
#[cfg(target_arch = "aarch64")]
use vmm_core::vendor::arm64::bringup::boot_selected_control;
#[cfg(target_arch = "x86_64")]
use vmm_core::vendor::x86::bringup::boot_linux_stock_virtual_time;

use super::*;
use crate::Client;

type Server = Client<ControlServer<Box<dyn Backend<A = HostArch>>>>;

/// One in-process whole-VM session.
pub struct Session {
    client: Server,
    setup: SnapId,
    setup_at: u64,
    image_identity: [u8; 32],
    config: SessionConfig,
    snapshot_times: BTreeMap<SnapId, u64>,
    /// Set once a run exceeds the wall-clock bound. The VM was canceled, so no
    /// later request may enter it.
    abandoned: bool,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Session")
            .field("setup", &self.setup)
            .field("setup_at", &self.setup_at)
            .field("image_identity", &self.image_identity)
            .field("config", &self.config)
            .field("snapshot_times", &self.snapshot_times.len())
            .field("abandoned", &self.abandoned)
            .finish_non_exhaustive()
    }
}

impl Session {
    /// Stable identity for the complete VM image and neutral session contract.
    #[must_use]
    pub fn identity(kernel: &[u8], initramfs: &[u8]) -> String {
        super::identity(kernel, initramfs)
    }

    /// Stable identity for an image and its complete launch/resource contract.
    #[must_use]
    pub fn identity_with_config(kernel: &[u8], initramfs: &[u8], config: &SessionConfig) -> String {
        super::identity_with_config(kernel, initramfs, config)
    }

    /// Boot one guest, configure the empty ordered payload tape, and retain a
    /// setup snapshot at the guest's `setup_complete` lifecycle point.
    pub fn new(kernel: &[u8], initramfs: &[u8]) -> Result<Self, Box<dyn Error>> {
        Self::new_with_config(kernel, initramfs, SessionConfig::default())
    }

    /// Boot one guest with package-owned launch and resource settings.
    pub fn new_with_config(
        kernel: &[u8],
        initramfs: &[u8],
        config: SessionConfig,
    ) -> Result<Self, Box<dyn Error>> {
        Self::new_with_config_and_payloads(kernel, initramfs, config, Vec::new())
    }

    /// Boot one guest with explicit launch settings and setup payloads.
    pub fn new_with_config_and_payloads(
        kernel: &[u8],
        initramfs: &[u8],
        config: SessionConfig,
        setup_payloads: Vec<Vec<u8>>,
    ) -> Result<Self, Box<dyn Error>> {
        config.validate()?;
        let image_identity = image_identity_with_config(kernel, initramfs, &config);
        let ram = config.ram_bytes;
        #[cfg(target_arch = "x86_64")]
        let seed = config.seed;
        let cmdline = config.cmdline.clone();
        let defer_checkpoint_hashes = config.defer_virtual_time_checkpoint_hashes;
        let boot = move |kernel: &[u8], initramfs: &[u8]| {
            #[cfg(target_arch = "x86_64")]
            let mut vmm = boot_linux_stock_virtual_time(kernel, initramfs, ram, &cmdline, seed)
                .map_err(|error| format!("Consonance boot compose failed: {error:?}"))?;
            #[cfg(target_arch = "aarch64")]
            let mut vmm = boot_selected_control(kernel, initramfs, &cmdline, ram)
                .map_err(|error| format!("Consonance boot compose failed: {error:?}"))?;
            vmm.wire_snapshot_hashing();
            // Before the guest runs, so the boot's own checkpoints are deferred
            // too, and on every VM this closure builds, which includes the ones
            // a restore boots from the session factory.
            if defer_checkpoint_hashes {
                vmm.defer_virtual_time_checkpoint_hashes()
                    .map_err(|error| format!("defer virtual-time hashes: {error}"))?;
            }
            Ok::<_, String>(vmm)
        };

        let live = boot(kernel, initramfs).map_err(SessionError::Control)?;
        let factory_kernel = kernel.to_vec();
        let factory_initramfs = initramfs.to_vec();
        let factory: VmmFactory<Box<dyn Backend<A = HostArch>>> = Box::new(move || {
            boot(&factory_kernel, &factory_initramfs)
                .map_err(vmm_core::vmm::VmmError::ContractViolation)
        });
        let mut server = ControlServer::new(live, factory);
        server.set_restore_mode(RestoreMode::InPlace);
        let mut client = Client::connect(server, server_caps())
            .map_err(|error| SessionError::Control(error.to_string()))?;

        let genesis = snapshot_handle(&mut client, "genesis snapshot")?;
        branch_payload(&mut client, genesis.id, setup_payloads, config.seed)?;
        // A setup run that exceeds the bound fails construction outright, so
        // the session this returns has never been abandoned.
        let mut abandoned = false;
        let setup_stop = run_to_snapshot(
            &mut client,
            genesis.at,
            config.run_budget,
            config.wall_limit,
            &mut abandoned,
        )?;
        let setup = snapshot_handle(&mut client, "setup snapshot")?;
        if setup_stop != setup.at {
            return Err(SessionError::Control(
                "setup lifecycle point and setup seal have different V-times".into(),
            )
            .into());
        }
        Ok(Self {
            client,
            setup: setup.id,
            setup_at: setup.at,
            image_identity,
            config,
            snapshot_times: BTreeMap::from([(genesis.id, genesis.at), (setup.id, setup.at)]),
            abandoned,
        })
    }

    /// Construct a session from image paths.
    pub fn from_paths(kernel: &Path, initramfs: &Path) -> Result<Self, Box<dyn Error>> {
        let kernel_bytes = std::fs::read(kernel)
            .map_err(|error| format!("read kernel {}: {error}", kernel.display()))?;
        let initramfs_bytes = std::fs::read(initramfs)
            .map_err(|error| format!("read initramfs {}: {error}", initramfs.display()))?;
        Self::new(&kernel_bytes, &initramfs_bytes)
    }

    /// Construct a configured session from image paths.
    pub fn from_paths_with_config(
        kernel: &Path,
        initramfs: &Path,
        config: SessionConfig,
    ) -> Result<Self, Box<dyn Error>> {
        let kernel_bytes = std::fs::read(kernel)
            .map_err(|error| format!("read kernel {}: {error}", kernel.display()))?;
        let initramfs_bytes = std::fs::read(initramfs)
            .map_err(|error| format!("read initramfs {}: {error}", initramfs.display()))?;
        Self::new_with_config(&kernel_bytes, &initramfs_bytes, config)
    }

    /// Capture the setup point as a portable snapshot.
    pub fn setup_snapshot(&self) -> Result<PortableSnapshot, Box<dyn Error>> {
        self.export_snapshot(self.setup, self.setup_at)
    }

    /// Capture the setup point in the sparse snapshot representation used by
    /// adapters that retain page and sidecar sharing across archive entries.
    pub fn setup_sparse_snapshot(&self) -> Result<SparseSnapshot, Box<dyn Error>> {
        self.export_sparse_snapshot(self.setup, None)
    }

    /// Capture a held snapshot as a sparse, share-aware portable value.
    pub fn export_sparse_snapshot(
        &self,
        snapshot: SnapId,
        export_base: Option<&SparseSnapshot>,
    ) -> Result<SparseSnapshot, Box<dyn Error>> {
        self.snapshot_times
            .get(&snapshot)
            .ok_or_else(|| SessionError::Portable("snapshot handle is not held".into()))?;
        let sparse = self
            .client
            .transport()
            .export_sparse_snapshot(self.setup, snapshot)
            .map_err(|error| SessionError::Portable(error.to_string()))?;
        SparseSnapshot::from_parts(
            self.setup.0,
            self.image_identity,
            sparse.pages,
            &sparse.sidecar,
            export_base,
        )
        .map_err(SessionError::Portable)
        .map_err(Into::into)
    }

    /// Import a sparse portable value and return its fresh control handle.
    pub fn import_sparse_snapshot(
        &mut self,
        snapshot: &SparseSnapshot,
    ) -> Result<SnapId, Box<dyn Error>> {
        if snapshot.base != self.setup.0 || snapshot.image_identity != self.image_identity {
            return Err(SessionError::Portable("snapshot identity differs".into()).into());
        }
        let receipt = self
            .client
            .transport_mut()
            .import_sparse_snapshot_parts(
                self.setup,
                &snapshot.pages,
                &snapshot.sidecar.materialize(),
            )
            .map_err(|error| SessionError::Portable(error.to_string()))?;
        self.snapshot_times.insert(receipt.id, receipt.at.0);
        Ok(receipt.id)
    }

    /// Snapshot the current control-server state and retain its V-time.
    pub fn snapshot(&mut self) -> Result<(SnapId, u64), Box<dyn Error>> {
        let receipt = snapshot_handle(&mut self.client, "snapshot")?;
        self.snapshot_times.insert(receipt.id, receipt.at);
        Ok((receipt.id, receipt.at))
    }

    /// Drop a held control-server snapshot.
    pub fn drop_snapshot(&mut self, snapshot: SnapId) -> Result<(), Box<dyn Error>> {
        self.drop_handle(snapshot)?;
        self.snapshot_times.remove(&snapshot);
        Ok(())
    }

    /// Branch from a held snapshot with ordered opaque payload records.
    pub fn branch_payloads(
        &mut self,
        snapshot: SnapId,
        payloads: Vec<Vec<u8>>,
    ) -> Result<(), Box<dyn Error>> {
        branch_payload(&mut self.client, snapshot, payloads, self.config.seed)
    }

    /// Install the composition's service implementation resolver, so a branch
    /// carrying a package's service configuration builds that package's
    /// handler for opaque SDK service requests. Held snapshots keep the
    /// identity and configuration they recorded.
    pub fn set_service_factory(&mut self, factory: ServiceFactory) {
        self.client.transport_mut().set_service_factory(factory);
    }

    /// Branch from a held snapshot under a package's service configuration,
    /// ordered payload records, and the host-plane effects to apply during the
    /// run that follows, each at the virtual moment it is recorded against.
    ///
    /// The installed service factory builds the handler and the control server
    /// checks every effect before the live VM changes, so a configuration whose
    /// implementation is not installed — or an effect the machine cannot apply
    /// at the moment given — fails the branch and leaves the session untouched.
    pub fn branch_with_service(
        &mut self,
        snapshot: SnapId,
        config: ServiceConfig,
        payloads: Vec<Vec<u8>>,
        effects: Vec<(u64, Effect)>,
    ) -> Result<(), Box<dyn Error>> {
        let spec = service_branch_spec(self.config.seed, config, payloads, effects)?;
        branch_spec(&mut self.client, snapshot, &spec)
    }

    /// Run from the current state until virtual time reaches `deadline`, or
    /// until the guest stops earlier on an SDK assertion, a crash, or
    /// quiescence.
    pub fn run_until(&mut self, deadline: u64) -> Result<StopReason, Box<dyn Error>> {
        let request = Request::Run {
            until: StopConditions {
                deadline: Some(control_proto::Moment(deadline)),
                on: StopMask::NONE.arm(class_bit::ASSERTION),
            },
            resolve: None,
        };
        match self.drive(&request)? {
            Reply::Stop(stop) => Ok(stop),
            reply => Err(SessionError::Reply {
                operation: "run until",
                reply,
            }
            .into()),
        }
    }

    /// Snapshot the current stopped state, running the guest a further
    /// `settle_step` of virtual time whenever the control server cannot seal
    /// that point yet, up to `max_settle` in total.
    ///
    /// Reports the snapshot, its V-time, and the stop reason of the last settle
    /// run — absent when the point sealed with no settling.
    pub fn seal(
        &mut self,
        settle_step: u64,
        max_settle: u64,
    ) -> Result<(SnapId, u64, Option<StopReason>), Box<dyn Error>> {
        seal_after_settling(
            self,
            settle_step,
            max_settle,
            Self::try_seal,
            Self::settle_step,
        )
    }

    /// Seal the current point, or `None` when the server cannot seal it yet.
    fn try_seal(&mut self) -> Result<Option<(SnapId, u64)>, Box<dyn Error>> {
        let outcome = self.client.transport_mut().handle(&Request::Snapshot);
        match outcome {
            Ok(Ok(Reply::Snapshot {
                id,
                at,
                tainted: false,
                ..
            })) => {
                self.snapshot_times.insert(id, at.0);
                Ok(Some((id, at.0)))
            }
            Ok(Ok(Reply::Snapshot {
                id, tainted: true, ..
            })) => {
                // A tainted seal is still minted; release it before reporting.
                let _ = drop_control_handle(&mut self.client, id);
                Err(SessionError::Control("seal was tainted".into()).into())
            }
            Ok(Ok(reply)) => Err(SessionError::Reply {
                operation: "seal",
                reply,
            }
            .into()),
            Ok(Err(ControlError::NotQuiescent)) => Ok(None),
            Ok(Err(error)) => Err(SessionError::Control(error.to_string()).into()),
            Err(error) => Err(SessionError::Control(error.to_string()).into()),
        }
    }

    /// Run `step` further nanoseconds of virtual time from wherever the guest
    /// currently stands.
    fn settle_step(&mut self, step: u64) -> Result<StopReason, Box<dyn Error>> {
        let now = self
            .client
            .transport()
            .vmm()
            .and_then(Vmm::effective_vns)
            .ok_or_else(|| SessionError::Control("live VM has no virtual time".to_owned()))?;
        let deadline = now
            .checked_add(step)
            .ok_or("Consonance settle deadline overflow")?;
        self.run_until(deadline)
    }

    /// Issue one control request with the session's host wall-clock bound armed.
    fn drive(&mut self, request: &Request) -> Result<Reply, Box<dyn Error>> {
        drive_guarded(
            &mut self.client,
            self.config.wall_limit,
            &mut self.abandoned,
            request,
        )
    }

    /// Replay a held snapshot without staging any payloads.
    pub fn replay_snapshot(&mut self, snapshot: SnapId) -> Result<(), Box<dyn Error>> {
        self.replay(snapshot)
    }

    /// Run until the caller's control conditions return a stop reason.
    pub fn run(
        &mut self,
        until: StopConditions,
        resolve: Option<control_proto::Resolution>,
    ) -> Result<StopReason, Box<dyn Error>> {
        let reply = self.drive(&Request::Run { until, resolve })?;
        match reply {
            Reply::Stop(stop) => Ok(stop),
            reply => Err(SessionError::Reply {
                operation: "run",
                reply,
            }
            .into()),
        }
    }

    /// Read guest physical memory through the control protocol.
    pub fn read(&mut self, gpa: u64, len: u32) -> Result<Vec<u8>, Box<dyn Error>> {
        match self
            .client
            .request(&Request::Read { gpa, len })
            .map_err(|error| SessionError::Control(error.to_string()))?
        {
            Reply::Bytes(bytes) => Ok(bytes),
            reply => Err(SessionError::Reply {
                operation: "read",
                reply,
            }
            .into()),
        }
    }

    /// Read a bounded tail of the guest console for diagnostics.
    ///
    /// Console collection is best-effort at the workload boundary: callers can
    /// report the original control or execution failure when the diagnostic
    /// request itself is unavailable.
    pub fn console_tail(&mut self) -> Result<Vec<u8>, Box<dyn Error>> {
        drain_console(&mut self.client)
    }

    /// Run the fixed setup lifecycle point used by payload guests.
    pub fn run_to_snapshot(&mut self, floor: u64) -> Result<u64, Box<dyn Error>> {
        run_to_snapshot(
            &mut self.client,
            floor,
            self.config.run_budget,
            self.config.wall_limit,
            &mut self.abandoned,
        )
    }

    /// Set the VMM snapshot derive-chain bound for profiling experiments.
    pub fn set_max_chain_len(&mut self, max_chain_len: u32) {
        self.client.transport_mut().set_max_chain_len(max_chain_len);
    }

    /// Non-zero pages owned by one held snapshot, when store statistics exist.
    pub fn snapshot_owned_pages(&self, snapshot: SnapId) -> Option<u64> {
        self.client
            .transport()
            .snapshot_stats(snapshot)
            .map(|stats| stats.owned_pages)
    }

    /// Dirty page GFNs drained by the most recent seal.
    pub fn last_seal_dirty_gfns(&self) -> Option<Vec<u64>> {
        self.client
            .transport()
            .last_seal_dirty_gfns()
            .map(ToOwned::to_owned)
    }

    /// Derive-chain length of one held snapshot.
    pub fn snapshot_chain_len(&self, snapshot: SnapId) -> Option<u32> {
        self.client.transport().snapshot_chain_len(snapshot)
    }

    /// Restore statistics from the most recent branch or replay.
    pub fn last_restore_stats(&self) -> (u64, u64) {
        (
            self.client.transport().last_restore_bytes_written(),
            self.client.transport().in_place_fallbacks(),
        )
    }

    /// Doorbell exits accumulated by the live VMM.
    pub fn doorbell_exits(&self) -> u64 {
        self.client
            .transport()
            .vmm()
            .map(|vmm| vmm.doorbell_exits())
            .unwrap_or(0)
    }

    /// Restore a portable snapshot into this session and make it current.
    pub fn restore(&mut self, snapshot: &PortableSnapshot) -> Result<(), Box<dyn Error>> {
        let handle = self.import_snapshot(snapshot)?;
        let result = self.replay(handle);
        self.cleanup_handles([handle], result)
    }

    /// Apply exactly one opaque payload record from `snapshot` and stop at the
    /// guest's next lifecycle point.  The result is a portable whole-VM seal.
    pub fn apply_payload(
        &mut self,
        snapshot: &PortableSnapshot,
        payload: Vec<u8>,
    ) -> Result<PortableSnapshot, Box<dyn Error>> {
        let base = self.import_snapshot(snapshot)?;
        let mut temporary_handles = vec![base];
        let result = (|| {
            branch_payload(&mut self.client, base, vec![payload], self.config.seed)?;
            let stop_at = run_to_snapshot(
                &mut self.client,
                snapshot.at,
                self.config.run_budget,
                self.config.wall_limit,
                &mut self.abandoned,
            )?;
            let target = snapshot_handle(&mut self.client, "action snapshot")?;
            temporary_handles.push(target.id);
            if stop_at != target.at {
                return Err(SessionError::Control(
                    "action lifecycle point and action seal have different V-times".into(),
                )
                .into());
            }
            // Keep cleanup after export: the sparse target may still depend on
            // its imported base until the store has resolved the page set.
            self.export_snapshot(target.id, target.at)
        })();
        self.cleanup_handles(temporary_handles, result)
    }

    /// Read the current SDK event prefix, including the package's check state.
    pub fn sdk_events(&mut self) -> Result<Vec<SdkEvent>, Box<dyn Error>> {
        let mut offset = 0_u32;
        let mut events = Vec::new();
        loop {
            let reply = self
                .client
                .request(&Request::SdkEvents { offset })
                .map_err(|error| SessionError::Control(error.to_string()))?;
            let Reply::SdkEvents(page) = reply else {
                return Err(SessionError::Reply {
                    operation: "SDK events",
                    reply,
                }
                .into());
            };
            if page.is_empty() {
                break;
            }
            offset = offset
                .checked_add(u32::try_from(page.len()).map_err(|_| "SDK event page overflow")?)
                .ok_or("SDK event offset overflow")?;
            events.extend(page);
        }
        Ok(events)
    }

    /// Read the current whole-VM state digest.
    pub fn state_hash(&mut self) -> Result<[u8; 32], Box<dyn Error>> {
        match self
            .client
            .request(&Request::Hash {
                scope: control_proto::HashScope::Whole,
            })
            .map_err(|error| SessionError::Control(error.to_string()))?
        {
            Reply::Hash(hash) => Ok(hash),
            reply => Err(SessionError::Reply {
                operation: "whole-state hash",
                reply,
            }
            .into()),
        }
    }

    /// The setup image identity used to reject a portable snapshot from a
    /// different kernel/initramfs pair.
    #[must_use]
    pub fn image_identity(&self) -> [u8; 32] {
        self.image_identity
    }

    /// The setup V-time from which the session's portable snapshots derive.
    #[must_use]
    pub fn setup_at(&self) -> u64 {
        self.setup_at
    }

    /// The setup snapshot handle retained by this session.
    #[must_use]
    pub fn setup_handle(&self) -> (SnapId, u64) {
        (self.setup, self.setup_at)
    }

    /// V-time associated with a live control snapshot handle.
    #[must_use]
    pub fn snapshot_time(&self, snapshot: SnapId) -> Option<u64> {
        self.snapshot_times.get(&snapshot).copied()
    }

    fn export_snapshot(&self, target: SnapId, at: u64) -> Result<PortableSnapshot, Box<dyn Error>> {
        let sparse = self
            .client
            .transport()
            .export_sparse_snapshot(self.setup, target)
            .map_err(|error| SessionError::Portable(error.to_string()))?;
        let pages = sparse
            .pages
            .into_iter()
            .map(|(gfn, page)| (gfn, page.as_ref().to_vec()))
            .collect();
        Ok(PortableSnapshot {
            setup: self.setup.0,
            image_identity: self.image_identity,
            at,
            pages,
            sidecar: sparse.sidecar,
        })
    }

    fn import_snapshot(&mut self, snapshot: &PortableSnapshot) -> Result<SnapId, Box<dyn Error>> {
        let pages = snapshot
            .validated_pages(self.setup.0, self.image_identity, self.setup_at)
            .map_err(SessionError::Portable)?;
        let receipt = self
            .client
            .transport_mut()
            .import_sparse_snapshot_parts(self.setup, &pages, &snapshot.sidecar)
            .map_err(|error| SessionError::Portable(error.to_string()))?;
        if receipt.at.0 != snapshot.at {
            let error = Err(SessionError::Portable("portable V-time differs".into()).into());
            return self.cleanup_handles([receipt.id], error);
        }
        Ok(receipt.id)
    }

    fn replay(&mut self, handle: SnapId) -> Result<(), Box<dyn Error>> {
        let reply = self
            .client
            .request(&Request::Replay(handle))
            .map_err(|error| SessionError::Control(error.to_string()))?;
        expect_unit(reply, "replay")?;
        retire_restore_trace(&mut self.client);
        Ok(())
    }

    fn drop_handle(&mut self, handle: SnapId) -> Result<(), Box<dyn Error>> {
        drop_control_handle(&mut self.client, handle)
    }

    fn cleanup_handles<T>(
        &mut self,
        handles: impl IntoIterator<Item = SnapId>,
        result: Result<T, Box<dyn Error>>,
    ) -> Result<T, Box<dyn Error>> {
        finish_with_cleanup(handles, result, |handle| self.drop_handle(handle))
    }
}

fn finish_with_cleanup<T, Handles, DropHandle>(
    handles: Handles,
    result: Result<T, Box<dyn Error>>,
    mut drop_handle: DropHandle,
) -> Result<T, Box<dyn Error>>
where
    Handles: IntoIterator<Item = SnapId>,
    DropHandle: FnMut(SnapId) -> Result<(), Box<dyn Error>>,
{
    let mut cleanup_error = None;
    for handle in handles {
        if let Err(error) = drop_handle(handle) {
            cleanup_error.get_or_insert(error);
        }
    }
    match result {
        Ok(value) => cleanup_error.map_or(Ok(value), Err),
        Err(error) => Err(error),
    }
}

struct SnapshotReceipt {
    id: SnapId,
    at: u64,
}

fn snapshot_handle(
    client: &mut Server,
    operation: &'static str,
) -> Result<SnapshotReceipt, Box<dyn Error>> {
    let reply = client
        .request(&Request::Snapshot)
        .map_err(|error| SessionError::Control(error.to_string()))?;
    match reply {
        Reply::Snapshot {
            id,
            at,
            tainted: false,
            ..
        } => Ok(SnapshotReceipt { id, at: at.0 }),
        Reply::Snapshot {
            id, tainted: true, ..
        } => {
            let error: Box<dyn Error> =
                SessionError::Control(format!("{operation} was tainted")).into();
            // A tainted snapshot is still minted by the control server. The
            // session rejects it, so release the handle before returning the
            // caller-facing error.
            let _ = drop_control_handle(client, id);
            Err(error)
        }
        reply => Err(SessionError::Reply { operation, reply }.into()),
    }
}

fn branch_payload(
    client: &mut Server,
    snap: SnapId,
    payloads: Vec<Vec<u8>>,
    seed: u64,
) -> Result<(), Box<dyn Error>> {
    let mut spec = InputSpec::seeded(seed);
    spec.set_payloads(Some(payloads));
    branch_spec(client, snap, &spec)
}

fn branch_spec(client: &mut Server, snap: SnapId, spec: &InputSpec) -> Result<(), Box<dyn Error>> {
    let reply = client
        .request(&Request::Branch {
            snap,
            env: Reproducer {
                blob_version: InputSpec::BLOB_VERSION,
                bytes: spec.encode(),
            },
        })
        .map_err(|error| SessionError::Control(error.to_string()))?;
    expect_unit(reply, "branch")?;
    retire_restore_trace(client);
    Ok(())
}

/// Issue one control request, abandoning the guest if it spends more than
/// `wall_limit` of total host time inside the request.
///
/// The bound is measured against the host clock on purpose: a guest that stalls
/// advances no virtual time, so nothing else can notice it. Once the guard
/// expires the VM is unusable, so the request's own reply is discarded in favor
/// of [`SessionError::Hung`] and `abandoned` is set, which turns every later
/// request into [`SessionError::Abandoned`]. A request that returns first
/// claims the run and keeps its reply.
fn drive_guarded(
    client: &mut Server,
    wall_limit: Option<Duration>,
    abandoned: &mut bool,
    request: &Request,
) -> Result<Reply, Box<dyn Error>> {
    let cancel = client.transport().vmm().and_then(Vmm::cancellation_flag);
    let Some((limit, cancel)) = guarded_run_plan(*abandoned, wall_limit, cancel)? else {
        return client
            .request(request)
            .map_err(|error| SessionError::Control(error.to_string()).into());
    };
    let watchdog = Watchdog::start(limit, cancel).map_err(|error| {
        SessionError::Control(format!("cannot arm the wall-clock bound: {error}"))
    })?;
    let reply = client.request(request);
    let claimed = watchdog.claim();
    drop(watchdog);
    if !claimed {
        *abandoned = true;
        return Err(SessionError::Hung(limit).into());
    }
    reply.map_err(|error| SessionError::Control(error.to_string()).into())
}

// A session exposes observations and snapshots; it does not archive the control
// server's normalized exit trace. Each restore closes the previous segment.
// Release that host-only evidence at the same boundary so long campaigns retain
// only the active segment. This leaves guest state and snapshot hashes untouched.
fn retire_restore_trace(client: &mut Server) {
    drop(client.transport_mut().take_session_virtual_time_trace());
}

fn run_to_snapshot(
    client: &mut Server,
    floor: u64,
    run_budget: u64,
    wall_limit: Option<Duration>,
    abandoned: &mut bool,
) -> Result<u64, Box<dyn Error>> {
    let deadline = floor
        .checked_add(run_budget)
        .ok_or("Consonance run deadline overflow")?;
    let request = Request::Run {
        until: StopConditions {
            deadline: Some(control_proto::Moment(deadline)),
            on: StopMask::NONE.arm(class_bit::SNAPSHOT_POINT),
        },
        resolve: None,
    };
    let reply = drive_guarded(client, wall_limit, abandoned, &request)
        .map_err(|error| with_console(client, error))?;
    match reply {
        Reply::Stop(StopReason::SnapshotPoint { vtime }) => Ok(vtime.0),
        Reply::Stop(stop) => Err(with_console(client, SessionError::Stop(stop).into())),
        reply => Err(with_console(
            client,
            SessionError::Reply {
                operation: "run",
                reply,
            }
            .into(),
        )),
    }
}

/// Capture a bounded console tail/trace without hiding the original failure.
/// Console reads are best-effort: an unavailable or malformed diagnostic reply
/// returns the original error unchanged.
fn with_console(client: &mut Server, error: Box<dyn Error>) -> Box<dyn Error> {
    let Ok(console) = drain_console(client) else {
        return error;
    };
    if console.is_empty() {
        return error;
    }
    Box::new(ConsoleDiagnostic {
        source: error,
        console,
    })
}

fn drain_console(client: &mut Server) -> Result<Vec<u8>, Box<dyn Error>> {
    super::drain_console_pages(|offset| {
        client
            .request(&Request::Console { offset })
            .map_err(|error| -> Box<dyn Error> { error })
    })
}

#[derive(Debug)]
struct ConsoleDiagnostic {
    source: Box<dyn Error>,
    console: Vec<u8>,
}

impl fmt::Display for ConsoleDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}; guest console:\n{}",
            self.source,
            String::from_utf8_lossy(&self.console)
        )
    }
}

impl Error for ConsoleDiagnostic {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

fn expect_unit(reply: Reply, operation: &'static str) -> Result<(), Box<dyn Error>> {
    match reply {
        Reply::Unit => Ok(()),
        reply => Err(SessionError::Reply { operation, reply }.into()),
    }
}

fn drop_control_handle(client: &mut Server, handle: SnapId) -> Result<(), Box<dyn Error>> {
    let reply = client
        .request(&Request::Drop(handle))
        .map_err(|error| SessionError::Control(error.to_string()))?;
    expect_unit(reply, "drop snapshot")
}

/// Return this process's minor-fault count when the host exposes it.
#[must_use]
pub fn host_minor_faults() -> Option<u64> {
    vmm_core::control::host_minor_faults()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[test]
    fn cleanup_attempts_every_handle_and_preserves_primary_error() {
        let mut dropped = Vec::new();
        let primary: Result<(), Box<dyn Error>> = Err(io::Error::other("operation failed").into());
        let result = finish_with_cleanup([SnapId(7), SnapId(11), SnapId(13)], primary, |handle| {
            dropped.push(handle);
            Err(io::Error::other("cleanup failed").into())
        });

        assert_eq!(dropped, [SnapId(7), SnapId(11), SnapId(13)]);
        assert_eq!(result.unwrap_err().to_string(), "operation failed");
    }

    #[test]
    fn cleanup_error_is_reported_when_operation_succeeds() {
        let result = finish_with_cleanup([SnapId(17)], Ok::<_, Box<dyn Error>>(42_u8), |_| {
            Err(io::Error::other("cleanup failed").into())
        });

        assert_eq!(result.unwrap_err().to_string(), "cleanup failed");
    }
}
