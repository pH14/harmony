// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, fs, path::PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::target::{FaultAction, FaultObservations, FaultStop};

pub const PACKAGE: &str = "faults";
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Options {
    pub seed: u64,
    pub workers: u32,
    pub executions: u64,
    pub actions: usize,
    pub ram_mib: u32,
    pub knobs: Vec<String>,
    pub wall_minutes: Option<u64>,
    pub output: PathBuf,
}

impl Options {
    pub fn validate(&self) -> Result<(), Box<dyn Error>> {
        if self.workers == 0 || self.actions == 0 || self.executions == 0 {
            return Err("workers, actions, and executions must be positive".into());
        }
        if self.ram_mib == 0 {
            return Err("--ram-mib must be positive".into());
        }
        if self.output.exists() && fs::read_dir(&self.output)?.next().is_some() {
            return Err("search output directory must be empty; choose a new --out path".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BugSummary {
    pub execution: u64,
    pub actions: Vec<FaultAction>,
    pub stop: FaultStop,
    pub violations: Vec<String>,
    pub sometimes: Vec<String>,
    pub state_hash: String,
    #[serde(default)]
    pub state_hash_encoding: StateHashEncoding,
    pub confirmed: bool,
    pub replay: Option<ReplaySummary>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StateHashEncoding {
    EngineDigest,
    #[default]
    LegacySha256OfDigest,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReplaySummary {
    pub run: u32,
    pub bug: bool,
    pub stop: FaultStop,
    pub state_hash: String,
    #[serde(default)]
    pub state_hash_encoding: StateHashEncoding,
    pub violations: Vec<String>,
    pub sometimes: Vec<String>,
    pub actions_applied: u64,
    pub settle_actions: u64,
    pub settle_ticks: u64,
    pub guest_horizons: u64,
    pub check: Option<crate::target::CheckEvidence>,
}

#[cfg(any(
    test,
    all(
        feature = "consonance",
        any(
            all(
                target_os = "linux",
                any(target_arch = "x86_64", target_arch = "aarch64")
            ),
            all(target_os = "macos", target_arch = "aarch64")
        ),
        not(miri)
    )
))]
const REPLAY_SETTLE_TICKS: [u16; 13] = [1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1_024, 2_048, 4_096];

#[cfg(any(
    test,
    all(
        feature = "consonance",
        any(
            all(
                target_os = "linux",
                any(target_arch = "x86_64", target_arch = "aarch64")
            ),
            all(target_os = "macos", target_arch = "aarch64")
        ),
        not(miri)
    )
))]
fn replay_needs_settle(observation: &FaultObservations) -> bool {
    observation.check.as_ref().is_some_and(|check| {
        check.run == 0
            || check.start_generation != check.disturbance_generation
            || check.end_generation != check.disturbance_generation
            || check.pending_faults != 0
    })
}

impl ReplaySummary {
    #[must_use]
    pub fn from_observation(
        observation: &FaultObservations,
        state_digest: [u8; 32],
        actions_applied: u64,
        guest_horizons: u64,
    ) -> Self {
        Self {
            run: 1,
            bug: observation.is_bug(),
            stop: observation.stop,
            state_hash: state_digest_hex(&state_digest),
            state_hash_encoding: StateHashEncoding::EngineDigest,
            violations: observation.violations().into_iter().collect(),
            sometimes: observation.sometimes().into_iter().collect(),
            actions_applied,
            settle_actions: 0,
            settle_ticks: 0,
            guest_horizons,
            check: observation.check.clone(),
        }
    }
}

#[must_use]
pub fn replay_confirms_bug(
    recorded_stop: FaultStop,
    recorded_violations: &[String],
    replay: &ReplaySummary,
) -> bool {
    replay.bug
        && recorded_violations
            .iter()
            .all(|point| replay.violations.contains(point))
        && (!recorded_violations.is_empty() || replay.stop == recorded_stop)
}

#[must_use]
pub fn first_confirmed_bug(bugs: &[BugSummary]) -> Option<u64> {
    bugs.iter()
        .filter(|bug| bug.confirmed)
        .map(|bug| bug.execution)
        .min()
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Report {
    pub package: String,
    pub mode: String,
    pub image_sha256: String,
    pub kernel_sha256: String,
    pub identity: String,
    pub seed: u64,
    pub workers: u32,
    pub horizon_ms: u64,
    pub ram_mib: u32,
    pub executions: u64,
    pub bug_found: bool,
    pub first_bug_execution: Option<u64>,
    pub bugs: Vec<BugSummary>,
    pub replays: Vec<ReplaySummary>,
    pub execution_ticks: u64,
    pub wall_seconds: u64,
    #[serde(default)]
    pub watchdog_cutoffs: u64,
    pub execution_failures: u64,
}

impl Report {
    #[must_use]
    pub fn new(mode: &str, artifacts: &Artifacts, identity: String, options: &Options) -> Self {
        Self {
            package: PACKAGE.to_owned(),
            mode: mode.to_owned(),
            image_sha256: sha256_hex(&artifacts.initramfs),
            kernel_sha256: sha256_hex(&artifacts.kernel),
            identity,
            seed: options.seed,
            workers: options.workers,
            horizon_ms: crate::target::DEFAULT_HORIZON_NANOS / 1_000_000,
            ram_mib: options.ram_mib,
            executions: 0,
            bug_found: false,
            first_bug_execution: None,
            bugs: Vec::new(),
            replays: Vec::new(),
            execution_ticks: 0,
            wall_seconds: 0,
            watchdog_cutoffs: 0,
            execution_failures: 0,
        }
    }

    pub fn write(&self, directory: &std::path::Path) -> Result<(), Box<dyn Error>> {
        fs::create_dir_all(directory)?;
        serde_json::to_writer_pretty(fs::File::create(directory.join("report.json"))?, self)?;
        Ok(())
    }
}

pub struct Artifacts {
    pub kernel: Vec<u8>,
    pub initramfs: Vec<u8>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
enum RecordedInput {
    Actions(Vec<FaultAction>),
    Report {
        actions: Vec<FaultAction>,
        horizon_nanos: Option<u64>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordedActions {
    pub actions: Vec<FaultAction>,
    pub horizon_nanos: Option<u64>,
}

pub fn parse_recorded_input(text: &str) -> Result<RecordedActions, Box<dyn Error>> {
    let (actions, horizon_nanos) = match serde_json::from_str::<RecordedInput>(text)? {
        RecordedInput::Actions(actions) => (actions, None),
        RecordedInput::Report {
            actions,
            horizon_nanos,
        } => (actions, horizon_nanos),
    };
    if actions.is_empty() {
        return Err("the recorded input names no actions".into());
    }
    if actions.len() > crate::target::MAX_FAULT_ACTIONS {
        return Err("recorded input exceeds the action bound".into());
    }
    for action in &actions {
        match action {
            FaultAction::EventKill { rarity, .. } | FaultAction::EventPark { rarity, .. }
                if *rarity >= 64 =>
            {
                return Err("event rarity exceeds the runtime hash width".into());
            }
            FaultAction::EventPark { hold_us: 0, .. } => {
                return Err("event park hold must be positive".into());
            }
            _ => {}
        }
    }
    Ok(RecordedActions {
        actions,
        horizon_nanos,
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn state_digest_hex(digest: &[u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(all(
    feature = "consonance",
    any(
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ),
        all(target_os = "macos", target_arch = "aarch64")
    ),
    not(miri)
))]
mod live {
    use std::{error::Error, io::BufWriter, time::Instant};

    use searcher::search::{
        archive::{MAX_ARCHIVE_ENTRIES, RetentionPolicy},
        campaign::CampaignOrigin,
        draw::{DrawMixture, SuffixShape},
    };
    use serde_json::json;

    use super::{
        Artifacts, BugSummary, Options, REPLAY_SETTLE_TICKS, ReplaySummary, Report,
        StateHashEncoding, first_confirmed_bug, replay_confirms_bug, replay_needs_settle,
    };
    use crate::{
        bundle::FaultVocabulary,
        campaign::{FaultCampaignConfig, FaultWorkload, run_fault_campaign_checkpointed},
        consonance::{FaultConfig, FaultTarget, identity},
        report::{BugReport, write_bug_reports},
        target::{ActionWindows, FaultAction},
    };

    const MEMORY_BUDGET_MIB: usize = 512;

    fn config(options: &Options) -> FaultConfig {
        FaultConfig {
            knobs: options.knobs.clone(),
            ram_mib: options.ram_mib,
        }
    }

    pub fn search(
        artifacts: &Artifacts,
        vocabulary: &FaultVocabulary,
        options: &Options,
    ) -> Result<Report, Box<dyn Error>> {
        options.validate()?;
        let config = config(options);
        let identity = identity(&artifacts.kernel, &artifacts.initramfs, &config);
        let mut report = Report::new("search", artifacts, identity, options);
        std::fs::create_dir_all(&options.output)?;
        let game = FaultWorkload::new(&artifacts.kernel, &artifacts.initramfs, &config);
        let campaign = FaultCampaignConfig {
            campaign_seed: options.seed,
            vocabulary: vocabulary.clone(),
            workers: options.workers,
            execution_budget: options.executions,
            action_limit: options.actions,
            host: hostname(),
            wall_budget: options
                .wall_minutes
                .map(|minutes| std::time::Duration::from_secs(minutes.saturating_mul(60))),
            archive_entry_limit: MAX_ARCHIVE_ENTRIES,
            memory_budget_mib: Some(MEMORY_BUDGET_MIB),
            materialize_final_artifacts: true,
            retention: RetentionPolicy::Unprobed,
            suffix: SuffixShape::OneToSix,
            mixture: DrawMixture::AlphabetOnly,
            objective_witness_path: Some(options.output.join("first-bug-input.json")),
        };
        #[allow(clippy::disallowed_methods)]
        let started = Instant::now();
        let mut stream =
            BufWriter::new(std::fs::File::create(options.output.join("stream.jsonl"))?);
        let mut progress = std::fs::File::create(options.output.join("progress.jsonl"))?;
        let (campaign_report, _checkpoint) = run_fault_campaign_checkpointed(
            &game,
            &campaign,
            &CampaignOrigin::Genesis,
            &mut stream,
            Some(&mut progress),
        )?;
        let archive = &campaign_report.campaign.archive;
        let windows = ActionWindows {
            root_seal: archive.root_seal,
            horizon_nanos: archive.horizon_nanos,
        };
        let written = write_bug_reports(windows, &archive.bugs, &options.output)?;
        let summary = json!({
            "mode": "faultlab_campaign",
            "image": game.image_identity(),
            "horizon_nanos": archive.horizon_nanos,
            "root_seal": archive.root_seal,
            "vocabulary": vocabulary.identifier(),
            "campaign_seed": campaign_report.campaign.campaign_seed,
            "workers": campaign_report.campaign.workers,
            "execution_budget": campaign_report.campaign.execution_budget,
            "executions": campaign_report.campaign.executions_completed,
            "execution_ticks": campaign_report.campaign.execution_work,
            "stream_sha256": campaign_report.campaign.stream_sha256,
            "archive_entries": archive.entries.len(),
            "progress": archive.progress_watermark,
            "milestones": archive.milestones,
            "assertions": archive.assertions,
            "bugs_found": campaign_report.bugs_found,
            "executions_to_first_bug": campaign_report.executions_to_first_bug,
            "bug_reports": written.iter().map(BugReport::file_name).collect::<Vec<_>>(),
            "watchdog_cutoffs": archive.watchdog_cutoffs,
            "execution_failures": campaign_report.campaign.execution_failures,
        });
        std::fs::write(
            options.output.join("campaign-summary.json"),
            serde_json::to_vec_pretty(&summary)?,
        )?;
        report.executions = campaign_report.campaign.executions_completed;
        report.execution_ticks = campaign_report.campaign.execution_work;
        for bug in &written {
            let violations: Vec<String> = bug.observations.violations().into_iter().collect();
            let witness = match replay_once(artifacts, &config, &bug.actions) {
                Ok(summary) => Some(summary),
                Err(error) => {
                    eprintln!("bug {} did not replay: {error}", bug.bug);
                    None
                }
            };
            let confirmed = witness.as_ref().is_some_and(|witness| {
                replay_confirms_bug(bug.observations.stop, &violations, witness)
            });
            if !confirmed {
                eprintln!(
                    "bug {} was not confirmed by replay and is reported unconfirmed",
                    bug.bug
                );
            }
            report.bugs.push(BugSummary {
                execution: bug.execution,
                actions: bug.actions.clone(),
                stop: bug.observations.stop,
                violations,
                sometimes: bug.observations.sometimes().into_iter().collect(),
                state_hash: witness
                    .as_ref()
                    .map(|witness| witness.state_hash.clone())
                    .unwrap_or_default(),
                state_hash_encoding: witness
                    .as_ref()
                    .map_or(StateHashEncoding::default(), |witness| {
                        witness.state_hash_encoding
                    }),
                confirmed,
                replay: witness,
            });
        }
        report.first_bug_execution = first_confirmed_bug(&report.bugs);
        report.bug_found = report.first_bug_execution.is_some();
        report.wall_seconds = started.elapsed().as_secs();
        report.watchdog_cutoffs = archive.watchdog_cutoffs;
        report.execution_failures = campaign_report.campaign.execution_failures;
        report.write(&options.output)?;
        Ok(report)
    }

    pub fn replay(
        artifacts: &Artifacts,
        actions: &[FaultAction],
        repeat: u32,
        options: &Options,
    ) -> Result<Report, Box<dyn Error>> {
        if repeat == 0 {
            return Err("--repeat must be positive".into());
        }
        let config = config(options);
        let identity = identity(&artifacts.kernel, &artifacts.initramfs, &config);
        let mut report = Report::new("replay", artifacts, identity, options);
        #[allow(clippy::disallowed_methods)]
        let started = Instant::now();
        for run in 1..=repeat {
            let mut summary = replay_once(artifacts, &config, actions)?;
            summary.run = run;
            report.execution_ticks = report.execution_ticks.saturating_add(
                actions
                    .iter()
                    .take(summary.actions_applied as usize)
                    .map(crate::target::action_ticks)
                    .sum::<u64>()
                    .saturating_add(summary.settle_ticks),
            );
            report.bug_found |= summary.bug;
            report.replays.push(summary);
        }
        report.wall_seconds = started.elapsed().as_secs();
        report.write(&options.output)?;
        Ok(report)
    }

    fn replay_once(
        artifacts: &Artifacts,
        config: &FaultConfig,
        actions: &[FaultAction],
    ) -> Result<ReplaySummary, Box<dyn Error>> {
        let mut target = FaultTarget::fresh(&artifacts.kernel, &artifacts.initramfs, config)?;
        for action in actions {
            target.apply(*action);
        }
        let actions_applied = target.actions().len() as u64;
        let mut settle_actions = 0_u64;
        let mut settle_ticks = 0_u64;
        for ticks in REPLAY_SETTLE_TICKS {
            if target.failed() || !replay_needs_settle(target.observation()) {
                break;
            }
            let before = target.actions().len();
            target.apply(FaultAction::Wait(
                std::num::NonZeroU16::new(ticks).expect("settle duration"),
            ));
            if target.actions().len() == before {
                break;
            }
            settle_actions = settle_actions.saturating_add(1);
            settle_ticks = settle_ticks.saturating_add(u64::from(ticks));
        }
        if target.failed() {
            return Err(format!(
                "the replay failed after {actions_applied} of {} input actions and {settle_actions} settlement actions",
                actions.len(),
            )
            .into());
        }
        let observation = target.observation();
        let mut summary = ReplaySummary::from_observation(
            observation,
            target.state_hash()?,
            actions_applied,
            target.guest_horizons_run(),
        );
        summary.settle_actions = settle_actions;
        summary.settle_ticks = settle_ticks;
        Ok(summary)
    }

    fn hostname() -> String {
        std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".to_owned())
    }
}

#[cfg(all(
    feature = "consonance",
    any(
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ),
        all(target_os = "macos", target_arch = "aarch64")
    ),
    not(miri)
))]
pub use live::{replay, search};

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> Options {
        Options {
            seed: 7,
            workers: 2,
            executions: 10,
            actions: 4,
            ram_mib: 1024,
            knobs: Vec::new(),
            wall_minutes: None,
            output: PathBuf::from("unused"),
        }
    }

    #[test]
    fn replay_settlement_waits_for_current_quiescent_check_evidence() {
        let current = crate::target::CheckEvidence {
            disturbance_generation: 7,
            run: 3,
            start_generation: 7,
            end_generation: 7,
            points: ids(&[11]),
            pending_faults: 0,
        };
        let observation = |check| FaultObservations {
            check: Some(check),
            ..FaultObservations::default()
        };
        assert!(!replay_needs_settle(&observation(current.clone())));
        for stale in [
            crate::target::CheckEvidence {
                run: 0,
                ..current.clone()
            },
            crate::target::CheckEvidence {
                start_generation: 6,
                ..current.clone()
            },
            crate::target::CheckEvidence {
                end_generation: 6,
                ..current.clone()
            },
            crate::target::CheckEvidence {
                pending_faults: 1,
                ..current
            },
        ] {
            assert!(replay_needs_settle(&observation(stale)));
        }
        assert!(!replay_needs_settle(&FaultObservations::default()));
        assert_eq!(REPLAY_SETTLE_TICKS.first(), Some(&1));
        assert_eq!(REPLAY_SETTLE_TICKS.last(), Some(&4_096));
        assert!(
            REPLAY_SETTLE_TICKS
                .windows(2)
                .all(|pair| pair[1] == pair[0] * 2)
        );
    }

    #[test]
    fn zero_bounds_are_refused_before_a_guest_boots() {
        for broken in [
            Options {
                workers: 0,
                ..options()
            },
            Options {
                executions: 0,
                ..options()
            },
            Options {
                actions: 0,
                ..options()
            },
            Options {
                ram_mib: 0,
                ..options()
            },
        ] {
            assert!(broken.validate().is_err());
        }
    }

    #[test]
    fn a_non_empty_output_directory_is_refused() {
        let directory = tempfile::tempdir().expect("temp dir");
        let options = Options {
            output: directory.path().to_path_buf(),
            ..options()
        };
        assert!(options.validate().is_ok(), "an empty directory is accepted");
        std::fs::write(directory.path().join("report.json"), b"{}").expect("write");
        assert!(options.validate().is_err());
    }

    #[test]
    fn checked_in_historical_inputs_use_the_current_action_encoding() {
        for text in [
            include_str!("../../bugs/historical/postgres-cic-corruption/probe.json"),
            include_str!("../../bugs/historical/postgres-cic-corruption/samples/quiet-wait.json"),
        ] {
            let input = parse_recorded_input(text).unwrap();
            assert!(
                input
                    .actions
                    .iter()
                    .any(|action| matches!(action, FaultAction::Wait(_)))
            );
            assert!(input.actions.iter().all(|action| match action {
                FaultAction::Wait(ticks) => ticks.get() == 50,
                _ => true,
            }));
        }
    }

    #[test]
    fn a_recorded_input_reads_as_a_bug_report_or_a_bare_action_list() {
        let actions = vec![FaultAction::Hook(1), FaultAction::Kill(0)];
        let bare = serde_json::to_string(&actions).expect("serialize");
        assert_eq!(
            parse_recorded_input(&bare).expect("bare"),
            RecordedActions {
                actions: actions.clone(),
                horizon_nanos: None,
            }
        );
        let report = serde_json::json!({ "bug": 1, "actions": actions }).to_string();
        assert_eq!(
            parse_recorded_input(&report).expect("report").actions,
            actions
        );
        let timed = serde_json::json!({
            "bug": 1,
            "actions": actions,
            "horizon_nanos": 250_000_000_u64,
        })
        .to_string();
        assert_eq!(
            parse_recorded_input(&timed)
                .expect("timed report")
                .horizon_nanos,
            Some(250_000_000)
        );
        assert!(parse_recorded_input("[]").is_err());
        assert!(parse_recorded_input("{}").is_err());
    }

    fn ids(values: &[u32]) -> Vec<String> {
        values.iter().map(u32::to_string).collect()
    }

    fn replay_summary(bug: bool, stop: FaultStop, violations: &[u32]) -> ReplaySummary {
        ReplaySummary {
            check: None,
            run: 1,
            bug,
            stop,
            state_hash: "hash".to_owned(),
            state_hash_encoding: StateHashEncoding::LegacySha256OfDigest,
            violations: ids(violations),
            sometimes: ids(&[24]),
            actions_applied: 3,
            settle_actions: 0,
            settle_ticks: 0,
            guest_horizons: 3,
        }
    }

    fn bug_summary(execution: u64, confirmed: bool) -> BugSummary {
        BugSummary {
            execution,
            actions: vec![FaultAction::Hook(3)],
            stop: FaultStop::Assertion { point: 2 },
            violations: ids(&[2]),
            sometimes: ids(&[24]),
            state_hash: "hash".to_owned(),
            state_hash_encoding: StateHashEncoding::LegacySha256OfDigest,
            confirmed,
            replay: None,
        }
    }

    #[test]
    fn a_replay_confirms_a_bug_only_by_reproducing_its_evidence() {
        let violated = FaultStop::Assertion { point: 2 };
        assert!(
            replay_confirms_bug(violated, &ids(&[2]), &replay_summary(true, violated, &[2])),
            "the recorded assertion fired again"
        );
        assert!(
            !replay_confirms_bug(
                violated,
                &ids(&[2]),
                &replay_summary(false, FaultStop::Deadline, &[])
            ),
            "a clean replay confirms nothing"
        );
        assert!(
            !replay_confirms_bug(
                violated,
                &ids(&[2]),
                &replay_summary(true, FaultStop::Crash, &[])
            ),
            "a crash is not the assertion the campaign recorded"
        );
        assert!(
            !replay_confirms_bug(
                violated,
                &ids(&[2]),
                &replay_summary(true, FaultStop::Assertion { point: 7 }, &[7])
            ),
            "another assertion is another bug"
        );
    }

    #[test]
    fn a_stop_only_bug_is_confirmed_by_the_same_stop() {
        assert!(replay_confirms_bug(
            FaultStop::Crash,
            &ids(&[]),
            &replay_summary(true, FaultStop::Crash, &[])
        ));
        assert!(
            !replay_confirms_bug(
                FaultStop::Crash,
                &ids(&[]),
                &replay_summary(true, FaultStop::Assertion { point: 2 }, &[2])
            ),
            "a crash and an assertion are different evidence"
        );
    }

    #[test]
    fn a_campaign_hit_no_replay_reproduced_is_not_a_rediscovery() {
        assert_eq!(first_confirmed_bug(&[]), None);
        assert_eq!(
            first_confirmed_bug(&[bug_summary(4, false), bug_summary(9, false)]),
            None,
            "unconfirmed hits leave the run with nothing to report"
        );
        assert_eq!(
            first_confirmed_bug(&[bug_summary(9, true), bug_summary(4, false)]),
            Some(9),
            "the first hit is the earliest confirmed one, not the earliest recorded"
        );
        assert_eq!(
            first_confirmed_bug(&[bug_summary(9, true), bug_summary(4, true)]),
            Some(4)
        );
    }

    #[test]
    fn a_report_pins_every_artifact_and_round_trips_through_json() {
        let artifacts = Artifacts {
            kernel: b"kernel".to_vec(),
            initramfs: b"initramfs".to_vec(),
        };
        let report = Report::new("search", &artifacts, "identity".to_owned(), &options());
        assert_eq!(report.package, PACKAGE);
        assert_eq!(report.kernel_sha256, sha256_hex(b"kernel"));
        assert_eq!(report.image_sha256, sha256_hex(b"initramfs"));
        assert_eq!(report.first_bug_execution, None);
        let text = serde_json::to_string(&report).expect("serialize");
        assert!(text.contains("\"first_bug_execution\":null"));
        let decoded: Report = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(decoded, report);
    }

    #[test]
    fn a_report_is_written_into_the_output_directory() {
        let directory = tempfile::tempdir().expect("temp dir");
        let artifacts = Artifacts {
            kernel: Vec::new(),
            initramfs: Vec::new(),
        };
        let report = Report::new("replay", &artifacts, String::new(), &options());
        report.write(directory.path()).expect("write");
        let text = std::fs::read_to_string(directory.path().join("report.json")).expect("read");
        assert_eq!(
            serde_json::from_str::<Report>(&text).expect("decode"),
            report
        );
    }

    #[test]
    fn a_replay_report_serializes_the_engine_digest_without_hashing_it_again() {
        let state_digest = [
            0x14, 0xbc, 0xef, 0x45, 0x9b, 0x6a, 0x75, 0x95, 0x9b, 0x50, 0x36, 0x70, 0x94, 0xbf,
            0x7f, 0x48, 0x2b, 0x55, 0xd5, 0x34, 0xbd, 0xb7, 0x94, 0xfd, 0x79, 0x0f, 0x37, 0xab,
            0x70, 0xcc, 0x5c, 0x96,
        ];
        let observation = FaultObservations {
            check: Some(crate::target::CheckEvidence {
                run: 3,
                points: ids(&[7, 11]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let summary = ReplaySummary::from_observation(&observation, state_digest, 1, 1);
        assert_eq!(summary.check, observation.check);
        assert_eq!((summary.settle_actions, summary.settle_ticks), (0, 0));
        let artifacts = Artifacts {
            kernel: Vec::new(),
            initramfs: Vec::new(),
        };
        let mut report = Report::new("replay", &artifacts, String::new(), &options());
        report.replays.push(summary);

        let expected = "14bcef459b6a75959b50367094bf7f482b55d534bdb794fd790f37ab70cc5c96";
        assert_eq!(report.replays[0].state_hash, expected);
        assert_eq!(
            report.replays[0].state_hash_encoding,
            StateHashEncoding::EngineDigest
        );
        assert_ne!(report.replays[0].state_hash, sha256_hex(&state_digest));

        let directory = tempfile::tempdir().expect("temp dir");
        report.write(directory.path()).expect("write");
        let text = std::fs::read_to_string(directory.path().join("report.json")).expect("read");
        let decoded: Report = serde_json::from_str(&text).expect("decode");
        assert_eq!(decoded.replays[0].state_hash, expected);
        assert_eq!(
            decoded.replays[0].state_hash_encoding,
            StateHashEncoding::EngineDigest
        );
    }

    #[test]
    fn a_legacy_replay_report_without_encoding_marker_stays_legacy() {
        let artifacts = Artifacts {
            kernel: Vec::new(),
            initramfs: Vec::new(),
        };
        let mut report = Report::new("replay", &artifacts, String::new(), &options());
        report.bugs.push(bug_summary(1, false));
        report
            .replays
            .push(replay_summary(false, FaultStop::Deadline, &[]));
        let legacy_hash = "4218d5589ba8e154820f48e72ff2e82ddd26078f11b6c09da4d3b6e4bce7a079";
        report.bugs[0].state_hash = legacy_hash.to_owned();
        report.replays[0].state_hash = legacy_hash.to_owned();
        let mut value = serde_json::to_value(&report).expect("serialize");
        value["bugs"][0]
            .as_object_mut()
            .expect("bug object")
            .remove("state_hash_encoding");
        value["replays"][0]
            .as_object_mut()
            .expect("replay object")
            .remove("state_hash_encoding");

        let decoded: Report = serde_json::from_value(value).expect("decode legacy report");
        assert_eq!(decoded.bugs[0].state_hash, legacy_hash);
        assert_eq!(
            decoded.bugs[0].state_hash_encoding,
            StateHashEncoding::LegacySha256OfDigest
        );
        assert_eq!(decoded.replays[0].state_hash, legacy_hash);
        assert_eq!(
            decoded.replays[0].state_hash_encoding,
            StateHashEncoding::LegacySha256OfDigest
        );
    }
}
