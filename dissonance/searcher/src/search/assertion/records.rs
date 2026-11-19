// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    error::Error,
    fs::File,
    io::{BufWriter, Write},
    path::Path,
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::catalog::{ACTION_END_SLOT, Catalog, RECORD_SLOTS, Record};
use crate::search::choice::ChoiceRecord;

pub const RECORDS_FORMAT: &str = "assertion-records-v7";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StreamIdentity {
    pub campaign_seed: u64,
    pub workload_identity_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecordsHeader {
    pub format: String,
    pub stream: StreamIdentity,
    pub policy: String,
    pub strategy: String,
    pub catalog: Catalog,
    pub memory_windows: Vec<(u64, u32)>,
    pub genesis: Record,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordedAction<A> {
    pub sequence: u64,
    pub action: A,
    pub failed: bool,
    pub cost: u64,
    pub frames: Vec<Record>,
    pub choices: Vec<ChoiceRecord>,
}

#[derive(Deserialize, Serialize)]
struct EncodedJob<A> {
    sequence: u64,
    actions: Vec<Encoded<A>>,
}

#[derive(Deserialize, Serialize)]
struct Encoded<A> {
    action: A,
    failed: bool,
    cost: u64,
    width: u8,
    changes: Vec<Vec<(u8, i64)>>,
    choices: Vec<ChoiceRecord>,
}

impl<A: Clone> Encoded<A> {
    fn new(recorded: &RecordedAction<A>) -> Self {
        let width = recorded
            .frames
            .first()
            .map_or(0, |record| record.values().len());
        let mut previous = [0_i64; RECORD_SLOTS];
        let changes = recorded
            .frames
            .iter()
            .map(|record| {
                let values = record.values();
                let mut changed: Vec<(u8, i64)> = values
                    .iter()
                    .enumerate()
                    .filter(|(slot, value)| previous[*slot] != **value)
                    .map(|(slot, value)| (u8::try_from(slot).unwrap_or(u8::MAX), *value))
                    .collect();
                previous[..values.len()].copy_from_slice(values);
                let action_end = record.slots()[ACTION_END_SLOT];
                if previous[ACTION_END_SLOT] != action_end {
                    changed.push((u8::try_from(ACTION_END_SLOT).unwrap_or(u8::MAX), action_end));
                    previous[ACTION_END_SLOT] = action_end;
                }
                changed
            })
            .collect();
        Self {
            action: recorded.action.clone(),
            failed: recorded.failed,
            cost: recorded.cost,
            width: u8::try_from(width).unwrap_or(u8::MAX),
            changes,
            choices: recorded.choices.clone(),
        }
    }
}

impl<A> Encoded<A> {
    fn decode(self, sequence: u64) -> Result<RecordedAction<A>, Box<dyn Error>> {
        let width = usize::from(self.width);
        if width > ACTION_END_SLOT {
            return Err("a recorded action is wider than the record".into());
        }
        let mut values = [0_i64; RECORD_SLOTS];
        let mut frames = Vec::with_capacity(self.changes.len());
        for changed in self.changes {
            for (slot, value) in changed {
                let slot = usize::from(slot);
                *values
                    .get_mut(slot)
                    .filter(|_| slot < width || slot == ACTION_END_SLOT)
                    .ok_or("a recorded change names a slot outside the record")? = value;
            }
            let mut frame =
                Record::from_values(&values[..width]).ok_or("record exceeds the slot limit")?;
            frame.set_action_end(values[ACTION_END_SLOT] != 0);
            frames.push(frame);
        }
        Ok(RecordedAction {
            sequence,
            action: self.action,
            failed: self.failed,
            cost: self.cost,
            frames,
            choices: self.choices,
        })
    }
}

pub struct RecordsWriter<A: Clone + Serialize> {
    out: BufWriter<File>,
    job: Option<EncodedJob<A>>,
    jobs: u64,
    bytes: u64,
}

impl<A: Clone + Serialize> RecordsWriter<A> {
    pub fn create(path: &Path, header: &RecordsHeader) -> Result<Self, Box<dyn Error>> {
        let mut out = BufWriter::new(File::create(path)?);
        let bytes = postcard::to_allocvec(header)?;
        out.write_all(&bytes)?;
        out.flush()?;
        Ok(Self {
            out,
            job: None,
            jobs: 0,
            bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        })
    }

    pub fn push(&mut self, recorded: &RecordedAction<A>) -> Result<(), Box<dyn Error>> {
        if self
            .job
            .as_ref()
            .is_some_and(|job| job.sequence != recorded.sequence)
        {
            self.write_job()?;
        }
        self.job
            .get_or_insert_with(|| EncodedJob {
                sequence: recorded.sequence,
                actions: Vec::new(),
            })
            .actions
            .push(Encoded::new(recorded));
        Ok(())
    }

    fn write_job(&mut self) -> Result<(), Box<dyn Error>> {
        if let Some(job) = self.job.take() {
            let bytes = postcard::to_allocvec(&job)?;
            self.out.write_all(&bytes)?;
            self.out.flush()?;
            self.jobs += 1;
            self.bytes = self
                .bytes
                .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<RecordsSize, Box<dyn Error>> {
        self.write_job()?;
        Ok(RecordsSize {
            jobs: self.jobs,
            bytes: self.bytes,
        })
    }
}

impl<A: Clone + Serialize> Drop for RecordsWriter<A> {
    fn drop(&mut self) {
        let _ = self.write_job();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct RecordsSize {
    pub jobs: u64,
    pub bytes: u64,
}

pub struct Recording<A> {
    pub header: RecordsHeader,
    pub actions: Vec<RecordedAction<A>>,
    pub jobs: usize,
    pub truncated_bytes: usize,
}

impl<A: DeserializeOwned> Recording<A> {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Box<dyn Error>> {
        let (format, _): (String, _) = postcard::take_from_bytes(bytes)?;
        if format != RECORDS_FORMAT {
            return Err(format!("records format {format} is not {RECORDS_FORMAT}").into());
        }
        let (header, mut rest): (RecordsHeader, _) = postcard::take_from_bytes(bytes)?;
        let mut actions = Vec::new();
        let mut jobs = 0;
        let mut last = None;
        while !rest.is_empty() {
            let Ok((job, tail)) = postcard::take_from_bytes::<EncodedJob<A>>(rest) else {
                break;
            };
            if last.is_some_and(|last| job.sequence <= last) {
                return Err(format!(
                    "records job {} does not follow job {}",
                    job.sequence,
                    last.unwrap_or_default()
                )
                .into());
            }
            last = Some(job.sequence);
            rest = tail;
            jobs += 1;
            for action in job.actions {
                actions.push(action.decode(job.sequence)?);
            }
        }
        Ok(Self {
            header,
            actions,
            jobs,
            truncated_bytes: rest.len(),
        })
    }
}

impl<A> Recording<A> {
    pub fn continue_with(mut self, later: Self) -> Result<Self, Box<dyn Error>> {
        if later.header.policy != self.header.policy || later.header.catalog != self.header.catalog
        {
            return Err("a continued recording has another policy or catalog".into());
        }
        let first = later
            .actions
            .first()
            .map_or(u64::MAX, |action| action.sequence);
        let dropped: std::collections::BTreeSet<u64> = self
            .actions
            .iter()
            .filter(|action| action.sequence >= first)
            .map(|action| action.sequence)
            .collect();
        self.actions.retain(|action| action.sequence < first);
        self.jobs = self.jobs.saturating_sub(dropped.len()) + later.jobs;
        self.actions.extend(later.actions);
        self.truncated_bytes = later.truncated_bytes;
        Ok(self)
    }

    pub fn check_stream(
        &self,
        stream: &StreamIdentity,
        policy: &str,
    ) -> Result<(), Box<dyn Error>> {
        if &self.header.stream != stream {
            return Err("the records belong to another run's stream".into());
        }
        if self.header.policy != policy {
            return Err(
                "the records were written under another key policy than the stream's".into(),
            );
        }
        Ok(())
    }
}
