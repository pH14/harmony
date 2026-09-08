// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded reporting-only replacement samples for equal-suffix experiments.

use super::{
    archive::MetroidArchiveKey,
    target::{ButtonChord, MetroidInput, MetroidMechanicalState, MetroidSnapshot},
};
use crate::search::archive::{EntrySelectorCounters, RetentionObservation};
use serde::{Deserialize, Serialize};
use std::{error::Error, path::PathBuf};

const PER_STRATUM: usize = 16;
const STRATA: usize = 5;
const MAX_ACTIONS: usize = 8192;

/// One independently replayable pair, without emulator snapshots or ROM bytes.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ReplacementPair {
    /// Diagnostic category; categories are not search rewards.
    pub stratum: usize,
    /// Campaign execution at which the competition happened.
    pub execution: u64,
    /// True if the candidate replaced the incumbent.
    pub replaces: bool,
    /// Decoded candidate endpoint.
    pub candidate: MetroidMechanicalState,
    /// Decoded incumbent endpoint.
    pub incumbent: MetroidMechanicalState,
    /// Incumbent creation time.
    pub created_execution: u64,
    /// Incumbent selection exposure at competition time.
    pub exposure: EntrySelectorCounters,
    /// Whether it entered the selectable recency window.
    pub in_window_ever: bool,
    /// Candidate input from ordinary gameplay genesis.
    pub candidate_input: MetroidInput,
    /// Incumbent input from ordinary gameplay genesis.
    pub incumbent_input: MetroidInput,
}

#[derive(Serialize)]
pub(crate) struct RetentionAudit {
    #[serde(skip)]
    path: PathBuf,
    #[serde(skip)]
    finished: bool,
    format: &'static str,
    strata: [&'static str; STRATA],
    seen: [u64; STRATA],
    missing_snapshot: u64,
    retained_both: u64,
    oversized_input: u64,
    input_reconstructions: u64,
    diagnostic_action_capacity_bytes: usize,
    samples: Vec<Vec<ReplacementPair>>,
}

// An independent deterministic sampling hash, never the campaign RNG.
fn mix(mut value: u64) -> u64 {
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

impl RetentionAudit {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            finished: false,
            format: "metroid-retention-audit-v1",
            strata: [
                "different_equipment",
                "different_capacity",
                "resource_tradeoff",
                "equal_preference",
                "ordered_resources",
            ],
            seen: [0; STRATA],
            missing_snapshot: 0,
            retained_both: 0,
            oversized_input: 0,
            input_reconstructions: 0,
            diagnostic_action_capacity_bytes: STRATA
                * PER_STRATUM
                * 2
                * MAX_ACTIONS
                * std::mem::size_of::<ButtonChord>(),
            samples: (0..STRATA)
                .map(|_| Vec::with_capacity(PER_STRATUM))
                .collect(),
        }
    }

    pub(crate) fn observe(
        &mut self,
        event: &RetentionObservation<'_, ButtonChord, MetroidArchiveKey, MetroidSnapshot>,
    ) -> Result<(), Box<dyn Error>> {
        if self.finished {
            return Ok(());
        }
        if event.candidate_admitted && !event.replaces {
            self.retained_both += 1;
            return Ok(());
        }
        let Some(incumbent) = event.incumbent.1 else {
            self.missing_snapshot += 1;
            return Ok(());
        };
        let candidate = event.candidate.1.state();
        let incumbent = incumbent.state();
        let stratum = if candidate.equipment != incumbent.equipment
            || candidate.bosses != incumbent.bosses
        {
            0
        } else if (candidate.missile_capacity, candidate.energy_tanks)
            != (incumbent.missile_capacity, incumbent.energy_tanks)
        {
            1
        } else if (candidate.health > incumbent.health && candidate.missiles < incumbent.missiles)
            || (candidate.health < incumbent.health && candidate.missiles > incumbent.missiles)
        {
            2
        } else if (candidate.health, candidate.missiles) == (incumbent.health, incumbent.missiles) {
            3
        } else {
            4
        };
        self.seen[stratum] += 1;
        let seen = self.seen[stratum];
        let index = if seen <= PER_STRATUM as u64 {
            seen - 1
        } else {
            mix(seen ^ ((stratum as u64 + 1) << 56)) % seen
        };
        if index < PER_STRATUM as u64 {
            let (candidate_input, incumbent_input) = (event.inputs)()?;
            self.input_reconstructions += 2;
            if candidate_input.actions.len() > MAX_ACTIONS
                || incumbent_input.actions.len() > MAX_ACTIONS
            {
                self.oversized_input += 1;
                return Ok(());
            }
            let pair = ReplacementPair {
                stratum,
                execution: event.execution,
                replaces: event.replaces,
                candidate,
                incumbent,
                created_execution: event.created_execution,
                exposure: event.exposure,
                in_window_ever: event.in_window_ever,
                candidate_input,
                incumbent_input,
            };
            if (index as usize) < self.samples[stratum].len() {
                self.samples[stratum][index as usize] = pair;
            } else {
                self.samples[stratum].push(pair);
            }
            // Each update overwrites one bounded sample file, preserving partial evidence.
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&self) -> Result<(), Box<dyn Error>> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = self.path.with_extension("json.tmp");
        let mut writer = std::io::BufWriter::new(std::fs::File::create(&temporary)?);
        serde_json::to_writer(&mut writer, self)?;
        std::io::Write::flush(&mut writer)?;
        std::fs::rename(temporary, &self.path)?;
        Ok(())
    }

    pub(crate) fn finish(&mut self) -> Result<(), Box<dyn Error>> {
        self.finished = true;
        self.flush()
    }
}
