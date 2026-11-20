// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    any::TypeId,
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fs,
    path::Path,
};

use machine::nes::ButtonChord;
use nes_games::workload::{
    Game, Lowered, NesSnapshot, Never, RECORDS_FILE, REPLAY_RECORDS_FILE,
    SNAPSHOT_CHECKPOINT_FORMAT,
};
use nes_games::{mm2::MegaMan2, smb::game::SuperMarioBros};
use nes_workload::{
    donkey_kong::DonkeyKong,
    famidash::{Famidash, LuckyDraw},
    ice_climbers::IceClimbers,
    joust::Joust,
    pac_man::PacMan,
    punchout::PunchOut,
    twin_dragons::TwinDragons,
    zelda::Zelda,
};
use searcher::search::{
    assertion::{
        AssertionKey, Compiled, RecordedAction, RecordedJob, Recording, Rekeyed, Rekeying,
        StreamIdentity, same_decisions,
    },
    campaign::SnapshotCheckpoint,
};
use serde_json::{Value, json};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

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

fn rekeyed_report(rekeyed: &Rekeyed) -> Value {
    json!({
        "format": rekeyed.format,
        "policy": rekeyed.policy,
        "jobs": rekeyed.jobs,
        "candidates": rekeyed.candidates,
        "retained": rekeyed.retained,
        "rejected": rekeyed.rejected,
        "cells": rekeyed.cells,
        "entries": rekeyed.holders.iter().map(|held| held.ids.len()).sum::<usize>(),
        "tiers": rekeyed.tiers,
        "findings": rekeyed.findings,
        "stage_falls": rekeyed.stage_falls,
        "holders": rekeyed.holders,
    })
}

fn exact_rekeying<G: Game>(strategies: [&Compiled; 2]) -> Result<()> {
    let moves = TypeId::of::<G::Move>() != TypeId::of::<Never>();
    if moves
        && strategies
            .iter()
            .any(|compiled| !compiled.is_pure() || !compiled.stage_names().is_empty())
    {
        return Err(format!(
            "{} has moves, and a strategy with stages or a `highest` aggregate re-keys exactly only on a game without moves (https://github.com/pH14/harmony/issues/1062)",
            G::NAME
        )
        .into());
    }
    Ok(())
}

fn rekey<G: Game>(game: G, run: &Path, strategy: Option<&str>) -> Result<Value> {
    let recording = Recording::<ButtonChord>::from_bytes(&fs::read(run.join(RECORDS_FILE))?)?;
    let recorded = Lowered::new(game.clone(), &recording.header.strategy)?;
    if recorded.records_catalog()? != recording.header.catalog {
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
    exact_rekeying::<G>([&recorded.compiled, &keyed.compiled])?;
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
    .run(&recording.header.genesis, &jobs, &recording.actions)?;
    let same_policy = keyed.compiled.policy_identifier() == recording.header.policy;
    let mut report = rekeyed_report(&rekeyed);
    report["recorded_policy"] = json!(recording.header.policy);
    report["records_jobs"] = json!(recording.jobs);
    report["records_truncated_bytes"] = json!(recording.truncated_bytes);
    report["actions"] = json!(recording.actions.len());
    report["frames"] = json!(
        recording
            .actions
            .iter()
            .map(|action| action.frames.len())
            .sum::<usize>()
    );
    report["cost"] = json!(
        recording
            .actions
            .iter()
            .map(|action| action.cost)
            .sum::<u64>()
    );
    report["forced_move_frames"] = json!(forced_move_frames(&recording.actions));
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
        "famidash-lucky-draw" => rekey(LuckyDraw::default(), run, strategy.as_deref())?,
        "twin-dragons" => rekey(TwinDragons, run, strategy.as_deref())?,
        "punchout" => rekey(PunchOut, run, strategy.as_deref())?,
        "donkey-kong" => rekey(DonkeyKong, run, strategy.as_deref())?,
        "ice-climbers" => rekey(IceClimbers, run, strategy.as_deref())?,
        "pac-man" => rekey(PacMan, run, strategy.as_deref())?,
        "smb-strategy" => rekey(SuperMarioBros, run, strategy.as_deref())?,
        "mm2-strategy" => rekey(MegaMan2, run, strategy.as_deref())?,
        other => return Err(format!("{other} does not declare assertions").into()),
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use searcher::search::assertion::{Holders, Tier};

    #[test]
    fn the_rekeyed_report_names_its_format() {
        let rekeyed = Rekeyed {
            format: "assertion-rekeyed-v2",
            policy: "assertion_key_test".to_owned(),
            jobs: 1,
            candidates: 2,
            retained: 1,
            rejected: 1,
            cells: 1,
            tiers: vec![Tier {
                progress: 0,
                cells: 1,
                entries: 1,
            }],
            holders: vec![Holders {
                progress: 0,
                place: 3,
                identity: u64::from(u32::MAX) + 1,
                ids: vec![7],
                keys: vec![AssertionKey::default()],
            }],
            snapshots: vec![7],
            findings: Vec::new(),
            stage_falls: Vec::new(),
            decisions: Vec::new(),
        };
        let report = rekeyed_report(&rekeyed);
        assert_eq!(report["format"], "assertion-rekeyed-v2");
        assert_eq!(report["entries"], 1);
        assert_eq!(report["holders"][0]["identity"], u64::from(u32::MAX) + 1);
    }

    #[test]
    fn a_game_with_moves_refuses_a_strategy_that_reads_its_lineage() {
        let shipped = Lowered::new(LuckyDraw::default(), LuckyDraw::STRATEGY).unwrap();
        let highest = Lowered::new(
            LuckyDraw::default(),
            &format!("identity = \"highest(x)\"\n{}", LuckyDraw::STRATEGY),
        )
        .unwrap();
        let staged = Lowered::new(
            LuckyDraw::default(),
            "goal = \"0\"\nabsent = \"menu_wait == 2\"\ncapacity = 1\nspread = [\"menu\", \"menu_wait\", \"x / 32\"]\n[[stage]]\nname = \"waiting\"\ngoal = \"menu_wait == 0\"\n[[stage]]\nname = \"after\"\ngoal = \"0\"\n",
        )
        .unwrap();
        assert!(shipped.compiled.stage_names().is_empty());
        assert!(shipped.compiled.is_pure());
        assert!(!highest.compiled.is_pure());
        assert!(staged.compiled.is_pure());
        assert_eq!(staged.compiled.stage_names(), ["waiting", "after"]);
        assert!(exact_rekeying::<LuckyDraw>([&shipped.compiled, &shipped.compiled]).is_ok());
        for strategies in [
            [&shipped.compiled, &highest.compiled],
            [&highest.compiled, &shipped.compiled],
            [&shipped.compiled, &staged.compiled],
            [&staged.compiled, &shipped.compiled],
        ] {
            let refused = exact_rekeying::<LuckyDraw>(strategies).unwrap_err();
            assert!(refused.to_string().contains("issues/1062"));
        }
        let smb = Lowered::new(
            SuperMarioBros,
            &SuperMarioBros::STRATEGY.replace(
                "identity = \"screen_x\"",
                "identity = \"highest(screen_x)\"",
            ),
        )
        .unwrap();
        assert!(!smb.compiled.is_pure());
        assert!(exact_rekeying::<SuperMarioBros>([&smb.compiled, &smb.compiled]).is_ok());
    }
}
