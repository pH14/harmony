// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, fs, path::PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::target::{FaultAction, FaultObservations, FaultOperation, FaultStop};

pub const PACKAGE: &str = "faults";
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Options {
    pub seed: u64,
    pub executions: u64,
    pub ram_mib: u32,
    pub knobs: Vec<String>,
    pub wall_seconds: Option<u64>,
    pub output: PathBuf,
    pub uml_profile: Option<PathBuf>,
    pub root: Option<PathBuf>,
}

impl Options {
    pub fn validate(&self) -> Result<(), Box<dyn Error>> {
        if self.executions == 0 {
            return Err("--executions must be positive".into());
        }
        if self.ram_mib == 0 {
            return Err("--ram-mib must be positive".into());
        }
        if [
            "report.json",
            "stream.jsonl",
            "progress.jsonl",
            "campaign-summary.json",
        ]
        .iter()
        .any(|name| self.output.join(name).exists())
        {
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
pub struct ReplayStep {
    pub step: u64,
    pub action: Option<FaultAction>,
    pub observation: FaultObservations,
    pub console: String,
    pub settlement: bool,
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
    pub event_kill_fires: u64,
    pub event_park_fires: u64,
    pub check: Option<crate::target::CheckEvidence>,
    pub timeline: Vec<ReplayStep>,
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
            event_kill_fires: observation.event_kill_fires,
            event_park_fires: observation.event_park_fires,
            check: observation.check.clone(),
            timeline: Vec::new(),
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
    pub ram_mib: u32,
    pub executions: u64,
    pub bug_found: bool,
    pub first_bug_execution: Option<u64>,
    pub bugs: Vec<BugSummary>,
    #[serde(default)]
    pub never_satisfied: Vec<String>,
    pub replays: Vec<ReplaySummary>,
    pub execution_ticks: u64,
    pub wall_seconds: u64,
    #[serde(default)]
    pub watchdog_cutoffs: u64,
    pub execution_failures: u64,
}

impl Report {
    #[must_use]
    pub fn new(
        mode: &str,
        artifacts: &Artifacts,
        identity: String,
        options: &Options,
        workers: u32,
    ) -> Self {
        Self {
            package: PACKAGE.to_owned(),
            mode: mode.to_owned(),
            image_sha256: sha256_hex(&artifacts.initramfs),
            kernel_sha256: sha256_hex(&artifacts.kernel),
            identity,
            seed: options.seed,
            workers,
            ram_mib: options.ram_mib,
            executions: 0,
            bug_found: false,
            first_bug_execution: None,
            bugs: Vec::new(),
            never_satisfied: Vec::new(),
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

#[derive(Clone, Debug, Default)]
pub enum SearchStart {
    #[default]
    Genesis,
    Actions(Vec<FaultAction>),
    Checkpoint(PathBuf),
}

pub struct Artifacts {
    pub kernel: Vec<u8>,
    pub initramfs: Vec<u8>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
enum RecordedInput {
    Actions(Vec<FaultAction>),
    Report { actions: Vec<FaultAction> },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordedActions {
    pub actions: Vec<FaultAction>,
}

pub fn parse_recorded_input(text: &str) -> Result<RecordedActions, Box<dyn Error>> {
    let actions = match serde_json::from_str::<RecordedInput>(text)? {
        RecordedInput::Actions(actions) | RecordedInput::Report { actions } => actions,
    };
    if actions.is_empty() {
        return Err("the recorded input names no actions".into());
    }
    for action in &actions {
        match &action.operation {
            FaultOperation::EventKill { rarity, .. } if *rarity >= 64 => {
                return Err("event rarity exceeds the runtime hash width".into());
            }
            FaultOperation::EventPark { edges, .. }
                if *edges == 0 || *edges > fault_policy::EVENT_PARK_EDGE_LIMIT =>
            {
                return Err("event park edge count is outside the runtime range".into());
            }
            FaultOperation::EventPark { hold_us: 0, .. } => {
                return Err("event park hold must be positive".into());
            }
            FaultOperation::EventPark { hold_us, ticks, .. }
                if u64::from(*hold_us)
                    > u64::from(ticks.get()) * crate::target::SUPERVISOR_TICK_MICROS =>
            {
                return Err("event park hold must fit its window".into());
            }
            _ => {}
        }
    }
    Ok(RecordedActions { actions })
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
    use std::{error::Error, io::BufWriter, sync::Arc, time::Instant};

    use consonance_client::{
        cache::{CacheIndex, LocalIndex, plan_memory, segments},
        placement::{CorePool, Placement, pin_current_thread},
        session::WorkerLauncher,
    };
    use searcher::search::{
        archive::{MAX_ARCHIVE_ENTRIES, RetentionPolicy},
        campaign::{CampaignOrigin, PlacedThread, ThreadPlacement},
        draw::DrawMixture,
    };
    use serde_json::json;

    use super::{
        Artifacts, BugSummary, Options, REPLAY_SETTLE_TICKS, ReplaySummary, Report,
        StateHashEncoding, first_confirmed_bug, replay_confirms_bug, replay_needs_settle,
    };
    use crate::{
        bundle::FaultVocabulary,
        campaign::{FaultCampaignConfig, FaultWorkload, run_fault_campaign_with_plan},
        consonance::{FaultConfig, FaultTarget, GuestBackend, UmlGuest, identity},
        report::{BugReport, write_bug_reports},
        target::{ActionWindows, FaultAction, FaultOperation},
    };

    const MEMORY_BUDGET_MIB: usize = 512;
    const WORKER_OVERHEAD_MIB: u64 = 512;
    #[cfg(target_os = "macos")]
    const VM_WORKER_LIMIT: usize = 4;

    #[derive(Clone, Debug)]
    pub struct Resources {
        pub placement: Placement,
        pub budget: usize,
    }

    pub fn resources(options: &Options) -> Result<Resources, String> {
        let pool = CorePool::detect()?;
        let cores = pool.max_workers();
        #[cfg(target_os = "macos")]
        let cores = cores.min(VM_WORKER_LIMIT);
        let guest = u64::from(options.ram_mib) << 20;
        let memory = plan_memory(
            cores,
            guest.saturating_add(WORKER_OVERHEAD_MIB << 20),
            guest,
        )?;
        let workers = u32::try_from(memory.workers).map_err(|error| error.to_string())?;
        let placement = pool.plan(workers)?;
        eprintln!(
            "{workers} workers on {} cores, {} MiB for the snapshot cache and worker stores",
            pool.cpus().len(),
            memory.budget >> 20
        );
        Ok(Resources {
            placement,
            budget: memory.budget,
        })
    }

    fn pinned(plan: Placement) -> ThreadPlacement {
        ThreadPlacement::new(move |thread| {
            let cpu = match thread {
                PlacedThread::Coordinator => plan.coordinator,
                PlacedThread::Worker(worker) => {
                    plan.workers.get(worker as usize).copied().flatten()
                }
            };
            cpu.map_or(Ok(()), pin_current_thread)
        })
    }

    fn config(options: &Options) -> Result<FaultConfig, String> {
        let backend = match &options.uml_profile {
            None => GuestBackend::Vm,
            Some(profile) => {
                GuestBackend::Uml(UmlGuest::load(profile, options.output.join("uml-work"))?)
            }
        };
        Ok(FaultConfig {
            knobs: options.knobs.clone(),
            ram_mib: options.ram_mib,
            backend,
            root: options
                .root
                .as_ref()
                .map(std::fs::read)
                .transpose()
                .map_err(|e| e.to_string())?
                .map(Arc::new),
        })
    }

    pub fn prepare_debug(
        artifacts: &Artifacts,
        actions: &[FaultAction],
        options: &Options,
    ) -> Result<FaultTarget, Box<dyn Error>> {
        let mut target =
            FaultTarget::fresh(&artifacts.kernel, &artifacts.initramfs, &config(options)?)?;
        for action in actions {
            target.apply_replayed(*action);
        }
        if target.actions().len() != actions.len() || target.failed() {
            return Err("selected prefix could not be reconstructed".into());
        }
        Ok(target)
    }

    pub fn search(
        artifacts: &Artifacts,
        vocabulary: &FaultVocabulary,
        options: &Options,
        resources: &Resources,
        session_worker: Option<WorkerLauncher>,
        start: &super::SearchStart,
    ) -> Result<Report, Box<dyn Error>> {
        options.validate()?;
        let workers = u32::try_from(resources.placement.workers.len())?;
        let config = config(options)?;
        let identity = identity(&artifacts.kernel, &artifacts.initramfs, &config);
        let mut report = Report::new("search", artifacts, identity, options, workers);
        std::fs::create_dir_all(&options.output)?;
        segments::raise_descriptor_limit();
        let cache: Arc<dyn CacheIndex> = Arc::new(LocalIndex::new(resources.budget));
        let game = FaultWorkload::new(&artifacts.kernel, &artifacts.initramfs, &config)
            .with_snapshot_cache(Some(Arc::clone(&cache)))
            .with_session_worker(session_worker);
        let campaign = FaultCampaignConfig {
            campaign_seed: options.seed,
            vocabulary: vocabulary.clone(),
            workers,
            execution_budget: options.executions,
            host: hostname(),
            wall_budget: options.wall_seconds.map(std::time::Duration::from_secs),
            archive_entry_limit: MAX_ARCHIVE_ENTRIES,
            memory_budget_mib: Some(MEMORY_BUDGET_MIB),
            materialize_final_artifacts: true,
            retention: RetentionPolicy::Unprobed,
            mixture: DrawMixture::EnergySplice { scale: 6 },
            objective_witness_path: Some(options.output.join("first-bug-input.json")),
            placement: Some(pinned(resources.placement.clone())),
        };
        #[allow(clippy::disallowed_methods)]
        let started = Instant::now();
        let mut stream =
            BufWriter::new(std::fs::File::create(options.output.join("stream.jsonl"))?);
        let mut progress = std::fs::File::create(options.output.join("progress.jsonl"))?;
        use searcher::search::campaign::{
            CampaignCheckpoint, Reporting, SnapshotCheckpoint, SnapshotCheckpointEntry,
        };
        let origin = match start {
            super::SearchStart::Genesis => CampaignOrigin::Genesis,
            super::SearchStart::Checkpoint(path) => {
                CampaignOrigin::SearchCheckpoint { path: path.clone() }
            }
            super::SearchStart::Actions(actions) => {
                let mut target =
                    FaultTarget::fresh(&artifacts.kernel, &artifacts.initramfs, &config)?;
                for action in actions {
                    target.apply_replayed(*action);
                }
                if target.actions().len() != actions.len() {
                    return Err("branch prefix terminated before its selected point".into());
                }
                let snapshot = target
                    .snapshot()
                    .ok_or("cannot search from a terminal point; rewind further")?;
                let snapshots = SnapshotCheckpoint {
                    format: game.checkpoint_format().into(),
                    entries: vec![SnapshotCheckpointEntry { id: 0, snapshot }],
                };
                let digest = super::sha256_hex(&snapshots.to_bytes()?);
                CampaignOrigin::SnapshotRoot {
                    checkpoint: CampaignCheckpoint {
                        path: "branch-input".into(),
                        file_sha256: digest,
                        snapshots,
                    },
                }
            }
        };
        let (campaign_report, _checkpoint) = run_fault_campaign_with_plan(
            &game,
            &campaign,
            &origin,
            &mut stream,
            Some(&mut progress),
            Some(searcher::search::checkpoint::CheckpointPlan {
                directory: options.output.join("checkpoints"),
                every: std::num::NonZeroU64::new(100),
                on_marks: false,
                on_top_progress: false,
            }),
        )?;
        let archive = &campaign_report.campaign.archive;
        let windows = ActionWindows {
            root_seal: archive.root_seal,
        };
        let written = write_bug_reports(windows, &archive.bugs, &options.output)?;
        if let Some(first) = written.first() {
            std::fs::write(
                options.output.join("first-bug-input.json"),
                serde_json::to_vec_pretty(&first.actions)?,
            )?;
        }
        let summary = json!({
            "mode": "faultlab_campaign",
            "image": game.image_identity(),
            "root_seal": archive.root_seal,
            "vocabulary": vocabulary.identifier(),
            "campaign_seed": campaign_report.campaign.campaign_seed,
            "workers": campaign_report.campaign.telemetry.workers.len(),
            "execution_budget": campaign_report.campaign.execution_budget,
            "executions": campaign_report.campaign.executions_completed,
            "execution_ticks": campaign_report.campaign.execution_work,
            "stream_sha256": campaign_report.campaign.stream_sha256,
            "archive_entries": archive.entries.len(),
            "progress": archive.progress_watermark,
            "milestones": archive.milestones,
            "assertions": archive.assertions,
            "never_satisfied": archive.assertions.never_satisfied(),
            "park_sites": archive.park_sites,
            "park_reads": archive.park_reads,
            "park_thresholds": archive.park_thresholds,
            "bugs_found": campaign_report.bugs_found,
            "executions_to_first_bug": campaign_report.executions_to_first_bug,
            "bug_reports": written.iter().map(BugReport::file_name).collect::<Vec<_>>(),
            "watchdog_cutoffs": archive.watchdog_cutoffs,
            "execution_failures": campaign_report.campaign.execution_failures,
            "telemetry": campaign_report.campaign.telemetry,
            "snapshot_cache": cache
                .stats()
                .counters()
                .into_iter()
                .collect::<std::collections::BTreeMap<_, _>>(),
        });
        std::fs::write(
            options.output.join("campaign-summary.json"),
            serde_json::to_vec_pretty(&summary)?,
        )?;
        report.never_satisfied = archive.assertions.never_satisfied().into_iter().collect();
        for id in &report.never_satisfied {
            eprintln!("FAIL: assertion never satisfied: {id}");
        }
        report.executions = campaign_report.campaign.executions_completed;
        report.execution_ticks = campaign_report.campaign.execution_work;
        for bug in &written {
            let violations: Vec<String> = bug.observations.violations().into_iter().collect();
            let witness = match replay_once(artifacts, &config, &bug.actions, true) {
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
        execute_actions(artifacts, actions, repeat, options, true)
    }

    pub fn execute_actions(
        artifacts: &Artifacts,
        actions: &[FaultAction],
        repeat: u32,
        options: &Options,
        settle: bool,
    ) -> Result<Report, Box<dyn Error>> {
        if repeat == 0 {
            return Err("--repeat must be positive".into());
        }
        let config = config(options)?;
        let identity = identity(&artifacts.kernel, &artifacts.initramfs, &config);
        let mut report = Report::new("replay", artifacts, identity, options, 1);
        #[allow(clippy::disallowed_methods)]
        let started = Instant::now();
        for run in 1..=repeat {
            let mut summary = replay_once(artifacts, &config, actions, settle)?;
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
        settle: bool,
    ) -> Result<ReplaySummary, Box<dyn Error>> {
        let mut target = FaultTarget::fresh(&artifacts.kernel, &artifacts.initramfs, config)?;
        let mut timeline = vec![super::ReplayStep {
            step: 0,
            action: None,
            observation: target.observation().clone(),
            console: target.console_evidence()?,
            settlement: false,
        }];
        for (index, action) in actions.iter().enumerate() {
            target.apply_replayed(*action);
            timeline.push(super::ReplayStep {
                step: index as u64 + 1,
                action: Some(*action),
                observation: target.observation().clone(),
                console: target.console_evidence()?,
                settlement: false,
            });
            if target.actions().len() != index + 1 {
                break;
            }
        }
        let actions_applied = target.actions().len() as u64;
        let mut settle_actions = 0_u64;
        let mut settle_ticks = 0_u64;
        for ticks in REPLAY_SETTLE_TICKS.into_iter().filter(|_| settle) {
            if target.failed() || !replay_needs_settle(target.observation()) {
                break;
            }
            let before = target.actions().len();
            let action = FaultAction::new(
                FaultOperation::Wait(std::num::NonZeroU16::new(ticks).expect("settle duration")),
                actions
                    .last()
                    .map_or(std::num::NonZeroU16::MIN, |action| action.coverage_quantum),
            );
            target.apply_replayed(action);
            timeline.push(super::ReplayStep {
                step: target.actions().len() as u64,
                action: Some(action),
                observation: target.observation().clone(),
                console: target.console_evidence()?,
                settlement: true,
            });
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
        summary.timeline = timeline;
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
pub use live::{Resources, execute_actions, prepare_debug, replay, resources, search};

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> Options {
        Options {
            seed: 7,
            executions: 10,
            ram_mib: 1024,
            knobs: Vec::new(),
            wall_seconds: None,
            output: PathBuf::from("unused"),
            uml_profile: None,
            root: None,
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
                executions: 0,
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
                    .any(|action| matches!(action.operation, FaultOperation::Wait(_)))
            );
            assert!(input.actions.iter().all(|action| match action.operation {
                FaultOperation::Wait(ticks) => ticks.get() == 50,
                _ => true,
            }));
        }
    }

    #[test]
    fn a_recorded_input_reads_as_a_bug_report_or_a_bare_action_list() {
        let ticks = std::num::NonZeroU16::new(50).unwrap();
        let actions = vec![
            FaultOperation::Hook(1, ticks).into(),
            FaultOperation::Kill(0, ticks).into(),
        ];
        let bare = serde_json::to_string(&actions).expect("serialize");
        assert_eq!(
            parse_recorded_input(&bare).expect("bare"),
            RecordedActions {
                actions: actions.clone(),
            }
        );
        let report = serde_json::json!({ "bug": 1, "actions": actions }).to_string();
        assert_eq!(
            parse_recorded_input(&report).expect("report").actions,
            actions
        );
        assert!(parse_recorded_input(r#"[{"Kill":0}]"#).is_err());
        assert!(parse_recorded_input("[]").is_err());
        assert!(parse_recorded_input("{}").is_err());
        let park = |hold_us| {
            serde_json::to_string(&[FaultAction::from(FaultOperation::EventPark {
                node: 0,
                edges: 1,
                hold_us,
                ticks: std::num::NonZeroU16::new(2).unwrap(),
                target: None,
            })])
            .expect("serialize")
        };
        assert!(parse_recorded_input(&park(20_000)).is_ok());
        assert!(parse_recorded_input(&park(20_001)).is_err());
        assert!(parse_recorded_input(&park(0)).is_err());
    }

    fn ids(values: &[u32]) -> Vec<String> {
        values.iter().map(u32::to_string).collect()
    }

    fn replay_summary(bug: bool, stop: FaultStop, violations: &[u32]) -> ReplaySummary {
        ReplaySummary {
            check: None,
            timeline: Vec::new(),
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
            event_kill_fires: 0,
            event_park_fires: 0,
        }
    }

    fn bug_summary(execution: u64, confirmed: bool) -> BugSummary {
        BugSummary {
            execution,
            actions: vec![FaultOperation::Hook(3, std::num::NonZeroU16::new(50).unwrap()).into()],
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
        let report = Report::new("search", &artifacts, "identity".to_owned(), &options(), 2);
        assert_eq!((report.package.as_str(), report.workers), (PACKAGE, 2));
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
        let report = Report::new("replay", &artifacts, String::new(), &options(), 1);
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
            event_kill_fires: 2,
            event_park_fires: 3,
            check: Some(crate::target::CheckEvidence {
                run: 3,
                points: ids(&[7, 11]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let summary = ReplaySummary::from_observation(&observation, state_digest, 1, 1);
        assert_eq!(summary.check, observation.check);
        assert_eq!((summary.event_kill_fires, summary.event_park_fires), (2, 3));
        assert_eq!((summary.settle_actions, summary.settle_ticks), (0, 0));
        let artifacts = Artifacts {
            kernel: Vec::new(),
            initramfs: Vec::new(),
        };
        let mut report = Report::new("replay", &artifacts, String::new(), &options(), 1);
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
        let mut report = Report::new("replay", &artifacts, String::new(), &options(), 1);
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
