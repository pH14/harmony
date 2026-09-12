// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded reporting-only replacement samples for equal-suffix experiments.

use super::{
    archive::MetroidArchiveKey,
    target::{ButtonChord, MetroidInput, MetroidMechanicalState, MetroidSnapshot},
};
#[cfg(feature = "metroid-complete-retention-audit")]
use crate::search::archive::ArchiveKey;
use crate::search::archive::{EntrySelectorCounters, RetentionObservation};
use serde::{Deserialize, Serialize};
use std::{error::Error, path::PathBuf};

const PER_STRATUM: usize = 16;
const STRATA: usize = 5;
const MAX_ACTIONS: usize = 8192;

// Appending a candidate suffix can leave Vec capacity above MAX_ACTIONS even
// when its length fits. Store a tightly sized payload so the declared bound
// covers retained action capacity; temporary reconstruction remains separate.
fn compact_input(input: MetroidInput) -> MetroidInput {
    MetroidInput {
        actions: input.actions.into_boxed_slice().into_vec(),
    }
}

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
    /// Complete local-rule competition when requested and fully reconstructable.
    #[cfg(feature = "metroid-complete-retention-audit")]
    #[serde(default)]
    pub complete_competition: Option<CompleteCompetition>,
}

/// All existing members of the sampled local slot, before global eviction.
#[cfg(feature = "metroid-complete-retention-audit")]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CompleteCompetition {
    /// Whether the local rule proposes admitting the candidate in the pair.
    pub candidate_admitted: bool,
    /// Every incumbent, including those proposed for removal.
    pub incumbents: Vec<CompleteIncumbent>,
}

/// A reconstructable incumbent in a complete local-rule competition.
#[cfg(feature = "metroid-complete-retention-audit")]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CompleteIncumbent {
    /// Stable stream entry id.
    pub id: u64,
    /// Optional opaque retention context.
    pub context: Option<u64>,
    /// Cached decoded endpoint.
    pub state: MetroidMechanicalState,
    /// Input from ordinary gameplay genesis.
    pub input: MetroidInput,
    /// Local proposal only; subsequent global eviction is outside this audit.
    pub retained_by_local_rule: bool,
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
    #[cfg(feature = "metroid-complete-retention-audit")]
    complete_missing_snapshot: u64,
    #[cfg(feature = "metroid-complete-retention-audit")]
    complete_oversized_input: u64,
    #[cfg(feature = "metroid-complete-retention-audit")]
    complete_unsupported_slot: u64,
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
            format: if cfg!(feature = "metroid-complete-retention-audit") {
                "metroid-retention-audit-v2-local-survivors"
            } else {
                "metroid-retention-audit-v1"
            },
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
                * if cfg!(feature = "metroid-complete-retention-audit") {
                    4
                } else {
                    2
                }
                * MAX_ACTIONS
                * std::mem::size_of::<ButtonChord>(),
            samples: (0..STRATA)
                .map(|_| Vec::with_capacity(PER_STRATUM))
                .collect(),
            #[cfg(feature = "metroid-complete-retention-audit")]
            complete_missing_snapshot: 0,
            #[cfg(feature = "metroid-complete-retention-audit")]
            complete_oversized_input: 0,
            #[cfg(feature = "metroid-complete-retention-audit")]
            complete_unsupported_slot: 0,
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
                candidate_input: compact_input(candidate_input),
                incumbent_input: compact_input(incumbent_input),
                #[cfg(feature = "metroid-complete-retention-audit")]
                complete_competition: self.complete_competition(event)?,
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

    #[cfg(feature = "metroid-complete-retention-audit")]
    fn complete_competition(
        &mut self,
        event: &RetentionObservation<'_, ButtonChord, MetroidArchiveKey, MetroidSnapshot>,
    ) -> Result<Option<CompleteCompetition>, Box<dyn Error>> {
        if !(1..=2).contains(&event.slot_member_count) {
            self.complete_unsupported_slot += 1;
            return Ok(None);
        }
        let members = (event.slot_members)()?;
        self.input_reconstructions += members.len() as u64;
        if members.iter().any(|member| member.snapshot.is_none()) {
            self.complete_missing_snapshot += 1;
            return Ok(None);
        }
        if members
            .iter()
            .any(|member| member.input.actions.len() > MAX_ACTIONS)
        {
            self.complete_oversized_input += 1;
            return Ok(None);
        }
        Ok(Some(CompleteCompetition {
            candidate_admitted: event.candidate_admitted,
            incumbents: members
                .into_iter()
                .map(|member| CompleteIncumbent {
                    id: member.id,
                    context: member.key.retention_context(),
                    state: member.snapshot.expect("all snapshots checked").state(),
                    input: compact_input(member.input),
                    retained_by_local_rule: member.retained_by_local_rule,
                })
                .collect(),
        }))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_action_capacity_is_bounded_without_changing_serialized_inputs() {
        let mut actions = Vec::with_capacity(MAX_ACTIONS - 1);
        actions.resize(MAX_ACTIONS - 1, ButtonChord::new(0, 1));
        actions.push(ButtonChord::new(1, 2));
        assert_eq!(actions.len(), MAX_ACTIONS);
        assert!(actions.capacity() > MAX_ACTIONS);
        let input = MetroidInput { actions };
        let expected = serde_json::to_vec(&input).unwrap();
        let stored = compact_input(input);
        assert_eq!(stored.actions.capacity(), MAX_ACTIONS);
        assert_eq!(serde_json::to_vec(&stored).unwrap(), expected);
        assert_eq!(
            compact_input(MetroidInput {
                actions: Vec::new()
            })
            .actions
            .capacity(),
            0
        );
    }
}
