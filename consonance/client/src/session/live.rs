// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{collections::BTreeMap, error::Error, fmt, path::Path, time::Duration};

use control_proto::{
    Reply, Reproducer, Request, SnapId, StopConditions, StopMask, StopReason, class_bit,
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

pub struct Session {
    client: Server,
    setup: SnapId,
    setup_at: u64,
    image_identity: [u8; 32],
    config: SessionConfig,
    snapshot_times: BTreeMap<SnapId, u64>,
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
    #[must_use]
    pub fn identity(kernel: &[u8], initramfs: &[u8]) -> String {
        super::identity(kernel, initramfs)
    }

    #[must_use]
    pub fn identity_with_config(kernel: &[u8], initramfs: &[u8], config: &SessionConfig) -> String {
        super::identity_with_config(kernel, initramfs, config)
    }

    pub fn new(kernel: &[u8], initramfs: &[u8]) -> Result<Self, Box<dyn Error>> {
        Self::new_with_config(kernel, initramfs, SessionConfig::default())
    }

    pub fn new_with_config(
        kernel: &[u8],
        initramfs: &[u8],
        config: SessionConfig,
    ) -> Result<Self, Box<dyn Error>> {
        Self::new_with_config_and_payloads(kernel, initramfs, config, Vec::new())
    }

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

    pub fn from_paths(kernel: &Path, initramfs: &Path) -> Result<Self, Box<dyn Error>> {
        let kernel_bytes = std::fs::read(kernel)
            .map_err(|error| format!("read kernel {}: {error}", kernel.display()))?;
        let initramfs_bytes = std::fs::read(initramfs)
            .map_err(|error| format!("read initramfs {}: {error}", initramfs.display()))?;
        Self::new(&kernel_bytes, &initramfs_bytes)
    }

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

    pub fn setup_snapshot(&self) -> Result<PortableSnapshot, Box<dyn Error>> {
        self.export_snapshot(self.setup, self.setup_at)
    }

    pub fn setup_sparse_snapshot(&self) -> Result<SparseSnapshot, Box<dyn Error>> {
        self.export_sparse_snapshot(self.setup, None)
    }

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

    pub fn snapshot(&mut self) -> Result<(SnapId, u64), Box<dyn Error>> {
        let receipt = snapshot_handle(&mut self.client, "snapshot")?;
        self.snapshot_times.insert(receipt.id, receipt.at);
        Ok((receipt.id, receipt.at))
    }

    pub fn drop_snapshot(&mut self, snapshot: SnapId) -> Result<(), Box<dyn Error>> {
        self.drop_handle(snapshot)?;
        self.snapshot_times.remove(&snapshot);
        Ok(())
    }

    pub fn branch_payloads(
        &mut self,
        snapshot: SnapId,
        payloads: Vec<Vec<u8>>,
    ) -> Result<(), Box<dyn Error>> {
        branch_payload(&mut self.client, snapshot, payloads, self.config.seed)
    }

    pub fn set_service_factory(&mut self, factory: ServiceFactory) {
        self.client.transport_mut().set_service_factory(factory);
    }

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

    fn drive(&mut self, request: &Request) -> Result<Reply, Box<dyn Error>> {
        drive_guarded(
            &mut self.client,
            self.config.wall_limit,
            &mut self.abandoned,
            request,
        )
    }

    pub fn replay_snapshot(&mut self, snapshot: SnapId) -> Result<(), Box<dyn Error>> {
        self.replay(snapshot)
    }

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

    pub fn console_tail(&mut self) -> Result<Vec<u8>, Box<dyn Error>> {
        drain_console(&mut self.client)
    }

    pub fn run_to_snapshot(&mut self, floor: u64) -> Result<u64, Box<dyn Error>> {
        run_to_snapshot(
            &mut self.client,
            floor,
            self.config.run_budget,
            self.config.wall_limit,
            &mut self.abandoned,
        )
    }

    pub fn set_max_chain_len(&mut self, max_chain_len: u32) {
        self.client.transport_mut().set_max_chain_len(max_chain_len);
    }

    pub fn snapshot_owned_pages(&self, snapshot: SnapId) -> Option<u64> {
        self.client
            .transport()
            .snapshot_stats(snapshot)
            .map(|stats| stats.owned_pages)
    }

    pub fn last_seal_dirty_gfns(&self) -> Option<Vec<u64>> {
        self.client
            .transport()
            .last_seal_dirty_gfns()
            .map(ToOwned::to_owned)
    }

    pub fn snapshot_chain_len(&self, snapshot: SnapId) -> Option<u32> {
        self.client.transport().snapshot_chain_len(snapshot)
    }

    pub fn last_restore_stats(&self) -> (u64, u64) {
        (
            self.client.transport().last_restore_bytes_written(),
            self.client.transport().in_place_fallbacks(),
        )
    }

    pub fn doorbell_exits(&self) -> u64 {
        self.client
            .transport()
            .vmm()
            .map(|vmm| vmm.doorbell_exits())
            .unwrap_or(0)
    }

    pub fn restore(&mut self, snapshot: &PortableSnapshot) -> Result<(), Box<dyn Error>> {
        let handle = self.import_snapshot(snapshot)?;
        let result = self.replay(handle);
        self.cleanup_handles([handle], result)
    }

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
            self.export_snapshot(target.id, target.at)
        })();
        self.cleanup_handles(temporary_handles, result)
    }

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

    #[must_use]
    pub fn image_identity(&self) -> [u8; 32] {
        self.image_identity
    }

    #[must_use]
    pub fn setup_at(&self) -> u64 {
        self.setup_at
    }

    #[must_use]
    pub fn setup_handle(&self) -> (SnapId, u64) {
        (self.setup, self.setup_at)
    }

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
