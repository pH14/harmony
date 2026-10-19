// SPDX-License-Identifier: AGPL-3.0-or-later

use super::campaign::{SnapshotCheckpoint, SnapshotCheckpointEntry};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    io::{Read, Write},
    marker::PhantomData,
};

pub type RecordKey = [u8; 32];
pub type CodecResult<T> = Result<T, Box<dyn Error>>;

pub trait CheckpointSnapshot: Sized + Clone {
    type Codec: SnapshotCodec<Self>;
    fn checkpoint_image_charge(
        &self,
        budget: &mut super::checkpoint::image::ImageBudget,
    ) -> CodecResult<()>;
    fn freeze_checkpoint(&self) -> CodecResult<Self>;
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SharedRecord {
    pub key: RecordKey,
    pub dependencies: Vec<RecordKey>,
    pub payload: Vec<u8>,
}

impl SharedRecord {
    pub fn fingerprint(&self) -> CodecResult<[u8; 32]> {
        Ok(Sha256::digest(postcard::to_allocvec(self)?).into())
    }
    pub fn validate_dependencies(&self) -> CodecResult<()> {
        let unique: BTreeSet<_> = self.dependencies.iter().copied().collect();
        if unique.len() != self.dependencies.len() || unique.contains(&self.key) {
            return Err("snapshot record has duplicate or self dependencies".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EncodedSnapshot {
    pub roots: Vec<RecordKey>,
    pub payload: Vec<u8>,
}

pub trait SharedRecordSink {
    fn contains(&self, record: &SharedRecord) -> CodecResult<bool>;
    fn insert(&mut self, record: SharedRecord) -> CodecResult<()>;
}

pub trait SnapshotCodec<S> {
    const FORMAT: &'static str;
    type DecodeState: Default;
    fn encode(snapshot: &S, sink: &mut dyn SharedRecordSink) -> CodecResult<EncodedSnapshot>;
    fn decode_record(state: &mut Self::DecodeState, record: SharedRecord) -> CodecResult<()>;
    fn decode_snapshot(state: &Self::DecodeState, snapshot: EncodedSnapshot) -> CodecResult<S>;
}

pub struct PostcardSnapshotCodec<S>(PhantomData<S>);
impl<S: Serialize + DeserializeOwned> SnapshotCodec<S> for PostcardSnapshotCodec<S> {
    const FORMAT: &'static str = "postcard-snapshot-v1";
    type DecodeState = ();
    fn encode(snapshot: &S, _: &mut dyn SharedRecordSink) -> CodecResult<EncodedSnapshot> {
        Ok(EncodedSnapshot {
            roots: Vec::new(),
            payload: postcard::to_allocvec(snapshot)?,
        })
    }
    fn decode_record(_: &mut (), _: SharedRecord) -> CodecResult<()> {
        Err("plain snapshot codec cannot decode shared records".into())
    }
    fn decode_snapshot(_: &(), snapshot: EncodedSnapshot) -> CodecResult<S> {
        if !snapshot.roots.is_empty() {
            return Err("plain snapshot has shared roots".into());
        }
        exact(&snapshot.payload)
    }
}

impl CheckpointSnapshot for () {
    type Codec = PostcardSnapshotCodec<Self>;
    fn checkpoint_image_charge(
        &self,
        budget: &mut super::checkpoint::image::ImageBudget,
    ) -> CodecResult<()> {
        budget.reserve(std::mem::size_of::<Self>())
    }
    fn freeze_checkpoint(&self) -> CodecResult<Self> {
        Ok(())
    }
}

impl CheckpointSnapshot for u8 {
    type Codec = PostcardSnapshotCodec<Self>;
    fn checkpoint_image_charge(
        &self,
        budget: &mut super::checkpoint::image::ImageBudget,
    ) -> CodecResult<()> {
        budget.reserve(std::mem::size_of::<Self>())
    }
    fn freeze_checkpoint(&self) -> CodecResult<Self> {
        Ok(*self)
    }
}
impl CheckpointSnapshot for Vec<u8> {
    type Codec = PostcardSnapshotCodec<Self>;
    fn checkpoint_image_charge(
        &self,
        budget: &mut super::checkpoint::image::ImageBudget,
    ) -> CodecResult<()> {
        budget.reserve(std::mem::size_of::<Self>())?;
        budget.array::<u8>(self.capacity())
    }
    fn freeze_checkpoint(&self) -> CodecResult<Self> {
        Ok(self.clone())
    }
}

pub(crate) fn exact<T: DeserializeOwned>(bytes: &[u8]) -> CodecResult<T> {
    let (value, tail) = postcard::take_from_bytes(bytes)?;
    if !tail.is_empty() {
        return Err("snapshot frame has trailing bytes".into());
    }
    Ok(value)
}

const MAGIC: &[u8; 8] = b"HMSNAP02";
#[derive(Serialize, Deserialize)]
struct Header {
    format: String,
    codec: String,
    entries: u64,
}
#[derive(Serialize, Deserialize)]
enum Frame {
    Shared(SharedRecord),
    Snapshot(u64, EncodedSnapshot),
}

pub(crate) fn write_frame(out: &mut dyn Write, value: &impl Serialize) -> CodecResult<()> {
    let bytes = postcard::to_allocvec(value)?;
    out.write_all(&u64::try_from(bytes.len())?.to_le_bytes())?;
    out.write_all(&bytes)?;
    out.write_all(&Sha256::digest(&bytes))?;
    Ok(())
}

pub(crate) fn read_frame(input: &mut dyn Read) -> CodecResult<Vec<u8>> {
    let mut len = [0; 8];
    input.read_exact(&mut len)?;
    let len = u64::from_le_bytes(len);
    let mut bytes = Vec::new();
    input.take(len).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len())? != len {
        return Err("truncated snapshot frame".into());
    }
    let mut checksum = [0; 32];
    input.read_exact(&mut checksum)?;
    if checksum != <[u8; 32]>::from(Sha256::digest(&bytes)) {
        return Err("snapshot frame checksum differs".into());
    }
    Ok(bytes)
}

struct EnvelopeSink<'a> {
    out: &'a mut dyn Write,
    records: BTreeMap<RecordKey, ([u8; 32], Vec<RecordKey>)>,
}
impl SharedRecordSink for EnvelopeSink<'_> {
    fn contains(&self, record: &SharedRecord) -> CodecResult<bool> {
        record.validate_dependencies()?;
        match self.records.get(&record.key) {
            Some((fingerprint, _)) if *fingerprint != record.fingerprint()? => {
                Err("snapshot shared key conflicts".into())
            }
            Some(_) => Ok(true),
            None => Ok(false),
        }
    }
    fn insert(&mut self, record: SharedRecord) -> CodecResult<()> {
        if self.contains(&record)? {
            return Ok(());
        }
        if record
            .dependencies
            .iter()
            .any(|key| !self.records.contains_key(key))
        {
            return Err("snapshot dependency must precede its child".into());
        }
        let key = record.key;
        let fingerprint = record.fingerprint()?;
        let dependencies = record.dependencies.clone();
        write_frame(self.out, &Frame::Shared(record))?;
        self.records.insert(key, (fingerprint, dependencies));
        Ok(())
    }
}

pub fn write_checkpoint<S: CheckpointSnapshot>(
    checkpoint: &SnapshotCheckpoint<S>,
    out: &mut dyn Write,
) -> CodecResult<()> {
    out.write_all(MAGIC)?;
    write_frame(
        out,
        &Header {
            format: checkpoint.format.clone(),
            codec: S::Codec::FORMAT.into(),
            entries: u64::try_from(checkpoint.entries.len())?,
        },
    )?;
    let mut sink = EnvelopeSink {
        out,
        records: BTreeMap::new(),
    };
    let mut ids = BTreeSet::new();
    let mut roots = Vec::new();
    for entry in &checkpoint.entries {
        if !ids.insert(entry.id) {
            return Err("duplicate snapshot checkpoint entry ID".into());
        }
        let encoded = S::Codec::encode(&entry.snapshot, &mut sink)?;
        if encoded.roots.iter().collect::<BTreeSet<_>>().len() != encoded.roots.len() {
            return Err("snapshot has duplicate roots".into());
        }
        if encoded
            .roots
            .iter()
            .any(|key| !sink.records.contains_key(key))
        {
            return Err("snapshot root is not present".into());
        }
        roots.extend_from_slice(&encoded.roots);
        write_frame(sink.out, &Frame::Snapshot(entry.id, encoded))?;
    }
    let mut seen = BTreeSet::new();
    while let Some(key) = roots.pop() {
        if seen.insert(key) {
            roots.extend_from_slice(&sink.records.get(&key).ok_or("snapshot root is missing")?.1);
        }
    }
    if seen.len() != sink.records.len() {
        return Err("snapshot codec emitted unreferenced records".into());
    }
    write_frame(sink.out, &"snapshot-checkpoint-end-v2")?;
    Ok(())
}

pub fn read_checkpoint<S: CheckpointSnapshot>(
    input: &mut dyn Read,
    expected_format: &str,
) -> CodecResult<SnapshotCheckpoint<S>> {
    let mut magic = [0; 8];
    input.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err("snapshot checkpoint format is not recognized".into());
    }
    let header: Header = exact(&read_frame(input)?)?;
    if header.format != expected_format || header.codec != S::Codec::FORMAT {
        return Err("snapshot checkpoint format is not recognized".into());
    }
    let mut state = <S::Codec as SnapshotCodec<S>>::DecodeState::default();
    let mut dependencies = BTreeMap::<RecordKey, Vec<RecordKey>>::new();
    let mut roots = Vec::new();
    let mut entries = Vec::new();
    let mut ids = BTreeSet::new();
    while u64::try_from(entries.len())? < header.entries {
        match exact(&read_frame(input)?)? {
            Frame::Shared(record) => {
                record.validate_dependencies()?;
                if dependencies.contains_key(&record.key)
                    || record
                        .dependencies
                        .iter()
                        .any(|key| !dependencies.contains_key(key))
                {
                    return Err(
                        "snapshot shared record is duplicate or has a forward dependency".into(),
                    );
                }
                dependencies.insert(record.key, record.dependencies.clone());
                S::Codec::decode_record(&mut state, record)?;
            }
            Frame::Snapshot(id, encoded) => {
                if !ids.insert(id)
                    || encoded.roots.iter().collect::<BTreeSet<_>>().len() != encoded.roots.len()
                    || encoded
                        .roots
                        .iter()
                        .any(|key| !dependencies.contains_key(key))
                {
                    return Err("snapshot entry has duplicate ID or unknown root".into());
                }
                roots.extend_from_slice(&encoded.roots);
                entries.push(SnapshotCheckpointEntry {
                    id,
                    snapshot: S::Codec::decode_snapshot(&state, encoded)?,
                });
            }
        }
    }
    let end: String = exact(&read_frame(input)?)?;
    if end != "snapshot-checkpoint-end-v2" {
        return Err("snapshot checkpoint terminator differs".into());
    }
    let mut tail = [0];
    if input.read(&mut tail)? != 0 {
        return Err("snapshot checkpoint has trailing bytes".into());
    }
    let mut seen = BTreeSet::new();
    while let Some(key) = roots.pop() {
        if seen.insert(key) {
            roots.extend_from_slice(dependencies.get(&key).ok_or("snapshot root is absent")?);
        }
    }
    if seen.len() != dependencies.len() {
        return Err("snapshot checkpoint has unreferenced shared records".into());
    }
    Ok(SnapshotCheckpoint {
        format: header.format,
        entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn checkpoint() -> SnapshotCheckpoint<Vec<u8>> {
        SnapshotCheckpoint {
            format: "test".into(),
            entries: vec![SnapshotCheckpointEntry {
                id: 7,
                snapshot: vec![1, 2, 3],
            }],
        }
    }
    #[test]
    fn framed_plain_snapshots_are_exact_and_reject_older_formats() {
        let current = checkpoint();
        let bytes = current.to_bytes().unwrap();
        assert_eq!(
            SnapshotCheckpoint::<Vec<u8>>::from_bytes(&bytes, "test").unwrap(),
            current
        );
        for cut in 0..bytes.len() {
            assert!(
                SnapshotCheckpoint::<Vec<u8>>::from_bytes(&bytes[..cut], "test").is_err(),
                "cut {cut}"
            );
        }
        let mut changed = bytes.clone();
        changed.push(0);
        assert!(SnapshotCheckpoint::<Vec<u8>>::from_bytes(&changed, "test").is_err());
        let old = postcard::to_allocvec(&("test", vec![(7u64, vec![1u8, 2, 3])])).unwrap();
        assert!(SnapshotCheckpoint::<Vec<u8>>::from_bytes(&old, "test").is_err());
        assert!(SnapshotCheckpoint::<Vec<u8>>::from_bytes(&bytes, "other").is_err());
        for index in [8, 16, bytes.len() - 1] {
            let mut changed = bytes.clone();
            changed[index] ^= 1;
            assert!(SnapshotCheckpoint::<Vec<u8>>::from_bytes(&changed, "test").is_err());
        }
        let mut impossible = Vec::from(MAGIC.as_slice());
        impossible.extend_from_slice(&u64::MAX.to_le_bytes());
        assert!(SnapshotCheckpoint::<Vec<u8>>::from_bytes(&impossible, "test").is_err());
    }
    #[test]
    fn live_shared_keys_check_payload_and_dependency_fingerprints() {
        let mut bytes = Vec::new();
        let mut sink = EnvelopeSink {
            out: &mut bytes,
            records: BTreeMap::new(),
        };
        let root = SharedRecord {
            key: [1; 32],
            dependencies: vec![],
            payload: vec![3],
        };
        sink.insert(root.clone()).unwrap();
        assert!(sink.contains(&root).unwrap());
        let mut changed = root.clone();
        changed.payload.push(4);
        assert!(sink.contains(&changed).is_err());
        changed = root.clone();
        changed.dependencies.push([2; 32]);
        assert!(sink.contains(&changed).is_err());
        let child = SharedRecord {
            key: [2; 32],
            dependencies: vec![[1; 32]],
            payload: vec![4],
        };
        sink.insert(child).unwrap();
        for deps in [vec![[3; 32]], vec![[1; 32], [1; 32]], vec![[9; 32]]] {
            assert!(
                sink.insert(SharedRecord {
                    key: [3; 32],
                    dependencies: deps,
                    payload: vec![]
                })
                .is_err()
            );
        }
    }
    #[test]
    fn reader_refuses_duplicate_entries_wrong_codec_and_plain_shared_records() {
        for frames in [
            vec![Frame::Snapshot(
                7,
                EncodedSnapshot {
                    roots: vec![[1; 32]],
                    payload: vec![0],
                },
            )],
            vec![Frame::Shared(SharedRecord {
                key: [1; 32],
                dependencies: vec![],
                payload: vec![0],
            })],
            vec![
                Frame::Snapshot(
                    7,
                    EncodedSnapshot {
                        roots: vec![],
                        payload: vec![0],
                    },
                ),
                Frame::Snapshot(
                    7,
                    EncodedSnapshot {
                        roots: vec![],
                        payload: vec![1],
                    },
                ),
            ],
        ] {
            let mut bytes = Vec::from(MAGIC.as_slice());
            write_frame(
                &mut bytes,
                &Header {
                    format: "test".into(),
                    codec: <PostcardSnapshotCodec<u8> as SnapshotCodec<u8>>::FORMAT.into(),
                    entries: 2,
                },
            )
            .unwrap();
            for frame in frames {
                write_frame(&mut bytes, &frame).unwrap();
            }
            write_frame(&mut bytes, &"snapshot-checkpoint-end-v2").unwrap();
            assert!(SnapshotCheckpoint::<u8>::from_bytes(&bytes, "test").is_err());
        }
        let mut duplicate = checkpoint();
        duplicate.entries.push(duplicate.entries[0].clone());
        assert!(duplicate.to_bytes().is_err());
        let mut bytes = Vec::from(MAGIC.as_slice());
        write_frame(
            &mut bytes,
            &Header {
                format: "test".into(),
                codec: "other-codec".into(),
                entries: 0,
            },
        )
        .unwrap();
        assert!(SnapshotCheckpoint::<u8>::from_bytes(&bytes, "test").is_err());
    }
}
