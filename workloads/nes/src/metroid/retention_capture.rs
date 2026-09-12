// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded snapshot capture for replay diagnostics, outside search policy state.

use super::{
    archive::MetroidArchiveKey,
    target::{ButtonChord, MetroidInput, MetroidSnapshot},
};
use crate::search::archive::{ArchiveKey, RetentionObservation};
use serde::{Deserialize, Serialize};
use std::{
    error::Error,
    fs::{File, OpenOptions},
    io::{BufReader, BufWriter, Read, Write},
    path::Path,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
type Observation<'a> = RetentionObservation<'a, ButtonChord, MetroidArchiveKey, MetroidSnapshot>;

const FORMAT: &str = "metroid-local-retention-snapshots-v1";
const MAX_RECORDS: u64 = 5_000;
const MAX_ACTIONS: usize = 4_096;
const MAX_FRAME_BYTES: usize = 256 * 1024;
const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;

/// Exact replay stream and checkpoint that give the local inputs their origin.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CaptureIdentity {
    /// Hash of the bytes actually passed to the campaign replayer.
    pub stream_sha256: String,
    /// Hash of the checkpoint bytes, not a path or a final cached-state census.
    pub origin_sha256: String,
}

impl CaptureIdentity {
    fn valid(&self) -> bool {
        [&self.stream_sha256, &self.origin_sha256]
            .into_iter()
            .all(|s| {
                s.len() == 64
                    && s.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
    }
}

/// One actual single-member local competition. HP is not decoded here.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetentionSnapshotRecord {
    /// One-based competition order, including admitted and rejected candidates.
    pub ordinal: u64,
    /// Ordered campaign job; more than one competition can occur in a job.
    pub execution: u64,
    /// Stable ID of the current member, never an inactive cached substitute.
    pub incumbent_id: u64,
    /// Whether the local rule replaces this sole incumbent.
    pub replaces: bool,
    pub candidate_key: MetroidArchiveKey,
    pub incumbent_key: MetroidArchiveKey,
    pub candidate_snapshot: MetroidSnapshot,
    /// Missing cached state remains unavailable. No implicit replay occurs.
    pub incumbent_snapshot: Option<MetroidSnapshot>,
    /// Input relative to the checkpoint origin named by `CaptureIdentity`.
    pub candidate_input: MetroidInput,
    /// Input relative to the same checkpoint origin.
    pub incumbent_input: MetroidInput,
}

impl RetentionSnapshotRecord {
    fn valid(&self) -> bool {
        self.ordinal > 0
            && self.ordinal <= MAX_RECORDS
            && self.execution > 0
            && self.candidate_input.actions.len() <= MAX_ACTIONS
            && self.incumbent_input.actions.len() <= MAX_ACTIONS
            && self.candidate_key.group(0) == self.incumbent_key.group(0)
    }
}

#[derive(Deserialize, Serialize)]
enum Frame {
    Header {
        format: String,
        identity: CaptureIdentity,
        snapshot_format: String,
        key_policy: String,
    },
    Competition(Box<RetentionSnapshotRecord>),
    Complete {
        records: u64,
        bytes_before_footer: u64,
    },
}

/// Exhaustive bounded capture, enabled only by an explicit diagnostic caller.
/// Errors leave an incomplete file; no later call may certify it complete.
pub(crate) struct RetentionCapture {
    writer: BufWriter<File>,
    scratch: Vec<u8>,
    bytes: u64,
    records: u64,
    failed: bool,
    finished: bool,
}

impl RetentionCapture {
    pub(crate) fn create(path: &Path, identity: CaptureIdentity) -> Result<Self> {
        if !identity.valid() {
            return Err("capture requires exact lowercase stream/origin SHA-256 values".into());
        }
        let file = OpenOptions::new().create_new(true).write(true).open(path)?;
        let mut capture = Self {
            writer: BufWriter::new(file),
            scratch: vec![0; MAX_FRAME_BYTES],
            bytes: 0,
            records: 0,
            failed: false,
            finished: false,
        };
        capture.write_frame(&Frame::Header {
            format: FORMAT.into(),
            identity,
            snapshot_format: super::campaign::SNAPSHOT_CHECKPOINT_FORMAT.into(),
            key_policy: super::archive::KEY_POLICY_IDENTIFIER.into(),
        })?;
        Ok(capture)
    }

    fn write_frame(&mut self, value: &Frame) -> Result<()> {
        let bytes = postcard::to_slice(value, &mut self.scratch)?;
        let size = u32::try_from(bytes.len())?;
        let total = self
            .bytes
            .checked_add(u64::from(size) + 4)
            .ok_or("capture length overflow")?;
        if total > MAX_FILE_BYTES {
            return Err("retention capture output ceiling reached".into());
        }
        self.writer.write_all(&size.to_le_bytes())?;
        self.writer.write_all(bytes)?;
        self.bytes = total;
        Ok(())
    }

    pub(crate) fn observe(&mut self, event: &Observation<'_>) -> Result<()> {
        if self.failed {
            return Err("retention capture previously failed".into());
        }
        if self.finished {
            return Ok(());
        }
        // Every fallible path below stays failed unless the complete frame was written.
        self.failed = true;
        if self.records >= MAX_RECORDS || event.slot_member_count != 1 {
            return Err("capture requires at most 5000 single-member competitions".into());
        }
        let members = (event.slot_members)()?;
        let [member] = members.as_slice() else {
            return Err("capture slot count disagrees with its members".into());
        };
        if member.key != event.incumbent.0
            || member.snapshot != event.incumbent.1
            || event.replaces != event.candidate_admitted
            || member.retained_by_local_rule == event.candidate_admitted
        {
            return Err("capture requires the actual ordinary local competitor".into());
        }
        let (candidate_input, incumbent_input) = (event.inputs)()?;
        if candidate_input.actions.len() > MAX_ACTIONS
            || incumbent_input.actions.len() > MAX_ACTIONS
            || incumbent_input != member.input
        {
            return Err("capture input exceeds bounds or names a different incumbent".into());
        }
        // Check encoded snapshot bounds in the fixed buffer before cloning any
        // payload. Oversized diagnostic state must leave an incomplete capture.
        postcard::to_slice(event.candidate.1, &mut self.scratch)?;
        if let Some(snapshot) = member.snapshot {
            postcard::to_slice(snapshot, &mut self.scratch)?;
        }
        let record = RetentionSnapshotRecord {
            ordinal: self.records + 1,
            execution: event.execution,
            incumbent_id: member.id,
            replaces: event.replaces,
            candidate_key: event.candidate.0,
            incumbent_key: member.key,
            candidate_snapshot: event.candidate.1.clone(),
            incumbent_snapshot: member.snapshot.cloned(),
            candidate_input,
            incumbent_input,
        };
        if !record.valid() {
            return Err("invalid local retention capture record".into());
        }
        self.write_frame(&Frame::Competition(Box::new(record)))?;
        self.records += 1;
        self.failed = false;
        Ok(())
    }

    pub(crate) fn finish(&mut self) -> Result<()> {
        if self.failed {
            return Err("cannot complete a failed retention capture".into());
        }
        if !self.finished {
            self.failed = true;
            self.write_frame(&Frame::Complete {
                records: self.records,
                bytes_before_footer: self.bytes,
            })?;
            self.writer.flush()?;
            self.finished = true;
            self.failed = false;
        }
        Ok(())
    }
}

/// Streaming reader: a successful terminal `None` verifies the completion footer.
/// Individual rows from an interrupted read never establish exhaustive coverage.
pub struct CaptureReader {
    reader: BufReader<File>,
    identity: CaptureIdentity,
    bytes: u64,
    records: u64,
    finished: bool,
    failed: bool,
}

fn read_frame(reader: &mut impl Read, bytes: &mut u64) -> Result<Frame> {
    let mut size = [0; 4];
    reader.read_exact(&mut size)?;
    let size = usize::try_from(u32::from_le_bytes(size))?;
    if size == 0 || size > MAX_FRAME_BYTES {
        return Err("invalid capture frame length".into());
    }
    *bytes = bytes
        .checked_add(u64::try_from(size)? + 4)
        .ok_or("capture length overflow")?;
    if *bytes > MAX_FILE_BYTES {
        return Err("capture exceeds output ceiling".into());
    }
    let mut payload = vec![0; size];
    reader.read_exact(&mut payload)?;
    let (value, remaining) = postcard::take_from_bytes(&payload)?;
    if !remaining.is_empty() {
        return Err("capture frame has trailing bytes".into());
    }
    Ok(value)
}

impl CaptureReader {
    /// Refuse oversized files, wrong formats and incomplete headers before rows.
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        if file.metadata()?.len() > MAX_FILE_BYTES {
            return Err("capture exceeds output ceiling".into());
        }
        let mut reader = BufReader::new(file);
        let mut bytes = 0;
        let Frame::Header {
            format,
            identity,
            snapshot_format,
            key_policy,
        } = read_frame(&mut reader, &mut bytes)?
        else {
            return Err("capture is missing its header".into());
        };
        if format != FORMAT
            || !identity.valid()
            || snapshot_format != super::campaign::SNAPSHOT_CHECKPOINT_FORMAT
            || key_policy != super::archive::KEY_POLICY_IDENTIFIER
        {
            return Err("unsupported capture format or identity".into());
        }
        Ok(Self {
            reader,
            identity,
            bytes,
            records: 0,
            finished: false,
            failed: false,
        })
    }

    #[must_use]
    pub fn identity(&self) -> &CaptureIdentity {
        &self.identity
    }

    /// Read one bounded competition or verify the exact complete-file footer.
    pub fn next_competition(&mut self) -> Result<Option<RetentionSnapshotRecord>> {
        if self.failed {
            return Err("capture reader previously failed".into());
        }
        if self.finished {
            return Ok(None);
        }
        self.failed = true;
        let before = self.bytes;
        match read_frame(&mut self.reader, &mut self.bytes)? {
            Frame::Competition(record) => {
                if !record.valid() || record.ordinal != self.records + 1 {
                    return Err("capture order, group or input bounds disagree".into());
                }
                self.records += 1;
                self.failed = false;
                Ok(Some(*record))
            }
            Frame::Complete {
                records,
                bytes_before_footer,
            } => {
                if records != self.records || bytes_before_footer != before {
                    return Err("capture footer disagrees with its rows".into());
                }
                if self.reader.read(&mut [0])? != 0 {
                    return Err("capture continues after its completion footer".into());
                }
                self.finished = true;
                self.failed = false;
                Ok(None)
            }
            Frame::Header { .. } => Err("unexpected second capture header".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        metroid::{
            campaign::{MetroidCampaignRun, MetroidGame},
            target::MetroidMechanicalState,
        },
        search::{
            archive::{EntrySelectorCounters, RetentionSlotMember},
            campaign::{InputPolicy, Reporting},
        },
    };
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    struct TestFile(PathBuf);
    impl TestFile {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            Self(std::env::temp_dir().join(format!(
                "harmony-retention-capture-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            )))
        }
    }
    impl Drop for TestFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    fn identity() -> CaptureIdentity {
        CaptureIdentity {
            stream_sha256: "a".repeat(64),
            origin_sha256: "b".repeat(64),
        }
    }
    fn snapshot() -> MetroidSnapshot {
        // Serialization fixture only; never restored into an emulator.
        serde_json::from_value(serde_json::json!({"emulator_state":[1,2,3],
            "observation":{"frame_count":0,"decoded":MetroidMechanicalState::default(),
                "boss_defeats":{"kraid":false,"ridley":false},"mother_brain_status":0,
                "tourian_events":{"mother_brain_defeated":false,"escape_started":false},
                "endpoint_boss_slots":null,"changed_indices":[],"dead":false,"log_line":""},
            "failed":false}))
        .unwrap()
    }
    fn observe(
        capture: &mut RetentionCapture,
        incumbent: Option<&MetroidSnapshot>,
        actions: usize,
    ) -> Result<()> {
        let candidate = snapshot();
        let key = MetroidArchiveKey::default();
        let input = MetroidInput {
            actions: vec![
                ButtonChord {
                    buttons: 1,
                    hold_frames: 1
                };
                actions
            ],
        };
        let inputs = || Ok((input.clone(), input.clone()));
        let members = || {
            Ok(vec![RetentionSlotMember {
                id: 7,
                key,
                snapshot: incumbent,
                input: input.clone(),
                retained_by_local_rule: true,
            }])
        };
        capture.observe(&Observation {
            execution: 3,
            candidate: (key, &candidate),
            incumbent: (key, incumbent),
            replaces: false,
            candidate_admitted: false,
            created_execution: 1,
            exposure: EntrySelectorCounters {
                selected: 2,
                productive: 1,
            },
            in_window_ever: true,
            inputs: &inputs,
            slot_member_count: 1,
            slot_members: &members,
        })
    }
    fn complete_file(path: &Path) {
        let mut capture = RetentionCapture::create(path, identity()).unwrap();
        observe(&mut capture, Some(&snapshot()), 2).unwrap();
        observe(&mut capture, None, 1).unwrap();
        capture.finish().unwrap();
        capture.finish().unwrap();
        // The campaign's finish hook disables capture during later verification.
        observe(&mut capture, None, 1).unwrap();
    }

    #[test]
    fn round_trip_preserves_origin_members_missing_state_and_complete_order() {
        let path = TestFile::new();
        complete_file(&path.0);
        assert!(RetentionCapture::create(&path.0, identity()).is_err());
        let mut reader = CaptureReader::open(&path.0).unwrap();
        assert_eq!(reader.identity(), &identity());
        let first = reader.next_competition().unwrap().unwrap();
        assert_eq!(
            (first.ordinal, first.execution, first.incumbent_id),
            (1, 3, 7)
        );
        assert_eq!(first.candidate_snapshot, snapshot());
        assert_eq!(first.incumbent_snapshot, Some(snapshot()));
        assert_eq!(first.candidate_input.actions.len(), 2);
        let second = reader.next_competition().unwrap().unwrap();
        assert_eq!(second.ordinal, 2);
        assert_eq!(second.incumbent_snapshot, None);
        assert!(reader.next_competition().unwrap().is_none());
        assert!(reader.next_competition().unwrap().is_none());
    }

    #[test]
    fn failed_bounds_cannot_be_resumed_or_certified_complete() {
        for failure in 0..4 {
            let path = TestFile::new();
            let mut capture = RetentionCapture::create(&path.0, identity()).unwrap();
            match failure {
                0 => capture.records = MAX_RECORDS,
                1 => capture.bytes = MAX_FILE_BYTES,
                2 => capture.scratch.truncate(8),
                _ => (),
            }
            let actions = if failure == 3 { MAX_ACTIONS + 1 } else { 1 };
            assert!(observe(&mut capture, None, actions).is_err());
            assert!(observe(&mut capture, None, 1).is_err());
            assert!(capture.finish().is_err());
            drop(capture);
            let mut reader = CaptureReader::open(&path.0).unwrap();
            assert!(reader.next_competition().is_err());
            assert!(reader.next_competition().is_err());
        }
    }

    #[test]
    fn truncated_footer_extra_bytes_and_bad_lengths_are_not_complete() {
        for extra in [false, true] {
            let path = TestFile::new();
            complete_file(&path.0);
            let mut bytes = std::fs::read(&path.0).unwrap();
            if extra {
                bytes.push(0);
            } else {
                bytes.pop();
            }
            std::fs::write(&path.0, bytes).unwrap();
            let mut reader = CaptureReader::open(&path.0).unwrap();
            assert!(reader.next_competition().unwrap().is_some());
            assert!(reader.next_competition().unwrap().is_some());
            assert!(reader.next_competition().is_err());
            assert!(reader.next_competition().is_err());
        }
        let mut bytes_read = 0;
        let bad_size = u32::try_from(MAX_FRAME_BYTES + 1).unwrap().to_le_bytes();
        assert!(read_frame(&mut bad_size.as_slice(), &mut bytes_read).is_err());
    }

    #[test]
    fn capture_is_absent_from_policy_identity_and_constructs_no_target() {
        let path = TestFile::new();
        let game = MetroidGame::new(&[], Path::new("unused-core"), &"a".repeat(64));
        let before = game.policies(&MetroidCampaignRun);
        let game = game.with_retention_capture(&path.0, identity()).unwrap();
        assert_eq!(game.policies(&MetroidCampaignRun), before);
        game.finish_retention_observation().unwrap();
        assert!(
            CaptureReader::open(&path.0)
                .unwrap()
                .next_competition()
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn incompatible_key_or_snapshot_encodings_are_rejected_at_the_header() {
        for wrong_key in [false, true] {
            let path = TestFile::new();
            let header = Frame::Header {
                format: FORMAT.into(),
                identity: identity(),
                snapshot_format: if wrong_key {
                    super::super::campaign::SNAPSHOT_CHECKPOINT_FORMAT.into()
                } else {
                    "incompatible-snapshot".into()
                },
                key_policy: if wrong_key {
                    "incompatible-key".into()
                } else {
                    super::super::archive::KEY_POLICY_IDENTIFIER.into()
                },
            };
            let payload = postcard::to_allocvec(&header).unwrap();
            let mut bytes = u32::try_from(payload.len()).unwrap().to_le_bytes().to_vec();
            bytes.extend(payload);
            std::fs::write(&path.0, bytes).unwrap();
            assert!(CaptureReader::open(&path.0).is_err());
        }
    }
}
