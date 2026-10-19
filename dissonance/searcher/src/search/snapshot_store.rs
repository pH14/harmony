// SPDX-License-Identifier: AGPL-3.0-or-later

use super::{
    checkpoint::StoredSnapshot,
    snapshot_codec::{
        CheckpointSnapshot, CodecResult, EncodedSnapshot, RecordKey, SharedRecord,
        SharedRecordSink, SnapshotCodec, exact, read_frame, write_frame,
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{BufWriter, Read, Seek, SeekFrom, Write},
};

const MAGIC: &[u8; 8] = b"HMSTORE2";
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct Locator {
    key: RecordKey,
    offset: u64,
    len: u64,
    sha256: [u8; 32],
}
#[derive(Serialize, Deserialize)]
enum Frame {
    Shared {
        key: RecordKey,
        parents: Vec<Locator>,
        payload: Vec<u8>,
    },
    Snapshot {
        id: u64,
        roots: Vec<Locator>,
        payload: Vec<u8>,
    },
}
#[derive(Clone)]
struct Node {
    locator: Locator,
    parents: Vec<RecordKey>,
    fingerprint: [u8; 32],
    owners: u64,
}
struct Snapshot {
    range: StoredSnapshot,
    roots: Vec<RecordKey>,
    fingerprint: [u8; 32],
    listings: u64,
}
pub(super) struct StoreFrames {
    file: BufWriter<File>,
    pub(super) len: u64,
    pub(super) live_bytes: u64,
    snapshots: BTreeMap<u64, Snapshot>,
    records: BTreeMap<RecordKey, Node>,
    codec: Option<&'static str>,
    failed: bool,
}
pub(super) struct Stage {
    pub(super) index: Vec<StoredSnapshot>,
    snapshots: BTreeMap<u64, Snapshot>,
    records: BTreeMap<RecordKey, Node>,
    pub(super) new_bytes: u64,
    increments: BTreeMap<RecordKey, u64>,
    live_bytes: u64,
}
impl Stage {
    pub(super) fn new_snapshot_count(&self) -> usize {
        self.snapshots.len()
    }
}

struct Sink<'a> {
    file: &'a mut BufWriter<File>,
    len: &'a mut u64,
    live: &'a BTreeMap<RecordKey, Node>,
    pending: &'a mut BTreeMap<RecordKey, Node>,
}
fn append(
    file: &mut BufWriter<File>,
    len: &mut u64,
    frame: &Frame,
    key: RecordKey,
) -> CodecResult<Locator> {
    let bytes = postcard::to_allocvec(frame)?;
    let size = u64::try_from(bytes.len())?;
    let end = len
        .checked_add(size)
        .ok_or("snapshot store offset overflow")?;
    let locator = Locator {
        key,
        offset: *len,
        len: size,
        sha256: Sha256::digest(&bytes).into(),
    };
    file.write_all(&bytes)?;
    *len = end;
    Ok(locator)
}
impl Sink<'_> {
    fn node(&self, key: &RecordKey) -> Option<&Node> {
        self.pending.get(key).or_else(|| self.live.get(key))
    }
    fn roots(&self, keys: &[RecordKey]) -> CodecResult<Vec<Locator>> {
        let unique: BTreeSet<_> = keys.iter().collect();
        if unique.len() != keys.len() {
            return Err("snapshot has duplicate roots".into());
        }
        keys.iter()
            .map(|key| {
                self.node(key)
                    .map(|node| node.locator)
                    .ok_or_else(|| "snapshot root is missing".into())
            })
            .collect()
    }
}
impl SharedRecordSink for Sink<'_> {
    fn contains(&self, record: &SharedRecord) -> CodecResult<bool> {
        record.validate_dependencies()?;
        match self.node(&record.key) {
            Some(node) if node.fingerprint != record.fingerprint()? => {
                Err("snapshot shared key conflicts with live record".into())
            }
            Some(_) => Ok(true),
            None => Ok(false),
        }
    }
    fn insert(&mut self, record: SharedRecord) -> CodecResult<()> {
        if self.contains(&record)? {
            return Ok(());
        }
        let parents = self.roots(&record.dependencies)?;
        let fingerprint = record.fingerprint()?;
        let locator = append(
            self.file,
            self.len,
            &Frame::Shared {
                key: record.key,
                parents,
                payload: record.payload,
            },
            record.key,
        )?;
        self.pending.insert(
            record.key,
            Node {
                locator,
                parents: record.dependencies,
                fingerprint,
                owners: 0,
            },
        );
        Ok(())
    }
}
impl StoreFrames {
    pub(super) fn new(file: File) -> Self {
        Self {
            file: BufWriter::with_capacity(1 << 22, file),
            len: 0,
            live_bytes: 0,
            snapshots: BTreeMap::new(),
            records: BTreeMap::new(),
            codec: None,
            failed: false,
        }
    }
    pub(super) fn stage<'a, S: CheckpointSnapshot + 'a>(
        &mut self,
        snapshots: impl Iterator<Item = (u64, &'a S)>,
    ) -> CodecResult<Stage> {
        if self.failed {
            return Err("snapshot store writer failed before publication".into());
        }
        let result = self.stage_inner(snapshots);
        self.failed = result.is_err();
        result
    }
    fn stage_inner<'a, S: CheckpointSnapshot + 'a>(
        &mut self,
        snapshots: impl Iterator<Item = (u64, &'a S)>,
    ) -> CodecResult<Stage> {
        match self.codec {
            Some(codec) if codec != S::Codec::FORMAT => {
                return Err("snapshot store codec changed".into());
            }
            None => {
                self.file.write_all(MAGIC)?;
                write_frame(&mut self.file, &S::Codec::FORMAT)?;
                self.file.flush()?;
                self.len = self.file.stream_position()?;
                self.codec = Some(S::Codec::FORMAT);
            }
            _ => {}
        }
        let start = self.len;
        let mut stage = Stage {
            index: Vec::new(),
            snapshots: BTreeMap::new(),
            records: BTreeMap::new(),
            new_bytes: 0,
            increments: BTreeMap::new(),
            live_bytes: 0,
        };
        let mut ids = BTreeSet::new();
        for (id, snapshot) in snapshots {
            if !ids.insert(id) {
                return Err("duplicate snapshot store entry ID".into());
            }
            let mut sink = Sink {
                file: &mut self.file,
                len: &mut self.len,
                live: &self.records,
                pending: &mut stage.records,
            };
            let encoded = S::Codec::encode(snapshot, &mut sink)?;
            let roots = sink.roots(&encoded.roots)?;
            let fingerprint = Sha256::digest(postcard::to_allocvec(&encoded)?).into();
            let range = if let Some(stored) = self.snapshots.get(&id) {
                if stored.fingerprint != fingerprint {
                    return Err("snapshot store entry ID changed identity".into());
                }
                stored.range
            } else {
                let locator = append(
                    &mut self.file,
                    &mut self.len,
                    &Frame::Snapshot {
                        id,
                        roots,
                        payload: encoded.payload,
                    },
                    fingerprint,
                )?;
                let range = StoredSnapshot {
                    id,
                    offset: locator.offset,
                    len: locator.len,
                    sha256: locator.sha256,
                };
                stage.snapshots.insert(
                    id,
                    Snapshot {
                        range,
                        roots: encoded.roots,
                        fingerprint,
                        listings: 0,
                    },
                );
                range
            };
            stage.index.push(range);
        }
        let mut visit: Vec<_> = stage
            .snapshots
            .values()
            .flat_map(|snapshot| snapshot.roots.iter().copied())
            .collect();
        let mut seen = BTreeSet::new();
        while let Some(key) = visit.pop() {
            if let Some(node) = stage.records.get(&key)
                && seen.insert(key)
            {
                visit.extend_from_slice(&node.parents);
            }
        }
        if seen.len() != stage.records.len() {
            return Err("snapshot codec emitted unreferenced records".into());
        }
        stage.new_bytes = self
            .len
            .checked_sub(start)
            .ok_or("snapshot store length moved backwards")?;

        for key in stage
            .records
            .values()
            .flat_map(|node| node.parents.iter())
            .chain(
                stage
                    .snapshots
                    .values()
                    .flat_map(|snapshot| snapshot.roots.iter()),
            )
        {
            let count = stage.increments.entry(*key).or_default();
            *count = count.checked_add(1).ok_or("snapshot edge count overflow")?;
        }
        for (key, count) in &stage.increments {
            let node = stage
                .records
                .get(key)
                .or_else(|| self.records.get(key))
                .ok_or("snapshot edge is missing")?;
            node.owners
                .checked_add(*count)
                .ok_or("snapshot ownership overflow")?;
        }
        for range in &stage.index {
            self.snapshots
                .get(&range.id)
                .or_else(|| stage.snapshots.get(&range.id))
                .ok_or("snapshot listing is missing")?
                .listings
                .checked_add(1)
                .ok_or("snapshot listing overflow")?;
        }
        stage.live_bytes = self
            .live_bytes
            .checked_add(stage.new_bytes)
            .ok_or("snapshot live byte overflow")?;
        self.file.flush()?;
        self.file.get_ref().sync_data()?;
        Ok(stage)
    }
    pub(super) fn publish(&mut self, stage: Stage) {
        self.records.extend(stage.records);
        self.snapshots.extend(stage.snapshots);
        for (key, count) in stage.increments {
            self.records.get_mut(&key).expect("validated node").owners += count;
        }
        for range in stage.index {
            self.snapshots
                .get_mut(&range.id)
                .expect("validated snapshot")
                .listings += 1;
        }
        self.live_bytes = stage.live_bytes;
    }
    pub(super) fn release(&mut self, ids: &[u64]) -> CodecResult<(usize, u64)> {
        let mut freed = Vec::new();
        let mut roots = Vec::new();
        let mut snapshots = 0;
        for id in ids {
            let snapshot = self
                .snapshots
                .get_mut(id)
                .ok_or("pruned checkpoint snapshot is missing")?;
            snapshot.listings = snapshot
                .listings
                .checked_sub(1)
                .ok_or("snapshot listing underflow")?;
            if snapshot.listings == 0 {
                let snapshot = self.snapshots.remove(id).expect("live snapshot");
                freed.push((snapshot.range.offset, snapshot.range.len));
                roots.extend(snapshot.roots);
                snapshots += 1;
            }
        }
        while let Some(key) = roots.pop() {
            let node = self
                .records
                .get_mut(&key)
                .ok_or("released snapshot dependency is missing")?;
            node.owners = node
                .owners
                .checked_sub(1)
                .ok_or("snapshot owner underflow")?;
            if node.owners == 0 {
                let node = self.records.remove(&key).expect("live node");
                freed.push((node.locator.offset, node.locator.len));
                roots.extend(node.parents);
            }
        }
        freed.sort_unstable();
        let mut bytes = 0u64;
        let mut hole: Option<(u64, u64)> = None;
        for (offset, len) in freed {
            bytes = bytes
                .checked_add(len)
                .ok_or("freed snapshot byte overflow")?;
            hole = match hole {
                Some((start, size)) if start.checked_add(size) == Some(offset) => Some((
                    start,
                    size.checked_add(len).ok_or("snapshot hole overflow")?,
                )),
                Some((start, size)) => {
                    super::checkpoint::punch_hole(self.file.get_ref(), start, size)?;
                    Some((offset, len))
                }
                None => Some((offset, len)),
            };
        }
        if let Some((start, size)) = hole {
            super::checkpoint::punch_hole(self.file.get_ref(), start, size)?;
        }
        self.live_bytes = self
            .live_bytes
            .checked_sub(bytes)
            .ok_or("snapshot live byte underflow")?;
        Ok((snapshots, bytes))
    }
    #[cfg(test)]
    pub(super) fn snapshot_len(&self, id: u64) -> u64 {
        self.snapshots[&id].range.len
    }
    #[cfg(test)]
    pub(super) fn header_len(&self) -> u64 {
        8 + 8
            + postcard::to_allocvec(&self.codec.expect("store codec"))
                .unwrap()
                .len() as u64
            + 32
    }
    #[cfg(test)]
    pub(super) fn snapshot_ids(&self) -> Vec<u64> {
        self.snapshots.keys().copied().collect()
    }
}

fn load(
    file: &mut File,
    locator: Locator,
    min: u64,
    file_len: u64,
    before: u64,
) -> CodecResult<Frame> {
    let end = locator
        .offset
        .checked_add(locator.len)
        .ok_or("snapshot reference range overflow")?;
    if locator.offset < min || locator.len == 0 || end > file_len || end > before {
        return Err("snapshot reference range is invalid".into());
    }
    file.seek(SeekFrom::Start(locator.offset))?;
    let mut bytes = Vec::new();
    file.take(locator.len).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len())? != locator.len
        || <[u8; 32]>::from(Sha256::digest(&bytes)) != locator.sha256
    {
        return Err("snapshot reference checksum differs".into());
    }
    exact(&bytes)
}
pub(super) fn read<S: CheckpointSnapshot>(
    file: &mut File,
    index: &[StoredSnapshot],
) -> CodecResult<Vec<(u64, S)>> {
    file.seek(SeekFrom::Start(0))?;
    let mut magic = [0; 8];
    file.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err("snapshot store format is not recognized".into());
    }
    let codec: String = exact(&read_frame(file)?)?;
    if codec != S::Codec::FORMAT {
        return Err("snapshot store codec differs".into());
    }
    let min = file.stream_position()?;
    let file_len = file.metadata()?.len();
    let mut nodes = BTreeMap::<RecordKey, (Locator, SharedRecord)>::new();
    let mut endpoints = Vec::new();
    let mut ids = BTreeSet::new();
    let mut pending = Vec::new();
    let mut ranges = BTreeMap::<u64, (u64, [u8; 32])>::new();
    for stored in index {
        if !ids.insert(stored.id) {
            return Err("duplicate snapshot checkpoint ID".into());
        }
        let locator = Locator {
            key: [0; 32],
            offset: stored.offset,
            len: stored.len,
            sha256: stored.sha256,
        };
        match load(file, locator, min, file_len, file_len)? {
            Frame::Snapshot { id, roots, payload } if id == stored.id => {
                let mut keys = BTreeSet::new();
                for root in &roots {
                    if !keys.insert(root.key) {
                        return Err("snapshot has duplicate root references".into());
                    }
                    pending.push((*root, stored.offset));
                }
                endpoints.push((
                    id,
                    EncodedSnapshot {
                        roots: roots.iter().map(|root| root.key).collect(),
                        payload,
                    },
                ));
            }
            _ => return Err("snapshot reference has wrong type or entry ID".into()),
        }
        if ranges
            .insert(stored.offset, (stored.len, stored.sha256))
            .is_some()
        {
            return Err("snapshot frames overlap".into());
        }
    }
    while let Some((locator, before)) = pending.pop() {
        if locator.offset >= before
            || locator
                .offset
                .checked_add(locator.len)
                .is_none_or(|end| end > before)
        {
            return Err("snapshot parent must precede its child".into());
        }
        if let Some((old, _)) = nodes.get(&locator.key) {
            if *old != locator {
                return Err("snapshot semantic key has conflicting locators".into());
            }
            continue;
        }
        let frame = load(file, locator, min, file_len, before)?;
        let Frame::Shared {
            key,
            parents,
            payload,
        } = frame
        else {
            return Err("snapshot parent reference has wrong frame type".into());
        };
        if key != locator.key {
            return Err("snapshot reference semantic key differs".into());
        }
        let record = SharedRecord {
            key,
            dependencies: parents.iter().map(|parent| parent.key).collect(),
            payload,
        };
        record.validate_dependencies()?;
        if ranges
            .insert(locator.offset, (locator.len, locator.sha256))
            .is_some()
        {
            return Err("snapshot frames have conflicting physical locations".into());
        }
        for parent in parents {
            pending.push((parent, locator.offset));
        }
        nodes.insert(key, (locator, record));
    }
    let mut previous_end = min;
    for (offset, (len, _)) in ranges {
        if offset < previous_end {
            return Err("snapshot frame ranges overlap".into());
        }
        previous_end = offset
            .checked_add(len)
            .ok_or("snapshot frame range overflow")?;
    }
    let mut ordered: Vec<_> = nodes.into_values().collect();
    ordered.sort_unstable_by_key(|(locator, _)| locator.offset);
    let mut state = <S::Codec as SnapshotCodec<S>>::DecodeState::default();
    for (_, record) in ordered {
        S::Codec::decode_record(&mut state, record)?;
    }
    endpoints
        .into_iter()
        .map(|(id, encoded)| Ok((id, S::Codec::decode_snapshot(&state, encoded)?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    #[derive(Clone, Debug, Eq, PartialEq)]
    struct Graph {
        domain: u8,
        path: Vec<u8>,
    }
    struct GraphCodec;
    impl CheckpointSnapshot for Graph {
        type Codec = GraphCodec;
    }
    impl SnapshotCodec<Graph> for GraphCodec {
        const FORMAT: &'static str = "test-semantic-graph-v1";
        type DecodeState = BTreeMap<RecordKey, std::sync::Arc<(u8, Vec<u8>)>>;
        fn encode(
            snapshot: &Graph,
            sink: &mut dyn SharedRecordSink,
        ) -> CodecResult<EncodedSnapshot> {
            let mut rows = Vec::new();
            for depth in (0..=snapshot.path.len()).rev() {
                let payload = postcard::to_allocvec(&(snapshot.domain, &snapshot.path[..depth]))?;
                let key = Sha256::digest(&payload).into();
                let dependencies = if depth == 0 {
                    vec![]
                } else {
                    vec![
                        Sha256::digest(postcard::to_allocvec(&(
                            snapshot.domain,
                            &snapshot.path[..depth - 1],
                        ))?)
                        .into(),
                    ]
                };
                let row = SharedRecord {
                    key,
                    dependencies,
                    payload,
                };
                if sink.contains(&row)? {
                    break;
                }
                rows.push(row);
            }
            for row in rows.into_iter().rev() {
                sink.insert(row)?;
            }
            Ok(EncodedSnapshot {
                roots: vec![
                    Sha256::digest(postcard::to_allocvec(&(snapshot.domain, &snapshot.path))?)
                        .into(),
                ],
                payload: vec![],
            })
        }
        fn decode_record(state: &mut Self::DecodeState, row: SharedRecord) -> CodecResult<()> {
            let (domain, path): (u8, Vec<u8>) = exact(&row.payload)?;
            if row.key != <[u8; 32]>::from(Sha256::digest(&row.payload)) {
                return Err("test key differs".into());
            }
            let expected: Vec<RecordKey> = if path.is_empty() {
                vec![]
            } else {
                vec![
                    Sha256::digest(postcard::to_allocvec(&(domain, &path[..path.len() - 1]))?)
                        .into(),
                ]
            };
            if row.dependencies != expected || expected.iter().any(|key| !state.contains_key(key)) {
                return Err("test parent differs".into());
            }
            state.insert(row.key, std::sync::Arc::new((domain, path)));
            Ok(())
        }
        fn decode_snapshot(
            state: &Self::DecodeState,
            snapshot: EncodedSnapshot,
        ) -> CodecResult<Graph> {
            let [key] = snapshot.roots.as_slice() else {
                return Err("test root count".into());
            };
            let root = state.get(key).ok_or("test root missing")?;
            Ok(Graph {
                domain: root.0,
                path: root.1.clone(),
            })
        }
    }
    fn path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("checkpoint-dag-{name}-{}", std::process::id()))
    }
    fn store(name: &str) -> (PathBuf, StoreFrames) {
        let path = path(name);
        let file = File::create(&path).unwrap();
        (path, StoreFrames::new(file))
    }
    fn append_snapshot(store: &mut StoreFrames, id: u64, snapshot: &Graph) -> Vec<StoredSnapshot> {
        let stage = store.stage(std::iter::once((id, snapshot))).unwrap();
        let index = stage.index.clone();
        store.publish(stage);
        index
    }
    #[test]
    fn shared_store_counts_edges_once_preserves_ancestors_and_reemits_reclaimed_roots() {
        let (path, mut store) = store("ownership");
        let a = Graph {
            domain: 1,
            path: vec![1, 2, 3],
        };
        let b = Graph {
            domain: 1,
            path: vec![1, 2, 4],
        };
        let root = Graph {
            domain: 1,
            path: vec![],
        };
        let first = append_snapshot(&mut store, 1, &a);
        assert_eq!(store.records.len(), 4);
        let repeated = append_snapshot(&mut store, 1, &a);
        assert_eq!(first, repeated);
        assert_eq!(store.records.len(), 4);
        assert_eq!(store.snapshots[&1].listings, 2);
        let sibling = append_snapshot(&mut store, 2, &b);
        assert_eq!(store.records.len(), 5);
        let genesis = append_snapshot(&mut store, 3, &root);
        assert_eq!(store.records.len(), 5);
        store.release(&[1]).unwrap();
        assert_eq!(store.records.len(), 5);
        store.release(&[1]).unwrap();
        assert_eq!(store.records.len(), 4);
        assert_eq!(
            read::<Graph>(&mut File::open(&path).unwrap(), &sibling).unwrap(),
            vec![(2, b.clone())]
        );
        store.release(&[2]).unwrap();
        assert_eq!(store.records.len(), 1);
        assert_eq!(
            read::<Graph>(&mut File::open(&path).unwrap(), &genesis).unwrap(),
            vec![(3, root)]
        );
        store.release(&[3]).unwrap();
        assert!(store.records.is_empty());
        assert_eq!(store.live_bytes, 0);
        let revived = append_snapshot(&mut store, 1, &a);
        assert_eq!(store.records.len(), 4);
        assert!(revived[0].offset > first[0].offset);
        assert_eq!(
            read::<Graph>(&mut File::open(&path).unwrap(), &revived).unwrap(),
            vec![(1, a)]
        );
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn failed_staging_preserves_published_listing_and_dependency_ownership() {
        let (path, mut store) = store("failed-stage");
        let snapshot = Graph {
            domain: 2,
            path: vec![1, 2],
        };
        let index = append_snapshot(&mut store, 4, &snapshot);
        let owners: Vec<_> = store
            .records
            .iter()
            .map(|(key, node)| (*key, node.owners))
            .collect();
        let bytes = store.live_bytes;
        let (other_path, mut other) = self::store("changed-identity");
        let other_index = append_snapshot(&mut other, 4, &snapshot);
        assert!(
            other
                .stage(std::iter::once((
                    4,
                    &Graph {
                        domain: 2,
                        path: vec![9]
                    }
                )))
                .err()
                .unwrap()
                .to_string()
                .contains("changed identity")
        );
        assert_eq!(other.snapshots[&4].listings, 1);
        assert_eq!(
            read::<Graph>(&mut File::open(&other_path).unwrap(), &other_index).unwrap(),
            vec![(4, snapshot.clone())]
        );
        std::fs::remove_file(other_path).unwrap();
        assert!(
            store
                .stage([(5, &snapshot), (5, &snapshot)].into_iter())
                .is_err()
        );
        assert_eq!(store.snapshot_ids(), vec![4]);
        assert_eq!(store.snapshots[&4].listings, 1);
        assert_eq!(store.live_bytes, bytes);
        assert_eq!(
            owners,
            store
                .records
                .iter()
                .map(|(key, node)| (*key, node.owners))
                .collect::<Vec<_>>()
        );
        assert!(
            store
                .stage(std::iter::once((
                    4,
                    &Graph {
                        domain: 2,
                        path: vec![9]
                    }
                )))
                .is_err()
        );
        assert_eq!(
            read::<Graph>(&mut File::open(&path).unwrap(), &index).unwrap(),
            vec![(4, snapshot)]
        );
        store.release(&[4]).unwrap();
        assert!(store.records.is_empty());
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn persistent_frames_reject_bad_ranges_checksum_entry_type_parent_and_key() {
        let (path, mut store) = store("corruption");
        let snapshot = Graph {
            domain: 3,
            path: vec![1, 2],
        };
        let original = append_snapshot(&mut store, 7, &snapshot);
        let source = std::fs::read(&path).unwrap();
        for changed in [
            StoredSnapshot {
                offset: 0,
                ..original[0]
            },
            StoredSnapshot {
                len: 0,
                ..original[0]
            },
            StoredSnapshot {
                offset: u64::MAX,
                ..original[0]
            },
            StoredSnapshot {
                len: u64::MAX,
                ..original[0]
            },
            StoredSnapshot {
                id: 8,
                ..original[0]
            },
            StoredSnapshot {
                sha256: [0; 32],
                ..original[0]
            },
        ] {
            assert!(read::<Graph>(&mut File::open(&path).unwrap(), &[changed]).is_err());
        }
        let leaf = *store
            .records
            .values()
            .max_by_key(|node| node.locator.offset)
            .map(|node| &node.locator)
            .unwrap();
        let wrong_type = StoredSnapshot {
            id: 7,
            offset: leaf.offset,
            len: leaf.len,
            sha256: leaf.sha256,
        };
        assert!(read::<Graph>(&mut File::open(&path).unwrap(), &[wrong_type]).is_err());
        for mutation in 0..6 {
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap();
            file.seek(SeekFrom::End(0)).unwrap();
            let mut root = leaf;
            match mutation {
                0 => root.key = [8; 32],
                1 => root.offset = u64::MAX,
                2 => root.len = u64::MAX,
                3 => root.sha256 = [8; 32],
                4 => {
                    root.offset = file.metadata().unwrap().len();
                }
                _ => {}
            }
            let roots = if mutation == 5 {
                vec![
                    root,
                    Locator {
                        offset: root.offset - 1,
                        ..root
                    },
                ]
            } else {
                vec![root]
            };
            let frame = Frame::Snapshot {
                id: 7,
                roots,
                payload: vec![],
            };
            let bytes = postcard::to_allocvec(&frame).unwrap();
            let offset = file.stream_position().unwrap();
            file.write_all(&bytes).unwrap();
            file.flush().unwrap();
            let forged = StoredSnapshot {
                id: 7,
                offset,
                len: bytes.len() as u64,
                sha256: Sha256::digest(&bytes).into(),
            };
            assert!(
                read::<Graph>(&mut file, &[forged]).is_err(),
                "mutation {mutation}"
            );
            std::fs::write(&path, &source).unwrap();
        }
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn one_checkpoint_cannot_bind_a_semantic_key_to_two_physical_frames() {
        let (path, mut store) = store("conflicting-locators");
        let original = append_snapshot(
            &mut store,
            1,
            &Graph {
                domain: 4,
                path: vec![1, 2],
            },
        );
        let leaf = store
            .records
            .values()
            .max_by_key(|node| node.locator.offset)
            .unwrap()
            .locator;
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let frame = load(&mut file, leaf, store.header_len(), store.len, store.len).unwrap();
        let mut out = BufWriter::new(file);
        out.seek(SeekFrom::End(0)).unwrap();
        let mut len = out.stream_position().unwrap();
        let copied = append(&mut out, &mut len, &frame, leaf.key).unwrap();
        assert_ne!(leaf.offset, copied.offset);
        let mut index = Vec::new();
        for (id, root) in [(8, leaf), (9, copied)] {
            let locator = append(
                &mut out,
                &mut len,
                &Frame::Snapshot {
                    id,
                    roots: vec![root],
                    payload: vec![],
                },
                [0; 32],
            )
            .unwrap();
            index.push(StoredSnapshot {
                id,
                offset: locator.offset,
                len: locator.len,
                sha256: locator.sha256,
            });
        }
        out.flush().unwrap();
        assert!(
            read::<Graph>(&mut File::open(&path).unwrap(), &index)
                .unwrap_err()
                .to_string()
                .contains("conflicting locators")
        );
        assert_eq!(
            read::<Graph>(&mut File::open(&path).unwrap(), &original).unwrap(),
            vec![(
                1,
                Graph {
                    domain: 4,
                    path: vec![1, 2]
                }
            )]
        );
        assert!(
            store
                .stage(std::iter::once((8, &0u8)))
                .err()
                .unwrap()
                .to_string()
                .contains("codec changed")
        );
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn ownership_and_listing_overflow_fail_during_staging_before_publication() {
        let (path, mut store) = store("listing-overflow");
        let snapshot = Graph {
            domain: 5,
            path: vec![1],
        };
        let index = append_snapshot(&mut store, 1, &snapshot);
        store.snapshots.get_mut(&1).unwrap().listings = u64::MAX;
        assert!(
            store
                .stage(std::iter::once((1, &snapshot)))
                .err()
                .unwrap()
                .to_string()
                .contains("listing overflow")
        );
        assert_eq!(store.snapshots[&1].listings, u64::MAX);
        assert_eq!(
            read::<Graph>(&mut File::open(&path).unwrap(), &index).unwrap(),
            vec![(1, snapshot.clone())]
        );
        std::fs::remove_file(path).unwrap();
        let (path, mut store) = self::store("owner-overflow");
        let index = append_snapshot(&mut store, 1, &snapshot);
        let root = store.snapshots[&1].roots[0];
        store.records.get_mut(&root).unwrap().owners = u64::MAX;
        assert!(
            store
                .stage(std::iter::once((
                    2,
                    &Graph {
                        domain: 5,
                        path: vec![1, 2]
                    }
                )))
                .err()
                .unwrap()
                .to_string()
                .contains("ownership overflow")
        );
        assert_eq!(store.records[&root].owners, u64::MAX);
        assert_eq!(store.snapshot_ids(), vec![1]);
        assert_eq!(
            read::<Graph>(&mut File::open(&path).unwrap(), &index).unwrap(),
            vec![(1, snapshot)]
        );
        std::fs::remove_file(path).unwrap();
    }
}
