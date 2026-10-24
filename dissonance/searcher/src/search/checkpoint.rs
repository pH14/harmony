// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::{BTreeMap, VecDeque},
    error::Error,
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Write},
    num::NonZeroU64,
    path::{Path, PathBuf},
    time::Instant,
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

pub mod image;
pub(crate) mod publisher;

pub const SEARCH_CHECKPOINT_FORMAT: &str = "dissonance-search-checkpoint-v11";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PinnedCheckpoint {
    pub path: PathBuf,
    pub sha256: String,
}
const SNAPSHOT_STORE: &str = "snapshots.store";
const CHECKPOINT_LOG: &str = "checkpoints.jsonl";
const CHECKPOINT_EXTENSION: &str = "ckpt";
const INTERVAL_CHECKPOINTS_KEPT: usize = 2;

pub(crate) mod json_bytes {
    use serde::{Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned};

    pub(crate) fn serialize<T: Serialize, S: Serializer>(
        value: &T,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        super::image::StreamedBytes(|out: &mut dyn std::io::Write| {
            serde_json::to_writer(out, value)?;
            Ok(())
        })
        .serialize(serializer)
    }

    pub(crate) fn deserialize<'de, T: DeserializeOwned, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<T, D::Error> {
        let bytes = Vec::<u8>::deserialize(deserializer)?;
        serde_json::from_slice(&bytes).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug)]
pub struct CheckpointPlan {
    pub directory: PathBuf,
    pub every: Option<NonZeroU64>,
    pub on_marks: bool,
    pub on_top_progress: bool,
    pub checkpoint_owner: Option<String>,
    pub max_image_bytes: usize,
    #[cfg(test)]
    pub(crate) before_write: Option<publisher::TestWriteHook>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CheckpointHeader {
    pub format: String,
    pub checkpoint_owner: Option<String>,
    pub reason: String,
    pub workload_identity_sha256: String,
    pub executable_identity: String,
    pub campaign_seed: u64,
    pub window: usize,
    pub archive_entry_limit: usize,
    pub memory_budget_mib: Option<usize>,
    pub policies: BTreeMap<String, String>,
    pub executions: u64,
    pub reserved: u64,
    pub admitted: usize,
    pub planned_clock: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StoredSnapshot {
    pub id: u64,
    pub offset: u64,
    pub len: u64,
    pub sha256: [u8; 32],
}

#[derive(Clone, Debug, Serialize)]
struct CheckpointLogLine<'a> {
    executions: u64,
    reserved: u64,
    reason: &'a str,
    file: String,
    checkpoint_sha256: String,
    write_seconds: f64,
    snapshots: usize,
    new_snapshots: usize,
    new_snapshot_bytes: u64,
    checkpoint_bytes: u64,
    store_bytes: u64,
    freed_snapshots: usize,
    freed_snapshot_bytes: u64,
    live_snapshot_bytes: u64,
}

struct HashWriter<W> {
    inner: W,
    digest: Sha256,
}

impl<W: Write> Write for HashWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.digest.update(&buf[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

pub(crate) struct CheckpointSchedule<P> {
    plan: CheckpointPlan,
    last_marks: usize,
    last_top: Option<P>,
}
impl<P: Copy + Ord> CheckpointSchedule<P> {
    pub(crate) fn new(plan: CheckpointPlan, marks: usize, top: Option<P>) -> Self {
        Self {
            plan,
            last_marks: marks,
            last_top: top,
        }
    }
    pub(crate) fn due(
        &mut self,
        executions: u64,
        marks: usize,
        top: Option<P>,
    ) -> Option<&'static str> {
        let mut reason = None;
        if self.plan.on_marks && marks > self.last_marks {
            reason = Some("milestone");
        }
        if self.plan.on_top_progress && top > self.last_top {
            reason = reason.or(Some("top_progress"));
        }
        if self
            .plan
            .every
            .is_some_and(|every| executions.is_multiple_of(every.get()))
        {
            reason = reason.or(Some("interval"));
        }
        self.last_marks = self.last_marks.max(marks);
        self.last_top = self.last_top.max(top);
        reason
    }
}

#[derive(Serialize)]
struct CheckpointHeaderRef<'a> {
    format: &'a str,
    checkpoint_owner: Option<&'a str>,
    reason: &'a str,
    workload_identity_sha256: &'a str,
    executable_identity: &'a str,
    campaign_seed: u64,
    window: usize,
    archive_entry_limit: usize,
    memory_budget_mib: Option<usize>,
    policies: &'a BTreeMap<String, String>,
    executions: u64,
    reserved: u64,
    admitted: usize,
    planned_clock: u64,
}

pub(crate) struct CheckpointStoreWriter {
    plan: CheckpointPlan,
    frames: super::snapshot_store::StoreFrames,
    intervals: VecDeque<(PathBuf, Vec<u64>)>,
    #[cfg(test)]
    io: PublicationIoTest,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PublicationIo {
    CheckpointSync,
    Rename,
    PublicationDirectorySync,
    Prune,
    PruneDirectorySync,
    JournalSync,
    JournalDirectorySync,
}
#[cfg(test)]
#[derive(Default)]
struct PublicationIoTest {
    fail: Option<PublicationIo>,
    trace: Vec<PublicationIo>,
    header_policy_buffers: Vec<(usize, usize)>,
}
impl CheckpointStoreWriter {
    pub(crate) fn create(plan: CheckpointPlan) -> Result<Self, Box<dyn Error>> {
        if plan.max_image_bytes == 0 {
            return Err("checkpoint image byte limit is zero".into());
        }
        let absolute = if plan.directory.is_absolute() {
            plan.directory.clone()
        } else {
            std::env::current_dir()?.join(&plan.directory)
        };
        let mut created = Vec::new();
        for directory in absolute.ancestors() {
            match fs::metadata(directory) {
                Ok(_) => break,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    created.push(directory.to_owned())
                }
                Err(error) => return Err(error.into()),
            }
        }
        fs::create_dir_all(&plan.directory)?;
        for directory in created.into_iter().rev() {
            File::open(&directory)?.sync_all()?;
            if let Some(parent) = directory.parent() {
                File::open(parent)?.sync_all()?;
            }
        }
        let store = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(plan.directory.join(SNAPSHOT_STORE))
            .map_err(|error| {
                format!("checkpoint directory already holds a snapshot store: {error}")
            })?;
        store.sync_all()?;
        File::open(&plan.directory)?.sync_all()?;
        Ok(Self {
            plan,
            frames: super::snapshot_store::StoreFrames::new(store),
            intervals: VecDeque::new(),
            #[cfg(test)]
            io: PublicationIoTest::default(),
        })
    }

    fn publication_io(
        &mut self,
        phase: PublicationIo,
        operation: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        #[cfg(test)]
        {
            self.io.trace.push(phase);
            if self.io.fail == Some(phase) {
                return Err(io::Error::other(format!("injected {phase:?} failure")));
            }
        }
        operation()
            .map_err(|error| io::Error::new(error.kind(), format!("checkpoint {phase:?}: {error}")))
    }
    fn sync_directory(&mut self, phase: PublicationIo) -> io::Result<()> {
        let directory = File::open(&self.plan.directory)?;
        self.publication_io(phase, || directory.sync_all())
    }

    pub(crate) fn write<'a, S: super::snapshot_codec::CheckpointSnapshot + 'a>(
        &mut self,
        header: &CheckpointHeader,
        snapshots: impl Iterator<Item = (u64, &'a S)>,
        body: impl FnOnce(&mut dyn Write) -> Result<(), Box<dyn Error>>,
    ) -> Result<(), Box<dyn Error>> {
        let serialized_header = CheckpointHeaderRef {
            format: &header.format,
            checkpoint_owner: self.plan.checkpoint_owner.as_deref(),
            reason: &header.reason,
            workload_identity_sha256: &header.workload_identity_sha256,
            executable_identity: &header.executable_identity,
            campaign_seed: header.campaign_seed,
            window: header.window,
            archive_entry_limit: header.archive_entry_limit,
            memory_budget_mib: header.memory_budget_mib,
            policies: &header.policies,
            executions: header.executions,
            reserved: header.reserved,
            admitted: header.admitted,
            planned_clock: header.planned_clock,
        };
        #[allow(clippy::disallowed_methods)]
        let started = Instant::now();
        let stage = self.frames.stage(snapshots)?;
        let index = stage.index.clone();
        let new_snapshots = stage.new_snapshot_count();
        let new_snapshot_bytes = stage.new_bytes;
        let name = format!(
            "{:012}-{}.{CHECKPOINT_EXTENSION}",
            header.executions, header.reason
        );
        let path = self.plan.directory.join(&name);
        let partial = self.plan.directory.join(format!("{name}.partial"));
        let file = File::create(&partial)?;
        let mut out = BufWriter::with_capacity(
            1 << 22,
            HashWriter {
                inner: file,
                digest: Sha256::new(),
            },
        );
        #[cfg(test)]
        {
            self.io.header_policy_buffers = serialized_header
                .policies
                .iter()
                .flat_map(|(key, value)| {
                    [
                        (key.as_ptr() as usize, key.len()),
                        (value.as_ptr() as usize, value.len()),
                    ]
                })
                .collect();
        }
        postcard::to_io(&serialized_header, &mut out)?;
        postcard::to_io(&index, &mut out)?;
        body(&mut out)?;
        out.flush()?;
        let hashing = out.into_inner().map_err(|error| error.into_error())?;
        let checksum = hashing.digest.clone().finalize();
        let mut checkpoint_digest = hashing.digest;
        checkpoint_digest.update(checksum);
        let checkpoint_sha256 = format!("{:x}", checkpoint_digest.finalize());
        let mut file = hashing.inner;
        file.write_all(&checksum)?;
        self.publication_io(PublicationIo::CheckpointSync, || file.sync_all())?;
        let checkpoint_bytes = file.metadata()?.len();
        drop(file);
        self.publication_io(PublicationIo::Rename, || fs::rename(&partial, &path))?;
        self.sync_directory(PublicationIo::PublicationDirectorySync)?;
        self.frames.publish(stage);
        let mut freed = (0_usize, 0_u64);
        if header.reason == "interval" {
            self.intervals
                .push_back((path, index.iter().map(|stored| stored.id).collect()));
            let mut pruned = Vec::new();
            while self.intervals.len() > INTERVAL_CHECKPOINTS_KEPT {
                if let Some((oldest, ids)) = self.intervals.pop_front() {
                    self.publication_io(PublicationIo::Prune, || fs::remove_file(oldest))?;
                    pruned.push(ids);
                }
            }
            if !pruned.is_empty() {
                self.sync_directory(PublicationIo::PruneDirectorySync)?;
            }
            for ids in pruned {
                let (snapshots, bytes) = self.frames.release(&ids)?;
                freed = (
                    freed.0.saturating_add(snapshots),
                    freed.1.saturating_add(bytes),
                );
            }
        }
        let line = serde_json::to_string(&CheckpointLogLine {
            executions: header.executions,
            reserved: header.reserved,
            reason: &header.reason,
            file: name,
            checkpoint_sha256,
            write_seconds: started.elapsed().as_secs_f64(),
            snapshots: index.len(),
            new_snapshots,
            new_snapshot_bytes,
            checkpoint_bytes,
            store_bytes: self.frames.len,
            freed_snapshots: freed.0,
            freed_snapshot_bytes: freed.1,
            live_snapshot_bytes: self.frames.live_bytes,
        })?;
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.plan.directory.join(CHECKPOINT_LOG))?;
        log.write_all(line.as_bytes())?;
        log.write_all(b"\n")?;
        self.publication_io(PublicationIo::JournalSync, || log.sync_all())?;
        self.sync_directory(PublicationIo::JournalDirectorySync)?;
        Ok(())
    }
}

#[cfg(target_os = "linux")]
pub(super) fn punch_hole(store: &File, offset: u64, len: u64) -> std::io::Result<()> {
    use rustix::{
        fs::{FallocateFlags, fallocate},
        io::Errno,
    };
    match fallocate(
        store,
        FallocateFlags::PUNCH_HOLE | FallocateFlags::KEEP_SIZE,
        offset,
        len,
    ) {
        Err(Errno::OPNOTSUPP) => Ok(()),
        result => result.map_err(std::io::Error::from),
    }
}

#[cfg(not(target_os = "linux"))]
pub(super) fn punch_hole(_: &File, _: u64, _: u64) -> std::io::Result<()> {
    Ok(())
}

pub fn read_header(path: &Path) -> Result<CheckpointHeader, Box<dyn Error>> {
    Ok(CheckpointReader::open(path)?.header)
}

pub fn read_header_with_sha(path: &Path) -> Result<(CheckpointHeader, String), Box<dyn Error>> {
    let reader = CheckpointReader::open(path)?;
    Ok((reader.header, reader.file_sha256))
}

pub(crate) struct CheckpointReader {
    pub(crate) header: CheckpointHeader,
    pub(crate) file_sha256: String,
    index: Vec<StoredSnapshot>,
    store: PathBuf,
    bytes: Vec<u8>,
    cursor: usize,
}

impl CheckpointReader {
    pub(crate) fn open(path: &Path) -> Result<Self, Box<dyn Error>> {
        let mut bytes = fs::read(path)?;
        let (format, _): (String, _) = postcard::take_from_bytes(&bytes)?;
        if format != SEARCH_CHECKPOINT_FORMAT {
            return Err("search checkpoint format is not recognized".into());
        }
        let payload_len = bytes
            .len()
            .checked_sub(32)
            .ok_or("search checkpoint checksum is missing")?;
        let actual: [u8; 32] = Sha256::digest(&bytes[..payload_len]).into();
        if bytes[payload_len..] != actual {
            return Err("search checkpoint checksum differs from its bytes".into());
        }
        let file_sha256 = format!("{:x}", Sha256::digest(&bytes));
        bytes.truncate(payload_len);
        let (header, rest): (CheckpointHeader, _) = postcard::take_from_bytes(&bytes)?;
        let (index, rest): (Vec<StoredSnapshot>, _) = postcard::take_from_bytes(rest)?;
        let cursor = bytes.len() - rest.len();
        let store = path
            .parent()
            .ok_or("search checkpoint has no directory")?
            .join(SNAPSHOT_STORE);
        Ok(Self {
            header,
            file_sha256,
            index,
            store,
            bytes,
            cursor,
        })
    }

    pub(crate) fn next<T: DeserializeOwned>(&mut self) -> Result<T, Box<dyn Error>> {
        let (value, rest) = postcard::take_from_bytes(&self.bytes[self.cursor..])?;
        self.cursor = self.bytes.len() - rest.len();
        Ok(value)
    }

    pub(crate) fn finish(self) -> Result<(), Box<dyn Error>> {
        if self.cursor != self.bytes.len() {
            return Err("search checkpoint has trailing bytes".into());
        }
        Ok(())
    }

    pub(crate) fn snapshots<S: super::snapshot_codec::CheckpointSnapshot>(
        &self,
    ) -> Result<Vec<(u64, S)>, Box<dyn Error>> {
        super::snapshot_store::read::<S>(&mut File::open(&self.store)?, &self.index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(directory: PathBuf) -> CheckpointPlan {
        CheckpointPlan {
            directory,
            every: NonZeroU64::new(10),
            on_marks: true,
            on_top_progress: true,
            checkpoint_owner: None,
            max_image_bytes: 64 << 20,
            before_write: None,
        }
    }

    #[test]
    fn checkpoints_fall_due_on_intervals_new_marks_and_a_higher_top_tier() {
        let directory =
            std::env::temp_dir().join(format!("dissonance-checkpoint-due-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        let mut writer = CheckpointSchedule::<u8>::new(plan(directory.clone()), 0, Some(1));
        assert_eq!(writer.due(3, 0, Some(1)), None);
        assert_eq!(writer.due(10, 0, Some(1)), Some("interval"));
        assert_eq!(writer.due(11, 1, Some(1)), Some("milestone"));
        assert_eq!(writer.due(12, 1, Some(1)), None);
        assert_eq!(writer.due(13, 1, Some(2)), Some("top_progress"));
        assert_eq!(writer.due(14, 1, Some(1)), None);
        assert_eq!(writer.due(20, 2, Some(3)), Some("milestone"));
        assert!(!directory.exists());
    }

    #[test]
    fn streaming_json_keeps_the_existing_checkpoint_body_bytes() {
        #[derive(Serialize, Deserialize, Debug, PartialEq)]
        struct Json(#[serde(with = "json_bytes")] serde_json::Value);
        let value = serde_json::json!({"message": "checkpoint λ\n\"", "samples": (0..1024).collect::<Vec<u64>>(), "empty": []});
        let old = postcard::to_allocvec(&serde_json::to_vec(&value).unwrap()).unwrap();
        let actual = postcard::to_allocvec(&Json(value.clone())).unwrap();
        assert_eq!(actual, old);
        assert_eq!(postcard::from_bytes::<Json>(&actual).unwrap(), Json(value));
    }

    #[test]
    fn every_publication_syncs_its_rename_and_journal_before_acknowledging() {
        let directory =
            std::env::temp_dir().join(format!("checkpoint-durable-order-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        let mut writer = CheckpointStoreWriter::create(plan(directory.clone())).unwrap();
        for (execution, reason) in [
            (1, "milestone"),
            (2, "top_progress"),
            (3, "final"),
            (4, "interval"),
            (5, "interval"),
            (6, "interval"),
        ] {
            writer.io.trace.clear();
            writer
                .write(
                    &header(execution, reason),
                    std::iter::once((execution, &vec![7_u8])),
                    |out| {
                        postcard::to_io(&execution, out)?;
                        Ok(())
                    },
                )
                .unwrap();
            let mut expected = vec![
                PublicationIo::CheckpointSync,
                PublicationIo::Rename,
                PublicationIo::PublicationDirectorySync,
            ];
            if execution == 6 {
                expected.extend([PublicationIo::Prune, PublicationIo::PruneDirectorySync]);
            }
            expected.extend([
                PublicationIo::JournalSync,
                PublicationIo::JournalDirectorySync,
            ]);
            assert_eq!(writer.io.trace, expected);
        }
        assert!(!directory.join("000000000004-interval.ckpt").exists());
        drop(writer);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn publication_failures_never_damage_a_previously_kept_checkpoint() {
        for phase in [
            PublicationIo::CheckpointSync,
            PublicationIo::Rename,
            PublicationIo::PublicationDirectorySync,
            PublicationIo::Prune,
            PublicationIo::PruneDirectorySync,
            PublicationIo::JournalSync,
            PublicationIo::JournalDirectorySync,
        ] {
            let directory = std::env::temp_dir().join(format!(
                "checkpoint-durable-failure-{phase:?}-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&directory);
            let mut writer = CheckpointStoreWriter::create(plan(directory.clone())).unwrap();
            for execution in 1..=2 {
                writer
                    .write(
                        &header(execution, "interval"),
                        std::iter::once((execution, &vec![execution as u8])),
                        |out| {
                            postcard::to_io(&execution, out)?;
                            Ok(())
                        },
                    )
                    .unwrap();
            }
            writer.io.fail = Some(phase);
            let error = writer
                .write(
                    &header(3, "interval"),
                    std::iter::once((3, &vec![3_u8])),
                    |out| {
                        postcard::to_io(&3_u64, out)?;
                        Ok(())
                    },
                )
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains(&format!("injected {phase:?} failure"))
            );
            let mut kept =
                CheckpointReader::open(&directory.join("000000000002-interval.ckpt")).unwrap();
            assert_eq!(kept.next::<u64>().unwrap(), 2);
            assert_eq!(kept.snapshots::<Vec<u8>>().unwrap(), [(2, vec![2])]);
            kept.finish().unwrap();
            drop(writer);
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn written_checkpoint_carries_the_run_owner() {
        let directory = std::env::temp_dir().join(format!(
            "dissonance-checkpoint-owner-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&directory);
        let mut plan = plan(directory.clone());
        plan.checkpoint_owner = Some("saved-run-a".into());
        let mut writer = CheckpointStoreWriter::create(plan).expect("create");
        writer
            .write(
                &header(1, "final"),
                std::iter::empty::<(u64, &Vec<u8>)>(),
                |out| Ok(postcard::to_io(&1_u64, out).map(|_| ())?),
            )
            .expect("write checkpoint");
        let path = directory.join("000000000001-final.ckpt");
        assert_eq!(
            read_header(&path).expect("read header").checkpoint_owner,
            Some("saved-run-a".into())
        );
        let journal = fs::read_to_string(directory.join(CHECKPOINT_LOG)).expect("read journal");
        let record: serde_json::Value = serde_json::from_str(journal.trim()).expect("record");
        assert_eq!(
            record["checkpoint_sha256"],
            format!(
                "{:x}",
                Sha256::digest(fs::read(path).expect("read checkpoint"))
            )
        );
        drop(writer);
        fs::remove_dir_all(directory).expect("remove directory");
    }

    #[test]
    fn a_checkpoint_stores_each_snapshot_once_and_reads_it_back() {
        let directory = std::env::temp_dir().join(format!(
            "dissonance-checkpoint-store-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&directory);
        let mut writer = CheckpointStoreWriter::create(plan(directory.clone())).expect("create");
        let first = [(1_u64, vec![1_u8, 2]), (2, vec![3])];
        writer
            .write(
                &header(10, "interval"),
                first.iter().map(|(id, snapshot)| (*id, snapshot)),
                |out| Ok(postcard::to_io(&7_u32, out).map(|_| ())?),
            )
            .expect("write the first checkpoint");
        let second = [(2_u64, vec![3_u8]), (4, vec![5, 6, 7])];
        writer
            .write(
                &header(20, "interval"),
                second.iter().map(|(id, snapshot)| (*id, snapshot)),
                |out| Ok(postcard::to_io(&9_u32, out).map(|_| ())?),
            )
            .expect("write the second checkpoint");
        let log = fs::read_to_string(directory.join(CHECKPOINT_LOG)).expect("read the log");
        let new: Vec<u64> = log
            .lines()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line).expect("log line")["new_snapshots"]
                    .as_u64()
                    .expect("count")
            })
            .collect();
        assert_eq!(new, [2, 1]);
        let mut reader = CheckpointReader::open(&directory.join("000000000020-interval.ckpt"))
            .expect("open the second checkpoint");
        assert_eq!(reader.header, header(20, "interval"));
        assert_eq!(reader.next::<u32>().expect("body"), 9);
        let snapshots = reader.snapshots::<Vec<u8>>().expect("read the snapshots");
        let mut snapshots = snapshots;
        snapshots.sort_unstable();
        assert_eq!(snapshots, [(2, vec![3]), (4, vec![5, 6, 7])]);
        reader.finish().expect("no trailing bytes");
        writer
            .write(
                &header(30, "interval"),
                std::iter::empty::<(u64, &Vec<u8>)>(),
                |out| Ok(postcard::to_io(&11_u32, out).map(|_| ())?),
            )
            .expect("write the third checkpoint");
        assert!(!directory.join("000000000010-interval.ckpt").exists());
        assert!(directory.join("000000000020-interval.ckpt").exists());
        assert!(directory.join("000000000030-interval.ckpt").exists());
        assert!(
            CheckpointStoreWriter::create(plan(directory.clone())).is_err(),
            "a directory holds one run's snapshot store"
        );
        fs::remove_dir_all(&directory).expect("remove the directory");
    }

    #[test]
    fn failed_checkpoint_body_does_not_retain_staged_snapshot_frames() {
        let directory = std::env::temp_dir().join(format!(
            "dissonance-checkpoint-publish-failure-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&directory);
        let mut writer = CheckpointStoreWriter::create(plan(directory.clone())).unwrap();
        writer
            .write(
                &header(1, "final"),
                std::iter::once((1, &vec![1u8, 2])),
                |_| Ok(()),
            )
            .unwrap();
        let bytes = writer.frames.live_bytes;
        assert!(
            writer
                .write(
                    &header(2, "interval"),
                    [(1, &vec![1u8, 2]), (2, &vec![3u8])].into_iter(),
                    |_| Err("injected checkpoint body failure".into())
                )
                .is_err()
        );
        assert_eq!(writer.frames.snapshot_ids(), vec![1]);
        assert_eq!(writer.frames.live_bytes, bytes);
        let reader = CheckpointReader::open(&directory.join("000000000001-final.ckpt")).unwrap();
        assert_eq!(
            reader.snapshots::<Vec<u8>>().unwrap(),
            vec![(1, vec![1, 2])]
        );
        writer
            .write(
                &header(3, "final"),
                std::iter::once((2, &vec![3u8])),
                |_| Ok(()),
            )
            .unwrap();
        assert_eq!(writer.frames.snapshot_ids(), vec![1, 2]);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn checkpoint_reader_rejects_changed_body() {
        let directory = std::env::temp_dir().join(format!(
            "dissonance-checkpoint-body-integrity-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&directory);
        let mut writer = CheckpointStoreWriter::create(plan(directory.clone())).unwrap();
        writer
            .write(
                &header(10, "interval"),
                std::iter::empty::<(u64, &Vec<u8>)>(),
                |out| Ok(postcard::to_io(&7_u32, out).map(|_| ())?),
            )
            .unwrap();
        let path = directory.join("000000000010-interval.ckpt");
        let mut bytes = fs::read(&path).unwrap();
        let body_end = bytes.len() - 32;
        bytes[body_end - 1] ^= 1;
        fs::write(&path, bytes).unwrap();
        assert!(CheckpointReader::open(&path).is_err());
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn checkpoint_reader_rejects_changed_snapshot_bytes() {
        let directory = std::env::temp_dir().join(format!(
            "dissonance-checkpoint-snapshot-integrity-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&directory);
        let mut writer = CheckpointStoreWriter::create(plan(directory.clone())).unwrap();
        let snapshots = [(1_u64, vec![1_u8, 2])];
        writer
            .write(
                &header(10, "interval"),
                snapshots.iter().map(|(id, snapshot)| (*id, snapshot)),
                |out| Ok(postcard::to_io(&7_u32, out).map(|_| ())?),
            )
            .unwrap();
        let store_path = directory.join(SNAPSHOT_STORE);
        let mut bytes = fs::read(&store_path).unwrap();
        bytes[1] ^= 1;
        fs::write(&store_path, bytes).unwrap();
        let reader = CheckpointReader::open(&directory.join("000000000010-interval.ckpt")).unwrap();
        assert!(reader.snapshots::<Vec<u8>>().is_err());
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn older_checkpoint_reports_unsupported_format() {
        let directory = std::env::temp_dir().join(format!(
            "dissonance-checkpoint-old-format-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir(&directory).unwrap();
        for format in [
            "dissonance-search-checkpoint-v4",
            "dissonance-search-checkpoint-v8",
            "dissonance-search-checkpoint-v9",
            "dissonance-search-checkpoint-v10",
        ] {
            let mut old = header(10, "interval");
            old.format = format.into();
            let path = directory.join("old.ckpt");
            fs::write(&path, postcard::to_allocvec(&old).unwrap()).unwrap();
            assert_eq!(
                CheckpointReader::open(&path).err().unwrap().to_string(),
                "search checkpoint format is not recognized"
            );
        }
        fs::remove_dir_all(&directory).unwrap();
    }

    fn header(executions: u64, reason: &str) -> CheckpointHeader {
        CheckpointHeader {
            format: SEARCH_CHECKPOINT_FORMAT.to_owned(),
            checkpoint_owner: None,
            reason: reason.to_owned(),
            workload_identity_sha256: String::new(),
            executable_identity: String::new(),
            campaign_seed: 1,
            window: 1,
            archive_entry_limit: 8,
            memory_budget_mib: None,
            policies: BTreeMap::new(),
            executions,
            reserved: executions,
            admitted: 0,
            planned_clock: 0,
        }
    }

    fn snapshot(id: u64) -> Vec<u8> {
        vec![u8::try_from(id).expect("small id"); 1 << 18]
    }

    fn write_snapshots(
        writer: &mut CheckpointStoreWriter,
        executions: u64,
        reason: &str,
        ids: &[u64],
    ) {
        let snapshots: Vec<(u64, Vec<u8>)> = ids.iter().map(|&id| (id, snapshot(id))).collect();
        writer
            .write(
                &header(executions, reason),
                snapshots.iter().map(|(id, snapshot)| (*id, snapshot)),
                |out| Ok(postcard::to_io(&executions, out).map(|_| ())?),
            )
            .expect("write the checkpoint");
    }

    #[cfg(target_os = "linux")]
    fn allocated_bytes(path: &Path) -> u64 {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(path).expect("file metadata").blocks() * 512
    }

    #[cfg(target_os = "linux")]
    fn punches_holes(directory: &Path) -> bool {
        let path = directory.join("hole-probe");
        let mut probe = File::create(&path).expect("create the probe");
        probe.write_all(&[1; 1 << 20]).expect("fill the probe");
        probe.sync_all().expect("sync the probe");
        let before = allocated_bytes(&path);
        let punched = rustix::fs::fallocate(
            &probe,
            rustix::fs::FallocateFlags::PUNCH_HOLE | rustix::fs::FallocateFlags::KEEP_SIZE,
            0,
            1 << 20,
        )
        .is_ok()
            && allocated_bytes(&path) < before;
        fs::remove_file(&path).expect("remove the probe");
        punched
    }

    #[test]
    fn a_pruned_interval_checkpoint_frees_the_snapshots_only_it_listed() {
        let directory = std::env::temp_dir().join(format!(
            "dissonance-checkpoint-release-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&directory);
        let mut writer = CheckpointStoreWriter::create(plan(directory.clone())).expect("create");
        write_snapshots(&mut writer, 10, "interval", &[1, 2, 3]);
        write_snapshots(&mut writer, 15, "milestone", &[2, 3]);
        write_snapshots(&mut writer, 20, "interval", &[2, 3, 4]);
        #[cfg(target_os = "linux")]
        let allocated = allocated_bytes(&directory.join(SNAPSHOT_STORE));
        write_snapshots(&mut writer, 30, "interval", &[3, 4]);
        #[cfg(target_os = "linux")]
        if punches_holes(&directory) {
            assert!(
                allocated_bytes(&directory.join(SNAPSHOT_STORE)) + (1 << 17) < allocated,
                "pruning the first interval checkpoint released no store blocks"
            );
        }
        assert_eq!(writer.frames.snapshot_ids(), [2, 3, 4]);
        write_snapshots(&mut writer, 40, "interval", &[5]);
        write_snapshots(&mut writer, 50, "interval", &[5, 6]);
        assert_eq!(writer.frames.snapshot_ids(), [2, 3, 5, 6]);
        let snapshot_len = writer.frames.snapshot_len(2);
        let header_len = writer.frames.header_len();
        let log: Vec<serde_json::Value> = fs::read_to_string(directory.join(CHECKPOINT_LOG))
            .expect("read the log")
            .lines()
            .map(|line| serde_json::from_str(line).expect("log line"))
            .collect();
        let field = |name: &str| -> Vec<u64> {
            log.iter()
                .map(|line| line[name].as_u64().expect("count"))
                .collect()
        };
        assert_eq!(field("freed_snapshots"), [0, 0, 0, 1, 0, 1]);
        assert_eq!(
            field("live_snapshot_bytes"),
            [3, 3, 4, 3, 4, 4].map(|count| count * snapshot_len)
        );
        assert_eq!(
            *field("store_bytes").last().expect("line"),
            header_len + 6 * snapshot_len
        );
        for (executions, reason, ids) in [
            (15, "milestone", vec![2, 3]),
            (40, "interval", vec![5]),
            (50, "interval", vec![5, 6]),
        ] {
            let mut reader =
                CheckpointReader::open(&directory.join(format!("{executions:012}-{reason}.ckpt")))
                    .expect("open a kept checkpoint");
            assert_eq!(reader.next::<u64>().expect("body"), executions);
            let mut snapshots = reader.snapshots::<Vec<u8>>().expect("read the snapshots");
            snapshots.sort_unstable();
            assert_eq!(
                snapshots,
                ids.into_iter()
                    .map(|id| (id, snapshot(id)))
                    .collect::<Vec<_>>()
            );
            reader.finish().expect("no trailing bytes");
        }
        for executions in [10, 20, 30] {
            assert!(
                !directory
                    .join(format!("{executions:012}-interval.ckpt"))
                    .exists()
            );
        }
        fs::remove_dir_all(&directory).expect("remove the directory");
    }

    #[test]
    fn pruning_releases_snapshots_that_encode_to_no_bytes() {
        let directory = std::env::temp_dir().join(format!(
            "dissonance-checkpoint-empty-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&directory);
        let mut writer = CheckpointStoreWriter::create(plan(directory.clone())).expect("create");
        for (executions, id) in [(10, 1), (20, 2), (30, 3)] {
            writer
                .write(
                    &header(executions, "interval"),
                    std::iter::once((id, &())),
                    |out| Ok(postcard::to_io(&executions, out).map(|_| ())?),
                )
                .expect("write the checkpoint");
        }
        assert_eq!(writer.frames.snapshot_ids(), [2, 3]);
        let mut reader = CheckpointReader::open(&directory.join("000000000030-interval.ckpt"))
            .expect("open the last checkpoint");
        assert_eq!(reader.next::<u64>().expect("body"), 30);
        assert_eq!(reader.snapshots::<()>().expect("read"), [(3, ())]);
        fs::remove_dir_all(&directory).expect("remove the directory");
    }

    #[test]
    fn a_checkpoint_of_another_format_is_refused_before_its_body_is_read() {
        let directory = std::env::temp_dir().join(format!(
            "dissonance-checkpoint-format-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&directory);
        let mut writer = CheckpointStoreWriter::create(plan(directory.clone())).expect("create");
        let header = CheckpointHeader {
            format: "dissonance-search-checkpoint-v9".to_owned(),
            checkpoint_owner: None,
            reason: "interval".to_owned(),
            workload_identity_sha256: String::new(),
            executable_identity: String::new(),
            campaign_seed: 1,
            window: 1,
            archive_entry_limit: 8,
            memory_budget_mib: None,
            policies: BTreeMap::new(),
            executions: 10,
            reserved: 10,
            admitted: 0,
            planned_clock: 0,
        };
        writer
            .write(&header, std::iter::empty::<(u64, &Vec<u8>)>(), |out| {
                Ok(postcard::to_io(&7_u32, out).map(|_| ())?)
            })
            .expect("write the checkpoint");
        let refused = CheckpointReader::open(&directory.join("000000000010-interval.ckpt"))
            .err()
            .expect("another format is refused");
        assert_eq!(
            refused.to_string(),
            "search checkpoint format is not recognized"
        );
        fs::remove_dir_all(&directory).expect("remove the directory");
    }
}
