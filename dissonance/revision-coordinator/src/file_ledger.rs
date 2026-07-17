// SPDX-License-Identifier: AGPL-3.0-or-later
//! The M1 file-backed [`Ledger`]: one append-only file of checksummed
//! frames, fsynced at every `sync` barrier.
//!
//! Portable by construction (Convention rule 6): plain `std::fs` only — no
//! `memfd_create`, no `io_uring`, no `/proc`, no `#[cfg(target_os)]` forks.
//! Appends are staged in userspace and hit the file only inside `sync`
//! (write + `fdatasync`), so the durable prefix on disk is always a whole
//! number of sync batches and the [`Ledger`] durability contract (a crash
//! loses at most the unsynced suffix) holds by construction rather than by
//! hoping about kernel writeback order.
//!
//! `open` repairs a torn tail: if the file ends in a partial or
//! checksum-failing frame (a crash tore the final write), the damage is
//! truncated away before any new append, so later appends can never be
//! shadowed behind an undecodable frame.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::ledger::{Ledger, LedgerError, LedgerRecord, decode_frames, encode_frame};

/// File-backed append-only ledger. See the module docs for the layout and
/// durability story.
pub struct FileLedger {
    path: PathBuf,
    file: File,
    /// Encoded frames staged since the last `sync`.
    pending: Vec<u8>,
}

impl FileLedger {
    /// Open (creating if absent) the ledger at `path`, repairing any torn
    /// tail left by a crash mid-write.
    pub fn open(path: &Path) -> Result<Self, LedgerError> {
        let created = !path.exists();
        let file = OpenOptions::new()
            .read(true)
            .create(true)
            .append(true)
            .open(path)?;
        if created {
            // Make the file's existence itself durable: fsync the parent
            // directory (macOS and Linux both allow a read-only open of a
            // directory for this purpose).
            if let Some(parent) = path.parent()
                && !parent.as_os_str().is_empty()
            {
                File::open(parent)?.sync_all()?;
            }
        }
        let bytes = std::fs::read(path)?;
        let (_, valid_len) = decode_frames(&bytes);
        if valid_len < bytes.len() {
            // Torn tail from a crash mid-write: truncate the damage so new
            // frames append after the last whole one.
            file.set_len(valid_len as u64)?;
            file.sync_data()?;
        }
        Ok(FileLedger {
            path: path.to_path_buf(),
            file,
            pending: Vec::new(),
        })
    }
}

impl Ledger for FileLedger {
    fn append(&mut self, record: &LedgerRecord) -> Result<(), LedgerError> {
        encode_frame(record, &mut self.pending)
    }

    fn sync(&mut self) -> Result<(), LedgerError> {
        if !self.pending.is_empty() {
            self.file.write_all(&self.pending)?;
            self.file.sync_data()?;
            self.pending.clear();
        }
        Ok(())
    }

    fn replay(&self) -> Result<Vec<LedgerRecord>, LedgerError> {
        // Read from disk, not from this handle's buffers: replay is defined
        // as the crash-durable prefix, and only synced frames are on disk.
        let bytes = std::fs::read(&self.path)?;
        let (records, _) = decode_frames(&bytes);
        Ok(records)
    }

    fn reopen(&self) -> Result<Box<dyn Ledger>, LedgerError> {
        Ok(Box::new(FileLedger::open(&self.path)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{CampaignConfigId, CohortId, EvidenceBatchId, ProposalId, Revision};
    use crate::ledger::LedgerRecord;

    fn sample() -> Vec<LedgerRecord> {
        vec![
            LedgerRecord::Genesis {
                config: CampaignConfigId::digest(b"cfg"),
            },
            LedgerRecord::CohortOpen {
                cohort: CohortId::new(1),
                view: Revision::ZERO,
            },
            LedgerRecord::Proposal {
                proposal: ProposalId::new(1),
                revision: Revision::new(1),
                cohort: CohortId::new(1),
            },
            LedgerRecord::Commit {
                proposal: ProposalId::new(1),
                revision: Revision::new(1),
                batch: EvidenceBatchId::digest(b"b"),
                terminal: crate::ids::TerminalRecord { moment: 3, work: 5 },
            },
        ]
    }

    #[test]
    fn file_ledger_round_trips_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wal");
        let mut l = FileLedger::open(&path).unwrap();
        for r in sample() {
            l.append(&r).unwrap();
        }
        l.sync().unwrap();
        drop(l);
        let l2 = FileLedger::open(&path).unwrap();
        assert_eq!(l2.replay().unwrap(), sample());
    }

    #[test]
    fn unsynced_appends_are_not_durable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wal");
        let mut l = FileLedger::open(&path).unwrap();
        let recs = sample();
        l.append(&recs[0]).unwrap();
        l.sync().unwrap();
        l.append(&recs[1]).unwrap();
        // No sync: a crash (drop) loses the tail.
        drop(l);
        let l2 = FileLedger::open(&path).unwrap();
        assert_eq!(l2.replay().unwrap(), recs[..1]);
    }

    #[test]
    fn torn_tail_is_repaired_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wal");
        let mut l = FileLedger::open(&path).unwrap();
        for r in sample() {
            l.append(&r).unwrap();
            l.sync().unwrap();
        }
        drop(l);
        let full = std::fs::read(&path).unwrap();
        // Tear the final frame at every possible byte boundary.
        for cut in (full.len() - 20)..full.len() {
            std::fs::write(&path, &full[..cut]).unwrap();
            let l2 = FileLedger::open(&path).unwrap();
            let got = l2.replay().unwrap();
            assert_eq!(got, sample()[..3], "cut at {cut}");
            drop(l2);
            // The torn bytes were truncated away; appending works again.
            let mut l3 = FileLedger::open(&path).unwrap();
            l3.append(&sample()[3]).unwrap();
            l3.sync().unwrap();
            assert_eq!(l3.replay().unwrap(), sample());
        }
    }
}
