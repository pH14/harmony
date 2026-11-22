// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    error::Error,
    fs::OpenOptions,
    io::Write,
    num::NonZeroU16,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use searcher::search::rollout::RolloutStop;
use serde::Serialize;

use super::FaultCampaignRun;
use crate::{
    consonance::FaultTarget,
    target::{FaultAction, FaultObservations, FaultOperation},
};

pub(crate) const RECORD_ENV: &str = "HARMONY_FAULTS_SETTLE_RECORD";
pub(crate) const HEAL_ENV: &str = "HARMONY_FAULTS_SETTLE_HEAL";
pub(crate) const END_TICKS: u16 = 16;
pub(crate) const WAIT_TICKS: u16 = 50;
pub(crate) const RECOVERY_LIMIT_TICKS: u64 = 3_000;
pub(crate) const CHECK_LIMIT_TICKS: u64 = 6_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SettlePolicy {
    pub(crate) record: PathBuf,
    pub(crate) heal: Option<u32>,
}

impl SettlePolicy {
    pub(crate) fn parse(
        record: Option<std::ffi::OsString>,
        heal: Option<String>,
    ) -> Result<Option<Self>, String> {
        let Some(record) = record else {
            return Ok(None);
        };
        let heal = heal
            .map(|text| {
                text.parse::<u32>()
                    .map_err(|error| format!("{HEAL_ENV}={text:?}: {error}"))
            })
            .transpose()?;
        Ok(Some(Self {
            record: PathBuf::from(record),
            heal,
        }))
    }
}

pub(crate) fn policy() -> Result<Option<&'static SettlePolicy>, Box<dyn Error>> {
    static POLICY: OnceLock<Result<Option<SettlePolicy>, String>> = OnceLock::new();
    POLICY
        .get_or_init(|| {
            SettlePolicy::parse(std::env::var_os(RECORD_ENV), std::env::var(HEAL_ENV).ok())
        })
        .as_ref()
        .map(Option::as_ref)
        .map_err(|error| error.clone().into())
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct JobStart {
    pub(crate) work: u64,
    pub(crate) fault_free_checks_finished: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Skip {
    Finding,
    Terminal,
    Cancelled,
    Hung(Option<FaultAction>),
}

impl Skip {
    const fn label(self) -> &'static str {
        match self {
            Self::Finding => "skipped: the execution reached its objective",
            Self::Terminal => "skipped: the execution ended terminal",
            Self::Cancelled => "skipped: the job was cancelled",
            Self::Hung(_) => "skipped: the job hung",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Step {
    Continue,
    Compared,
    NotReady,
    NoFaultFreeCheck,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct SettleClock {
    pub(crate) ready_after: Option<u64>,
    pub(crate) compared_after: Option<u64>,
    started_at_ready: u64,
}

impl SettleClock {
    pub(crate) fn observe(
        &mut self,
        since_end: Option<u64>,
        observation: &FaultObservations,
    ) -> Step {
        let Some(since_end) = since_end else {
            return Step::Continue;
        };
        if self.ready_after.is_none()
            && !observation.faults_in_force
            && !observation.recovery_pending
        {
            self.ready_after = Some(since_end);
            self.started_at_ready = observation.fault_free_checks_started;
        }
        if self.ready_after.is_some() && observation.fault_free_check_passed > self.started_at_ready
        {
            self.compared_after = Some(since_end);
            return Step::Compared;
        }
        match self.ready_after {
            None if since_end >= RECOVERY_LIMIT_TICKS => Step::NotReady,
            Some(_) if since_end >= CHECK_LIMIT_TICKS => Step::NoFaultFreeCheck,
            _ => Step::Continue,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub(crate) struct SettleRecord {
    pub(crate) pid: u32,
    pub(crate) path_actions: usize,
    pub(crate) suffix_actions: usize,
    pub(crate) job_work_ticks: u64,
    pub(crate) outcome: &'static str,
    pub(crate) event_kill_armed_at_suffix_end: u64,
    pub(crate) restarted: Vec<u16>,
    pub(crate) healed: bool,
    pub(crate) end_ticks: u64,
    pub(crate) ready_after_ticks: Option<u64>,
    pub(crate) compared_after_ticks: Option<u64>,
    pub(crate) settle_actions: usize,
    pub(crate) settle_ticks: u64,
    pub(crate) fault_free_checks_at_job_start: u64,
    pub(crate) fault_free_checks_after_suffix: u64,
    pub(crate) fault_free_checks_after_settle: u64,
    pub(crate) fault_free_checks_in_execution: u64,
    pub(crate) faults_in_force_after_settle: bool,
    pub(crate) recovery_pending_after_settle: bool,
    pub(crate) violations: Vec<String>,
    pub(crate) actions: Vec<FaultAction>,
    pub(crate) error: Option<String>,
}

pub(crate) fn skipped(
    target: &FaultTarget,
    policy: &SettlePolicy,
    start: JobStart,
    suffix_actions: usize,
    skip: Skip,
) {
    let observation = target.observation();
    let record = SettleRecord {
        pid: std::process::id(),
        path_actions: target.actions().len(),
        suffix_actions,
        job_work_ticks: target.execution_ticks().saturating_sub(start.work),
        outcome: skip.label(),
        fault_free_checks_at_job_start: start.fault_free_checks_finished,
        fault_free_checks_after_suffix: observation.fault_free_checks_finished,
        fault_free_checks_after_settle: observation.fault_free_checks_finished,
        fault_free_checks_in_execution: observation
            .fault_free_checks_finished
            .saturating_sub(start.fault_free_checks_finished),
        faults_in_force_after_settle: observation.faults_in_force,
        recovery_pending_after_settle: observation.recovery_pending,
        actions: target
            .actions()
            .iter()
            .copied()
            .chain(match skip {
                Skip::Hung(action) => action,
                _ => None,
            })
            .collect(),
        ..SettleRecord::default()
    };
    write_record(&policy.record, &record);
}

pub(crate) fn settle_execution(
    run: &FaultCampaignRun,
    target: &mut FaultTarget,
    stop: &RolloutStop,
    policy: &SettlePolicy,
    start: JobStart,
    suffix_actions: usize,
) -> Result<Option<Vec<FaultAction>>, Box<dyn Error>> {
    let mut record = SettleRecord {
        pid: std::process::id(),
        path_actions: target.actions().len(),
        suffix_actions,
        job_work_ticks: target.execution_ticks().saturating_sub(start.work),
        fault_free_checks_at_job_start: start.fault_free_checks_finished,
        fault_free_checks_after_suffix: target.observation().fault_free_checks_finished,
        ..SettleRecord::default()
    };
    let settled = target.run_detached(|target| {
        let found = settle(run, target, stop, policy, &mut record);
        let end = target.observation();
        record.fault_free_checks_after_settle = end.fault_free_checks_finished;
        record.faults_in_force_after_settle = end.faults_in_force;
        record.recovery_pending_after_settle = end.recovery_pending;
        found
    });
    if let Err(error) = &settled {
        record.outcome = "error";
        record.error = Some(error.to_string());
    }
    record.fault_free_checks_in_execution = record
        .fault_free_checks_after_settle
        .saturating_sub(start.fault_free_checks_finished);
    write_record(&policy.record, &record);
    settled
}

fn settle(
    run: &FaultCampaignRun,
    target: &mut FaultTarget,
    stop: &RolloutStop,
    policy: &SettlePolicy,
    record: &mut SettleRecord,
) -> Option<Vec<FaultAction>> {
    let quantum = target
        .actions()
        .last()
        .map_or(NonZeroU16::MIN, |action| action.coverage_quantum);
    let alive = target.observation().alive;
    record.event_kill_armed_at_suffix_end = target.observation().event_kill_armed;
    let mut planned = vec![FaultOperation::EndFaults(ticks(END_TICKS))];
    for node in 0..run.vocabulary.nodes() {
        if alive >> node & 1 == 0 {
            planned.push(FaultOperation::Restart(node, ticks(END_TICKS)));
            record.restarted.push(node);
        }
    }
    if let Some(heal) = policy.heal {
        planned.push(FaultOperation::Hook(heal, ticks(END_TICKS)));
        record.healed = true;
    }
    record.end_ticks = planned
        .iter()
        .map(|operation| crate::target::action_ticks(&FaultAction::new(*operation, quantum)))
        .sum();
    let before = target.observation().violations();
    let began = target.execution_ticks();
    let mut planned = planned.into_iter();
    let mut clock = SettleClock::default();
    let mut found = None;
    loop {
        let elapsed = target.execution_ticks().saturating_sub(began);
        let since_end = (planned.len() == 0).then(|| elapsed.saturating_sub(record.end_ticks));
        let step = clock.observe(since_end, target.observation());
        record.ready_after_ticks = clock.ready_after;
        record.compared_after_ticks = clock.compared_after;
        let ended = match step {
            Step::Compared => Some("compared"),
            Step::NotReady => Some("not ready within the recovery limit"),
            Step::NoFaultFreeCheck => Some("no check without a fault in force finished"),
            Step::Continue if stop.cancelled() => Some("cancelled"),
            Step::Continue => None,
        };
        if let Some(outcome) = ended {
            record.outcome = outcome;
            break;
        }
        let operation = planned
            .next()
            .unwrap_or(FaultOperation::Wait(ticks(WAIT_TICKS)));
        let action = FaultAction::new(operation, quantum);
        let applied = target.actions().len();
        target.apply(action);
        record.settle_actions += 1;
        if target.watchdog_cutoffs() != 0 || target.infrastructure_failure() || target.failed() {
            record.outcome = "incomplete";
            record.actions = target.actions().to_vec();
            if record.actions.len() == applied {
                record.actions.push(action);
            }
            break;
        }
        let violations: Vec<String> = target
            .observation()
            .violations()
            .difference(&before)
            .cloned()
            .collect();
        if !violations.is_empty() && run.reproduced_by(target.observation()) {
            record.outcome = "bug";
            record.violations = violations;
            record.actions = target.actions().to_vec();
            found = record
                .actions
                .get(record.path_actions..)
                .map(<[FaultAction]>::to_vec);
            break;
        }
        if !target.observation().stop.is_continuable() {
            record.outcome = "terminal";
            break;
        }
        if target.rejected() {
            record.outcome = "rejected";
            break;
        }
    }
    record.settle_ticks = target.execution_ticks().saturating_sub(began);
    if record.outcome != "compared" && record.actions.is_empty() {
        record.actions = target.actions().to_vec();
    }
    found
}

fn ticks(value: u16) -> NonZeroU16 {
    NonZeroU16::new(value).unwrap_or(NonZeroU16::MIN)
}

fn write_record(path: &Path, record: &SettleRecord) {
    let mut line = serde_json::to_string(record)
        .unwrap_or_else(|error| format!("{{\"error\":\"settle record: {error}\"}}"));
    line.push('\n');
    match OpenOptions::new().create(true).append(true).open(path) {
        Ok(mut file) => {
            if let Err(error) = file.write_all(line.as_bytes()) {
                eprintln!("settle record {}: {error}", path.display());
            }
        }
        Err(error) => eprintln!("settle record {}: {error}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(in_force: bool, pending: bool, started: u64, passed: u64) -> FaultObservations {
        FaultObservations {
            faults_in_force: in_force,
            recovery_pending: pending,
            fault_free_checks_started: started,
            fault_free_check_passed: passed,
            ..FaultObservations::default()
        }
    }

    #[test]
    fn a_check_started_after_ready_counts_as_the_comparison() {
        let mut clock = SettleClock::default();
        assert_eq!(
            clock.observe(None, &seen(false, false, 4, 4)),
            Step::Continue
        );
        assert_eq!(
            clock.observe(Some(0), &seen(true, false, 4, 4)),
            Step::Continue
        );
        assert_eq!(
            clock.observe(Some(50), &seen(false, true, 4, 4)),
            Step::Continue
        );
        assert_eq!(
            clock.observe(Some(100), &seen(false, false, 4, 4)),
            Step::Continue
        );
        assert_eq!(clock.ready_after, Some(100));
        assert_eq!(
            clock.observe(Some(150), &seen(false, false, 5, 4)),
            Step::Continue
        );
        assert_eq!(
            clock.observe(Some(200), &seen(false, false, 5, 5)),
            Step::Compared
        );
        assert_eq!(clock.compared_after, Some(200));
    }

    #[test]
    fn a_check_running_at_ready_does_not_count() {
        let mut clock = SettleClock::default();
        assert_eq!(
            clock.observe(Some(0), &seen(false, false, 5, 4)),
            Step::Continue
        );
        assert_eq!(
            clock.observe(Some(50), &seen(false, false, 5, 5)),
            Step::Continue
        );
        assert_eq!(
            clock.observe(Some(100), &seen(false, false, 6, 6)),
            Step::Compared
        );
    }

    #[test]
    fn a_failed_check_neither_counts_nor_holds_back_a_later_pass() {
        let mut clock = SettleClock::default();
        assert_eq!(
            clock.observe(Some(0), &seen(false, false, 1, 0)),
            Step::Continue
        );
        assert_eq!(
            clock.observe(Some(50), &seen(false, false, 2, 0)),
            Step::Continue
        );
        assert_eq!(
            clock.observe(Some(100), &seen(false, false, 3, 0)),
            Step::Continue
        );
        assert_eq!(
            clock.observe(Some(150), &seen(false, false, 3, 3)),
            Step::Compared
        );
        assert_eq!(clock.compared_after, Some(150));
    }

    #[test]
    fn readiness_is_bounded_by_the_recovery_limit() {
        let mut clock = SettleClock::default();
        let unready = seen(false, true, 0, 0);
        assert_eq!(
            clock.observe(Some(RECOVERY_LIMIT_TICKS - 1), &unready),
            Step::Continue
        );
        assert_eq!(
            clock.observe(Some(RECOVERY_LIMIT_TICKS), &unready),
            Step::NotReady
        );
    }

    #[test]
    fn the_comparison_is_bounded_by_the_check_limit() {
        let mut clock = SettleClock::default();
        let running = seen(false, false, 1, 0);
        assert_eq!(clock.observe(Some(0), &running), Step::Continue);
        assert_eq!(
            clock.observe(Some(CHECK_LIMIT_TICKS - 1), &running),
            Step::Continue
        );
        assert_eq!(
            clock.observe(Some(CHECK_LIMIT_TICKS), &running),
            Step::NoFaultFreeCheck
        );
    }

    #[test]
    fn a_record_line_is_a_recorded_input() {
        let actions = vec![
            FaultAction::new(FaultOperation::EndFaults(ticks(END_TICKS)), NonZeroU16::MIN),
            FaultAction::new(FaultOperation::Wait(ticks(WAIT_TICKS)), NonZeroU16::MIN),
        ];
        let line = serde_json::to_string(&SettleRecord {
            outcome: "incomplete",
            actions: actions.clone(),
            ..SettleRecord::default()
        })
        .unwrap();
        assert_eq!(
            crate::package::parse_recorded_input(&line).unwrap().actions,
            actions
        );
    }

    #[test]
    fn the_policy_needs_a_record_path_and_a_numeric_heal() {
        assert_eq!(SettlePolicy::parse(None, Some("4".into())), Ok(None));
        assert_eq!(
            SettlePolicy::parse(Some("settle.jsonl".into()), Some("4".into())),
            Ok(Some(SettlePolicy {
                record: PathBuf::from("settle.jsonl"),
                heal: Some(4),
            }))
        );
        assert_eq!(
            SettlePolicy::parse(Some("settle.jsonl".into()), None),
            Ok(Some(SettlePolicy {
                record: PathBuf::from("settle.jsonl"),
                heal: None,
            }))
        );
        assert!(SettlePolicy::parse(Some("settle.jsonl".into()), Some("heal".into())).is_err());
    }
}
