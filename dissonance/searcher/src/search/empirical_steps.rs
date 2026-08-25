// SPDX-License-Identifier: AGPL-3.0-or-later

//! Game-neutral empirical step tables folded deterministically from retained sequences.

use std::{collections::VecDeque, error::Error, fmt};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Registered rule producing the recorded table hash.
///
/// `FullJson` re-serializes both ordered tables on every visible update, so
/// its cost grows with the never-deleted history and dominates the run once
/// the source is large. `IncrementalHistory` feeds each appended contribution
/// into a persistent hasher and re-serializes only the bounded recent window,
/// keeping every update O(recent). The two rules produce different hashes for
/// the same tables, so each is bound to its own recorded policy identifier.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum EmpiricalStepHashRule {
    /// Hash the JSON of both complete ordered tables.
    #[default]
    FullJson,
    /// Fold appended history into a running hasher; re-serialize only recent.
    IncrementalHistory,
}

/// Registered parameters for one deterministic empirical-step fold.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EmpiricalStepParameters {
    /// Ignore this many leading steps in every retained sequence.
    pub prefix_steps: usize,
    /// Number of most recent retained successes contributing to the recent table.
    pub recent_successes: usize,
    /// Frequency multiplier for the recent table in biased draws.
    pub recent_weight: usize,
    /// Frequency multiplier for the never-deleted all-history table.
    pub all_history_weight: usize,
    /// Recorded-stream interval between visible table updates.
    pub update_every_records: u64,
    /// Recorded-stream interval between table-hash checkpoints.
    pub hash_every_records: u64,
}

impl EmpiricalStepParameters {
    /// Validate the bounded, non-vacuous table configuration.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero recent window, zero checkpoint interval, or
    /// a mixture in which both weights are zero.
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

/// One reproducible table checkpoint.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EmpiricalStepCheckpoint {
    /// Recorded stream records folded through this checkpoint.
    pub records: u64,
    /// Retained success sequences folded through this checkpoint.
    pub retained_successes: u64,
    /// SHA-256 of the ordered recent and all-history tables.
    pub table_sha256: String,
}

/// Deterministic empirical-step fold failure.
#[derive(Debug)]
pub enum EmpiricalStepError {
    /// Registered parameters are vacuous or out of bounds.
    InvalidParameters(&'static str),
    /// Weighted table length overflowed.
    TableLengthOverflow,
    /// Ordered table serialization failed.
    Serialization(serde_json::Error),
    /// Internal recent-window accounting diverged.
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

/// Recent and all-history tables derived only from stream-ordered successes.
#[derive(Clone, Debug)]
pub struct EmpiricalStepTables<Step> {
    parameters: EmpiricalStepParameters,
    hash_rule: EmpiricalStepHashRule,
    pending: Vec<Vec<Step>>,
    recent_sequences: VecDeque<Vec<Step>>,
    recent: Vec<Step>,
    all_history: Vec<Step>,
    history_hasher: Sha256,
    table_sha256: String,
    records: u64,
    retained_successes: u64,
    checkpoints: Vec<EmpiricalStepCheckpoint>,
}

impl<Step> EmpiricalStepTables<Step>
where
    Step: Clone + Serialize,
{
    /// Start an empty deterministic fold.
    ///
    /// # Errors
    ///
    /// Returns an error when the parameters are invalid.
    pub fn new(parameters: EmpiricalStepParameters) -> Result<Self, EmpiricalStepError> {
        Self::with_hash_rule(parameters, EmpiricalStepHashRule::FullJson)
    }

    /// Start an empty deterministic fold under the named table-hash rule.
    ///
    /// # Errors
    ///
    /// Returns an error when the parameters are invalid.
    pub fn with_hash_rule(
        parameters: EmpiricalStepParameters,
        hash_rule: EmpiricalStepHashRule,
    ) -> Result<Self, EmpiricalStepError> {
        parameters.validate()?;
        let mut tables = Self {
            parameters,
            hash_rule,
            pending: Vec::new(),
            recent_sequences: VecDeque::new(),
            recent: Vec::new(),
            all_history: Vec::new(),
            history_hasher: Sha256::new(),
            table_sha256: String::new(),
            records: 0,
            retained_successes: 0,
            checkpoints: Vec::new(),
        };
        tables.table_sha256 = tables.hash_current_tables()?;
        Ok(tables)
    }

    fn hash_current_tables(&self) -> Result<String, EmpiricalStepError> {
        match self.hash_rule {
            EmpiricalStepHashRule::FullJson => hash_tables(&self.recent, &self.all_history),
            EmpiricalStepHashRule::IncrementalHistory => {
                let mut hasher = self.history_hasher.clone();
                let recent =
                    serde_json::to_vec(&self.recent).map_err(EmpiricalStepError::Serialization)?;
                hasher.update(&recent);
                Ok(format!("{:x}", hasher.finalize()))
            }
        }
    }

    /// Fold one retained success sequence in stream order.
    ///
    /// A sequence at or before the registered prefix contributes no steps and
    /// is not counted as a useful retained success.
    ///
    /// # Errors
    ///
    /// Returns an error only if internal recent-window accounting diverges.
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
        if self.hash_rule == EmpiricalStepHashRule::IncrementalHistory {
            // Each contribution is a complete JSON array, so the byte stream
            // fed to the running hasher is framed unambiguously.
            let bytes =
                serde_json::to_vec(&contribution).map_err(EmpiricalStepError::Serialization)?;
            self.history_hasher.update(&bytes);
        }
        self.all_history.extend_from_slice(&contribution);
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

    /// Make every buffered success visible immediately.
    ///
    /// Archive source loading calls this once after folding the complete source.
    ///
    /// # Errors
    ///
    /// Returns an error if internal recent-window accounting diverges or the
    /// updated ordered tables cannot be serialized for their cached hash.
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

    /// Registered table-hash rule.
    #[must_use]
    pub fn hash_rule(&self) -> EmpiricalStepHashRule {
        self.hash_rule
    }

    /// Finish one recorded stream record and emit its periodic hash if due.
    ///
    /// # Errors
    ///
    /// Returns an error when table serialization fails.
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
        let checkpoint = self.checkpoint()?;
        self.checkpoints.push(checkpoint.clone());
        Ok(Some(checkpoint))
    }

    /// Snapshot the table hash at its current fold position.
    ///
    /// # Errors
    ///
    /// The result remains fallible for API compatibility. Serialization
    /// failures are reported when a visible generation is created or updated.
    pub fn checkpoint(&self) -> Result<EmpiricalStepCheckpoint, EmpiricalStepError> {
        Ok(EmpiricalStepCheckpoint {
            records: self.records,
            retained_successes: self.retained_successes,
            table_sha256: self.table_sha256.clone(),
        })
    }

    /// Registered fold parameters.
    #[must_use]
    pub fn parameters(&self) -> EmpiricalStepParameters {
        self.parameters
    }

    /// Recorded stream records folded so far.
    #[must_use]
    pub fn records(&self) -> u64 {
        self.records
    }

    /// Retained success sequences that contributed at least one step.
    #[must_use]
    pub fn retained_successes(&self) -> u64 {
        self.retained_successes
    }

    /// Ordered steps from the registered recent-success window.
    #[must_use]
    pub fn recent(&self) -> &[Step] {
        &self.recent
    }

    /// Ordered steps from every success ever folded.
    #[must_use]
    pub fn all_history(&self) -> &[Step] {
        &self.all_history
    }

    /// Periodic checkpoints emitted so far.
    #[must_use]
    pub fn checkpoints(&self) -> &[EmpiricalStepCheckpoint] {
        &self.checkpoints
    }

    /// Frequency-weighted mixed-table length.
    ///
    /// # Errors
    ///
    /// Returns an error if the registered weighted length overflows.
    pub fn mixed_len(&self) -> Result<usize, EmpiricalStepError> {
        self.view().mixed_len()
    }

    /// Resolve one frequency-weighted mixed-table index.
    #[must_use]
    pub fn mixed_step(&self, index: usize) -> Option<&Step> {
        self.view().mixed_step(index)
    }

    /// Borrowed view of the current visible tables.
    #[must_use]
    pub fn view(&self) -> EmpiricalStepTableRef<'_, Step> {
        EmpiricalStepTableRef {
            parameters: self.parameters,
            recent: &self.recent,
            all_history: &self.all_history,
        }
    }
}

/// Borrowed visible tables for one draw: the current fold state, or a
/// historical version rebuilt from the append-only history plus a saved
/// recent window.
#[derive(Clone, Copy)]
pub struct EmpiricalStepTableRef<'a, Step> {
    parameters: EmpiricalStepParameters,
    recent: &'a [Step],
    all_history: &'a [Step],
}

impl<'a, Step> EmpiricalStepTableRef<'a, Step> {
    /// Assemble a view from borrowed table slices.
    #[must_use]
    pub fn from_parts(
        parameters: EmpiricalStepParameters,
        recent: &'a [Step],
        all_history: &'a [Step],
    ) -> Self {
        Self {
            parameters,
            recent,
            all_history,
        }
    }

    /// Frequency-weighted mixed-table length.
    ///
    /// # Errors
    ///
    /// Returns an error if the registered weighted length overflows.
    pub fn mixed_len(&self) -> Result<usize, EmpiricalStepError> {
        self.recent
            .len()
            .checked_mul(self.parameters.recent_weight)
            .and_then(|recent| {
                self.all_history
                    .len()
                    .checked_mul(self.parameters.all_history_weight)
                    .and_then(|history| recent.checked_add(history))
            })
            .ok_or(EmpiricalStepError::TableLengthOverflow)
    }

    /// Resolve one frequency-weighted mixed-table index.
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
        let history_span = self
            .all_history
            .len()
            .checked_mul(self.parameters.all_history_weight)?;
        (history_index < history_span && !self.all_history.is_empty())
            .then(|| &self.all_history[history_index % self.all_history.len()])
    }
}

fn hash_tables<Step>(recent: &[Step], all_history: &[Step]) -> Result<String, EmpiricalStepError>
where
    Step: Serialize,
{
    let bytes =
        serde_json::to_vec(&(recent, all_history)).map_err(EmpiricalStepError::Serialization)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use serde::Serializer;

    use super::{EmpiricalStepParameters, EmpiricalStepTables};

    #[derive(Clone)]
    struct CountingStep {
        value: u8,
        serializations: Rc<Cell<usize>>,
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
    fn fold_keeps_recent_and_never_deletes_history() {
        let mut tables = EmpiricalStepTables::new(parameters()).expect("valid parameters");
        tables.fold_retained(&[0, 1, 2]).expect("first success");
        assert!(tables.finish_record().expect("first record").is_none());
        tables.fold_retained(&[0, 3]).expect("second success");
        assert!(tables.finish_record().expect("second record").is_some());
        tables.fold_retained(&[0, 4, 5]).expect("third success");
        tables.finish_record().expect("third record");
        tables.flush().expect("final update");
        assert_eq!(tables.recent(), &[3, 4, 5]);
        assert_eq!(tables.all_history(), &[1, 2, 3, 4, 5]);
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
    fn incremental_rule_folds_identically_and_reproducibly() {
        use super::{EmpiricalStepHashRule, EmpiricalStepTables as Tables};
        let run = || {
            let mut tables =
                Tables::with_hash_rule(parameters(), EmpiricalStepHashRule::IncrementalHistory)
                    .expect("valid parameters");
            for sequence in [vec![0_u8, 1, 2], vec![0, 3], vec![0, 4, 5]] {
                tables.fold_retained(&sequence).expect("fold success");
                let _ = tables.finish_record().expect("finish record");
            }
            tables.flush().expect("final update");
            tables
        };
        let first = run();
        let second = run();
        assert_eq!(first.recent(), &[3, 4, 5]);
        assert_eq!(first.all_history(), &[1, 2, 3, 4, 5]);
        assert_eq!(
            first.checkpoint().expect("first checkpoint"),
            second.checkpoint().expect("second checkpoint")
        );

        let mut full = EmpiricalStepTables::new(parameters()).expect("valid parameters");
        for sequence in [vec![0_u8, 1, 2], vec![0, 3], vec![0, 4, 5]] {
            full.fold_retained(&sequence).expect("fold success");
            let _ = full.finish_record().expect("finish record");
        }
        full.flush().expect("final update");
        assert_eq!(full.recent(), first.recent());
        assert_eq!(full.all_history(), first.all_history());
        assert_ne!(
            full.checkpoint()
                .expect("full-json checkpoint")
                .table_sha256,
            first
                .checkpoint()
                .expect("incremental checkpoint")
                .table_sha256,
        );
    }

    #[test]
    fn incremental_hash_ignores_history_reserialization() {
        use super::{EmpiricalStepHashRule, EmpiricalStepTables as Tables};
        let serializations = Rc::new(Cell::new(0));
        let step = |value| CountingStep {
            value,
            serializations: Rc::clone(&serializations),
        };
        let mut tables =
            Tables::with_hash_rule(parameters(), EmpiricalStepHashRule::IncrementalHistory)
                .expect("valid parameters");
        let flush_cost = |tables: &mut Tables<CountingStep>, value| {
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
        // Once the recent window is full, per-flush serialization work is
        // constant: history growth never re-enters the hash.
        assert_eq!(costs[2], costs[5]);
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
}
