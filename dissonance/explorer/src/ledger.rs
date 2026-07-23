// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **crash-safe append-only campaign evidence ledger** (`hm-bbx.4`), now
//! carrying the explicit retention/GC records of `hm-5sv`.
//!
//! The generic Explorer owns the durable append of every completed, normalized
//! evidence batch **before** it can be submitted at a revision. This is the
//! authority for evidence payloads — crash-safe append/replay, keyed by the
//! durable batch identity ([`revision_coordinator::EvidenceBatchId`],
//! the blake3 digest the Revision coordinator commits). On restart the ledger
//! replays its durable frames and rebuilds the canonical inputs; a torn tail (a
//! crash mid-append) is repaired by truncation, interior damage is a typed error,
//! never a silent short read.
//!
//! ## Format v2 — tagged durable records
//!
//! Since `hm-5sv` a frame carries a tagged [`LedgerRecord`], not a bare
//! evidence payload: evidence, a retention **tombstone** (the completeness/loss
//! metadata of proven physical GC), a durable retention **checkpoint** (the
//! rebuild anchor GC cites), and the campaign's explicit **finalized** end to
//! future reinterpretation. A v1 file is rejected loudly
//! ([`LedgerError::UnsupportedVersion`]) — the format predates any integrated
//! deployment and campaign ledgers are per-campaign artifacts.
//!
//! ## Format v3 — suffix-only Seal records (`hm-j7ie`)
//!
//! Task 144 (`hm-aqf0`) changed the *meaning* of a durable **Seal** record: a
//! Seal now serializes the run-forward **suffix + observed cut**, where a
//! version-2 Seal serialized the full rollout `normalized` + base-branch
//! `parent_cut`. Because the meaning of a durable record changed, the header
//! bumps to `VERSION = 3` and every pre-3 ledger is **refused loudly**
//! ([`LedgerError::UnsupportedVersion`]). Reopening a version-2 seal under the
//! new lineage walk would resurrect it with historically **truncated** cells
//! (the exact silent-wrong the fix closes), and the same seed's batch identity
//! ([`CompletedRunEvidence::canonical_bytes`](crate::CompletedRunEvidence::canonical_bytes))
//! no longer matches across the upgrade — so a cross-version identity compare is
//! meaningless. There is no read-old or in-place migration path; if one is ever
//! wanted it is its own future task, not this format.
//!
//! ## TraceStore is referenced backing, not the relational authority
//!
//! The strategy is explicit: the `TraceStore` "may remain payload backing for
//! immutable reproducers or journals referenced by digest and format version, but
//! it is not the evidence ledger." So the ledger owns the evidence *and* holds a
//! [`TraceStore`] of the large immutable payloads (the reproducers) it references
//! by digest. Invariants held by construction:
//!
//! - **A reference reachable from a retained ledger record or a live Entry
//!   cannot be invalidated.** Every retained evidence entry keeps its referenced
//!   payload live ([`EvidenceLedger::live_references`]), and
//!   [`collect`](EvidenceLedger::collect) refuses a batch whose reproducer a
//!   live Entry needs ([`RetentionError::LiveEntryReference`]).
//! - **TraceStore retention cannot delete a live reference.**
//!   [`TraceStore::retain`] only ever removes payloads absent from the live set.
//! - **Physical GC proves coverage first.** [`collect`](EvidenceLedger::collect)
//!   requires a durable covering checkpoint or the explicit finalized end
//!   ([`RetentionError::NotCovered`] otherwise), writes the tombstone durable
//!   before touching any in-memory state, and
//!   [`compact`](EvidenceLedger::compact) physically reclaims file bytes by an
//!   atomic rewrite that preserves the rebuild anchor.
//! - **Exhaustion is loud.** An optional declared byte budget makes an
//!   over-budget evidence append fail with [`LedgerError::Exhausted`] — disk
//!   pressure never expires, collects, or downgrades anything on its own.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use revision_coordinator::EvidenceBatchId;
use serde::{Deserialize, Serialize};

use crate::evidence::CompletedRunEvidence;
use crate::retention::{CollectedBatch, CoverageRef, RetentionCheckpoint, RetentionError};

/// The ledger file magic and format version — a header a foreign or future file
/// is rejected against, never silently reinterpreted. Version 2 introduced the
/// tagged [`LedgerRecord`] frames (`hm-5sv`); version 3 (`hm-j7ie`) marks the
/// suffix-only Seal representation of task 144, under which a Seal's
/// `canonical_bytes()` batch-identity preimage differs from a version-2 Seal for
/// the same seed. Every pre-3 file (version-2 tagged frames, version-1 bare
/// evidence) is rejected loudly ([`LedgerError::UnsupportedVersion`]) — no
/// in-place migration is built.
const MAGIC: [u8; 4] = *b"HEVL";
const VERSION: u32 = 3;
/// The frame header: `len(u32) + payload_digest(32)`. A frame with fewer bytes
/// than this remaining is a torn tail.
const FRAME_HEADER: usize = 4 + 32;
/// The fixed file header length (`MAGIC + VERSION`).
const FILE_HEADER: u64 = 8;
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
    /// The file was written by an unsupported ledger version — either older or
    /// newer than this build's `VERSION`. Version 3 (`hm-j7ie`) refuses every
    /// pre-3 ledger loudly rather than silently reinterpreting a stale Seal
    /// shape: task 144 changed a Seal record to serialize the run-forward
    /// suffix + observed cut, so a version-2 seal reopened under the new
    /// lineage walk would carry historically truncated cells (and no longer
    /// matches its batch identity for the same seed). A `found` newer than
    /// `VERSION` (a future build's file) carries no such history and gets a
    /// version-neutral reason instead — this build simply does not know what
    /// that version's records mean. No read-old, no forward-compat, and no
    /// in-place migration path exists in either direction.
    #[error(
        "evidence ledger version {found} unsupported (this build writes {VERSION}): {}",
        if *found < VERSION {
            "the Seal record representation changed in task 144 — a Seal now serializes \
             the run-forward suffix + observed cut, not the full rollout normalized + \
             base-branch parent_cut, so a pre-144 (version < 3) ledger's advanced seals \
             would reopen with historically truncated cells; old ledgers are refused, not \
             silently reinterpreted"
        } else {
            "this file was written by a newer build than this one understands; refused \
             rather than silently reinterpreting a record shape this build has never seen"
        }
    )]
    UnsupportedVersion {
        /// The version found in the file header.
        found: u32,
    },
    /// A persisted record failed to decode (a tampered or version-skewed
    /// artifact) — surfaced as corruption, never a panic.
    #[error("evidence ledger record at offset {offset} did not decode: {detail}")]
    BadPayload {
        /// The byte offset of the offending frame.
        offset: u64,
        /// A human-readable detail.
        detail: String,
    },
    /// The declared evidence byte budget cannot fit the append. **Loud by
    /// design**: host disk pressure never expires, collects, or downgrades
    /// evidence on its own — the declared retention policy stands until an
    /// operator frees space or issues a new campaign configuration.
    #[error(
        "evidence budget exhausted: append needs {needed} bytes, budget is {budget} — \
         retention policy is never silently downgraded"
    )]
    Exhausted {
        /// The file size the append would have reached.
        needed: u64,
        /// The declared budget.
        budget: u64,
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

/// One tagged durable frame (format v2). The frame digest still guards the
/// serialized record bytes; an evidence batch's *identity* remains the digest
/// of its own canonical bytes, independent of the framing.
#[derive(Serialize, Deserialize)]
enum LedgerRecord {
    /// One completed run's immutable evidence.
    Evidence(CompletedRunEvidence),
    /// A retention downgrade tombstone written by proven physical GC — the
    /// completeness/loss metadata that outlives the raw evidence.
    Tombstone(CollectedBatch),
    /// A durable retention checkpoint (the rebuild anchor GC may cite). The
    /// last one in the file wins.
    Checkpoint(RetentionCheckpoint),
    /// The campaign's explicit end to future raw-evidence reinterpretation.
    Finalized,
}

/// The crash-safe append-only evidence ledger (module doc).
#[derive(Debug)]
pub struct EvidenceLedger {
    file: File,
    path: PathBuf,
    /// The current durable end offset (kept in step with appends/compaction so
    /// the byte budget is checked without re-stating the file).
    end: u64,
    /// The declared evidence byte budget, if any (checked on evidence appends).
    budget: Option<u64>,
    /// The retained durable evidence, keyed by its committed batch identity.
    /// Rebuilt from the file on [`open`](Self::open); appended to on
    /// [`append`](Self::append); shrunk only by proven [`collect`](Self::collect).
    entries: BTreeMap<EvidenceBatchId, CompletedRunEvidence>,
    /// The referenced immutable-payload backing (reproducers), kept live for
    /// every retained entry.
    store: TraceStore,
    /// The reproducer reference each retained entry keeps live
    /// (`batch → its payload`), so [`live_references`](Self::live_references)
    /// is exact.
    refs: BTreeMap<EvidenceBatchId, PayloadRef>,
    /// The completeness/loss metadata of every collected batch (durable
    /// tombstones, replayed on open).
    collected: BTreeMap<EvidenceBatchId, CollectedBatch>,
    /// The last durable retention checkpoint, if any (the rebuild anchor).
    checkpoint: Option<RetentionCheckpoint>,
    /// Whether the campaign's explicit finalized end marker is durable.
    finalized: bool,
}

/// The format version reproducer payloads are stored under in the [`TraceStore`]
/// (the `Reproducer::blob_version` is carried separately; this versions the
/// *ledger's* payload framing, not the blob).
const REPRODUCER_FORMAT_VERSION: u16 = 1;

impl EvidenceLedger {
    /// Open (creating if absent) the durable evidence ledger at `path`, replaying
    /// every durable frame to rebuild the in-memory authority. A torn tail (a
    /// crash mid-append) is truncated and the repair fsynced before the ledger is
    /// exposed, so replay only ever returns whole, checksum-verified records;
    /// interior damage is a typed [`LedgerError::Corrupt`].
    pub fn open(path: &Path) -> Result<Self, LedgerError> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        let end = file.seek(SeekFrom::End(0))?;
        let mut led = Self {
            file,
            path: path.to_path_buf(),
            end: FILE_HEADER,
            budget: None,
            entries: BTreeMap::new(),
            store: TraceStore::new(),
            refs: BTreeMap::new(),
            collected: BTreeMap::new(),
            checkpoint: None,
            finalized: false,
        };
        if end == 0 {
            // Fresh file: write the header and fsync it durable.
            led.file.seek(SeekFrom::Start(0))?;
            led.file.write_all(&MAGIC)?;
            led.file.write_all(&VERSION.to_le_bytes())?;
            led.file.sync_data()?;
        } else {
            led.file.seek(SeekFrom::Start(0))?;
            let mut hdr = [0u8; FILE_HEADER as usize];
            led.file
                .read_exact(&mut hdr)
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
            let good_end = led.replay_frames(end)?;
            if good_end < end {
                // A torn tail: truncate to the last whole frame and fsync the repair
                // before exposing replay.
                led.file.set_len(good_end)?;
                led.file.sync_data()?;
            }
            led.end = good_end;
            // A pre-compaction file replays evidence-then-tombstone for a
            // collected batch, transiently resurrecting its payload into the
            // store; sweep to exactly the retained references.
            let live = led.live_references();
            led.store.retain(&live);
        }
        led.file.seek(SeekFrom::End(0))?;
        Ok(led)
    }

    /// Replay the durable frames after the header, rebuilding the authority.
    /// Returns the offset of the end of the last **whole, verified** frame (== `end`
    /// on a clean file, `< end` when a torn tail was found).
    fn replay_frames(&mut self, end: u64) -> Result<u64, LedgerError> {
        let mut pos: u64 = FILE_HEADER;
        self.file.seek(SeekFrom::Start(pos))?;
        loop {
            if pos == end {
                return Ok(pos);
            }
            // A partial frame header at the tail is a torn write — stop here.
            if end - pos < FRAME_HEADER as u64 {
                return Ok(pos);
            }
            let mut header = [0u8; FRAME_HEADER];
            self.file.read_exact(&mut header)?;
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
            self.file.read_exact(&mut payload)?;
            if *blake3::hash(&payload).as_bytes() != digest {
                // A verified payload digest that disagrees is interior damage, not
                // a torn tail (the length and digest were fully present).
                return Err(LedgerError::Corrupt {
                    offset: pos,
                    detail: "frame payload digest mismatch".into(),
                });
            }
            let record: LedgerRecord =
                serde_json::from_slice(&payload).map_err(|e| LedgerError::BadPayload {
                    offset: pos,
                    detail: e.to_string(),
                })?;
            self.apply(record);
            pos += FRAME_HEADER as u64 + len as u64;
        }
    }

    /// Apply one replayed record to the in-memory authority (replay is the
    /// exact fold appends performed live, so restart rebuilds the same state).
    fn apply(&mut self, record: LedgerRecord) {
        match record {
            LedgerRecord::Evidence(evidence) => {
                // The batch identity is the digest of the evidence's own
                // canonical bytes, independent of the frame encoding.
                let id = EvidenceBatchId::digest(&evidence.canonical_bytes());
                // A tombstone earlier in a compacted file wins: collected raw
                // evidence is never resurrected by a stale frame.
                if !self.collected.contains_key(&id) {
                    Self::index(
                        &mut self.entries,
                        &mut self.store,
                        &mut self.refs,
                        id,
                        evidence,
                    );
                }
            }
            LedgerRecord::Tombstone(tomb) => {
                self.entries.remove(&tomb.batch);
                self.refs.remove(&tomb.batch);
                self.collected.insert(tomb.batch, tomb);
            }
            LedgerRecord::Checkpoint(cp) => self.checkpoint = Some(cp),
            LedgerRecord::Finalized => self.finalized = true,
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

    /// Frame one record and write it durable (fsynced before return).
    fn append_record(&mut self, record: &LedgerRecord) -> Result<(), LedgerError> {
        // Infallible for our owned, finite, non-float types; a serialize error
        // here would be a programming error, not untrusted input.
        let payload = serde_json::to_vec(record).expect("LedgerRecord serializes");
        let digest = *blake3::hash(&payload).as_bytes();
        let mut frame = Vec::with_capacity(FRAME_HEADER + payload.len());
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        frame.extend_from_slice(&digest);
        frame.extend_from_slice(&payload);
        self.file.write_all(&frame)?;
        self.file.sync_data()?;
        self.end += frame.len() as u64;
        Ok(())
    }

    /// Declare (or clear) the evidence byte budget. The budget bounds the
    /// durable file size evidence appends may reach; hitting it fails loudly
    /// ([`LedgerError::Exhausted`]) and never changes retention behavior.
    /// Retention/GC records (tombstones, checkpoints, finalization) are exempt:
    /// refusing them could block the explicit recovery that reclaims space,
    /// while admitting them can never silently change policy.
    pub fn set_budget(&mut self, budget: Option<u64>) {
        self.budget = budget;
    }

    /// The declared evidence byte budget, if any.
    pub fn budget(&self) -> Option<u64> {
        self.budget
    }

    /// Durably append one completed run's normalized evidence, returning its
    /// batch identity — the id the caller then submits to the Revision
    /// coordinator for commit. The frame is written and **fsynced durable before
    /// return**, so once this returns `Ok` the batch survives a crash; a crash
    /// before it returns leaves at most a torn tail the next
    /// [`open`](Self::open) repairs. Appending byte-identical evidence twice is
    /// idempotent (same digest, one durable copy is enough). An append past the
    /// declared byte budget fails loudly **before** any state changes
    /// ([`LedgerError::Exhausted`]).
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
        let record = LedgerRecord::Evidence(evidence.clone());
        // Budget check before any write or state change: exhaustion aborts the
        // append; it never expires or collects anything.
        if let Some(budget) = self.budget {
            // The framed record is the payload re-serialized under the tag; its
            // length is what actually lands on disk.
            let framed = serde_json::to_vec(&record).expect("LedgerRecord serializes");
            let needed = self.end + (FRAME_HEADER + framed.len()) as u64;
            if needed > budget {
                return Err(LedgerError::Exhausted { needed, budget });
            }
        }
        self.append_record(&record)?;
        Self::index(
            &mut self.entries,
            &mut self.store,
            &mut self.refs,
            id,
            evidence.clone(),
        );
        Ok(id)
    }

    /// Durably commit a retention checkpoint — the rebuild anchor
    /// [`collect`](Self::collect) may cite for coverage. The last committed
    /// checkpoint wins (on replay and for coverage).
    pub fn commit_checkpoint(&mut self, cp: &RetentionCheckpoint) -> Result<(), LedgerError> {
        self.append_record(&LedgerRecord::Checkpoint(cp.clone()))?;
        self.checkpoint = Some(cp.clone());
        Ok(())
    }

    /// The last durably committed retention checkpoint, if any.
    pub fn last_checkpoint(&self) -> Option<&RetentionCheckpoint> {
        self.checkpoint.as_ref()
    }

    /// Durably mark the campaign's **explicit end to future raw-evidence
    /// reinterpretation**. Idempotent. After this, [`collect`](Self::collect)
    /// accepts batches no checkpoint covers (their reinterpretation ended).
    pub fn finalize(&mut self) -> Result<(), LedgerError> {
        if !self.finalized {
            self.append_record(&LedgerRecord::Finalized)?;
            self.finalized = true;
        }
        Ok(())
    }

    /// Whether the explicit finalized end marker is durable.
    pub fn is_finalized(&self) -> bool {
        self.finalized
    }

    /// **Proven physical GC of one batch's raw evidence** (the ledger half of
    /// the retention contract — the campaign proves working-set expiry before
    /// calling). Obligations, all loud:
    ///
    /// - the batch must be a retained record ([`RetentionError::UnknownBatch`]);
    /// - its reproducer payload must not be required by a live Entry
    ///   (`protected`, [`RetentionError::LiveEntryReference`]) — evidence needed
    ///   to reproduce a retained Entry cannot be collected while it is live;
    /// - a durable checkpoint must cover the batch, or the campaign must be
    ///   finalized ([`RetentionError::NotCovered`]) — GC leaves a rebuildable
    ///   checkpoint or an explicit end to future reinterpretation.
    ///
    /// The tombstone (exact completeness/loss metadata) is written durable
    /// **before** any in-memory downgrade; the payload store is then swept to
    /// the remaining live references. Returns the tombstone recorded.
    pub fn collect(
        &mut self,
        id: EvidenceBatchId,
        protected: &BTreeSet<[u8; 32]>,
    ) -> Result<CollectedBatch, RetentionError> {
        let Some(ev) = self.entries.get(&id) else {
            return Err(RetentionError::UnknownBatch { batch: id });
        };
        let covered_by = if let Some(cp) = &self.checkpoint
            && cp.covers(ev.rollout.issue)
        {
            CoverageRef::Checkpoint {
                frontier_issue: cp.views.frontier_issue,
            }
        } else if self.finalized {
            CoverageRef::Finalized
        } else {
            return Err(RetentionError::NotCovered {
                batch: id,
                issue: ev.rollout.issue,
            });
        };
        if protected.contains(blake3::hash(&ev.env.bytes).as_bytes()) {
            return Err(RetentionError::LiveEntryReference { batch: id });
        }
        let tomb = CollectedBatch {
            batch: id,
            rollout: ev.rollout,
            role: ev.role,
            cut: ev.cut,
            events: ev.normalized.events.len() as u64,
            covered_by,
        };
        // Durable tombstone first: a crash after this replays the downgrade.
        self.append_record(&LedgerRecord::Tombstone(tomb.clone()))
            .map_err(RetentionError::from)?;
        self.entries.remove(&id);
        self.refs.remove(&id);
        self.collected.insert(id, tomb.clone());
        // Sweep the payload backing to the remaining retained references. A
        // protected (live-Entry) digest is always still referenced by some
        // retained entry: collecting its last referencing batch was refused
        // above, so the sweep can never drop it.
        let live = self.live_references();
        self.store.retain(&live);
        Ok(tomb)
    }

    /// **Physically reclaim file bytes** by rewriting the ledger without the
    /// collected batches' raw evidence: header, finalized marker, the rebuild
    /// checkpoint, every tombstone, then every retained evidence record. The
    /// rewrite is crash-safe — written to a sibling temp file, fsynced, then
    /// atomically renamed over the ledger (a crash mid-compaction leaves the
    /// original intact). Returns the bytes reclaimed.
    pub fn compact(&mut self) -> Result<u64, LedgerError> {
        let old_end = self.end;
        let tmp_path = self.path.with_extension("compact");
        let mut tmp = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp_path)?;
        tmp.write_all(&MAGIC)?;
        tmp.write_all(&VERSION.to_le_bytes())?;
        let mut new_end = FILE_HEADER;
        let mut write_frame = |record: &LedgerRecord| -> Result<u64, LedgerError> {
            // Infallible for our owned, finite, non-float types (comment as in
            // `append_record`).
            let payload = serde_json::to_vec(record).expect("LedgerRecord serializes");
            let digest = *blake3::hash(&payload).as_bytes();
            tmp.write_all(&(payload.len() as u32).to_le_bytes())?;
            tmp.write_all(&digest)?;
            tmp.write_all(&payload)?;
            Ok((FRAME_HEADER + payload.len()) as u64)
        };
        if self.finalized {
            new_end += write_frame(&LedgerRecord::Finalized)?;
        }
        if let Some(cp) = &self.checkpoint {
            new_end += write_frame(&LedgerRecord::Checkpoint(cp.clone()))?;
        }
        for tomb in self.collected.values() {
            new_end += write_frame(&LedgerRecord::Tombstone(tomb.clone()))?;
        }
        for ev in self.entries.values() {
            new_end += write_frame(&LedgerRecord::Evidence(ev.clone()))?;
        }
        tmp.sync_data()?;
        drop(tmp);
        std::fs::rename(&tmp_path, &self.path)?;
        self.file = OpenOptions::new().read(true).write(true).open(&self.path)?;
        self.file.seek(SeekFrom::End(0))?;
        self.end = new_end;
        Ok(old_end.saturating_sub(new_end))
    }

    /// The durable evidence behind a batch identity, if retained.
    pub fn get(&self, id: &EvidenceBatchId) -> Option<&CompletedRunEvidence> {
        self.entries.get(id)
    }

    /// Whether a batch identity is durably retained (a collected batch is not:
    /// its tombstone is, see [`collected`](Self::collected)).
    pub fn contains(&self, id: &EvidenceBatchId) -> bool {
        self.entries.contains_key(id)
    }

    /// The number of retained batches.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the ledger retains no batches.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Every retained batch identity, in canonical order — the canonical inputs a
    /// restart rebuilds views from.
    pub fn batch_ids(&self) -> impl Iterator<Item = &EvidenceBatchId> {
        self.entries.keys()
    }

    /// The completeness/loss metadata of every collected batch (the durable
    /// tombstones), in canonical order.
    pub fn collected(&self) -> impl Iterator<Item = (&EvidenceBatchId, &CollectedBatch)> {
        self.collected.iter()
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
    use crate::evidence::{EvidenceRole, RunId};
    use crate::retention::{RetentionProfile, RetentionViews};
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
            role: EvidenceRole::Rollout,
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
            parent_cut: None,
            sealable_moments: Vec::new(),
        }
    }

    /// A checkpoint whose coverage frontier is `issue` (empty views otherwise).
    fn checkpoint_at(issue: u64) -> crate::retention::RetentionCheckpoint {
        let mut views = RetentionViews::new(RetentionProfile::Full);
        views.frontier_issue = issue;
        crate::retention::RetentionCheckpoint { views }
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

    /// A v1 file (or any foreign version) is rejected loudly, never silently
    /// reinterpreted.
    #[test]
    fn foreign_version_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("evidence.log");
        {
            let mut f = File::create(&path).unwrap();
            f.write_all(&MAGIC).unwrap();
            f.write_all(&1u32.to_le_bytes()).unwrap();
            f.sync_data().unwrap();
        }
        let err = EvidenceLedger::open(&path).expect_err("v1 rejected");
        assert!(matches!(err, LedgerError::UnsupportedVersion { found: 1 }));
    }

    /// A **version-2** ledger (pre-144 tagged frames, whose Seal records still
    /// carry the old full-`normalized` + base-branch `parent_cut` shape) is
    /// refused loudly on reopen — the F5 silent-truncation this task closes — and
    /// the refusal names *why* (the suffix-only Seal representation change), so an
    /// operator is never left guessing why an old campaign ledger will not open.
    #[test]
    fn version_two_ledger_is_refused_with_the_suffix_reason() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("evidence.log");
        // A well-formed header at the prior version: valid magic, VERSION == 2.
        {
            let mut f = File::create(&path).unwrap();
            f.write_all(&MAGIC).unwrap();
            f.write_all(&2u32.to_le_bytes()).unwrap();
            f.sync_data().unwrap();
        }
        let err = EvidenceLedger::open(&path).expect_err("v2 refused");
        assert!(matches!(err, LedgerError::UnsupportedVersion { found: 2 }));
        // Loud about the reason: the refusal message names the suffix-only
        // representation change and the truncation it would otherwise cause.
        let msg = err.to_string();
        assert!(
            msg.contains("suffix") && msg.contains("truncated") && msg.contains("task 144"),
            "the refusal names the suffix-only representation change: {msg}"
        );
    }

    /// A **future** ledger (`found` newer than this build's `VERSION`) is
    /// refused loudly like any other unsupported version, but the refusal must
    /// not misdiagnose it: this build never wrote a pre-144 ledger of this
    /// file, so the message must not claim the truncation history that only
    /// applies to `found < VERSION`. It gets a plain, version-neutral reason
    /// instead.
    #[test]
    fn future_version_is_rejected_without_the_pre_144_claim() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("evidence.log");
        // A well-formed header from a hypothetical future build: VERSION == 4.
        {
            let mut f = File::create(&path).unwrap();
            f.write_all(&MAGIC).unwrap();
            f.write_all(&4u32.to_le_bytes()).unwrap();
            f.sync_data().unwrap();
        }
        let err = EvidenceLedger::open(&path).expect_err("v4 refused");
        assert!(matches!(err, LedgerError::UnsupportedVersion { found: 4 }));
        let msg = err.to_string();
        assert!(
            !msg.contains("pre-144") && !msg.contains("truncated") && !msg.contains("task 144"),
            "a future version must not be misdiagnosed as a pre-144 ledger: {msg}"
        );
        assert!(
            msg.contains("newer"),
            "a future version's refusal names it as newer than this build understands: {msg}"
        );
    }

    /// A freshly written ledger stamps `VERSION = 3` in its durable header and
    /// reopens cleanly (round-trip) — the current build both *writes* and *reads*
    /// version 3, so our own files are never caught by the pre-3 refusal.
    #[test]
    fn fresh_ledger_is_version_three_and_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("evidence.log");
        let id = {
            let mut led = EvidenceLedger::open(&path).expect("open");
            led.append(&evidence(0, b"v3")).expect("append")
        };
        // The durable header carries version 3, not 2.
        assert_eq!(VERSION, 3, "this build writes version 3");
        let mut hdr = [0u8; FILE_HEADER as usize];
        File::open(&path)
            .unwrap()
            .read_exact(&mut hdr)
            .expect("read header");
        assert_eq!(&hdr[0..4], &MAGIC, "magic");
        assert_eq!(
            u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]),
            3,
            "on-disk version byte is 3"
        );
        // …and it reopens cleanly at version 3 (no refusal on our own file).
        let led = EvidenceLedger::open(&path).expect("reopen at v3");
        assert!(led.contains(&id), "the round-tripped batch survives reopen");
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

    /// The declared byte budget fails an over-budget evidence append loudly and
    /// changes nothing: no entry is dropped, nothing is collected, the file
    /// stays valid, and space freed by policy (a raised budget) resumes appends.
    /// The budget boundary is exact: an append that lands the file precisely at
    /// the budget is admitted.
    #[test]
    fn exhaustion_is_loud_and_changes_no_policy() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("evidence.log");
        let mut led = EvidenceLedger::open(&path).expect("open");
        assert_eq!(led.budget(), None, "no budget unless declared");
        let id0 = led.append(&evidence(0, b"first")).expect("append 0");
        let before_len = led.len();
        let before_end = led.end;
        // A budget below what the next append needs.
        led.set_budget(Some(led.end + 8));
        assert_eq!(led.budget(), Some(before_end + 8), "the declared budget");
        let err = led.append(&evidence(1, b"second")).expect_err("exhausted");
        assert!(matches!(err, LedgerError::Exhausted { .. }));
        // LOUD, not lossy: nothing was expired, collected, or truncated.
        assert_eq!(led.len(), before_len);
        assert_eq!(led.end, before_end);
        assert!(led.contains(&id0));
        assert_eq!(led.collected().count(), 0, "no silent collection");

        // The exact boundary: measure the frame's true on-disk size on a twin
        // ledger, then declare a budget the append lands on precisely — it must
        // be admitted (the check is `needed > budget`, byte-exact arithmetic).
        let twin_path = dir.path().join("twin.log");
        let mut twin = EvidenceLedger::open(&twin_path).expect("twin");
        let twin_base = std::fs::metadata(&twin_path).expect("meta").len();
        twin.append(&evidence(1, b"second")).expect("twin append");
        let frame = std::fs::metadata(&twin_path).expect("meta").len() - twin_base;
        led.set_budget(Some(before_end + frame));
        let id1 = led
            .append(&evidence(1, b"second"))
            .expect("an append landing exactly at the budget is admitted");
        assert_eq!(
            std::fs::metadata(&path).expect("meta").len(),
            before_end + frame,
            "the file landed exactly on the declared budget"
        );
        drop(led);
        let led = EvidenceLedger::open(&path).expect("reopen clean");
        assert!(led.contains(&id0) && led.contains(&id1));
    }

    /// `collect` demands proof: an uncovered batch is refused until a covering
    /// checkpoint (or finalization) is durable; a live-Entry-protected batch is
    /// refused while protected; an unknown id is a typed error.
    #[test]
    fn collect_requires_coverage_and_refuses_protected_references() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("evidence.log");
        let mut led = EvidenceLedger::open(&path).expect("open");
        let id0 = led.append(&evidence(3, b"repro-a")).expect("append");
        let none = BTreeSet::new();

        // Unknown batch.
        let bogus = EvidenceBatchId::digest(b"nope");
        assert!(matches!(
            led.collect(bogus, &none),
            Err(RetentionError::UnknownBatch { .. })
        ));

        // No checkpoint, not finalized: not covered.
        assert!(matches!(
            led.collect(id0, &none),
            Err(RetentionError::NotCovered { issue: 3, .. })
        ));

        // A checkpoint that does NOT cover issue 3 still refuses.
        led.commit_checkpoint(&checkpoint_at(2)).expect("cp");
        assert!(matches!(
            led.collect(id0, &none),
            Err(RetentionError::NotCovered { .. })
        ));

        // Covered — but protected by a live Entry: refused, nothing changes.
        led.commit_checkpoint(&checkpoint_at(3)).expect("cp");
        let protected: BTreeSet<[u8; 32]> = [*blake3::hash(b"repro-a").as_bytes()].into();
        assert!(matches!(
            led.collect(id0, &protected),
            Err(RetentionError::LiveEntryReference { .. })
        ));
        assert!(led.contains(&id0), "a refused collect changes nothing");

        // Proof complete: collected, tombstoned with its coverage.
        let tomb = led.collect(id0, &none).expect("collect");
        assert_eq!(tomb.batch, id0);
        assert_eq!(
            tomb.covered_by,
            CoverageRef::Checkpoint { frontier_issue: 3 }
        );
        assert!(!led.contains(&id0));
        assert_eq!(led.trace_store().len(), 0, "payload reclaimed");
        // The tombstone (completeness/loss metadata) is durable across reopen.
        drop(led);
        let led = EvidenceLedger::open(&path).expect("reopen");
        assert!(!led.contains(&id0));
        let collected: Vec<_> = led.collected().collect();
        assert_eq!(collected.len(), 1);
        assert_eq!(collected[0].1.rollout.issue, 3);
    }

    /// Finalization is the other GC leg: with the explicit end marker durable,
    /// an uncovered batch may be collected and its tombstone cites it.
    #[test]
    fn finalization_permits_collection_without_checkpoint() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("evidence.log");
        let mut led = EvidenceLedger::open(&path).expect("open");
        let id0 = led.append(&evidence(7, b"repro-b")).expect("append");
        led.finalize().expect("finalize");
        assert!(led.is_finalized());
        let tomb = led.collect(id0, &BTreeSet::new()).expect("collect");
        assert_eq!(tomb.covered_by, CoverageRef::Finalized);
        // The end marker is durable.
        drop(led);
        let mut led = EvidenceLedger::open(&path).expect("reopen");
        assert!(led.is_finalized());
        // …and it survives compaction (the rewritten file carries the marker,
        // with the tracked end matching the real file).
        led.compact().expect("compact");
        assert_eq!(
            std::fs::metadata(&path).expect("meta").len(),
            led.end,
            "tracked end matches the compacted file"
        );
        drop(led);
        let led = EvidenceLedger::open(&path).expect("reopen compacted");
        assert!(
            led.is_finalized(),
            "the finalized marker survives compaction"
        );
        assert_eq!(led.collected().count(), 1);
    }

    /// A payload shared by two batches survives collecting one of them: the
    /// remaining retained reference keeps it live in the store.
    #[test]
    fn shared_payload_survives_collecting_one_referent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("evidence.log");
        let mut led = EvidenceLedger::open(&path).expect("open");
        // Two distinct batches (different issues) sharing one reproducer blob.
        let ida = led.append(&evidence(1, b"shared")).expect("a");
        let idb = led.append(&evidence(2, b"shared")).expect("b");
        assert_ne!(ida, idb);
        assert_eq!(led.trace_store().len(), 1, "content-addressed");
        led.commit_checkpoint(&checkpoint_at(10)).expect("cp");
        led.collect(ida, &BTreeSet::new()).expect("collect a");
        let shared = *blake3::hash(b"shared").as_bytes();
        assert!(
            led.live_references().contains(&shared),
            "b still references the payload"
        );
        assert_eq!(led.trace_store().len(), 1, "payload survives");
    }

    /// Compaction physically reclaims the collected raw bytes, preserves the
    /// rebuild anchor + tombstones + retained evidence, and the compacted file
    /// replays to the same state (crash-safe rename, no resurrection).
    #[test]
    fn compaction_reclaims_bytes_and_replays_identically() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("evidence.log");
        let mut led = EvidenceLedger::open(&path).expect("open");
        let big = vec![0xABu8; 4096];
        let id_big = led.append(&evidence(1, &big)).expect("big");
        let id_keep = led.append(&evidence(2, b"keep")).expect("keep");
        led.commit_checkpoint(&checkpoint_at(5)).expect("cp");
        led.collect(id_big, &BTreeSet::new()).expect("collect");
        let before = std::fs::metadata(&path).unwrap().len();
        let reclaimed = led.compact().expect("compact");
        let after = std::fs::metadata(&path).unwrap().len();
        assert!(reclaimed >= 4096, "the big raw payload left the file");
        assert_eq!(after, led.end, "tracked end matches the real file");
        assert!(after < before);
        // Post-compaction state: retained evidence, tombstone, checkpoint all
        // survive; appends continue against the new file.
        assert!(led.contains(&id_keep));
        assert!(!led.contains(&id_big));
        assert_eq!(led.collected().count(), 1);
        assert!(led.last_checkpoint().is_some());
        let id_after = led.append(&evidence(3, b"post")).expect("append");
        // And a reopen replays the compacted stream to the identical state.
        drop(led);
        let led = EvidenceLedger::open(&path).expect("reopen");
        assert!(led.contains(&id_keep) && led.contains(&id_after));
        assert!(!led.contains(&id_big), "no resurrection");
        assert_eq!(led.collected().count(), 1);
        assert_eq!(
            led.last_checkpoint().unwrap().views.frontier_issue,
            5,
            "the rebuild anchor survives compaction"
        );
    }
}
