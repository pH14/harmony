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
// Frame codec (shared by the file ledger; MemLedger stores records directly).
// ---------------------------------------------------------------------------

/// Frame layout: `len: u32 LE | check: 8 bytes (BLAKE3(payload) prefix) |
/// payload: canonical serde_json`. The checksum detects the torn tail a
/// crash-mid-write can leave.
const FRAME_HEADER: usize = 4 + 8;

/// Encode one record as a frame, appending to `out`.
pub(crate) fn encode_frame(record: &LedgerRecord, out: &mut Vec<u8>) -> Result<(), LedgerError> {
    let payload = serde_json::to_vec(record).map_err(|e| LedgerError::Corrupt {
        offset: out.len(),
        detail: format!("encode: {e}"),
    })?;
    let len = u32::try_from(payload.len()).map_err(|_| LedgerError::Corrupt {
        offset: out.len(),
        detail: "record over u32::MAX bytes".to_owned(),
    })?;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&blake3::hash(&payload).as_bytes()[..8]);
    out.extend_from_slice(&payload);
    Ok(())
}

/// Decode all valid frames from `bytes`. Returns the records plus the byte
/// length of the valid prefix. An incomplete or checksum-failing frame ends
/// decoding cleanly (the torn-tail rule: our writer only ever syncs whole
/// frames, so a partial or mangled frame can only be the tail a crash tore;
/// mid-file rot is indistinguishable from a torn tail and also truncates —
/// documented limitation).
pub(crate) fn decode_frames(bytes: &[u8]) -> (Vec<LedgerRecord>, usize) {
    let mut records = Vec::new();
    let mut at = 0usize;
    loop {
        let rest = &bytes[at..];
        if rest.len() < FRAME_HEADER {
            break;
        }
        // Infallible: length checked above.
        let len_bytes: [u8; 4] = rest[..4].try_into().unwrap_or([0; 4]);
        let len = u32::from_le_bytes(len_bytes) as usize;
        let Some(frame_end) = FRAME_HEADER.checked_add(len) else {
            break;
        };
        if rest.len() < frame_end {
            break; // torn tail: frame extends past the durable bytes
        }
        let check = &rest[4..FRAME_HEADER];
        let payload = &rest[FRAME_HEADER..frame_end];
        if &blake3::hash(payload).as_bytes()[..8] != check {
            break; // torn or rotten frame
        }
        let Ok(record) = serde_json::from_slice::<LedgerRecord>(payload) else {
            break; // checksummed but undecodable: treat as tail damage
        };
        records.push(record);
        at += frame_end;
    }
    (records, at)
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

    #[test]
    fn frame_codec_round_trips_and_tolerates_torn_tail() {
        let records = vec![
            LedgerRecord::Genesis {
                config: CampaignConfigId::digest(b"cfg"),
            },
            LedgerRecord::Commit {
                proposal: ProposalId::new(1),
                revision: Revision::new(1),
                batch: EvidenceBatchId::digest(b"batch"),
                terminal: TerminalRecord { moment: 7, work: 9 },
            },
        ];
        let mut bytes = Vec::new();
        for r in &records {
            encode_frame(r, &mut bytes).unwrap();
        }
        let (decoded, len) = decode_frames(&bytes);
        assert_eq!(decoded, records);
        assert_eq!(len, bytes.len());

        // Every truncation of the final frame decodes to exactly the prefix.
        let (first, first_len) = {
            let mut one = Vec::new();
            encode_frame(&records[0], &mut one).unwrap();
            (one.clone(), one.len())
        };
        for cut in first_len..bytes.len() {
            let (decoded, len) = decode_frames(&bytes[..cut]);
            assert_eq!(decoded, records[..1], "cut at {cut}");
            assert_eq!(len, first.len());
        }

        // A flipped payload byte fails the checksum and ends decoding.
        let mut mangled = bytes.clone();
        let last = mangled.len() - 1;
        mangled[last] ^= 0xff;
        let (decoded, _) = decode_frames(&mangled);
        assert_eq!(decoded, records[..1]);
    }
}
