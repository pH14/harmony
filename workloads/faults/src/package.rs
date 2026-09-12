// SPDX-License-Identifier: AGPL-3.0-or-later

//! The package's two entry points and the report both write.
//!
//! Search runs a campaign over the workload's action alphabet until it finds a
//! bug or spends its budget. Replay runs one recorded action list a fixed
//! number of times, which is how a reported bug is confirmed. Both write
//! `report.json` into the output directory with the same fields, so one reader
//! serves both.

use std::{error::Error, fs, path::PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    archive::FaultMilestones,
    target::{FaultAction, FaultObservations, FaultStop},
};

/// Package name recorded in every report.
pub const PACKAGE: &str = "faults";
/// What one campaign or replay was asked to do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Options {
    /// Campaign seed. A replay records it but does not draw from it.
    pub seed: u64,
    /// Evaluator threads, each owning one guest.
    pub workers: u32,
    /// Executions the campaign may admit.
    pub executions: u64,
    /// Maximum actions in one input.
    pub actions: usize,
    /// Milliseconds of guest time one action runs for.
    pub horizon_ms: u64,
    /// Guest RAM in MiB.
    pub ram_mib: u32,
    /// Extra guest command-line words.
    pub knobs: Vec<String>,
    /// Execution places the Park action may hold a node at.
    pub places: Vec<u64>,
    /// Optional wall-clock cutoff on a search.
    pub wall_minutes: Option<u64>,
    /// Artifact destination.
    pub output: PathBuf,
}

impl Options {
    /// Virtual nanoseconds one action runs for.
    #[must_use]
    pub fn horizon_nanos(&self) -> u64 {
        self.horizon_ms.saturating_mul(1_000_000)
    }

    /// Reject settings that cannot produce a run before a guest boots.
    ///
    /// # Errors
    ///
    /// Returns an error when a bound is zero or the output directory already
    /// holds artifacts.
    pub fn validate(&self) -> Result<(), Box<dyn Error>> {
        if self.workers == 0 || self.actions == 0 || self.executions == 0 {
            return Err("workers, actions, and executions must be positive".into());
        }
        if self.horizon_ms == 0 || self.ram_mib == 0 {
            return Err("--horizon-ms and --ram-mib must be positive".into());
        }
        if self.output.exists() && fs::read_dir(&self.output)?.next().is_some() {
            return Err("search output directory must be empty; choose a new --out path".into());
        }
        Ok(())
    }
}

/// One bug the run found, with the action list that reproduces it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BugSummary {
    /// Ordered admission position of the execution that found it.
    pub execution: u64,
    /// The action list, in execution order.
    pub actions: Vec<FaultAction>,
    /// How the guest stopped.
    pub stop: FaultStop,
    /// `assert_always` ids the guest reported violated.
    pub violations: Vec<u32>,
    /// `assert_sometimes` ids the guest reported hit.
    pub sometimes: Vec<u32>,
    /// Whole-VM state hash at the terminal endpoint, lowercase hex.
    pub state_hash: String,
    /// Whether replaying the action list reproduced this bug's evidence.
    pub confirmed: bool,
    /// The confirming replay run, absent when the replay could not run.
    pub replay: Option<ReplaySummary>,
}

/// One run of a replayed action list.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReplaySummary {
    /// One-based run ordinal.
    pub run: u32,
    /// Whether this run reproduced the bug.
    pub bug: bool,
    /// How the guest stopped.
    pub stop: FaultStop,
    /// Whole-VM state hash at the endpoint, lowercase hex.
    pub state_hash: String,
    /// `assert_always` ids the guest reported violated.
    pub violations: Vec<u32>,
    /// `assert_sometimes` and `assert_reachable` ids the guest reported hit.
    /// A workload's oracle publishes one of these when it reached a verdict,
    /// so a run with none of them checked nothing.
    pub sometimes: Vec<u32>,
    /// Actions this run applied. A run that stops at a bug applies no more.
    pub actions_applied: u64,
    /// Action horizons this run executed in the guest. It equals
    /// `actions_applied` when every applied action ran in the guest rather
    /// than being answered from a cached snapshot of an earlier run.
    pub guest_horizons: u64,
    /// The endpoint's registers and liveness. A fault that leaves no trace in
    /// the stop or the assertions -- a kill that fired, a park that held -- is
    /// visible only here, so a replay carries them for comparison.
    #[serde(default)]
    pub observations: FaultObservations,
}

/// Whether `replay` reproduced the evidence a campaign recorded for one bug.
///
/// A campaign hit counts as a rediscovery only when running its action list
/// again shows the same evidence: every assertion the campaign saw violated,
/// and the same stop when the stop was the only evidence the campaign had. A
/// run that reported no bug, or a different one, confirms nothing.
#[must_use]
pub fn replay_confirms_bug(
    recorded_stop: FaultStop,
    recorded_violations: &[u32],
    replay: &ReplaySummary,
) -> bool {
    replay.bug
        && recorded_violations
            .iter()
            .all(|point| replay.violations.contains(point))
        && (!recorded_violations.is_empty() || replay.stop == recorded_stop)
}

/// The admission position of the first campaign hit a replay confirmed.
///
/// A run's verdict rests on this: a campaign that recorded hits none of which
/// replayed found nothing it can hand to a reader.
#[must_use]
pub fn first_confirmed_bug(bugs: &[BugSummary]) -> Option<u64> {
    bugs.iter()
        .filter(|bug| bug.confirmed)
        .map(|bug| bug.execution)
        .min()
}

/// The report both modes write to `report.json`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Report {
    /// Always [`PACKAGE`].
    pub package: String,
    /// `search` or `replay`.
    pub mode: String,
    /// SHA-256 of the prepared guest initramfs.
    pub image_sha256: String,
    /// SHA-256 of the guest kernel.
    pub kernel_sha256: String,
    /// SHA-256 of the static fault agent installed in the image.
    pub fault_agent_sha256: String,
    /// The execution identity the run pinned.
    pub identity: String,
    /// Campaign seed.
    pub seed: u64,
    /// Evaluator threads.
    pub workers: u32,
    /// Milliseconds of guest time one action ran for.
    pub horizon_ms: u64,
    /// Guest RAM in MiB.
    pub ram_mib: u32,
    /// Executions the campaign completed; zero in replay mode.
    pub executions: u64,
    /// Whether the run found or reproduced a bug.
    pub bug_found: bool,
    /// Admission position of the first bug-finding execution.
    pub first_bug_execution: Option<u64>,
    /// Bugs the search recorded; empty in replay mode.
    pub bugs: Vec<BugSummary>,
    /// Strongest workload milestones reached anywhere in a search campaign.
    #[serde(default)]
    pub campaign_milestones: FaultMilestones,
    /// Replay runs; empty in search mode.
    pub replays: Vec<ReplaySummary>,
    /// Action horizons the run clocked.
    pub horizons_clocked: u64,
    /// Wall-clock seconds the run took.
    pub wall_seconds: u64,
}

impl Report {
    /// A report with everything the run knows before it boots a guest.
    #[must_use]
    pub fn new(mode: &str, artifacts: &Artifacts, identity: String, options: &Options) -> Self {
        Self {
            package: PACKAGE.to_owned(),
            mode: mode.to_owned(),
            image_sha256: sha256_hex(&artifacts.initramfs),
            kernel_sha256: sha256_hex(&artifacts.kernel),
            fault_agent_sha256: sha256_hex(&artifacts.agent),
            identity,
            seed: options.seed,
            workers: options.workers,
            horizon_ms: options.horizon_ms,
            ram_mib: options.ram_mib,
            executions: 0,
            bug_found: false,
            first_bug_execution: None,
            bugs: Vec::new(),
            campaign_milestones: FaultMilestones::default(),
            replays: Vec::new(),
            horizons_clocked: 0,
            wall_seconds: 0,
        }
    }

    /// Write the report into `directory`.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory or the file cannot be written.
    pub fn write(&self, directory: &std::path::Path) -> Result<(), Box<dyn Error>> {
        fs::create_dir_all(directory)?;
        serde_json::to_writer_pretty(fs::File::create(directory.join("report.json"))?, self)?;
        Ok(())
    }
}

/// The three byte artifacts one run pins.
pub struct Artifacts {
    /// The controlled guest kernel.
    pub kernel: Vec<u8>,
    /// The prepared guest initramfs.
    pub initramfs: Vec<u8>,
    /// The static fault agent installed in the image.
    pub agent: Vec<u8>,
}

/// A recorded action list, read from `--replay`. A bug report written by a
/// search parses directly, and so does a bare action array.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
enum RecordedInput {
    Actions(Vec<FaultAction>),
    Report {
        actions: Vec<FaultAction>,
        horizon_nanos: Option<u64>,
    },
}

/// A recorded action list and the window length it was recorded under, when
/// the record says.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordedActions {
    /// The action list, in execution order.
    pub actions: Vec<FaultAction>,
    /// Virtual nanoseconds each action window spanned when the list was
    /// recorded. A replay under another horizon times its faults differently,
    /// so a run must not take the list without the horizon.
    pub horizon_nanos: Option<u64>,
}

/// Read a recorded action list from a bug report or a bare action array.
///
/// # Errors
///
/// Returns an error when the text is neither shape or names no actions.
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
    Ok(RecordedActions {
        actions,
        horizon_nanos,
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
mod live {
    use std::{error::Error, io::BufWriter, time::Instant};

    use searcher::search::{
        archive::{MAX_ARCHIVE_ENTRIES, RetentionPolicy, RetireThresholds, SelectorPolicy},
        campaign::CampaignOrigin,
        draw::{DrawMixture, SuffixShape},
    };
    use serde_json::json;

    use super::{
        Artifacts, BugSummary, Options, ReplaySummary, Report, first_confirmed_bug,
        replay_confirms_bug, sha256_hex,
    };
    use crate::{
        bundle::FaultVocabulary,
        campaign::{FaultCampaignConfig, FaultGame, run_fault_campaign_checkpointed},
        consonance::{FaultConfig, FaultTarget, identity},
        report::{BugReport, write_bug_reports},
        target::{ActionWindows, FaultAction},
    };

    /// Logical memory the live search structures may hold. A worker's guest
    /// RAM dwarfs this, so the search side is bounded well below it.
    const MEMORY_BUDGET_MIB: usize = 512;

    /// Draws one entry takes without a retained descendant before it retires,
    /// and the pooled thresholds for the group depths above it.
    fn retire_thresholds() -> RetireThresholds {
        RetireThresholds {
            entry: 3,
            groups: vec![6, 2],
        }
    }

    fn config(options: &Options) -> FaultConfig {
        FaultConfig {
            knobs: options.knobs.clone(),
            horizon_nanos: options.horizon_nanos(),
            ram_mib: options.ram_mib,
        }
    }

    /// Search the workload's action alphabet for a bug.
    ///
    /// # Errors
    ///
    /// Returns an error when the campaign cannot run or its artifacts cannot
    /// be written.
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
        let game = FaultGame::new(&artifacts.kernel, &artifacts.initramfs, &config);
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
            retention: RetentionPolicy::AdmitAlive,
            selector: SelectorPolicy::EnergyFrontierCheapest(retire_thresholds()),
            suffix: SuffixShape::OneToSix,
            mixture: DrawMixture::AlphabetOnly,
            victory_input_path: Some(options.output.join("first-bug-input.json")),
        };
        // not order-observable: the elapsed wall time is reported to the
        // operator and never reaches a search decision, an archive key, or a
        // recorded byte.
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
        let mut measures = archive.measures.clone();
        // Guest time is the campaign's, not any one endpoint's: every admitted
        // horizon ran the guest for `horizon_nanos`.
        measures.guest_seconds = campaign_report
            .campaign
            .frames_emulated
            .saturating_mul(archive.horizon_nanos)
            / 1_000_000_000;
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
            "horizons": campaign_report.campaign.frames_emulated,
            "stream_sha256": campaign_report.campaign.stream_sha256,
            "archive_entries": archive.entries.len(),
            "progress": archive.progress_watermark,
            "milestones": archive.milestones,
            "coordinate_telemetry": archive.coordinate_telemetry,
            "bugs_found": campaign_report.bugs_found,
            "executions_to_first_bug": campaign_report.executions_to_first_bug,
            "bug_reports": written.iter().map(BugReport::file_name).collect::<Vec<_>>(),
            "measures": measures,
        });
        std::fs::write(
            options.output.join("campaign-summary.json"),
            serde_json::to_vec_pretty(&summary)?,
        )?;
        report.executions = campaign_report.campaign.executions_completed;
        report.horizons_clocked = campaign_report.campaign.frames_emulated;
        report.campaign_milestones = archive.milestones;
        for bug in &written {
            // A campaign hit is a claim about an action list, so each one is
            // replayed from a fresh session: the replay both supplies the
            // guest state hash the campaign never recorded and decides whether
            // the hit is a rediscovery.
            let violations: Vec<u32> = bug.observations.violations.iter().copied().collect();
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
                sometimes: bug.observations.sometimes.iter().copied().collect(),
                state_hash: witness
                    .as_ref()
                    .map(|witness| witness.state_hash.clone())
                    .unwrap_or_default(),
                confirmed,
                replay: witness,
            });
        }
        // A campaign hit no replay reproduced is not a rediscovery, so the
        // run's verdict and its first hit both come from the confirmed bugs.
        report.first_bug_execution = first_confirmed_bug(&report.bugs);
        report.bug_found = report.first_bug_execution.is_some();
        report.wall_seconds = started.elapsed().as_secs();
        report.write(&options.output)?;
        Ok(report)
    }

    /// Run one recorded action list `repeat` times.
    ///
    /// # Errors
    ///
    /// Returns an error when a run cannot reach its guest.
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
        // not order-observable: the elapsed wall time is reported to the
        // operator and never reaches a replay's inputs or its state hash.
        #[allow(clippy::disallowed_methods)]
        let started = Instant::now();
        for run in 1..=repeat {
            let mut summary = replay_once(artifacts, &config, actions)?;
            summary.run = run;
            report.horizons_clocked = report
                .horizons_clocked
                .saturating_add(summary.guest_horizons);
            report.bug_found |= summary.bug;
            report.replays.push(summary);
        }
        report.wall_seconds = started.elapsed().as_secs();
        report.write(&options.output)?;
        Ok(report)
    }

    /// Apply one action list to a session no earlier run has touched and
    /// observe the endpoint.
    ///
    /// The session is fresh so that no snapshot an earlier run cached can
    /// stand in for guest execution: the run boots, reaches the sealed setup
    /// point, and executes every action of the list. `run` is set by the
    /// caller that ordered the runs.
    fn replay_once(
        artifacts: &Artifacts,
        config: &FaultConfig,
        actions: &[FaultAction],
    ) -> Result<ReplaySummary, Box<dyn Error>> {
        let mut target = FaultTarget::fresh(&artifacts.kernel, &artifacts.initramfs, config)?;
        for action in actions {
            target.apply(*action);
        }
        // A host-side failure leaves the rest of the list unapplied, so the
        // endpoint is no verdict on the recorded actions.
        if target.failed() {
            return Err(format!(
                "the replay failed after {} of {} actions",
                target.horizons_clocked(),
                actions.len()
            )
            .into());
        }
        let observation = target.observation();
        let summary = ReplaySummary {
            run: 1,
            bug: target.found_bug(),
            stop: observation.stop,
            state_hash: target.state_hash().map(|bytes| sha256_hex(&bytes))?,
            violations: observation.violations.iter().copied().collect(),
            sometimes: observation.sometimes.iter().copied().collect(),
            actions_applied: target.horizons_clocked(),
            guest_horizons: target.guest_horizons_run(),
            observations: observation.clone(),
        };
        Ok(summary)
    }

    fn hostname() -> String {
        std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".to_owned())
    }
}

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
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
            horizon_ms: 500,
            ram_mib: 1024,
            knobs: Vec::new(),
            places: Vec::new(),
            wall_minutes: None,
            output: PathBuf::from("unused"),
        }
    }

    #[test]
    fn a_horizon_in_milliseconds_becomes_guest_nanoseconds() {
        assert_eq!(options().horizon_nanos(), 500_000_000);
        assert_eq!(
            Options {
                horizon_ms: u64::MAX,
                ..options()
            }
            .horizon_nanos(),
            u64::MAX
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
                horizon_ms: 0,
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

    /// Every action list a historical case commits has to keep parsing under
    /// the current vocabulary. A stale one is otherwise caught only by a guest
    /// replay job that costs tens of minutes, long after the change broke it.
    #[test]
    fn every_committed_case_input_parses_under_the_current_vocabulary() {
        let cases = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bugs/historical");
        let mut checked = 0_u32;
        for entry in std::fs::read_dir(&cases).expect("read the historical case directory") {
            let case = entry.expect("read a case directory").path();
            for name in ["probe.json", "witness.json"] {
                let path = case.join(name);
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                parse_recorded_input(&text)
                    .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
                checked += 1;
            }
        }
        assert!(
            checked > 0,
            "no committed case input under {}",
            cases.display()
        );
    }

    fn replay_summary(bug: bool, stop: FaultStop, violations: &[u32]) -> ReplaySummary {
        ReplaySummary {
            run: 1,
            bug,
            stop,
            state_hash: "hash".to_owned(),
            violations: violations.to_vec(),
            sometimes: vec![24],
            actions_applied: 3,
            guest_horizons: 3,
            observations: FaultObservations::default(),
        }
    }

    fn bug_summary(execution: u64, confirmed: bool) -> BugSummary {
        BugSummary {
            execution,
            actions: vec![FaultAction::Hook(3)],
            stop: FaultStop::Assertion { point: 2 },
            violations: vec![2],
            sometimes: vec![24],
            state_hash: "hash".to_owned(),
            confirmed,
            replay: None,
        }
    }

    #[test]
    fn a_replay_confirms_a_bug_only_by_reproducing_its_evidence() {
        let violated = FaultStop::Assertion { point: 2 };
        assert!(
            replay_confirms_bug(violated, &[2], &replay_summary(true, violated, &[2])),
            "the recorded assertion fired again"
        );
        assert!(
            !replay_confirms_bug(
                violated,
                &[2],
                &replay_summary(false, FaultStop::Deadline, &[])
            ),
            "a clean replay confirms nothing"
        );
        assert!(
            !replay_confirms_bug(violated, &[2], &replay_summary(true, FaultStop::Crash, &[])),
            "a crash is not the assertion the campaign recorded"
        );
        assert!(
            !replay_confirms_bug(
                violated,
                &[2],
                &replay_summary(true, FaultStop::Assertion { point: 7 }, &[7])
            ),
            "another assertion is another bug"
        );
    }

    #[test]
    fn a_stop_only_bug_is_confirmed_by_the_same_stop() {
        assert!(replay_confirms_bug(
            FaultStop::Crash,
            &[],
            &replay_summary(true, FaultStop::Crash, &[])
        ));
        assert!(
            !replay_confirms_bug(
                FaultStop::Crash,
                &[],
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
            agent: b"agent".to_vec(),
        };
        let report = Report::new("search", &artifacts, "identity".to_owned(), &options());
        assert_eq!(report.package, PACKAGE);
        assert_eq!(report.kernel_sha256, sha256_hex(b"kernel"));
        assert_eq!(report.image_sha256, sha256_hex(b"initramfs"));
        assert_eq!(report.fault_agent_sha256, sha256_hex(b"agent"));
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
            agent: Vec::new(),
        };
        let report = Report::new("replay", &artifacts, String::new(), &options());
        report.write(directory.path()).expect("write");
        let text = std::fs::read_to_string(directory.path().join("report.json")).expect("read");
        assert_eq!(
            serde_json::from_str::<Report>(&text).expect("decode"),
            report
        );
    }
}
