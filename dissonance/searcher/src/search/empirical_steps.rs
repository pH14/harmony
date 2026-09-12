// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::{BTreeMap, VecDeque},
    error::Error,
    fmt,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_COMPACT_HISTORY_DISTINCT: usize = 4096;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EmpiricalStepParameters {
    pub prefix_steps: usize,
    pub recent_successes: usize,
    pub recent_weight: usize,
    pub all_history_weight: usize,
    pub update_every_records: u64,
    pub hash_every_records: u64,
}

impl EmpiricalStepParameters {
    pub fn validate(self) -> Result<(), EmpiricalStepError> {
        if self.recent_successes == 0 {
            return Err(EmpiricalStepError::InvalidParameters(
                "recent success window must be nonzero",
            ));
        }
        if self.update_every_records == 0 || self.hash_every_records == 0 {
            return Err(EmpiricalStepError::InvalidParameters(
                "table update and hash intervals must be nonzero",
            ));
        }
        if self.recent_weight == 0 && self.all_history_weight == 0 {
            return Err(EmpiricalStepError::InvalidParameters(
                "at least one empirical step table weight must be nonzero",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EmpiricalStepCheckpoint {
    pub records: u64,
    pub retained_successes: u64,
    pub table_sha256: String,
}

#[derive(Debug)]
pub enum EmpiricalStepError {
    InvalidParameters(&'static str),
    TableLengthOverflow,
    Serialization(serde_json::Error),
    RecentWindowDiverged,
}

impl fmt::Display for EmpiricalStepError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidParameters(message) => formatter.write_str(message),
            Self::TableLengthOverflow => {
                formatter.write_str("weighted empirical step table is too large")
            }
            Self::Serialization(error) => {
                write!(
                    formatter,
                    "empirical step table serialization failed: {error}"
                )
            }
            Self::RecentWindowDiverged => {
                formatter.write_str("recent empirical step window accounting diverged")
            }
        }
    }
}

impl Error for EmpiricalStepError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Serialization(error) => Some(error),
            Self::InvalidParameters(_) | Self::TableLengthOverflow | Self::RecentWindowDiverged => {
                None
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct EmpiricalStepTables<Step> {
    parameters: EmpiricalStepParameters,
    pending: Vec<Vec<Step>>,
    recent_sequences: VecDeque<Vec<Step>>,
    recent: Vec<Step>,
    compact_history: BTreeMap<Step, usize>,
    compact_history_len: usize,
    history_hasher: Sha256,
    table_sha256: String,
    records: u64,
    retained_successes: u64,
}

impl<Step> EmpiricalStepTables<Step>
where
    Step: Clone + Ord + Serialize,
{
    pub fn new(parameters: EmpiricalStepParameters) -> Result<Self, EmpiricalStepError> {
        parameters.validate()?;
        let mut tables = Self {
            parameters,
            pending: Vec::new(),
            recent_sequences: VecDeque::new(),
            recent: Vec::new(),
            compact_history: BTreeMap::new(),
            compact_history_len: 0,
            history_hasher: Sha256::new(),
            table_sha256: String::new(),
            records: 0,
            retained_successes: 0,
        };
        tables.table_sha256 = tables.hash_current_tables()?;
        Ok(tables)
    }

    fn hash_current_tables(&self) -> Result<String, EmpiricalStepError> {
        let mut hasher = self.history_hasher.clone();
        hasher.update(b"dissonance-empirical-compact-history-v1\0");
        let recent = serde_json::to_vec(&self.recent).map_err(EmpiricalStepError::Serialization)?;
        hasher.update(&recent);
        Ok(format!("{:x}", hasher.finalize()))
    }

    pub fn fold_retained(&mut self, sequence: &[Step]) -> Result<(), EmpiricalStepError> {
        let Some(suffix) = sequence.get(self.parameters.prefix_steps..) else {
            return Ok(());
        };
        if suffix.is_empty() {
            return Ok(());
        }
        self.pending.push(suffix.to_vec());
        self.retained_successes = self.retained_successes.saturating_add(1);
        Ok(())
    }

    fn apply_contribution(&mut self, contribution: Vec<Step>) -> Result<(), EmpiricalStepError> {
        let bytes = serde_json::to_vec(&contribution).map_err(EmpiricalStepError::Serialization)?;
        self.history_hasher.update(&bytes);
        for step in &contribution {
            if let Some(count) = self.compact_history.get_mut(step) {
                *count = count
                    .checked_add(1)
                    .ok_or(EmpiricalStepError::TableLengthOverflow)?;
                self.compact_history_len = self
                    .compact_history_len
                    .checked_add(1)
                    .ok_or(EmpiricalStepError::TableLengthOverflow)?;
            } else if self.compact_history.len() < MAX_COMPACT_HISTORY_DISTINCT {
                self.compact_history.insert(step.clone(), 1);
                self.compact_history_len = self
                    .compact_history_len
                    .checked_add(1)
                    .ok_or(EmpiricalStepError::TableLengthOverflow)?;
            }
        }
        self.recent.extend_from_slice(&contribution);
        self.recent_sequences.push_back(contribution);
        while self.recent_sequences.len() > self.parameters.recent_successes {
            let removed = self
                .recent_sequences
                .pop_front()
                .ok_or(EmpiricalStepError::RecentWindowDiverged)?;
            if removed.len() > self.recent.len() {
                return Err(EmpiricalStepError::RecentWindowDiverged);
            }
            self.recent.drain(..removed.len());
        }
        Ok(())
    }

    pub fn flush(&mut self) -> Result<(), EmpiricalStepError> {
        let pending = std::mem::take(&mut self.pending);
        if pending.is_empty() {
            return Ok(());
        }
        for contribution in pending {
            self.apply_contribution(contribution)?;
        }
        self.table_sha256 = self.hash_current_tables()?;
        Ok(())
    }

    pub fn finish_record(&mut self) -> Result<Option<EmpiricalStepCheckpoint>, EmpiricalStepError> {
        self.records = self.records.saturating_add(1);
        if self
            .records
            .is_multiple_of(self.parameters.update_every_records)
        {
            self.flush()?;
        }
        if !self
            .records
            .is_multiple_of(self.parameters.hash_every_records)
        {
            return Ok(None);
        }
        self.checkpoint().map(Some)
    }

    pub fn checkpoint(&self) -> Result<EmpiricalStepCheckpoint, EmpiricalStepError> {
        Ok(EmpiricalStepCheckpoint {
            records: self.records,
            retained_successes: self.retained_successes,
            table_sha256: self.table_sha256.clone(),
        })
    }

    #[must_use]
    pub fn parameters(&self) -> EmpiricalStepParameters {
        self.parameters
    }

    #[must_use]
    pub fn records(&self) -> u64 {
        self.records
    }

    #[must_use]
    pub fn retained_successes(&self) -> u64 {
        self.retained_successes
    }

    #[must_use]
    pub fn recent(&self) -> &[Step] {
        &self.recent
    }

    pub fn mixed_len(&self) -> Result<usize, EmpiricalStepError> {
        self.view().mixed_len()
    }

    #[must_use]
    pub fn mixed_step(&self, index: usize) -> Option<&Step> {
        self.view().mixed_step(index)
    }

    #[must_use]
    pub fn view(&self) -> EmpiricalStepTableRef<'_, Step> {
        EmpiricalStepTableRef::from_counts(
            self.parameters,
            &self.recent,
            &self.compact_history,
            self.compact_history_len,
        )
    }

    #[must_use]
    pub fn history_len(&self) -> usize {
        self.compact_history_len
    }

    #[must_use]
    pub fn compact_history(&self) -> &BTreeMap<Step, usize> {
        &self.compact_history
    }

    #[must_use]
    pub fn memory_bytes(&self) -> usize {
        let step = std::mem::size_of::<Step>();
        let pending_steps = self.pending.iter().map(Vec::len).sum::<usize>();
        let recent_sequence_steps = self.recent_sequences.iter().map(Vec::len).sum::<usize>();
        self.recent
            .len()
            .saturating_add(pending_steps)
            .saturating_add(recent_sequence_steps)
            .saturating_mul(step)
            .saturating_add(
                self.compact_history
                    .len()
                    .saturating_mul(step.saturating_add(std::mem::size_of::<usize>())),
            )
    }
}

#[derive(Clone, Copy)]
pub struct EmpiricalStepTableRef<'a, Step> {
    parameters: EmpiricalStepParameters,
    recent: &'a [Step],
    history: EmpiricalStepHistoryRef<'a, Step>,
}

#[derive(Clone, Copy)]
enum EmpiricalStepHistoryRef<'a, Step> {
    Counts(&'a BTreeMap<Step, usize>, usize),
}

impl<'a, Step> EmpiricalStepTableRef<'a, Step> {
    #[must_use]
    pub fn from_counts(
        parameters: EmpiricalStepParameters,
        recent: &'a [Step],
        history: &'a BTreeMap<Step, usize>,
        history_len: usize,
    ) -> Self {
        Self {
            parameters,
            recent,
            history: EmpiricalStepHistoryRef::Counts(history, history_len),
        }
    }

    fn history_len(&self) -> usize {
        match &self.history {
            EmpiricalStepHistoryRef::Counts(_, history_len) => *history_len,
        }
    }

    pub fn mixed_len(&self) -> Result<usize, EmpiricalStepError> {
        self.recent
            .len()
            .checked_mul(self.parameters.recent_weight)
            .and_then(|recent| {
                self.history_len()
                    .checked_mul(self.parameters.all_history_weight)
                    .and_then(|history| recent.checked_add(history))
            })
            .ok_or(EmpiricalStepError::TableLengthOverflow)
    }

    #[must_use]
    pub fn mixed_step(&self, index: usize) -> Option<&'a Step> {
        let recent_span = self
            .recent
            .len()
            .checked_mul(self.parameters.recent_weight)?;
        if index < recent_span {
            return (!self.recent.is_empty()).then(|| &self.recent[index % self.recent.len()]);
        }
        let history_index = index.checked_sub(recent_span)?;
        let history_len = self.history_len();
        let history_span = history_len.checked_mul(self.parameters.all_history_weight)?;
        if history_index >= history_span || history_len == 0 {
            return None;
        }
        let base_index = history_index % history_len;
        match self.history {
            EmpiricalStepHistoryRef::Counts(history, _) => {
                let mut remaining = base_index;
                for (step, count) in history {
                    if remaining < *count {
                        return Some(step);
                    }
                    remaining = remaining.checked_sub(*count)?;
                }
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, cmp::Ordering, collections::BTreeMap, rc::Rc};

    use serde::Serializer;

    use super::{EmpiricalStepParameters, EmpiricalStepTables};

    #[derive(Clone)]
    struct CountingStep {
        value: u8,
        serializations: Rc<Cell<usize>>,
    }

    impl PartialEq for CountingStep {
        fn eq(&self, other: &Self) -> bool {
            self.value == other.value
        }
    }

    impl Eq for CountingStep {}

    impl PartialOrd for CountingStep {
        fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
            Some(self.cmp(other))
        }
    }

    impl Ord for CountingStep {
        fn cmp(&self, other: &Self) -> Ordering {
            self.value.cmp(&other.value)
        }
    }

    impl serde::Serialize for CountingStep {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            self.serializations
                .set(self.serializations.get().saturating_add(1));
            serializer.serialize_u8(self.value)
        }
    }

    fn parameters() -> EmpiricalStepParameters {
        EmpiricalStepParameters {
            prefix_steps: 1,
            recent_successes: 2,
            recent_weight: 2,
            all_history_weight: 1,
            update_every_records: 2,
            hash_every_records: 2,
        }
    }

    #[test]
    fn fold_keeps_recent_and_compact_history() {
        let mut tables = EmpiricalStepTables::new(parameters()).expect("valid parameters");
        tables.fold_retained(&[0, 1, 2]).expect("first success");
        assert!(tables.finish_record().expect("first record").is_none());
        tables.fold_retained(&[0, 3]).expect("second success");
        assert!(tables.finish_record().expect("second record").is_some());
        tables.fold_retained(&[0, 4, 5]).expect("third success");
        tables.finish_record().expect("third record");
        tables.flush().expect("final update");
        assert_eq!(tables.recent(), &[3, 4, 5]);
        assert_eq!(tables.history_len(), 5);
        assert_eq!(
            tables.compact_history(),
            &BTreeMap::from([(1, 1), (2, 1), (3, 1), (4, 1), (5, 1)])
        );
        assert_eq!(tables.retained_successes(), 3);
    }

    #[test]
    fn mixed_index_repeats_each_empirical_table_by_weight() {
        let mut tables = EmpiricalStepTables::new(parameters()).expect("valid parameters");
        tables.fold_retained(&[0, 7]).expect("success");
        tables.flush().expect("make buffered success visible");
        assert_eq!(tables.mixed_len().expect("mixed length"), 3);
        assert_eq!(tables.mixed_step(0), Some(&7));
        assert_eq!(tables.mixed_step(1), Some(&7));
        assert_eq!(tables.mixed_step(2), Some(&7));
        assert_eq!(tables.mixed_step(3), None);
    }

    #[test]
    fn checkpoints_are_reproducible() {
        let run = || {
            let mut tables = EmpiricalStepTables::new(parameters()).expect("valid parameters");
            for sequence in [vec![0, 1], vec![0, 2], vec![0, 3]] {
                tables.fold_retained(&sequence).expect("fold success");
                let _ = tables.finish_record().expect("finish record");
            }
            tables.checkpoint().expect("final checkpoint")
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn incremental_hash_avoids_history_reserialization() {
        let serializations = Rc::new(Cell::new(0));
        let step = |value| CountingStep {
            value,
            serializations: Rc::clone(&serializations),
        };
        let mut tables = EmpiricalStepTables::new(parameters()).expect("valid parameters");
        let flush_cost = |tables: &mut EmpiricalStepTables<CountingStep>, value| {
            tables
                .fold_retained(&[step(0), step(value)])
                .expect("fold success");
            let before = serializations.get();
            tables.flush().expect("make buffered success visible");
            serializations.get() - before
        };
        let mut costs = Vec::new();
        for round in 0..6_u8 {
            costs.push(flush_cost(&mut tables, round));
        }
        assert_eq!(costs[2], costs[5]);
    }

    #[test]
    fn compact_history_preserves_frequencies_in_deterministic_key_order() {
        use super::EmpiricalStepTables as Tables;

        let mut compact_parameters = parameters();
        compact_parameters.prefix_steps = 0;
        compact_parameters.recent_weight = 0;
        let mut tables = Tables::new(compact_parameters).expect("valid parameters");
        tables
            .fold_retained(&[3_u16, 1, 3, 2])
            .expect("fold success");
        tables.flush().expect("make compact history visible");

        assert_eq!(tables.history_len(), 4);
        assert_eq!(tables.compact_history().len(), 3);
        assert_eq!(tables.mixed_len().expect("mixed length"), 4);
        assert_eq!(
            (0..4)
                .map(|index| tables.mixed_step(index).copied())
                .collect::<Vec<_>>(),
            vec![Some(1), Some(2), Some(3), Some(3)]
        );
    }

    #[test]
    fn compact_history_has_a_fixed_distinct_step_cap() {
        use super::{EmpiricalStepTables as Tables, MAX_COMPACT_HISTORY_DISTINCT};

        let mut compact_parameters = parameters();
        compact_parameters.prefix_steps = 0;
        let mut tables = Tables::new(compact_parameters).expect("valid parameters");
        for step in 0..MAX_COMPACT_HISTORY_DISTINCT.saturating_add(257) {
            tables.fold_retained(&[step]).expect("fold success");
        }
        tables.flush().expect("make compact history visible");

        assert_eq!(tables.compact_history().len(), MAX_COMPACT_HISTORY_DISTINCT);
        assert_eq!(tables.history_len(), MAX_COMPACT_HISTORY_DISTINCT);
        assert_eq!(
            tables.mixed_step(tables.mixed_len().expect("mixed length")),
            None
        );
    }

    #[test]
    fn checkpoint_reuses_hash_until_visible_tables_change() {
        let serializations = Rc::new(Cell::new(0));
        let step = |value| CountingStep {
            value,
            serializations: Rc::clone(&serializations),
        };
        let mut tables = EmpiricalStepTables::new(parameters()).expect("valid parameters");
        tables
            .fold_retained(&[step(0), step(1)])
            .expect("fold success");
        tables.flush().expect("make buffered success visible");
        let after_flush = serializations.get();
        assert!(after_flush > 0);

        let first = tables.checkpoint().expect("first checkpoint");
        let second = tables.checkpoint().expect("second checkpoint");
        assert_eq!(first, second);
        assert_eq!(serializations.get(), after_flush);
    }

    #[test]
    fn hash_domain_and_memory_charge_are_exact() {
        use super::EmpiricalStepTables as Tables;
        use sha2::{Digest, Sha256};

        let mut compact = Tables::<u16>::new(parameters()).expect("compact incremental tables");
        let mut expected_compact = Sha256::new();
        expected_compact.update(b"dissonance-empirical-compact-history-v1\0");
        expected_compact
            .update(serde_json::to_vec(&Vec::<u16>::new()).expect("encode empty recent"));
        assert_eq!(
            compact
                .checkpoint()
                .expect("compact checkpoint")
                .table_sha256,
            format!("{:x}", expected_compact.finalize())
        );

        compact
            .fold_retained(&[0, 3, 3, 7])
            .expect("fold compact sequence");
        assert!(compact.memory_bytes() > 1);
        compact.flush().expect("flush compact sequence");
        assert!(compact.memory_bytes() > 1);
    }
}
