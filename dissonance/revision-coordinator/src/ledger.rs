// SPDX-License-Identifier: AGPL-3.0-or-later
//! The append-only, fsync-ordered ledger the coordinator persists to.
//!
//! The [`Ledger`] trait is defined HERE (Convention rule 2); `hm-bbx.4`
//! supplies the concrete evidence-payload backing in production. Two
//! implementations ship with this crate: [`MemLedger`] (M0 — in-memory, with
//! simulated crash and fault injection for the recovery tests) and
//! [`FileLedger`](crate::FileLedger) (M1 — file-backed, portable).
//!
//! Durability contract, which every implementation must honor:
//!
//! - `append` stages a record; **append order is durable order** and is never
//!   reordered.
//! - `sync` is the durability barrier: when it returns `Ok`, every previously
//!   appended record survives any crash. A crash may lose any *suffix* of
//!   appends staged since the last successful `sync`, but can never lose or
//!   reorder records below that barrier.
//! - `replay` returns exactly the durable records, in append order — what a
//!   recovery after a crash-right-now would see. Records staged but not yet
//!   synced are NOT visible to `replay`.
//! - `reopen` yields an independent handle to the same durable log (the
//!   recovery path). Reopening while another handle still writes is
//!   split-brain and unsupported.

use serde::{Deserialize, Serialize};

use crate::ids::{
    CampaignConfigId, CohortId, EvidenceBatchId, ProposalId, Revision, TerminalRecord,
};

/// One durable coordinator event. The ledger is the ONLY authority: restart
/// replays these records and never trusts a live arrangement
/// (`docs/DISSONANCE-STRATEGY.md`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LedgerRecord {
    /// First record of every ledger: pins the immutable campaign
    /// configuration this coordinator orders proposals under.
    Genesis {
        /// Content-addressed campaign configuration identity.
        config: CampaignConfigId,
    },
    /// A cohort opened, freezing its selector/archive view at the
    /// search-visible frontier of that instant.
    CohortOpen {
        /// The cohort (dense mint order).
        cohort: CohortId,
        /// The frozen search-visible frontier (inclusive watermark).
        view: Revision,
    },
    /// The persist-then-dispatch handshake: a proposal's `Revision`
    /// assignment, durable BEFORE the caller may dispatch it.
    Proposal {
        /// The proposal (dense mint order).
        proposal: ProposalId,
        /// Its reserved revision slot (dense, never reused).
        revision: Revision,
        /// The cohort it was minted under.
        cohort: CohortId,
    },
    /// An already-durable evidence-batch identity committed to its
    /// proposal's revision, with the deterministic terminal record that
    /// closed the rollout.
    Commit {
        /// The proposal being completed.
        proposal: ProposalId,
        /// Its revision slot (redundant with the proposal record; checked on
        /// replay).
        revision: Revision,
        /// The opaque, already-durable batch identity.
        batch: EvidenceBatchId,
        /// Deterministic V-time/work terminal record.
        terminal: TerminalRecord,
    },
    /// A cohort closed: no further proposals mint under it; once every
    /// member commits, its results become search-visible.
    CohortClose {
        /// The cohort being closed.
        cohort: CohortId,
    },
    /// Unrecoverable host/control failure: the campaign aborts, the frontier
    /// never advances again, and no slot is ever skipped. Always the last
    /// record of a ledger.
    Abort {
        /// Human-readable failure description (post-mortem only; never
        /// state-affecting).
        reason: String,
    },
}

/// Typed ledger failures.
#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    /// Underlying I/O failure (file ledger).
    #[error("ledger I/O: {0}")]
    Io(#[from] std::io::Error),
    /// A record failed to encode or a durable frame failed to decode.
    #[error("ledger corrupt at byte {offset}: {detail}")]
    Corrupt {
        /// Byte offset of the offending frame.
        offset: usize,
        /// What was wrong.
        detail: String,
    },
    /// Backend-specific failure (also the injected-fault variant used by the
    /// crash-recovery test model).
    #[error("ledger backend: {0}")]
    Backend(String),
    /// The stream carries a format version this reader does not support
    /// (PR #124 F10: refuse — typed — rather than misparse; the format is
    /// load-bearing for `hm-bbx.4` the moment it builds on it).
    #[error("unsupported ledger format version {found} (this reader supports {supported})")]
    UnsupportedVersion {
        /// The version the stream declares.
        found: u32,
        /// The version this reader writes and understands.
        supported: u32,
    },
}

/// Append-only, fsync-ordered record log. See the module docs for the
/// durability contract.
pub trait Ledger {
    /// Stage `record` after every previously appended record.
    fn append(&mut self, record: &LedgerRecord) -> Result<(), LedgerError>;

    /// Durability barrier: on `Ok`, everything appended so far survives any
    /// crash.
    fn sync(&mut self) -> Result<(), LedgerError>;

    /// The durable records, in append order (staged-but-unsynced records are
    /// not included).
    fn replay(&self) -> Result<Vec<LedgerRecord>, LedgerError>;

    /// An independent handle to the same durable log, for recovery.
    fn reopen(&self) -> Result<Box<dyn Ledger>, LedgerError>;
}

// ---------------------------------------------------------------------------
// Stream codec (shared by the file ledger; MemLedger stores records
// directly). Hardened per the PR #124 FAM-WAL ruling: versioned header
// (F10), interior-vs-tail damage distinction (F3), bounded decode (F5).
// ---------------------------------------------------------------------------

/// Stream magic: the first four bytes of every ledger stream.
pub(crate) const STREAM_MAGIC: [u8; 4] = *b"HWAL";
/// The format version this codec writes and understands. Bump on any frame
/// or record-encoding change; an unknown version is a typed refusal
/// ([`LedgerError::UnsupportedVersion`]), never a misparse.
pub(crate) const STREAM_VERSION: u32 = 1;
/// Stream header layout: `magic: 4 | version: u32 LE`.
pub(crate) const STREAM_HEADER: usize = 8;

/// Frame layout: `len: u32 LE | len_check: 4 bytes (BLAKE3(len) prefix) |
/// payload_check: 8 bytes (BLAKE3(payload) prefix) | payload: canonical
/// serde_json`. The LENGTH is independently verifiable (PR #124 VERIFY V1):
/// without `len_check`, a corrupted in-bound length landing past
/// end-of-stream is indistinguishable from a torn tail, and recovery would
/// physically truncate committed records and re-mint their revisions. The
/// checks distinguish damage from data; the tear rules live in
/// [`decode_stream`].
const LEN_PREFIX: usize = 4 + 4;
const FRAME_HEADER: usize = LEN_PREFIX + 8;

/// Bounded decode limit (F5): the writer refuses to encode a payload larger
/// than this, so a stream length field above it is damage by definition —
/// the decoder never allocates or skips on an attacker/rot-controlled size.
pub(crate) const MAX_FRAME_PAYLOAD: usize = 1 << 20;

/// Append the stream header to `out`.
pub(crate) fn encode_stream_header(out: &mut Vec<u8>) {
    out.extend_from_slice(&STREAM_MAGIC);
    out.extend_from_slice(&STREAM_VERSION.to_le_bytes());
}

/// Encode one record as a frame, appending to `out`.
pub(crate) fn encode_frame(record: &LedgerRecord, out: &mut Vec<u8>) -> Result<(), LedgerError> {
    let payload = serde_json::to_vec(record).map_err(|e| LedgerError::Corrupt {
        offset: out.len(),
        detail: format!("encode: {e}"),
    })?;
    if payload.len() > MAX_FRAME_PAYLOAD {
        return Err(LedgerError::Corrupt {
            offset: out.len(),
            detail: format!(
                "record encodes to {} bytes, over the {MAX_FRAME_PAYLOAD}-byte frame bound",
                payload.len()
            ),
        });
    }
    // Infallible: MAX_FRAME_PAYLOAD < u32::MAX.
    let len = payload.len() as u32;
    let len_bytes = len.to_le_bytes();
    out.extend_from_slice(&len_bytes);
    out.extend_from_slice(&blake3::hash(&len_bytes).as_bytes()[..4]);
    out.extend_from_slice(&blake3::hash(&payload).as_bytes()[..8]);
    out.extend_from_slice(&payload);
    Ok(())
}

/// Decode a full ledger stream (header + frames). Returns the records plus
/// the byte length of the valid prefix (for torn-tail repair).
///
/// Damage rules (F3 + VERIFY V1 — interior damage is an ERROR, only a
/// genuine tear truncates, and a tear may only be declared on a VERIFIED
/// length):
///
/// - **Tear** (decodes to `Ok` with the shorter valid prefix): the length
///   prefix itself cut short, or a verified length whose payload (or
///   payload check) is incomplete — the shapes a crash mid-`write_all`
///   can actually produce. A truncated stream header on a brand-new file
///   is a torn creation.
/// - **Everything else refuses** with [`LedgerError::Corrupt`]: a length
///   check that fails with its bytes present (VERIFY V1 — without it, a
///   corrupted in-bound length landing past end-of-stream reads as a tear
///   and recovery physically truncates committed records), a length over
///   [`MAX_FRAME_PAYLOAD`], a payload check that fails on a complete
///   frame, or a checksummed-but-undecodable payload. Silent truncation
///   here would drop durable records and remint committed revisions.
pub(crate) fn decode_stream(bytes: &[u8]) -> Result<(Vec<LedgerRecord>, usize), LedgerError> {
    if bytes.is_empty() {
        return Ok((Vec::new(), 0));
    }
    if bytes.len() < STREAM_HEADER {
        // A torn creation (crash between file creation and the header
        // sync) is a strict prefix of the expected header; anything else
        // is not a ledger.
        let mut expected = Vec::with_capacity(STREAM_HEADER);
        encode_stream_header(&mut expected);
        return if expected.starts_with(bytes) {
            Ok((Vec::new(), 0))
        } else {
            Err(LedgerError::Corrupt {
                offset: 0,
                detail: "not a ledger stream (bad partial header)".to_owned(),
            })
        };
    }
    if bytes[..4] != STREAM_MAGIC {
        return Err(LedgerError::Corrupt {
            offset: 0,
            detail: "not a ledger stream (bad magic)".to_owned(),
        });
    }
    // Infallible: length checked above.
    let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap_or([0; 4]));
    if version != STREAM_VERSION {
        return Err(LedgerError::UnsupportedVersion {
            found: version,
            supported: STREAM_VERSION,
        });
    }

    let mut records = Vec::new();
    let mut at = STREAM_HEADER;
    loop {
        let rest = &bytes[at..];
        if rest.is_empty() {
            break; // clean end
        }
        if rest.len() < LEN_PREFIX {
            break; // tear: the length prefix itself was cut mid-write
        }
        // The length is independently verified BEFORE it can classify
        // anything as a tear (PR #124 VERIFY V1): an unverified corrupt
        // length landing past end-of-stream would otherwise masquerade as
        // a torn tail, and recovery would physically truncate committed
        // records and re-mint their revisions.
        // Infallible: length checked above.
        let len_bytes: [u8; 4] = rest[..4].try_into().unwrap_or([0; 4]);
        if blake3::hash(&len_bytes).as_bytes()[..4] != rest[4..LEN_PREFIX] {
            return Err(LedgerError::Corrupt {
                offset: at,
                detail: "length check failed with bytes present (interior damage)".to_owned(),
            });
        }
        let len = u32::from_le_bytes(len_bytes) as usize;
        if len > MAX_FRAME_PAYLOAD {
            return Err(LedgerError::Corrupt {
                offset: at,
                detail: format!("frame length {len} over the {MAX_FRAME_PAYLOAD}-byte bound"),
            });
        }
        let frame_end = FRAME_HEADER + len;
        if rest.len() < frame_end {
            break; // tear: verified length, payload (or payload check) cut
        }
        let check = &rest[LEN_PREFIX..FRAME_HEADER];
        let payload = &rest[FRAME_HEADER..frame_end];
        if &blake3::hash(payload).as_bytes()[..8] != check {
            return Err(LedgerError::Corrupt {
                offset: at,
                detail: "payload check failed on a complete frame (interior damage)".to_owned(),
            });
        }
        let record =
            serde_json::from_slice::<LedgerRecord>(payload).map_err(|e| LedgerError::Corrupt {
                offset: at,
                detail: format!("checksummed frame does not decode: {e}"),
            })?;
        records.push(record);
        at += frame_end;
    }
    Ok((records, at))
}

// ---------------------------------------------------------------------------
// MemLedger — the M0 in-memory implementation (no fsync), with simulated
// crash and fault injection for the M1 recovery model.
// ---------------------------------------------------------------------------

/// Where the next injected fault fires inside [`MemLedger`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemFault {
    /// The next `append` fails; nothing is staged.
    Append,
    /// The next `sync` fails; staged records stay volatile.
    Sync,
}

#[derive(Default)]
struct MemStore {
    records: Vec<LedgerRecord>,
    synced: usize,
    fault: Option<MemFault>,
}

/// In-memory [`Ledger`] with simulated durability: records staged by
/// `append` become durable only at `sync`; [`MemLedger::crash`] drops the
/// unsynced tail exactly as a power loss would. Cloning (or `reopen`) yields
/// a handle to the SAME store, which is how the recovery tests re-attach
/// after a simulated crash. Single-threaded by design (`Rc`), like the
/// coordinator itself.
#[derive(Clone)]
pub struct MemLedger {
    store: std::rc::Rc<std::cell::RefCell<MemStore>>,
}

impl MemLedger {
    /// A fresh, empty ledger.
    pub fn new() -> Self {
        MemLedger {
            store: std::rc::Rc::default(),
        }
    }

    /// Simulate a crash: every record staged since the last successful
    /// `sync` is lost.
    pub fn crash(&self) {
        let mut s = self.store.borrow_mut();
        let keep = s.synced;
        s.records.truncate(keep);
    }

    /// Arm a one-shot fault at the given point; the failing call clears it.
    pub fn fail_next(&self, fault: MemFault) {
        self.store.borrow_mut().fault = Some(fault);
    }

    /// Number of durable (synced) records.
    pub fn durable_len(&self) -> usize {
        self.store.borrow().synced
    }
}

impl Default for MemLedger {
    fn default() -> Self {
        MemLedger::new()
    }
}

impl Ledger for MemLedger {
    fn append(&mut self, record: &LedgerRecord) -> Result<(), LedgerError> {
        let mut s = self.store.borrow_mut();
        if s.fault == Some(MemFault::Append) {
            s.fault = None;
            return Err(LedgerError::Backend("injected append fault".to_owned()));
        }
        s.records.push(record.clone());
        Ok(())
    }

    fn sync(&mut self) -> Result<(), LedgerError> {
        let mut s = self.store.borrow_mut();
        if s.fault == Some(MemFault::Sync) {
            s.fault = None;
            return Err(LedgerError::Backend("injected sync fault".to_owned()));
        }
        s.synced = s.records.len();
        Ok(())
    }

    fn replay(&self) -> Result<Vec<LedgerRecord>, LedgerError> {
        let s = self.store.borrow();
        Ok(s.records[..s.synced].to_vec())
    }

    fn reopen(&self) -> Result<Box<dyn Ledger>, LedgerError> {
        Ok(Box::new(self.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(n: u64) -> LedgerRecord {
        LedgerRecord::CohortOpen {
            cohort: CohortId::new(n),
            view: Revision::ZERO,
        }
    }

    #[test]
    fn mem_ledger_crash_drops_unsynced_tail() {
        let mut l = MemLedger::new();
        l.append(&rec(1)).unwrap();
        l.sync().unwrap();
        l.append(&rec(2)).unwrap();
        assert_eq!(l.replay().unwrap(), vec![rec(1)]); // unsynced invisible
        l.crash();
        l.sync().unwrap();
        assert_eq!(l.replay().unwrap(), vec![rec(1)]);
    }

    #[test]
    fn mem_ledger_reopen_shares_durable_state() {
        let mut l = MemLedger::new();
        l.append(&rec(1)).unwrap();
        l.sync().unwrap();
        let r = l.reopen().unwrap();
        assert_eq!(r.replay().unwrap(), vec![rec(1)]);
    }

    #[test]
    fn injected_faults_fire_once() {
        let mut l = MemLedger::new();
        l.fail_next(MemFault::Append);
        assert!(l.append(&rec(1)).is_err());
        l.append(&rec(1)).unwrap();
        l.fail_next(MemFault::Sync);
        assert!(l.sync().is_err());
        assert_eq!(l.durable_len(), 0);
        l.sync().unwrap();
        assert_eq!(l.durable_len(), 1);
    }

    fn sample_records() -> Vec<LedgerRecord> {
        vec![
            LedgerRecord::Genesis {
                config: CampaignConfigId::digest(b"cfg"),
            },
            LedgerRecord::Commit {
                proposal: ProposalId::new(1),
                revision: Revision::new(1),
                batch: EvidenceBatchId::digest(b"batch"),
                terminal: TerminalRecord { moment: 7, work: 9 },
            },
        ]
    }

    fn sample_stream() -> (Vec<LedgerRecord>, Vec<u8>) {
        let records = sample_records();
        let mut bytes = Vec::new();
        encode_stream_header(&mut bytes);
        for r in &records {
            encode_frame(r, &mut bytes).unwrap();
        }
        (records, bytes)
    }

    /// The frame bound is exact: a payload of exactly `MAX_FRAME_PAYLOAD`
    /// bytes encodes and decodes; one byte more is refused (mutants
    /// follow-up: pins `>` vs `>=` on both sides of the codec).
    #[test]
    fn frame_bound_is_exact() {
        let overhead = serde_json::to_vec(&LedgerRecord::Abort {
            reason: String::new(),
        })
        .unwrap()
        .len();
        let exact = LedgerRecord::Abort {
            reason: "x".repeat(MAX_FRAME_PAYLOAD - overhead),
        };
        let mut bytes = Vec::new();
        encode_stream_header(&mut bytes);
        encode_frame(&exact, &mut bytes).unwrap();
        let (decoded, len) = decode_stream(&bytes).unwrap();
        assert_eq!(decoded, vec![exact]);
        assert_eq!(len, bytes.len());
    }

    #[test]
    fn stream_codec_round_trips_and_tolerates_only_a_torn_tail() {
        let (records, bytes) = sample_stream();
        let (decoded, len) = decode_stream(&bytes).unwrap();
        assert_eq!(decoded, records);
        assert_eq!(len, bytes.len());

        // Every truncation of the FINAL frame is a legitimate tear: the
        // decode yields exactly the prefix, never an error.
        let first_len = {
            let mut one = Vec::new();
            encode_stream_header(&mut one);
            encode_frame(&records[0], &mut one).unwrap();
            one.len()
        };
        for cut in first_len..bytes.len() {
            let (decoded, len) = decode_stream(&bytes[..cut]).unwrap();
            assert_eq!(decoded, records[..1], "cut at {cut}");
            assert_eq!(len, first_len);
        }

        // A truncated header is a torn creation: empty, repairable.
        for cut in 0..STREAM_HEADER {
            let (decoded, len) = decode_stream(&bytes[..cut]).unwrap();
            assert_eq!(decoded, vec![]);
            assert_eq!(len, 0, "cut at {cut}");
        }
    }

    /// PR #124 F3 regression (the judge's flipped-byte probe): interior
    /// damage — a checksum failure on a frame whose bytes are all present —
    /// must be a typed error. The old torn-tail rule silently dropped every
    /// durable record after the flip, and recovery then reminted committed
    /// revisions.
    #[test]
    fn interior_damage_is_an_error_not_a_truncation() {
        let (_, bytes) = sample_stream();
        // Flip one payload byte in the FIRST frame (records follow it).
        let mut mangled = bytes.clone();
        mangled[STREAM_HEADER + FRAME_HEADER + 2] ^= 0xff;
        assert!(matches!(
            decode_stream(&mangled),
            Err(LedgerError::Corrupt { .. })
        ));

        // A flipped byte in the final, complete frame is interior damage
        // too — the frame is fully present, so it cannot be a tear.
        let mut mangled = bytes.clone();
        let last = mangled.len() - 1;
        mangled[last] ^= 0xff;
        assert!(matches!(
            decode_stream(&mangled),
            Err(LedgerError::Corrupt { .. })
        ));
    }

    #[test]
    fn unknown_version_and_bad_magic_are_typed_refusals() {
        let (_, mut bytes) = sample_stream();
        bytes[4] = 0x2a; // version 42
        match decode_stream(&bytes) {
            Err(LedgerError::UnsupportedVersion { found, supported }) => {
                assert_eq!(found, 42);
                assert_eq!(supported, STREAM_VERSION);
            }
            other => panic!("expected UnsupportedVersion, got {other:?}"),
        }

        let (_, mut bytes) = sample_stream();
        bytes[0] = b'X';
        assert!(matches!(
            decode_stream(&bytes),
            Err(LedgerError::Corrupt { .. })
        ));
        // A short prefix that is NOT a prefix of the real header is not a
        // torn creation either.
        assert!(matches!(
            decode_stream(b"XYZ"),
            Err(LedgerError::Corrupt { .. })
        ));
    }

    /// PR #124 F5: decode is bounded — a length field over the frame bound
    /// is damage by definition (the writer refuses to produce one). The
    /// crafted length carries a VALID length check, so this pins the bound
    /// branch specifically.
    #[test]
    fn oversized_length_field_is_corrupt_and_encode_is_bounded() {
        let mut bytes = Vec::new();
        encode_stream_header(&mut bytes);
        let len_bytes = (MAX_FRAME_PAYLOAD as u32 + 1).to_le_bytes();
        bytes.extend_from_slice(&len_bytes);
        bytes.extend_from_slice(&blake3::hash(&len_bytes).as_bytes()[..4]);
        bytes.extend_from_slice(&[0u8; 8]);
        assert!(matches!(
            decode_stream(&bytes),
            Err(LedgerError::Corrupt { .. })
        ));

        let big = LedgerRecord::Abort {
            reason: "x".repeat(MAX_FRAME_PAYLOAD + 1),
        };
        let mut out = Vec::new();
        assert!(matches!(
            encode_frame(&big, &mut out),
            Err(LedgerError::Corrupt { .. })
        ));
    }

    /// PR #124 VERIFY V1 regression (the judge's past-EOF probe): a
    /// corrupted frame length that stays under the 1 MiB bound but lands
    /// past end-of-stream must refuse as Corrupt. Before the length check
    /// existed, this read as a torn tail — `open` then physically
    /// truncated 4 of 5 durable records and recovery re-minted a committed
    /// Revision.
    #[test]
    fn past_eof_length_corruption_is_corrupt_not_a_tear() {
        let (_, bytes) = sample_stream(); // two frames; corrupt the FIRST
        let mut mangled = bytes.clone();
        mangled[STREAM_HEADER..STREAM_HEADER + 4].copy_from_slice(&983_040u32.to_le_bytes());
        assert!(matches!(
            decode_stream(&mangled),
            Err(LedgerError::Corrupt { .. })
        ));
    }

    /// PR #124 VERIFY V1 control (the judge's in-bound-grow control): a
    /// frame whose VERIFIED length extends past the stream because the
    /// payload was genuinely cut mid-write still classifies as a tear —
    /// the length check must not turn real tears into refusals.
    #[test]
    fn in_bound_grow_with_verified_length_still_tears() {
        let records = sample_records();
        let mut bytes = Vec::new();
        encode_stream_header(&mut bytes);
        encode_frame(&records[0], &mut bytes).unwrap();
        let first_len = bytes.len();
        encode_frame(&records[1], &mut bytes).unwrap();
        // Cut the second frame mid-payload: its length prefix (and length
        // check) are intact and verify, but the payload is short — the
        // authentic length now "grows past" the available bytes.
        let cut = first_len + FRAME_HEADER + 3;
        assert!(cut < bytes.len());
        let (decoded, len) = decode_stream(&bytes[..cut]).unwrap();
        assert_eq!(decoded, records[..1]);
        assert_eq!(len, first_len);
    }
}
