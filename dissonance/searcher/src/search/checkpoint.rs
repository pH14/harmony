// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::{BTreeMap, VecDeque, btree_map::Entry},
    error::Error,
    fs::{self, File, OpenOptions},
    io::{BufWriter, Read, Seek, SeekFrom, Write},
    num::NonZeroU64,
    path::{Path, PathBuf},
    time::Instant,
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

pub const SEARCH_CHECKPOINT_FORMAT: &str = "dissonance-search-checkpoint-v4";
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
        serde_json::to_vec(value)
            .map_err(serde::ser::Error::custom)?
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
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CheckpointHeader {
    pub format: String,
    pub reason: String,
    pub workload_identity_sha256: String,
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
}

#[derive(Clone, Debug, Serialize)]
struct CheckpointLogLine<'a> {
    executions: u64,
    reason: &'a str,
    file: String,
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

#[derive(Clone, Copy, Debug)]
struct StoredRange {
    offset: u64,
    len: u64,
    listings: u64,
}

pub(crate) struct CheckpointWriter<P> {
    plan: CheckpointPlan,
    store: BufWriter<File>,
    store_len: u64,
    live_bytes: u64,
    stored: BTreeMap<u64, StoredRange>,
    last_marks: usize,
    last_top: Option<P>,
    intervals: VecDeque<(PathBuf, Vec<u64>)>,
}

impl<P: Copy + Ord> CheckpointWriter<P> {
    pub(crate) fn create(
        plan: CheckpointPlan,
        marks: usize,
        top: Option<P>,
    ) -> Result<Self, Box<dyn Error>> {
        fs::create_dir_all(&plan.directory)?;
        let store = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(plan.directory.join(SNAPSHOT_STORE))
            .map_err(|error| {
                format!("checkpoint directory already holds a snapshot store: {error}")
            })?;
        Ok(Self {
            plan,
            store: BufWriter::with_capacity(1 << 22, store),
            store_len: 0,
            live_bytes: 0,
            stored: BTreeMap::new(),
            last_marks: marks,
            last_top: top,
            intervals: VecDeque::new(),
        })
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

    pub(crate) fn write<'a, S: Serialize + 'a>(
        &mut self,
        header: &CheckpointHeader,
        snapshots: impl Iterator<Item = (u64, &'a S)>,
        body: impl FnOnce(&mut dyn Write) -> Result<(), Box<dyn Error>>,
    ) -> Result<(), Box<dyn Error>> {
        #[allow(clippy::disallowed_methods)]
        let started = Instant::now();
        let mut index = Vec::new();
        let mut new_snapshots = 0_usize;
        let mut new_snapshot_bytes = 0_u64;
        for (id, snapshot) in snapshots {
            let stored = match self.stored.entry(id) {
                Entry::Occupied(entry) => {
                    let stored = entry.into_mut();
                    stored.listings = stored.listings.saturating_add(1);
                    *stored
                }
                Entry::Vacant(entry) => {
                    let bytes = postcard::to_allocvec(snapshot)?;
                    let len = u64::try_from(bytes.len())?;
                    self.store.write_all(&bytes)?;
                    let stored = StoredRange {
                        offset: self.store_len,
                        len,
                        listings: 1,
                    };
                    self.store_len = self.store_len.saturating_add(len);
                    self.live_bytes = self.live_bytes.saturating_add(len);
                    new_snapshots = new_snapshots.saturating_add(1);
                    new_snapshot_bytes = new_snapshot_bytes.saturating_add(len);
                    *entry.insert(stored)
                }
            };
            index.push(StoredSnapshot {
                id,
                offset: stored.offset,
                len: stored.len,
            });
        }
        self.store.flush()?;
        self.store.get_ref().sync_data()?;
        let name = format!(
            "{:012}-{}.{CHECKPOINT_EXTENSION}",
            header.executions, header.reason
        );
        let path = self.plan.directory.join(&name);
        let partial = self.plan.directory.join(format!("{name}.partial"));
        let file = File::create(&partial)?;
        let mut out = BufWriter::with_capacity(1 << 22, file);
        postcard::to_io(header, &mut out)?;
        postcard::to_io(&index, &mut out)?;
        body(&mut out)?;
        out.flush()?;
        let file = out.into_inner().map_err(|error| error.into_error())?;
        file.sync_data()?;
        let checkpoint_bytes = file.metadata()?.len();
        drop(file);
        fs::rename(&partial, &path)?;
        let mut freed = (0_usize, 0_u64);
        if header.reason == "interval" {
            self.intervals
                .push_back((path, index.iter().map(|stored| stored.id).collect()));
            let mut pruned = Vec::new();
            while self.intervals.len() > INTERVAL_CHECKPOINTS_KEPT {
                if let Some((oldest, ids)) = self.intervals.pop_front() {
                    fs::remove_file(oldest)?;
                    pruned.push(ids);
                }
            }
            if !pruned.is_empty() {
                File::open(&self.plan.directory)?.sync_all()?;
            }
            for ids in pruned {
                let (snapshots, bytes) = self.release(&ids)?;
                freed = (
                    freed.0.saturating_add(snapshots),
                    freed.1.saturating_add(bytes),
                );
            }
        }
        let line = serde_json::to_string(&CheckpointLogLine {
            executions: header.executions,
            reason: &header.reason,
            file: name,
            write_seconds: started.elapsed().as_secs_f64(),
            snapshots: index.len(),
            new_snapshots,
            new_snapshot_bytes,
            checkpoint_bytes,
            store_bytes: self.store_len,
            freed_snapshots: freed.0,
            freed_snapshot_bytes: freed.1,
            live_snapshot_bytes: self.live_bytes,
        })?;
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.plan.directory.join(CHECKPOINT_LOG))?;
        log.write_all(line.as_bytes())?;
        log.write_all(b"\n")?;
        Ok(())
    }

    fn release(&mut self, ids: &[u64]) -> Result<(usize, u64), Box<dyn Error>> {
        let mut freed = Vec::new();
        for &id in ids {
            let Entry::Occupied(mut entry) = self.stored.entry(id) else {
                return Err("a pruned checkpoint lists a snapshot the store does not hold".into());
            };
            entry.get_mut().listings -= 1;
            if entry.get().listings == 0 {
                freed.push(entry.remove());
            }
        }
        freed.sort_unstable_by_key(|stored| stored.offset);
        let mut bytes = 0_u64;
        let mut hole: Option<(u64, u64)> = None;
        for stored in freed.iter().filter(|stored| stored.len > 0) {
            bytes = bytes.saturating_add(stored.len);
            hole = match hole {
                Some((offset, len)) if offset.saturating_add(len) == stored.offset => {
                    Some((offset, len.saturating_add(stored.len)))
                }
                Some((offset, len)) => {
                    punch_hole(self.store.get_ref(), offset, len)?;
                    Some((stored.offset, stored.len))
                }
                None => Some((stored.offset, stored.len)),
            };
        }
        if let Some((offset, len)) = hole {
            punch_hole(self.store.get_ref(), offset, len)?;
        }
        self.live_bytes = self.live_bytes.saturating_sub(bytes);
        Ok((freed.len(), bytes))
    }
}

#[cfg(target_os = "linux")]
fn punch_hole(store: &File, offset: u64, len: u64) -> std::io::Result<()> {
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
fn punch_hole(_: &File, _: u64, _: u64) -> std::io::Result<()> {
    Ok(())
}

pub fn read_header(path: &Path) -> Result<CheckpointHeader, Box<dyn Error>> {
    Ok(CheckpointReader::open(path)?.header)
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
        let bytes = fs::read(path)?;
        let file_sha256 = format!("{:x}", Sha256::digest(&bytes));
        let (header, rest): (CheckpointHeader, _) = postcard::take_from_bytes(&bytes)?;
        if header.format != SEARCH_CHECKPOINT_FORMAT {
            return Err("search checkpoint format is not recognized".into());
        }
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

    pub(crate) fn snapshots<S: DeserializeOwned>(&self) -> Result<Vec<(u64, S)>, Box<dyn Error>> {
        let mut order = self.index.clone();
        order.sort_by_key(|stored| stored.offset);
        let mut store = File::open(&self.store)?;
        let mut snapshots = Vec::with_capacity(order.len());
        let mut buffer = Vec::new();
        for stored in order {
            store.seek(SeekFrom::Start(stored.offset))?;
            buffer.resize(usize::try_from(stored.len)?, 0);
            store.read_exact(&mut buffer)?;
            snapshots.push((stored.id, postcard::from_bytes(&buffer)?));
        }
        Ok(snapshots)
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
        }
    }

    #[test]
    fn checkpoints_fall_due_on_intervals_new_marks_and_a_higher_top_tier() {
        let directory =
            std::env::temp_dir().join(format!("dissonance-checkpoint-due-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        let mut writer = CheckpointWriter::<u8>::create(plan(directory.clone()), 0, Some(1))
            .expect("create the writer");
        assert_eq!(writer.due(3, 0, Some(1)), None);
        assert_eq!(writer.due(10, 0, Some(1)), Some("interval"));
        assert_eq!(writer.due(11, 1, Some(1)), Some("milestone"));
        assert_eq!(writer.due(12, 1, Some(1)), None);
        assert_eq!(writer.due(13, 1, Some(2)), Some("top_progress"));
        assert_eq!(writer.due(14, 1, Some(1)), None);
        assert_eq!(writer.due(20, 2, Some(3)), Some("milestone"));
        fs::remove_dir_all(&directory).expect("remove the directory");
    }

    #[test]
    fn a_checkpoint_stores_each_snapshot_once_and_reads_it_back() {
        let directory = std::env::temp_dir().join(format!(
            "dissonance-checkpoint-store-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&directory);
        let mut writer =
            CheckpointWriter::<u8>::create(plan(directory.clone()), 0, None).expect("create");
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
            CheckpointWriter::<u8>::create(plan(directory.clone()), 0, None).is_err(),
            "a directory holds one run's snapshot store"
        );
        fs::remove_dir_all(&directory).expect("remove the directory");
    }

    fn header(executions: u64, reason: &str) -> CheckpointHeader {
        CheckpointHeader {
            format: SEARCH_CHECKPOINT_FORMAT.to_owned(),
            reason: reason.to_owned(),
            workload_identity_sha256: String::new(),
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
        writer: &mut CheckpointWriter<u8>,
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
        let mut writer =
            CheckpointWriter::<u8>::create(plan(directory.clone()), 0, None).expect("create");
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
        assert_eq!(writer.stored.keys().copied().collect::<Vec<_>>(), [2, 3, 4]);
        write_snapshots(&mut writer, 40, "interval", &[5]);
        write_snapshots(&mut writer, 50, "interval", &[5, 6]);
        assert_eq!(
            writer.stored.keys().copied().collect::<Vec<_>>(),
            [2, 3, 5, 6]
        );
        let snapshot_len =
            u64::try_from(postcard::to_allocvec(&snapshot(1)).expect("encode").len())
                .expect("length");
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
            6 * snapshot_len
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
        let mut writer =
            CheckpointWriter::<u8>::create(plan(directory.clone()), 0, None).expect("create");
        for (executions, id) in [(10, 1), (20, 2), (30, 3)] {
            writer
                .write(
                    &header(executions, "interval"),
                    std::iter::once((id, &())),
                    |out| Ok(postcard::to_io(&executions, out).map(|_| ())?),
                )
                .expect("write the checkpoint");
        }
        assert_eq!(writer.stored.keys().copied().collect::<Vec<_>>(), [2, 3]);
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
        let mut writer =
            CheckpointWriter::<u8>::create(plan(directory.clone()), 0, None).expect("create");
        let header = CheckpointHeader {
            format: "dissonance-search-checkpoint-v2".to_owned(),
            reason: "interval".to_owned(),
            workload_identity_sha256: String::new(),
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
