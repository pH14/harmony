// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    path::Path,
};

use super::{CampaignCounters, CoreCheckpoint, InFlightJob, LiveCoordinatorProfile, Workload};
use crate::search::{
    archive::{Archive, EntrySelectorCounters, SelectorAccounting},
    checkpoint::{CheckpointHeader, CheckpointReader},
    duration::DurationPoliciesCheckpoint,
    rand::RomuDuoJrRand,
};

pub struct CheckpointEntry<K, S> {
    pub id: u64,
    pub key: K,
    pub active: bool,
    pub selector: EntrySelectorCounters,
    pub snapshot: Option<S>,
}

pub struct CheckpointInspection<K, S> {
    pub header: CheckpointHeader,
    pub sha256: String,
    pub selector: SelectorAccounting,
    pub entries: Vec<CheckpointEntry<K, S>>,
    pub historical_entries_dropped: u64,
}

pub fn inspect_checkpoint<G: Workload>(
    path: &Path,
) -> Result<CheckpointInspection<G::Key, G::Snapshot>, Box<dyn Error>> {
    let mut reader = CheckpointReader::open(path)?;
    let _: CoreCheckpoint<G> = reader.next()?;
    let archive: Archive<G::Action, G::Key, G::Milestones, G::Snapshot> = reader.next()?;
    let _: Vec<u8> = reader.next()?;
    let _: Vec<u8> = reader.next()?;
    let _: DurationPoliciesCheckpoint<G::Key> = reader.next()?;
    let _: RomuDuoJrRand = reader.next()?;
    let _: CampaignCounters = reader.next()?;
    let _: LiveCoordinatorProfile = reader.next()?;
    let in_flight: Vec<InFlightJob<G>> = reader.next()?;
    if reader.header.reason != "final" || !in_flight.is_empty() {
        return Err("inspection requires a final checkpoint with no in-flight jobs".into());
    }
    let ids: BTreeSet<_> = archive.entries.iter().map(|entry| entry.id).collect();
    if ids.len() != archive.entries.len() || archive.active.len() != archive.entries.len() {
        return Err("checkpoint archive identities or active flags are invalid".into());
    }
    let mut snapshots = BTreeMap::new();
    for (id, snapshot) in reader.snapshots::<G::Snapshot>()? {
        if !ids.contains(&id) || snapshots.insert(id, snapshot).is_some() {
            return Err("checkpoint has an unknown or duplicate snapshot entry".into());
        }
    }
    let header = reader.header.clone();
    let sha256 = reader.file_sha256.clone();
    reader.finish()?;
    let mut entries = Vec::with_capacity(archive.entries.len());
    for (index, entry) in archive.entries.iter().enumerate() {
        entries.push(CheckpointEntry {
            id: entry.id,
            key: entry.key,
            active: *archive
                .active
                .get(index)
                .ok_or("checkpoint active flags are missing")?,
            selector: archive
                .entry_selector_counters(index)
                .ok_or("checkpoint selector counters are missing")?,
            snapshot: snapshots.remove(&entry.id),
        });
    }
    Ok(CheckpointInspection {
        header,
        sha256,
        selector: archive.inspection_selector(),
        entries,
        historical_entries_dropped: archive.historical_entries_dropped(),
    })
}
