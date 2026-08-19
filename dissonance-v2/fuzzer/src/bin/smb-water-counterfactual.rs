// SPDX-License-Identifier: AGPL-3.0-or-later

//! Measure transfer from an earlier SMB water stretch into a later one.

use std::{
    collections::{BTreeMap, VecDeque},
    env,
    error::Error,
    fs,
    io::{BufReader, Read},
    path::{Path, PathBuf},
};

use fuzzer::phase4b::{ButtonChord, SmbInput};
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor},
};

const RECENT_SUCCESSES: usize = 128;
const PARTS_PER_BILLION: u128 = 1_000_000_000;
const PARTS_PER_MILLION: u128 = 1_000_000;
const RANDOM_COMMON_DENOMINATOR: u128 = 5_500;

#[derive(Debug, Default)]
struct FoldStats {
    entries_examined: u64,
    entries_used: u64,
    chords: u64,
    counts: BTreeMap<ButtonChord, u64>,
    recent_sequences: VecDeque<Vec<ButtonChord>>,
    recent_chords: u64,
    recent_counts: BTreeMap<ButtonChord, u64>,
}

impl FoldStats {
    fn fold(&mut self, suffix: &[ButtonChord]) {
        self.entries_used = self.entries_used.saturating_add(1);
        self.chords = self
            .chords
            .saturating_add(u64::try_from(suffix.len()).unwrap_or(u64::MAX));
        for &chord in suffix {
            *self.counts.entry(chord).or_insert(0) += 1;
            *self.recent_counts.entry(chord).or_insert(0) += 1;
        }
        self.recent_chords = self
            .recent_chords
            .saturating_add(u64::try_from(suffix.len()).unwrap_or(u64::MAX));
        self.recent_sequences.push_back(suffix.to_vec());
        while self.recent_sequences.len() > RECENT_SUCCESSES {
            if let Some(removed) = self.recent_sequences.pop_front() {
                self.recent_chords = self
                    .recent_chords
                    .saturating_sub(u64::try_from(removed.len()).unwrap_or(u64::MAX));
                for chord in removed {
                    if let Some(count) = self.recent_counts.get_mut(&chord) {
                        *count = count.saturating_sub(1);
                        if *count == 0 {
                            self.recent_counts.remove(&chord);
                        }
                    }
                }
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct MinimalEntry {
    created_execution: u64,
    input: SmbInput,
    key: MinimalKey,
}

#[derive(Clone, Copy, Debug, Deserialize)]
struct MinimalKey {
    world: u8,
    level: u8,
    progress: u16,
}

struct ArchiveSeed<'a> {
    stats: &'a mut FoldStats,
    filter: MinimalKey,
    prefix_steps: usize,
    run_only: bool,
}

impl<'de> DeserializeSeed<'de> for ArchiveSeed<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(ArchiveVisitor {
            stats: self.stats,
            filter: self.filter,
            prefix_steps: self.prefix_steps,
            run_only: self.run_only,
        })
    }
}

struct ArchiveVisitor<'a> {
    stats: &'a mut FoldStats,
    filter: MinimalKey,
    prefix_steps: usize,
    run_only: bool,
}

impl<'de> Visitor<'de> for ArchiveVisitor<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("an SMB archive report")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        while let Some(field) = map.next_key::<String>()? {
            if field == "entries" {
                map.next_value_seed(EntriesSeed {
                    stats: self.stats,
                    filter: self.filter,
                    prefix_steps: self.prefix_steps,
                    run_only: self.run_only,
                })?;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(())
    }
}

struct EntriesSeed<'a> {
    stats: &'a mut FoldStats,
    filter: MinimalKey,
    prefix_steps: usize,
    run_only: bool,
}

impl<'de> DeserializeSeed<'de> for EntriesSeed<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(EntriesVisitor {
            stats: self.stats,
            filter: self.filter,
            prefix_steps: self.prefix_steps,
            run_only: self.run_only,
        })
    }
}

struct EntriesVisitor<'a> {
    stats: &'a mut FoldStats,
    filter: MinimalKey,
    prefix_steps: usize,
    run_only: bool,
}

impl<'de> Visitor<'de> for EntriesVisitor<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("the SMB archive entry array")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while let Some(entry) = sequence.next_element::<MinimalEntry>()? {
            self.stats.entries_examined = self.stats.entries_examined.saturating_add(1);
            if self.run_only && entry.created_execution == 0 {
                continue;
            }
            if (entry.key.world, entry.key.level) != (self.filter.world, self.filter.level)
                || entry.key.progress < self.filter.progress
            {
                continue;
            }
            if let Some(suffix) = entry.input.actions.get(self.prefix_steps..)
                && !suffix.is_empty()
            {
                self.stats.fold(suffix);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
struct Alignment {
    parts_per_billion: u64,
    speedup_parts_per_million: u64,
}

#[derive(Debug, Serialize)]
struct MeasurementReport {
    source_archive: PathBuf,
    target_archive: PathBuf,
    world: u8,
    level: u8,
    minimum_progress: u16,
    prefix_steps: usize,
    recent_successes: usize,
    source_entries_examined: u64,
    source_entries_used: u64,
    source_all_history_chords: u64,
    source_recent_chords: u64,
    target_entries_examined: u64,
    target_run_entries_used: u64,
    target_run_chords: u64,
    random_alignment_parts_per_billion: u64,
    all_history: Alignment,
    recent: Alignment,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let source_archive = PathBuf::from(args.next().ok_or(
        "usage: smb-water-counterfactual <source-archive> <target-archive> <world> <level> <minimum-progress> <prefix-steps> <output.json>",
    )?);
    let target_archive = PathBuf::from(args.next().ok_or("missing target archive")?);
    let filter = MinimalKey {
        world: parse_next(&mut args, "world")?,
        level: parse_next(&mut args, "level")?,
        progress: parse_next(&mut args, "minimum progress")?,
    };
    let prefix_steps = parse_next(&mut args, "prefix steps")?;
    let output = PathBuf::from(args.next().ok_or("missing output path")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }

    let source = scan_archive(&source_archive, filter, prefix_steps, false)?;
    let target = scan_archive(&target_archive, filter, prefix_steps, true)?;
    if source.chords == 0 || source.recent_chords == 0 || target.chords == 0 {
        return Err("counterfactual has an empty source, recent, or target chord table".into());
    }

    let random_numerator = target.counts.iter().fold(0_u128, |total, (chord, count)| {
        total.saturating_add(u128::from(*count).saturating_mul(random_weight(*chord)))
    });
    let random_denominator = u128::from(target.chords).saturating_mul(RANDOM_COMMON_DENOMINATOR);
    let random_ppb = ratio_scaled(random_numerator, random_denominator, PARTS_PER_BILLION);
    let all_history = alignment(
        &source.counts,
        source.chords,
        &target,
        random_numerator,
        random_denominator,
    );
    let recent = alignment(
        &source.recent_counts,
        source.recent_chords,
        &target,
        random_numerator,
        random_denominator,
    );
    let report = MeasurementReport {
        source_archive,
        target_archive,
        world: filter.world,
        level: filter.level,
        minimum_progress: filter.progress,
        prefix_steps,
        recent_successes: RECENT_SUCCESSES,
        source_entries_examined: source.entries_examined,
        source_entries_used: source.entries_used,
        source_all_history_chords: source.chords,
        source_recent_chords: source.recent_chords,
        target_entries_examined: target.entries_examined,
        target_run_entries_used: target.entries_used,
        target_run_chords: target.chords,
        random_alignment_parts_per_billion: random_ppb,
        all_history,
        recent,
    };
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn scan_archive(
    path: &Path,
    filter: MinimalKey,
    prefix_steps: usize,
    run_only: bool,
) -> Result<FoldStats, Box<dyn Error>> {
    let input = fs::File::open(path)?;
    scan_reader(BufReader::new(input), filter, prefix_steps, run_only)
}

fn scan_reader(
    input: impl Read,
    filter: MinimalKey,
    prefix_steps: usize,
    run_only: bool,
) -> Result<FoldStats, Box<dyn Error>> {
    let mut deserializer = serde_json::Deserializer::from_reader(input);
    let mut stats = FoldStats::default();
    ArchiveSeed {
        stats: &mut stats,
        filter,
        prefix_steps,
        run_only,
    }
    .deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(stats)
}

fn alignment(
    source: &BTreeMap<ButtonChord, u64>,
    source_chords: u64,
    target: &FoldStats,
    random_numerator: u128,
    random_denominator: u128,
) -> Alignment {
    let numerator = target.counts.iter().fold(0_u128, |total, (chord, count)| {
        total.saturating_add(
            u128::from(*count).saturating_mul(u128::from(*source.get(chord).unwrap_or(&0))),
        )
    });
    let denominator = u128::from(target.chords).saturating_mul(u128::from(source_chords));
    let parts_per_billion = ratio_scaled(numerator, denominator, PARTS_PER_BILLION);
    let speedup_numerator = numerator
        .saturating_mul(random_denominator)
        .saturating_mul(PARTS_PER_MILLION);
    let speedup_denominator = denominator.saturating_mul(random_numerator);
    Alignment {
        parts_per_billion,
        speedup_parts_per_million: u64::try_from(
            speedup_numerator
                .checked_div(speedup_denominator.max(1))
                .unwrap_or(u128::MAX),
        )
        .unwrap_or(u64::MAX),
    }
}

fn random_weight(chord: ButtonChord) -> u128 {
    const MASKS: [u8; 10] = [0x00, 0x01, 0x02, 0x40, 0x80, 0x81, 0x82, 0x83, 0x10, 0x20];
    if !MASKS.contains(&chord.buttons) {
        return 0;
    }
    match chord.hold_frames {
        2..=12 => 25,
        96..=120 => 11,
        _ => 0,
    }
}

fn ratio_scaled(numerator: u128, denominator: u128, scale: u128) -> u64 {
    u64::try_from(
        numerator
            .saturating_mul(scale)
            .checked_div(denominator.max(1))
            .unwrap_or(u128::MAX),
    )
    .unwrap_or(u64::MAX)
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

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{MinimalKey, RANDOM_COMMON_DENOMINATOR, random_weight, scan_reader};
    use fuzzer::phase4b::ButtonChord;

    #[test]
    fn stratified_random_weights_form_one_distribution() {
        let masks = [0x00, 0x01, 0x02, 0x40, 0x80, 0x81, 0x82, 0x83, 0x10, 0x20];
        let total = masks
            .iter()
            .flat_map(|&buttons| (1..=120).map(move |hold| ButtonChord::new(buttons, hold)))
            .map(random_weight)
            .sum::<u128>();
        assert_eq!(total, RANDOM_COMMON_DENOMINATOR);
    }

    #[test]
    fn streaming_fold_filters_pair_prefix_and_bootstrap() {
        let archive = br#"{
            "ignored": [1, 2, 3],
            "entries": [
                {"created_execution":0,"input":{"actions":[{"buttons":1,"hold_frames":2},{"buttons":129,"hold_frames":96}]},"key":{"world":6,"level":1,"progress":27}},
                {"created_execution":7,"input":{"actions":[{"buttons":1,"hold_frames":2},{"buttons":130,"hold_frames":97}]},"key":{"world":6,"level":1,"progress":28}},
                {"created_execution":8,"input":{"actions":[{"buttons":1,"hold_frames":2},{"buttons":131,"hold_frames":98}]},"key":{"world":6,"level":2,"progress":1}}
            ]
        }"#;
        let stats = scan_reader(
            Cursor::new(archive),
            MinimalKey {
                world: 6,
                level: 1,
                progress: 0,
            },
            1,
            true,
        )
        .expect("streaming fold");
        assert_eq!(stats.entries_examined, 3);
        assert_eq!(stats.entries_used, 1);
        assert_eq!(stats.chords, 1);
        assert_eq!(stats.counts.get(&ButtonChord::new(130, 97)), Some(&1));
    }
}
