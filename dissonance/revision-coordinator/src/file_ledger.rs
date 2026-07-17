// SPDX-License-Identifier: AGPL-3.0-or-later
//! The M1 file-backed [`Ledger`]: one append-only file — a versioned stream
//! header plus checksummed frames — fsynced at every `sync` barrier.
//!
//! Portable by construction (Convention rule 6): plain `std::fs` only — no
//! `memfd_create`, no `io_uring`, no `/proc`, no `#[cfg(target_os)]` forks.
//! Appends are staged in userspace and hit the file only inside `sync`
//! (write + `fdatasync`), so the durable prefix on disk is always a whole
//! number of sync batches and the [`Ledger`] durability contract (a crash
//! loses at most the unsynced suffix) holds by construction rather than by
//! hoping about kernel writeback order.
//!
//! Damage handling (PR #124 FAM-WAL ruling): `open` repairs a genuine tear
//! — an incomplete final frame or a truncated header from a crash mid-write
//! — by truncating it away and fsyncing the repaired file BEFORE any replay
//! is exposed (F4), so later appends can never hide behind an undecodable
//! frame. *Interior* damage (a checksum failure on a complete frame, an
//! over-bound length, a foreign or future-format header) is a typed refusal
//! ([`LedgerError::Corrupt`] / [`LedgerError::UnsupportedVersion`]), never
//! a silent truncation — truncating there would drop durable records and
//! remint committed revisions on recovery (F3/F10).

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::ledger::{
    Ledger, LedgerError, LedgerRecord, decode_stream, encode_frame, encode_stream_header,
};

/// The directory whose entry must be fsynced for `path`'s creation to be
/// durable. An empty parent (a bare relative filename like `"wal"`) means
/// the current directory (PR #124 F8 — `File::open("")` fails).
fn creation_dir(path: &Path) -> &Path {
    match path.parent() {
        None => Path::new("."),
        Some(p) if p.as_os_str().is_empty() => Path::new("."),
        Some(p) => p,
    }
}

/// File-backed append-only ledger. See the module docs for the layout and
/// durability story.
#[derive(Debug)]
pub struct FileLedger {
    path: PathBuf,
    file: File,
    /// Encoded frames staged since the last `sync`.
    pending: Vec<u8>,
}

impl FileLedger {
    /// Open (creating if absent) the ledger at `path`. A torn tail from a
    /// crash mid-write is truncated away and the repair is fsynced before
    /// this returns; interior damage or an unsupported format version is a
    /// typed error, never a repair.
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
            File::open(creation_dir(path))?.sync_all()?;
        }

        let bytes = std::fs::read(path)?;
        // Interior damage / bad magic / unknown version refuse here.
        let (_, valid_len) = decode_stream(&bytes)?;

        if valid_len < bytes.len() {
            // Genuine tear (incomplete final frame or truncated header):
            // truncate the damage so new frames append after the last
            // whole one.
            file.set_len(valid_len as u64)?;
        }
        if valid_len == 0 {
            // Brand-new (or torn-at-creation) stream: write the versioned
            // header.
            let mut header = Vec::new();
            encode_stream_header(&mut header);
            (&file).write_all(&header)?;
        }
        // F4 + PR #124 VERIFY V2: UNCONDITIONAL barrier before any replay
        // is exposed — not just after a repair. fsync is per-inode, so this
        // also flushes pages a dead writer left dirty in the page cache
        // (killed between its write_all and sync_data, the stream is
        // complete-looking but not durable; the clean path must not skip
        // the barrier).
        file.sync_data()?;
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
        let (records, _) = decode_stream(&bytes)?;
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
    use crate::ledger::{LedgerRecord, STREAM_HEADER, STREAM_VERSION};

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
        // The stream carries the versioned header.
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..4], b"HWAL");
        assert_eq!(
            u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
            STREAM_VERSION
        );
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

    #[test]
    fn torn_creation_is_repaired_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wal");
        // A crash between creation and the header sync leaves a strict
        // prefix of the header (including the empty file).
        for cut in 0..STREAM_HEADER {
            let full: Vec<u8> = {
                let mut v = Vec::new();
                crate::ledger::encode_stream_header(&mut v);
                v
            };
            std::fs::write(&path, &full[..cut]).unwrap();
            let mut l = FileLedger::open(&path).unwrap();
            assert_eq!(l.replay().unwrap(), vec![], "cut at {cut}");
            l.append(&sample()[0]).unwrap();
            l.sync().unwrap();
            assert_eq!(l.replay().unwrap(), sample()[..1]);
        }
    }

    /// PR #124 F3 regression at the file level: the judge's flipped-byte
    /// probe. Interior damage refuses to open — it must never silently
    /// truncate 4 of 5 durable records and let recovery remint a committed
    /// revision.
    #[test]
    fn interior_damage_refuses_to_open_and_replay() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wal");
        let mut l = FileLedger::open(&path).unwrap();
        for r in sample() {
            l.append(&r).unwrap();
        }
        l.sync().unwrap();
        // Keep a handle from BEFORE the damage to exercise replay() too.
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[STREAM_HEADER + 18] ^= 0xff; // inside the first frame's payload
        std::fs::write(&path, &bytes).unwrap();
        assert!(matches!(
            FileLedger::open(&path),
            Err(LedgerError::Corrupt { .. })
        ));
        assert!(matches!(l.replay(), Err(LedgerError::Corrupt { .. })));
    }

    /// PR #124 VERIFY V1 at the file level: a corrupted in-bound frame
    /// length landing past end-of-stream must refuse to open — never
    /// physically truncate committed records and let recovery re-mint
    /// their revisions.
    #[test]
    fn past_eof_length_corruption_refuses_to_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wal");
        let mut l = FileLedger::open(&path).unwrap();
        for r in sample() {
            l.append(&r).unwrap();
        }
        l.sync().unwrap();
        drop(l);
        let before = std::fs::read(&path).unwrap();
        let mut bytes = before.clone();
        // First frame's length -> 983,040 (under the bound, past EOF).
        bytes[STREAM_HEADER..STREAM_HEADER + 4].copy_from_slice(&983_040u32.to_le_bytes());
        std::fs::write(&path, &bytes).unwrap();
        assert!(matches!(
            FileLedger::open(&path),
            Err(LedgerError::Corrupt { .. })
        ));
        // Refusal, not repair: the file was not truncated.
        assert_eq!(std::fs::read(&path).unwrap().len(), before.len());
    }

    /// PR #124 F10: a future format version is a typed refusal, not a
    /// misparse or a repair.
    #[test]
    fn future_version_refuses_to_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wal");
        drop(FileLedger::open(&path).unwrap());
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[4] = 9; // version 9
        std::fs::write(&path, &bytes).unwrap();
        match FileLedger::open(&path) {
            Err(LedgerError::UnsupportedVersion { found, supported }) => {
                assert_eq!(found, 9);
                assert_eq!(supported, STREAM_VERSION);
            }
            other => panic!("expected UnsupportedVersion, got {other:?}"),
        }
    }

    /// PR #124 F8: a bare relative filename has `parent() == Some("")`;
    /// the creation dirsync must target `"."`, not `""`.
    #[test]
    fn creation_dir_maps_empty_parent_to_cwd() {
        assert_eq!(creation_dir(Path::new("wal")), Path::new("."));
        assert_eq!(creation_dir(Path::new("a/wal")), Path::new("a"));
        assert_eq!(creation_dir(Path::new("/tmp/wal")), Path::new("/tmp"));
    }
}
