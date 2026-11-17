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
        (r#"prefer_scope = "lives""#, "together"),
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
