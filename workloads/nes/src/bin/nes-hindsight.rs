// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use nes_workload::{
    metroid::archive::MetroidArchiveKey,
    search::{
        archive::{ArchiveKey, Position, Slot},
        campaign::{CampaignAdmissionDecision, CampaignSpliceRecord, CampaignStreamRecord},
        empirical_steps::EmpiricalStepCheckpoint,
        labels::LabelGraph,
    },
    smb::archive::SmbArchiveKey,
};
use serde_json::{Value, json};

const PASS_RATIO: f64 = 0.8;
const FAIL_RATIO: f64 = 0.95;

type Stock<K> = (&'static str, fn(K) -> u64);

type Series = Vec<(u64, Vec<(u64, Option<u64>)>)>;

fn metroid_stocks() -> Vec<Stock<MetroidArchiveKey>> {
    vec![
        ("missiles", |key| u64::from(key.missiles)),
        ("health", |key| u64::from(key.health)),
        ("missile_capacity", |key| u64::from(key.missile_capacity)),
        ("energy_tanks", |key| u64::from(key.energy_tanks)),
    ]
}

fn smb_stocks() -> Vec<Stock<SmbArchiveKey>> {
    vec![("clock", |key| u64::from(key.clock))]
}

fn slot_of<K: ArchiveKey>(key: K) -> Slot<K> {
    ((key.progress(), key.place()), key.identity())
}

fn position_of<K: ArchiveKey>(key: K) -> Position<K> {
    (key.place(), key.identity())
}

struct Milestone<K: ArchiveKey> {
    name: String,
    first_seen: u64,
    position: Position<K>,
    arrival: K,
    series: Series,
}

struct Point {
    executions: u64,
    edges: usize,
    labels: usize,
    dominating: usize,
    holders: usize,
    examinations: usize,
}

fn named_first_seen(campaign: &Path) -> Result<BTreeMap<String, u64>, Box<dyn Error>> {
    let path = campaign.join("progress.jsonl");
    let Ok(text) = fs::read_to_string(&path) else {
        return Ok(BTreeMap::new());
    };
    let Some(last) = text.lines().rev().find(|line| !line.trim().is_empty()) else {
        return Ok(BTreeMap::new());
    };
    let value: Value = serde_json::from_str(last)?;
    let mut seen = BTreeMap::new();
    if let Some(first_seen) =
        value["workload_diagnostics"]["named_progress"]["first_seen"].as_object()
    {
        for (name, entry) in first_seen {
            if let Some(execution) = entry["execution"].as_u64() {
                seen.insert(name.clone(), execution);
            }
        }
    }
    Ok(seen)
}

fn checkpoint_executions(campaign: &Path) -> Result<BTreeSet<u64>, Box<dyn Error>> {
    let path = campaign.join("checkpoints").join("checkpoints.jsonl");
    let mut executions = BTreeSet::new();
    let Ok(text) = fs::read_to_string(&path) else {
        return Ok(executions);
    };
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let value: Value = serde_json::from_str(line)?;
        if let Some(execution) = value["executions"].as_u64() {
            executions.insert(execution);
        }
    }
    Ok(executions)
}

fn run<K: ArchiveKey>(
    game: &str,
    campaign: &Path,
    every: u64,
    stocks: &[Stock<K>],
) -> Result<Value, Box<dyn Error>> {
    let named = named_first_seen(campaign)?;
    let mut report_points = checkpoint_executions(campaign)?;
    let preferences = K::preferences().max(1);
    let mut graph: LabelGraph<(), K> = LabelGraph::new(usize::MAX);
    let mut holders: BTreeMap<Slot<K>, Vec<K>> = BTreeMap::new();
    let mut keys: BTreeMap<u64, K> = BTreeMap::new();
    let mut tiers_seen: BTreeSet<K::Progress> = BTreeSet::new();
    let mut milestones: Vec<Milestone<K>> = Vec::new();
    let mut points: Vec<Point> = Vec::new();
    let mut jobs = 0_u64;
    let mut retained = 0_u64;
    let mut transfers = 0_u64;
    let mut landed = 0_u64;
    let mut last_report = 0_u64;
    let mut last_sequence = 0_u64;
    let reader = BufReader::new(fs::File::open(campaign.join("stream.jsonl"))?);
    let mut lines = reader.lines();
    let _header = lines.next().transpose()?;
    let raisable = |_: Slot<K>| true;
    for line in lines {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let record: CampaignStreamRecord<EmpiricalStepCheckpoint, K> = serde_json::from_str(&line)?;
        let CampaignStreamRecord::Job(job) = record else {
            continue;
        };
        jobs += 1;
        last_sequence = job.sequence;
        let parent = keys.get(&job.parent_id).copied();
        let mut retained_here = job.retained_keys.iter().copied();
        let mut best_here: Option<K> = None;
        for decision in &job.decisions {
            let CampaignAdmissionDecision::Retained { id } = decision else {
                continue;
            };
            let Some(key) = retained_here.next() else {
                return Err(
                    format!("job {} lacks a key for retained entry {id}", job.sequence).into(),
                );
            };
            retained += 1;
            keys.insert(*id, key);
            if best_here
                .is_none_or(|best| key.preference_cmp(0, best) == std::cmp::Ordering::Greater)
            {
                best_here = Some(key);
            }
            if let Some(parent) = parent {
                graph.record(parent, key, job.parent_id, *id, &[], job.execution_work);
            }
            let slot = slot_of(key);
            let mut changed = None;
            match holders.get_mut(&slot) {
                Some(held) => {
                    for (preference, held) in held.iter_mut().enumerate() {
                        if key.preference_cmp(preference, *held) == std::cmp::Ordering::Greater {
                            *held = key;
                            changed.get_or_insert(preference);
                        }
                    }
                }
                None => {
                    holders.insert(slot, vec![key; preferences]);
                    changed = Some(0);
                }
            }
            graph.holder_changed(key, changed);
            if tiers_seen.insert(key.progress()) {
                milestones.push(Milestone {
                    name: format!("progress {}", serde_json::to_string(&key.progress())?),
                    first_seen: job.sequence,
                    position: position_of(key),
                    arrival: key,
                    series: Vec::new(),
                });
            }
        }
        if let (Some(best), Some((name, _))) = (
            best_here,
            named
                .iter()
                .find(|(_, execution)| **execution == job.sequence),
        ) {
            milestones.push(Milestone {
                name: name.clone(),
                first_seen: job.sequence,
                position: position_of(best),
                arrival: best,
                series: Vec::new(),
            });
        }
        if let Some(CampaignSpliceRecord::Tail {
            donor_id, leaf_id, ..
        }) = &job.splice
            && let (Some(donor), Some(leaf)) = (keys.get(donor_id), keys.get(leaf_id))
        {
            let arrived = job
                .retained_keys
                .iter()
                .any(|key| position_of(*key) == position_of(*leaf));
            transfers += 1;
            landed += u64::from(arrived);
            graph.transfer_attempted(position_of(*donor), position_of(*leaf), arrived);
        }
        let due = report_points.contains(&job.sequence) || job.sequence >= last_report + every;
        if due {
            last_report = job.sequence;
            report_points.remove(&job.sequence);
            report::<K>(
                &mut graph,
                &holders,
                &mut milestones,
                &mut points,
                job.sequence,
                stocks,
                &raisable,
            );
        }
    }
    if points
        .last()
        .is_none_or(|point| point.executions != last_sequence)
    {
        report::<K>(
            &mut graph,
            &holders,
            &mut milestones,
            &mut points,
            last_sequence,
            stocks,
            &raisable,
        );
    }
    let mut best_ratio: Option<f64> = None;
    let milestone_values: Vec<Value> = milestones
        .iter()
        .map(|milestone| {
            let fields: serde_json::Map<String, Value> = stocks
                .iter()
                .enumerate()
                .map(|(index, (name, stock))| {
                    let final_realized = milestone
                        .series
                        .iter()
                        .map(|(_, values)| values[index].0)
                        .max()
                        .unwrap_or(0);
                    let t_search = milestone
                        .series
                        .iter()
                        .find(|(_, values)| values[index].0 >= final_realized)
                        .map(|(executions, _)| *executions);
                    let t_label = milestone
                        .series
                        .iter()
                        .find(|(_, values)| {
                            values[index].1.is_some_and(|label| label >= final_realized)
                        })
                        .map(|(executions, _)| *executions);
                    let ratio = match (t_label, t_search) {
                        (Some(label), Some(search))
                            if search > 0 && final_realized > stock(milestone.arrival) =>
                        {
                            Some(label as f64 / search as f64)
                        }
                        _ => None,
                    };
                    if let Some(ratio) = ratio
                        && best_ratio.is_none_or(|best| ratio < best)
                    {
                        best_ratio = Some(ratio);
                    }
                    (
                        (*name).to_string(),
                        json!({
                            "arrived_with": stock(milestone.arrival),
                            "final_realized": final_realized,
                            "t_search": t_search,
                            "t_label": t_label,
                            "ratio": ratio,
                        }),
                    )
                })
                .collect();
            json!({
                "name": milestone.name,
                "first_seen": milestone.first_seen,
                "position": serde_json::to_value(milestone.position).unwrap_or(Value::Null),
                "fields": fields,
            })
        })
        .collect();
    let verdict = match best_ratio {
        Some(ratio) if ratio <= PASS_RATIO => "pass",
        Some(ratio) if ratio >= FAIL_RATIO => "fail",
        Some(_) => "undecided",
        None => "no_milestone_gained_stock",
    };
    Ok(json!({
        "format": "nes-hindsight-v1",
        "game": game,
        "campaign": campaign.display().to_string(),
        "jobs": jobs,
        "retained": retained,
        "continuation_transfers": transfers,
        "continuation_landed": landed,
        "edges": graph.edge_count(),
        "labels": graph.label_count(),
        "accounting": serde_json::to_value(graph.accounting())?,
        "points": points.iter().map(|point| json!({
            "executions": point.executions,
            "edges": point.edges,
            "labels": point.labels,
            "holders": point.holders,
            "dominating": point.dominating,
            "dominating_share": point.dominating as f64 / point.holders.max(1) as f64,
            "examinations": point.examinations,
        })).collect::<Vec<_>>(),
        "milestones": milestone_values,
        "best_ratio": best_ratio,
        "pass_ratio": PASS_RATIO,
        "fail_ratio": FAIL_RATIO,
        "verdict": verdict,
    }))
}

fn report<K: ArchiveKey>(
    graph: &mut LabelGraph<(), K>,
    holders: &BTreeMap<Slot<K>, Vec<K>>,
    milestones: &mut [Milestone<K>],
    points: &mut Vec<Point>,
    executions: u64,
    stocks: &[Stock<K>],
    raisable: &dyn Fn(Slot<K>) -> bool,
) {
    let best_holder = |slot: Slot<K>, preference: usize| -> Option<K> {
        holders
            .get(&slot)
            .map(|held| held[preference.min(held.len() - 1)])
    };
    let examinations = graph.relax_fully(&best_holder, raisable);
    let dominating = graph
        .labels()
        .filter(|(slot, _)| graph.dominating(**slot, 0, &best_holder).is_some())
        .count();
    points.push(Point {
        executions,
        edges: graph.edge_count(),
        labels: graph.label_count(),
        dominating,
        holders: holders.len(),
        examinations,
    });
    for milestone in milestones.iter_mut() {
        let values = stocks
            .iter()
            .map(|(_, stock)| {
                let realized = graph
                    .occupied_at(milestone.position)
                    .filter_map(|slot| holders.get(&slot))
                    .flat_map(|held| held.iter().map(|key| stock(*key)))
                    .max()
                    .unwrap_or(0);
                let label = graph
                    .occupied_at(milestone.position)
                    .filter_map(|slot| graph.label(slot))
                    .flat_map(|label| {
                        label
                            .entries()
                            .iter()
                            .flatten()
                            .map(|entry| stock(entry.key))
                    })
                    .max();
                (realized, label)
            })
            .collect();
        milestone.series.push((executions, values));
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !(2..=3).contains(&args.len()) {
        return Err("usage: nes-hindsight GAME CAMPAIGN_DIR [EVERY]".into());
    }
    let campaign = PathBuf::from(&args[1]);
    let every: u64 = args.get(2).map_or(Ok(250_000), |every| every.parse())?;
    let report = match args[0].as_str() {
        "metroid" => run("metroid", &campaign, every, &metroid_stocks())?,
        "smb" => run("smb", &campaign, every, &smb_stocks())?,
        other => return Err(format!("unsupported game {other}").into()),
    };
    println!("{report}");
    Ok(())
}
