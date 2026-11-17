// SPDX-License-Identifier: AGPL-3.0-or-later

use std::cmp::Ordering;

use super::*;
use crate::search::archive::{ArchiveKey, KeyShape};

#[derive(Clone, Copy, Default)]
struct Walk {
    room: u8,
    x: u8,
    lives: u8,
    time: u16,
    items: u8,
    transition: bool,
}

const ITEMS: &[&str] = &["sword", "bow", "key"];
const POSITION: &[&str] = &["room", "x"];

fn declare(state: Walk) -> [Assertion; 7] {
    [
        Assertion::Always {
            name: "lives stay in range",
            condition: state.lives <= 9,
        },
        Assertion::SometimesGreaterThanOrEqualTo {
            name: "far room",
            left: Number::byte(state.room),
            right: Number::constant(5),
        },
        Assertion::SometimesAll {
            name: "items",
            terms: ITEMS,
            holding: u64::from(state.items),
        },
        Assertion::SometimesEach {
            name: "position",
            fields: POSITION,
            values: [
                Number::new(i64::from(state.room), 0, 15),
                Number::byte(state.x).unit(16),
                Number::constant(0),
                Number::constant(0),
            ],
        },
        Assertion::Value {
            name: "lives",
            value: Number::new(i64::from(state.lives), 0, 9),
        },
        Assertion::Value {
            name: "time",
            value: Number::word(state.time),
        },
        Assertion::Sometimes {
            name: "in transition",
            condition: state.transition,
        },
    ]
}

fn catalog() -> Catalog {
    Catalog::from_assertions(declare(Walk::default())).unwrap()
}

fn record(state: Walk) -> Record {
    let catalog = catalog();
    let checked = catalog.checked_record(declare(state)).unwrap();
    assert_eq!(checked, Record::from_assertions(declare(state)));
    checked
}

#[test]
fn the_catalog_names_every_slot_from_one_declaration() {
    let catalog = catalog();
    let names: Vec<&str> = catalog
        .signals()
        .iter()
        .map(|signal| signal.name.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "lives stay in range",
            "far room",
            "far room.left",
            "far room.right",
            "far room.gap",
            "items",
            "items.sword",
            "items.bow",
            "items.key",
            "position.room",
            "position.x",
            "lives",
            "time",
            "in transition"
        ]
    );
    assert_eq!(catalog.group("position"), Some(9..11));
    let gap = &catalog.signals()[4];
    assert_eq!((gap.min, gap.max), (-5, 250));
    let values = record(Walk {
        room: 3,
        x: 40,
        lives: 2,
        items: 0b101,
        ..Walk::default()
    });
    assert_eq!(
        values.values(),
        [1, 0, 3, 5, -2, 2, 1, 0, 1, 3, 40, 2, 0, 0]
    );
    assert!(
        catalog
            .checked_record(declare(Walk {
                lives: 12,
                ..Walk::default()
            }))
            .is_err()
    );
    assert!(
        Catalog::from_assertions([
            Assertion::Value {
                name: "a",
                value: Number::byte(0)
            },
            Assertion::Value {
                name: "a",
                value: Number::byte(0)
            },
        ])
        .is_err()
    );
}

#[test]
fn a_tier_spread_and_preference_pack_in_order() {
    let compiled = Compiled::parse(
        r#"
        capacity = 1
        tier = ["items", "step(time, 100)"]
        spread = "position"
        identity = ["lives > 3"]
        prefer = [["lives: more", "time: less"]]
        absent = "in transition"
        reject = ["lives == 0", "room == 9"]
        "#
        .replace("room == 9", "'position.room' == 9")
        .as_str(),
        &catalog(),
    )
    .unwrap();
    assert_eq!(
        compiled.shape(),
        KeyShape {
            capacity: 1,
            preferences: 1
        }
    );
    let key = |state: Walk| compiled.key(&record(state)).unwrap();
    let base = Walk {
        room: 2,
        x: 33,
        lives: 3,
        time: 250,
        items: 0b001,
        ..Walk::default()
    };
    assert_eq!(
        crate::search::archive::slot_of_key(key(base)),
        crate::search::archive::slot_of_key(key(Walk { time: 299, ..base }))
    );
    assert_ne!(key(base).place(), key(Walk { x: 34, ..base }).place());
    assert!(key(base).progress() < key(Walk { time: 300, ..base }).progress());
    assert!(
        key(Walk { time: 999, ..base }).progress()
            < key(Walk {
                items: 0b011,
                ..base
            })
            .progress()
    );
    assert_ne!(
        key(base).identity(),
        key(Walk { lives: 4, ..base }).identity()
    );
    assert_eq!(
        key(Walk { lives: 2, ..base }).preference_cmp(0, key(base)),
        Ordering::Less
    );
    assert_eq!(
        key(Walk { time: 240, ..base }).preference_cmp(0, key(base)),
        Ordering::Greater
    );
    assert!(
        compiled
            .key(&record(Walk {
                transition: true,
                ..base
            }))
            .is_none()
    );
    let observed = compiled.observe(&record(Walk { lives: 0, ..base }), &Accumulator::default());
    assert_eq!(observed.rejected, Some(0));
    let observed = compiled.observe(&record(Walk { room: 9, ..base }), &Accumulator::default());
    assert_eq!(observed.rejected, Some(1));
    assert_eq!(compiled.reject_names(None)[1], "'position.room' == 9");
    let view = compiled.view(key(base));
    assert_eq!(view.progress, [1, 2]);
    assert_eq!(view.place, [2, 33]);
    assert!(compiled.is_pure());
}

#[test]
fn expressions_cover_tables_conditionals_and_popcount() {
    let compiled = Compiled::parse(
        r#"
        spread = [
            "index('position.room', route)",
            "if lives >= 5 then 1 else 0",
            "popcount(time) % 4",
            "bucket(time, bands)",
            "in('position.room', route) and not 'in transition'",
            "max(lives, 'position.room') - min(lives, 2) * 3",
            "('position.room', lives) > (2, 1)",
        ]
        [table]
        route = [1, 2, 7]
        bands = [10, 100, 1000]
        "#,
        &catalog(),
    )
    .unwrap();
    let view = |state: Walk| compiled.view(compiled.key(&record(state)).unwrap()).place;
    assert_eq!(
        view(Walk {
            room: 7,
            lives: 6,
            time: 0b1011,
            ..Walk::default()
        }),
        [2, 1, 3, 1, 1, 1, 1]
    );
    assert_eq!(
        view(Walk {
            room: 3,
            lives: 1,
            time: 150,
            ..Walk::default()
        }),
        [NONE, 0, 0, 2, 0, 0, 1]
    );
}

#[test]
fn stages_climb_their_gap_latch_and_carry() {
    let catalog = catalog();
    let compiled = Compiled::parse(
        r#"
        spread = "position"
        [[stage]]
        goal = "far room"
        carry = true
        objective = "if 'in transition' then none else 'far room.gap'"
        [[stage]]
        name = "sword"
        goal = "'items.sword'"
        latch = true
        [[stage]]
        goal = "items"
        spread = ["lives"]
        "#,
        &catalog,
    )
    .unwrap();
    assert_eq!(compiled.stage_names(), ["far room", "sword", "items"]);
    assert!(!compiled.is_pure());
    let observe = |state: Walk, parent: &Accumulator| compiled.observe(&record(state), parent);
    let start = observe(
        Walk {
            room: 2,
            ..Walk::default()
        },
        &Accumulator::default(),
    );
    let further = observe(
        Walk {
            room: 3,
            ..Walk::default()
        },
        &start.accumulator,
    );
    assert!(start.key.unwrap().progress() < further.key.unwrap().progress());
    let transition = observe(
        Walk {
            room: 0,
            transition: true,
            ..Walk::default()
        },
        &further.accumulator,
    );
    assert_eq!(
        transition.key.unwrap().progress(),
        further.key.unwrap().progress()
    );
    let far = observe(
        Walk {
            room: 6,
            ..Walk::default()
        },
        &transition.accumulator,
    );
    assert_eq!(
        compiled.view(far.key.unwrap()).stage.as_deref(),
        Some("sword")
    );
    let sword = observe(
        Walk {
            room: 6,
            items: 0b001,
            lives: 4,
            ..Walk::default()
        },
        &far.accumulator,
    );
    let view = compiled.view(sword.key.unwrap());
    assert_eq!(view.stage.as_deref(), Some("items"));
    assert_eq!(view.place, [4]);
    let back = observe(
        Walk {
            room: 1,
            lives: 4,
            ..Walk::default()
        },
        &sword.accumulator,
    );
    assert_eq!(
        compiled.view(back.key.unwrap()).stage.as_deref(),
        Some("far room")
    );
    let latched = Compiled::parse(
        r#"
        [[stage]]
        goal = "'items.bow'"
        latch = true
        [[stage]]
        goal = "items"
        "#,
        &catalog,
    )
    .unwrap();
    let bow = latched.observe(
        &record(Walk {
            items: 0b010,
            ..Walk::default()
        }),
        &Accumulator::default(),
    );
    let dropped = latched.observe(&record(Walk::default()), &bow.accumulator);
    assert_eq!(
        latched.view(dropped.key.unwrap()).stage,
        latched.view(bow.key.unwrap()).stage
    );
    let unlatched = latched.observe(&record(Walk::default()), &Accumulator::default());
    assert_ne!(
        latched.view(unlatched.key.unwrap()).stage,
        latched.view(bow.key.unwrap()).stage
    );
}

#[test]
fn aggregates_fold_over_the_lineage_and_reset_since_a_signal_changes() {
    let compiled = Compiled::parse(
        r#"
        spread = [
            "highest(lives)",
            "ever('items.key')",
            "count('in transition')",
            "last(if 'in transition' then none else 'position.x')",
            "first(time)",
            "highest(lives, 'position.room')",
        ]
        "#,
        &catalog(),
    )
    .unwrap();
    let mut accumulator = Accumulator::default();
    let mut views = Vec::new();
    for state in [
        Walk {
            lives: 3,
            x: 10,
            time: 7,
            ..Walk::default()
        },
        Walk {
            lives: 5,
            items: 0b100,
            x: 20,
            transition: true,
            ..Walk::default()
        },
        Walk {
            lives: 1,
            x: 30,
            transition: true,
            time: 9,
            ..Walk::default()
        },
        Walk {
            room: 1,
            lives: 2,
            x: 40,
            ..Walk::default()
        },
    ] {
        let observed = compiled.observe(&record(state), &accumulator);
        accumulator = observed.accumulator;
        views.push(compiled.view(observed.key.unwrap()).place);
    }
    assert_eq!(
        views,
        [
            vec![3, 0, 0, 10, 7, 3],
            vec![5, 1, 1, 10, 7, 5],
            vec![5, 1, 2, 10, 7, 5],
            vec![5, 1, 2, 40, 7, 2],
        ]
    );
}

#[test]
fn chain_scoped_preferences_compare_across_scopes_by_one_list() {
    let compiled = Compiled::parse(
        r#"
        spread = "position"
        prefer = [["lives: more", "time: more"], ["time: more", "lives: more"]]
        prefer_scope = "'position.room' / 4"
        prefer_across = ["lives: more"]
        "#,
        &catalog(),
    )
    .unwrap();
    let key = |room, lives, time| {
        compiled
            .key(&record(Walk {
                room,
                lives,
                time,
                ..Walk::default()
            }))
            .unwrap()
    };
    assert_eq!(
        key(1, 3, 9).preference_cmp(1, key(2, 3, 5)),
        Ordering::Greater
    );
    assert_eq!(
        key(1, 3, 9).preference_cmp(1, key(5, 3, 5)),
        Ordering::Equal
    );
    assert_eq!(
        key(1, 4, 0).preference_cmp(0, key(5, 3, 5)),
        Ordering::Greater
    );
}

#[test]
fn stages_are_the_default_preference_scope() {
    let compiled = Compiled::parse(
        r#"
        spread = "position"
        prefer = [["time: more"]]
        prefer_across = ["lives: more"]
        [[stage]]
        goal = "'items.bow'"
        [[stage]]
        goal = "items"
        "#,
        &catalog(),
    )
    .unwrap();
    let key = |items, lives, time| {
        compiled
            .key(&record(Walk {
                items,
                lives,
                time,
                ..Walk::default()
            }))
            .unwrap()
    };
    assert_eq!(
        key(0, 3, 9).preference_cmp(0, key(0b010, 2, 1)),
        Ordering::Greater
    );
    assert_eq!(
        key(0, 3, 9).preference_cmp(0, key(0b010, 4, 99)),
        Ordering::Less
    );
    assert_eq!(
        key(0, 1, 20).preference_cmp(0, key(0, 3, 9)),
        Ordering::Greater
    );
}

#[test]
fn strategies_reject_unknown_names_lines_and_overflows() {
    let catalog = catalog();
    for (text, message) in [
        (r#"spread = "nowhere""#, "not in the catalog"),
        (r#"spread = "time / lives""#, "positive literal"),
        (r#"sprad = "time""#, "unknown field"),
        (r#"spread = "position + 1""#, "is a tuple"),
        (
            r#"spread = ["time * 100000", "time * 100000", "time"]"#,
            "key field holds",
        ),
        (r#"prefer = [["time: most"]]"#, "is not `more`"),
        (r#"spread = "in(time, nothing)""#, "no table"),
        (r#"spread = "highest(ever(lives))""#, "another aggregate"),
        ("tier = \"lives\"\n[[stage]]\ngoal = \"items\"", "not both"),
        (
            r#"prefer_scope = "lives""#,
            "`prefer_scope` reads `lives`, which is not a whole value of the progress or `spread`",
        ),
        (
            r#"prefer_across = ["lives: more"]"#,
            "needs more than one scope",
        ),
        (
            "spread = \"a\"\n[define]\na = \"b + 1\"\nb = \"a\"",
            "in terms of itself",
        ),
    ] {
        let error = Compiled::parse(text, &catalog).unwrap_err().to_string();
        assert!(error.contains(message), "{text}: {error}");
    }
}

#[test]
fn a_preference_scope_reads_only_whole_values_of_the_cell() {
    let catalog = catalog();
    for (text, read) in [
        (
            "spread = \"time / 10\"\nprefer = [[\"lives: more\"]]\nprefer_scope = \"time\"\nprefer_across = [\"'position.room': more\"]",
            "time",
        ),
        (
            "spread = \"lives\"\nprefer_scope = \"highest(lives)\"",
            "highest(...)",
        ),
        (
            "spread = \"place\"\nprefer_scope = \"inner\"\n[define]\nplace = \"inner + lives\"\ninner = \"time / 10\"",
            "inner",
        ),
    ] {
        let error = Compiled::parse(text, &catalog).unwrap_err().to_string();
        assert!(
            error.contains(&format!("`prefer_scope` reads `{read}`")),
            "{text}: {error}"
        );
    }
    for text in [
        "spread = \"time\"\nprefer_scope = \"time / 10\"",
        "spread = \"position\"\nprefer_scope = \"in('position.room', rooms)\"\n[table]\nrooms = [1, 2]",
        "tier = \"lives\"\nspread = \"time / 10\"\nprefer_scope = \"lives / 2\"",
    ] {
        Compiled::parse(text, &catalog).unwrap_or_else(|error| panic!("{text}: {error}"));
    }
}

#[test]
fn malformed_strategies_return_errors_rather_than_panicking() {
    let catalog = catalog();
    let deep = (1..=40).fold("e0 = \"1\"".to_owned(), |text, depth| {
        format!("{text}\ne{depth} = \"1 + (e{})\"", depth - 1)
    });
    for (text, message) in [
        (
            format!("spread = \"highest(lives, e40)\"\n[define]\n{deep}"),
            "stack values",
        ),
        (
            "[[stage]]\ngoal = \"false\"\nobjective = []".to_owned(),
            "objective holds from 1",
        ),
        ("spread = \"\u{e9}\"".to_owned(), "unexpected `\u{e9}`"),
    ] {
        let error = Compiled::parse(&text, &catalog).unwrap_err().to_string();
        assert!(error.contains(message), "{text}: {error}");
    }
}

#[test]
fn a_none_operand_of_min_and_max_keeps_the_other_operand_range() {
    for function in ["min", "max"] {
        let compiled =
            Compiled::parse(&format!("spread = \"{function}(none, lives)\""), &catalog()).unwrap();
        let key = |lives| {
            compiled.key(&record(Walk {
                lives,
                ..Walk::default()
            }))
        };
        assert_ne!(key(1), key(2), "{function}");
    }
}

#[test]
fn definitions_name_expressions_and_tuples() {
    let compiled = Compiled::parse(
        r#"
        spread = ["where", "double"]
        identity = "(where, double) > (3, 2, 3)"
        [define]
        where = "('position.room', lives)"
        double = "twice + twice"
        twice = "lives"
        "#,
        &catalog(),
    )
    .unwrap();
    let key = compiled
        .key(&record(Walk {
            room: 3,
            lives: 2,
            ..Walk::default()
        }))
        .unwrap();
    let view = compiled.view(key);
    assert_eq!(view.place, [3, 2, 4]);
    assert_eq!(view.identity, [1]);
}

#[test]
fn the_policy_identifier_follows_the_strategy_and_the_catalog() {
    let catalog = catalog();
    let one = Compiled::parse(r#"spread = "position""#, &catalog).unwrap();
    let same = Compiled::parse("spread = 'position'", &catalog).unwrap();
    let other = Compiled::parse(r#"spread = "lives""#, &catalog).unwrap();
    assert_eq!(one.policy_identifier(), same.policy_identifier());
    assert_ne!(one.policy_identifier(), other.policy_identifier());
    let smaller = Catalog::from_assertions(declare(Walk::default()).into_iter().take(4)).unwrap();
    let narrowed = Compiled::parse(r#"spread = "position""#, &smaller).unwrap();
    assert_ne!(one.policy_identifier(), narrowed.policy_identifier());
    let every = Compiled::parse("[every_stage]\nspread = \"position\"", &catalog).unwrap();
    assert_eq!(
        every.key(&record(Walk::default())),
        one.key(&record(Walk::default()))
    );
}

#[test]
fn the_goal_names_a_declaration_or_an_expression() {
    let observe = |text: &str, state: Walk| {
        Compiled::parse(text, &catalog())
            .unwrap()
            .observe(&record(state), &Accumulator::default())
            .goal
    };
    let far = Walk {
        room: 6,
        ..Walk::default()
    };
    assert!(observe("goal = \"far room\"", far));
    assert!(!observe("goal = \"far room\"", Walk::default()));
    assert!(observe(
        "goal = \"items\"",
        Walk {
            items: 7,
            ..Walk::default()
        }
    ));
    assert!(!observe(
        "goal = \"items\"",
        Walk {
            items: 3,
            ..Walk::default()
        }
    ));
    assert!(observe("goal = \"position.room == 6 and lives == 0\"", far));
    assert!(!observe("tier = \"position.room\"", far));
    assert!(Compiled::parse("goal = \"position.room, position.x\"", &catalog()).is_err());
}

#[test]
fn checks_find_a_goal_the_ranges_rule_out_and_a_step_that_thins_the_leading_tier() {
    let catalog = catalog();
    let walks: Vec<Record> = (0..300_u16)
        .map(|index| {
            record(Walk {
                room: u8::try_from(index / 60).unwrap(),
                x: u8::try_from(index % 60).unwrap(),
                ..Walk::default()
            })
        })
        .collect();
    let start = Accumulator::default();
    let run = |text: &str| {
        Compiled::parse(text, &catalog)
            .unwrap()
            .check(walks.iter().map(|record| (record, &start)))
    };
    assert_eq!(
        run("goal = \"'position.room' >= 4\"\nspread = \"position\""),
        []
    );
    assert_eq!(run("goal = \"time > 65534\"\nspread = \"position\""), []);
    for goal in [
        "time > 65535",
        "'position.room' > 15 or time < 0",
        "not (time >= 0)",
        "'position.room' == 3 and 'position.room' * 2 > 30",
    ] {
        assert_eq!(
            run(&format!("goal = \"{goal}\"\nspread = \"position\"")),
            [Finding::UnreachableGoal {
                goal: "goal".to_owned(),
                records: 300
            }],
            "{goal}"
        );
    }
    assert_eq!(
        run("spread = \"position\"\n[[stage]]\ngoal = \"time > 70000\""),
        [Finding::UnreachableGoal {
            goal: "time > 70000".to_owned(),
            records: 300
        }]
    );
    let staged = |step: u32| {
        run(&format!(
            "spread = \"position\"\n[[stage]]\ngoal = \"far room\"\nstep = {step}"
        ))
        .into_iter()
        .filter(|finding| matches!(finding, Finding::ThinLeadingTier { .. }))
        .collect::<Vec<_>>()
    };
    assert_eq!(
        staged(1),
        [Finding::ThinLeadingTier {
            stage: "far room".to_owned(),
            objective: vec![-1],
            states: 60
        }]
    );
    assert_eq!(staged(8), []);
}

fn walk_frames(room: u8, from: u8, to: u8) -> Vec<Record> {
    [from, to]
        .into_iter()
        .map(|x| {
            record(Walk {
                room,
                x,
                lives: 3,
                ..Walk::default()
            })
        })
        .collect()
}

fn walk_recording() -> (Record, Vec<RecordedJob>, Vec<RecordedAction<u8>>) {
    use crate::search::campaign::CampaignAdmissionDecision::{Rejected, Retained};
    let genesis = walk_frames(0, 0, 0)[0];
    let action = |sequence, action, frames: Vec<Record>| RecordedAction {
        sequence,
        action,
        failed: false,
        cost: frames.len() as u64,
        frames,
    };
    let actions = vec![
        action(1, 1, walk_frames(0, 0, 16)),
        action(1, 2, walk_frames(0, 16, 32)),
        action(2, 3, walk_frames(0, 32, 0)),
        action(2, 4, walk_frames(1, 0, 48)),
    ];
    let jobs = vec![
        RecordedJob {
            sequence: 1,
            parent_id: 0,
            decisions: vec![Retained { id: 1 }, Retained { id: 2 }],
        },
        RecordedJob {
            sequence: 2,
            parent_id: 2,
            decisions: vec![Rejected, Retained { id: 3 }],
        },
    ];
    (genesis, jobs, actions)
}

fn walk_header(genesis: Record) -> RecordsHeader {
    RecordsHeader {
        format: RECORDS_FORMAT.to_owned(),
        stream: StreamIdentity {
            campaign_seed: 7,
            workload_identity_sha256: "walk".to_owned(),
        },
        policy: "walk".to_owned(),
        strategy: "spread = \"position\"".to_owned(),
        catalog: catalog(),
        genesis,
    }
}

fn written(header: &RecordsHeader, actions: &[RecordedAction<u8>]) -> (Vec<u8>, RecordsSize) {
    static FILES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "assertion-records-{}-{}.bin",
        std::process::id(),
        FILES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let mut writer = RecordsWriter::create(&path, header).unwrap();
    for action in actions {
        writer.push(action).unwrap();
    }
    let size = writer.finish().unwrap();
    let bytes = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    (bytes, size)
}

#[test]
fn records_round_trip_through_the_delta_encoding() {
    let (genesis, _, actions) = walk_recording();
    let header = walk_header(genesis);
    let (bytes, size) = written(&header, &actions);
    assert_eq!(size.jobs, 2);
    assert_eq!(usize::try_from(size.bytes).unwrap(), bytes.len());
    let recording = Recording::<u8>::from_bytes(&bytes).unwrap();
    assert_eq!(recording.header, header);
    assert_eq!(recording.actions, actions);
    assert_eq!((recording.jobs, recording.truncated_bytes), (2, 0));
    assert!(recording.check_stream(&header.stream, "walk").is_ok());
    assert!(
        recording
            .check_stream(
                &StreamIdentity {
                    campaign_seed: 8,
                    ..header.stream.clone()
                },
                "walk"
            )
            .is_err()
    );
    assert!(recording.check_stream(&header.stream, "other").is_err());
}

#[test]
fn a_cut_records_file_reads_as_its_whole_jobs_and_a_later_file_replaces_the_jobs_it_repeats() {
    let (genesis, _, actions) = walk_recording();
    let header = walk_header(genesis);
    let (bytes, _) = written(&header, &actions);
    let (first_job, _) = written(&header, &actions[..2]);
    let header_bytes = postcard::to_allocvec(&header).unwrap().len();
    for cut in header_bytes..=bytes.len() {
        let recording = Recording::<u8>::from_bytes(&bytes[..cut]).unwrap();
        let whole = if cut == bytes.len() {
            2
        } else {
            usize::from(cut >= first_job.len())
        };
        assert_eq!(recording.jobs, whole, "cut at {cut}");
        assert_eq!(recording.actions, actions[..2 * whole], "cut at {cut}");
        assert_eq!(
            recording.truncated_bytes,
            cut - [header_bytes, first_job.len(), bytes.len()][whole]
        );
    }
    let resumed: Vec<RecordedAction<u8>> = actions[2..]
        .iter()
        .cloned()
        .chain(std::iter::once(RecordedAction {
            sequence: 3,
            action: 9,
            failed: false,
            cost: 17,
            frames: walk_frames(1, 48, 64),
        }))
        .collect();
    let earlier = Recording::<u8>::from_bytes(&bytes[..bytes.len() - 1]).unwrap();
    let later = Recording::<u8>::from_bytes(&written(&header, &resumed).0).unwrap();
    let joined = earlier.continue_with(later).unwrap();
    assert_eq!(joined.jobs, 3);
    assert_eq!(joined.actions[..2], actions[..2]);
    assert_eq!(joined.actions[2..], resumed[..]);
}

#[test]
fn rekeying_under_the_recorded_strategy_reproduces_every_decision() {
    let catalog = catalog();
    let (genesis, jobs, actions) = walk_recording();
    let recorded = Compiled::parse("spread = \"position\"\ncapacity = 1", &catalog).unwrap();
    let same = Rekeying {
        recorded: &recorded,
        strategy: &recorded,
        entry_limit: 1024,
    }
    .run(&genesis, &jobs, &actions, |_| 1)
    .unwrap();
    assert_eq!(same_decisions(&jobs, &same), 2);
    assert_eq!(same.cells, 4);
    assert_eq!(
        same.holders
            .iter()
            .map(|held| held.ids.clone())
            .collect::<Vec<_>>(),
        [vec![0], vec![1], vec![2], vec![3]]
    );
    let coarse = Compiled::parse("spread = \"position.room\"\ncapacity = 1", &catalog).unwrap();
    let rooms = Rekeying {
        recorded: &recorded,
        strategy: &coarse,
        entry_limit: 1024,
    }
    .run(&genesis, &jobs, &actions, |_| 1)
    .unwrap();
    assert_eq!(rooms.cells, 2);
    assert_eq!(same_decisions(&jobs, &rooms), 0);
    let mut short = jobs.clone();
    short[1].decisions.pop();
    assert!(
        Rekeying {
            recorded: &recorded,
            strategy: &recorded,
            entry_limit: 1024,
        }
        .run(&genesis, &short, &actions, |_| 1)
        .is_err()
    );
}

#[test]
fn rekeying_keeps_the_descendants_of_a_state_the_strategy_stops_at_stopped() {
    let catalog = catalog();
    let (genesis, jobs, actions) = walk_recording();
    let recorded = Compiled::parse("spread = \"position\"\ncapacity = 1", &catalog).unwrap();
    let rejecting = Compiled::parse(
        "spread = \"position\"\ncapacity = 1\nreject = \"'position.x' == 16\"",
        &catalog,
    )
    .unwrap();
    let rekeyed = Rekeying {
        recorded: &recorded,
        strategy: &rejecting,
        entry_limit: 1024,
    }
    .run(&genesis, &jobs, &actions, |_| 1)
    .unwrap();
    assert_eq!(
        rekeyed
            .holders
            .iter()
            .map(|held| held.ids.clone())
            .collect::<Vec<_>>(),
        [vec![0]]
    );
}

#[test]
fn a_recorded_action_wider_than_the_record_is_an_error() {
    #[derive(serde::Serialize)]
    struct Job {
        sequence: u64,
        actions: Vec<Action>,
    }
    #[derive(serde::Serialize)]
    struct Action {
        action: u8,
        failed: bool,
        cost: u64,
        width: u8,
        changes: Vec<Vec<(u8, i64)>>,
    }
    let (genesis, _, _) = walk_recording();
    let mut bytes = postcard::to_allocvec(&walk_header(genesis)).unwrap();
    let width = u8::try_from(RECORD_SLOTS + 1).unwrap();
    bytes.extend(
        postcard::to_allocvec(&Job {
            sequence: 1,
            actions: vec![Action {
                action: 1,
                failed: false,
                cost: 1,
                width,
                changes: vec![Vec::new()],
            }],
        })
        .unwrap(),
    );
    let error = Recording::<u8>::from_bytes(&bytes)
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("wider than the record"), "{error}");
}

#[test]
fn every_archive_preference_comparison_is_at_a_listed_site() {
    let source = include_str!("../archive.rs");
    let production = source
        .split("\n#[cfg(test)]\nimpl<A, K, M, S> Archive<A, K, M, S>")
        .next()
        .unwrap_or(source);
    let mut function = "";
    let mut sites = Vec::new();
    for line in production.lines() {
        let trimmed = line.trim_start();
        let declared = [
            "fn ",
            "pub fn ",
            "pub(crate) fn ",
            "const fn ",
            "pub const fn ",
        ]
        .iter()
        .find_map(|prefix| trimmed.strip_prefix(prefix));
        if let Some(rest) = declared {
            function = rest.split(['(', '<']).next().unwrap_or(rest);
            continue;
        }
        if line.contains("preference_cmp(") {
            sites.push(function);
        }
    }
    let within_slot = [
        "visit_preference_winners",
        "insert_after_costed",
        "insert_after_costed",
        "insert_after_costed",
        "champions_slot",
    ];
    let across_slots_in_one_cell = ["cmp", "holder_order"];
    let across_cells_pairwise = ["insert_after_costed", "outranks_slot_holders"];
    let mut expected: Vec<&str> = within_slot
        .into_iter()
        .chain(across_slots_in_one_cell)
        .chain(across_cells_pairwise)
        .collect();
    expected.sort_unstable();
    sites.sort_unstable();
    assert_eq!(sites, expected);
    for other in [
        include_str!("../campaign.rs"),
        include_str!("../campaign_coordinator.rs"),
        include_str!("../continuation.rs"),
        include_str!("../draw.rs"),
        include_str!("rekey.rs"),
    ] {
        assert!(!other.contains("preference_cmp("));
    }
}
