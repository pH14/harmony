// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **crash-safe append-only campaign evidence ledger** (`hm-bbx.4`).
//!
//! The generic Explorer owns the durable append of every completed, normalized
//! evidence batch **before** it can be submitted at a revision. This is the
//! authority for evidence payloads — full-retention, crash-safe append/replay,
//! keyed by the durable batch identity ([`revision_coordinator::EvidenceBatchId`],
//! the blake3 digest the Revision coordinator commits). On restart the ledger
//! replays its durable frames and rebuilds the canonical inputs; a torn tail (a
//! crash mid-append) is repaired by truncation, interior damage is a typed error,
//! never a silent short read.
//!
//! ## TraceStore is referenced backing, not the relational authority
//!
//! The strategy is explicit: the `TraceStore` "may remain payload backing for
//! immutable reproducers or journals referenced by digest and format version, but
//! it is not the evidence ledger." So the ledger owns the evidence *and* holds a
//! [`TraceStore`] of the large immutable payloads (the reproducers) it references
//! by digest. Two invariants hold by construction:
//!
//! - **A live ledger reference cannot be invalidated.** Every retained evidence
//!   entry keeps its referenced payload live ([`EvidenceLedger::live_references`]).
//! - **TraceStore retention cannot delete a live reference.**
//!   [`TraceStore::retain`] only ever removes payloads absent from the live set,
//!   so a payload a live entry references survives every retention pass.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use blake3::Hasher;
use revision_coordinator::EvidenceBatchId;

use crate::evidence::CompletedRunEvidence;

/// The ledger file magic and format version — a header a foreign or future file
/// is rejected against, never silently reinterpreted.
const MAGIC: [u8; 4] = *b"HEVL";
const VERSION: u32 = 1;
/// The frame header: `len(u32) + payload_digest(32)`. A frame with fewer bytes
/// than this remaining is a torn tail.
const FRAME_HEADER: usize = 4 + 32;
/// A sanity bound on one frame's payload (evidence batches are small normalized
/// records; anything larger is a corrupt length, not a real frame).
const MAX_FRAME_PAYLOAD: usize = 64 << 20;

/// A reference to an immutable payload in the [`TraceStore`] backing: the payload
/// digest plus the format version it was written under (so a later decoder can
/// audit or migrate it). Referenced by digest, never inlined into the relational
/// authority.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct PayloadRef {
    /// The blake3 digest of the referenced payload bytes.
    pub digest: [u8; 32],
    /// The format version the payload was written under.
    pub format_version: u16,
}

/// Typed, no-panic ledger errors.
#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    /// An I/O failure reaching the durable file.
    #[error("evidence ledger I/O: {0}")]
    Io(#[from] std::io::Error),
    /// The durable stream is damaged in its interior (not merely a torn tail): a
    /// bad magic, a frame whose payload digest disagrees, or an oversized length.
    #[error("evidence ledger corrupt at offset {offset}: {detail}")]
    Corrupt {
        /// The byte offset the damage was detected at.
        offset: u64,
        /// A human-readable detail.
        detail: String,
    },
    /// The file was written by an unsupported ledger version.
    #[error("evidence ledger version {found} unsupported (this build writes {VERSION})")]
    UnsupportedVersion {
        /// The version found in the file header.
        found: u32,
    },
    /// A persisted evidence payload failed to decode (a tampered or version-skewed
    /// artifact) — surfaced as corruption, never a panic.
    #[error("evidence ledger payload at offset {offset} did not decode: {detail}")]
    BadPayload {
        /// The byte offset of the offending frame.
        offset: u64,
        /// A human-readable detail.
        detail: String,
    },
}

/// The referenced immutable-payload backing (the `TraceStore` stand-in in this
/// crate): a content-addressed store of large immutable payloads (reproducers,
/// journals) referenced by [`PayloadRef`]. It is **not** the relational
/// authority — the [`EvidenceLedger`] is — and its retention honors live
/// references.
#[derive(Clone, Debug, Default)]
pub struct TraceStore {
    payloads: BTreeMap<[u8; 32], (u16, Vec<u8>)>,
}

impl TraceStore {
    /// An empty payload store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Store `bytes` under `format_version`, returning the content reference. Idempotent —
    /// re-putting identical bytes returns the same reference.
    pub fn put(&mut self, bytes: &[u8], format_version: u16) -> PayloadRef {
        let digest = *blake3::hash(bytes).as_bytes();
        self.payloads
            .entry(digest)
            .or_insert_with(|| (format_version, bytes.to_vec()));
        PayloadRef {
            digest,
            format_version,
        }
    }

    /// The bytes behind a reference, if present.
    pub fn get(&self, r: &PayloadRef) -> Option<&[u8]> {
        self.payloads.get(&r.digest).map(|(_, b)| b.as_slice())
    }

    /// Whether a reference resolves.
    pub fn contains(&self, r: &PayloadRef) -> bool {
        self.payloads.contains_key(&r.digest)
    }

    /// The number of stored payloads.
    pub fn len(&self) -> usize {
        self.payloads.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.payloads.is_empty()
    }

    /// Retain exactly the payloads in `live`, dropping every other. **A live
    /// reference is never deleted** — this is the strategy's "TraceStore
    /// retention cannot delete a live reference": retention is a set intersection
    /// with the live set, so a payload a live ledger entry references always
    /// survives. Returns how many payloads were reclaimed.
    pub fn retain(&mut self, live: &BTreeSet<[u8; 32]>) -> usize {
        let before = self.payloads.len();
        self.payloads.retain(|digest, _| live.contains(digest));
        before - self.payloads.len()
    }
}

/// The crash-safe append-only evidence ledger (module doc).
#[derive(Debug)]
pub struct EvidenceLedger {
    file: File,
    path: PathBuf,
    /// The durable evidence, keyed by its committed batch identity. Rebuilt from
    /// the file on [`open`](Self::open); appended to on [`append`](Self::append).
    entries: BTreeMap<EvidenceBatchId, CompletedRunEvidence>,
    /// The referenced immutable-payload backing (reproducers), kept live for
    /// every retained entry.
    store: TraceStore,
    /// The reproducer reference each entry keeps live (`batch → its payload`), so
    /// [`live_references`](Self::live_references) is exact.
    refs: BTreeMap<EvidenceBatchId, PayloadRef>,
}

/// The format version reproducer payloads are stored under in the [`TraceStore`]
/// (the `Reproducer::blob_version` is carried separately; this versions the
/// *ledger's* payload framing, not the blob).
const REPRODUCER_FORMAT_VERSION: u16 = 1;

impl EvidenceLedger {
    /// Open (creating if absent) the durable evidence ledger at `path`, replaying
    /// every durable frame to rebuild the in-memory authority. A torn tail (a
    /// crash mid-append) is truncated and the repair fsynced before the ledger is
    /// exposed, so replay only ever returns whole, checksum-verified batches;
    /// interior damage is a typed [`LedgerError::Corrupt`].
    pub fn open(path: &Path) -> Result<Self, LedgerError> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        let end = file.seek(SeekFrom::End(0))?;
        let mut entries = BTreeMap::new();
        let mut store = TraceStore::new();
        let mut refs = BTreeMap::new();

        if end == 0 {
            // Fresh file: write the header and fsync it durable.
            file.seek(SeekFrom::Start(0))?;
            file.write_all(&MAGIC)?;
            file.write_all(&VERSION.to_le_bytes())?;
            file.sync_data()?;
        } else {
            file.seek(SeekFrom::Start(0))?;
            let mut hdr = [0u8; 8];
            file.read_exact(&mut hdr)
                .map_err(|_| LedgerError::Corrupt {
                    offset: 0,
                    detail: "file too short for a header".into(),
                })?;
            if hdr[0..4] != MAGIC {
                return Err(LedgerError::Corrupt {
                    offset: 0,
                    detail: "bad magic".into(),
                });
            }
            let found = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]);
            if found != VERSION {
                return Err(LedgerError::UnsupportedVersion { found });
            }
            let good_end =
                Self::replay_frames(&mut file, end, &mut entries, &mut store, &mut refs)?;
            if good_end < end {
                // A torn tail: truncate to the last whole frame and fsync the repair
                // before exposing replay.
                file.set_len(good_end)?;
                file.sync_data()?;
            }
        }
        file.seek(SeekFrom::End(0))?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
            entries,
            store,
            refs,
        })
    }

    /// Replay the durable frames after the header, rebuilding the authority.
    /// Returns the offset of the end of the last **whole, verified** frame (== `end`
    /// on a clean file, `< end` when a torn tail was found).
    fn replay_frames(
        file: &mut File,
        end: u64,
        entries: &mut BTreeMap<EvidenceBatchId, CompletedRunEvidence>,
        store: &mut TraceStore,
        refs: &mut BTreeMap<EvidenceBatchId, PayloadRef>,
    ) -> Result<u64, LedgerError> {
        let mut pos: u64 = 8;
        file.seek(SeekFrom::Start(pos))?;
        loop {
            if pos == end {
                return Ok(pos);
            }
            // A partial frame header at the tail is a torn write — stop here.
            if end - pos < FRAME_HEADER as u64 {
                return Ok(pos);
            }
            let mut header = [0u8; FRAME_HEADER];
            file.read_exact(&mut header)?;
            let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
            let digest: [u8; 32] = header[4..36].try_into().expect("36-byte header");
            if len > MAX_FRAME_PAYLOAD {
                return Err(LedgerError::Corrupt {
                    offset: pos,
                    detail: format!("frame length {len} exceeds the bound"),
                });
            }
            // A payload truncated by a crash is a torn tail — stop cleanly.
            if (end - pos - FRAME_HEADER as u64) < len as u64 {
                return Ok(pos);
            }
            let mut payload = vec![0u8; len];
            file.read_exact(&mut payload)?;
            if *blake3::hash(&payload).as_bytes() != digest {
                // A verified payload digest that disagrees is interior damage, not
                // a torn tail (the length and digest were fully present).
                return Err(LedgerError::Corrupt {
                    offset: pos,
                    detail: "frame payload digest mismatch".into(),
                });
            }
            let evidence: CompletedRunEvidence =
                serde_json::from_slice(&payload).map_err(|e| LedgerError::BadPayload {
                    offset: pos,
                    detail: e.to_string(),
                })?;
            let id = EvidenceBatchId::digest(&payload);
            Self::index(entries, store, refs, id, evidence);
            pos += FRAME_HEADER as u64 + len as u64;
        }
    }

    /// Index one replayed/appended batch: retain the evidence, store its
    /// reproducer payload as referenced backing, and keep that reference live.
    fn index(
        entries: &mut BTreeMap<EvidenceBatchId, CompletedRunEvidence>,
        store: &mut TraceStore,
        refs: &mut BTreeMap<EvidenceBatchId, PayloadRef>,
        id: EvidenceBatchId,
        evidence: CompletedRunEvidence,
    ) {
        let payload_ref = store.put(&evidence.env.bytes, REPRODUCER_FORMAT_VERSION);
        refs.insert(id, payload_ref);
        entries.insert(id, evidence);
    }

    /// Durably append one completed run's normalized evidence, returning its
    /// batch identity — the id the caller then submits to the Revision
    /// coordinator for commit. The frame is written and **fsynced durable before
    /// return**, so once this returns `Ok` the batch survives a crash; a crash
    /// before it returns leaves at most a torn tail the next
    /// [`open`](Self::open) repairs. Appending byte-identical evidence twice is
    /// idempotent (same digest, one durable copy is enough).
    pub fn append(
        &mut self,
        evidence: &CompletedRunEvidence,
    ) -> Result<EvidenceBatchId, LedgerError> {
        let payload = evidence.canonical_bytes();
        let id = EvidenceBatchId::digest(&payload);
        if self.entries.contains_key(&id) {
            // Already durable — the digest is content-addressed, so re-appending
            // adds nothing. (Idempotent retry, exactly like the coordinator's
            // byte-identical commit.)
            return Ok(id);
        }
        let mut hasher = Hasher::new();
        hasher.update(&payload);
        let digest = *hasher.finalize().as_bytes();
        let mut frame = Vec::with_capacity(FRAME_HEADER + payload.len());
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        frame.extend_from_slice(&digest);
        frame.extend_from_slice(&payload);
        self.file.write_all(&frame)?;
        self.file.sync_data()?;
        Self::index(
            &mut self.entries,
            &mut self.store,
            &mut self.refs,
            id,
            evidence.clone(),
        );
        Ok(id)
    }

    /// The durable evidence behind a batch identity, if present.
    pub fn get(&self, id: &EvidenceBatchId) -> Option<&CompletedRunEvidence> {
        self.entries.get(id)
    }

    /// Whether a batch identity is durably present.
    pub fn contains(&self, id: &EvidenceBatchId) -> bool {
        self.entries.contains_key(id)
    }

    /// The number of durable batches.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the ledger holds no batches.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Every durable batch identity, in canonical order — the canonical inputs a
    /// restart rebuilds views from.
    pub fn batch_ids(&self) -> impl Iterator<Item = &EvidenceBatchId> {
        self.entries.keys()
    }

    /// The referenced payload backing (the `TraceStore` stand-in), read-only.
    pub fn trace_store(&self) -> &TraceStore {
        &self.store
    }

    /// The set of payload digests **live** references keep alive — the exact set a
    /// `TraceStore` retention pass must preserve. A live ledger reference can
    /// never be invalidated: every retained entry contributes its reproducer's
    /// digest here.
    pub fn live_references(&self) -> BTreeSet<[u8; 32]> {
        self.refs.values().map(|r| r.digest).collect()
    }

    /// Run a `TraceStore` retention pass honoring the live references: only
    /// payloads no retained entry references are reclaimed. Returns the count
    /// reclaimed. A live reference is guaranteed to survive.
    pub fn retain_live_payloads(&mut self) -> usize {
        let live = self.refs.values().map(|r| r.digest).collect();
        self.store.retain(&live)
    }

    /// The ledger's durable path (for reopening on restart).
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::RunId;
    use crate::spine::{EvidenceCut, Moment};
    use crate::{Reproducer, StopReason};
    use sdk_events::decode_antithesis;

    fn evidence(issue: u64, blob: &[u8]) -> CompletedRunEvidence {
        // A tiny normalized artifact (one always-true assertion).
        let n = decode_antithesis(&[(
            sdk_events::Moment(1),
            br#"{"antithesis_assert":{"assert_type":"always","condition":true,"message":"m"}}"#
                .to_vec(),
        )])
        .expect("decodes");
        CompletedRunEvidence {
            rollout: RunId {
                issue,
                parent: None,
            },
            terminal: StopReason::Quiescent { vtime: Moment(100) },
            env: Reproducer {
                blob_version: 1,
                bytes: blob.to_vec(),
            },
            cut: EvidenceCut {
                at: Moment(100),
                sdk_events: 1,
            },
            normalized: n,
        }
    }

    /// Append then reopen: the ledger replays every durable batch, and each id
    /// resolves to its evidence — the restart-rebuilds-from-the-ledger contract.
    #[test]
    fn append_survives_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("evidence.log");
        let (id0, id1);
        {
            let mut led = EvidenceLedger::open(&path).expect("open");
            id0 = led.append(&evidence(0, b"aaa")).expect("append 0");
            id1 = led.append(&evidence(1, b"bbb")).expect("append 1");
            assert_eq!(led.len(), 2);
        }
        // Restart: a fresh handle rebuilds from the durable frames alone.
        let led = EvidenceLedger::open(&path).expect("reopen");
        assert_eq!(led.len(), 2);
        assert!(led.contains(&id0));
        assert!(led.contains(&id1));
        assert_eq!(led.get(&id0).unwrap().rollout.issue, 0);
        assert_eq!(led.get(&id1).unwrap().env.bytes, b"bbb");
    }

    /// A torn tail (a crash mid-append) is repaired on reopen: the whole prior
    /// batches replay, the partial frame is dropped, and appends resume.
    #[test]
    fn a_torn_tail_is_repaired_on_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("evidence.log");
        let id0;
        {
            let mut led = EvidenceLedger::open(&path).expect("open");
            id0 = led.append(&evidence(0, b"whole")).expect("append 0");
        }
        // Simulate a crash mid-append: append a partial frame by hand (a length
        // header with a truncated payload).
        {
            let mut f = OpenOptions::new().append(true).open(&path).expect("append");
            f.write_all(&(999u32).to_le_bytes()).expect("len");
            f.write_all(&[0u8; 32]).expect("digest");
            f.write_all(b"short").expect("truncated payload"); // < 999 bytes
            f.sync_data().expect("sync");
        }
        // Reopen repairs the torn tail; the whole batch survives and new appends work.
        let mut led = EvidenceLedger::open(&path).expect("reopen repairs");
        assert_eq!(led.len(), 1);
        assert!(led.contains(&id0));
        let id1 = led
            .append(&evidence(1, b"after"))
            .expect("append after repair");
        assert!(led.contains(&id1));
        // And the repair is durable across a further reopen.
        drop(led);
        let led = EvidenceLedger::open(&path).expect("reopen again");
        assert_eq!(led.len(), 2);
    }

    /// Interior corruption (a payload digest that disagrees) is a typed error,
    /// never a silent short read.
    #[test]
    fn interior_corruption_is_a_typed_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("evidence.log");
        {
            let mut led = EvidenceLedger::open(&path).expect("open");
            led.append(&evidence(0, b"aaa")).expect("append 0");
            led.append(&evidence(1, b"bbb")).expect("append 1");
        }
        // Flip a byte inside the first frame's payload (past the 8-byte header +
        // 36-byte frame header).
        {
            use std::io::{Read, Seek, SeekFrom, Write};
            let mut f = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap();
            f.seek(SeekFrom::Start(8 + 36 + 10)).unwrap();
            let mut b = [0u8; 1];
            f.read_exact(&mut b).unwrap();
            f.seek(SeekFrom::Start(8 + 36 + 10)).unwrap();
            f.write_all(&[b[0] ^ 0xFF]).unwrap();
            f.sync_data().unwrap();
        }
        let err = EvidenceLedger::open(&path).expect_err("interior damage");
        assert!(matches!(err, LedgerError::Corrupt { .. }));
    }

    /// TraceStore retention cannot delete a live reference: while its entry is in
    /// the ledger, a reproducer payload survives every retention pass.
    #[test]
    fn retention_cannot_delete_a_live_reference() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("evidence.log");
        let mut led = EvidenceLedger::open(&path).expect("open");
        led.append(&evidence(0, b"live-repro")).expect("append");
        let live_digest = *blake3::hash(b"live-repro").as_bytes();
        assert!(led.live_references().contains(&live_digest));
        // Add an orphan payload directly to the store, then retain: the orphan is
        // reclaimed, the live reference is not.
        led.store.put(b"orphan", 1);
        assert_eq!(led.trace_store().len(), 2);
        let reclaimed = led.retain_live_payloads();
        assert_eq!(reclaimed, 1, "only the orphan is reclaimed");
        assert!(
            led.trace_store().contains(&PayloadRef {
                digest: live_digest,
                format_version: REPRODUCER_FORMAT_VERSION
            }),
            "the live reference survives retention"
        );
    }
}
