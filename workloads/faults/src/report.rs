// SPDX-License-Identifier: AGPL-3.0-or-later

//! The bug report one terminal endpoint writes: the action list that produced
//! it and the standing-fault window list those actions install, which is what
//! the package stages to replay it.

use std::{error::Error, path::Path};

use serde::{Deserialize, Serialize};

use crate::archive::FaultBugRecord;
use crate::target::{ActionWindows, FaultAction, FaultObservations, standing_windows};

/// File-name stem of a bug report, completed with the bug's ordinal.
pub const BUG_REPORT_STEM: &str = "bug-";

/// One reproducible bug found by a campaign.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BugReport {
    /// One-based ordinal within the campaign.
    pub bug: u64,
    /// Ordered admission position of the execution that found it.
    pub execution: u64,
    /// Root seal `Moment` every action window is measured from.
    pub root_seal: u64,
    /// Virtual nanoseconds each action window spans.
    pub horizon_nanos: u64,
    /// The action list, in execution order.
    pub actions: Vec<FaultAction>,
    /// The encoded standing-fault window list, lowercase hex. These are the
    /// configuration bytes the package hands its service handler to replay
    /// this bug.
    pub standing: String,
    /// The terminal endpoint's observations.
    pub observations: FaultObservations,
}

impl BugReport {
    /// Build a report for the `bug`-th bug of a campaign.
    ///
    /// # Errors
    ///
    /// Returns an error when the actions install a window list the shared
    /// codec refuses to encode.
    pub fn new(
        bug: u64,
        execution: u64,
        windows: ActionWindows,
        actions: &[FaultAction],
        observations: &FaultObservations,
    ) -> Result<Self, Box<dyn Error>> {
        let standing = fault_policy::encode_windows(&standing_windows(windows, actions))?;
        Ok(Self {
            bug,
            execution,
            root_seal: windows.root_seal,
            horizon_nanos: windows.horizon_nanos,
            actions: actions.to_vec(),
            standing: hex(&standing),
            observations: observations.clone(),
        })
    }

    /// The report's file name, `bug-<n>.json`.
    #[must_use]
    pub fn file_name(&self) -> String {
        format!("{BUG_REPORT_STEM}{}.json", self.bug)
    }

    /// Write the report into `directory`.
    ///
    /// # Errors
    ///
    /// Returns an error when the report cannot be serialized or written.
    pub fn write(&self, directory: &Path) -> Result<(), Box<dyn Error>> {
        std::fs::write(
            directory.join(self.file_name()),
            serde_json::to_vec_pretty(self)?,
        )?;
        Ok(())
    }

    /// The standing-fault window list the report carries.
    ///
    /// # Errors
    ///
    /// Returns an error when the hex field is malformed.
    pub fn standing_bytes(&self) -> Result<Vec<u8>, Box<dyn Error>> {
        if !self.standing.len().is_multiple_of(2) {
            return Err("bug report standing list has an odd hex length".into());
        }
        self.standing
            .as_bytes()
            .chunks(2)
            .map(|pair| {
                let text = std::str::from_utf8(pair)?;
                Ok(u8::from_str_radix(text, 16)?)
            })
            .collect()
    }
}

/// Write one report per recorded bug into `directory`, numbered from one in
/// admission order, and return them.
///
/// # Errors
///
/// Returns an error when a report cannot be serialized or written.
pub fn write_bug_reports(
    windows: ActionWindows,
    bugs: &[FaultBugRecord],
    directory: &Path,
) -> Result<Vec<BugReport>, Box<dyn Error>> {
    let mut written = Vec::with_capacity(bugs.len());
    for (index, bug) in bugs.iter().enumerate() {
        let ordinal = u64::try_from(index)?.saturating_add(1);
        let report = BugReport::new(
            ordinal,
            bug.execution,
            windows,
            &bug.input.actions,
            &bug.observations,
        )?;
        report.write(directory)?;
        written.push(report);
    }
    Ok(written)
}

/// What a campaign did, in the terms an operator uses to decide whether it is
/// worth continuing: how much guest time it bought, how much of the workload
/// the oracle actually verified, and whether the faults it drew fired.
///
/// Every field is a plain count. None of them reaches a search decision, an
/// archive key, or a recorded byte. The archive fold fills every field it can
/// see from endpoints; `guest_seconds` and `fired_site` come from the run that
/// clocked the horizons and holds the image's symbol table.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CampaignMeasures {
    /// Guest seconds executed across every admitted action.
    pub guest_seconds: u64,
    /// Greatest count of workload units an endpoint's oracle verified.
    pub acknowledged_writes: u64,
    /// EventKill arms whose coordinate was reached.
    pub kills_fired: u64,
    /// EventKill arms the run ended without reaching.
    pub kills_unfired: u64,
    /// Median agent ticks a fired arm survived between its arm and its kill.
    pub kill_age_ticks_median: u64,
    /// Greatest agent ticks a fired arm survived.
    pub kill_age_ticks_max: u64,
    /// Check runs that reached a verdict.
    pub checks_conclusive: u64,
    /// Check runs that finished without one.
    pub checks_inconclusive: u64,
    /// Symbol of the instrumented site where an arm fired, when the image
    /// named one.
    pub fired_site: Option<String>,
}

/// The fired-arm ages a campaign saw, folded into their median and maximum.
///
/// Ages arrive one at a time and out of order, so the fold keeps them and
/// sorts once. The list is bounded by the campaign's execution budget.
#[derive(Clone, Debug, Default)]
pub struct KillAges(Vec<u64>);

impl KillAges {
    /// Record one fired arm's age in agent ticks.
    pub fn push(&mut self, ticks: u64) {
        self.0.push(ticks);
    }

    /// Median and maximum, both zero when no arm fired.
    #[must_use]
    pub fn summarize(&self) -> (u64, u64) {
        if self.0.is_empty() {
            return (0, 0);
        }
        let mut sorted = self.0.clone();
        sorted.sort_unstable();
        // Even counts take the lower of the two middle values, so the median is
        // always an age some arm actually reached.
        let median = sorted[(sorted.len() - 1) / 2];
        let max = *sorted.last().unwrap_or(&0);
        (median, max)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::{DEFAULT_HORIZON_NANOS, FaultStop};

    const WINDOWS: ActionWindows = ActionWindows {
        root_seal: 1_000,
        horizon_nanos: DEFAULT_HORIZON_NANOS,
    };

    fn sample() -> BugReport {
        let observations = FaultObservations {
            hooks_finished: 2,
            alive: 0b01,
            stop: FaultStop::Assertion { point: 1 },
            ..FaultObservations::default()
        };
        BugReport::new(
            1,
            42,
            WINDOWS,
            &[FaultAction::Hook(1), FaultAction::Kill(0)],
            &observations,
        )
        .expect("build a report")
    }

    #[test]
    fn no_fired_arm_leaves_both_kill_ages_at_zero() {
        assert_eq!(KillAges::default().summarize(), (0, 0));
    }

    #[test]
    fn kill_ages_report_the_lower_middle_value_and_the_greatest() {
        let mut ages = KillAges::default();
        for ticks in [9, 1, 5, 3] {
            ages.push(ticks);
        }
        assert_eq!(ages.summarize(), (3, 9));
        ages.push(7);
        assert_eq!(ages.summarize(), (5, 9));
    }

    #[test]
    fn a_report_carries_the_replayable_window_list_and_the_actions() {
        let report = sample();
        assert_eq!(report.file_name(), "bug-1.json");
        assert_eq!(report.actions.len(), 2);
        let bytes = report.standing_bytes().expect("hex round-trips");
        assert_eq!(report.horizon_nanos, DEFAULT_HORIZON_NANOS);
        assert_eq!(
            fault_policy::decode_windows(&bytes).expect("the window list decodes"),
            standing_windows(WINDOWS, &report.actions),
            "both faulting actions reach the window list"
        );
    }

    #[test]
    fn a_report_round_trips_through_json() {
        let report = sample();
        let text = serde_json::to_string(&report).expect("serialize");
        let decoded: BugReport = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(decoded, report);
        assert_eq!(decoded.observations.stop, FaultStop::Assertion { point: 1 });
    }

    #[test]
    fn a_malformed_hex_field_is_an_error_rather_than_a_panic() {
        let mut report = sample();
        report.standing = "0".to_owned();
        assert!(report.standing_bytes().is_err());
        report.standing = "zz".to_owned();
        assert!(report.standing_bytes().is_err());
    }

    #[test]
    fn reports_are_written_under_their_ordinal() {
        let directory = tempfile::tempdir().expect("temp dir");
        let report = sample();
        report.write(directory.path()).expect("write");
        let text = std::fs::read_to_string(directory.path().join("bug-1.json")).expect("read");
        let decoded: BugReport = serde_json::from_str(&text).expect("decode");
        assert_eq!(decoded, report);
    }

    #[test]
    fn every_recorded_bug_gets_its_own_numbered_report() {
        let directory = tempfile::tempdir().expect("temp dir");
        let bug = |execution, action| FaultBugRecord {
            execution,
            input: crate::archive::FaultInput {
                actions: vec![action],
            },
            observations: FaultObservations {
                stop: FaultStop::Crash,
                ..FaultObservations::default()
            },
        };
        let bugs = [bug(4, FaultAction::Kill(0)), bug(9, FaultAction::Wait(0))];
        let windows = ActionWindows {
            root_seal: 7,
            ..WINDOWS
        };
        let written = write_bug_reports(windows, &bugs, directory.path()).expect("write");
        assert_eq!(written.len(), 2);
        assert_eq!(written[0].bug, 1);
        assert_eq!(written[0].execution, 4);
        assert_eq!(written[1].bug, 2);
        assert_eq!(written[1].execution, 9);
        for name in ["bug-1.json", "bug-2.json"] {
            assert!(directory.path().join(name).exists(), "{name} was written");
        }
    }
}
