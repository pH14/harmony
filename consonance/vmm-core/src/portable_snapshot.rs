// SPDX-License-Identifier: AGPL-3.0-or-later
//! Host-neutral control-snapshot artifacts.
//!
//! A snapshot-store layer is not portable by itself: the control server keeps
//! the SDK stream, remaining ordered payloads, generic service configuration,
//! evidence cut, and lineage taint in handle-keyed side tables.
//! This module serializes that complete replay state together with materialized
//! RAM and the canonical vendor VM-state blob. The format is fixed-order,
//! little-endian, length-bounded, and protected by a trailing SHA-256 digest
//! for complete artifacts. Version 5 conditionally carries an opaque
//! control-plane state section after the existing body; empty control state
//! retains the byte layout of v4. Version 6 keeps the v5 field order while
//! allowing sections beyond the legacy fixed bounds. Readers also retain
//! v3/v4/v5 compatibility, and SDK pending-stop fields remain available in
//! every format from v4 onward.

use std::{
    collections::BTreeMap,
    io::{Read, Write},
    sync::Arc,
};

use environment::{channel, input_spec::ServiceConfig};
use sha2::{Digest, Sha256};

use crate::snapshot::SnapshotError;
use crate::vmm::{SdkSnapshot, SdkStop};
use snapshot_store::PAGE_SIZE;

const MAGIC: [u8; 8] = *b"HMSNAP01";
const V3_VERSION: u16 = 3;
const LEGACY_VERSION: u16 = 4;
const V5_VERSION: u16 = 5;
const VERSION: u16 = 6;
const FLAG_SDK: u16 = 1;
const FLAG_TAINTED: u16 = 1 << 1;
const KNOWN_FLAGS: u16 = FLAG_SDK | FLAG_TAINTED;

const MAX_VM_STATE_LEN: usize = 16 * 1024 * 1024;
const MAX_SDK_LEN: usize = 64 * 1024 * 1024;
const MAX_CONTROL_LEN: usize = MAX_SDK_LEN;
const MAX_POLICY_LEN: usize = 1024 * 1024;
const SPARSE_MAGIC: [u8; 8] = *b"HMSSNAP1";
const SPARSE_VERSION: u16 = VERSION;
const SPARSE_FLAG_SDK: u16 = 1;
const SPARSE_FLAG_TAINTED: u16 = 1 << 1;
const SPARSE_KNOWN_FLAGS: u16 = SPARSE_FLAG_SDK | SPARSE_FLAG_TAINTED;
const MAX_SUFFIX_LEN: usize = 16 * 1024 * 1024;
const MAX_SPARSE_SIDECAR_LEN: usize = MAX_VM_STATE_LEN
    .saturating_add(MAX_SDK_LEN)
    .saturating_add(MAX_CONTROL_LEN)
    .saturating_add(MAX_POLICY_LEN)
    .saturating_add(MAX_SUFFIX_LEN)
    .saturating_add(128);

/// In-process sparse portable snapshot data.
///
/// pages contains only the target-resolved pages that differ from the
/// export base, in strictly increasing GFN order. The sidecar is an opaque,
/// versioned byte vector containing the non-memory replay state; it never
/// contains guest RAM or a whole-state hash. Keeping pages in Arcs lets a
/// search worker pass the sparse image between layers without eagerly copying
/// each 4-KiB frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SparsePortableSnapshot {
    /// Target-resolved changed pages, sorted by GFN and unique.
    pub pages: Vec<(u64, Arc<[u8; PAGE_SIZE]>)>,
    /// Versioned sidecar containing vendor and control-plane replay state.
    pub sidecar: Vec<u8>,
}

/// Metadata returned after importing an in-process sparse snapshot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SparsePortableSnapshotReceipt {
    /// Newly minted session-local snapshot handle.
    pub id: control_proto::SnapId,
    /// Exact synchronized V-time at the imported seal.
    pub at: control_proto::Moment,
    /// SDK event-prefix length included in the snapshot.
    pub sdk_events: u64,
    /// Portable normalized-event prefix length at the seal.
    pub trace_events: u64,
    /// Portable deadline-schedule prefix length at the seal.
    pub trace_schedules: u64,
    /// Whether the sealed lineage was tainted by improvisation.
    pub tainted: bool,
}

pub(crate) struct SparsePortableSidecar {
    pub(crate) vm_state: Vec<u8>,
    pub(crate) sdk: Option<SdkSnapshot>,
    pub(crate) policy: ServiceConfig,
    pub(crate) at: u64,
    pub(crate) sdk_events: u64,
    pub(crate) trace_events: u64,
    pub(crate) trace_schedules: u64,
    pub(crate) tainted: bool,
    pub(crate) state_blob_suffix: Vec<u8>,
    /// Opaque control-server state. Empty keeps the v4 sidecar layout; a
    /// nonempty value selects the v5 extension.
    pub(crate) control_state: Vec<u8>,
}

pub(crate) struct SparsePortableSidecarRef<'a> {
    pub(crate) vm_state: &'a [u8],
    pub(crate) sdk: Option<&'a SdkSnapshot>,
    pub(crate) policy: &'a ServiceConfig,
    pub(crate) at: u64,
    pub(crate) sdk_events: u64,
    pub(crate) trace_events: u64,
    pub(crate) trace_schedules: u64,
    pub(crate) tainted: bool,
    pub(crate) state_blob_suffix: &'a [u8],
    /// Opaque control-server state. Empty keeps the v4 sidecar layout; a
    /// nonempty value selects the v5 extension.
    pub(crate) control_state: &'a [u8],
}

/// A complete decoded portable snapshot.
pub(crate) struct PortableSnapshot {
    pub(crate) memory: Vec<u8>,
    pub(crate) vm_state: Vec<u8>,
    pub(crate) sdk: Option<SdkSnapshot>,
    pub(crate) policy: ServiceConfig,
    pub(crate) at: u64,
    pub(crate) sdk_events: u64,
    pub(crate) trace_events: u64,
    pub(crate) trace_schedules: u64,
    pub(crate) tainted: bool,
    pub(crate) state_hash: [u8; 32],
    /// Opaque control-server state. Empty keeps the v4 artifact layout; a
    /// nonempty value selects the v5 extension.
    pub(crate) control_state: Vec<u8>,
}

/// Borrowed form used while streaming an existing store layer to disk.
pub(crate) struct PortableSnapshotRef<'a> {
    pub(crate) memory: &'a [u8],
    pub(crate) vm_state: &'a [u8],
    pub(crate) sdk: Option<&'a SdkSnapshot>,
    pub(crate) policy: &'a ServiceConfig,
    pub(crate) at: u64,
    pub(crate) sdk_events: u64,
    pub(crate) trace_events: u64,
    pub(crate) trace_schedules: u64,
    pub(crate) tainted: bool,
    pub(crate) state_hash: [u8; 32],
    /// Opaque control-server state. Empty keeps the v4 artifact layout; a
    /// nonempty value selects the v5 extension.
    pub(crate) control_state: &'a [u8],
}

/// Strict portable-snapshot encode/decode failure.
#[derive(Debug, thiserror::Error)]
pub enum PortableSnapshotError {
    /// Artifact I/O failed.
    #[error("portable snapshot I/O error")]
    Io(#[from] std::io::Error),
    /// The container magic is not the portable-snapshot magic.
    #[error("portable snapshot has bad magic")]
    BadMagic,
    /// The format version is not supported by this build.
    #[error("portable snapshot version {0} is unsupported")]
    BadVersion(u16),
    /// An unknown flag or a presence/length contradiction was found.
    #[error("portable snapshot flags are malformed")]
    BadFlags,
    /// A section length is not admissible.
    #[error("portable snapshot {section} length {got} exceeds {max}")]
    Length {
        /// Stable section name.
        section: &'static str,
        /// Encoded length.
        got: u64,
        /// Maximum accepted length.
        max: u64,
    },
    /// A section has an invalid tag, count, truncation, or trailing byte.
    #[error("portable snapshot section malformed: {0}")]
    Malformed(&'static str),
    /// The trailing artifact digest does not authenticate the preceding bytes.
    #[error("portable snapshot SHA-256 mismatch")]
    DigestMismatch,
    /// The embedded generic service configuration or channel answer was malformed.
    #[error("portable snapshot environment state malformed")]
    Environment(#[from] channel::ChannelError),
    /// The snapshot store or vendor VM-state codec rejected imported bytes.
    #[error("portable snapshot store/VM-state failure")]
    Snapshot(#[from] SnapshotError),
    /// The requested session-local handle is absent.
    #[error("unknown portable snapshot handle {0}")]
    UnknownSnapshot(u64),
}

impl PortableSnapshotRef<'_> {
    pub(crate) fn write_to<W: Write>(&self, mut writer: W) -> Result<(), PortableSnapshotError> {
        let sdk = self.sdk.map(encode_sdk).transpose()?;
        let policy = self.policy.encode();
        let version = complete_version(
            self.vm_state.len(),
            sdk.as_ref().map_or(0, Vec::len),
            policy.len(),
            self.control_state.len(),
        );
        if self.vm_state.is_empty() {
            return Err(PortableSnapshotError::BadFlags);
        }
        if version != VERSION {
            check_len("memory", self.memory.len(), self.memory.len())?;
            check_len("vm_state", self.vm_state.len(), MAX_VM_STATE_LEN)?;
            check_len("sdk", sdk.as_ref().map_or(0, Vec::len), MAX_SDK_LEN)?;
            check_len("policy", policy.len(), MAX_POLICY_LEN)?;
            check_len("control_state", self.control_state.len(), MAX_CONTROL_LEN)?;
        }

        let mut flags = 0;
        if sdk.is_some() {
            flags |= FLAG_SDK;
        }
        if self.tainted {
            flags |= FLAG_TAINTED;
        }

        let mut out = HashWriter::new(&mut writer);
        out.write_all(&MAGIC)?;
        put_u16(&mut out, version)?;
        put_u16(&mut out, flags)?;
        put_len(&mut out, self.memory.len())?;
        put_len(&mut out, self.vm_state.len())?;
        put_len(&mut out, sdk.as_ref().map_or(0, Vec::len))?;
        put_len(&mut out, policy.len())?;
        if version >= V5_VERSION {
            put_len(&mut out, self.control_state.len())?;
        }
        put_u64(&mut out, self.at)?;
        put_u64(&mut out, self.sdk_events)?;
        put_u64(&mut out, self.trace_events)?;
        put_u64(&mut out, self.trace_schedules)?;
        out.write_all(&self.state_hash)?;
        out.write_all(self.memory)?;
        out.write_all(self.vm_state)?;
        if let Some(bytes) = &sdk {
            out.write_all(bytes)?;
        }
        out.write_all(&policy)?;
        if version >= V5_VERSION {
            out.write_all(self.control_state)?;
        }
        // Finish the authenticated body before consuming the hash adapter;
        // the digest itself is then written and flushed through the original
        // writer below.
        out.flush()?;
        let digest = out.finish();
        writer.write_all(&digest)?;
        writer.flush()?;
        Ok(())
    }
}

impl PortableSnapshot {
    pub(crate) fn read_from<R: Read>(
        mut reader: R,
        expected_memory_len: usize,
    ) -> Result<Self, PortableSnapshotError> {
        let mut input = HashReader::new(&mut reader);
        let mut magic = [0; 8];
        input.read_exact(&mut magic)?;
        if magic != MAGIC {
            return Err(PortableSnapshotError::BadMagic);
        }
        let version = get_u16(&mut input)?;
        if version != V3_VERSION
            && version != LEGACY_VERSION
            && version != V5_VERSION
            && version != VERSION
        {
            return Err(PortableSnapshotError::BadVersion(version));
        }
        let flags = get_u16(&mut input)?;
        if flags & !KNOWN_FLAGS != 0 {
            return Err(PortableSnapshotError::BadFlags);
        }
        let memory_len = get_u64(&mut input)?;
        if memory_len != expected_memory_len as u64 {
            return Err(PortableSnapshotError::Length {
                section: "memory",
                got: memory_len,
                max: expected_memory_len as u64,
            });
        }
        let vm_state_len = section_len(&mut input, "vm_state", version, MAX_VM_STATE_LEN)?;
        let sdk_len = section_len(&mut input, "sdk", version, MAX_SDK_LEN)?;
        let policy_len = section_len(&mut input, "policy", version, MAX_POLICY_LEN)?;
        let control_len = if version >= V5_VERSION {
            section_len(&mut input, "control_state", version, MAX_CONTROL_LEN)?
        } else {
            0
        };
        let has_sdk = flags & FLAG_SDK != 0;
        if vm_state_len == 0
            || has_sdk != (sdk_len != 0)
            || policy_len == 0
            || (version == V5_VERSION && control_len == 0)
        {
            return Err(PortableSnapshotError::BadFlags);
        }
        let at = get_u64(&mut input)?;
        let sdk_events = get_u64(&mut input)?;
        let trace_events = get_u64(&mut input)?;
        let trace_schedules = get_u64(&mut input)?;
        let mut state_hash = [0; 32];
        input.read_exact(&mut state_hash)?;
        let memory = read_vec(&mut input, expected_memory_len)?;
        let vm_state = read_vec(&mut input, vm_state_len)?;
        let sdk_bytes = read_vec(&mut input, sdk_len)?;
        let policy_bytes = read_vec(&mut input, policy_len)?;
        let control_state = read_vec(&mut input, control_len)?;
        let calculated = input.finish();
        let mut recorded = [0; 32];
        reader.read_exact(&mut recorded)?;
        if calculated != recorded {
            return Err(PortableSnapshotError::DigestMismatch);
        }
        let mut trailing = [0; 1];
        if reader.read(&mut trailing)? != 0 {
            return Err(PortableSnapshotError::Malformed("trailing bytes"));
        }
        let sdk = has_sdk
            .then(|| decode_sdk(&sdk_bytes, version))
            .transpose()?;
        let policy = ServiceConfig::decode(&policy_bytes)?;
        Ok(Self {
            memory,
            vm_state,
            sdk,
            policy,
            at,
            sdk_events,
            trace_events,
            trace_schedules,
            tainted: flags & FLAG_TAINTED != 0,
            state_hash,
            control_state,
        })
    }
}

/// Encode the non-memory half of an in-process sparse snapshot.
///
/// This format intentionally has no RAM section and no whole-state digest:
/// the store already authenticates each page and vm_state blob, while the
/// sparse seam must not turn an export into an O(total-RAM) operation.
pub(crate) fn encode_sparse_sidecar(
    sidecar: &SparsePortableSidecarRef<'_>,
) -> Result<Vec<u8>, PortableSnapshotError> {
    let sdk = sidecar.sdk.map(encode_sdk).transpose()?;
    let policy = sidecar.policy.encode();
    let version = sparse_version(
        sidecar.vm_state.len(),
        sdk.as_ref().map_or(0, Vec::len),
        policy.len(),
        sidecar.state_blob_suffix.len(),
        sidecar.control_state.len(),
    );
    if sidecar.vm_state.is_empty() {
        return Err(PortableSnapshotError::BadFlags);
    }
    if version != SPARSE_VERSION {
        check_len("vm_state", sidecar.vm_state.len(), MAX_VM_STATE_LEN)?;
        check_len("sdk", sdk.as_ref().map_or(0, Vec::len), MAX_SDK_LEN)?;
        check_len("policy", policy.len(), MAX_POLICY_LEN)?;
        check_len(
            "state_blob_suffix",
            sidecar.state_blob_suffix.len(),
            MAX_SUFFIX_LEN,
        )?;
        check_len(
            "control_state",
            sidecar.control_state.len(),
            MAX_CONTROL_LEN,
        )?;
    }

    let mut flags = 0;
    if sdk.is_some() {
        flags |= SPARSE_FLAG_SDK;
    }
    if sidecar.tainted {
        flags |= SPARSE_FLAG_TAINTED;
    }

    let mut out = Vec::new();
    out.extend_from_slice(&SPARSE_MAGIC);
    put_u16(&mut out, version)?;
    put_u16(&mut out, flags)?;
    put_len(&mut out, sidecar.vm_state.len())?;
    put_len(&mut out, sdk.as_ref().map_or(0, Vec::len))?;
    put_len(&mut out, policy.len())?;
    put_len(&mut out, sidecar.state_blob_suffix.len())?;
    if version >= V5_VERSION {
        put_len(&mut out, sidecar.control_state.len())?;
    }
    put_u64(&mut out, sidecar.at)?;
    put_u64(&mut out, sidecar.sdk_events)?;
    put_u64(&mut out, sidecar.trace_events)?;
    put_u64(&mut out, sidecar.trace_schedules)?;
    out.extend_from_slice(sidecar.vm_state);
    if let Some(bytes) = &sdk {
        out.extend_from_slice(bytes);
    }
    out.extend_from_slice(&policy);
    out.extend_from_slice(sidecar.state_blob_suffix);
    if version >= V5_VERSION {
        out.extend_from_slice(sidecar.control_state);
    }
    if version != SPARSE_VERSION {
        check_len("sidecar", out.len(), MAX_SPARSE_SIDECAR_LEN)?;
    }
    Ok(out)
}

/// Decode and strictly validate an in-process sparse sidecar.
///
/// All section lengths are checked before allocation/copying, optional-section
/// flags must agree with their lengths, and no trailing bytes are accepted.
pub(crate) fn decode_sparse_sidecar(
    bytes: &[u8],
) -> Result<SparsePortableSidecar, PortableSnapshotError> {
    let mut input = SliceReader::new(bytes);
    if input.take(SPARSE_MAGIC.len())? != SPARSE_MAGIC {
        return Err(PortableSnapshotError::BadMagic);
    }
    let version = input.u16()?;
    if version != V3_VERSION
        && version != LEGACY_VERSION
        && version != V5_VERSION
        && version != SPARSE_VERSION
    {
        return Err(PortableSnapshotError::BadVersion(version));
    }
    if version != SPARSE_VERSION {
        check_len("sidecar", bytes.len(), MAX_SPARSE_SIDECAR_LEN)?;
    }
    let flags = input.u16()?;
    if flags & !SPARSE_KNOWN_FLAGS != 0 {
        return Err(PortableSnapshotError::BadFlags);
    }
    let vm_state_len = input.section_len("vm_state", version, MAX_VM_STATE_LEN)?;
    let sdk_len = input.section_len("sdk", version, MAX_SDK_LEN)?;
    let policy_len = input.section_len("policy", version, MAX_POLICY_LEN)?;
    let suffix_len = input.section_len("state_blob_suffix", version, MAX_SUFFIX_LEN)?;
    let control_len = if version >= V5_VERSION {
        input.section_len("control_state", version, MAX_CONTROL_LEN)?
    } else {
        0
    };
    let has_sdk = flags & SPARSE_FLAG_SDK != 0;
    if vm_state_len == 0
        || policy_len == 0
        || has_sdk != (sdk_len != 0)
        || (version == V5_VERSION && control_len == 0)
    {
        return Err(PortableSnapshotError::BadFlags);
    }
    let at = input.u64()?;
    let sdk_events = input.u64()?;
    let trace_events = input.u64()?;
    let trace_schedules = input.u64()?;
    let vm_state = input.section(vm_state_len)?;
    let sdk_bytes = input.section(sdk_len)?;
    let policy_bytes = input.section(policy_len)?;
    let state_blob_suffix = input.section(suffix_len)?;
    let control_state = input.section(control_len)?;
    input.finish("sparse sidecar")?;
    let sdk = has_sdk
        .then(|| decode_sdk(&sdk_bytes, version))
        .transpose()?;
    let policy = ServiceConfig::decode(&policy_bytes)?;
    Ok(SparsePortableSidecar {
        vm_state,
        sdk,
        policy,
        at,
        sdk_events,
        trace_events,
        trace_schedules,
        tainted: flags & SPARSE_FLAG_TAINTED != 0,
        state_blob_suffix,
        control_state,
    })
}

fn encode_sdk(sdk: &SdkSnapshot) -> Result<Vec<u8>, PortableSnapshotError> {
    let mut out = Vec::new();
    let recorded = sdk.recorded.encode();
    put_vec_len(&mut out, recorded.len());
    out.extend_from_slice(&recorded);
    out.push(u8::from(sdk.pending_snapshot));
    encode_pending_stop(&mut out, sdk.pending_stop.as_ref());
    put_vec_len(&mut out, sdk.events.len());
    for (moment, local, payload) in &sdk.events {
        out.extend_from_slice(&moment.to_le_bytes());
        out.extend_from_slice(&local.to_le_bytes());
        put_vec_len(&mut out, payload.len());
        out.extend_from_slice(payload);
    }
    if !sdk.coverage_thresholds.is_empty() {
        out.extend_from_slice(b"COVR");
        put_vec_len(&mut out, sdk.coverage_thresholds.len());
        for (thread, threshold) in &sdk.coverage_thresholds {
            out.extend_from_slice(&thread.to_le_bytes());
            out.extend_from_slice(&threshold.to_le_bytes());
        }
    }
    Ok(out)
}

fn decode_sdk(bytes: &[u8], version: u16) -> Result<SdkSnapshot, PortableSnapshotError> {
    let mut input = SliceReader::new(bytes);
    let recorded = channel::RecordedState::decode(&input.bytes()?)
        .map_err(|_| PortableSnapshotError::Malformed("SDK recorded state"))?;
    let pending_snapshot = input.boolean()?;
    // Version 3 only admitted SDK snapshots with no pending stop.
    let pending_stop = if version >= 4 {
        decode_pending_stop(&mut input)?
    } else {
        None
    };
    let event_count = input.count("SDK event count", 20)?;
    let mut events = Vec::new();
    events
        .try_reserve_exact(event_count)
        .map_err(|_| PortableSnapshotError::Malformed("SDK event allocation"))?;
    for _ in 0..event_count {
        let moment = input.u64()?;
        let local = input.u32()?;
        let payload = input.bytes()?;
        events.push((moment, local, payload));
    }
    let mut coverage_thresholds = BTreeMap::new();
    if input.remaining() != 0 {
        if input.take(4)? != b"COVR" {
            return Err(PortableSnapshotError::Malformed("SDK coverage tag"));
        }
        let count = input.count("SDK coverage threshold count", 12)?;
        if count == 0 {
            return Err(PortableSnapshotError::Malformed(
                "SDK coverage threshold count",
            ));
        }
        let mut previous_thread = None;
        for _ in 0..count {
            let thread = input.u32()?;
            let threshold = input.u64()?;
            if threshold == 0 || previous_thread.is_some_and(|previous| thread <= previous) {
                return Err(PortableSnapshotError::Malformed("SDK coverage threshold"));
            }
            previous_thread = Some(thread);
            coverage_thresholds.insert(thread, threshold);
        }
    }
    input.finish("SDK")?;
    Ok(SdkSnapshot {
        recorded,
        events,
        pending_snapshot,
        pending_stop,
        coverage_thresholds,
    })
}

// The SDK layer owns the stop vocabulary; the portable container carries the
// outstanding response sequence and request verbatim, without resolving it.
fn encode_pending_stop(out: &mut Vec<u8>, stop: Option<&SdkStop>) {
    match stop {
        None => out.push(0),
        Some(SdkStop::Quiescent) => out.push(1),
        Some(SdkStop::Assertion { id, data }) => {
            out.push(2);
            out.extend_from_slice(&id.to_le_bytes());
            put_vec_len(out, data.len());
            out.extend_from_slice(data);
        }
        Some(SdkStop::Decision {
            moment,
            seq,
            question,
        }) => {
            out.push(3);
            out.extend_from_slice(&moment.to_le_bytes());
            out.extend_from_slice(&seq.to_le_bytes());
            out.extend_from_slice(&question.service().to_le_bytes());
            out.extend_from_slice(&question.request_id().to_le_bytes());
            put_vec_len(out, question.payload().len());
            out.extend_from_slice(question.payload());
        }
    }
}

fn decode_pending_stop(
    input: &mut SliceReader<'_>,
) -> Result<Option<SdkStop>, PortableSnapshotError> {
    Ok(match input.u8()? {
        0 => None,
        1 => Some(SdkStop::Quiescent),
        2 => Some(SdkStop::Assertion {
            id: input.u32()?,
            data: input.bytes()?,
        }),
        3 => {
            let moment = input.u64()?;
            let seq = input.u32()?;
            let service = input.u16()?;
            let request = input.u64()?;
            let payload = input.bytes()?;
            let question = channel::Question::with_request_id(request, service, payload)
                .map_err(|_| PortableSnapshotError::Malformed("SDK pending question"))?;
            Some(SdkStop::Decision {
                moment,
                seq,
                question,
            })
        }
        _ => return Err(PortableSnapshotError::Malformed("SDK pending stop")),
    })
}

// The old versions retain their resource limits and byte layouts. Version 6
// changes the allocation policy, not the ownership or meaning of a section.
fn complete_version(vm: usize, sdk: usize, policy: usize, control: usize) -> u16 {
    if vm > MAX_VM_STATE_LEN
        || sdk > MAX_SDK_LEN
        || policy > MAX_POLICY_LEN
        || control > MAX_CONTROL_LEN
    {
        VERSION
    } else if control == 0 {
        LEGACY_VERSION
    } else {
        V5_VERSION
    }
}

fn sparse_version(vm: usize, sdk: usize, policy: usize, suffix: usize, control: usize) -> u16 {
    if suffix > MAX_SUFFIX_LEN {
        VERSION
    } else {
        complete_version(vm, sdk, policy, control)
    }
}

fn section_len<R: Read>(
    input: &mut R,
    section: &'static str,
    version: u16,
    legacy_max: usize,
) -> Result<usize, PortableSnapshotError> {
    bounded_len(
        input,
        section,
        if version == VERSION {
            isize::MAX as usize
        } else {
            legacy_max
        },
    )
}

fn check_len(section: &'static str, got: usize, max: usize) -> Result<(), PortableSnapshotError> {
    if got > max {
        return Err(PortableSnapshotError::Length {
            section,
            got: got as u64,
            max: max as u64,
        });
    }
    Ok(())
}

fn bounded_len<R: Read>(
    input: &mut R,
    section: &'static str,
    max: usize,
) -> Result<usize, PortableSnapshotError> {
    let got = get_u64(input)?;
    let len = usize::try_from(got).map_err(|_| PortableSnapshotError::Length {
        section,
        got,
        max: max as u64,
    })?;
    check_len(section, len, max)?;
    Ok(len)
}

fn read_vec<R: Read>(input: &mut R, len: usize) -> Result<Vec<u8>, PortableSnapshotError> {
    // Never reserve a declared section length before receiving its bytes. A
    // truncated header advertising a huge v6 section costs only this buffer.
    let mut chunk = [0_u8; 64 * 1024];
    let mut bytes = Vec::new();
    while bytes.len() < len {
        let wanted = (len - bytes.len()).min(chunk.len());
        let received = match input.read(&mut chunk[..wanted]) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if received == 0 {
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
        }
        bytes
            .try_reserve(received)
            .map_err(|_| PortableSnapshotError::Malformed("section allocation"))?;
        bytes.extend_from_slice(&chunk[..received]);
    }
    Ok(bytes)
}

fn put_vec_len(out: &mut Vec<u8>, len: usize) {
    out.extend_from_slice(&(len as u64).to_le_bytes());
}

fn put_len<W: Write>(out: &mut W, len: usize) -> Result<(), std::io::Error> {
    put_u64(out, len as u64)
}

fn put_u16<W: Write>(out: &mut W, value: u16) -> Result<(), std::io::Error> {
    out.write_all(&value.to_le_bytes())
}

fn put_u64<W: Write>(out: &mut W, value: u64) -> Result<(), std::io::Error> {
    out.write_all(&value.to_le_bytes())
}

fn get_u16<R: Read>(input: &mut R) -> Result<u16, std::io::Error> {
    let mut bytes = [0; 2];
    input.read_exact(&mut bytes)?;
    Ok(u16::from_le_bytes(bytes))
}

fn get_u64<R: Read>(input: &mut R) -> Result<u64, std::io::Error> {
    let mut bytes = [0; 8];
    input.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

struct HashWriter<W> {
    inner: W,
    digest: Sha256,
}

impl<W> HashWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            digest: Sha256::new(),
        }
    }

    fn finish(self) -> [u8; 32] {
        self.digest.finalize().into()
    }
}

impl<W: Write> Write for HashWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(bytes)?;
        self.digest.update(&bytes[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

struct HashReader<R> {
    inner: R,
    digest: Sha256,
}

impl<R> HashReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            digest: Sha256::new(),
        }
    }

    fn finish(self) -> [u8; 32] {
        self.digest.finalize().into()
    }
}

impl<R: Read> Read for HashReader<R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(bytes)?;
        self.digest.update(&bytes[..read]);
        Ok(read)
    }
}

struct SliceReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> SliceReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], PortableSnapshotError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or(PortableSnapshotError::Malformed("section offset overflow"))?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or(PortableSnapshotError::Malformed("section truncation"))?;
        self.offset = end;
        Ok(bytes)
    }

    fn u8(&mut self) -> Result<u8, PortableSnapshotError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, PortableSnapshotError> {
        let mut bytes = [0; 2];
        bytes.copy_from_slice(self.take(2)?);
        Ok(u16::from_le_bytes(bytes))
    }

    fn boolean(&mut self) -> Result<bool, PortableSnapshotError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(PortableSnapshotError::Malformed("boolean")),
        }
    }

    fn u32(&mut self) -> Result<u32, PortableSnapshotError> {
        let mut bytes = [0; 4];
        bytes.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(bytes))
    }

    fn u64(&mut self) -> Result<u64, PortableSnapshotError> {
        let mut bytes = [0; 8];
        bytes.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(bytes))
    }

    fn count(
        &mut self,
        label: &'static str,
        minimum_record_len: usize,
    ) -> Result<usize, PortableSnapshotError> {
        let count =
            usize::try_from(self.u64()?).map_err(|_| PortableSnapshotError::Malformed(label))?;
        if count > self.remaining() / minimum_record_len {
            return Err(PortableSnapshotError::Malformed(label));
        }
        Ok(count)
    }

    fn bytes(&mut self) -> Result<Vec<u8>, PortableSnapshotError> {
        let len = usize::try_from(self.u64()?)
            .map_err(|_| PortableSnapshotError::Malformed("nested length"))?;
        self.section(len)
    }

    fn section(&mut self, len: usize) -> Result<Vec<u8>, PortableSnapshotError> {
        let bytes = self.take(len)?;
        let mut copy = Vec::new();
        copy.try_reserve_exact(len)
            .map_err(|_| PortableSnapshotError::Malformed("section allocation"))?;
        copy.extend_from_slice(bytes);
        Ok(copy)
    }

    fn section_len(
        &mut self,
        section: &'static str,
        version: u16,
        legacy_max: usize,
    ) -> Result<usize, PortableSnapshotError> {
        let max = if version == VERSION {
            self.bytes.len()
        } else {
            legacy_max
        };
        self.bounded_len(section, max)
    }

    fn bounded_len(
        &mut self,
        section: &'static str,
        max: usize,
    ) -> Result<usize, PortableSnapshotError> {
        let got = self.u64()?;
        let len = usize::try_from(got).map_err(|_| PortableSnapshotError::Length {
            section,
            got,
            max: max as u64,
        })?;
        check_len(section, len, max)?;
        Ok(len)
    }

    fn finish(self, section: &'static str) -> Result<(), PortableSnapshotError> {
        if self.offset != self.bytes.len() {
            return Err(PortableSnapshotError::Malformed(section));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_six_round_trips_large_sections_with_optional_control() {
        let (memory, vm, sdk, _) = fixture();
        let policy = ServiceConfig {
            identity: b"large".to_vec(),
            configuration: vec![0x45; MAX_POLICY_LEN],
        };
        for control in [&[][..], &b"opaque-control"[..]] {
            let mut full = Vec::new();
            PortableSnapshotRef {
                memory: &memory,
                vm_state: &vm,
                sdk: Some(&sdk),
                policy: &policy,
                at: 23,
                sdk_events: 2,
                trace_events: 17,
                trace_schedules: 5,
                tainted: true,
                state_hash: [0xa5; 32],
                control_state: control,
            }
            .write_to(&mut full)
            .unwrap();
            assert_eq!(u16::from_le_bytes(full[8..10].try_into().unwrap()), VERSION);
            let restored = PortableSnapshot::read_from(full.as_slice(), memory.len()).unwrap();
            assert_eq!(restored.memory, memory);
            assert_eq!(restored.vm_state, vm);
            assert_eq!(restored.policy, policy);
            assert_eq!(restored.control_state, control);
            assert_eq!(restored.sdk.unwrap().events, sdk.events);
            let mut trailing = full.clone();
            trailing.push(0);
            assert!(PortableSnapshot::read_from(trailing.as_slice(), memory.len()).is_err());
            let mut corrupt = full;
            let end = corrupt.len() - 1;
            corrupt[end] ^= 1;
            assert!(PortableSnapshot::read_from(corrupt.as_slice(), memory.len()).is_err());
            let sparse = encode_sparse_sidecar(&SparsePortableSidecarRef {
                vm_state: &vm,
                sdk: Some(&sdk),
                policy: &policy,
                at: 23,
                sdk_events: 2,
                trace_events: 17,
                trace_schedules: 5,
                tainted: true,
                state_blob_suffix: b"suffix",
                control_state: control,
            })
            .unwrap();
            assert_eq!(
                u16::from_le_bytes(sparse[8..10].try_into().unwrap()),
                VERSION
            );
            let restored = decode_sparse_sidecar(&sparse).unwrap();
            assert_eq!(restored.vm_state, vm);
            assert_eq!(restored.policy, policy);
            assert_eq!(restored.control_state, control);
            assert_eq!(restored.sdk.unwrap().events, sdk.events);
            assert_eq!(restored.state_blob_suffix, b"suffix");
            let mut trailing = sparse.clone();
            trailing.push(0);
            assert!(decode_sparse_sidecar(&trailing).is_err());
            let mut huge = sparse;
            huge[12..20].copy_from_slice(&u64::MAX.to_le_bytes());
            assert!(matches!(
                decode_sparse_sidecar(&huge),
                Err(PortableSnapshotError::Length {
                    section: "vm_state",
                    ..
                })
            ));
        }
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "large stream size regression; small SDK codecs run under Miri"
    )]
    fn sdk_stream_beyond_legacy_limit_selects_version_six_without_dropping_events() {
        let (memory, vm, mut sdk, policy) = fixture();
        sdk.events = (0..65)
            .map(|at| (at, 9, vec![at as u8; 1024 * 1024]))
            .collect();
        let mut full = Vec::new();
        PortableSnapshotRef {
            memory: &memory,
            vm_state: &vm,
            sdk: Some(&sdk),
            policy: &policy,
            at: 65,
            sdk_events: 65,
            trace_events: 0,
            trace_schedules: 0,
            tainted: false,
            state_hash: [0xa5; 32],
            control_state: &[],
        }
        .write_to(&mut full)
        .unwrap();
        assert_eq!(u16::from_le_bytes(full[8..10].try_into().unwrap()), VERSION);
        let restored = PortableSnapshot::read_from(full.as_slice(), memory.len()).unwrap();
        assert_eq!(restored.sdk.unwrap().events, sdk.events);
        drop(full);
        let sparse = encode_sparse_sidecar(&SparsePortableSidecarRef {
            vm_state: &vm,
            sdk: Some(&sdk),
            policy: &policy,
            at: 65,
            sdk_events: 65,
            trace_events: 0,
            trace_schedules: 0,
            tainted: false,
            state_blob_suffix: &[],
            control_state: &[],
        })
        .unwrap();
        assert_eq!(
            u16::from_le_bytes(sparse[8..10].try_into().unwrap()),
            VERSION
        );
        assert_eq!(
            decode_sparse_sidecar(&sparse).unwrap().sdk.unwrap().events,
            sdk.events
        );
    }

    #[test]
    fn version_selection_preserves_old_layouts_and_admits_each_large_section() {
        assert_eq!(
            complete_version(MAX_VM_STATE_LEN, MAX_SDK_LEN, MAX_POLICY_LEN, 0),
            LEGACY_VERSION
        );
        assert_eq!(
            complete_version(
                MAX_VM_STATE_LEN,
                MAX_SDK_LEN,
                MAX_POLICY_LEN,
                MAX_CONTROL_LEN
            ),
            V5_VERSION
        );
        for lengths in [
            (MAX_VM_STATE_LEN + 1, 0, 0, 0),
            (1, MAX_SDK_LEN + 1, 0, 0),
            (1, 0, MAX_POLICY_LEN + 1, 0),
            (1, 0, 0, MAX_CONTROL_LEN + 1),
        ] {
            assert_eq!(
                complete_version(lengths.0, lengths.1, lengths.2, lengths.3),
                VERSION
            );
            assert_eq!(
                sparse_version(lengths.0, lengths.1, lengths.2, 0, lengths.3),
                VERSION
            );
        }
        assert_eq!(sparse_version(1, 0, 0, MAX_SUFFIX_LEN, 0), LEGACY_VERSION);
        assert_eq!(sparse_version(1, 0, 0, MAX_SUFFIX_LEN + 1, 0), VERSION);
    }

    #[test]
    fn huge_version_six_declaration_reads_only_bounded_actual_bytes() {
        // A v5 header has the same section positions as v6. Remove all body
        // bytes and advertise a representable, enormous vendor-state section.
        let mut header = encoded_with_control_state(b"control");
        header[8..10].copy_from_slice(&VERSION.to_le_bytes());
        header[12..20].copy_from_slice(&0_u64.to_le_bytes());
        header[20..28].copy_from_slice(&(isize::MAX as u64).to_le_bytes());
        header.truncate(116);
        struct BoundedReads<'a> {
            bytes: &'a [u8],
            largest: usize,
        }
        impl Read for BoundedReads<'_> {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                self.largest = self.largest.max(out.len());
                assert!(
                    out.len() <= 64 * 1024,
                    "declared length used as allocation/read size"
                );
                self.bytes.read(out)
            }
        }
        let mut reader = BoundedReads {
            bytes: &header,
            largest: 0,
        };
        let result = PortableSnapshot::read_from(&mut reader, 0);
        assert!(
            matches!(result, Err(PortableSnapshotError::Io(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof)
        );
        assert_eq!(reader.largest, 64 * 1024);
        header[20..28].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(matches!(
            PortableSnapshot::read_from(header.as_slice(), 0),
            Err(PortableSnapshotError::Length {
                section: "vm_state",
                ..
            })
        ));
    }

    fn refresh_digest(bytes: &mut [u8]) {
        let body_len = bytes.len() - 32;
        let digest: [u8; 32] = Sha256::digest(&bytes[..body_len]).into();
        bytes[body_len..].copy_from_slice(&digest);
    }

    fn fixture_with_memory_len(
        memory_len: usize,
    ) -> (Vec<u8>, Vec<u8>, SdkSnapshot, ServiceConfig) {
        let memory = (0..=255).cycle().take(memory_len).collect();
        let vm_state = b"strict-vm-state".to_vec();
        let mut recorded_env = channel::RecordedEnv::nominal(0x5a5a_5a5a_5a5a_5a5a);
        recorded_env
            .set_payloads(Some(vec![vec![4, 5], Vec::new()]))
            .unwrap();
        let sdk = SdkSnapshot {
            recorded: recorded_env.snapshot_state().unwrap(),
            events: vec![(7, 3, vec![1, 2, 3]), (11, 9, Vec::new())],
            pending_snapshot: true,
            pending_stop: None,
            coverage_thresholds: BTreeMap::from([(2, 9), (7, 14)]),
        };
        (memory, vm_state, sdk, ServiceConfig::default())
    }

    fn fixture() -> (Vec<u8>, Vec<u8>, SdkSnapshot, ServiceConfig) {
        fixture_with_memory_len(8192)
    }

    #[test]
    fn wire_constants_are_exact_contract_values() {
        assert_eq!(MAGIC, *b"HMSNAP01");
        assert_eq!(V3_VERSION, 3);
        assert_eq!(LEGACY_VERSION, 4);
        assert_eq!(V5_VERSION, 5);
        assert_eq!(VERSION, 6);
        assert_eq!((FLAG_SDK, FLAG_TAINTED, KNOWN_FLAGS), (1, 2, 3));
        assert_eq!(MAX_VM_STATE_LEN, 16 * 1024 * 1024);
        assert_eq!(MAX_SDK_LEN, 64 * 1024 * 1024);
        assert_eq!(MAX_CONTROL_LEN, MAX_SDK_LEN);
        assert_eq!(MAX_POLICY_LEN, 1024 * 1024);
        assert_eq!(SPARSE_MAGIC, *b"HMSSNAP1");
        assert_eq!(SPARSE_VERSION, VERSION);
        assert_eq!(
            (SPARSE_FLAG_SDK, SPARSE_FLAG_TAINTED, SPARSE_KNOWN_FLAGS),
            (1, 2, 3)
        );
        assert_eq!(MAX_SUFFIX_LEN, 16 * 1024 * 1024);
        assert_eq!(
            MAX_SPARSE_SIDECAR_LEN,
            MAX_VM_STATE_LEN
                + MAX_SDK_LEN
                + MAX_CONTROL_LEN
                + MAX_POLICY_LEN
                + MAX_SUFFIX_LEN
                + 128
        );

        for old_version in [1_u16, 2, VERSION + 1] {
            let mut old = encoded();
            old[8..10].copy_from_slice(&old_version.to_le_bytes());
            refresh_digest(&mut old);
            assert!(matches!(
                PortableSnapshot::read_from(old.as_slice(), 8192),
                Err(PortableSnapshotError::BadVersion(version)) if version == old_version
            ));
        }
    }

    fn encoded_with_memory_len(memory_len: usize) -> Vec<u8> {
        let (memory, vm_state, sdk, policy) = fixture_with_memory_len(memory_len);
        let mut bytes = Vec::new();
        PortableSnapshotRef {
            memory: &memory,
            vm_state: &vm_state,
            sdk: Some(&sdk),
            policy: &policy,
            at: 23,
            sdk_events: 2,
            trace_events: 17,
            trace_schedules: 5,
            tainted: true,
            state_hash: [0xa5; 32],
            control_state: &[],
        }
        .write_to(&mut bytes)
        .unwrap();
        bytes
    }

    fn encoded() -> Vec<u8> {
        encoded_with_memory_len(8192)
    }

    fn encoded_with_control_state(control_state: &[u8]) -> Vec<u8> {
        let (memory, vm_state, sdk, policy) = fixture();
        let mut bytes = Vec::new();
        PortableSnapshotRef {
            memory: &memory,
            vm_state: &vm_state,
            sdk: Some(&sdk),
            policy: &policy,
            at: 23,
            sdk_events: 2,
            trace_events: 17,
            trace_schedules: 5,
            tainted: true,
            state_hash: [0xa5; 32],
            control_state,
        }
        .write_to(&mut bytes)
        .unwrap();
        bytes
    }

    fn sparse_sidecar_fixture() -> Vec<u8> {
        let (_, vm_state, sdk, policy) = fixture();
        encode_sparse_sidecar(&SparsePortableSidecarRef {
            vm_state: &vm_state,
            sdk: Some(&sdk),
            policy: &policy,
            at: 23,
            sdk_events: 2,
            trace_events: 17,
            trace_schedules: 5,
            tainted: true,
            state_blob_suffix: b"canonical-suffix",
            control_state: &[],
        })
        .unwrap()
    }

    fn sparse_sidecar_fixture_with_control_state(control_state: &[u8]) -> Vec<u8> {
        let (_, vm_state, sdk, policy) = fixture();
        encode_sparse_sidecar(&SparsePortableSidecarRef {
            vm_state: &vm_state,
            sdk: Some(&sdk),
            policy: &policy,
            at: 23,
            sdk_events: 2,
            trace_events: 17,
            trace_schedules: 5,
            tainted: true,
            state_blob_suffix: b"canonical-suffix",
            control_state,
        })
        .unwrap()
    }

    #[test]
    fn sparse_sidecar_is_versioned_non_memory_state_only() {
        let bytes = sparse_sidecar_fixture();
        assert_eq!(&bytes[..8], b"HMSSNAP1");
        assert_eq!(
            u16::from_le_bytes(bytes[8..10].try_into().unwrap()),
            LEGACY_VERSION
        );
        let decoded = decode_sparse_sidecar(&bytes).unwrap();
        assert_eq!(decoded.vm_state, b"strict-vm-state");
        assert_eq!(decoded.sdk.as_ref().unwrap().events.len(), 2);
        assert_eq!(decoded.state_blob_suffix, b"canonical-suffix");
        assert_eq!(decoded.at, 23);
        assert_eq!(decoded.sdk_events, 2);
        assert_eq!(decoded.trace_events, 17);
        assert_eq!(decoded.trace_schedules, 5);
        assert!(decoded.tainted);
        assert!(decoded.control_state.is_empty());
        // The sparse sidecar has no memory length, RAM section, or state hash.
        assert!(!bytes.windows(4).any(|tag| tag == b"MEM\0"));
        assert!(!bytes.windows(4).any(|tag| tag == b"SHA2"));
    }

    #[test]
    fn v5_complete_snapshot_round_trips_multiple_control_states() {
        for control_state in [
            vec![0x01],
            vec![0x00, 0xfe, 0xa5],
            (0u8..=255).collect::<Vec<_>>(),
        ] {
            let bytes = encoded_with_control_state(&control_state);
            assert_eq!(
                u16::from_le_bytes(bytes[8..10].try_into().unwrap()),
                V5_VERSION
            );
            // v5 adds one u64 length after the four v4 section lengths.
            assert_eq!(
                u64::from_le_bytes(bytes[44..52].try_into().unwrap()),
                control_state.len() as u64
            );
            let decoded = PortableSnapshot::read_from(bytes.as_slice(), 8192).unwrap();
            assert_eq!(decoded.control_state, control_state);

            let mut reencoded = Vec::new();
            PortableSnapshotRef {
                memory: &decoded.memory,
                vm_state: &decoded.vm_state,
                sdk: decoded.sdk.as_ref(),
                policy: &decoded.policy,
                at: decoded.at,
                sdk_events: decoded.sdk_events,
                trace_events: decoded.trace_events,
                trace_schedules: decoded.trace_schedules,
                tainted: decoded.tainted,
                state_hash: decoded.state_hash,
                control_state: &decoded.control_state,
            }
            .write_to(&mut reencoded)
            .unwrap();
            assert_eq!(reencoded, bytes);
        }
    }

    #[test]
    fn v5_sparse_sidecar_round_trips_multiple_control_states() {
        for control_state in [
            vec![0x01],
            vec![0x00, 0xfe, 0xa5],
            (0u8..=255).collect::<Vec<_>>(),
        ] {
            let bytes = sparse_sidecar_fixture_with_control_state(&control_state);
            assert_eq!(
                u16::from_le_bytes(bytes[8..10].try_into().unwrap()),
                V5_VERSION
            );
            // v5 adds one u64 length after the four v4 sidecar lengths.
            assert_eq!(
                u64::from_le_bytes(bytes[44..52].try_into().unwrap()),
                control_state.len() as u64
            );
            let decoded = decode_sparse_sidecar(&bytes).unwrap();
            assert_eq!(decoded.control_state, control_state);

            let reencoded = encode_sparse_sidecar(&SparsePortableSidecarRef {
                vm_state: &decoded.vm_state,
                sdk: decoded.sdk.as_ref(),
                policy: &decoded.policy,
                at: decoded.at,
                sdk_events: decoded.sdk_events,
                trace_events: decoded.trace_events,
                trace_schedules: decoded.trace_schedules,
                tainted: decoded.tainted,
                state_blob_suffix: &decoded.state_blob_suffix,
                control_state: &decoded.control_state,
            })
            .unwrap();
            assert_eq!(reencoded, bytes);
        }
    }

    #[test]
    fn v5_complete_snapshot_control_state_is_nonempty_bounded_trailing_and_authenticated() {
        let good = encoded_with_control_state(&[1, 2, 3]);

        let mut missing = good.clone();
        missing[44..52].copy_from_slice(&0_u64.to_le_bytes());
        refresh_digest(&mut missing);
        assert!(matches!(
            PortableSnapshot::read_from(missing.as_slice(), 8192),
            Err(PortableSnapshotError::BadFlags)
        ));

        let mut oversized = good.clone();
        oversized[44..52].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(matches!(
            PortableSnapshot::read_from(oversized.as_slice(), 8192),
            Err(PortableSnapshotError::Length {
                section: "control_state",
                ..
            })
        ));

        let mut truncated = good.clone();
        truncated.truncate(truncated.len() - 1);
        assert!(PortableSnapshot::read_from(truncated.as_slice(), 8192).is_err());

        let mut trailing = good.clone();
        trailing.push(0);
        assert!(matches!(
            PortableSnapshot::read_from(trailing.as_slice(), 8192),
            Err(PortableSnapshotError::Malformed("trailing bytes"))
        ));

        // The final body byte belongs to control_state, so it must be covered by
        // the complete-artifact digest just like every earlier section.
        let mut corrupted = good;
        let body_last = corrupted.len() - 33;
        corrupted[body_last] ^= 1;
        assert!(matches!(
            PortableSnapshot::read_from(corrupted.as_slice(), 8192),
            Err(PortableSnapshotError::DigestMismatch)
        ));
    }

    #[test]
    fn v5_sparse_sidecar_control_state_is_nonempty_bounded_trailing_and_total() {
        let good = sparse_sidecar_fixture_with_control_state(&[1, 2, 3]);

        let mut missing = good.clone();
        missing[44..52].copy_from_slice(&0_u64.to_le_bytes());
        assert!(matches!(
            decode_sparse_sidecar(&missing),
            Err(PortableSnapshotError::BadFlags)
        ));

        let mut oversized = good.clone();
        oversized[44..52].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(matches!(
            decode_sparse_sidecar(&oversized),
            Err(PortableSnapshotError::Length {
                section: "control_state",
                ..
            })
        ));

        let mut trailing = good.clone();
        trailing.push(0);
        assert!(matches!(
            decode_sparse_sidecar(&trailing),
            Err(PortableSnapshotError::Malformed("sparse sidecar"))
        ));

        for end in 0..good.len() {
            assert!(
                decode_sparse_sidecar(&good[..end]).is_err(),
                "truncated v5 sidecar prefix {end} was accepted"
            );
        }
    }

    #[test]
    fn sparse_sidecar_rejects_version_flags_and_truncation() {
        let original = sparse_sidecar_fixture();
        for end in 0..original.len() {
            assert!(
                decode_sparse_sidecar(&original[..end]).is_err(),
                "truncated sidecar prefix {end} was accepted"
            );
        }
        let mut bad_version = original.clone();
        bad_version[8..10].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(matches!(
            decode_sparse_sidecar(&bad_version),
            Err(PortableSnapshotError::BadVersion(u16::MAX))
        ));
        for old_version in [1_u16, 2, VERSION + 1] {
            let mut old = original.clone();
            old[8..10].copy_from_slice(&old_version.to_le_bytes());
            assert!(matches!(
                decode_sparse_sidecar(&old),
                Err(PortableSnapshotError::BadVersion(version)) if version == old_version
            ));
        }
        let mut bad_flags = original;
        bad_flags[10..12].copy_from_slice(&0x8000_u16.to_le_bytes());
        assert!(matches!(
            decode_sparse_sidecar(&bad_flags),
            Err(PortableSnapshotError::BadFlags)
        ));
    }

    #[test]
    fn sparse_sidecar_optional_flags_are_independent() {
        let (_, vm_state, sdk, policy) = fixture();
        for (has_sdk, tainted) in [(false, false), (true, false), (false, true), (true, true)] {
            let bytes = encode_sparse_sidecar(&SparsePortableSidecarRef {
                vm_state: &vm_state,
                sdk: has_sdk.then_some(&sdk),
                policy: &policy,
                at: 23,
                sdk_events: 2,
                trace_events: 17,
                trace_schedules: 5,
                tainted,
                state_blob_suffix: b"canonical-suffix",
                control_state: &[],
            })
            .unwrap();
            let decoded = decode_sparse_sidecar(&bytes).unwrap();
            assert_eq!(decoded.sdk.is_some(), has_sdk);
            assert_eq!(decoded.tainted, tainted);
        }
    }

    #[test]
    fn sparse_sidecar_rejects_each_invalid_required_or_optional_length_as_flags() {
        const FLAGS: std::ops::Range<usize> = 10..12;
        const VM_STATE_LEN: std::ops::Range<usize> = 12..20;
        const SDK_LEN: std::ops::Range<usize> = 20..28;
        const POLICY_LEN: std::ops::Range<usize> = 28..36;

        let original = sparse_sidecar_fixture();
        for range in [VM_STATE_LEN, POLICY_LEN] {
            let mut malformed = original.clone();
            malformed[range].copy_from_slice(&0_u64.to_le_bytes());
            assert!(matches!(
                decode_sparse_sidecar(&malformed),
                Err(PortableSnapshotError::BadFlags)
            ));
        }

        {
            let range = SDK_LEN;
            let mut missing_section = original.clone();
            missing_section[range].copy_from_slice(&0_u64.to_le_bytes());
            assert!(matches!(
                decode_sparse_sidecar(&missing_section),
                Err(PortableSnapshotError::BadFlags)
            ));
        }

        {
            let flag = SPARSE_FLAG_SDK;
            let mut missing_flag = original.clone();
            let flags = u16::from_le_bytes(missing_flag[FLAGS.clone()].try_into().unwrap());
            missing_flag[FLAGS.clone()].copy_from_slice(&(flags & !flag).to_le_bytes());
            assert!(matches!(
                decode_sparse_sidecar(&missing_flag),
                Err(PortableSnapshotError::BadFlags)
            ));
        }
    }

    #[test]
    fn snapshot_and_sparse_encoders_reject_empty_vm_state() {
        let memory = [0_u8; 16];
        let policy = ServiceConfig::default();
        let mut artifact = Vec::new();
        assert!(matches!(
            (PortableSnapshotRef {
                memory: &memory,
                vm_state: &[],
                sdk: None,
                policy: &policy,
                at: 0,
                sdk_events: 0,
                trace_events: 0,
                trace_schedules: 0,
                tainted: false,
                state_hash: [0; 32],
                control_state: &[],
            })
            .write_to(&mut artifact),
            Err(PortableSnapshotError::BadFlags)
        ));
        assert!(artifact.is_empty());

        assert!(matches!(
            encode_sparse_sidecar(&SparsePortableSidecarRef {
                vm_state: &[],
                sdk: None,
                policy: &policy,
                at: 0,
                sdk_events: 0,
                trace_events: 0,
                trace_schedules: 0,
                tainted: false,
                state_blob_suffix: b"suffix",
                control_state: &[],
            }),
            Err(PortableSnapshotError::BadFlags)
        ));
    }

    #[test]
    fn snapshot_decoder_rejects_empty_vm_state() {
        let mut malformed = encoded();
        // Header field layout: vm_state length occupies bytes 20..28.
        malformed[20..28].copy_from_slice(&0_u64.to_le_bytes());
        refresh_digest(&mut malformed);
        assert!(matches!(
            PortableSnapshot::read_from(malformed.as_slice(), 8192),
            Err(PortableSnapshotError::BadFlags)
        ));
    }

    #[test]
    fn absent_optional_sections_and_clear_flags_round_trip() {
        let memory = vec![0x5a; 32];
        let policy = ServiceConfig::default();
        let mut bytes = Vec::new();
        PortableSnapshotRef {
            memory: &memory,
            vm_state: b"vm",
            sdk: None,
            policy: &policy,
            at: 1,
            sdk_events: 0,
            trace_events: 0,
            trace_schedules: 0,
            tainted: false,
            state_hash: [7; 32],
            control_state: &[],
        }
        .write_to(&mut bytes)
        .unwrap();
        let decoded = PortableSnapshot::read_from(bytes.as_slice(), memory.len()).unwrap();
        assert!(decoded.sdk.is_none());
        assert!(!decoded.tainted);
    }

    #[test]
    fn every_presence_flag_must_match_its_section_length() {
        // Header offsets: flags=10, sdk length=28.
        {
            let flag = FLAG_SDK;
            let mut flag_without_section = encoded();
            flag_without_section[10..12].copy_from_slice(&(KNOWN_FLAGS & !flag).to_le_bytes());
            refresh_digest(&mut flag_without_section);
            assert!(matches!(
                PortableSnapshot::read_from(flag_without_section.as_slice(), 8192),
                Err(PortableSnapshotError::BadFlags)
            ));

            let memory = vec![0; 16];
            let policy = ServiceConfig::default();
            let mut section_without_flag = Vec::new();
            PortableSnapshotRef {
                memory: &memory,
                vm_state: b"v",
                sdk: None,
                policy: &policy,
                at: 0,
                sdk_events: 0,
                trace_events: 0,
                trace_schedules: 0,
                tainted: false,
                state_hash: [0; 32],
                control_state: &[],
            }
            .write_to(&mut section_without_flag)
            .unwrap();
            section_without_flag[10..12].copy_from_slice(&flag.to_le_bytes());
            refresh_digest(&mut section_without_flag);
            assert!(matches!(
                PortableSnapshot::read_from(section_without_flag.as_slice(), memory.len()),
                Err(PortableSnapshotError::BadFlags)
            ));
        }

        let mut unknown = encoded();
        unknown[10..12].copy_from_slice(&(KNOWN_FLAGS | 0x8000).to_le_bytes());
        refresh_digest(&mut unknown);
        assert!(matches!(
            PortableSnapshot::read_from(unknown.as_slice(), 8192),
            Err(PortableSnapshotError::BadFlags)
        ));
    }

    #[test]
    fn sdk_without_optional_payloads_round_trips() {
        let (_, _, mut sdk, _) = fixture();
        let mut recorded_env = channel::RecordedEnv::nominal(7);
        recorded_env.set_payloads(None).unwrap();
        sdk.recorded = recorded_env.snapshot_state().unwrap();
        let bytes = encode_sdk(&sdk).unwrap();
        assert_eq!(
            decode_sdk(&bytes, VERSION).unwrap().remaining_payloads(),
            None
        );
    }

    #[test]
    fn sdk_decoder_rejects_impossible_counts_and_trailing_bytes() {
        let (_, _, sdk, _) = fixture();
        let mut impossible = encode_sdk(&sdk).unwrap();
        let recorded_len = u64::from_le_bytes(impossible[..8].try_into().unwrap()) as usize;
        let event_count = 8 + recorded_len + 2;
        impossible[event_count..event_count + 8].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(matches!(
            decode_sdk(&impossible, VERSION),
            Err(PortableSnapshotError::Malformed("SDK event count"))
        ));

        let mut trailing = encode_sdk(&sdk).unwrap();
        trailing.push(0);
        assert!(matches!(
            decode_sdk(&trailing, VERSION),
            Err(PortableSnapshotError::Malformed("SDK"))
        ));
    }

    #[test]
    fn sdk_decoder_rejects_empty_or_noncanonical_coverage_tables() {
        let (_, _, mut sdk, _) = fixture();
        sdk.coverage_thresholds.clear();
        let mut empty = encode_sdk(&sdk).unwrap();
        empty.extend_from_slice(b"COVR");
        empty.extend_from_slice(&0_u64.to_le_bytes());
        assert!(matches!(
            decode_sdk(&empty, VERSION),
            Err(PortableSnapshotError::Malformed(
                "SDK coverage threshold count"
            ))
        ));

        let (_, _, sdk, _) = fixture();
        let mut descending = encode_sdk(&sdk).unwrap();
        let tag = descending
            .windows(4)
            .position(|window| window == b"COVR")
            .expect("coverage tag");
        let first_thread = tag + 12;
        let second_thread = first_thread + 12;
        descending[first_thread..first_thread + 4].copy_from_slice(&7_u32.to_le_bytes());
        descending[second_thread..second_thread + 4].copy_from_slice(&2_u32.to_le_bytes());
        assert!(matches!(
            decode_sdk(&descending, VERSION),
            Err(PortableSnapshotError::Malformed("SDK coverage threshold"))
        ));
    }

    #[test]
    fn snapshot_writer_propagates_the_final_flush_error() {
        struct FlushFails(Vec<u8>);
        impl Write for FlushFails {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::Error::other("planted flush failure"))
            }
        }

        let memory = [0; 1];
        let policy = ServiceConfig::default();
        let result = PortableSnapshotRef {
            memory: &memory,
            vm_state: b"v",
            sdk: None,
            policy: &policy,
            at: 0,
            sdk_events: 0,
            trace_events: 0,
            trace_schedules: 0,
            tainted: false,
            state_hash: [0; 32],
            control_state: &[],
        }
        .write_to(FlushFails(Vec::new()));
        assert!(matches!(result, Err(PortableSnapshotError::Io(_))));
    }

    #[test]
    fn snapshot_writer_flushes_the_body_and_the_complete_artifact() {
        use std::{cell::Cell, rc::Rc};

        struct FlushCounter {
            bytes: Vec<u8>,
            flushes: Rc<Cell<usize>>,
        }
        impl Write for FlushCounter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.flushes.set(self.flushes.get() + 1);
                Ok(())
            }
        }

        let flushes = Rc::new(Cell::new(0));
        let memory = [0; 1];
        let policy = ServiceConfig::default();
        PortableSnapshotRef {
            memory: &memory,
            vm_state: b"v",
            sdk: None,
            policy: &policy,
            at: 0,
            sdk_events: 0,
            trace_events: 0,
            trace_schedules: 0,
            tainted: false,
            state_hash: [0; 32],
            control_state: &[],
        }
        .write_to(FlushCounter {
            bytes: Vec::new(),
            flushes: Rc::clone(&flushes),
        })
        .unwrap();
        assert_eq!(flushes.get(), 2);
    }

    #[test]
    fn section_count_bound_uses_division_not_a_loose_product() {
        let mut bytes = 2_u64.to_le_bytes().to_vec();
        bytes.extend_from_slice(&[0; 20]);
        let mut reader = SliceReader::new(&bytes);
        assert!(matches!(
            reader.count("planted count", 20),
            Err(PortableSnapshotError::Malformed("planted count"))
        ));
    }

    #[test]
    fn complete_snapshot_round_trips_byte_exactly() {
        let bytes = encoded();
        assert_eq!(
            u16::from_le_bytes(bytes[8..10].try_into().unwrap()),
            LEGACY_VERSION
        );
        let decoded = PortableSnapshot::read_from(bytes.as_slice(), 8192).unwrap();
        assert_eq!(decoded.memory, fixture().0);
        assert_eq!(decoded.vm_state, b"strict-vm-state");
        assert_eq!(decoded.sdk.as_ref().unwrap().recorded, fixture().2.recorded);
        assert_eq!(decoded.sdk.as_ref().unwrap().events.len(), 2);
        assert_eq!(
            decoded.sdk.as_ref().unwrap().coverage_thresholds,
            BTreeMap::from([(2, 9), (7, 14)])
        );
        assert_eq!(
            decoded.sdk.as_ref().unwrap().remaining_payloads().unwrap()[0],
            [4, 5]
        );
        assert_eq!(decoded.policy, ServiceConfig::default());
        assert_eq!(decoded.at, 23);
        assert_eq!(decoded.sdk_events, 2);
        assert_eq!(decoded.trace_events, 17);
        assert_eq!(decoded.trace_schedules, 5);
        assert!(decoded.tainted);
        assert_eq!(decoded.state_hash, [0xa5; 32]);
        assert!(decoded.control_state.is_empty());
    }

    #[test]
    fn planted_corruption_in_each_load_bearing_section_is_rejected() {
        let original = encoded();
        const HEADER_LEN: usize = 108;
        let section_len = |offset: usize| {
            let mut bytes = [0; 8];
            bytes.copy_from_slice(&original[offset..offset + 8]);
            usize::try_from(u64::from_le_bytes(bytes)).unwrap()
        };
        let memory_len = section_len(12);
        let vm_state_len = section_len(20);
        let sdk_len = section_len(28);
        let policy_len = section_len(36);
        let memory_start = HEADER_LEN;
        let vm_state_start = memory_start + memory_len;
        let sdk_start = vm_state_start + vm_state_len;
        let policy_start = sdk_start + sdk_len;
        assert_eq!(policy_start + policy_len + 32, original.len());
        // Flip RAM, VM state, SDK, and policy bytes; every mutation must
        // reach the independent trailing digest check.
        for index in [memory_start, vm_state_start, sdk_start, policy_start] {
            let mut planted = original.clone();
            planted[index] ^= 1;
            assert!(matches!(
                PortableSnapshot::read_from(planted.as_slice(), 8192),
                Err(PortableSnapshotError::DigestMismatch)
            ));
        }
    }

    #[test]
    fn bad_lengths_and_all_truncations_are_total() {
        // Re-decoding every prefix re-hashes the prefix. Keep the full 8-KiB
        // artifact natively, but avoid quadratic interpreted SHA-256 over
        // thousands of semantically identical bulk-memory prefixes under
        // Miri. The smaller artifact retains every section and the loop still
        // exercises every one of its truncation points.
        let memory_len = if cfg!(miri) { 128 } else { 8192 };
        let bytes = encoded_with_memory_len(memory_len);
        for end in 0..bytes.len() {
            assert!(PortableSnapshot::read_from(&bytes[..end], memory_len).is_err());
        }
        let mut oversized = bytes;
        // vm_state length begins after magic/version/flags/memory length.
        oversized[20..28].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(matches!(
            PortableSnapshot::read_from(oversized.as_slice(), memory_len),
            Err(PortableSnapshotError::Length {
                section: "vm_state",
                ..
            })
        ));
    }

    #[test]
    fn sdk_coverage_threshold_fields_fail_independently() {
        let (_, _, sdk, _) = fixture();
        let bytes = encode_sdk(&sdk).unwrap();
        let covr = bytes
            .windows(4)
            .position(|window| window == b"COVR")
            .expect("fixture has coverage extension");

        let mut zero = bytes.clone();
        zero[covr + 16..covr + 24].copy_from_slice(&0_u64.to_le_bytes());
        assert!(matches!(
            decode_sdk(&zero, VERSION),
            Err(PortableSnapshotError::Malformed("SDK coverage threshold"))
        ));

        let mut duplicate = bytes;
        let first_thread = duplicate[covr + 12..covr + 16].to_vec();
        duplicate[covr + 24..covr + 28].copy_from_slice(&first_thread);
        assert!(matches!(
            decode_sdk(&duplicate, VERSION),
            Err(PortableSnapshotError::Malformed("SDK coverage threshold"))
        ));
    }
    #[test]
    fn pending_stops_round_trip_and_reject_truncated_or_unknown_variants() {
        let stops = [
            None,
            Some(SdkStop::Quiescent),
            Some(SdkStop::Assertion {
                id: 5,
                data: vec![8, 9],
            }),
            Some(SdkStop::Decision {
                moment: 31,
                seq: 42,
                question: channel::Question::with_request_id(77, 19, b"choose".to_vec()).unwrap(),
            }),
        ];
        for stop in stops {
            let (memory, vm_state, mut sdk, policy) = fixture();
            sdk.pending_stop = stop.clone();
            let encoded = encode_sdk(&sdk).unwrap();
            let restored = decode_sdk(&encoded, VERSION).unwrap();
            assert_eq!(restored.pending_stop, stop);
            assert_eq!(restored.recorded.encode(), sdk.recorded.encode());
            assert_eq!(restored.events, sdk.events);
            assert_eq!(restored.coverage_thresholds, sdk.coverage_thresholds);
            let mut full = Vec::new();
            PortableSnapshotRef {
                memory: &memory,
                vm_state: &vm_state,
                sdk: Some(&sdk),
                policy: &policy,
                at: 31,
                sdk_events: 2,
                trace_events: 0,
                trace_schedules: 0,
                tainted: false,
                state_hash: [0; 32],
                control_state: &[],
            }
            .write_to(&mut full)
            .unwrap();
            let restored = PortableSnapshot::read_from(full.as_slice(), memory.len()).unwrap();
            assert_eq!(restored.sdk.unwrap().pending_stop, stop);
            let sparse = encode_sparse_sidecar(&SparsePortableSidecarRef {
                vm_state: &vm_state,
                sdk: Some(&sdk),
                policy: &policy,
                at: 31,
                sdk_events: 2,
                trace_events: 0,
                trace_schedules: 0,
                tainted: false,
                state_blob_suffix: b"suffix",
                control_state: &[],
            })
            .unwrap();
            assert_eq!(
                decode_sparse_sidecar(&sparse)
                    .unwrap()
                    .sdk
                    .unwrap()
                    .pending_stop,
                stop
            );
            let mut bytes = Vec::new();
            encode_pending_stop(&mut bytes, stop.as_ref());
            for end in 0..bytes.len() {
                assert!(decode_pending_stop(&mut SliceReader::new(&bytes[..end])).is_err());
            }
        }
        assert!(decode_pending_stop(&mut SliceReader::new(&[4])).is_err());
        // An excessive request length must fail before a Question can be built.
        let mut bytes = vec![3];
        bytes.extend_from_slice(&[0; 22]);
        bytes.extend_from_slice(&u64::MAX.to_le_bytes());
        assert!(decode_pending_stop(&mut SliceReader::new(&bytes)).is_err());
    }

    #[test]
    fn version_three_complete_and_sparse_artifacts_remain_readable() {
        // Emitted by the version-3 writer at 7c1f9e89, not reconstructed by
        // this implementation. See tests/fixtures/README.md for provenance.
        let (memory, _, sdk, _) = fixture();
        let full = include_bytes!("../tests/fixtures/sdk-v3-full.bin");
        let decoded = PortableSnapshot::read_from(full.as_slice(), memory.len()).unwrap();
        let decoded_sdk = decoded.sdk.unwrap();
        assert_eq!(decoded.memory, memory);
        assert_eq!(decoded_sdk.pending_stop, None);
        assert_eq!(decoded_sdk.recorded.encode(), sdk.recorded.encode());
        assert_eq!(decoded_sdk.events, sdk.events);
        let sparse = include_bytes!("../tests/fixtures/sdk-v3-sparse.bin");
        let decoded = decode_sparse_sidecar(sparse).unwrap();
        assert_eq!(decoded.sdk.unwrap().pending_stop, None);
        assert_eq!(decoded.state_blob_suffix, b"canonical-suffix");
    }

    #[test]
    fn version_four_complete_and_sparse_artifacts_remain_byte_stable() {
        // Emitted by the pre-control-state writer at c950d497, not reconstructed
        // by this implementation. See tests/fixtures/README.md for provenance.
        let full = include_bytes!("../tests/fixtures/sdk-v4-full-pending.bin");
        assert_eq!(
            u16::from_le_bytes(full[8..10].try_into().unwrap()),
            LEGACY_VERSION
        );
        let decoded = PortableSnapshot::read_from(full.as_slice(), 8192).unwrap();
        assert!(decoded.control_state.is_empty());
        assert_eq!(
            decoded.sdk.as_ref().unwrap().pending_stop,
            Some(SdkStop::Decision {
                moment: 47,
                seq: 3,
                question: channel::Question::with_request_id(
                    0x1234,
                    7,
                    b"portable-v4-decision".to_vec(),
                )
                .unwrap(),
            })
        );
        let mut reencoded = Vec::new();
        PortableSnapshotRef {
            memory: &decoded.memory,
            vm_state: &decoded.vm_state,
            sdk: decoded.sdk.as_ref(),
            policy: &decoded.policy,
            at: decoded.at,
            sdk_events: decoded.sdk_events,
            trace_events: decoded.trace_events,
            trace_schedules: decoded.trace_schedules,
            tainted: decoded.tainted,
            state_hash: decoded.state_hash,
            control_state: &decoded.control_state,
        }
        .write_to(&mut reencoded)
        .unwrap();
        assert_eq!(reencoded.as_slice(), full);

        let sparse = include_bytes!("../tests/fixtures/sdk-v4-sparse-pending.bin");
        assert_eq!(
            u16::from_le_bytes(sparse[8..10].try_into().unwrap()),
            LEGACY_VERSION
        );
        let decoded = decode_sparse_sidecar(sparse).unwrap();
        assert!(decoded.control_state.is_empty());
        assert_eq!(
            decoded.sdk.as_ref().unwrap().pending_stop,
            Some(SdkStop::Decision {
                moment: 47,
                seq: 3,
                question: channel::Question::with_request_id(
                    0x1234,
                    7,
                    b"portable-v4-decision".to_vec(),
                )
                .unwrap(),
            })
        );
        let reencoded = encode_sparse_sidecar(&SparsePortableSidecarRef {
            vm_state: &decoded.vm_state,
            sdk: decoded.sdk.as_ref(),
            policy: &decoded.policy,
            at: decoded.at,
            sdk_events: decoded.sdk_events,
            trace_events: decoded.trace_events,
            trace_schedules: decoded.trace_schedules,
            tainted: decoded.tainted,
            state_blob_suffix: &decoded.state_blob_suffix,
            control_state: &decoded.control_state,
        })
        .unwrap();
        assert_eq!(reencoded.as_slice(), sparse);
    }
}
