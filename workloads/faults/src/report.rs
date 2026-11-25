// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{collections::BTreeSet, error::Error, path::Path};

use fault_policy::StandingWindow;
use serde::{Deserialize, Serialize};

use crate::archive::{FaultBugRecord, FaultFailureRecord, FaultHangRecord};
use crate::guest::{GuestAnswer, answers_through};
use crate::target::{ActionWindows, FaultAction, FaultObservations, FaultStop, standing_windows};

pub const BUG_REPORT_STEM: &str = "bug-";
pub const HANG_REPORT_STEM: &str = "hang-";
pub const FAILURE_REPORT_STEM: &str = "failure-";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BugReport {
    pub bug: u64,
    pub execution: u64,
    pub root_seal: u64,
    pub actions: Vec<FaultAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub answers: Vec<GuestAnswer>,
    pub standing: String,
    pub observations: FaultObservations,
}

impl BugReport {
    pub fn new(
        bug: u64,
        execution: u64,
        windows: ActionWindows,
        inherited: &[StandingWindow],
        actions: &[FaultAction],
        observations: &FaultObservations,
    ) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            bug,
            execution,
            root_seal: windows.root_seal,
            actions: actions.to_vec(),
            answers: Vec::new(),
            standing: standing_hex(windows, inherited, actions)?,
            observations: observations.clone(),
        })
    }

    #[must_use]
    pub fn file_name(&self) -> String {
        format!("{BUG_REPORT_STEM}{}.json", self.bug)
    }

    pub fn write(&self, directory: &Path, earlier: Option<&Path>) -> Result<(), Box<dyn Error>> {
        let bytes = serde_json::to_vec_pretty(self)?;
        let destination = directory.join(self.file_name());
        if let Some(earlier) = earlier.map(|earlier| earlier.join(self.file_name()))
            && std::fs::read(&earlier).is_ok_and(|existing| existing == bytes)
            && link_or_copy(&earlier, &destination).is_ok()
            && std::fs::read(&destination).is_ok_and(|linked| linked == bytes)
        {
            return Ok(());
        }
        replace_file(&destination, &bytes)
    }

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

pub fn write_bug_reports(
    windows: ActionWindows,
    inherited: &[StandingWindow],
    answers: &[GuestAnswer],
    bugs: &[FaultBugRecord],
    directory: &Path,
    earlier: Option<&Path>,
) -> Result<Vec<BugReport>, Box<dyn Error>> {
    let mut written = Vec::with_capacity(bugs.len());
    for (index, bug) in bugs.iter().enumerate() {
        let ordinal = u64::try_from(index)?.saturating_add(1);
        let mut report = BugReport::new(
            ordinal,
            bug.execution,
            windows,
            inherited,
            &bug.input.actions,
            &bug.observations,
        )?;
        report.answers = answers_through(answers, bug.input.actions.len());
        report.write(directory, earlier)?;
        written.push(report);
    }
    Ok(written)
}

pub fn link_or_copy(source: &Path, destination: &Path) -> Result<(), Box<dyn Error>> {
    let staging = tempfile::Builder::new().prefix(".link").tempdir_in(
        destination
            .parent()
            .ok_or("link destination needs a parent")?,
    )?;
    let staged = staging.path().join("file");
    let regular = std::fs::symlink_metadata(source)
        .map_err(|error| format!("cannot read {}: {error}", source.display()))?
        .file_type()
        .is_file();
    let linked = regular
        && std::fs::hard_link(source, &staged).is_ok()
        && std::fs::symlink_metadata(&staged)?.file_type().is_file();
    if !linked {
        if std::fs::symlink_metadata(&staged).is_ok() {
            std::fs::remove_file(&staged)?;
        }
        std::fs::copy(source, &staged)
            .map_err(|error| format!("cannot copy {}: {error}", source.display()))?;
    }
    std::fs::rename(&staged, destination)?;
    Ok(())
}

pub fn replace_file(destination: &Path, bytes: &[u8]) -> Result<(), Box<dyn Error>> {
    let temp = tempfile::NamedTempFile::new_in(
        destination
            .parent()
            .ok_or("file destination needs a parent")?,
    )?;
    std::fs::write(temp.path(), bytes)?;
    temp.persist(destination)?;
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ViolationClass {
    pub stop: FaultStop,
    pub violations: BTreeSet<String>,
    pub reports: Vec<String>,
}

#[must_use]
pub fn violation_classes(reports: &[BugReport]) -> Vec<ViolationClass> {
    let mut classes: Vec<ViolationClass> = Vec::new();
    for report in reports {
        let stop = report.observations.stop;
        let violations = report.observations.violations();
        match classes
            .iter_mut()
            .find(|class| class.stop == stop && class.violations == violations)
        {
            Some(class) => class.reports.push(report.file_name()),
            None => classes.push(ViolationClass {
                stop,
                violations,
                reports: vec![report.file_name()],
            }),
        }
    }
    classes
}

#[must_use]
pub fn distinct_reports(reports: &[BugReport]) -> usize {
    reports
        .iter()
        .map(|report| report.execution)
        .collect::<BTreeSet<_>>()
        .len()
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HangReport {
    pub hang: u64,
    pub execution: u64,
    pub root_seal: u64,
    pub actions: Vec<FaultAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub answers: Vec<GuestAnswer>,
    pub standing: String,
}

impl HangReport {
    #[must_use]
    pub fn file_name(&self) -> String {
        format!("{HANG_REPORT_STEM}{}.json", self.hang)
    }
}

pub fn write_hang_reports(
    windows: ActionWindows,
    inherited: &[StandingWindow],
    answers: &[GuestAnswer],
    hangs: &[FaultHangRecord],
    directory: &Path,
) -> Result<Vec<HangReport>, Box<dyn Error>> {
    let mut written = Vec::with_capacity(hangs.len());
    for (index, hang) in hangs.iter().enumerate() {
        let report = HangReport {
            hang: u64::try_from(index)?.saturating_add(1),
            execution: hang.execution,
            root_seal: windows.root_seal,
            actions: hang.input.actions.clone(),
            answers: answers_through(answers, hang.input.actions.len()),
            standing: standing_hex(windows, inherited, &hang.input.actions)?,
        };
        std::fs::write(
            directory.join(report.file_name()),
            serde_json::to_vec_pretty(&report)?,
        )?;
        written.push(report);
    }
    Ok(written)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FailureReport {
    pub failure: u64,
    pub execution: u64,
    pub root_seal: u64,
    pub actions: Vec<FaultAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub answers: Vec<GuestAnswer>,
    pub standing: String,
}

impl FailureReport {
    #[must_use]
    pub fn file_name(&self) -> String {
        format!("{FAILURE_REPORT_STEM}{}.json", self.failure)
    }
}

pub fn write_failure_reports(
    windows: ActionWindows,
    inherited: &[StandingWindow],
    answers: &[GuestAnswer],
    failures: &[FaultFailureRecord],
    directory: &Path,
) -> Result<Vec<FailureReport>, Box<dyn Error>> {
    let mut written = Vec::with_capacity(failures.len());
    for (index, failure) in failures.iter().enumerate() {
        let report = FailureReport {
            failure: u64::try_from(index)?.saturating_add(1),
            execution: failure.execution,
            root_seal: windows.root_seal,
            actions: failure.input.actions.clone(),
            answers: answers_through(answers, failure.input.actions.len()),
            standing: standing_hex(windows, inherited, &failure.input.actions)?,
        };
        std::fs::write(
            directory.join(report.file_name()),
            serde_json::to_vec_pretty(&report)?,
        )?;
        written.push(report);
    }
    Ok(written)
}

fn standing_hex(
    windows: ActionWindows,
    inherited: &[StandingWindow],
    actions: &[FaultAction],
) -> Result<String, Box<dyn Error>> {
    let standing = standing_windows(windows, inherited, actions)?;
    Ok(hex(&fault_policy::encode_windows(&standing)?))
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
    use crate::assertion::{AssertionKind, AssertionOutcome, Assertions};
    use crate::target::FaultOperation;
    use std::num::NonZeroU16;

    const WINDOWS: ActionWindows = ActionWindows {
        root_seal: 1_000,
        hold_hooks: false,
    };
    const TICKS: NonZeroU16 = NonZeroU16::new(50).unwrap();

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
            &[],
            &[
                FaultOperation::Hook(1, TICKS).into(),
                FaultOperation::Kill(0, TICKS).into(),
            ],
            &observations,
        )
        .expect("build a report")
    }

    #[test]
    fn reports_with_one_stop_and_violation_set_share_a_class() {
        let report = |bug, execution, point, violated: &[&str]| {
            let mut assertions = Assertions::default();
            for id in violated {
                assertions.record(
                    (*id).to_owned(),
                    AssertionOutcome {
                        kind: AssertionKind::Always,
                        message: (*id).to_owned(),
                        location: String::new(),
                        passed: true,
                        failed: true,
                        holds: None,
                        failure_details: None,
                    },
                );
            }
            let observations = FaultObservations {
                stop: FaultStop::Assertion { point },
                assertions,
                ..FaultObservations::default()
            };
            BugReport::new(bug, execution, WINDOWS, &[], &[], &observations)
                .expect("build a report")
        };
        let reports = [
            report(1, 10, 1, &["register"]),
            report(2, 10, 1, &["register", "liveness"]),
            report(3, 20, 1, &["register"]),
            report(4, 30, 2, &["register"]),
        ];
        assert_eq!(
            violation_classes(&reports)
                .iter()
                .map(|class| class.reports.clone())
                .collect::<Vec<_>>(),
            [
                vec!["bug-1.json".to_owned(), "bug-3.json".to_owned()],
                vec!["bug-2.json".to_owned()],
                vec!["bug-4.json".to_owned()],
            ]
        );
        assert_eq!(distinct_reports(&reports), 3);
    }

    #[test]
    fn a_report_carries_the_replayable_window_list_and_the_actions() {
        let report = sample();
        assert_eq!(report.file_name(), "bug-1.json");
        assert_eq!(report.actions.len(), 2);
        let bytes = report.standing_bytes().expect("hex round-trips");
        assert_eq!(
            fault_policy::decode_windows(&bytes).expect("the window list decodes"),
            standing_windows(WINDOWS, &[], &report.actions).unwrap(),
            "both faulting actions reach the window list"
        );
    }

    #[test]
    fn a_rooted_report_carries_inherited_standing_windows() {
        let origin = ActionWindows {
            root_seal: 0,
            hold_hooks: false,
        };
        let prefix = [FaultAction::from(FaultOperation::EventKill {
            node: 0,
            rarity: 1,
            ticks: TICKS,
        })];
        let inherited = standing_windows(origin, &[], &prefix).unwrap();
        let rooted = ActionWindows {
            root_seal: origin.window(&prefix, 0).unwrap().1,
            hold_hooks: false,
        };
        let report = BugReport::new(
            1,
            42,
            rooted,
            &inherited,
            &[FaultOperation::Wait(NonZeroU16::MIN).into()],
            &FaultObservations::default(),
        )
        .unwrap();
        assert_eq!(
            fault_policy::decode_windows(&report.standing_bytes().unwrap()).unwrap(),
            inherited
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
        report.write(directory.path(), None).expect("write");
        let text = std::fs::read_to_string(directory.path().join("bug-1.json")).expect("read");
        let decoded: BugReport = serde_json::from_str(&text).expect("decode");
        assert_eq!(decoded, report);
    }

    #[cfg(unix)]
    #[test]
    fn a_report_equal_to_the_earlier_chunk_is_linked_and_a_changed_one_is_written() {
        use std::os::unix::fs::MetadataExt;
        let earlier = tempfile::tempdir().expect("temp dir");
        let directory = tempfile::tempdir().expect("temp dir");
        let report = sample();
        report.write(earlier.path(), None).expect("write");
        report
            .write(directory.path(), Some(earlier.path()))
            .expect("link");
        let inode = |path: &Path| {
            let metadata = std::fs::metadata(path.join("bug-1.json")).expect("metadata");
            (metadata.dev(), metadata.ino())
        };
        assert_eq!(inode(earlier.path()), inode(directory.path()));

        let changed = tempfile::tempdir().expect("temp dir");
        let mut other = sample();
        other.execution += 1;
        other
            .write(changed.path(), Some(earlier.path()))
            .expect("write");
        assert_ne!(inode(earlier.path()), inode(changed.path()));
        let text = std::fs::read_to_string(changed.path().join("bug-1.json")).expect("read");
        assert_eq!(
            serde_json::from_str::<BugReport>(&text).expect("decode"),
            other
        );
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
        let bugs = [
            bug(4, FaultOperation::Kill(0, TICKS).into()),
            bug(9, FaultOperation::Wait(NonZeroU16::MIN).into()),
        ];
        let windows = ActionWindows {
            root_seal: 7,
            hold_hooks: false,
        };
        let answer = |step| GuestAnswer {
            step,
            choice: 0,
            label: Some(1),
        };
        let written = write_bug_reports(
            windows,
            &[],
            &[answer(1), answer(2)],
            &bugs,
            directory.path(),
            None,
        )
        .expect("write");
        assert_eq!(written[0].answers, vec![answer(1)]);
        let text = std::fs::read_to_string(directory.path().join("bug-1.json")).expect("read");
        assert_eq!(
            crate::package::parse_recorded_input(&text)
                .expect("recorded input")
                .answers,
            vec![answer(1)]
        );
        assert_eq!(written.len(), 2);
        assert_eq!(written[0].bug, 1);
        assert_eq!(written[0].execution, 4);
        assert_eq!(written[1].bug, 2);
        assert_eq!(written[1].execution, 9);
        for name in ["bug-1.json", "bug-2.json"] {
            assert!(directory.path().join(name).exists(), "{name} was written");
        }
    }

    #[test]
    fn every_recorded_hang_gets_a_numbered_replayable_input() {
        let directory = tempfile::tempdir().expect("temp dir");
        let actions = vec![
            FaultOperation::Kill(0, TICKS).into(),
            FaultOperation::Wait(NonZeroU16::MIN).into(),
        ];
        let hangs = [FaultHangRecord {
            execution: 12,
            input: crate::archive::FaultInput {
                actions: actions.clone(),
            },
        }];
        let written =
            write_hang_reports(WINDOWS, &[], &[], &hangs, directory.path()).expect("write");
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].file_name(), "hang-1.json");
        assert_eq!(written[0].execution, 12);
        let text = std::fs::read_to_string(directory.path().join("hang-1.json")).expect("read");
        assert_eq!(
            crate::package::parse_recorded_input(&text)
                .expect("a hang report is a recorded input")
                .actions,
            actions
        );
        let decoded: HangReport = serde_json::from_str(&text).expect("decode");
        assert_eq!(decoded, written[0]);
        assert_eq!(
            decoded.standing,
            BugReport::new(1, 12, WINDOWS, &[], &actions, &FaultObservations::default())
                .unwrap()
                .standing
        );
    }

    #[test]
    fn every_recorded_failure_gets_a_numbered_replayable_input() {
        let directory = tempfile::tempdir().expect("temp dir");
        let actions = vec![
            FaultOperation::Kill(0, TICKS).into(),
            FaultOperation::Wait(NonZeroU16::MIN).into(),
        ];
        let failures = [FaultFailureRecord {
            execution: 12,
            input: crate::archive::FaultInput {
                actions: actions.clone(),
            },
        }];
        let written =
            write_failure_reports(WINDOWS, &[], &[], &failures, directory.path()).expect("write");
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].file_name(), "failure-1.json");
        assert_eq!(written[0].execution, 12);
        let text = std::fs::read_to_string(directory.path().join("failure-1.json")).expect("read");
        assert_eq!(
            crate::package::parse_recorded_input(&text)
                .expect("a failure report is a recorded input")
                .actions,
            actions
        );
        let decoded: FailureReport = serde_json::from_str(&text).expect("decode");
        assert_eq!(decoded, written[0]);
        assert_eq!(
            decoded.standing,
            BugReport::new(1, 12, WINDOWS, &[], &actions, &FaultObservations::default())
                .unwrap()
                .standing
        );
    }
}
