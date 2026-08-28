// SPDX-License-Identifier: AGPL-3.0-or-later
//! Host-neutral control-snapshot artifacts.
//!
//! A snapshot-store layer is not portable by itself: the control server keeps
//! the SDK stream, remaining ordered payloads, network-decision prefix, active
//! fault policy, evidence cut, and lineage taint in handle-keyed side tables.
//! This module serializes that complete replay state together with materialized
//! RAM and the canonical vendor VM-state blob. The format is fixed-order,
//! little-endian, length-bounded, and protected by a trailing SHA-256 digest.

use std::{
    collections::BTreeMap,
    io::{Read, Write},
};

use environment::{Answer, EnvError, FaultPolicy};
use sha2::{Digest, Sha256};

use crate::snapshot::SnapshotError;
use crate::vmm::{NetSnapshot, SdkSnapshot};

const MAGIC: [u8; 8] = *b"HMSNAP01";
const VERSION: u16 = 1;
const FLAG_SDK: u16 = 1 << 0;
const FLAG_NET: u16 = 1 << 1;
const FLAG_TAINTED: u16 = 1 << 2;
const KNOWN_FLAGS: u16 = FLAG_SDK | FLAG_NET | FLAG_TAINTED;

const MAX_VM_STATE_LEN: usize = 16 * 1024 * 1024;
const MAX_SDK_LEN: usize = 64 * 1024 * 1024;
const MAX_NET_LEN: usize = 64 * 1024 * 1024;
const MAX_POLICY_LEN: usize = 1024 * 1024;

/// A complete decoded portable snapshot.
pub(crate) struct PortableSnapshot {
    pub(crate) memory: Vec<u8>,
    pub(crate) vm_state: Vec<u8>,
    pub(crate) sdk: Option<SdkSnapshot>,
    pub(crate) net: Option<NetSnapshot>,
    pub(crate) policy: FaultPolicy,
    pub(crate) at: u64,
    pub(crate) sdk_events: u64,
    pub(crate) trace_events: u64,
    pub(crate) trace_schedules: u64,
    pub(crate) tainted: bool,
    pub(crate) state_hash: [u8; 32],
}

/// Borrowed form used while streaming an existing store layer to disk.
pub(crate) struct PortableSnapshotRef<'a> {
    pub(crate) memory: &'a [u8],
    pub(crate) vm_state: &'a [u8],
    pub(crate) sdk: Option<&'a SdkSnapshot>,
    pub(crate) net: Option<&'a NetSnapshot>,
    pub(crate) policy: &'a FaultPolicy,
    pub(crate) at: u64,
    pub(crate) sdk_events: u64,
    pub(crate) trace_events: u64,
    pub(crate) trace_schedules: u64,
    pub(crate) tainted: bool,
    pub(crate) state_hash: [u8; 32],
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
    /// The embedded fault policy or network answer was malformed.
    #[error("portable snapshot environment state malformed")]
    Environment(#[from] EnvError),
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
        let net = self.net.map(encode_net).transpose()?;
        let policy = self.policy.to_bytes();
        check_len("memory", self.memory.len(), self.memory.len())?;
        check_len("vm_state", self.vm_state.len(), MAX_VM_STATE_LEN)?;
        check_len("sdk", sdk.as_ref().map_or(0, Vec::len), MAX_SDK_LEN)?;
        check_len("net", net.as_ref().map_or(0, Vec::len), MAX_NET_LEN)?;
        check_len("policy", policy.len(), MAX_POLICY_LEN)?;

        let mut flags = 0;
        if sdk.is_some() {
            flags |= FLAG_SDK;
        }
        if net.is_some() {
            flags |= FLAG_NET;
        }
        if self.tainted {
            flags |= FLAG_TAINTED;
        }

        let mut out = HashWriter::new(&mut writer);
        out.write_all(&MAGIC)?;
        put_u16(&mut out, VERSION)?;
        put_u16(&mut out, flags)?;
        put_len(&mut out, self.memory.len())?;
        put_len(&mut out, self.vm_state.len())?;
        put_len(&mut out, sdk.as_ref().map_or(0, Vec::len))?;
        put_len(&mut out, net.as_ref().map_or(0, Vec::len))?;
        put_len(&mut out, policy.len())?;
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
        if let Some(bytes) = &net {
            out.write_all(bytes)?;
        }
        out.write_all(&policy)?;
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
        if version != VERSION {
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
        let vm_state_len = bounded_len(&mut input, "vm_state", MAX_VM_STATE_LEN)?;
        let sdk_len = bounded_len(&mut input, "sdk", MAX_SDK_LEN)?;
        let net_len = bounded_len(&mut input, "net", MAX_NET_LEN)?;
        let policy_len = bounded_len(&mut input, "policy", MAX_POLICY_LEN)?;
        let has_sdk = flags & FLAG_SDK != 0;
        let has_net = flags & FLAG_NET != 0;
        if has_sdk != (sdk_len != 0) || has_net != (net_len != 0) || policy_len == 0 {
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
        let net_bytes = read_vec(&mut input, net_len)?;
        let policy_bytes = read_vec(&mut input, policy_len)?;
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
        let sdk = has_sdk.then(|| decode_sdk(&sdk_bytes)).transpose()?;
        let net = has_net.then(|| decode_net(&net_bytes)).transpose()?;
        let policy = FaultPolicy::from_bytes(&policy_bytes)?;
        Ok(Self {
            memory,
            vm_state,
            sdk,
            net,
            policy,
            at,
            sdk_events,
            trace_events,
            trace_schedules,
            tainted: flags & FLAG_TAINTED != 0,
            state_hash,
        })
    }
}

fn encode_sdk(sdk: &SdkSnapshot) -> Result<Vec<u8>, PortableSnapshotError> {
    let mut out = Vec::new();
    out.extend_from_slice(&sdk.stream);
    out.push(u8::from(sdk.pending_snapshot));
    put_vec_len(&mut out, sdk.events.len());
    for (moment, local, payload) in &sdk.events {
        out.extend_from_slice(&moment.to_le_bytes());
        out.extend_from_slice(&local.to_le_bytes());
        put_vec_len(&mut out, payload.len());
        out.extend_from_slice(payload);
    }
    match &sdk.payloads {
        None => out.push(0),
        Some(payloads) => {
            out.push(1);
            put_vec_len(&mut out, payloads.len());
            for payload in payloads {
                put_vec_len(&mut out, payload.len());
                out.extend_from_slice(payload);
            }
        }
    }
    if !sdk.coverage_thresholds.is_empty() {
        out.extend_from_slice(b"COVR");
        put_vec_len(&mut out, sdk.coverage_thresholds.len());
        for (thread, threshold) in &sdk.coverage_thresholds {
            out.extend_from_slice(&thread.to_le_bytes());
            out.extend_from_slice(&threshold.to_le_bytes());
        }
    }
    check_len("sdk", out.len(), MAX_SDK_LEN)?;
    Ok(out)
}

fn decode_sdk(bytes: &[u8]) -> Result<SdkSnapshot, PortableSnapshotError> {
    let mut input = SliceReader::new(bytes);
    let mut stream = [0; 16];
    stream.copy_from_slice(input.take(16)?);
    let pending_snapshot = input.boolean()?;
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
    let payloads = match input.u8()? {
        0 => None,
        1 => {
            let count = input.count("SDK payload count", 8)?;
            let mut payloads = Vec::new();
            payloads
                .try_reserve_exact(count)
                .map_err(|_| PortableSnapshotError::Malformed("SDK payload allocation"))?;
            for _ in 0..count {
                payloads.push(input.bytes()?);
            }
            Some(payloads)
        }
        _ => return Err(PortableSnapshotError::Malformed("SDK payload option")),
    };
    let mut coverage_thresholds = BTreeMap::new();
    if input.remaining() != 0 {
        if input.take(4)? != b"COVR" {
            return Err(PortableSnapshotError::Malformed("SDK coverage tag"));
        }
        let count = input.count("SDK coverage threshold count", 12)?;
        for _ in 0..count {
            let thread = input.u32()?;
            let threshold = input.u64()?;
            if threshold == 0 || coverage_thresholds.insert(thread, threshold).is_some() {
                return Err(PortableSnapshotError::Malformed("SDK coverage threshold"));
            }
        }
    }
    input.finish("SDK")?;
    Ok(SdkSnapshot {
        stream,
        events,
        pending_snapshot,
        payloads,
        coverage_thresholds,
    })
}

fn encode_net(net: &NetSnapshot) -> Result<Vec<u8>, PortableSnapshotError> {
    let mut out = Vec::new();
    put_vec_len(&mut out, net.decisions.len());
    for (moment, connection, answer) in &net.decisions {
        out.extend_from_slice(&moment.to_le_bytes());
        out.extend_from_slice(&connection.to_le_bytes());
        let answer = answer.encode();
        put_vec_len(&mut out, answer.len());
        out.extend_from_slice(&answer);
    }
    check_len("net", out.len(), MAX_NET_LEN)?;
    Ok(out)
}

fn decode_net(bytes: &[u8]) -> Result<NetSnapshot, PortableSnapshotError> {
    let mut input = SliceReader::new(bytes);
    let count = input.count("Net decision count", 24)?;
    let mut decisions = Vec::new();
    decisions
        .try_reserve_exact(count)
        .map_err(|_| PortableSnapshotError::Malformed("Net decision allocation"))?;
    for _ in 0..count {
        let moment = input.u64()?;
        let connection = input.u64()?;
        let answer = Answer::decode(&input.bytes()?)?;
        decisions.push((moment, connection, answer));
    }
    input.finish("Net")?;
    Ok(NetSnapshot { decisions })
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
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(len)
        .map_err(|_| PortableSnapshotError::Malformed("section allocation"))?;
    bytes.resize(len, 0);
    input.read_exact(&mut bytes)?;
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
        Ok(self.take(len)?.to_vec())
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

    fn fixture_with_memory_len(
        memory_len: usize,
    ) -> (Vec<u8>, Vec<u8>, SdkSnapshot, NetSnapshot, FaultPolicy) {
        let memory = (0..=255).cycle().take(memory_len).collect();
        let vm_state = b"strict-vm-state".to_vec();
        let sdk = SdkSnapshot {
            stream: [0x5a; 16],
            events: vec![(7, 3, vec![1, 2, 3]), (11, 9, Vec::new())],
            pending_snapshot: true,
            payloads: Some(vec![vec![4, 5], Vec::new()]),
            coverage_thresholds: BTreeMap::from([(2, 9), (7, 14)]),
        };
        let net = NetSnapshot {
            decisions: vec![(13, 17, Answer::Nominal)],
        };
        (memory, vm_state, sdk, net, FaultPolicy::none())
    }

    fn fixture() -> (Vec<u8>, Vec<u8>, SdkSnapshot, NetSnapshot, FaultPolicy) {
        fixture_with_memory_len(8192)
    }

    fn encoded_with_memory_len(memory_len: usize) -> Vec<u8> {
        let (memory, vm_state, sdk, net, policy) = fixture_with_memory_len(memory_len);
        let mut bytes = Vec::new();
        PortableSnapshotRef {
            memory: &memory,
            vm_state: &vm_state,
            sdk: Some(&sdk),
            net: Some(&net),
            policy: &policy,
            at: 23,
            sdk_events: 2,
            trace_events: 17,
            trace_schedules: 5,
            tainted: true,
            state_hash: [0xa5; 32],
        }
        .write_to(&mut bytes)
        .unwrap();
        bytes
    }

    fn encoded() -> Vec<u8> {
        encoded_with_memory_len(8192)
    }

    #[test]
    fn complete_snapshot_round_trips_byte_exactly() {
        let bytes = encoded();
        let decoded = PortableSnapshot::read_from(bytes.as_slice(), 8192).unwrap();
        assert_eq!(decoded.memory, fixture().0);
        assert_eq!(decoded.vm_state, b"strict-vm-state");
        assert_eq!(decoded.sdk.as_ref().unwrap().stream, [0x5a; 16]);
        assert_eq!(decoded.sdk.as_ref().unwrap().events.len(), 2);
        assert_eq!(
            decoded.sdk.as_ref().unwrap().coverage_thresholds,
            BTreeMap::from([(2, 9), (7, 14)])
        );
        assert_eq!(
            decoded.sdk.as_ref().unwrap().payloads.as_ref().unwrap()[0],
            [4, 5]
        );
        assert_eq!(decoded.net.as_ref().unwrap().decisions.len(), 1);
        assert_eq!(decoded.policy, FaultPolicy::none());
        assert_eq!(decoded.at, 23);
        assert_eq!(decoded.sdk_events, 2);
        assert_eq!(decoded.trace_events, 17);
        assert_eq!(decoded.trace_schedules, 5);
        assert!(decoded.tainted);
        assert_eq!(decoded.state_hash, [0xa5; 32]);
    }

    #[test]
    fn planted_corruption_in_each_load_bearing_section_is_rejected() {
        let original = encoded();
        const HEADER_LEN: usize = 116;
        let section_len = |offset: usize| {
            let mut bytes = [0; 8];
            bytes.copy_from_slice(&original[offset..offset + 8]);
            usize::try_from(u64::from_le_bytes(bytes)).unwrap()
        };
        let memory_len = section_len(12);
        let vm_state_len = section_len(20);
        let sdk_len = section_len(28);
        let net_len = section_len(36);
        let policy_len = section_len(44);
        let memory_start = HEADER_LEN;
        let vm_state_start = memory_start + memory_len;
        let sdk_start = vm_state_start + vm_state_len;
        let net_start = sdk_start + sdk_len;
        let policy_start = net_start + net_len;
        assert_eq!(policy_start + policy_len + 32, original.len());
        // Flip RAM, VM state, SDK, Net, and policy bytes; every mutation must
        // reach the independent trailing digest check.
        for index in [
            memory_start,
            vm_state_start,
            sdk_start,
            net_start,
            policy_start,
        ] {
            let mut planted = original.clone();
            planted[index] ^= 1;
            assert!(matches!(
                PortableSnapshot::read_from(planted.as_slice(), 8192),
                Err(PortableSnapshotError::DigestMismatch)
            ));
        }
    }

    #[test]
    fn hostile_lengths_and_all_truncations_are_total() {
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
        let (_, _, sdk, _, _) = fixture();
        let bytes = encode_sdk(&sdk).unwrap();
        let covr = bytes
            .windows(4)
            .position(|window| window == b"COVR")
            .expect("fixture has coverage extension");

        let mut zero = bytes.clone();
        zero[covr + 16..covr + 24].copy_from_slice(&0_u64.to_le_bytes());
        assert!(matches!(
            decode_sdk(&zero),
            Err(PortableSnapshotError::Malformed("SDK coverage threshold"))
        ));

        let mut duplicate = bytes;
        let first_thread = duplicate[covr + 12..covr + 16].to_vec();
        duplicate[covr + 24..covr + 28].copy_from_slice(&first_thread);
        assert!(matches!(
            decode_sdk(&duplicate),
            Err(PortableSnapshotError::Malformed("SDK coverage threshold"))
        ));
    }
}
