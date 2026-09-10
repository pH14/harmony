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

use crate::target::{FaultAction, FaultObservations, FaultStop};

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

    /// Validate replay bounds and its fresh output directory before booting.
    pub fn validate_replay(
        &self,
        actions: &[FaultAction],
        repeat: u32,
    ) -> Result<(), Box<dyn Error>> {
        self.validate()?;
        if repeat == 0 || actions.is_empty() {
            return Err("replay needs a nonempty action list and a positive repeat count".into());
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
    /// Whole-VM state hash at the terminal endpoint, lowercase hex. Its
    /// encoding is identified by [`state_hash_encoding`](Self::state_hash_encoding).
    pub state_hash: String,
    /// How [`Self::state_hash`] was encoded. Missing in legacy JSON reports,
    /// which the deserializer treats as [`StateHashEncoding::LegacySha256OfDigest`].
    #[serde(default)]
    pub state_hash_encoding: StateHashEncoding,
    /// Whether replaying the action list reproduced this bug's evidence.
    pub confirmed: bool,
    /// The confirming replay run, absent when the replay could not run.
    pub replay: Option<ReplaySummary>,
}

/// How a report `state_hash` string was encoded.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StateHashEncoding {
    /// The engine's SHA-256 digest was encoded directly as lowercase hex.
    EngineDigest,
    /// Legacy reports hashed the engine's digest with SHA-256 once more.
    #[default]
    LegacySha256OfDigest,
}

/// Versioned complete SDK event evidence retained for each replay run.
#[path = "package/replay_evidence.rs"]
pub mod replay_evidence;

/// One run of a replayed action list.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReplaySummary {
    /// One-based run ordinal.
    pub run: u32,
    /// Whether this run reproduced the bug.
    pub bug: bool,
    /// How the guest stopped.
    pub stop: FaultStop,
    /// Whole-VM state hash at the endpoint, lowercase hex. Its encoding is
    /// identified by [`state_hash_encoding`](Self::state_hash_encoding).
    pub state_hash: String,
    /// How [`Self::state_hash`] was encoded. Missing in legacy JSON reports,
    /// which the deserializer treats as [`StateHashEncoding::LegacySha256OfDigest`].
    #[serde(default)]
    pub state_hash_encoding: StateHashEncoding,
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
}

impl ReplaySummary {
    /// Build a report summary for one terminal endpoint.
    ///
    /// The execution engine already returns a state digest. It is encoded
    /// directly here; artifact bytes take the separate SHA-256 path below.
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
            violations: observation.violations.iter().copied().collect(),
            sometimes: observation.sometimes.iter().copied().collect(),
            actions_applied,
            guest_horizons,
        }
    }
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

/// Encode an engine-produced state digest without hashing it again.
fn state_digest_hex(digest: &[u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Validate all endpoint evidence read from one stopped replay.
///
/// The event stream is decoded in full to rebuild the target observations, and
/// the hash is checked on both sides of the read. This keeps a sidecar from
/// claiming a different endpoint when a control-plane read fails or races an
/// unexpected state change.
#[cfg(any(
    test,
    all(
        feature = "consonance",
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    )
))]
fn validate_replay_capture(
    observation: &FaultObservations,
    raw_events: &[(u64, u32, Vec<u8>)],
    before_hash: [u8; 32],
    after_hash: [u8; 32],
) -> Result<Vec<replay_evidence::SdkEventRecord>, String> {
    if before_hash != after_hash {
        return Err("replay endpoint state changed while reading SDK events".to_owned());
    }
    if raw_events
        .iter()
        .any(|(virtual_time, _, _)| *virtual_time > observation.moment)
    {
        return Err("replay SDK events contain an event after the endpoint moment".to_owned());
    }
    let capture = crate::target::decode_sdk_events(raw_events)?;
    let decoded = FaultObservations::new(observation.moment, &capture, observation.stop);
    if decoded != *observation {
        return Err("replay SDK events do not reproduce the target's full observations".to_owned());
    }
    replay_evidence::records_from_raw(raw_events)
}

/// Retain the first actual replay finding, keeping its observations and hash
/// from that same run. Later repetitions remain separate replay results.
#[cfg(any(
    test,
    all(
        feature = "consonance",
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    )
))]
fn retain_replay_finding(
    report: &mut Report,
    bug: &crate::report::BugReport,
    summary: &ReplaySummary,
    output: &std::path::Path,
) -> Result<(), Box<dyn Error>> {
    if !summary.bug
        || summary.stop != bug.observations.stop
        || summary.violations
            != bug
                .observations
                .violations
                .iter()
                .copied()
                .collect::<Vec<_>>()
        || summary.sometimes
            != bug
                .observations
                .sometimes
                .iter()
                .copied()
                .collect::<Vec<_>>()
    {
        return Err("replay finding and summary describe different observations".into());
    }
    if report.bugs.is_empty() {
        fs::create_dir_all(output)?;
        bug.write(output)?;
        report.bugs.push(BugSummary {
            execution: bug.execution,
            actions: bug.actions.clone(),
            stop: bug.observations.stop,
            violations: summary.violations.clone(),
            sometimes: summary.sometimes.clone(),
            state_hash: summary.state_hash.clone(),
            state_hash_encoding: summary.state_hash_encoding,
            confirmed: true,
            replay: Some(summary.clone()),
        });
        report.first_bug_execution = Some(bug.execution);
    }
    Ok(())
}

#[cfg(any(
    test,
    all(
        feature = "consonance",
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    )
))]
fn replay_confirms_endpoint(
    recorded: &FaultObservations,
    observed: Option<&FaultObservations>,
    replay: &ReplaySummary,
) -> bool {
    replay.bug && observed == Some(recorded)
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
        replay_evidence::ReplayEventEvidence,
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
            "bugs_found": campaign_report.bugs_found,
            "executions_to_first_bug": campaign_report.executions_to_first_bug,
            "bug_reports": written.iter().map(BugReport::file_name).collect::<Vec<_>>(),
        });
        std::fs::write(
            options.output.join("campaign-summary.json"),
            serde_json::to_vec_pretty(&summary)?,
        )?;
        report.executions = campaign_report.campaign.executions_completed;
        report.horizons_clocked = campaign_report.campaign.frames_emulated;
        for (index, bug) in written.iter().enumerate() {
            // A campaign hit is a claim about an action list, so each one is
            // replayed from a fresh session: the replay both supplies the
            // guest state hash the campaign never recorded and decides whether
            // the hit is a rediscovery.
            let violations: Vec<u32> = bug.observations.violations.iter().copied().collect();
            let run = u32::try_from(index.saturating_add(1))?;
            let witness = match replay_once(artifacts, &config, &bug.actions) {
                Ok(mut witness) => {
                    witness.summary.run = run;
                    witness.events.run = run;
                    witness.events.write(&options.output)?;
                    Some(witness)
                }
                Err(error) => {
                    eprintln!("bug {} did not replay: {error}", bug.bug);
                    None
                }
            };
            let confirmed = witness.as_ref().is_some_and(|witness| {
                witness.reproduction.as_ref().is_some_and(|observed| {
                    super::replay_confirms_endpoint(
                        &bug.observations,
                        Some(&observed.observations),
                        &witness.summary,
                    )
                })
            });
            let summary = witness.as_ref().map(|witness| &witness.summary);
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
                state_hash: summary.map_or_else(String::new, |summary| summary.state_hash.clone()),
                state_hash_encoding: summary
                    .map_or(super::StateHashEncoding::EngineDigest, |summary| {
                        summary.state_hash_encoding
                    }),
                confirmed,
                replay: summary.cloned(),
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
        options.validate_replay(actions, repeat)?;
        let config = config(options);
        let identity = identity(&artifacts.kernel, &artifacts.initramfs, &config);
        let mut report = Report::new("replay", artifacts, identity, options);
        // not order-observable: the elapsed wall time is reported to the
        // operator and never reaches a replay's inputs or its state hash.
        #[allow(clippy::disallowed_methods)]
        let started = Instant::now();
        for run in 1..=repeat {
            let mut witness = replay_once(artifacts, &config, actions)?;
            witness.summary.run = run;
            witness.events.run = run;
            // The sidecar is durable evidence for the run and must exist
            // before report.json (or its retained bug) is published.
            witness.events.write(&options.output)?;
            if let Some(mut bug) = witness.reproduction {
                bug.execution = u64::from(run);
                super::retain_replay_finding(&mut report, &bug, &witness.summary, &options.output)?;
            }
            report.executions = u64::from(run);
            report.horizons_clocked = report
                .horizons_clocked
                .saturating_add(witness.summary.guest_horizons);
            report.bug_found |= witness.summary.bug;
            report.replays.push(witness.summary);
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
    struct ReplayWitness {
        summary: ReplaySummary,
        reproduction: Option<crate::report::BugReport>,
        events: ReplayEventEvidence,
    }

    fn replay_once(
        artifacts: &Artifacts,
        config: &FaultConfig,
        actions: &[FaultAction],
    ) -> Result<ReplayWitness, Box<dyn Error>> {
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
        let observation = target.observation().clone();
        let state_hash = target.state_hash()?;
        let raw_events = target.sdk_events()?;
        let after_hash = target.state_hash()?;
        let event_records =
            super::validate_replay_capture(&observation, &raw_events, state_hash, after_hash)?;
        let events = ReplayEventEvidence::new(
            1,
            observation.moment,
            super::state_digest_hex(&state_hash),
            event_records,
        )?;
        let summary = ReplaySummary::from_observation(
            &observation,
            state_hash,
            target.horizons_clocked(),
            target.guest_horizons_run(),
        );
        let reproduction = summary
            .bug
            .then(|| {
                crate::report::BugReport::new(
                    1,
                    1,
                    crate::target::ActionWindows {
                        root_seal: target.root_seal(),
                        horizon_nanos: config.horizon_nanos,
                    },
                    actions,
                    &observation,
                )
            })
            .transpose()?;
        Ok(ReplayWitness {
            summary,
            reproduction,
            events,
        })
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
    use crate::target::decode_sdk_events;

    #[test]
    fn replay_capture_rejects_changed_events_moments_and_hashes() {
        let raw_events = vec![(5, (1_u32 << 24) | 7, vec![1, 0, 0])];
        let capture = decode_sdk_events(&raw_events).expect("valid violation event");
        let observation = FaultObservations::new(10, &capture, FaultStop::Assertion { point: 7 });
        let hash = [0x42; 32];
        let records = validate_replay_capture(&observation, &raw_events, hash, hash)
            .expect("the unchanged endpoint is accepted");
        assert_eq!(records[0].position, 0);

        let mut changed_payload = raw_events.clone();
        changed_payload[0].2[0] = 0;
        assert!(decode_sdk_events(&changed_payload).is_ok());
        assert!(validate_replay_capture(&observation, &changed_payload, hash, hash).is_err());

        let mut changed_moment = raw_events.clone();
        changed_moment[0].0 = observation.moment + 1;
        assert!(validate_replay_capture(&observation, &changed_moment, hash, hash).is_err());

        assert!(validate_replay_capture(&observation, &raw_events, hash, [0x43; 32]).is_err());
    }

    #[test]
    fn confirmation_requires_the_recorded_moment_and_all_observations() {
        let recorded = FaultObservations {
            moment: 123,
            stop: FaultStop::Assertion { point: 7 },
            violations: [7].into_iter().collect(),
            sometimes: [8].into_iter().collect(),
            ticks: 5,
            ..FaultObservations::default()
        };
        let replay = ReplaySummary::from_observation(&recorded, [0x42; 32], 1, 1);
        assert!(replay_confirms_endpoint(
            &recorded,
            Some(&recorded),
            &replay
        ));
        let mut changed = recorded.clone();
        changed.sometimes.clear();
        assert!(!replay_confirms_endpoint(
            &recorded,
            Some(&changed),
            &replay
        ));
        changed = recorded.clone();
        changed.moment += 1;
        assert!(!replay_confirms_endpoint(
            &recorded,
            Some(&changed),
            &replay
        ));
        changed = recorded.clone();
        changed.ticks += 1;
        assert!(!replay_confirms_endpoint(
            &recorded,
            Some(&changed),
            &replay
        ));
        assert!(!replay_confirms_endpoint(&recorded, None, &replay));
    }

    #[test]
    fn replay_publishes_the_same_run_observations_and_raw_digest() {
        let directory = tempfile::tempdir().unwrap();
        let artifacts = Artifacts {
            kernel: b"kernel".to_vec(),
            initramfs: b"image".to_vec(),
            agent: b"agent".to_vec(),
        };
        let mut report = Report::new("replay", &artifacts, "identity".to_owned(), &options());
        let observations = FaultObservations {
            moment: 123,
            stop: FaultStop::Assertion { point: 7 },
            violations: [7].into_iter().collect(),
            ..FaultObservations::default()
        };
        let windows = crate::target::ActionWindows {
            root_seal: 10,
            horizon_nanos: 100,
        };
        let bug = crate::report::BugReport::new(1, 1, windows, &[FaultAction::Wait], &observations)
            .unwrap();
        let summary = ReplaySummary::from_observation(&observations, [0x42; 32], 1, 1);
        let mut inconsistent = summary.clone();
        inconsistent.violations = vec![8];
        assert!(retain_replay_finding(&mut report, &bug, &inconsistent, directory.path()).is_err());
        assert!(!directory.path().join(bug.file_name()).exists());
        retain_replay_finding(&mut report, &bug, &summary, directory.path()).unwrap();
        assert_eq!(report.bugs[0].state_hash, "42".repeat(32));
        assert_eq!(
            report.bugs[0].state_hash_encoding,
            StateHashEncoding::EngineDigest
        );
        let first_bytes = fs::read(directory.path().join(bug.file_name())).unwrap();
        let mut later = bug.clone();
        later.execution = 2;
        later.observations.moment = 124;
        let later_summary = ReplaySummary::from_observation(&later.observations, [0x43; 32], 1, 1);
        retain_replay_finding(&mut report, &later, &later_summary, directory.path()).unwrap();
        assert_eq!(report.bugs.len(), 1);
        assert_eq!(
            fs::read(directory.path().join(bug.file_name())).unwrap(),
            first_bytes
        );
        report.write(directory.path()).unwrap();
        let workspace = crate::workspace::publish(
            directory.path(),
            "service.oci",
            "node service /service\n",
            &report,
            &options(),
        )
        .unwrap();
        let finding = workspace.finding("bug-1").unwrap();
        assert_eq!(finding.observations, observations);
        assert_eq!(finding.state_hash, summary.state_hash);
        assert_eq!(finding.state_hash_encoding, StateHashEncoding::EngineDigest);
        assert_eq!(workspace.facts().root_seal, windows.root_seal);
    }

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
    fn replay_rejects_invalid_bounds_and_preserves_existing_output() {
        let directory = tempfile::tempdir().unwrap();
        let mut options = options();
        options.output = directory.path().to_path_buf();
        assert!(options.validate_replay(&[FaultAction::Wait], 1).is_ok());
        assert!(options.validate_replay(&[], 1).is_err());
        assert!(options.validate_replay(&[FaultAction::Wait], 0).is_err());
        options.ram_mib = 0;
        assert!(options.validate_replay(&[FaultAction::Wait], 1).is_err());
        options.ram_mib = 1024;
        let retained = directory.path().join("report.json");
        fs::write(&retained, b"existing report").unwrap();
        assert!(options.validate_replay(&[FaultAction::Wait], 1).is_err());
        assert_eq!(fs::read(&retained).unwrap(), b"existing report");
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

    fn replay_summary(bug: bool, stop: FaultStop, violations: &[u32]) -> ReplaySummary {
        ReplaySummary {
            run: 1,
            bug,
            stop,
            state_hash: "hash".to_owned(),
            state_hash_encoding: StateHashEncoding::LegacySha256OfDigest,
            violations: violations.to_vec(),
            sometimes: vec![24],
            actions_applied: 3,
            guest_horizons: 3,
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
            state_hash_encoding: StateHashEncoding::LegacySha256OfDigest,
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

    #[test]
    fn a_replay_report_serializes_the_engine_digest_without_hashing_it_again() {
        let state_digest = [
            0x14, 0xbc, 0xef, 0x45, 0x9b, 0x6a, 0x75, 0x95, 0x9b, 0x50, 0x36, 0x70, 0x94, 0xbf,
            0x7f, 0x48, 0x2b, 0x55, 0xd5, 0x34, 0xbd, 0xb7, 0x94, 0xfd, 0x79, 0x0f, 0x37, 0xab,
            0x70, 0xcc, 0x5c, 0x96,
        ];
        let observation = FaultObservations::default();
        let summary = ReplaySummary::from_observation(&observation, state_digest, 1, 1);
        let artifacts = Artifacts {
            kernel: Vec::new(),
            initramfs: Vec::new(),
            agent: Vec::new(),
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
            agent: Vec::new(),
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
