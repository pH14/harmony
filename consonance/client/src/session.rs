// SPDX-License-Identifier: AGPL-3.0-or-later
//! A workload-neutral in-process Consonance session.
//!
//! [`Session`] owns one VMM and its control server.  Workload packages provide
//! only ordered payload records; this module supplies the common boot,
//! snapshot, branch, replay, SDK-event, and sparse portable-snapshot
//! mechanics.  Keeping this seam here prevents a package adapter from
//! depending on another workload's machine crate.

use std::{error::Error, fmt, sync::Arc, time::Duration};

use control_proto::{Reply, SnapId, StopReason};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Default guest memory for callers that use [`SessionConfig::default`].
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
/// Guest physical address where the main RAM mapping begins for this backend.
#[cfg(target_arch = "x86_64")]
pub const RAM_GPA_BASE: u64 = 0;
#[cfg(target_arch = "aarch64")]
pub const RAM_GPA_BASE: u64 = 0x4000_0000;

/// Package-owned launch and resource settings for one neutral session.
///
/// A workload selects these values once while preparing its execution
/// identity. They affect boot and restore compatibility, so the complete
/// configuration is included in [`Session::identity_with_config`].
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionConfig {
    /// Guest RAM in bytes. The VMM requires a page-aligned, non-zero value.
    pub ram_bytes: usize,
    /// Seed supplied to the controlled x86 virtual-time boot path.
    pub seed: u64,
    /// Maximum virtual time allotted to each lifecycle run.
    pub run_budget: u64,
    /// Exact Linux command line used by the image launch.
    pub cmdline: String,
    /// Optional domain tag included in portable image identity hashing.
    /// Empty selects the generic session identity domain.
    pub identity_tag: String,
    /// Host wall-clock bound on one lifecycle run, or `None` for no bound.
    ///
    /// A guest spinning on a frozen virtual clock takes no exit, so it never
    /// reaches its virtual-time deadline; only host time notices it. Past this
    /// bound the run is abandoned and reported as [`SessionError::Hung`]. This
    /// is a host resource bound rather than an input, so it is deliberately
    /// absent from [`identity_with_config`] and from the image identity.
    #[serde(default)]
    pub wall_limit: Option<Duration>,
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
        }
    }
}

impl SessionConfig {
    /// Build explicit package launch settings.
    #[must_use]
    pub fn new(ram_bytes: usize, seed: u64, run_budget: u64, cmdline: impl Into<String>) -> Self {
        Self {
            ram_bytes,
            seed,
            run_budget,
            cmdline: cmdline.into(),
            identity_tag: String::new(),
            wall_limit: None,
        }
    }

    /// Include a workload-specific domain in portable snapshot identity.
    #[must_use]
    pub fn with_identity_tag(mut self, identity_tag: impl Into<String>) -> Self {
        self.identity_tag = identity_tag.into();
        self
    }

    /// Abandon a run that spends more than `wall_limit` of host time inside
    /// the guest. Available where the backend can be interrupted mid-run.
    #[must_use]
    pub fn with_wall_limit(mut self, wall_limit: Duration) -> Self {
        self.wall_limit = Some(wall_limit);
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

/// A portable whole VM snapshot relative to this session's setup point.
///
/// The page list and sidecar are copied out of the control server so a
/// campaign can serialize checkpoints and later restore them into a fresh
/// worker VM.  Page bytes are intentionally represented as `Vec<u8>` here:
/// this type is a package boundary and must not expose `Arc` or VMM internals.
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

/// Chunked opaque sidecar bytes shared between related sparse snapshots.
/// Sharing metadata never appears in the serialized representation.
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

/// A sparse whole-VM snapshot whose page and sidecar chunks can share storage
/// with an export base. Its serde field layout is the established v2 machine
/// format: `base`, `image_identity`, `pages`, and `sidecar`.
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
    /// Exact synchronized V-time at the source seal.
    #[must_use]
    pub fn at(&self) -> u64 {
        self.at
    }

    /// Number of sparse pages carried by this snapshot.
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

/// Errors returned by the neutral session boundary.
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
    /// The guest spent more than the configured wall-clock limit inside one
    /// run without taking an exit. The VM is abandoned; the session cannot be
    /// resumed and the caller reports the run rather than retrying it.
    #[error("guest ran for more than {0:?} of host time without exiting")]
    Hung(Duration),
    /// The guest never reached a snapshot-eligible point within the caller's
    /// settle allowance.
    #[error("guest reached no snapshot-eligible point within {settled} ns of settling")]
    Settle {
        /// Virtual time spent settling before the attempt was abandoned.
        settled: u64,
    },
}

/// Whether running the guest further can move it off a point the control
/// server refuses to seal. A crashed or quiescent guest advances no further,
/// so a retried seal would only repeat the refusal.
#[cfg_attr(
    not(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    )),
    allow(dead_code)
)]
fn settling_can_advance(stop: &StopReason) -> bool {
    match stop {
        StopReason::Deadline { .. }
        | StopReason::Decision { .. }
        | StopReason::SnapshotPoint { .. }
        | StopReason::Assertion { .. } => true,
        StopReason::Crash { .. } | StopReason::Quiescent { .. } => false,
    }
}

/// Seal the current point, running the guest a little further whenever the
/// control server refuses it.
///
/// `seal` answers `None` for a point that is not snapshot-eligible yet, and
/// `advance` runs one settle step and reports where the guest stopped. Both
/// take `context` so one caller can lend the same session to each. This pure
/// retry policy stays here so it is testable without a VM or Linux linker; the
/// third result field is the last settle run's stop reason, absent when the
/// point sealed with no settling at all.
#[cfg_attr(
    not(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    )),
    allow(dead_code)
)]
fn seal_after_settling<C, Seal, Advance>(
    context: &mut C,
    settle_step: u64,
    max_settle: u64,
    mut seal: Seal,
    mut advance: Advance,
) -> Result<(SnapId, u64, Option<StopReason>), Box<dyn Error>>
where
    Seal: FnMut(&mut C) -> Result<Option<(SnapId, u64)>, Box<dyn Error>>,
    Advance: FnMut(&mut C, u64) -> Result<StopReason, Box<dyn Error>>,
{
    if settle_step == 0 {
        return Err(SessionError::Control("settle step is zero".into()).into());
    }
    let mut settled = 0_u64;
    let mut last: Option<StopReason> = None;
    loop {
        if let Some((snapshot, at)) = seal(context)? {
            return Ok((snapshot, at, last));
        }
        // Every stop is offered a seal before it is judged, so a guest that ran
        // to quiescence or crashed during the last step still gets its endpoint
        // sealed; only a second step is refused.
        if let Some(stop) = &last
            && !settling_can_advance(stop)
        {
            return Err(SessionError::Stop(stop.clone()).into());
        }
        if settled >= max_settle {
            return Err(SessionError::Settle { settled }.into());
        }
        let step = settle_step.min(max_settle - settled);
        last = Some(advance(context, step)?);
        settled += step;
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

/// Drain paged console replies into a bounded diagnostic buffer. The transport
/// closure stays in the live module; this pure paging policy is portable and
/// therefore testable without a VM or Linux linker.
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
        // The control server bounds every page below the u32 console offset
        // range, so the accumulated buffer length is the next page offset.
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

/// One SDK event tuple: V-time, publisher event id, and opaque value bytes.
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

/// Stable identity for the complete VM image and neutral session contract.
#[must_use]
pub fn identity(kernel: &[u8], initramfs: &[u8]) -> String {
    identity_with_config(kernel, initramfs, &SessionConfig::default())
}

/// Stable identity for an image and its complete launch/resource contract.
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

/// Convert a control session to a stable identity without constructing it.
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

    /// A caller-scripted stand-in for the control server's seal/run pair.
    struct SettleFixture {
        /// Virtual time at which the guest becomes snapshot-eligible.
        eligible_at: u64,
        now: u64,
        stop: StopReason,
        seals: usize,
        runs: Vec<u64>,
    }

    impl SettleFixture {
        fn new(eligible_at: u64) -> Self {
            Self {
                eligible_at,
                now: 0,
                stop: StopReason::Deadline {
                    vtime: control_proto::Moment(0),
                },
                seals: 0,
                runs: Vec::new(),
            }
        }

        fn seal(&mut self) -> Result<Option<(SnapId, u64)>, Box<dyn Error>> {
            self.seals += 1;
            Ok((self.now >= self.eligible_at).then_some((SnapId(7), self.now)))
        }

        fn advance(&mut self, step: u64) -> Result<StopReason, Box<dyn Error>> {
            self.runs.push(step);
            self.now += step;
            Ok(self.stop.clone())
        }
    }

    fn settle(fixture: &mut SettleFixture, step: u64, max: u64) -> Result<u64, Box<dyn Error>> {
        seal_after_settling(
            fixture,
            step,
            max,
            SettleFixture::seal,
            SettleFixture::advance,
        )
        .map(|(_, at, _)| at)
    }

    #[test]
    fn an_eligible_point_seals_without_running_the_guest() {
        let mut fixture = SettleFixture::new(0);
        let (snapshot, at, stop) = seal_after_settling(
            &mut fixture,
            10,
            100,
            SettleFixture::seal,
            SettleFixture::advance,
        )
        .expect("an eligible point seals");
        assert_eq!((snapshot, at), (SnapId(7), 0));
        assert_eq!(stop, None, "no settle run happened, so there is no stop");
        assert!(fixture.runs.is_empty());
    }

    #[test]
    fn settling_advances_in_steps_and_stops_at_the_allowance() {
        let mut fixture = SettleFixture::new(25);
        assert_eq!(settle(&mut fixture, 10, 100).unwrap(), 30);
        assert_eq!(fixture.runs, [10, 10, 10]);
        // The final step is clipped so settling never runs past the allowance,
        // and the point is still offered one last seal at the boundary.
        let mut clipped = SettleFixture::new(25);
        assert_eq!(settle(&mut clipped, 10, 25).unwrap(), 25);
        assert_eq!(clipped.runs, [10, 10, 5]);
        let mut exhausted = SettleFixture::new(31);
        let error = settle(&mut exhausted, 10, 30).unwrap_err().to_string();
        assert!(error.contains("30 ns of settling"), "{error}");
        assert_eq!(exhausted.runs, [10, 10, 10]);
        assert_eq!(exhausted.seals, 4, "the allowance boundary is sealed too");
    }

    #[test]
    fn a_guest_that_cannot_advance_is_sealed_once_more_then_reported() {
        for stop in [
            StopReason::Quiescent {
                vtime: control_proto::Moment(1),
            },
            StopReason::Crash {
                vtime: control_proto::Moment(1),
                info: control_proto::CrashInfo {
                    kind: control_proto::CrashKind::Panic,
                    detail: Vec::new(),
                },
            },
        ] {
            let mut fixture = SettleFixture::new(u64::MAX);
            fixture.stop = stop.clone();
            assert!(!settling_can_advance(&stop));
            let error = settle(&mut fixture, 10, 100).unwrap_err().to_string();
            assert!(
                error.contains("stopped before the expected snapshot point"),
                "{error}"
            );
            assert_eq!(fixture.runs, [10], "settling stopped after the first run");
            assert_eq!(fixture.seals, 2, "the endpoint was offered a final seal");
        }
    }

    #[test]
    fn a_zero_settle_step_is_rejected_before_any_control_request() {
        let mut fixture = SettleFixture::new(u64::MAX);
        assert!(settle(&mut fixture, 0, 100).is_err());
        assert_eq!(fixture.seals, 0);
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
