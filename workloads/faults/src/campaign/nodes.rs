// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use searcher::search::choice::{Choice, ChoicePlan, ChoiceProfile, ChoiceSite, Chooser};
use serde::Serialize;

use crate::{
    bundle::FaultVocabulary,
    guest::{ActionChoice, Asker},
    target::{FaultAction, FaultOperation},
};

pub const NODE_SITE: &str = "fault.node";
pub(crate) const NODE_SITE_ID: u16 = 1;
pub(crate) const RECORD_ENV: &str = "HARMONY_FAULTS_CHOICE_RECORD";
const SEED_MIX: u64 = 0x6e6f_6465_5f73_6974;

pub(crate) fn site(vocabulary: &FaultVocabulary) -> ChoiceSite {
    ChoiceSite::new(
        NODE_SITE,
        (0..vocabulary.nodes()).map(|node| node.to_string()),
    )
    .without_default()
}

pub(crate) fn asks(vocabulary: &FaultVocabulary, action: &FaultAction) -> bool {
    vocabulary.untried_first_nodes()
        && fresh(action)
        && !(vocabulary.resume_park().is_some()
            && matches!(action.operation, FaultOperation::Pause(..)))
}

fn fresh(action: &FaultAction) -> bool {
    action.guest.is_none() && action.operation.target_node().is_some()
}

#[must_use]
pub fn row(
    vocabulary: &FaultVocabulary,
    step: u64,
    moment: u64,
    operation: &FaultOperation,
) -> Option<ActionChoice> {
    let node = operation.target_node()?;
    let offered = operation.target_offer(vocabulary);
    let default = *offered.first()?;
    let taken = if node == default {
        Choice::Default
    } else {
        Choice::Option(node)
    };
    Some(ActionChoice {
        step,
        asker: Asker::Action,
        choice: None,
        moment,
        clock: 0,
        site: NODE_SITE.to_owned(),
        options: u16::try_from(offered.len()).unwrap_or(u16::MAX),
        offered,
        default,
        taken,
        label: node,
        answered: false,
    })
}

pub(crate) struct Resolved {
    pub(crate) suffix: Vec<FaultAction>,
    pub(crate) rows: Vec<Option<ActionChoice>>,
    pub(crate) untried: Option<u32>,
}

pub(crate) fn resolve(
    vocabulary: &FaultVocabulary,
    suffix: &[FaultAction],
    plan: Option<&ChoicePlan>,
    untried: Option<u32>,
    depth: u64,
) -> Resolved {
    let Some(plan) = plan else {
        return Resolved {
            suffix: suffix.to_vec(),
            rows: vec![None; suffix.len()],
            untried: None,
        };
    };
    if !vocabulary.untried_first_nodes() {
        return Resolved {
            suffix: suffix.to_vec(),
            rows: suffix
                .iter()
                .zip(1..)
                .map(|(action, step)| {
                    fresh(action)
                        .then(|| row(vocabulary, depth + step, 0, &action.operation))
                        .flatten()
                })
                .collect(),
            untried: None,
        };
    }
    let mut chooser = Chooser::new(&ChoicePlan {
        profile: ChoiceProfile::new(0, 0),
        seed: plan.seed ^ SEED_MIX,
        untried,
        held: Vec::new(),
        declared: Vec::new(),
        ..plan.clone()
    });
    let mut rows = Vec::with_capacity(suffix.len());
    let suffix = suffix
        .iter()
        .zip(1..)
        .map(|(action, step)| {
            let drawn = action
                .operation
                .target_node()
                .filter(|_| asks(vocabulary, action));
            let offered = action.operation.target_offer(vocabulary);
            let (Some(drawn), Some(&default)) = (drawn, offered.first()) else {
                rows.push(
                    fresh(action)
                        .then(|| row(vocabulary, depth + step, 0, &action.operation))
                        .flatten(),
                );
                return *action;
            };
            let weights = offered
                .iter()
                .map(|node| (*node, u32::from(*node == drawn)))
                .collect::<Vec<_>>();
            let taken = chooser.draw(NODE_SITE_ID, &weights, default);
            let operation = action
                .operation
                .with_target_node(taken.label(&offered, default));
            rows.push(row(vocabulary, depth + step, 0, &operation));
            FaultAction {
                operation,
                ..*action
            }
        })
        .collect();
    Resolved {
        suffix,
        rows,
        untried: chooser.untried_taken(),
    }
}

#[derive(Serialize)]
struct Line<'a> {
    depth: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    untried: Option<u32>,
    #[serde(flatten)]
    row: &'a ActionChoice,
}

pub(crate) fn record(depth: u64, untried: Option<u32>, row: &ActionChoice) {
    if let Some(path) = record_path() {
        write_line(path, depth, untried, row);
    }
}

fn record_path() -> Option<&'static Path> {
    static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
    PATH.get_or_init(|| std::env::var_os(RECORD_ENV).map(PathBuf::from))
        .as_deref()
}

fn write_line(path: &Path, depth: u64, untried: Option<u32>, row: &ActionChoice) {
    let mut line = serde_json::to_string(&Line {
        depth,
        untried,
        row,
    })
    .unwrap_or_else(|error| format!("{{\"error\":\"choice record: {error}\"}}"));
    line.push('\n');
    match OpenOptions::new().create(true).append(true).open(path) {
        Ok(mut file) => {
            if let Err(error) = file.write_all(line.as_bytes()) {
                eprintln!("choice record {}: {error}", path.display());
            }
        }
        Err(error) => eprintln!("choice record {}: {error}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU16;

    use searcher::search::choice::{ChoicePolicy, ChoiceProfiles};

    use super::*;

    const TICKS: NonZeroU16 = NonZeroU16::new(8).unwrap();

    fn vocabulary(untried_first_nodes: bool) -> FaultVocabulary {
        FaultVocabulary::new(3, vec![1])
            .unwrap()
            .with_disks(vec![1, 2])
            .unwrap()
            .with_checksummed(vec![2])
            .unwrap()
            .with_untried_first_nodes(untried_first_nodes)
    }

    fn plan(untried: Option<u32>) -> ChoicePlan {
        let policy = ChoicePolicy {
            profiles: ChoiceProfiles::Fixed(ChoiceProfile::new(1 << 15, 1 << 15)),
            untried_first: true,
        };
        ChoicePlan::new(
            &policy,
            ChoiceProfile::default(),
            11,
            untried,
            Vec::new(),
            Vec::new(),
        )
    }

    fn permuted(untried: Option<u32>, parent: u64) -> ChoicePlan {
        plan(untried).with_permuted_sites(&[NODE_SITE_ID], 9, parent)
    }

    fn action(operation: FaultOperation) -> FaultAction {
        FaultAction::from(operation).with_choice(5)
    }

    fn suffix() -> Vec<FaultAction> {
        [
            FaultOperation::Pause(2, TICKS),
            FaultOperation::Wait(TICKS),
            FaultOperation::Kill(1, TICKS),
            FaultOperation::DiskError(2, TICKS),
            FaultOperation::DiskCorrupt(2, TICKS),
            FaultOperation::Hook(1, TICKS),
        ]
        .map(action)
        .to_vec()
    }

    #[test]
    fn without_the_option_or_a_plan_the_drawn_nodes_stand() {
        let drawn = suffix();
        let unplanned = resolve(&vocabulary(true), &drawn, None, Some(1), 0);
        assert_eq!(unplanned.suffix, drawn);
        assert!(unplanned.rows.iter().all(Option::is_none));
        let off = vocabulary(false);
        let resolved = resolve(&off, &drawn, Some(&plan(Some(1))), Some(1), 0);
        assert_eq!(resolved.suffix, drawn);
        assert_eq!(resolved.untried, None);
        for ((action, asked), step) in drawn.iter().zip(&resolved.rows).zip(1..) {
            assert_eq!(
                asked.as_ref(),
                fresh(action)
                    .then(|| row(&off, step, 0, &action.operation))
                    .flatten()
                    .as_ref()
            );
        }
        assert!(resolved.rows[0].is_some() && resolved.rows[1].is_none());
    }

    #[test]
    fn the_untried_ordinal_walks_the_first_ask_and_every_other_ask_keeps_its_draw() {
        let drawn = suffix();
        let vocabulary = vocabulary(true);
        let mut firsts = (0..4)
            .map(|ordinal| {
                let resolved = resolve(
                    &vocabulary,
                    &drawn,
                    Some(&permuted(Some(ordinal), 4)),
                    Some(ordinal),
                    4,
                );
                assert_eq!(resolved.suffix[1..], drawn[1..]);
                assert_eq!(resolved.untried, (ordinal < 3).then_some(ordinal));
                resolved.suffix[0].operation.target_node()
            })
            .collect::<Vec<_>>();
        assert_eq!(firsts[3], Some(2));
        firsts[..3].sort_unstable();
        assert_eq!(firsts[..3], [Some(0), Some(1), Some(2)]);
        let resolved = resolve(&vocabulary, &drawn, Some(&plan(None)), None, 4);
        assert_eq!(resolved.suffix, drawn);
        let rows = resolved.rows;
        assert_eq!(
            rows.iter().map(Option::is_some).collect::<Vec<_>>(),
            [true, false, true, true, true, false]
        );
        let pause = rows[0].as_ref().unwrap();
        assert_eq!(
            (pause.step, pause.asker, pause.choice, pause.options),
            (5, Asker::Action, None, 3)
        );
        assert_eq!(
            (
                pause.offered.as_slice(),
                pause.default,
                pause.taken,
                pause.label
            ),
            (&[0, 1, 2][..], 0, Choice::Option(2), 2)
        );
        let disk = rows[3].as_ref().unwrap();
        assert_eq!(
            (
                disk.options,
                disk.offered.as_slice(),
                disk.default,
                disk.taken
            ),
            (2, &[1, 2][..], 1, Choice::Option(2))
        );
        let corrupt = rows[4].as_ref().unwrap();
        assert_eq!(
            (corrupt.options, corrupt.offered.as_slice(), corrupt.taken),
            (1, &[2][..], Choice::Default)
        );
    }

    #[test]
    fn one_parent_walks_three_distinct_nodes_and_every_node_starts_some_parent() {
        let vocabulary = vocabulary(true);
        let drawn = suffix();
        let first = |parent: u64, ordinal: u32| {
            let plan = permuted(Some(ordinal), parent);
            resolve(&vocabulary, &drawn, Some(&plan), Some(ordinal), 0).suffix[0]
                .operation
                .target_node()
                .unwrap()
        };
        let walked = (0..3)
            .map(|ordinal| first(4, ordinal))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(walked.len(), 3);
        let starts = (0..64)
            .map(|parent| first(parent, 0))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(starts.into_iter().collect::<Vec<_>>(), [0, 1, 2]);
    }

    #[test]
    fn a_stamped_action_and_a_pause_under_resume_park_keep_their_node() {
        let vocabulary = vocabulary(true);
        let stamped = action(FaultOperation::Kill(2, TICKS))
            .with_guest(crate::guest::GuestChoices::from_plan(&plan(None), 0, None));
        assert!(!asks(&vocabulary, &stamped));
        assert!(asks(&vocabulary, &action(FaultOperation::Pause(2, TICKS))));
        assert!(!asks(&vocabulary, &action(FaultOperation::KillAll(TICKS))));
        let parked = vocabulary
            .with_instrumented_events(true)
            .with_resume_park(Some(vec![7]))
            .unwrap();
        let pause = action(FaultOperation::Pause(2, TICKS));
        assert!(!asks(&parked, &pause));
        assert!(asks(&parked, &action(FaultOperation::Kill(2, TICKS))));
        let resolved = resolve(&parked, &[pause], Some(&permuted(Some(0), 4)), Some(0), 6);
        assert_eq!(resolved.suffix, [pause]);
        assert_eq!(resolved.untried, None);
        assert_eq!(resolved.rows, [row(&parked, 7, 0, &pause.operation)]);
        assert!(resolved.rows[0].is_some());
    }

    #[test]
    fn a_stored_action_gives_the_row_its_ask_wrote() {
        let vocabulary = vocabulary(true);
        let drawn = suffix();
        let resolved = resolve(&vocabulary, &drawn, Some(&plan(Some(1))), Some(1), 0);
        assert_eq!(
            resolved.suffix[0].operation,
            FaultOperation::Pause(1, TICKS)
        );
        for (action, asked) in resolved.suffix.iter().zip(&resolved.rows) {
            if let Some(asked) = asked {
                assert_eq!(
                    row(&vocabulary, asked.step, 0, &action.operation).as_ref(),
                    Some(asked)
                );
            }
        }
        assert_eq!(row(&vocabulary, 1, 0, &FaultOperation::Wait(TICKS)), None);
    }

    #[test]
    fn the_record_appends_one_line_per_ask() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("choices.jsonl");
        let vocabulary = vocabulary(true);
        let asked = row(&vocabulary, 3, 40, &FaultOperation::Pause(1, TICKS)).unwrap();
        write_line(&path, 2, Some(1), &asked);
        write_line(&path, 2, None, &asked);
        let lines = std::fs::read_to_string(&path).unwrap();
        let lines = lines
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["depth"], 2);
        assert_eq!(lines[0]["untried"], 1);
        assert_eq!(lines[0]["asker"], "action");
        assert_eq!(lines[0]["site"], NODE_SITE);
        assert_eq!(lines[0]["offered"], serde_json::json!([0, 1, 2]));
        assert_eq!(lines[0]["label"], 1);
        assert!(lines[1].get("untried").is_none());
        assert!(lines[1].get("choice").is_none());
    }
}
