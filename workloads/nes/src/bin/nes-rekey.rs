// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fs,
    path::Path,
};

use machine::nes::ButtonChord;
use nes_games::workload::{
    Game, Lowered, NesSnapshot, RECORDS_FILE, REPLAY_RECORDS_FILE, SNAPSHOT_CHECKPOINT_FORMAT,
};
use nes_workload::{famidash::Famidash, joust::Joust, zelda::Zelda};
use searcher::search::{
    assertion::{
        AssertionKey, RecordedAction, RecordedJob, Recording, Rekeying, StreamIdentity,
        same_decisions,
    },
    campaign::SnapshotCheckpoint,
};
use serde_json::{Value, json};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn chord_time(action: &ButtonChord) -> u64 {
    u64::from(action.bounded_hold_frames())
}

fn forced_move_frames<A>(actions: &[RecordedAction<A>]) -> BTreeMap<u64, usize> {
    let mut lengths = BTreeMap::new();
    for action in actions {
        let decoded = u64::try_from(action.frames.len()).unwrap_or(u64::MAX);
        if !action.failed && action.cost > decoded {
            *lengths.entry(action.cost - decoded + 1).or_default() += 1;
        }
    }
    lengths
}

fn rekey<G: Game>(game: G, run: &Path, strategy: Option<&str>) -> Result<Value> {
    let recording = Recording::<ButtonChord>::from_bytes(&fs::read(run.join(RECORDS_FILE))?)?;
    let recorded = Lowered::new(game.clone(), &recording.header.strategy)?;
    if recorded.catalog != recording.header.catalog {
        return Err("the records were written against another catalog".into());
    }
    let keyed = Lowered::new(game, strategy.unwrap_or(&recording.header.strategy))?;
    if !run.join("stream.jsonl").exists() {
        return Ok(json!({
            "recorded_policy": recording.header.policy,
            "records_jobs": recording.jobs,
            "records_truncated_bytes": recording.truncated_bytes,
            "actions": recording.actions.len(),
            "frames": recording.actions.iter().map(|action| action.frames.len()).sum::<usize>(),
            "cost": recording.actions.iter().map(|action| action.cost).sum::<u64>(),
            "forced_move_frames": forced_move_frames(&recording.actions),
        }));
    }
    let stream = fs::read_to_string(run.join("stream.jsonl"))?;
    let mut lines = stream.lines();
    let header: Value = serde_json::from_str(lines.next().ok_or("the stream is empty")?)?;
    recording.check_stream(
        &StreamIdentity {
            campaign_seed: header["campaign_seed"]
                .as_u64()
                .ok_or("the stream header has no campaign seed")?,
            workload_identity_sha256: header["workload_identity_sha256"]
                .as_str()
                .ok_or("the stream header has no workload identity")?
                .to_owned(),
        },
        header["key_policy"]
            .as_str()
            .ok_or("the stream header has no key policy")?,
    )?;
    let entry_limit = header["archive_entry_limit"]
        .as_u64()
        .and_then(|limit| usize::try_from(limit).ok())
        .ok_or("the stream header has no archive entry limit")?;
    let recorded_through = recording.actions.last().map_or(0, |action| action.sequence);
    let mut jobs = Vec::new();
    for line in lines {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            break;
        };
        if record["event"] == "job" {
            let job = serde_json::from_value::<RecordedJob>(record)?;
            if job.sequence > recorded_through {
                break;
            }
            jobs.push(job);
        }
    }
    let rekeyed = Rekeying {
        recorded: &recorded.compiled,
        strategy: &keyed.compiled,
        entry_limit,
    }
    .run(
        &recording.header.genesis,
        &jobs,
        &recording.actions,
        chord_time,
    )?;
    let same_policy = keyed.compiled.policy_identifier() == recording.header.policy;
    let mut report = json!({
        "recorded_policy": recording.header.policy,
        "policy": rekeyed.policy,
        "jobs": rekeyed.jobs,
        "records_jobs": recording.jobs,
        "records_truncated_bytes": recording.truncated_bytes,
        "actions": recording.actions.len(),
        "frames": recording.actions.iter().map(|action| action.frames.len()).sum::<usize>(),
        "cost": recording.actions.iter().map(|action| action.cost).sum::<u64>(),
        "forced_move_frames": forced_move_frames(&recording.actions),
        "candidates": rekeyed.candidates,
        "retained": rekeyed.retained,
        "rejected": rekeyed.rejected,
        "cells": rekeyed.cells,
        "entries": rekeyed.holders.iter().map(|held| held.ids.len()).sum::<usize>(),
        "tiers": rekeyed.tiers,
        "findings": rekeyed.findings,
        "holders": rekeyed.holders,
    });
    let replay = run.join(REPLAY_RECORDS_FILE);
    if replay.exists() {
        report["replay_records_identical"] =
            json!(fs::read(replay)? == fs::read(run.join(RECORDS_FILE))?);
    }
    if same_policy {
        report["jobs_with_recorded_decisions"] = json!(same_decisions(&jobs, &rekeyed));
        let checkpoint = run.join("checkpoint.bin");
        if checkpoint.exists() {
            let saved = SnapshotCheckpoint::<NesSnapshot<G::State>>::from_bytes(
                &fs::read(checkpoint)?,
                SNAPSHOT_CHECKPOINT_FORMAT,
            )?;
            let recorded_holders: BTreeSet<(u64, Option<AssertionKey>)> = saved
                .entries
                .iter()
                .map(|entry| (entry.id, entry.snapshot.observation.key))
                .collect();
            let rekeyed_holders: BTreeSet<(u64, Option<AssertionKey>)> = rekeyed
                .holders
                .iter()
                .flat_map(|held| held.ids.iter().zip(&held.keys))
                .map(|(id, key)| (*id, Some(*key)))
                .collect();
            let snapshot_ids: BTreeSet<u64> = rekeyed.snapshots.iter().copied().collect();
            report["checkpoint_entries"] = json!(recorded_holders.len());
            report["checkpoint_holders_match"] =
                json!(rekeyed_holders.is_subset(&recorded_holders));
            report["checkpoint_entries_match"] = json!(
                recorded_holders
                    .iter()
                    .map(|(id, _)| *id)
                    .collect::<BTreeSet<_>>()
                    == snapshot_ids
            );
            report["checkpoint_non_holders"] =
                json!(recorded_holders.difference(&rekeyed_holders).count());
        }
    }
    Ok(report)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (game, run) = match args.as_slice() {
        [game, run] | [game, run, _] => (game.as_str(), Path::new(run)),
        _ => return Err("usage: nes-rekey GAME RUN_DIR [STRATEGY.toml]".into()),
    };
    let strategy = args.get(2).map(fs::read_to_string).transpose()?;
    let report = match game {
        "joust" => rekey(Joust, run, strategy.as_deref())?,
        "zelda" => rekey(Zelda, run, strategy.as_deref())?,
        "famidash" => rekey(<Famidash>::default(), run, strategy.as_deref())?,
        other => return Err(format!("{other} does not declare assertions").into()),
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
