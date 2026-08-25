// SPDX-License-Identifier: AGPL-3.0-or-later

//! Classify every active archive entry of a recorded campaign by what its
//! selections produced — keepers, only rejected children, only dead
//! children, or never picked — and compute the selector's exact draw
//! shares with and without the barren classes skipped.
//!
//! Reads recorded artifacts only; no emulation. The active entry set is
//! reconstructed from the archive report by replaying the cell-displacement
//! rule in insertion order, which is deterministic. Draw shares are exact
//! probabilities of the recorded selector structure with fresh exhaustion
//! counters: one in four draws is uniform over active entries, the rest
//! walk room, band, and cell uniformly within the deepest (world, level)
//! pair that has an unexhausted entry.

use std::{
    collections::BTreeMap,
    env,
    error::Error,
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use searcher::smb::{
    archive::{SmbArchiveKey, SmbArchiveReport, SmbRoomIdentity},
    campaign::{SmbCampaignAdmissionDecision, SmbCampaignStreamHeader, SmbCampaignStreamRecord},
};
use serde::Serialize;
use sha2::{Digest, Sha256};

/// Progress-band width in buckets, matching the selector's band classes.
const BAND_WIDTH: u16 = 8;
/// Cell capacity, matching the archive's per-key entry bound.
const ENTRIES_PER_KEY: usize = 2;
/// Odds of the cell path, matching the selector's one-in-four uniform draw.
const CELL_PATH_SHARE: f64 = 0.75;

/// What an entry's recorded selections produced.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
enum EntryClass {
    /// At least one selection retained a child.
    Keepers,
    /// Selections produced admission decisions but never a retained child.
    AllRejected,
    /// Every selection ended with no candidate at all: the children died.
    AllDied,
    /// The entry was never selected.
    Unpicked,
}

#[derive(Serialize)]
struct ClassCounts {
    keepers: u64,
    all_rejected: u64,
    all_died: u64,
    unpicked: u64,
}

#[derive(Serialize)]
struct RoomShare {
    world: u8,
    level: u8,
    room: SmbRoomIdentity,
    band: u16,
    active_entries: u64,
    barren_entries: u64,
    progress_min: u16,
    progress_max: u16,
    baseline_share: f64,
    skip_barren_share: f64,
    retire_streak_share: f64,
}

#[derive(Serialize)]
struct Report {
    stream: PathBuf,
    archive: PathBuf,
    archive_sha256: String,
    active_entries: u64,
    displaced_entries: u64,
    /// Streak thresholds simulated, in entry, cell, band, room order.
    thresholds: [u64; 4],
    classes: ClassCounts,
    /// Class counts per (world, level, band), sorted by the key string.
    classes_by_pair_band: BTreeMap<String, ClassCounts>,
    /// Draw shares per (room, band) of the deepest pair, baseline and with
    /// the barren classes skipped.
    rooms: Vec<RoomShare>,
}

type RoomClass = (u8, u8, SmbRoomIdentity);

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let stream_path = PathBuf::from(args.next().ok_or(
        "usage: smb-energy-reach <stream.jsonl> <archive.json> \
         <entry-threshold> <cell-threshold> <band-threshold> <room-threshold> <output.json>",
    )?);
    let archive_path = PathBuf::from(args.next().ok_or("missing archive path")?);
    let thresholds: [u64; 4] = [
        parse_next(&mut args, "entry threshold")?,
        parse_next(&mut args, "cell threshold")?,
        parse_next(&mut args, "band threshold")?,
        parse_next(&mut args, "room threshold")?,
    ];
    let output = PathBuf::from(args.next().ok_or("missing output path")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }

    let archive_bytes = fs::read(&archive_path)?;
    let archive_sha256 = format!("{:x}", Sha256::digest(&archive_bytes));
    let archive: SmbArchiveReport = serde_json::from_slice(&archive_bytes)?;
    drop(archive_bytes);

    // Reconstruct frames-in-level and the active set by replaying the
    // displacement rule over entries in insertion order.
    let index_of: BTreeMap<u64, usize> = archive
        .entries
        .iter()
        .enumerate()
        .map(|(index, entry)| (entry.id, index))
        .collect();
    let mut frames_in_level = vec![0_u64; archive.entries.len()];
    let mut active = vec![true; archive.entries.len()];
    let mut cells = BTreeMap::<SmbArchiveKey, Vec<usize>>::new();
    let mut displaced_entries = 0_u64;
    for (index, entry) in archive.entries.iter().enumerate() {
        let frames = match entry.parent_id.and_then(|id| index_of.get(&id)) {
            Some(parent_index) => {
                let parent = &archive.entries[*parent_index];
                let added: u64 = entry.input.actions[parent.input.actions.len()..]
                    .iter()
                    .map(|action| u64::from(action.bounded_hold_frames()))
                    .sum();
                if (parent.key.world, parent.key.level) == (entry.key.world, entry.key.level) {
                    frames_in_level[*parent_index].saturating_add(added)
                } else {
                    added
                }
            }
            None => entry
                .input
                .actions
                .iter()
                .map(|action| u64::from(action.bounded_hold_frames()))
                .sum(),
        };
        frames_in_level[index] = frames;
        let cell = cells.entry(entry.key).or_default();
        if cell.len() >= ENTRIES_PER_KEY {
            let costliest = cell
                .iter()
                .copied()
                .max_by_key(|id| (frames_in_level[*id], archive.entries[*id].id));
            let Some(costliest) = costliest.filter(|id| frames < frames_in_level[*id]) else {
                return Err(format!(
                    "entry {} was retained into a full cell it cannot displace",
                    entry.id
                )
                .into());
            };
            active[costliest] = false;
            displaced_entries = displaced_entries.saturating_add(1);
            cell.retain(|id| *id != costliest);
        }
        cell.push(index);
    }

    // Classify every entry by its recorded selections, and track each
    // pooling level's trailing barren streak — consecutive picks since the
    // last retained child — which is the counter retirement would run.
    let mut picked = vec![false; archive.entries.len()];
    let mut kept = vec![false; archive.entries.len()];
    let mut saw_decision = vec![false; archive.entries.len()];
    let mut entry_streak = vec![0_u64; archive.entries.len()];
    let mut cell_streak = BTreeMap::<SmbArchiveKey, u64>::new();
    let mut band_streak = BTreeMap::<(RoomClass, u16), u64>::new();
    let mut room_streak = BTreeMap::<RoomClass, u64>::new();
    let cell_key_of = |key: &SmbArchiveKey| -> SmbArchiveKey {
        SmbArchiveKey {
            state_fingerprint: 0,
            ..*key
        }
    };
    let reader = BufReader::new(fs::File::open(&stream_path)?);
    let mut lines = reader.split(b'\n');
    let header_line = lines.next().ok_or("stream is empty")??;
    let header: SmbCampaignStreamHeader = serde_json::from_slice(&header_line)?;
    if header.parent_scheduler != "room_cell_uniform_128" {
        return Err(format!("unexpected parent scheduler {}", header.parent_scheduler).into());
    }
    for line in lines {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_slice::<SmbCampaignStreamRecord>(&line) else {
            break;
        };
        let (parent_id, decisions) = match &record {
            SmbCampaignStreamRecord::Job(job) => (job.parent_id, job.decisions.as_slice()),
            SmbCampaignStreamRecord::Skip(skip) => (skip.parent_id, [].as_slice()),
        };
        let Some(index) = index_of.get(&parent_id) else {
            continue;
        };
        picked[*index] = true;
        if !decisions.is_empty() {
            saw_decision[*index] = true;
        }
        let productive = decisions
            .iter()
            .any(|d| matches!(d, SmbCampaignAdmissionDecision::Retained { .. }));
        if productive {
            kept[*index] = true;
        }
        let key = archive.entries[*index].key;
        let room = (key.world, key.level, key.room);
        let band = (room, key.progress / BAND_WIDTH);
        let entry_slot = &mut entry_streak[*index];
        let cell_slot = cell_streak.entry(cell_key_of(&key)).or_insert(0);
        let band_slot = band_streak.entry(band).or_insert(0);
        let room_slot = room_streak.entry(room).or_insert(0);
        for slot in [entry_slot, cell_slot, band_slot, room_slot] {
            *slot = if productive {
                0
            } else {
                slot.saturating_add(1)
            };
        }
    }
    let class_of = |index: usize| -> EntryClass {
        if kept[index] {
            EntryClass::Keepers
        } else if saw_decision[index] {
            EntryClass::AllRejected
        } else if picked[index] {
            EntryClass::AllDied
        } else {
            EntryClass::Unpicked
        }
    };

    // Class counts, total and per (world, level, band).
    let mut classes = ClassCounts {
        keepers: 0,
        all_rejected: 0,
        all_died: 0,
        unpicked: 0,
    };
    let mut by_pair_band = BTreeMap::<(u8, u8, u16), ClassCounts>::new();
    for (index, entry) in archive.entries.iter().enumerate() {
        if !active[index] {
            continue;
        }
        let slot = by_pair_band
            .entry((
                entry.key.world,
                entry.key.level,
                entry.key.progress / BAND_WIDTH,
            ))
            .or_insert(ClassCounts {
                keepers: 0,
                all_rejected: 0,
                all_died: 0,
                unpicked: 0,
            });
        for target in [&mut classes, slot] {
            match class_of(index) {
                EntryClass::Keepers => target.keepers += 1,
                EntryClass::AllRejected => target.all_rejected += 1,
                EntryClass::AllDied => target.all_died += 1,
                EntryClass::Unpicked => target.unpicked += 1,
            }
        }
    }

    // Exact draw shares per (room, band). `live` marks the entries the cell
    // path may sample: all active ones at baseline, the non-barren ones in
    // the skip variant. The uniform quarter of draws ignores exhaustion, so
    // it is identical in both variants.
    let share_by_band = |live: &dyn Fn(usize) -> bool| -> BTreeMap<(RoomClass, u16), f64> {
        let mut shares = BTreeMap::<(RoomClass, u16), f64>::new();
        let total_active = active.iter().filter(|a| **a).count() as f64;
        for (index, entry) in archive.entries.iter().enumerate() {
            if active[index] {
                let room = (entry.key.world, entry.key.level, entry.key.room);
                let band = entry.key.progress / BAND_WIDTH;
                *shares.entry((room, band)).or_insert(0.0) +=
                    (1.0 - CELL_PATH_SHARE) / total_active;
            }
        }
        // The cell path: deepest pair with a live entry, room uniform, band
        // uniform within the room.
        let mut pairs = BTreeMap::<(u8, u8), BTreeMap<SmbRoomIdentity, Vec<usize>>>::new();
        for (index, entry) in archive.entries.iter().enumerate() {
            if active[index] && live(index) {
                pairs
                    .entry((entry.key.world, entry.key.level))
                    .or_default()
                    .entry(entry.key.room)
                    .or_default()
                    .push(index);
            }
        }
        if let Some(((world, level), rooms)) = pairs.into_iter().next_back() {
            let room_share = CELL_PATH_SHARE / rooms.len() as f64;
            for (room, members) in rooms {
                let mut bands = BTreeMap::<u16, u64>::new();
                for index in members {
                    *bands
                        .entry(archive.entries[index].key.progress / BAND_WIDTH)
                        .or_insert(0) += 1;
                }
                let band_share = room_share / bands.len() as f64;
                for band in bands.keys() {
                    *shares.entry(((world, level, room), *band)).or_insert(0.0) += band_share;
                }
            }
        }
        shares
    };
    let baseline = share_by_band(&|_index| true);
    let skip_barren = share_by_band(&|index| {
        matches!(class_of(index), EntryClass::Keepers | EntryClass::Unpicked)
    });
    // Retirement as built: an entry is skipped when its own trailing barren
    // streak, or any enclosing class's pooled streak, is at or over that
    // level's measured threshold.
    let retired = |index: usize| -> bool {
        let key = archive.entries[index].key;
        let room = (key.world, key.level, key.room);
        let band = (room, key.progress / BAND_WIDTH);
        entry_streak[index] >= thresholds[0]
            || cell_streak.get(&cell_key_of(&key)).copied().unwrap_or(0) >= thresholds[1]
            || band_streak.get(&band).copied().unwrap_or(0) >= thresholds[2]
            || room_streak.get(&room).copied().unwrap_or(0) >= thresholds[3]
    };
    let retire_streak = share_by_band(&|index| !retired(index));

    // Per-(room, band) statistics over active entries.
    let mut band_stats = BTreeMap::<(RoomClass, u16), (u64, u64, u16, u16)>::new();
    for (index, entry) in archive.entries.iter().enumerate() {
        if !active[index] {
            continue;
        }
        let room = (entry.key.world, entry.key.level, entry.key.room);
        let band = entry.key.progress / BAND_WIDTH;
        let slot = band_stats
            .entry((room, band))
            .or_insert((0, 0, u16::MAX, 0));
        slot.0 += 1;
        if !matches!(class_of(index), EntryClass::Keepers | EntryClass::Unpicked) {
            slot.1 += 1;
        }
        slot.2 = slot.2.min(entry.key.progress);
        slot.3 = slot.3.max(entry.key.progress);
    }
    let deepest_pair = archive
        .entries
        .iter()
        .enumerate()
        .filter(|(index, _)| active[*index])
        .map(|(_, entry)| (entry.key.world, entry.key.level))
        .max()
        .ok_or("archive has no active entry")?;
    let mut rooms = Vec::new();
    for ((room, band), (entries, barren, progress_min, progress_max)) in &band_stats {
        let baseline_share = baseline.get(&(*room, *band)).copied().unwrap_or(0.0);
        let skip_share = skip_barren.get(&(*room, *band)).copied().unwrap_or(0.0);
        let streak_share = retire_streak.get(&(*room, *band)).copied().unwrap_or(0.0);
        if (room.0, room.1) != deepest_pair
            && (baseline_share - skip_share).abs() < f64::EPSILON / 2.0
            && (baseline_share - streak_share).abs() < f64::EPSILON / 2.0
        {
            continue;
        }
        rooms.push(RoomShare {
            world: room.0,
            level: room.1,
            room: room.2,
            band: *band,
            active_entries: *entries,
            barren_entries: *barren,
            progress_min: *progress_min,
            progress_max: *progress_max,
            baseline_share,
            skip_barren_share: skip_share,
            retire_streak_share: streak_share,
        });
    }

    let classes_by_pair_band = by_pair_band
        .into_iter()
        .map(|((world, level, band), counts)| (format!("{world},{level},band{band}"), counts))
        .collect();
    let report = Report {
        stream: stream_path,
        archive: archive_path,
        archive_sha256,
        active_entries: active.iter().filter(|a| **a).count() as u64,
        displaced_entries,
        thresholds,
        classes,
        classes_by_pair_band,
        rooms,
    };
    create_parent(&output)?;
    fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    println!(
        "active {} displaced {} keepers {} all_rejected {} all_died {} unpicked {}",
        report.active_entries,
        report.displaced_entries,
        report.classes.keepers,
        report.classes.all_rejected,
        report.classes.all_died,
        report.classes.unpicked
    );
    for room in &report.rooms {
        println!(
            "({},{}) room {:?} band {} progress {}..{} entries {} barren {} \
             share {:.5} class-skip {:.5} streak-retire {:.5} ({:.2}x)",
            room.world,
            room.level,
            room.room,
            room.band,
            room.progress_min,
            room.progress_max,
            room.active_entries,
            room.barren_entries,
            room.baseline_share,
            room.skip_barren_share,
            room.retire_streak_share,
            if room.baseline_share > 0.0 {
                room.retire_streak_share / room.baseline_share
            } else {
                f64::INFINITY
            }
        );
    }
    Ok(())
}

fn parse_next<T>(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
    name: &str,
) -> Result<T, Box<dyn Error>>
where
    T: std::str::FromStr,
    T::Err: Error + 'static,
{
    Ok(args
        .next()
        .ok_or_else(|| format!("missing {name}"))?
        .to_string_lossy()
        .parse()?)
}

fn create_parent(path: &Path) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}
