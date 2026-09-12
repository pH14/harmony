// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, fmt, sync::Arc, time::Duration};

use crate::{Client, Transport};
use control_proto::{Reply, SnapId, StopReason};
use environment::{
    channel::Effect,
    input_spec::{InputSpec, ServiceConfig},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const DEFAULT_RAM: usize = 128 * 1024 * 1024;

#[cfg(target_arch = "x86_64")]
const CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t tsc=reliable \
    no_timer_check lpj=4000000 random.trust_cpu=off nokaslr nosmp maxcpus=1 \
    nox2apic hpet=disable harmony_pvclock rdinit=/init";
#[cfg(target_arch = "aarch64")]
const CMDLINE: &str = "console=ttyAMA0 earlycon=pl011,0x09000000 rdinit=/init nohlt";

fn default_cmdline() -> &'static str {
    CMDLINE
}

const DEFAULT_SEED: u64 = 0;
const DEFAULT_RUN_BUDGET: u64 = 2_000_000_000;
pub const PAGE_SIZE: usize = 4096;
#[cfg(target_arch = "x86_64")]
pub const RAM_GPA_BASE: u64 = 0;
#[cfg(target_arch = "aarch64")]
pub const RAM_GPA_BASE: u64 = 0x4000_0000;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionConfig {
    pub ram_bytes: usize,
    pub seed: u64,
    pub run_budget: u64,
    pub cmdline: String,
    pub identity_tag: String,
    #[serde(default)]
    pub wall_limit: Option<Duration>,
    #[serde(default)]
    pub defer_virtual_time_checkpoint_hashes: bool,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            ram_bytes: DEFAULT_RAM,
            seed: DEFAULT_SEED,
            run_budget: DEFAULT_RUN_BUDGET,
            cmdline: default_cmdline().to_owned(),
            identity_tag: String::new(),
            wall_limit: None,
            defer_virtual_time_checkpoint_hashes: false,
        }
    }
}

impl SessionConfig {
    #[must_use]
    pub fn new(ram_bytes: usize, seed: u64, run_budget: u64, cmdline: impl Into<String>) -> Self {
        Self {
            ram_bytes,
            seed,
            run_budget,
            cmdline: cmdline.into(),
            identity_tag: String::new(),
            wall_limit: None,
            defer_virtual_time_checkpoint_hashes: false,
        }
    }

    #[must_use]
    pub fn with_identity_tag(mut self, identity_tag: impl Into<String>) -> Self {
        self.identity_tag = identity_tag.into();
        self
    }

    #[must_use]
    pub fn with_wall_limit(mut self, wall_limit: Duration) -> Self {
        self.wall_limit = Some(wall_limit);
        self
    }

    #[must_use]
    pub fn with_deferred_virtual_time_checkpoint_hashes(mut self) -> Self {
        self.defer_virtual_time_checkpoint_hashes = true;
        self
    }

    #[cfg_attr(
        not(all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64"),
            not(miri)
        )),
        allow(dead_code)
    )]
    fn validate(&self) -> Result<(), Box<dyn Error>> {
        if self.ram_bytes == 0 || !self.ram_bytes.is_multiple_of(PAGE_SIZE) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "session RAM must be a non-zero page multiple",
            )
            .into());
        }
        if self.run_budget == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "session run budget is zero",
            )
            .into());
        }
        if self.cmdline.trim().is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "session command line is empty",
            )
            .into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PortableSnapshot {
    pub setup: u64,
    pub image_identity: [u8; 32],
    pub at: u64,
    pub pages: Vec<(u64, Vec<u8>)>,
    pub sidecar: Vec<u8>,
}

const SHARED_STATE_CHUNK_SIZE: usize = 512;

#[derive(Debug, Eq, PartialEq)]
struct SharedStateInner {
    chunks: Vec<Arc<[u8; SHARED_STATE_CHUNK_SIZE]>>,
    len: usize,
}

#[derive(Clone)]
pub struct SharedState {
    inner: Arc<SharedStateInner>,
}

impl fmt::Debug for SharedState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SharedState")
            .field("len", &self.inner.len)
            .field("chunks", &self.inner.chunks.len())
            .finish()
    }
}

impl PartialEq for SharedState {
    fn eq(&self, other: &Self) -> bool {
        self.inner == other.inner
    }
}

impl Eq for SharedState {}

impl SharedState {
    fn from_bytes(bytes: Vec<u8>, base: Option<&Self>) -> Self {
        let mut chunks = Vec::with_capacity(bytes.len().div_ceil(SHARED_STATE_CHUNK_SIZE));
        for (index, source) in bytes.chunks(SHARED_STATE_CHUNK_SIZE).enumerate() {
            let mut chunk = [0_u8; SHARED_STATE_CHUNK_SIZE];
            chunk[..source.len()].copy_from_slice(source);
            let shared = base
                .and_then(|state| state.inner.chunks.get(index))
                .filter(|existing| existing.as_ref() == &chunk)
                .cloned();
            chunks.push(shared.unwrap_or_else(|| Arc::new(chunk)));
        }
        Self {
            inner: Arc::new(SharedStateInner {
                chunks,
                len: bytes.len(),
            }),
        }
    }

    fn materialize(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.inner.len);
        for chunk in &self.inner.chunks {
            let remaining = self.inner.len.saturating_sub(bytes.len());
            bytes.extend_from_slice(&chunk[..remaining.min(SHARED_STATE_CHUNK_SIZE)]);
        }
        bytes
    }

    fn memory_charge(&self) -> usize {
        self.inner
            .chunks
            .len()
            .saturating_mul(SHARED_STATE_CHUNK_SIZE)
    }
}

impl Serialize for SharedState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.materialize().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SharedState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Vec::<u8>::deserialize(deserializer).map(|bytes| Self::from_bytes(bytes, None))
    }
}

pub struct SparseSnapshot {
    base: u64,
    image_identity: [u8; 32],
    pages: Vec<(u64, Arc<[u8; 4096]>)>,
    sidecar: SharedState,
}

impl Clone for SparseSnapshot {
    fn clone(&self) -> Self {
        Self {
            base: self.base,
            image_identity: self.image_identity,
            pages: self.pages.clone(),
            sidecar: self.sidecar.clone(),
        }
    }
}

impl fmt::Debug for SparseSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SparseSnapshot")
            .field("base", &self.base)
            .field("image_identity", &self.image_identity)
            .field("pages", &self.pages.len())
            .field("sidecar", &self.sidecar)
            .finish()
    }
}

impl PartialEq for SparseSnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.base == other.base
            && self.image_identity == other.image_identity
            && self.pages == other.pages
            && self.sidecar == other.sidecar
    }
}

impl Eq for SparseSnapshot {}

impl SparseSnapshot {
    pub fn from_parts(
        base: u64,
        image_identity: [u8; 32],
        pages: Vec<(u64, Arc<[u8; 4096]>)>,
        sidecar: &[u8],
        export_base: Option<&Self>,
    ) -> Result<Self, String> {
        if let Some(export_base) = export_base
            && (export_base.base != base || export_base.image_identity != image_identity)
        {
            return Err("portable snapshot setup/base identity mismatch".into());
        }
        validate_sparse_pages(&pages)?;
        let pages = pages
            .into_iter()
            .map(|(gfn, page)| {
                let shared = export_base
                    .and_then(|portable| {
                        portable
                            .pages
                            .binary_search_by_key(&gfn, |(existing, _)| *existing)
                            .ok()
                            .and_then(|index| portable.pages.get(index))
                    })
                    .filter(|(_, existing)| existing.as_ref() == page.as_ref())
                    .map(|(_, existing)| Arc::clone(existing));
                (gfn, shared.unwrap_or(page))
            })
            .collect();
        Ok(Self {
            base,
            image_identity,
            pages,
            sidecar: SharedState::from_bytes(
                sidecar.to_vec(),
                export_base.map(|portable| &portable.sidecar),
            ),
        })
    }

    #[must_use]
    pub fn base(&self) -> u64 {
        self.base
    }

    #[must_use]
    pub fn setup(&self) -> u64 {
        self.base
    }

    #[must_use]
    pub fn image_identity(&self) -> [u8; 32] {
        self.image_identity
    }

    #[must_use]
    pub fn pages(&self) -> &[(u64, Arc<[u8; 4096]>)] {
        &self.pages
    }

    #[must_use]
    pub fn sidecar(&self) -> Vec<u8> {
        self.sidecar.materialize()
    }

    #[must_use]
    pub fn memory_charge(&self) -> usize {
        self.pages
            .len()
            .saturating_mul(4096)
            .saturating_add(self.sidecar.memory_charge())
    }
}

#[derive(Deserialize, Serialize)]
struct SparseSnapshotWire {
    base: u64,
    image_identity: [u8; 32],
    pages: Vec<SparsePageWire>,
    sidecar: SharedState,
}

#[derive(Deserialize, Serialize)]
struct SparsePageWire {
    gfn: u64,
    bytes: Vec<u8>,
}

impl Serialize for SparseSnapshot {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        SparseSnapshotWire {
            base: self.base,
            image_identity: self.image_identity,
            pages: self
                .pages
                .iter()
                .map(|(gfn, page)| SparsePageWire {
                    gfn: *gfn,
                    bytes: page.to_vec(),
                })
                .collect(),
            sidecar: self.sidecar.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SparseSnapshot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = SparseSnapshotWire::deserialize(deserializer)?;
        let mut pages = Vec::new();
        pages.try_reserve(wire.pages.len()).map_err(|error| {
            serde::de::Error::custom(format!("portable pages allocation failed: {error}"))
        })?;
        for page in wire.pages {
            if page.bytes.len() != 4096 {
                return Err(serde::de::Error::custom(
                    "portable page is not exactly 4096 bytes",
                ));
            }
            pages.push((
                page.gfn,
                Arc::new(
                    page.bytes
                        .try_into()
                        .map_err(|_| serde::de::Error::custom("portable page conversion failed"))?,
                ),
            ));
        }
        validate_sparse_pages(&pages).map_err(serde::de::Error::custom)?;
        Ok(Self {
            base: wire.base,
            image_identity: wire.image_identity,
            pages,
            sidecar: wire.sidecar,
        })
    }
}

fn validate_sparse_pages(pages: &[(u64, Arc<[u8; 4096]>)]) -> Result<(), String> {
    if pages.windows(2).any(|window| window[0].0 >= window[1].0) {
        return Err("portable pages are not strictly sorted".into());
    }
    Ok(())
}

impl PortableSnapshot {
    #[must_use]
    pub fn at(&self) -> u64 {
        self.at
    }

    #[must_use]
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    #[cfg_attr(
        not(all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64"),
            not(miri)
        )),
        allow(dead_code)
    )]
    fn validated_pages(
        &self,
        setup: u64,
        image_identity: [u8; 32],
        setup_at: u64,
    ) -> Result<SparsePages, String> {
        if self.setup != setup {
            return Err("setup snapshot identity differs".into());
        }
        if self.image_identity != image_identity {
            return Err("image identity differs".into());
        }
        if self.at < setup_at {
            return Err("portable V-time predates setup".into());
        }
        let mut pages = SparsePages::with_capacity(self.pages.len());
        let mut previous = None;
        for (gfn, bytes) in &self.pages {
            if previous.is_some_and(|value| value >= *gfn) {
                return Err("sparse pages are not strictly sorted".into());
            }
            let page: [u8; PAGE_SIZE] = bytes
                .as_slice()
                .try_into()
                .map_err(|_| "sparse page has the wrong length".to_owned())?;
            pages.push((*gfn, Arc::new(page)));
            previous = Some(*gfn);
        }
        Ok(pages)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("control request {operation} returned an unexpected reply: {reply:?}")]
    Reply {
        operation: &'static str,
        reply: Reply,
    },
    #[error("control session: {0}")]
    Control(String),
    #[error("portable snapshot: {0}")]
    Portable(String),
    #[error("guest stopped before the expected snapshot point: {0:?}")]
    Stop(StopReason),
    #[error("guest ran for more than {0:?} of host time without exiting")]
    Hung(Duration),
    #[error("session was abandoned after a guest hang and cannot run again")]
    Abandoned,
    #[error("backend cannot be interrupted mid-run, so its runs cannot be wall-clock bounded")]
    Unboundable,
}

type CancelLatch = Arc<std::sync::atomic::AtomicBool>;

type GuardedRun = (Duration, CancelLatch);

#[cfg_attr(
    not(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    )),
    allow(dead_code)
)]
fn guarded_run_plan(
    abandoned: bool,
    wall_limit: Option<Duration>,
    cancel: Option<CancelLatch>,
) -> Result<Option<GuardedRun>, Box<dyn Error>> {
    if abandoned {
        return Err(SessionError::Abandoned.into());
    }
    let Some(limit) = wall_limit else {
        return Ok(None);
    };
    let cancel = cancel.ok_or(SessionError::Unboundable)?;
    Ok(Some((limit, cancel)))
}

#[cfg_attr(
    not(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    )),
    allow(dead_code)
)]
fn service_branch_spec(
    seed: u64,
    config: ServiceConfig,
    payloads: Vec<Vec<u8>>,
    effects: Vec<(u64, Effect)>,
) -> Result<InputSpec, Box<dyn Error>> {
    let mut spec = InputSpec::seeded(seed);
    spec.set_config(config);
    spec.set_payloads(Some(payloads));
    for (at, effect) in effects {
        if spec.effects().contains_key(&at) {
            return Err(
                SessionError::Control(format!("two branch effects share moment {at}")).into(),
            );
        }
        spec.record_effect(at, effect);
    }
    Ok(spec)
}

#[cfg_attr(
    not(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    )),
    allow(dead_code)
)]
#[derive(Debug, Eq, PartialEq)]
struct SnapshotReceipt {
    id: SnapId,
    at: u64,
}

#[cfg_attr(
    not(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    )),
    allow(dead_code)
)]
fn snapshot_handle<T: Transport>(
    client: &mut Client<T>,
    operation: &'static str,
) -> Result<SnapshotReceipt, Box<dyn Error>> {
    let reply = client
        .request(&control_proto::Request::Snapshot)
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
            let _ = drop_control_handle(client, id);
            Err(error)
        }
        reply => Err(SessionError::Reply { operation, reply }.into()),
    }
}

#[cfg_attr(
    not(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    )),
    allow(dead_code)
)]
fn drop_control_handle<T: Transport>(
    client: &mut Client<T>,
    handle: SnapId,
) -> Result<(), Box<dyn Error>> {
    let reply = client
        .request(&control_proto::Request::Drop(handle))
        .map_err(|error| SessionError::Control(error.to_string()))?;
    expect_unit(reply, "drop snapshot")
}

#[cfg_attr(
    not(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    )),
    allow(dead_code)
)]
fn expect_unit(reply: Reply, operation: &'static str) -> Result<(), Box<dyn Error>> {
    match reply {
        Reply::Unit => Ok(()),
        reply => Err(SessionError::Reply { operation, reply }.into()),
    }
}

#[cfg_attr(
    not(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    )),
    allow(dead_code)
)]
const MAX_CONSOLE_DIAGNOSTIC: usize = 64 * 1024;

#[cfg_attr(
    not(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    )),
    allow(dead_code)
)]
fn drain_console_pages<F>(mut request: F) -> Result<Vec<u8>, Box<dyn Error>>
where
    F: FnMut(u32) -> Result<Reply, Box<dyn Error>>,
{
    let mut console = Vec::new();
    let mut offset = 0_u32;
    loop {
        let reply = request(offset)?;
        let Reply::Console { total, chunk } = reply else {
            return Err(SessionError::Reply {
                operation: "console diagnostic",
                reply,
            }
            .into());
        };
        if chunk.is_empty() {
            break;
        }
        let remaining = MAX_CONSOLE_DIAGNOSTIC.saturating_sub(console.len());
        console.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        if console.len() == MAX_CONSOLE_DIAGNOSTIC {
            break;
        }
        offset = console.len() as u32;
        if offset >= total {
            break;
        }
    }
    Ok(console)
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
mod live;
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
pub use live::{Session, host_minor_faults};

pub type SdkEvent = (u64, u32, Vec<u8>);
#[cfg_attr(
    not(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    )),
    allow(dead_code)
)]
type SparsePage = (u64, Arc<[u8; PAGE_SIZE]>);
#[cfg_attr(
    not(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    )),
    allow(dead_code)
)]
type SparsePages = Vec<SparsePage>;

#[must_use]
pub fn identity(kernel: &[u8], initramfs: &[u8]) -> String {
    identity_with_config(kernel, initramfs, &SessionConfig::default())
}

#[must_use]
pub fn identity_with_config(kernel: &[u8], initramfs: &[u8], config: &SessionConfig) -> String {
    format!(
        "consonance-session-v2;image-sha256={};ram-bytes={};seed={};run-budget={};cmdline-sha256={:x};identity-tag={};sdk-input=payload-v1;snapshot=sparse-pages-plus-sidecar-v3",
        bytes_hex(&image_identity_with_config(kernel, initramfs, config)),
        config.ram_bytes,
        config.seed,
        config.run_budget,
        Sha256::digest(config.cmdline.as_bytes()),
        config.identity_tag,
    )
}

fn image_identity(kernel: &[u8], initramfs: &[u8]) -> [u8; 32] {
    image_identity_with_config(kernel, initramfs, &SessionConfig::default())
}

fn image_identity_with_config(kernel: &[u8], initramfs: &[u8], config: &SessionConfig) -> [u8; 32] {
    let mut digest = Sha256::new();
    if config.identity_tag.is_empty() {
        digest.update(b"consonance-session-image-v2\0");
        update_digest_field(&mut digest, kernel);
        update_digest_field(&mut digest, initramfs);
        digest.update(config.ram_bytes.to_le_bytes());
        digest.update(config.seed.to_le_bytes());
        digest.update(config.run_budget.to_le_bytes());
        update_digest_field(&mut digest, config.cmdline.as_bytes());
    } else {
        digest.update(config.identity_tag.as_bytes());
        update_digest_field(&mut digest, kernel);
        update_digest_field(&mut digest, initramfs);
    }
    digest.finalize().into()
}

fn update_digest_field(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
}

#[must_use]
pub fn image_identity_hex(kernel: &[u8], initramfs: &[u8]) -> String {
    bytes_hex(&image_identity(kernel, initramfs))
}

fn bytes_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::VecDeque, io};

    #[derive(Debug)]
    struct SnapshotTransport {
        requests: Vec<control_proto::Request>,
        replies: VecDeque<Result<Result<Reply, control_proto::ControlError>, io::Error>>,
    }

    impl Transport for SnapshotTransport {
        type Error = io::Error;

        fn exchange(
            &mut self,
            request: &control_proto::Request,
        ) -> Result<Result<Reply, control_proto::ControlError>, Self::Error> {
            self.requests.push(request.clone());
            self.replies
                .pop_front()
                .unwrap_or_else(|| Err(io::Error::other("connection closed")))
        }
    }

    fn test_caps() -> control_proto::Caps {
        control_proto::Caps {
            protocol_version: control_proto::APP_PROTOCOL_VERSION,
            env_version_min: 5,
            env_version_max: 5,
            coverage: Default::default(),
            flags: Default::default(),
        }
    }

    fn snapshot_client(
        replies: impl IntoIterator<Item = Result<Result<Reply, control_proto::ControlError>, io::Error>>,
    ) -> Client<SnapshotTransport> {
        let mut replies = VecDeque::from_iter(replies);
        replies.push_front(Ok(Ok(Reply::Hello(test_caps()))));
        Client::connect(
            SnapshotTransport {
                requests: Vec::new(),
                replies,
            },
            test_caps(),
        )
        .expect("test transport handshake")
    }

    fn assert_one_snapshot_without_run(requests: &[control_proto::Request]) {
        assert_eq!(
            requests
                .iter()
                .filter(|request| matches!(request, control_proto::Request::Snapshot))
                .count(),
            1,
            "snapshot request sequence: {requests:?}"
        );
        assert!(
            requests
                .iter()
                .all(|request| !matches!(request, control_proto::Request::Run { .. })),
            "exact snapshot must not retry through Run: {requests:?}"
        );
    }

    #[test]
    fn snapshot_handle_preserves_the_control_receipt() {
        let mut client = snapshot_client([Ok(Ok(Reply::Snapshot {
            id: SnapId(7),
            at: control_proto::Moment(42),
            sdk_events: 3,
            tainted: false,
        }))]);

        let receipt = snapshot_handle(&mut client, "snapshot").expect("snapshot receipt");

        assert_eq!(
            receipt,
            SnapshotReceipt {
                id: SnapId(7),
                at: 42
            }
        );
        assert_one_snapshot_without_run(&client.transport().requests);
    }

    #[test]
    fn snapshot_handle_reports_not_quiescent_without_retrying() {
        let mut client = snapshot_client([Ok(Err(control_proto::ControlError::NotQuiescent))]);

        let error =
            snapshot_handle(&mut client, "snapshot").expect_err("not-quiescent snapshot must fail");

        assert!(error.to_string().contains("NotQuiescent"));
        assert_one_snapshot_without_run(&client.transport().requests);
    }

    #[test]
    fn snapshot_handle_preserves_snapshot_refusal_diagnostics() {
        let mut client = snapshot_client([Ok(Err(control_proto::ControlError::SnapshotRefused {
            reason: "pvclock record unavailable".into(),
        }))]);

        let error =
            snapshot_handle(&mut client, "snapshot").expect_err("refused snapshot must fail");

        assert!(error.to_string().contains("pvclock record unavailable"));
        assert_one_snapshot_without_run(&client.transport().requests);
    }

    #[test]
    fn snapshot_handle_reports_transport_failures_without_retrying() {
        let mut client = snapshot_client([Err(io::Error::other("socket closed"))]);

        let error = snapshot_handle(&mut client, "snapshot")
            .expect_err("transport failure must fail the snapshot");

        assert!(error.to_string().contains("socket closed"));
        assert_one_snapshot_without_run(&client.transport().requests);
    }

    #[test]
    fn tainted_snapshot_drops_its_minted_handle() {
        let mut client = snapshot_client([
            Ok(Ok(Reply::Snapshot {
                id: SnapId(31),
                at: control_proto::Moment(99),
                sdk_events: 0,
                tainted: true,
            })),
            Ok(Ok(Reply::Unit)),
        ]);

        let error = snapshot_handle(&mut client, "snapshot")
            .expect_err("tainted snapshot must be rejected");

        assert!(error.to_string().contains("snapshot was tainted"));
        assert_eq!(
            client.transport().requests.last(),
            Some(&control_proto::Request::Drop(SnapId(31)))
        );
        assert_one_snapshot_without_run(&client.transport().requests);
    }

    #[test]
    fn deferred_checkpoint_hashing_is_opt_in_and_off_by_default() {
        let plain = SessionConfig::new(PAGE_SIZE, 1, 2, "cmdline");
        assert!(!plain.defer_virtual_time_checkpoint_hashes);
        assert!(!SessionConfig::default().defer_virtual_time_checkpoint_hashes);
        let deferred = plain.clone().with_deferred_virtual_time_checkpoint_hashes();
        assert!(deferred.defer_virtual_time_checkpoint_hashes);
        assert_eq!(
            SessionConfig {
                defer_virtual_time_checkpoint_hashes: false,
                ..deferred.clone()
            },
            plain,
            "the option changes nothing else about the launch settings"
        );
    }

    #[test]
    fn deferred_checkpoint_hashing_stays_out_of_execution_identity() {
        let plain = SessionConfig::new(PAGE_SIZE, 1, 2, "cmdline");
        let deferred = plain.clone().with_deferred_virtual_time_checkpoint_hashes();
        assert_eq!(
            identity_with_config(b"kernel", b"initramfs", &deferred),
            identity_with_config(b"kernel", b"initramfs", &plain)
        );
        assert_eq!(
            image_identity_with_config(b"kernel", b"initramfs", &deferred),
            image_identity_with_config(b"kernel", b"initramfs", &plain)
        );
        let tagged = plain.clone().with_identity_tag("workload");
        assert_ne!(
            identity_with_config(b"kernel", b"initramfs", &tagged),
            identity_with_config(b"kernel", b"initramfs", &plain),
            "a setting that does reach identity still moves it"
        );
    }

    #[test]
    fn a_config_without_the_option_decodes_with_it_off() {
        let json = serde_json::json!({
            "ram_bytes": PAGE_SIZE,
            "seed": 1,
            "run_budget": 2,
            "cmdline": "cmdline",
            "identity_tag": "",
        })
        .to_string();
        let decoded: SessionConfig = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded, SessionConfig::new(PAGE_SIZE, 1, 2, "cmdline"));
        let deferred = decoded.with_deferred_virtual_time_checkpoint_hashes();
        let round_tripped: SessionConfig =
            serde_json::from_str(&serde_json::to_string(&deferred).expect("encode"))
                .expect("decode");
        assert_eq!(round_tripped, deferred);
    }

    #[test]
    fn console_pages_are_drained_for_diagnostics() {
        let mut offsets = Vec::new();
        let console = drain_console_pages(|offset| {
            offsets.push(offset);
            Ok(match offset {
                0 => Reply::Console {
                    total: 10,
                    chunk: b"guest".to_vec(),
                },
                5 => Reply::Console {
                    total: 10,
                    chunk: b" boot".to_vec(),
                },
                10 => Reply::Console {
                    total: 10,
                    chunk: Vec::new(),
                },
                other => panic!("unexpected console offset {other}"),
            })
        })
        .expect("console pages");
        assert_eq!(offsets, [0, 5]);
        assert_eq!(console, b"guest boot");
    }

    #[test]
    fn console_pages_are_bounded() {
        const EXPECTED_CAP: usize = 64 * 1024;
        let mut calls = 0;
        let bounded = drain_console_pages(|_| {
            calls += 1;
            if calls == 1 {
                Ok(Reply::Console {
                    total: u32::MAX,
                    chunk: vec![b'x'; EXPECTED_CAP],
                })
            } else {
                Err(std::io::Error::other("unexpected second console page").into())
            }
        })
        .expect("bounded console");
        assert_eq!(bounded.len(), EXPECTED_CAP);
        assert_eq!(calls, 1);
    }

    #[test]
    fn console_pages_stop_on_empty_page() {
        let mut calls = 0;
        let console = drain_console_pages(|_| {
            calls += 1;
            Ok(Reply::Console {
                total: 100,
                chunk: Vec::new(),
            })
        })
        .expect("empty console");
        assert!(console.is_empty());
        assert_eq!(calls, 1);
    }

    #[test]
    fn console_page_errors_preserve_transport_and_reply_failures() {
        let transport = drain_console_pages(|_| {
            Err::<Reply, Box<dyn Error>>(std::io::Error::other("transport").into())
        })
        .expect_err("transport failure");
        assert_eq!(transport.to_string(), "transport");

        let reply = drain_console_pages(|_| Ok(Reply::Unit)).expect_err("reply failure");
        assert!(reply.to_string().contains("console diagnostic"));
    }

    #[test]
    fn image_identity_is_ordered_and_hex_stable() {
        assert_eq!(image_identity_hex(b"a", b"b").len(), 64);
        assert_ne!(image_identity(b"a", b"b"), image_identity(b"b", b"a"));
        assert_eq!(image_identity(b"a", b"b"), image_identity(b"a", b"b"));
        assert_eq!(
            identity(b"a", b"b"),
            identity_with_config(b"a", b"b", &SessionConfig::default())
        );
        assert!(identity(b"a", b"b").contains("consonance-session-v2"));
    }

    #[test]
    fn session_config_is_validated_and_part_of_identity() {
        let default = SessionConfig::default();
        assert_eq!(default.ram_bytes, 128 * 1024 * 1024);
        assert_eq!(default.cmdline, default_cmdline());
        assert!(!default.cmdline.trim().is_empty());
        assert!(default.cmdline.contains("rdinit=/init"));
        #[cfg(target_arch = "aarch64")]
        assert!(default.cmdline.starts_with("console=ttyAMA0"));
        #[cfg(target_arch = "x86_64")]
        assert!(default.cmdline.starts_with("console=ttyS0"));
        let mut changed = default.clone();
        changed.seed = changed.seed.wrapping_add(1);
        assert_ne!(
            identity_with_config(b"kernel", b"initramfs", &default),
            identity_with_config(b"kernel", b"initramfs", &changed)
        );

        changed.ram_bytes = 0;
        assert!(changed.validate().is_err());
        changed.ram_bytes = PAGE_SIZE - 1;
        assert!(changed.validate().is_err());
        changed.ram_bytes = PAGE_SIZE;
        changed.run_budget = 0;
        assert!(changed.validate().is_err());
        changed.run_budget = 1;
        changed.cmdline.clear();
        assert!(changed.validate().is_err());
    }

    #[test]
    fn session_config_identity_tag_changes_domain_without_dropping_image_bytes() {
        let default = SessionConfig::default();
        let tagged = default.clone().with_identity_tag("nes");
        assert_eq!(tagged.identity_tag, "nes");
        assert_ne!(
            identity_with_config(b"kernel", b"initramfs", &default),
            identity_with_config(b"kernel", b"initramfs", &tagged)
        );
        assert_ne!(
            identity_with_config(b"kernel-a", b"initramfs", &tagged),
            identity_with_config(b"kernel-b", b"initramfs", &tagged)
        );
    }

    #[test]
    fn shared_state_debug_equality_and_chunk_reuse_are_observable() {
        let base = SharedState::from_bytes(vec![1; SHARED_STATE_CHUNK_SIZE + 1], None);
        let same = SharedState::from_bytes(vec![1; SHARED_STATE_CHUNK_SIZE + 1], Some(&base));
        let different = SharedState::from_bytes(vec![2; SHARED_STATE_CHUNK_SIZE + 1], Some(&base));

        assert_eq!(base, same);
        assert_ne!(base, different);
        assert!(Arc::ptr_eq(&base.inner.chunks[0], &same.inner.chunks[0]));
        assert!(!Arc::ptr_eq(
            &base.inner.chunks[0],
            &different.inner.chunks[0]
        ));
        let debug = format!("{base:?}");
        assert!(debug.contains("SharedState"));
        assert!(debug.contains("len"));
        assert!(debug.contains("chunks"));
    }

    #[test]
    fn portable_snapshot_validates_pages_and_identity_without_vm_access() {
        let identity = [7; 32];
        let valid = PortableSnapshot {
            setup: 1,
            image_identity: identity,
            at: 4,
            pages: vec![(0, vec![0; PAGE_SIZE])],
            sidecar: Vec::new(),
        };
        assert_eq!(valid.page_count(), 1);
        assert_eq!(valid.at(), 4);
        assert_eq!(valid.validated_pages(1, identity, 3).unwrap().len(), 1);
        assert_eq!(
            (PortableSnapshot {
                pages: Vec::new(),
                ..valid.clone()
            })
            .page_count(),
            0
        );
        assert!(valid.validated_pages(1, identity, 4).is_ok());

        let mut wrong_length = valid.clone();
        wrong_length.pages[0].1.pop();
        assert_eq!(
            wrong_length.validated_pages(1, identity, 3).unwrap_err(),
            "sparse page has the wrong length"
        );

        let mut unsorted = valid.clone();
        unsorted.pages = vec![(2, vec![0; PAGE_SIZE]), (1, vec![0; PAGE_SIZE])];
        assert_eq!(
            unsorted.validated_pages(1, identity, 3).unwrap_err(),
            "sparse pages are not strictly sorted"
        );
        assert_eq!(
            valid.validated_pages(2, identity, 3).unwrap_err(),
            "setup snapshot identity differs"
        );
        assert_eq!(
            valid.validated_pages(1, [8; 32], 3).unwrap_err(),
            "image identity differs"
        );
        assert_eq!(
            valid.validated_pages(1, identity, 5).unwrap_err(),
            "portable V-time predates setup"
        );
    }

    #[test]
    fn sparse_snapshot_reuses_unchanged_pages_and_sidecar_chunks() {
        let base_page = Arc::new([0xabu8; PAGE_SIZE]);
        let base = SparseSnapshot::from_parts(
            7,
            [3; 32],
            vec![(11, Arc::clone(&base_page))],
            &[9; SHARED_STATE_CHUNK_SIZE + 1],
            None,
        )
        .expect("base sparse snapshot");
        let child = SparseSnapshot::from_parts(
            7,
            [3; 32],
            vec![(11, Arc::new([0xabu8; PAGE_SIZE]))],
            &[9; SHARED_STATE_CHUNK_SIZE + 1],
            Some(&base),
        )
        .expect("child sparse snapshot");

        assert!(Arc::ptr_eq(&base.pages()[0].1, &child.pages()[0].1));
        assert_eq!(
            child.memory_charge(),
            PAGE_SIZE + 2 * SHARED_STATE_CHUNK_SIZE
        );
        assert_eq!(child.sidecar(), vec![9; SHARED_STATE_CHUNK_SIZE + 1]);
        assert!(Arc::ptr_eq(
            &base.sidecar.inner.chunks[0],
            &child.sidecar.inner.chunks[0]
        ));

        let debug = format!("{child:?}");
        assert!(debug.contains("SparseSnapshot"));
        assert!(debug.contains("base"));
        assert!(debug.contains("pages"));
        assert!(debug.contains("sidecar"));
    }

    #[test]
    fn sparse_snapshot_equality_checks_every_wire_field() {
        let make = || {
            SparseSnapshot::from_parts(
                7,
                [3; 32],
                vec![(11, Arc::new([0xabu8; PAGE_SIZE]))],
                &[9; SHARED_STATE_CHUNK_SIZE + 1],
                None,
            )
            .expect("sparse snapshot")
        };
        let equal = make();
        assert_eq!(equal, make());

        let mut different_base = make();
        different_base.base += 1;
        assert_ne!(equal, different_base);

        let mut different_identity = make();
        different_identity.image_identity[0] ^= 1;
        assert_ne!(equal, different_identity);

        let mut different_pages = make();
        different_pages.pages[0].0 += 1;
        assert_ne!(equal, different_pages);

        let mut different_sidecar = make();
        different_sidecar.sidecar = SharedState::from_bytes(vec![8], None);
        assert_ne!(equal, different_sidecar);

        assert_eq!(equal.base(), 7);
        assert_eq!(equal.setup(), 7);
        assert_eq!(equal.image_identity(), [3; 32]);

        assert!(
            SparseSnapshot::from_parts(
                8,
                [3; 32],
                vec![(11, Arc::new([0xabu8; PAGE_SIZE]))],
                &[],
                Some(&equal),
            )
            .is_err()
        );
        assert!(
            SparseSnapshot::from_parts(
                7,
                [4; 32],
                vec![(11, Arc::new([0xabu8; PAGE_SIZE]))],
                &[],
                Some(&equal),
            )
            .is_err()
        );

        let encoded = serde_json::to_value(&equal).expect("serialize sparse snapshot");
        let decoded: SparseSnapshot =
            serde_json::from_value(encoded).expect("deserialize sparse snapshot");
        assert_eq!(equal, decoded);
    }

    fn service_config(identity: &[u8]) -> ServiceConfig {
        ServiceConfig {
            identity: identity.to_vec(),
            configuration: b"configuration".to_vec(),
        }
    }

    #[test]
    fn a_service_branch_records_its_configuration_payloads_and_effects() {
        let spec = service_branch_spec(
            7,
            service_config(b"package"),
            vec![b"first".to_vec(), b"second".to_vec()],
            vec![
                (900, Effect::InjectInterrupt { vector: 33 }),
                (100, Effect::InjectInterrupt { vector: 32 }),
            ],
        )
        .expect("distinct moments");
        assert_eq!(spec.seed(), 7);
        assert_eq!(spec.config(), &service_config(b"package"));
        assert_eq!(
            spec.payloads(),
            Some([b"first".to_vec(), b"second".to_vec()].as_slice()),
        );
        assert_eq!(
            spec.effects().iter().collect::<Vec<_>>(),
            [
                (&100, &Effect::InjectInterrupt { vector: 32 }),
                (&900, &Effect::InjectInterrupt { vector: 33 }),
            ],
            "effects reach the branch in moment order whatever order they were given in",
        );
    }

    #[test]
    fn two_branch_effects_at_one_moment_are_reported_rather_than_dropped() {
        let error = service_branch_spec(
            0,
            service_config(b"package"),
            Vec::new(),
            vec![
                (100, Effect::InjectInterrupt { vector: 32 }),
                (100, Effect::InjectInterrupt { vector: 33 }),
            ],
        )
        .expect_err("one moment carries one effect");
        assert!(
            error.to_string().contains("share moment 100"),
            "unexpected error: {error}",
        );
    }

    #[test]
    fn a_service_branch_without_effects_records_an_empty_schedule() {
        let spec = service_branch_spec(0, service_config(b"package"), Vec::new(), Vec::new())
            .expect("no effects");
        assert!(spec.effects().is_empty());
        assert_eq!(spec.payloads(), Some([].as_slice()));
    }

    #[test]
    fn the_host_wall_limit_is_outside_the_session_identity() {
        let bounded = SessionConfig::default().with_wall_limit(Duration::from_secs(30));
        assert_ne!(bounded, SessionConfig::default());
        assert_eq!(
            identity_with_config(b"kernel", b"initramfs", &bounded),
            identity_with_config(b"kernel", b"initramfs", &SessionConfig::default()),
        );
    }

    #[test]
    fn a_config_serialized_without_a_wall_limit_still_loads() {
        let mut value = serde_json::to_value(SessionConfig::default()).expect("serialize");
        value
            .as_object_mut()
            .expect("configuration is a JSON object")
            .remove("wall_limit")
            .expect("the field is serialized");
        let decoded: SessionConfig = serde_json::from_value(value).expect("deserialize");
        assert_eq!(decoded, SessionConfig::default());
    }

    #[test]
    fn an_unbounded_session_runs_every_request_unguarded() {
        let latch = Arc::new(std::sync::atomic::AtomicBool::new(false));
        assert!(
            guarded_run_plan(false, None, Some(latch))
                .unwrap()
                .is_none()
        );
        assert!(guarded_run_plan(false, None, None).unwrap().is_none());
    }

    #[test]
    fn a_bounded_session_arms_the_backend_latch() {
        let latch = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let limit = Duration::from_secs(30);
        let (armed, armed_latch) = guarded_run_plan(false, Some(limit), Some(Arc::clone(&latch)))
            .expect("a latched backend can be bounded")
            .expect("the bound is armed");
        assert_eq!(armed, limit);
        assert!(Arc::ptr_eq(&armed_latch, &latch));
    }

    #[test]
    fn a_backend_without_a_latch_reports_the_bound_it_cannot_honor() {
        let error = guarded_run_plan(false, Some(Duration::from_secs(30)), None)
            .expect_err("an unbounded backend cannot take a bound");
        assert!(
            matches!(
                error.downcast_ref::<SessionError>(),
                Some(SessionError::Unboundable)
            ),
            "{error}"
        );
    }

    #[test]
    fn an_abandoned_session_reports_the_hang_it_already_had() {
        let latch = Arc::new(std::sync::atomic::AtomicBool::new(true));
        for (wall_limit, cancel) in [
            (Some(Duration::from_secs(30)), Some(Arc::clone(&latch))),
            (None, None),
        ] {
            let error = guarded_run_plan(true, wall_limit, cancel)
                .expect_err("an abandoned session runs nothing");
            assert!(
                matches!(
                    error.downcast_ref::<SessionError>(),
                    Some(SessionError::Abandoned)
                ),
                "{error}"
            );
            assert!(error.to_string().contains("abandoned"), "{error}");
        }
    }

    #[test]
    fn sparse_snapshot_rejects_unsorted_pages() {
        let result = SparseSnapshot::from_parts(
            1,
            [0; 32],
            vec![(2, Arc::new([0; PAGE_SIZE])), (1, Arc::new([0; PAGE_SIZE]))],
            &[],
            None,
        );
        assert!(result.is_err());
    }
}
