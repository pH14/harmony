// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{collections::BTreeMap, error::Error, num::NonZeroUsize, rc::Rc};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::search::{
    empirical_steps::{
        EmpiricalStepCheckpoint, EmpiricalStepParameters, EmpiricalStepTableRef,
        EmpiricalStepTables,
    },
    rand::RomuDuoJrRand,
};

pub const DRAW_TABLE_POLICY_FIELD: &str = "draw_table_policy";

const DRAW_TABLE_PREFIX: &str = "retained_suffix_table_v1:";

pub const DEFAULT_DRAW_TABLE_PARAMETERS: EmpiricalStepParameters = EmpiricalStepParameters {
    prefix_steps: 0,
    recent_successes: 128,
    recent_weight: 3,
    all_history_weight: 1,
    update_every_records: 64,
    hash_every_records: 1024,
};

const DRAW_TABLE_MEMORY_RESERVE_BYTES: usize = 2 * 1024 * 1024;

const MAX_FEEDBACK_ENTRIES: usize = 4096;

const MAX_FEEDBACK_WEIGHT: u64 = 1 << 32;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DrawTableHeader {
    pub source_sha256: String,
    pub parameters: EmpiricalStepParameters,
    pub initial: EmpiricalStepCheckpoint,
}

#[must_use]
pub fn draw_table_identifier(parameters: EmpiricalStepParameters) -> String {
    format!(
        "{DRAW_TABLE_PREFIX}{},{},{},{},{},{}",
        parameters.prefix_steps,
        parameters.recent_successes,
        parameters.recent_weight,
        parameters.all_history_weight,
        parameters.update_every_records,
        parameters.hash_every_records
    )
}

pub fn draw_table_from_identifier(
    identifier: &str,
) -> Result<EmpiricalStepParameters, Box<dyn Error>> {
    let fields = identifier
        .strip_prefix(DRAW_TABLE_PREFIX)
        .ok_or("campaign stream draw table policy is not recognized")?;
    let mut fields = fields.split(',');
    let parameters = EmpiricalStepParameters {
        prefix_steps: parse_field(&mut fields, "prefix steps")?,
        recent_successes: parse_field(&mut fields, "recent successes")?,
        recent_weight: parse_field(&mut fields, "recent weight")?,
        all_history_weight: parse_field(&mut fields, "all-history weight")?,
        update_every_records: parse_field(&mut fields, "update interval")?,
        hash_every_records: parse_field(&mut fields, "hash interval")?,
    };
    if fields.next().is_some() {
        return Err("draw table policy carries extra fields".into());
    }
    parameters.validate()?;
    Ok(parameters)
}

fn parse_field<'a, T>(
    fields: &mut impl Iterator<Item = &'a str>,
    name: &str,
) -> Result<T, Box<dyn Error>>
where
    T: std::str::FromStr,
    T::Err: Error + 'static,
{
    Ok(fields
        .next()
        .ok_or_else(|| format!("draw table policy is missing {name}"))?
        .parse()?)
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DrawVersionSchedule {
    last_use: BTreeMap<u64, usize>,
}

impl DrawVersionSchedule {
    pub fn require(&mut self, version: u64, record: usize) {
        let entry = self.last_use.entry(version).or_insert(record);
        *entry = (*entry).max(record);
    }

    #[must_use]
    pub fn is_required(&self, version: u64) -> bool {
        self.last_use.contains_key(&version)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.last_use.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.last_use.is_empty()
    }

    fn expired(&self, version: u64, record: usize) -> bool {
        self.last_use
            .get(&version)
            .is_none_or(|last| *last < record)
    }
}

struct DrawTableVersion<A> {
    checkpoint: EmpiricalStepCheckpoint,
    history_len: usize,
    history_counts: Rc<BTreeMap<A, usize>>,
    recent: Rc<Vec<A>>,
    feedback: Rc<BTreeMap<u64, u64>>,
}

#[derive(Clone, Copy)]
pub struct DrawView<'a, A> {
    pub steps: EmpiricalStepTableRef<'a, A>,
    pub feedback: &'a BTreeMap<u64, u64>,
}

impl<A> DrawView<'_, A> {
    pub fn pick_feedback(&self, rand: &mut RomuDuoJrRand) -> Result<Option<u64>, Box<dyn Error>> {
        let total = self
            .feedback
            .values()
            .try_fold(0_u64, |total, weight| total.checked_add(*weight))
            .ok_or("draw feedback weights overflow")?;
        let Some(total) = NonZeroUsize::new(usize::try_from(total)?) else {
            return Ok(None);
        };
        let mut left = u64::try_from(rand.below(total))?;
        for (key, weight) in self.feedback {
            if left < *weight {
                return Ok(Some(*key));
            }
            left -= weight;
        }
        Err("draw feedback pick fell outside its weights".into())
    }
}

pub struct DrawTables<A: Copy + Ord + Serialize> {
    tables: EmpiricalStepTables<A>,
    versions: BTreeMap<u64, DrawTableVersion<A>>,
    feedback: Rc<BTreeMap<u64, u64>>,
    feedback_sha256: String,
}

impl<A: Copy + Ord + Serialize> DrawTables<A> {
    pub fn new(parameters: EmpiricalStepParameters) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            tables: EmpiricalStepTables::new(parameters)?,
            versions: BTreeMap::new(),
            feedback: Rc::new(BTreeMap::new()),
            feedback_sha256: String::new(),
        })
    }

    #[must_use]
    pub fn memory_reserve_bytes() -> usize {
        DRAW_TABLE_MEMORY_RESERVE_BYTES
    }

    #[must_use]
    pub fn memory_bytes(&self) -> usize {
        self.tables.memory_bytes().saturating_add(
            self.feedback
                .len()
                .saturating_mul(2 * std::mem::size_of::<u64>()),
        )
    }

    #[must_use]
    pub fn parameters(&self) -> EmpiricalStepParameters {
        self.tables.parameters()
    }

    #[must_use]
    pub fn version_count(&self) -> usize {
        self.versions.len()
    }

    pub fn fold_source(&mut self, suffix: &[A]) -> Result<(), Box<dyn Error>> {
        self.tables.fold_retained(suffix)?;
        Ok(())
    }

    pub fn flush(&mut self) -> Result<(), Box<dyn Error>> {
        self.tables.flush()?;
        Ok(())
    }

    pub fn checkpoint(&self) -> Result<EmpiricalStepCheckpoint, Box<dyn Error>> {
        Ok(self.with_feedback(self.tables.checkpoint()?))
    }

    fn with_feedback(&self, mut checkpoint: EmpiricalStepCheckpoint) -> EmpiricalStepCheckpoint {
        if !self.feedback_sha256.is_empty() {
            let mut hasher = Sha256::new();
            hasher.update(b"dissonance-draw-feedback-v1\0");
            hasher.update(checkpoint.table_sha256.as_bytes());
            hasher.update(self.feedback_sha256.as_bytes());
            checkpoint.table_sha256 = format!("{:x}", hasher.finalize());
        }
        checkpoint
    }

    fn set_feedback(&mut self, feedback: BTreeMap<u64, u64>) -> Result<(), Box<dyn Error>> {
        let mut weights = feedback
            .into_iter()
            .filter(|(_, weight)| *weight > 0)
            .map(|(key, weight)| (key, weight.min(MAX_FEEDBACK_WEIGHT)))
            .collect::<Vec<_>>();
        if weights.len() > MAX_FEEDBACK_ENTRIES {
            weights.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
            weights.truncate(MAX_FEEDBACK_ENTRIES);
        }
        let feedback = weights.into_iter().collect::<BTreeMap<_, _>>();
        if feedback == *self.feedback {
            return Ok(());
        }
        self.feedback_sha256 = if feedback.is_empty() {
            String::new()
        } else {
            let mut hasher = Sha256::new();
            hasher.update(serde_json::to_vec(&feedback)?);
            format!("{:x}", hasher.finalize())
        };
        self.feedback = Rc::new(feedback);
        Ok(())
    }

    pub fn finish_record(
        &mut self,
        retained: &[(usize, &[A])],
    ) -> Result<Option<EmpiricalStepCheckpoint>, Box<dyn Error>> {
        self.finish_record_with_feedback(retained, BTreeMap::new)
    }

    pub fn finish_record_with_feedback<F>(
        &mut self,
        retained: &[(usize, &[A])],
        feedback: F,
    ) -> Result<Option<EmpiricalStepCheckpoint>, Box<dyn Error>>
    where
        F: FnOnce() -> BTreeMap<u64, u64>,
    {
        for (parent_actions, input) in retained {
            let folded = input.get(*parent_actions..).unwrap_or(&[]);
            self.tables.fold_retained(folded)?;
        }
        if self
            .tables
            .records()
            .saturating_add(1)
            .is_multiple_of(self.tables.parameters().update_every_records)
        {
            self.set_feedback(feedback())?;
        }
        Ok(self
            .tables
            .finish_record()?
            .map(|checkpoint| self.with_feedback(checkpoint)))
    }

    pub fn draw<F, R>(
        &self,
        before: Option<&EmpiricalStepCheckpoint>,
        replay: bool,
        draw: F,
    ) -> Result<R, Box<dyn Error>>
    where
        F: FnOnce(DrawView<'_, A>) -> Result<R, Box<dyn Error>>,
    {
        if !replay {
            return draw(DrawView {
                steps: self.tables.view(),
                feedback: &self.feedback,
            });
        }
        let before = before.ok_or("recorded draw is missing its table version")?;
        let version = self
            .versions
            .get(&before.records)
            .ok_or("recorded draw names an unknown table version")?;
        if version.checkpoint != *before {
            return Err("recorded draw table hash does not match replay".into());
        }
        draw(DrawView {
            steps: EmpiricalStepTableRef::from_counts(
                self.tables.parameters(),
                &version.recent,
                &version.history_counts,
                version.history_len,
            ),
            feedback: &version.feedback,
        })
    }

    pub fn remember_version(
        &mut self,
        schedule: &DrawVersionSchedule,
    ) -> Result<(), Box<dyn Error>> {
        let records = self.tables.records();
        if !schedule.is_required(records) {
            return Ok(());
        }
        let checkpoint = self.checkpoint()?;
        let history_len = self.tables.history_len();
        let reusable = self.versions.last_key_value().filter(|(_, last)| {
            last.checkpoint.table_sha256 == checkpoint.table_sha256
                && last.history_len == history_len
        });
        let recent = reusable
            .map(|(_, last)| Rc::clone(&last.recent))
            .unwrap_or_else(|| Rc::new(self.tables.recent().to_vec()));
        let history_counts = reusable
            .map(|(_, last)| Rc::clone(&last.history_counts))
            .unwrap_or_else(|| Rc::new(self.tables.compact_history().clone()));
        self.versions.insert(
            records,
            DrawTableVersion {
                checkpoint,
                history_len,
                history_counts,
                recent,
                feedback: Rc::clone(&self.feedback),
            },
        );
        Ok(())
    }

    pub fn release_versions(&mut self, schedule: &DrawVersionSchedule, record: usize) {
        self.versions
            .retain(|version, _| !schedule.expired(*version, record));
    }
}

pub fn biased_step<A: Copy + Ord>(
    view: DrawView<'_, A>,
    rand: &mut RomuDuoJrRand,
) -> Result<Option<A>, Box<dyn Error>> {
    let length = view.steps.mixed_len()?;
    Ok(NonZeroUsize::new(length)
        .and_then(|length| view.steps.mixed_step(rand.below(length)))
        .copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tables() -> DrawTables<u8> {
        DrawTables::new(EmpiricalStepParameters {
            update_every_records: 1,
            hash_every_records: 1,
            ..DEFAULT_DRAW_TABLE_PARAMETERS
        })
        .expect("hash every record")
    }

    #[test]
    fn the_identifier_round_trips_and_rejects_a_bad_shape() {
        let identifier = draw_table_identifier(DEFAULT_DRAW_TABLE_PARAMETERS);
        assert_eq!(
            draw_table_from_identifier(&identifier).expect("parse identifier"),
            DEFAULT_DRAW_TABLE_PARAMETERS
        );
        for bad in [
            "retained_suffix_table_v1:0,128,3,1,64",
            "retained_suffix_table_v1:0,128,3,1,64,1024,7",
            "retained_suffix_table_v1:0,0,3,1,64,1024",
            "unknown_table_v1:0,128,3,1,64,1024",
        ] {
            assert!(
                draw_table_from_identifier(bad).is_err(),
                "{bad} must not parse"
            );
        }
    }

    #[test]
    fn a_recorded_draw_reads_the_version_it_names_and_rejects_a_tampered_one() {
        let mut live = tables();
        let mut schedule = DrawVersionSchedule::default();
        let mut checkpoints = Vec::new();
        for record in 0..4_usize {
            let suffix = [u8::try_from(record).expect("small record")];
            if let Some(checkpoint) = live
                .finish_record(&[(0, suffix.as_slice())])
                .expect("fold a record")
            {
                schedule.require(checkpoint.records, record);
                checkpoints.push(checkpoint);
            }
        }
        let mut replay = tables();
        for record in 0..4_usize {
            replay.remember_version(&schedule).expect("keep a version");
            let suffix = [u8::try_from(record).expect("small record")];
            replay
                .finish_record(&[(0, suffix.as_slice())])
                .expect("fold a record");
        }
        let checkpoint = checkpoints.first().expect("one checkpoint").clone();
        assert_eq!(checkpoint.records, 1);
        let sequence = |view: DrawView<'_, u8>| -> Result<Vec<u8>, Box<dyn Error>> {
            Ok((0..view.steps.mixed_len()?)
                .map(|index| {
                    *view
                        .steps
                        .mixed_step(index)
                        .expect("a step inside the table")
                })
                .collect())
        };
        let mut named = tables();
        named
            .finish_record(&[(0, [0_u8].as_slice())])
            .expect("fold a record");
        let expected = named.draw(None, false, sequence).expect("read one record");
        let latest = replay
            .draw(None, false, sequence)
            .expect("read four records");
        assert_ne!(expected, latest);
        let drawn = replay
            .draw(Some(&checkpoint), true, sequence)
            .expect("read the named version");
        assert_eq!(drawn, expected);
        let tampered = EmpiricalStepCheckpoint {
            table_sha256: "0".repeat(64),
            ..checkpoint.clone()
        };
        assert!(replay.draw(Some(&tampered), true, |_| Ok(())).is_err());
        assert!(replay.draw(None, true, |_| Ok(())).is_err());
    }

    #[test]
    fn feedback_lands_at_a_table_update_and_replays_with_its_version() {
        let mut live = DrawTables::<u8>::new(EmpiricalStepParameters {
            update_every_records: 2,
            hash_every_records: 1,
            ..DEFAULT_DRAW_TABLE_PARAMETERS
        })
        .expect("valid parameters");
        let untouched = live.checkpoint().expect("checkpoint");
        let weights = BTreeMap::from([(7, 1), (9, 3), (11, 0)]);
        let first = live
            .finish_record_with_feedback(&[], || weights.clone())
            .expect("finish a record")
            .expect("hash every record");
        assert_eq!(first.table_sha256, untouched.table_sha256);
        let second = live
            .finish_record_with_feedback(&[], || weights.clone())
            .expect("finish a record")
            .expect("hash every record");
        assert_ne!(second.table_sha256, untouched.table_sha256);
        let feedback = live
            .draw(None, false, |view| Ok(view.feedback.clone()))
            .expect("read the feedback");
        assert_eq!(feedback, BTreeMap::from([(7, 1), (9, 3)]));
        let mut rand = RomuDuoJrRand::with_seed(1);
        let mut picks = BTreeMap::new();
        for _ in 0..4_000 {
            let key = live
                .draw(None, false, |view| view.pick_feedback(&mut rand))
                .expect("pick a key")
                .expect("a weighted key");
            *picks.entry(key).or_insert(0_u32) += 1;
        }
        assert_eq!(picks.keys().copied().collect::<Vec<_>>(), [7, 9]);
        assert!(picks[&9] > 2 * picks[&7]);

        let mut schedule = DrawVersionSchedule::default();
        schedule.require(2, 2);
        let mut replay = DrawTables::<u8>::new(live.parameters()).expect("valid parameters");
        for _ in 0..2 {
            replay
                .finish_record_with_feedback(&[], || weights.clone())
                .expect("finish a record");
        }
        replay.remember_version(&schedule).expect("keep a version");
        replay
            .finish_record_with_feedback(&[], BTreeMap::new)
            .expect("finish a record");
        replay
            .finish_record_with_feedback(&[], BTreeMap::new)
            .expect("finish a record");
        assert!(
            replay
                .draw(None, false, |view| Ok(view.feedback.is_empty()))
                .expect("read live feedback")
        );
        assert_eq!(
            replay
                .draw(Some(&second), true, |view| Ok(view.feedback.clone()))
                .expect("read the recorded feedback"),
            feedback
        );
        assert_eq!(
            replay.checkpoint().expect("checkpoint").table_sha256,
            untouched.table_sha256
        );
    }

    #[test]
    fn a_version_is_dropped_once_its_last_record_has_replayed() {
        let mut schedule = DrawVersionSchedule::default();
        schedule.require(1, 5);
        schedule.require(1, 2);
        schedule.require(2, 9);
        assert_eq!(schedule.len(), 2);
        let mut replay = tables();
        replay.versions.insert(
            1,
            DrawTableVersion {
                checkpoint: EmpiricalStepCheckpoint {
                    records: 1,
                    retained_successes: 1,
                    table_sha256: String::new(),
                },
                history_len: 0,
                history_counts: Rc::new(BTreeMap::new()),
                recent: Rc::new(Vec::new()),
                feedback: Rc::new(BTreeMap::new()),
            },
        );
        replay.versions.insert(
            2,
            DrawTableVersion {
                checkpoint: EmpiricalStepCheckpoint {
                    records: 2,
                    retained_successes: 1,
                    table_sha256: String::new(),
                },
                history_len: 0,
                history_counts: Rc::new(BTreeMap::new()),
                recent: Rc::new(Vec::new()),
                feedback: Rc::new(BTreeMap::new()),
            },
        );
        replay.release_versions(&schedule, 5);
        assert_eq!(replay.version_count(), 2);
        replay.release_versions(&schedule, 6);
        assert_eq!(replay.version_count(), 1);
        replay.release_versions(&schedule, 9);
        assert_eq!(replay.version_count(), 1);
        replay.release_versions(&schedule, 10);
        assert_eq!(replay.version_count(), 0);
    }
}
