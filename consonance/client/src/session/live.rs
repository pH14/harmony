// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{collections::BTreeMap, error::Error, fmt, path::Path, time::Duration};

use control_proto::{
    Reply, Reproducer, Request, SnapId, StopConditions, StopMask, StopReason, class_bit,
};
use environment::{
    channel::Effect,
    input_spec::{InputSpec, ServiceConfig, ServiceFactory},
};
use sha2::{Digest, Sha256};
use vmm_backend::Backend;
use vmm_core::control::{ControlServer, RestoreMode, VmmFactory, server_caps};
use vmm_core::portable_snapshot::sparse_delta_target_commitment;

use crate::cache::extent::{extent_len, read_extent, resolve, write_extent};
use crate::cache::{ANCHOR_DEPTH, CacheIndex, CommittedExtent, Lease, Namespace, WritableExtent};
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
const CACHE_SIDECAR_MAGIC: [u8; 8] = *b"HMSESS01";

pub(super) fn vmm_target_commitment(
    extent: &mut WritableExtent,
) -> Result<[u8; 32], Box<dyn Error>> {
    let delta = read_extent(extent.bytes_mut())?;
    let envelope = super::attachment::decode(delta.sidecar)?;
    let actual = raw_target_commitment(envelope.raw)?;
    if actual != envelope.machine_commitment {
        return Err("cached machine commitment differs".into());
    }
    Ok(super::attachment::commitment(actual, &envelope.attachment))
}

pub(super) fn raw_target_commitment(sidecar: &[u8]) -> Result<[u8; 32], Box<dyn Error>> {
    let prefix_len = CACHE_SIDECAR_MAGIC.len() + 32;
    if sidecar.len() < prefix_len || sidecar[..CACHE_SIDECAR_MAGIC.len()] != CACHE_SIDECAR_MAGIC {
        return Err(SessionError::Portable("cached snapshot sidecar differs".into()).into());
    }
    let raw_commitment = sparse_delta_target_commitment(&sidecar[prefix_len..])?;
    let mut digest = Sha256::new();
    digest.update(b"consonance-cache-vmm-target-v2\0");
    digest.update(&sidecar[..prefix_len]);
    digest.update(raw_commitment);
    Ok(digest.finalize().into())
}

type CheckedAttachment = ([u8; 32], SnapshotAttachment, [u8; 32]);

pub(super) fn checked_cached_attachment(
    extents: &[&[u8]],
    expected: [u8; 32],
) -> Result<CheckedAttachment, Box<dyn Error>> {
    let target = read_extent(extents.last().ok_or("empty snapshot chain")?)?;
    let envelope = super::attachment::decode(target.sidecar)?;
    let machine = raw_target_commitment(envelope.raw)?;
    if machine != envelope.machine_commitment {
        return Err("cached machine commitment differs".into());
    }
    let actual = super::attachment::commitment(machine, &envelope.attachment);
    if actual != expected {
        return Err("cached object commitment differs".into());
    }
    Ok((machine, envelope.attachment, actual))
}

fn collect_sdk_events(
    mut offset: u32,
    mut fetch: impl FnMut(u32) -> Result<Vec<SdkEvent>, Box<dyn Error>>,
) -> Result<Vec<SdkEvent>, Box<dyn Error>> {
    let mut events = Vec::new();
    loop {
        let page = fetch(offset)?;
        if page.is_empty() {
            return Ok(events);
        }
        offset = offset
            .checked_add(u32::try_from(page.len()).map_err(|_| "SDK event page overflow")?)
            .ok_or("SDK event offset overflow")?;
        events.extend(page);
    }
}

pub struct Session {
    client: Server,
    loaded_image_guard: crate::image_identity::LoadedImageGuard,
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
    pub fn identity(kernel: &[u8], initramfs: &[u8]) -> Result<String, String> {
        super::identity(kernel, initramfs)
    }

    pub fn identity_with_config(
        kernel: &[u8],
        initramfs: &[u8],
        config: &SessionConfig,
    ) -> Result<String, String> {
        super::identity_with_config(kernel, initramfs, config)
    }

    pub fn identity_with_config_and_payloads(
        kernel: &[u8],
        initramfs: &[u8],
        config: &SessionConfig,
        setup_payloads: &[Vec<u8>],
    ) -> Result<String, String> {
        super::identity_with_config_and_payloads(kernel, initramfs, config, setup_payloads)
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
        let loaded_image_guard = crate::image_identity::LoadedImageGuard::capture()?;
        let image_identity = super::image_identity_with_guard(
            kernel,
            initramfs,
            &config,
            &setup_payloads,
            Some(&loaded_image_guard),
        )?;
        loaded_image_guard.check_at(crate::image_identity::GuardSite::Admission)?;
        Self::new_with_config_and_payloads_inner(
            kernel,
            initramfs,
            config,
            setup_payloads,
            image_identity,
            loaded_image_guard,
        )
    }

    fn new_with_config_and_payloads_inner(
        kernel: &[u8],
        initramfs: &[u8],
        config: SessionConfig,
        setup_payloads: Vec<Vec<u8>>,
        image_identity: [u8; 32],
        loaded_image_guard: crate::image_identity::LoadedImageGuard,
    ) -> Result<Self, Box<dyn Error>> {
        loaded_image_guard.check_at(crate::image_identity::GuardSite::Admission)?;
        let ram = config.ram_bytes;
        let seed = config.seed;
        let cmdline = config.cmdline.clone();
        let defer_checkpoint_hashes = config.defer_virtual_time_checkpoint_hashes;
        let guest_contract = config.guest_contract;
        let boot = move |kernel: &[u8], initramfs: &[u8]| {
            #[cfg(target_arch = "x86_64")]
            let mut vmm = match guest_contract {
                GuestContract::Ordinary => {
                    boot_linux_stock_virtual_time(kernel, initramfs, ram, &cmdline, seed)
                }
                #[cfg(target_os = "linux")]
                GuestContract::NestedHost => {
                    vmm_core::vendor::x86::bringup::boot_linux_nested_host_virtual_time_boxed(
                        kernel, initramfs, ram, &cmdline, seed,
                    )
                }
                #[cfg(not(target_os = "linux"))]
                GuestContract::NestedHost => {
                    return Err("nested-host sessions require Linux x86 KVM".into());
                }
            }
            .map_err(|error| format!("Consonance boot compose failed: {error:?}"))?;
            #[cfg(target_arch = "aarch64")]
            if guest_contract != GuestContract::Ordinary {
                return Err("nested-host sessions require Linux x86 KVM".into());
            }
            #[cfg(target_arch = "aarch64")]
            let mut vmm = boot_selected_control(kernel, initramfs, &cmdline, ram, seed)
                .map_err(|error| format!("Consonance boot compose failed: {error:?}"))?;
            vmm.wire_snapshot_hashing();
            if defer_checkpoint_hashes {
                vmm.defer_virtual_time_checkpoint_hashes()
                    .map_err(|error| format!("defer virtual-time hashes: {error}"))?;
            }
            Ok::<_, String>(vmm)
        };

        let live = boot(kernel, initramfs).map_err(SessionError::Control)?;
        loaded_image_guard.check_at(crate::image_identity::GuardSite::Admission)?;
        let factory_kernel = kernel.to_vec();
        let factory_initramfs = initramfs.to_vec();
        let factory: VmmFactory<Box<dyn Backend<A = HostArch>>> = Box::new(move || {
            boot(&factory_kernel, &factory_initramfs)
                .map_err(vmm_core::vmm::VmmError::ContractViolation)
        });
        let mut server = ControlServer::new(live, factory);
        let core_guard = loaded_image_guard.clone();
        server.set_host_operation_guard(Arc::new(move |site| {
            use crate::image_identity::GuardSite;
            use vmm_core::vmm::HostOperationSite;
            let site = match site {
                HostOperationSite::ControlBefore => GuardSite::ControlBefore,
                HostOperationSite::ControlAfter => GuardSite::ControlAfter,
                HostOperationSite::SdkRequest => GuardSite::SdkRequest,
                HostOperationSite::SdkResponse => GuardSite::SdkResponse,
            };
            core_guard.check_at(site).map_err(|error| error.to_string())
        }));
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
        loaded_image_guard.check_at(crate::image_identity::GuardSite::Admission)?;
        Ok(Self {
            client,
            loaded_image_guard,
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
        self.loaded_image_guard.check()?;
        self.snapshot_times
            .get(&snapshot)
            .ok_or_else(|| SessionError::Portable("snapshot handle is not held".into()))?;
        let sparse = self
            .client
            .transport()
            .export_sparse_snapshot(self.setup, snapshot)
            .map_err(|error| SessionError::Portable(error.to_string()))?;
        let result = SparseSnapshot::from_parts(
            self.setup.0,
            self.image_identity,
            sparse.pages,
            &sparse.sidecar,
            export_base,
        )
        .map_err(SessionError::Portable)
        .map_err(Into::into);
        self.loaded_image_guard.check()?;
        result
    }

    pub fn import_sparse_snapshot(
        &mut self,
        snapshot: &SparseSnapshot,
    ) -> Result<SnapId, Box<dyn Error>> {
        self.loaded_image_guard.check()?;
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
        self.loaded_image_guard.check()?;
        Ok(receipt.id)
    }

    pub fn publish_snapshot(
        &self,
        index: &dyn CacheIndex,
        namespace: Namespace,
        key: &[u8],
        parent: Option<(SnapId, &Lease)>,
        target: SnapId,
        cost: u64,
    ) -> Result<Lease, Box<dyn Error>> {
        self.publish_snapshot_attached(
            index,
            namespace,
            key,
            parent,
            target,
            &SnapshotAttachment::default(),
            cost,
        )
    }

    #[expect(clippy::too_many_arguments)]
    pub fn publish_snapshot_attached(
        &self,
        index: &dyn CacheIndex,
        namespace: Namespace,
        key: &[u8],
        parent: Option<(SnapId, &Lease)>,
        target: SnapId,
        attachment: &SnapshotAttachment,
        cost: u64,
    ) -> Result<Lease, Box<dyn Error>> {
        let parent = parent.filter(|(_, lease)| lease.depth() + 1 < ANCHOR_DEPTH);
        let mut extent = self.write_delta_attached(
            parent.map(|(snap, _)| snap),
            target,
            attachment,
            |len| Ok(index.extent(len)?),
            WritableExtent::bytes_mut,
        )?;
        let target_commitment = vmm_target_commitment(&mut extent)?;
        self.loaded_image_guard
            .check_at(crate::image_identity::GuardSite::PublicationBefore)?;
        let lease = index.publish(
            namespace,
            key,
            parent.map(|(_, lease)| lease),
            extent,
            target_commitment,
            cost,
        )?;
        let lease = self.loaded_image_guard.check_published(index, lease)?;
        if let Err(error) = index.verify_lease(&lease) {
            index.release(lease);
            return Err(error.into());
        }
        Ok(lease)
    }

    pub(super) fn write_delta_attached<T>(
        &self,
        parent: Option<SnapId>,
        target: SnapId,
        attachment: &SnapshotAttachment,
        allocate: impl FnOnce(usize) -> Result<T, Box<dyn Error>>,
        bytes: impl FnOnce(&mut T) -> &mut [u8],
    ) -> Result<T, Box<dyn Error>> {
        self.loaded_image_guard.check()?;
        let delta = self
            .client
            .transport()
            .export_sparse_delta(self.setup, parent, target)
            .map_err(|error| SessionError::Portable(error.to_string()))?;
        let sidecar_len = CACHE_SIDECAR_MAGIC
            .len()
            .checked_add(self.image_identity.len())
            .and_then(|len| len.checked_add(delta.sidecar.len()))
            .ok_or_else(|| SessionError::Portable("snapshot delta size overflows".into()))?;
        let mut sidecar = Vec::with_capacity(sidecar_len);
        sidecar.extend_from_slice(&CACHE_SIDECAR_MAGIC);
        sidecar.extend_from_slice(&self.image_identity);
        sidecar.extend_from_slice(&delta.sidecar);
        let machine = raw_target_commitment(&sidecar)?;
        let sidecar = super::attachment::encode(&sidecar, machine, attachment)?;
        let len = extent_len(delta.pages.len(), delta.reverted.len(), sidecar.len())
            .ok_or_else(|| SessionError::Portable("snapshot delta size overflows".into()))?;
        let mut out = allocate(len)?;
        write_extent(bytes(&mut out), &delta.pages, &delta.reverted, &sidecar)?;
        self.loaded_image_guard.check()?;
        Ok(out)
    }

    fn checked_cache_sidecar<'a>(&self, sidecar: &'a [u8]) -> Result<&'a [u8], Box<dyn Error>> {
        if sidecar.len() < CACHE_SIDECAR_MAGIC.len() + 32
            || sidecar[..CACHE_SIDECAR_MAGIC.len()] != CACHE_SIDECAR_MAGIC
        {
            return Err(SessionError::Portable("cached snapshot sidecar differs".into()).into());
        }
        let identity_start = CACHE_SIDECAR_MAGIC.len();
        let identity_end = identity_start + 32;
        if sidecar[identity_start..identity_end] != self.image_identity {
            return Err(SessionError::Portable("cached snapshot identity differs".into()).into());
        }
        Ok(&sidecar[identity_end..])
    }

    pub fn import_cached(
        &mut self,
        index: &dyn CacheIndex,
        lease: &Lease,
        near: SnapId,
    ) -> Result<(SnapId, u64), Box<dyn Error>> {
        let receipt = self.import_cached_receipt(index, lease, near)?;
        Ok((receipt.snapshot, receipt.moment))
    }

    pub fn import_cached_receipt(
        &mut self,
        index: &dyn CacheIndex,
        lease: &Lease,
        near: SnapId,
    ) -> Result<CachedSnapshotReceipt, Box<dyn Error>> {
        let chain = index.chain(lease)?;
        let extents: Vec<&[u8]> = chain.iter().map(CommittedExtent::bytes).collect();
        let (machine_commitment, attachment, object_commitment) =
            checked_cached_attachment(&extents, index.target_commitment(lease)?)?;
        let imported = self.import_extents(&extents, near)?;
        if let Err(error) = index.verify_lease(lease) {
            let _ = self.drop_snapshot(imported.0);
            return Err(error.into());
        }
        Ok(CachedSnapshotReceipt {
            snapshot: imported.0,
            moment: imported.1,
            machine_commitment,
            attachment,
            object_commitment,
        })
    }

    pub(super) fn import_extents(
        &mut self,
        extents: &[&[u8]],
        near: SnapId,
    ) -> Result<(SnapId, u64), Box<dyn Error>> {
        self.loaded_image_guard.check()?;
        let mut deltas = extents
            .iter()
            .map(|extent| read_extent(extent))
            .collect::<Result<Vec<_>, _>>()?;
        for delta in &mut deltas {
            let envelope = super::attachment::decode(delta.sidecar)?;
            if raw_target_commitment(envelope.raw)? != envelope.machine_commitment {
                return Err("cached machine commitment differs".into());
            }
            delta.sidecar = self.checked_cache_sidecar(envelope.raw)?;
        }
        let resolved = resolve(&deltas)?;
        let receipt = self
            .client
            .transport_mut()
            .import_sparse_delta(self.setup, near, &resolved.pages, resolved.sidecar)
            .map_err(|error| SessionError::Portable(error.to_string()))?;
        if let Err(error) = self.loaded_image_guard.check() {
            let _ = self.drop_handle(receipt.id);
            return Err(error.into());
        }
        self.snapshot_times.insert(receipt.id, receipt.at.0);
        Ok((receipt.id, receipt.at.0))
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

    pub fn branch_with_seed(&mut self, snapshot: SnapId, seed: u64) -> Result<(), Box<dyn Error>> {
        self.branch_input(snapshot, &InputSpec::seeded(seed))
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
        coverage_schedule: Option<CoverageSchedule>,
    ) -> Result<(), Box<dyn Error>> {
        let spec = service_branch_spec(
            self.config.seed,
            config,
            payloads,
            effects,
            coverage_schedule,
        )?;
        self.branch_input(snapshot, &spec)
    }

    pub(super) fn branch_input(
        &mut self,
        snapshot: SnapId,
        spec: &InputSpec,
    ) -> Result<(), Box<dyn Error>> {
        branch_spec(&mut self.client, snapshot, spec)
    }

    #[must_use]
    pub fn abandoned(&self) -> bool {
        self.abandoned
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

    #[must_use]
    pub fn store_bytes(&self) -> u64 {
        self.client
            .transport()
            .snapshot_store_stats()
            .bytes_resident
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

    #[must_use]
    pub fn telemetry_counters(&self) -> Vec<(String, u64)> {
        let mut counters = self.client.transport().host_telemetry().counters();
        counters.extend(self.loaded_image_guard.counters("guard.session"));
        counters
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
        self.sdk_events_since(0)
    }

    pub fn sdk_events_since(&mut self, offset: u32) -> Result<Vec<SdkEvent>, Box<dyn Error>> {
        collect_sdk_events(offset, |offset| {
            let reply = self
                .client
                .request(&Request::SdkEvents { offset })
                .map_err(|error| SessionError::Control(error.to_string()))?;
            match reply {
                Reply::SdkEvents(page) => Ok(page),
                reply => Err(SessionError::Reply {
                    operation: "SDK events",
                    reply,
                }
                .into()),
            }
        })
    }

    pub fn read_observation(
        &mut self,
        handle: u32,
        offset: u32,
        len: u32,
    ) -> Result<Vec<u8>, Box<dyn Error>> {
        let events = self.sdk_events()?;
        let region = super::observation_descriptor(&events, handle)?;
        let address = region.range(offset, len)?;
        let mut bytes = Vec::with_capacity(len as usize);
        let mut consumed = 0;
        while consumed < len {
            let chunk = (len - consumed).min(control_proto::READ_CAP);
            bytes.extend(self.read(address + u64::from(consumed), chunk)?);
            consumed += chunk;
        }
        Ok(bytes)
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

    pub fn event_history(&mut self) -> Result<control_proto::EventHistory, Box<dyn Error>> {
        match self
            .client
            .request(&Request::EventHistory)
            .map_err(|error| SessionError::Control(error.to_string()))?
        {
            Reply::EventHistory(history) => Ok(history),
            reply => Err(SessionError::Reply {
                operation: "ordered event history",
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
        self.loaded_image_guard.check()?;
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
        self.loaded_image_guard.check()?;
        Ok(PortableSnapshot {
            setup: self.setup.0,
            image_identity: self.image_identity,
            at,
            pages,
            sidecar: sparse.sidecar,
        })
    }

    fn import_snapshot(&mut self, snapshot: &PortableSnapshot) -> Result<SnapId, Box<dyn Error>> {
        self.loaded_image_guard.check()?;
        let pages = snapshot
            .validated_pages(self.setup.0, self.image_identity, self.setup_at)
            .map_err(SessionError::Portable)?;
        let receipt = self
            .client
            .transport_mut()
            .import_sparse_snapshot_parts(self.setup, &pages, &snapshot.sidecar)
            .map_err(|error| SessionError::Portable(error.to_string()))?;
        self.loaded_image_guard.check()?;
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
    let (cancel, progress) = client.transport().vmm().map_or((None, None), |vmm| {
        (vmm.cancellation_flag(), vmm.virtual_time_progress())
    });
    let Some((limit, cancel, progress)) =
        guarded_run_plan(*abandoned, wall_limit, cancel, progress)?
    else {
        return client
            .request(request)
            .map_err(|error| SessionError::Control(error.to_string()).into());
    };
    let watchdog = Watchdog::start_with_progress(limit, cancel, progress).map_err(|error| {
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

    #[cfg(target_arch = "aarch64")]
    fn mock_arm_session(config: SessionConfig, image: &[u8]) -> Session {
        mock_arm_session_with_payloads(config, image, None)
    }

    #[cfg(target_arch = "aarch64")]
    fn mock_arm_session_with_payloads(
        config: SessionConfig,
        image: &[u8],
        setup_payloads: Option<Vec<Vec<u8>>>,
    ) -> Session {
        use vmm_backend::MockArm64Backend;
        use vmm_core::vendor::arm64::{board, bringup};
        use vmm_core::vmm::{Vmm, VmmError, VtimeWiring};

        let image_identity = image_identity_with_loaded_executable(
            image,
            b"",
            &config,
            setup_payloads.as_deref().unwrap_or(&[]),
            "test-sealed-host-v1",
        );
        let boot_config = config.clone();
        let boot_image = image.to_vec();
        let boot = move || -> Result<Vmm<Box<dyn Backend<A = HostArch>>>, VmmError> {
            let backend: Box<dyn Backend<A = HostArch>> = Box::new(MockArm64Backend::new());
            let mut vmm = bringup::boot(
                backend,
                &boot_image,
                &boot_config.cmdline,
                boot_config.ram_bytes,
            )?;
            vmm.wire_gic(board::new_gic());
            vmm.wire_vtime(VtimeWiring::new_virtual_time(
                vtime::VClockConfig {
                    guest_hz: board::CNTFRQ_HZ,
                    guest_base: 0,
                    vns_base: 0,
                },
                boot_config.seed,
            )?);
            vmm.enable_pvclock();
            vmm.wire_snapshot_hashing();
            Ok(vmm)
        };
        let live = boot().unwrap();
        let factory: VmmFactory<Box<dyn Backend<A = HostArch>>> = Box::new(boot);
        let mut client = Client::connect(ControlServer::new(live, factory), server_caps()).unwrap();
        if let Some(payloads) = setup_payloads {
            let genesis = snapshot_handle(&mut client, "genesis snapshot").unwrap();
            branch_payload(&mut client, genesis.id, payloads, config.seed).unwrap();
        }
        let setup = snapshot_handle(&mut client, "setup snapshot").unwrap();
        Session {
            client,
            loaded_image_guard: crate::image_identity::LoadedImageGuard::fixture(),
            setup: setup.id,
            setup_at: setup.at,
            image_identity,
            config,
            snapshot_times: BTreeMap::from([(setup.id, setup.at)]),
            abandoned: false,
        }
    }

    #[test]
    fn sdk_suffix_pagination_advances_from_the_requested_event_offset() {
        let mut offsets = Vec::new();
        let first = (10, 1, vec![1]);
        let second = (10, 2, vec![2]);
        let third = (20, 3, vec![3]);
        let expected = vec![first.clone(), second.clone(), third.clone()];
        let mut pages = vec![vec![first, second], vec![third], Vec::new()].into_iter();
        let events = collect_sdk_events(7, |offset| {
            offsets.push(offset);
            Ok(pages.next().unwrap())
        })
        .unwrap();
        assert_eq!(events, expected);
        assert_eq!(offsets, vec![7, 9, 10]);
        let mut calls = 0;
        assert!(
            collect_sdk_events(u32::MAX, |offset| {
                assert_eq!(offset, u32::MAX);
                calls += 1;
                Ok(Vec::new())
            })
            .unwrap()
            .is_empty()
        );
        assert_eq!(calls, 1);
    }

    #[test]
    fn sdk_suffix_pagination_refuses_overflow_and_discards_partial_fetches() {
        let event = (10, 1, vec![1]);
        let mut offsets = Vec::new();
        let error = collect_sdk_events(u32::MAX, |offset| {
            offsets.push(offset);
            Ok(vec![event.clone()])
        })
        .unwrap_err();
        assert!(error.to_string().contains("SDK event offset overflow"));
        assert_eq!(offsets, vec![u32::MAX]);
        let mut offsets = Vec::new();
        let error = collect_sdk_events(8, |offset| {
            offsets.push(offset);
            if offset == 8 {
                Ok(vec![event.clone()])
            } else {
                Err(std::io::Error::other("suffix fetch failed").into())
            }
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "suffix fetch failed");
        assert_eq!(offsets, vec![8, 9]);
        let events = collect_sdk_events(u32::MAX - 1, |offset| {
            if offset == u32::MAX - 1 {
                Ok(vec![event.clone()])
            } else {
                assert_eq!(offset, u32::MAX);
                Ok(Vec::new())
            }
        })
        .unwrap();
        assert_eq!(events, vec![event]);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn served_export_writes_once_into_the_allocated_destination() {
        use crate::cache::{
            LocalIndex,
            extent::{read_extent, trace_writes},
        };
        use vmm_core::vendor::arm64::image_loader;
        let image = image_loader::wrap_image(&[0x1f, 0x20, 0x03, 0xd5], 0, 0xa);
        let config = SessionConfig::new(16 * 1024 * 1024, 7, 19, "console=ttyAMA0");
        let mut source = mock_arm_session(config.clone(), &image);
        let index = LocalIndex::new(16 << 20);
        let attachment = SnapshotAttachment {
            domain: b"copy-proof".to_vec(),
            bytes: vec![9; 40],
        };
        let mut destination = mock_arm_session(config.clone(), &image);
        let mut chain = Vec::new();
        let mut parent = None;
        for (fill, count) in [(0, 8), (7, 8), (9, 1), (0, 8), (11, 8)] {
            let rows = (100..100 + count)
                .map(|gfn| (gfn, [fill; 4096]))
                .collect::<Vec<_>>();
            source
                .client
                .transport_mut()
                .vmm_mut()
                .unwrap()
                .write_guest_pages(&rows)
                .unwrap();
            let (target, _) = source.snapshot().unwrap();
            let before = source.client.transport().host_telemetry().exports.count;
            let ((mut extent, allocations), writes) = trace_writes(|| {
                super::super::worker::export_destination_for_test(
                    &source,
                    &index,
                    parent,
                    target,
                    &attachment,
                )
            });
            assert_eq!(
                source.client.transport().host_telemetry().exports.count,
                before + 1
            );
            assert_eq!(allocations.len(), 1);
            let actual = read_extent(extent.bytes_mut()).unwrap();
            assert_eq!(
                writes,
                vec![(
                    allocations[0].0,
                    allocations[0].1,
                    actual.pages.len() * 4096
                )]
            );
            let direct = source
                .write_delta_attached(
                    parent,
                    target,
                    &attachment,
                    |len| Ok(vec![0; len]),
                    Vec::as_mut_slice,
                )
                .unwrap();
            let expected = read_extent(&direct).unwrap();
            assert_eq!(actual.pages, expected.pages);
            assert_eq!(actual.reverted, expected.reverted);
            assert_eq!(actual.sidecar, expected.sidecar);
            assert_eq!(vmm_target_commitment(&mut extent).unwrap(), {
                let mut direct_extent = index.extent(direct.len()).unwrap();
                direct_extent.bytes_mut().copy_from_slice(&direct);
                vmm_target_commitment(&mut direct_extent).unwrap()
            });
            chain.push(extent.bytes_mut().to_vec());
            let refs = chain.iter().map(Vec::as_slice).collect::<Vec<_>>();
            let (restored, _) = destination
                .import_extents(&refs, destination.setup)
                .unwrap();
            destination.replay_snapshot(restored).unwrap();
            assert_eq!(
                destination.state_hash().unwrap(),
                source.state_hash().unwrap()
            );
            assert_eq!(
                destination.event_history().unwrap(),
                source.event_history().unwrap()
            );
            parent = Some(target);
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn sparse_import_rejects_another_loaded_executable_before_mutation() {
        use vmm_core::vendor::arm64::image_loader;

        let image = image_loader::wrap_image(&[0x1f, 0x20, 0x03, 0xd5], 0, 0xa);
        let config = SessionConfig::new(16 * 1024 * 1024, 7, 19, "console=ttyAMA0");
        let source = mock_arm_session(config.clone(), &image);
        let sparse = source.setup_sparse_snapshot().unwrap();
        let mut matched = mock_arm_session(config.clone(), &image);
        matched.import_sparse_snapshot(&sparse).unwrap();

        let mut foreign = mock_arm_session(config.clone(), &image);
        foreign.image_identity = image_identity_with_loaded_executable(
            &image,
            b"",
            &config,
            &[],
            "another-loaded-executable",
        );
        assert_eq!(source.setup, foreign.setup);
        assert_ne!(source.image_identity(), foreign.image_identity());
        let before_hash = foreign.state_hash().unwrap();
        let before_store = foreign.store_bytes();
        assert!(foreign.import_sparse_snapshot(&sparse).is_err());
        assert_eq!(foreign.store_bytes(), before_store);
        assert_eq!(foreign.state_hash().unwrap(), before_hash);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn tagged_sparse_import_rejects_foreign_setup_ram_before_mutation() {
        use vmm_core::vendor::arm64::image_loader;

        let image = image_loader::wrap_image(&[0x1f, 0x20, 0x03, 0xd5], 0, 0xa);
        let source_config = SessionConfig::new(16 * 1024 * 1024, 7, 19, "console=ttyAMA0")
            .with_identity_tag("same-workload");
        let mut destination_config = source_config.clone();
        destination_config.cmdline.push_str(" debug");
        let mut source = mock_arm_session(source_config, &image);
        let mut destination = mock_arm_session(destination_config, &image);
        assert_eq!(source.setup, destination.setup);
        assert_ne!(source.image_identity(), destination.image_identity());

        let source_base = source.client.transport().vmm().unwrap().guest_memory();
        let destination_base = destination.client.transport().vmm().unwrap().guest_memory();
        let different_base_gfns: Vec<_> = source_base
            .chunks_exact(PAGE_SIZE)
            .zip(destination_base.chunks_exact(PAGE_SIZE))
            .enumerate()
            .filter_map(|(gfn, (left, right))| (left != right).then_some(gfn as u64))
            .collect();
        assert!(!different_base_gfns.is_empty());

        let mut changed = source_base.to_vec();
        changed[10 * PAGE_SIZE..11 * PAGE_SIZE].fill(0xa5);
        source
            .client
            .transport_mut()
            .vmm_mut()
            .unwrap()
            .restore_guest_memory(&changed)
            .unwrap();
        let (target, _) = source.snapshot().unwrap();
        let source_hash = source.state_hash().unwrap();
        let sparse = source.export_sparse_snapshot(target, None).unwrap();
        assert!(
            different_base_gfns
                .iter()
                .any(|gfn| !sparse.pages().iter().any(|(changed, _)| changed == gfn))
        );

        let destination_hash = destination.state_hash().unwrap();
        let destination_store = destination.store_bytes();
        assert!(destination.import_sparse_snapshot(&sparse).is_err());
        assert_eq!(destination.store_bytes(), destination_store);
        assert_eq!(destination.state_hash().unwrap(), destination_hash);
        destination.replay_snapshot(destination.setup).unwrap();
        assert_eq!(destination.state_hash().unwrap(), destination_hash);

        let mut matched = mock_arm_session(source.config.clone(), &image);
        let imported = matched.import_sparse_snapshot(&sparse).unwrap();
        matched.replay_snapshot(imported).unwrap();
        assert_eq!(matched.state_hash().unwrap(), source_hash);

        let mut different_seed = source.config.clone();
        different_seed.seed += 1;
        let mut different_ram = source.config.clone();
        different_ram.ram_bytes += PAGE_SIZE;
        let mut different_budget = source.config.clone();
        different_budget.run_budget += 1;
        for config in [different_seed, different_ram, different_budget] {
            let mut foreign = mock_arm_session(config, &image);
            assert_eq!(source.setup, foreign.setup);
            assert_ne!(source.image_identity(), foreign.image_identity());
            let before_hash = foreign.state_hash().unwrap();
            let before_store = foreign.store_bytes();
            assert!(foreign.import_sparse_snapshot(&sparse).is_err());
            assert_eq!(foreign.store_bytes(), before_store);
            assert_eq!(foreign.state_hash().unwrap(), before_hash);
            foreign.replay_snapshot(foreign.setup).unwrap();
            assert_eq!(foreign.state_hash().unwrap(), before_hash);
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn rewritten_outer_identity_cannot_import_a_foreign_sparse_base() {
        use vmm_core::vendor::arm64::image_loader;

        let image = image_loader::wrap_image(&[0x1f, 0x20, 0x03, 0xd5], 0, 0xa);
        let source_config = SessionConfig::new(16 * 1024 * 1024, 7, 19, "console=ttyAMA0")
            .with_identity_tag("same-workload");
        let mut destination_config = source_config.clone();
        destination_config.cmdline.push_str(" debug");
        let mut source = mock_arm_session(source_config, &image);
        let mut destination = mock_arm_session(destination_config, &image);
        let source_base = source
            .client
            .transport()
            .vmm()
            .unwrap()
            .guest_memory()
            .to_vec();
        let destination_base = destination.client.transport().vmm().unwrap().guest_memory();
        assert_ne!(source_base, destination_base);
        let mut changed = source_base;
        changed[10 * PAGE_SIZE..11 * PAGE_SIZE].fill(0xa5);
        source
            .client
            .transport_mut()
            .vmm_mut()
            .unwrap()
            .restore_guest_memory(&changed)
            .unwrap();
        let (target, _) = source.snapshot().unwrap();
        let source_hash = source.state_hash().unwrap();
        let sparse = source.export_sparse_snapshot(target, None).unwrap();
        assert_ne!(sparse.image_identity(), destination.image_identity());
        let mut serialized = serde_json::to_value(&sparse).unwrap();
        serialized["image_identity"] = serde_json::json!(destination.image_identity());
        let forged: SparseSnapshot = serde_json::from_value(serialized).unwrap();
        let before_hash = destination.state_hash().unwrap();
        let before_store = destination.store_bytes();
        assert!(destination.import_sparse_snapshot(&forged).is_err());
        assert_eq!(destination.store_bytes(), before_store);
        assert_eq!(destination.state_hash().unwrap(), before_hash);
        destination.replay_snapshot(destination.setup).unwrap();
        assert_eq!(destination.state_hash().unwrap(), before_hash);

        let mut matched = mock_arm_session(source.config.clone(), &image);
        let imported = matched.import_sparse_snapshot(&sparse).unwrap();
        matched.replay_snapshot(imported).unwrap();
        assert_eq!(matched.state_hash().unwrap(), source_hash);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn foreign_cached_extent_rejects_image_identity_before_mutation() {
        use crate::cache::{CacheIndex, LocalIndex, Namespace};
        use vmm_core::vendor::arm64::image_loader;

        let image = image_loader::wrap_image(&[0x1f, 0x20, 0x03, 0xd5], 0, 0xa);
        let source_config = SessionConfig::new(16 * 1024 * 1024, 7, 19, "console=ttyAMA0");
        let mut destination_config = source_config.clone();
        destination_config.seed = 8;
        let mut source = mock_arm_session(source_config, &image);
        let mut destination = mock_arm_session(destination_config, &image);
        assert_ne!(source.image_identity(), destination.image_identity());
        assert_eq!(
            source.client.transport().vmm().unwrap().guest_memory(),
            destination.client.transport().vmm().unwrap().guest_memory()
        );
        let source_hash = source.state_hash().unwrap();
        let destination_hash = destination.state_hash().unwrap();
        assert_ne!(source_hash, destination_hash);
        let (target, _) = source.snapshot().unwrap();
        let index = LocalIndex::new(1 << 20);
        let namespace = Namespace::new(&[b"source"]);
        let attachment = SnapshotAttachment {
            domain: b"test-v1".to_vec(),
            bytes: b"actual captured evidence".to_vec(),
        };
        let lease = source
            .publish_snapshot_attached(&index, namespace, b"key", None, target, &attachment, 1)
            .unwrap();
        let before_store = destination.store_bytes();
        let before_handles = destination.snapshot_times.len();
        let rejected = destination
            .import_cached(&index, &lease, destination.setup)
            .unwrap_err();
        assert!(
            rejected
                .to_string()
                .contains("cached snapshot identity differs")
        );
        assert_eq!(destination.store_bytes(), before_store);
        assert_eq!(destination.snapshot_times.len(), before_handles);
        assert_eq!(destination.state_hash().unwrap(), destination_hash);
        assert_eq!(destination.client.transport().recorded_env().seed(), 8);
        destination.replay_snapshot(destination.setup).unwrap();
        assert_eq!(destination.state_hash().unwrap(), destination_hash);

        let (destination_target, _) = destination.snapshot().unwrap();
        let child_lease = destination
            .publish_snapshot(
                &index,
                namespace,
                b"key-child",
                Some((destination.setup, &lease)),
                destination_target,
                1,
            )
            .unwrap();
        let child_store = destination.store_bytes();
        let child_handles = destination.snapshot_times.len();
        let rejected = destination
            .import_cached(&index, &child_lease, destination.setup)
            .unwrap_err();
        assert!(
            rejected
                .to_string()
                .contains("cached snapshot identity differs")
        );
        assert_eq!(destination.store_bytes(), child_store);
        assert_eq!(destination.snapshot_times.len(), child_handles);
        assert_eq!(destination.state_hash().unwrap(), destination_hash);
        index.release(child_lease);

        let mut matched = mock_arm_session(source.config.clone(), &image);
        let receipt = matched
            .import_cached_receipt(&index, &lease, matched.setup)
            .unwrap();
        assert_eq!(receipt.attachment, attachment);
        assert_eq!(
            receipt.object_commitment,
            index.target_commitment(&lease).unwrap()
        );
        let imported = receipt.snapshot;
        matched.replay_snapshot(imported).unwrap();
        assert_eq!(matched.state_hash().unwrap(), source_hash);
        assert_eq!(matched.client.transport().recorded_env().seed(), 7);

        let (source_child_target, _) = source.snapshot().unwrap();
        let source_child_lease = source
            .publish_snapshot(
                &index,
                namespace,
                b"key-source-child",
                Some((target, &lease)),
                source_child_target,
                1,
            )
            .unwrap();
        let (imported_child, _) = matched
            .import_cached(&index, &source_child_lease, matched.setup)
            .unwrap();
        matched.replay_snapshot(imported_child).unwrap();
        assert_eq!(matched.state_hash().unwrap(), source_hash);
        index.release(source_child_lease);
        index.release(lease);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn setup_payload_identity_rejects_foreign_cached_extent() {
        use crate::cache::{LocalIndex, Namespace};
        use vmm_core::vendor::arm64::image_loader;

        let image = image_loader::wrap_image(&[0x1f, 0x20, 0x03, 0xd5], 0, 0xa);
        let config = SessionConfig::new(16 * 1024 * 1024, 7, 19, "console=ttyAMA0");
        let mut source = mock_arm_session_with_payloads(
            config.clone(),
            &image,
            Some(vec![b"alpha setup payload".to_vec()]),
        );
        let mut destination = mock_arm_session_with_payloads(
            config,
            &image,
            Some(vec![b"beta setup payload".to_vec()]),
        );
        assert_eq!(source.setup, destination.setup);
        assert_eq!(
            source.client.transport().vmm().unwrap().guest_memory(),
            destination.client.transport().vmm().unwrap().guest_memory()
        );
        let source_hash = source.state_hash().unwrap();
        let destination_hash = destination.state_hash().unwrap();
        let source_payloads = source
            .client
            .transport()
            .recorded_env()
            .payloads()
            .map(|entries| entries.to_vec());
        let destination_payloads = destination
            .client
            .transport()
            .recorded_env()
            .payloads()
            .map(|entries| entries.to_vec());
        assert_ne!(source_hash, destination_hash);
        assert_ne!(source_payloads, destination_payloads);
        let (target, _) = source.snapshot().unwrap();
        let index = LocalIndex::new(1 << 20);
        let lease = source
            .publish_snapshot(
                &index,
                Namespace::new(&[b"payload-probe"]),
                b"source",
                None,
                target,
                1,
            )
            .unwrap();
        let before_store = destination.store_bytes();
        let before_handles = destination.snapshot_times.len();
        let rejected = destination
            .import_cached(&index, &lease, destination.setup)
            .unwrap_err();
        assert!(
            rejected
                .to_string()
                .contains("cached snapshot identity differs")
        );
        assert_ne!(source.image_identity(), destination.image_identity());
        assert_eq!(destination.store_bytes(), before_store);
        assert_eq!(destination.snapshot_times.len(), before_handles);
        assert_eq!(destination.state_hash().unwrap(), destination_hash);
        assert_eq!(
            destination
                .client
                .transport()
                .recorded_env()
                .payloads()
                .map(|entries| entries.to_vec()),
            destination_payloads
        );
        destination.replay_snapshot(destination.setup).unwrap();
        assert_eq!(destination.state_hash().unwrap(), destination_hash);

        let mut matched = mock_arm_session_with_payloads(
            source.config.clone(),
            &image,
            Some(vec![b"alpha setup payload".to_vec()]),
        );
        assert_eq!(source.image_identity(), matched.image_identity());
        let (imported, _) = matched
            .import_cached(&index, &lease, matched.setup)
            .unwrap();
        matched.replay_snapshot(imported).unwrap();
        assert_eq!(matched.state_hash().unwrap(), source_hash);
        assert_eq!(
            matched
                .client
                .transport()
                .recorded_env()
                .payloads()
                .map(|entries| entries.to_vec()),
            source_payloads
        );
        index.release(lease);
    }

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

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn cached_import_retains_history_and_equal_endpoint_different_history_quarantines() {
        use crate::cache::{CacheError, LocalIndex};
        use vmm_core::vendor::arm64::image_loader;
        let image = image_loader::wrap_image(&[0x1f, 0x20, 0x03, 0xd5], 0, 0xa);
        let config = SessionConfig::new(16 * 1024 * 1024, 7, 19, "console=ttyAMA0");
        let mut source = mock_arm_session(config.clone(), &image);
        let original = source.setup_sparse_snapshot().unwrap();
        let endpoint = source.state_hash().unwrap();
        let original_history = source.event_history().unwrap();
        let changed = |byte| {
            let mut sidecar = original.sidecar.materialize();
            let start = sidecar.len() - 41;
            assert_eq!(sidecar[start], 1);
            sidecar[start + 1..start + 9].copy_from_slice(&2u64.to_le_bytes());
            sidecar[start + 9..].fill(byte);
            SparseSnapshot::from_parts(
                original.base,
                original.image_identity,
                original.pages.clone(),
                &sidecar,
                None,
            )
            .unwrap()
        };
        let first = source.import_sparse_snapshot(&changed(11)).unwrap();
        source.replay_snapshot(first).unwrap();
        assert_eq!(source.state_hash().unwrap(), endpoint);
        let expected = source.event_history().unwrap();
        assert_eq!(
            expected,
            control_proto::EventHistory {
                count: 2,
                digest: [11; 32]
            }
        );
        let index = LocalIndex::new(1 << 20);
        let namespace = Namespace::new(&[b"event-history"]);
        let lease = source
            .publish_snapshot(&index, namespace, b"same-actions", None, first, 1)
            .unwrap();
        let mut destination = mock_arm_session(config.clone(), &image);
        let (imported, _) = destination
            .import_cached(&index, &lease, destination.setup)
            .unwrap();
        destination.replay_snapshot(imported).unwrap();
        assert_eq!(destination.state_hash().unwrap(), endpoint);
        assert_eq!(destination.event_history().unwrap(), expected);
        destination.replay_snapshot(destination.setup).unwrap();
        assert_eq!(destination.event_history().unwrap(), original_history);
        let second = source.import_sparse_snapshot(&changed(12)).unwrap();
        source.replay_snapshot(second).unwrap();
        assert_eq!(source.state_hash().unwrap(), endpoint);
        assert_eq!(source.event_history().unwrap().count, expected.count);
        let error = source
            .publish_snapshot(&index, namespace, b"same-actions", None, second, 1)
            .unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<CacheError>(),
                Some(CacheError::DivergentDuplicate { .. })
            ),
            "{error}"
        );
        assert!(index.verify_lease(&lease).is_err());
        assert!(
            destination
                .import_cached(&index, &lease, destination.setup)
                .is_err()
        );
    }
}
