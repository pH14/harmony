// SPDX-License-Identifier: AGPL-3.0-or-later

//! Score work-RAM bytes as archive-key candidates from a recorded run.
//!
//! For every room in the archive, each byte is scored by how few distinct
//! values it takes, how rarely it changes from parent to child, and how well
//! it separates productive parents from barren ones inside one selection
//! cell. The report is evidence for an inferred key; nothing here runs in
//! the search.

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    error::Error,
    fs,
    io::BufReader,
    path::PathBuf,
};

use searcher::{
    search::archive::ArchiveKey,
    smb::archive::{SmbArchiveKey, SmbArchiveReport},
    smb::campaign::SmbSnapshotCheckpoint,
    smb::target::{SmbTarget, WRAM_SIZE},
    target::Target,
};
use sha2::{Digest, Sha256};

/// Selections before an entry whose subtree stayed near counts as barren.
const BARREN_SELECTIONS: u64 = 2;
/// Progress a subtree must gain past its root to count as productive.
const FAR_PROGRESS: u16 = 32;

struct Labeled {
    cell: SmbArchiveKey,
    parent: Option<usize>,
    productive: bool,
    barren: bool,
    wram: Box<[u8; WRAM_SIZE]>,
}

/// Furthest point any descendant of an entry reached.
#[derive(Clone, Copy, Ord, PartialOrd, Eq, PartialEq)]
struct Reach {
    world: u8,
    level: u8,
    progress: u16,
}

impl Reach {
    fn of(key: &SmbArchiveKey) -> Self {
        Self {
            world: key.world,
            level: key.level,
            progress: key.progress,
        }
    }

    fn far_past(self, root: Self) -> bool {
        (self.world, self.level) > (root.world, root.level)
            || self.progress >= root.progress.saturating_add(FAR_PROGRESS)
    }
}

struct ByteScore {
    distinct: usize,
    changes: u64,
    pairs: u64,
    separation_weight: f64,
    separation: f64,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let run_dir = PathBuf::from(args.next().ok_or("usage: key-bytes <run-dir> [top]")?);
    let top: usize = args
        .next()
        .map(|value| value.to_string_lossy().parse())
        .transpose()?
        .unwrap_or(24);
    let rom = fs::read(env::var_os("HARMONY_SMB_ROM").ok_or("HARMONY_SMB_ROM is not set")?)?;
    let core_path = PathBuf::from(
        env::var_os("HARMONY_QUICKNES_CORE").ok_or("HARMONY_QUICKNES_CORE is not set")?,
    );
    let core_sha256 = format!("{:x}", Sha256::digest(fs::read(&core_path)?));
    let mut target = SmbTarget::from_smb_rom_bytes_headless(&rom, &core_path, &core_sha256)?;

    let report: SmbArchiveReport = serde_json::from_reader(BufReader::new(fs::File::open(
        run_dir.join("archive-live.json"),
    )?))?;
    let mut scratch = vec![0_u8; 1 << 20];
    let (snapshots, _): (SmbSnapshotCheckpoint, _) = postcard::from_io((
        BufReader::new(fs::File::open(run_dir.join("snapshots-live.bin"))?),
        scratch.as_mut_slice(),
    ))?;
    eprintln!(
        "entries {} snapshots {}",
        report.entries.len(),
        snapshots.entries.len()
    );

    let mut wram_by_id: BTreeMap<u64, Box<[u8; WRAM_SIZE]>> = BTreeMap::new();
    for entry in &snapshots.entries {
        target.restore(&entry.snapshot)?;
        wram_by_id.insert(entry.id, Box::new(target.wram()));
    }
    drop(snapshots);

    // Entries are in archive order, so every parent precedes its children and
    // a reverse pass folds each subtree's furthest point into its root.
    let mut position_by_id: BTreeMap<u64, usize> = BTreeMap::new();
    for (position, entry) in report.entries.iter().enumerate() {
        position_by_id.insert(entry.id, position);
    }
    let mut reach: Vec<Reach> = report
        .entries
        .iter()
        .map(|entry| Reach::of(&entry.key))
        .collect();
    for position in (0..report.entries.len()).rev() {
        let Some(parent) = report.entries[position]
            .parent_id
            .and_then(|parent| position_by_id.get(&parent).copied())
        else {
            continue;
        };
        reach[parent] = reach[parent].max(reach[position]);
    }

    // Rooms hold entries in archive order; parents are resolved inside the room.
    let mut rooms: BTreeMap<SmbArchiveKey, Vec<Labeled>> = BTreeMap::new();
    let mut index_by_id: BTreeMap<u64, (SmbArchiveKey, usize)> = BTreeMap::new();
    for (position, entry) in report.entries.iter().enumerate() {
        let Some(wram) = wram_by_id.remove(&entry.id) else {
            continue;
        };
        let room = entry.key.group(3);
        let parent = entry
            .parent_id
            .and_then(|parent| index_by_id.get(&parent))
            .filter(|(parent_room, _)| *parent_room == room)
            .map(|(_, index)| *index);
        let selected = entry
            .selector
            .as_ref()
            .map_or(0, |counters| counters.selected);
        let productive = reach[position].far_past(Reach::of(&entry.key));
        let list = rooms.entry(room).or_default();
        index_by_id.insert(entry.id, (room, list.len()));
        list.push(Labeled {
            cell: entry.key.group(1),
            parent,
            productive,
            barren: !productive && selected >= BARREN_SELECTIONS,
            wram,
        });
    }

    for (room, entries) in &rooms {
        let productive = entries.iter().filter(|entry| entry.productive).count();
        let barren = entries.iter().filter(|entry| entry.barren).count();
        println!(
            "\n== room {}-{} area {:02x}{:02x}/{} entries {} productive {} barren {}",
            room.world,
            room.level,
            room.room[0],
            room.room[1],
            room.room[2],
            entries.len(),
            productive,
            barren
        );
        let scores = score_room(entries);
        let mut ranked: Vec<(usize, &ByteScore)> = scores.iter().enumerate().collect();
        ranked.sort_by(|(a_offset, a), (b_offset, b)| {
            rank_key(b)
                .partial_cmp(&rank_key(a))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a_offset.cmp(b_offset))
        });
        println!("  rank offset distinct change_rate separation weight");
        for (rank, (offset, score)) in ranked.iter().enumerate() {
            let watched = matches!(*offset, 0x06d9 | 0x06da);
            if rank < top || watched {
                println!(
                    "  {:4} ${:04x} {:8} {:11.4} {:10.4} {:6.0}{}",
                    rank + 1,
                    offset,
                    score.distinct,
                    change_rate(score),
                    score.separation,
                    score.separation_weight,
                    if watched { " <- loop check" } else { "" }
                );
            }
        }
    }
    Ok(())
}

fn change_rate(score: &ByteScore) -> f64 {
    if score.pairs == 0 {
        0.0
    } else {
        score.changes as f64 / score.pairs as f64
    }
}

/// Separation carries the ranking; bytes that change on most steps or take a
/// value per entry are timers and positions the key already covers.
fn rank_key(score: &ByteScore) -> f64 {
    if score.distinct < 2 || score.distinct > 32 || change_rate(score) > 0.25 {
        return -1.0;
    }
    score.separation
}

fn score_room(entries: &[Labeled]) -> Vec<ByteScore> {
    let mut scores: Vec<ByteScore> = (0..WRAM_SIZE)
        .map(|_| ByteScore {
            distinct: 0,
            changes: 0,
            pairs: 0,
            separation_weight: 0.0,
            separation: 0.0,
        })
        .collect();
    let mut values: Vec<BTreeSet<u8>> = vec![BTreeSet::new(); WRAM_SIZE];
    for entry in entries {
        for (offset, byte) in entry.wram.iter().enumerate() {
            values[offset].insert(*byte);
        }
        if let Some(parent) = entry.parent {
            let parent = &entries[parent];
            for (score, (before, after)) in scores
                .iter_mut()
                .zip(parent.wram.iter().zip(entry.wram.iter()))
            {
                score.pairs += 1;
                if before != after {
                    score.changes += 1;
                }
            }
        }
    }
    for (offset, set) in values.iter().enumerate() {
        scores[offset].distinct = set.len();
    }

    // Within one selection cell, compare the value distribution of productive
    // entries against barren ones; total variation distance, weighted by the
    // smaller side so one-sided cells do not dominate.
    let mut cells: BTreeMap<SmbArchiveKey, Vec<usize>> = BTreeMap::new();
    for (index, entry) in entries.iter().enumerate() {
        if entry.productive || entry.barren {
            cells.entry(entry.cell).or_default().push(index);
        }
    }
    let mut weighted: Vec<f64> = vec![0.0; WRAM_SIZE];
    let mut weights: Vec<f64> = vec![0.0; WRAM_SIZE];
    for members in cells.values() {
        let productive: Vec<usize> = members
            .iter()
            .copied()
            .filter(|index| entries[*index].productive)
            .collect();
        let barren: Vec<usize> = members
            .iter()
            .copied()
            .filter(|index| entries[*index].barren)
            .collect();
        if productive.is_empty() || barren.is_empty() {
            continue;
        }
        let weight = productive.len().min(barren.len()) as f64;
        for offset in 0..WRAM_SIZE {
            let mut p = [0_u32; 256];
            let mut b = [0_u32; 256];
            for index in &productive {
                p[usize::from(entries[*index].wram[offset])] += 1;
            }
            for index in &barren {
                b[usize::from(entries[*index].wram[offset])] += 1;
            }
            let distance: f64 = (0..256)
                .map(|value| {
                    (f64::from(p[value]) / productive.len() as f64
                        - f64::from(b[value]) / barren.len() as f64)
                        .abs()
                })
                .sum::<f64>()
                / 2.0;
            weighted[offset] += distance * weight;
            weights[offset] += weight;
        }
    }
    for offset in 0..WRAM_SIZE {
        scores[offset].separation_weight = weights[offset];
        scores[offset].separation = if weights[offset] > 0.0 {
            weighted[offset] / weights[offset]
        } else {
            0.0
        };
    }
    scores
}
