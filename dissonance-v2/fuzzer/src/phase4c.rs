// SPDX-License-Identifier: AGPL-3.0-or-later

//! Deterministic snapshot-backed quality-diversity search for SMB completion.

use std::{collections::BTreeMap, error::Error, num::NonZeroUsize};

use libafl::executors::ExitKind;
use libafl_bolts::rands::{Rand, StdRand};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    phase4b::{
        ButtonChord, MAX_SMB_ACTIONS, SmbInput, SmbMilestoneInputs, SmbMilestoneTimes,
        SmbMilestones, SmbSnapshot, SmbTarget, smb_mechanical_state_from_wram,
        smb_milestones_from_wram,
    },
    target::Target,
};

const MAX_ARCHIVE_ENTRIES: usize = 32_768;
const MAX_ENTRIES_PER_KEY: usize = 2;
const FRONTIER_WINDOW: usize = 128;
const STATE_FINGERPRINT_MASK: u8 = 0x3f;
const BUTTON_MASKS: [u8; 9] = [0x00, 0x01, 0x02, 0x40, 0x80, 0x81, 0x82, 0x83, 0x10];

/// One bounded quality-diversity cell for an action-boundary snapshot.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SmbArchiveKey {
    /// Mechanical world number.
    pub world: u8,
    /// Mechanical level number.
    pub level: u8,
    /// Current 16-pixel progress bucket.
    pub progress: u16,
    /// Coarse player vertical-position bucket.
    pub player_y_bucket: u8,
    /// Mechanical player engine state.
    pub player_engine_state: u8,
    /// Six-bit deterministic fingerprint of otherwise-hidden work RAM state.
    pub state_fingerprint: u8,
}

/// Serializable lineage and retention record for one archived testcase.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbArchiveEntryReport {
    /// Stable insertion-order archive identifier.
    pub id: u64,
    /// Archive parent selected for the suffix execution.
    pub parent_id: Option<u64>,
    /// Target execution that created the entry; zero denotes bootstrap.
    pub created_execution: u64,
    /// Complete clean-reset input represented by this snapshot.
    pub input: SmbInput,
    /// Route-agnostic quality-diversity key.
    pub key: SmbArchiveKey,
    /// Strongest milestones observed along this input.
    pub milestones: SmbMilestones,
}

/// Deterministic progress sample from one archive campaign.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbArchiveProgressPoint {
    /// Completed target executions.
    pub executions: u64,
    /// Strongest milestone state observed so far.
    pub milestones: SmbMilestones,
    /// Number of active retained archive entries.
    pub active_entries: usize,
    /// Number of occupied quality-diversity cells.
    pub occupied_cells: usize,
    /// Number of terminal death transitions seen so far.
    pub deaths: u64,
}

/// Complete deterministic report for one snapshot-backed suffix-search campaign.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbArchiveReport {
    /// Caller-provided seeded RNG value.
    pub seed: u64,
    /// Number of suffix target executions.
    pub executions: u64,
    /// Strongest milestone values reached.
    pub milestones: SmbMilestones,
    /// First execution reaching each frozen milestone rung.
    pub first_reached: SmbMilestoneTimes,
    /// First clean-reset input reaching each rung.
    pub first_inputs: SmbMilestoneInputs,
    /// Current best clean-reset input.
    pub champion_input: SmbInput,
    /// Insertion and replacement records for retained testcases.
    pub entries: Vec<SmbArchiveEntryReport>,
    /// Fixed-interval deterministic progress curve.
    pub progress_curve: Vec<SmbArchiveProgressPoint>,
    /// Candidate snapshots admitted to the active archive.
    pub retained: u64,
    /// Candidate snapshots rejected by bounded quality-diversity retention.
    pub rejected: u64,
    /// Terminal death transitions observed.
    pub deaths: u64,
}

#[derive(Clone, Debug)]
struct ArchiveEntry {
    report: SmbArchiveEntryReport,
    snapshot: SmbSnapshot,
}

#[derive(Debug)]
struct Archive {
    entries: Vec<ArchiveEntry>,
    active: Vec<bool>,
    cells: BTreeMap<SmbArchiveKey, Vec<usize>>,
    input_ids: BTreeMap<SmbInput, usize>,
    retained: u64,
    rejected: u64,
}

impl Archive {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            active: Vec::new(),
            cells: BTreeMap::new(),
            input_ids: BTreeMap::new(),
            retained: 0,
            rejected: 0,
        }
    }

    fn insert(
        &mut self,
        parent_id: Option<usize>,
        execution: u64,
        input: SmbInput,
        key: SmbArchiveKey,
        milestones: SmbMilestones,
        snapshot: SmbSnapshot,
    ) -> Result<Option<usize>, Box<dyn Error>> {
        if let Some(existing) = self.input_ids.get(&input) {
            return Ok(Some(*existing));
        }
        let cell = self.cells.entry(key).or_default();
        let replace = if cell.len() < MAX_ENTRIES_PER_KEY {
            None
        } else {
            cell.iter()
                .copied()
                .max_by_key(|id| entry_cost(&self.entries[*id].report))
                .filter(|id| input.actions.len() < self.entries[*id].report.input.actions.len())
        };
        if cell.len() >= MAX_ENTRIES_PER_KEY && replace.is_none() {
            self.rejected = self.rejected.saturating_add(1);
            return Ok(None);
        }
        if self.entries.len() >= MAX_ARCHIVE_ENTRIES {
            self.rejected = self.rejected.saturating_add(1);
            return Ok(None);
        }
        if let Some(replaced) = replace {
            self.active[replaced] = false;
            cell.retain(|id| *id != replaced);
        }
        let id = self.entries.len();
        let report = SmbArchiveEntryReport {
            id: u64::try_from(id)?,
            parent_id: parent_id.map(u64::try_from).transpose()?,
            created_execution: execution,
            input: input.clone(),
            key,
            milestones,
        };
        self.entries.push(ArchiveEntry { report, snapshot });
        self.active.push(true);
        cell.push(id);
        self.input_ids.insert(input, id);
        self.retained = self.retained.saturating_add(1);
        Ok(Some(id))
    }

    fn active_ids(&self) -> Vec<usize> {
        self.active
            .iter()
            .enumerate()
            .filter_map(|(id, active)| {
                (*active && self.entries[id].report.input.actions.len() < MAX_SMB_ACTIONS)
                    .then_some(id)
            })
            .collect()
    }

    fn choose_parent(&self, rand: &mut StdRand) -> Result<usize, Box<dyn Error>> {
        let active = self.active_ids();
        if active.is_empty() {
            return Err("SMB archive has no expandable entry".into());
        }
        let use_frontier = rand.below(NonZeroUsize::new(4).ok_or("invalid frontier odds")?) != 0;
        if !use_frontier {
            return Ok(active[rand.below(NonZeroUsize::new(active.len()).ok_or("empty archive")?)]);
        }
        let mut ordered = active;
        ordered.sort_by_key(|id| {
            (
                milestone_key(self.entries[*id].report.milestones),
                self.entries[*id].report.key,
                self.entries[*id].report.id,
            )
        });
        let start = ordered.len().saturating_sub(FRONTIER_WINDOW);
        let frontier = &ordered[start..];
        Ok(frontier[rand.below(NonZeroUsize::new(frontier.len()).ok_or("empty frontier")?)])
    }
}

/// Run deterministic snapshot-backed short-horizon suffix search.
pub fn run_smb_archive_search(
    rom: &[u8],
    initial_inputs: &[SmbInput],
    seed: u64,
    execution_budget: u64,
) -> Result<SmbArchiveReport, Box<dyn Error>> {
    if initial_inputs.is_empty() {
        return Err("SMB archive search requires a nonempty initial corpus".into());
    }
    let mut target = SmbTarget::from_smb_rom_bytes_headless(rom)?;
    let mut archive = Archive::new();
    let mut aggregate = SmbMilestones::default();
    let mut first_reached = SmbMilestoneTimes::default();
    let mut first_inputs = SmbMilestoneInputs::default();
    let mut champion_input = SmbInput::default();
    let mut champion_milestones = SmbMilestones::default();

    target.reset();
    let genesis_key = archive_key(target.wram());
    let genesis_snapshot = target.snapshot().ok_or("failed to snapshot SMB genesis")?;
    let genesis_id = archive
        .insert(
            None,
            0,
            SmbInput::default(),
            genesis_key,
            SmbMilestones::default(),
            genesis_snapshot,
        )?
        .ok_or("failed to retain SMB genesis")?;
    for input in initial_inputs {
        target.reset();
        let mut prefix = SmbInput::default();
        let mut milestones = SmbMilestones::default();
        let mut parent_id = genesis_id;
        for action in &input.actions {
            if target.is_dead() {
                break;
            }
            prefix.actions.push(*action);
            target.apply(action);
            merge_action_milestones(&mut milestones, &target)?;
            merge_milestones(&mut aggregate, milestones);
            update_first_inputs(
                &mut first_reached,
                &mut first_inputs,
                milestones,
                0,
                &prefix,
            );
            if milestone_key(milestones) > milestone_key(champion_milestones) {
                champion_milestones = milestones;
                champion_input = prefix.clone();
            }
            if target.is_dead() || target.exit_kind() != ExitKind::Ok {
                break;
            }
            let snapshot = target
                .snapshot()
                .ok_or("failed to snapshot SMB bootstrap prefix")?;
            if let Some(id) = archive.insert(
                Some(parent_id),
                0,
                prefix.clone(),
                archive_key(target.wram()),
                milestones,
                snapshot,
            )? {
                parent_id = id;
            }
        }
    }

    let mut rand = StdRand::with_seed(seed);
    let mut curve = Vec::new();
    let mut deaths = 0_u64;
    for execution in 1..=execution_budget {
        let parent_id = archive.choose_parent(&mut rand)?;
        let parent = archive.entries[parent_id].clone();
        target.restore(&parent.snapshot)?;
        let mut input = parent.report.input.clone();
        let mut milestones = parent.report.milestones;
        let suffix_len = if rand.below(NonZeroUsize::new(4).ok_or("invalid suffix odds")?) == 0 {
            2
        } else {
            1
        };
        let mut current_parent = parent_id;
        for _ in 0..suffix_len {
            if target.is_dead() || input.actions.len() >= MAX_SMB_ACTIONS {
                break;
            }
            let action = sample_chord(&mut rand)?;
            input.actions.push(action);
            target.apply(&action);
            merge_action_milestones(&mut milestones, &target)?;
            merge_milestones(&mut aggregate, milestones);
            update_first_inputs(
                &mut first_reached,
                &mut first_inputs,
                milestones,
                execution,
                &input,
            );
            if milestone_key(milestones) > milestone_key(champion_milestones) {
                champion_milestones = milestones;
                champion_input = input.clone();
            }
            if target.is_dead() {
                deaths = deaths.saturating_add(1);
                break;
            }
            if target.exit_kind() != ExitKind::Ok {
                break;
            }
            let snapshot = target.snapshot().ok_or("failed to snapshot SMB suffix")?;
            if let Some(id) = archive.insert(
                Some(current_parent),
                execution,
                input.clone(),
                archive_key(target.wram()),
                milestones,
                snapshot,
            )? {
                current_parent = id;
            }
        }
        if execution % 100 == 0 || execution == execution_budget {
            curve.push(SmbArchiveProgressPoint {
                executions: execution,
                milestones: aggregate,
                active_entries: archive.active.iter().filter(|active| **active).count(),
                occupied_cells: archive.cells.len(),
                deaths,
            });
        }
    }

    Ok(SmbArchiveReport {
        seed,
        executions: execution_budget,
        milestones: aggregate,
        first_reached,
        first_inputs,
        champion_input,
        entries: archive
            .entries
            .into_iter()
            .map(|entry| entry.report)
            .collect(),
        progress_curve: curve,
        retained: archive.retained,
        rejected: archive.rejected,
        deaths,
    })
}

fn archive_key(wram: &[u8; 2_048]) -> SmbArchiveKey {
    let state = smb_mechanical_state_from_wram(wram);
    let digest = Sha256::digest(wram);
    SmbArchiveKey {
        world: state.world,
        level: state.level,
        progress: state.progress,
        player_y_bucket: state.player_y_bucket,
        player_engine_state: state.player_engine_state,
        state_fingerprint: digest[0] & STATE_FINGERPRINT_MASK,
    }
}

fn sample_chord(rand: &mut StdRand) -> Result<ButtonChord, Box<dyn Error>> {
    let buttons = BUTTON_MASKS
        [rand.below(NonZeroUsize::new(BUTTON_MASKS.len()).ok_or("empty SMB button vocabulary")?)];
    let hold_frames = if rand.below(NonZeroUsize::new(4).ok_or("invalid hold odds")?) != 0 {
        u8::try_from(2 + rand.below(NonZeroUsize::new(11).ok_or("invalid short hold span")?))?
    } else {
        u8::try_from(1 + rand.below(NonZeroUsize::new(120).ok_or("invalid hold span")?))?
    };
    Ok(ButtonChord::new(buttons, hold_frames))
}

fn merge_action_milestones(
    milestones: &mut SmbMilestones,
    target: &SmbTarget,
) -> Result<(), Box<dyn Error>> {
    for observation in target.last_action_observations() {
        let wram: &[u8; 2_048] = observation
            .wram
            .as_slice()
            .try_into()
            .map_err(|_| "SMB observation WRAM is not exactly 2 KiB")?;
        merge_milestones(milestones, smb_milestones_from_wram(wram));
    }
    Ok(())
}

fn merge_milestones(aggregate: &mut SmbMilestones, current: SmbMilestones) {
    aggregate.max_1_1_scroll_bucket = aggregate
        .max_1_1_scroll_bucket
        .max(current.max_1_1_scroll_bucket);
    aggregate.reached_1_1_flag |= current.reached_1_1_flag;
    aggregate.reached_1_2 |= current.reached_1_2;
    aggregate.reached_onward |= current.reached_onward;
}

fn update_first_inputs(
    times: &mut SmbMilestoneTimes,
    inputs: &mut SmbMilestoneInputs,
    current: SmbMilestones,
    execution: u64,
    input: &SmbInput,
) {
    if current.max_1_1_scroll_bucket > 0 {
        times.progress_into_1_1.get_or_insert(execution);
        inputs
            .progress_into_1_1
            .get_or_insert_with(|| input.clone());
    }
    if current.reached_1_1_flag {
        times.flag_1_1.get_or_insert(execution);
        inputs.flag_1_1.get_or_insert_with(|| input.clone());
    }
    if current.reached_1_2 {
        times.level_1_2.get_or_insert(execution);
        inputs.level_1_2.get_or_insert_with(|| input.clone());
    }
    if current.reached_onward {
        times.onward.get_or_insert(execution);
        inputs.onward.get_or_insert_with(|| input.clone());
    }
}

fn milestone_key(milestones: SmbMilestones) -> (bool, bool, bool, u16) {
    (
        milestones.reached_onward,
        milestones.reached_1_2,
        milestones.reached_1_1_flag,
        milestones.max_1_1_scroll_bucket,
    )
}

fn entry_cost(entry: &SmbArchiveEntryReport) -> (usize, u64) {
    (entry.input.actions.len(), entry.id)
}

#[cfg(test)]
mod tests {
    use super::run_smb_archive_search;
    use crate::phase4b::{MAX_SMB_ACTIONS, SmbInput};

    fn synthetic_nrom() -> Vec<u8> {
        let mut rom = vec![0_u8; 16 + (16 * 1024) + (8 * 1024)];
        rom[..16].copy_from_slice(&[b'N', b'E', b'S', 0x1a, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let prg = &mut rom[16..16 + (16 * 1024)];
        prg.fill(0xea);
        prg[..3].copy_from_slice(&[0x4c, 0x00, 0x80]);
        for vector in [0x3ffa, 0x3ffc, 0x3ffe] {
            prg[vector..vector + 2].copy_from_slice(&0x8000_u16.to_le_bytes());
        }
        rom
    }

    #[test]
    fn same_seed_archive_reports_match_and_stay_bounded() {
        let rom = synthetic_nrom();
        let initial = vec![SmbInput::default()];
        let first = run_smb_archive_search(&rom, &initial, 0x5eed_e000, 32)
            .expect("first archive campaign");
        let second = run_smb_archive_search(&rom, &initial, 0x5eed_e000, 32)
            .expect("second archive campaign");
        assert_eq!(first, second);
        assert_eq!(first.executions, 32);
        assert!(
            first
                .entries
                .iter()
                .all(|entry| entry.input.actions.len() <= MAX_SMB_ACTIONS)
        );
    }
}
