// SPDX-License-Identifier: AGPL-3.0-or-later
//! The catalog fold + never-fired report (task 73 gate 2 + gate 6).

use explorer::{GuestEvent, Moment};
use link::{Catalog, CatalogReport, PointKind, decode_events};
use proptest::prelude::*;
use std::collections::BTreeSet;

// Wire constants (mirror of `guest/sdk/src/wire.rs`).
const NS_SHIFT: u32 = 24;
const NS_ASSERT: u32 = 1;
const NS_STATE: u32 = 2;
const NS_BUGGIFY: u32 = 3;
const KIND_SOMETIMES: u8 = 1;
const KIND_STATE: u8 = 4;
const KIND_BUGGIFY: u8 = 5;

fn id(ns: u32, local: u32) -> u32 {
    (ns << NS_SHIFT) | local
}

/// Build a catalog-declaration blob from `(kind, local, name)` points, exactly as
/// the SDK's `init` marshals it.
fn declaration(points: &[(u8, u32, &str)]) -> Vec<u8> {
    let mut b = b"SDKC".to_vec();
    b.push(1); // version
    b.extend_from_slice(&(points.len() as u32).to_le_bytes());
    for (kind, local, name) in points {
        b.push(*kind);
        b.extend_from_slice(&local.to_le_bytes());
        b.extend_from_slice(&(name.len() as u16).to_le_bytes());
        b.extend_from_slice(name.as_bytes());
    }
    b
}

/// The decoded event stream for a set of raw firing events.
fn events(raw: &[(u64, u32, Vec<u8>)]) -> Vec<(Moment, GuestEvent)> {
    let raw: Vec<(Moment, u32, Vec<u8>)> = raw
        .iter()
        .map(|(m, id, b)| (Moment(*m), *id, b.clone()))
        .collect();
    decode_events(&raw)
}

/// GATE 6 — two declared sometimes points, one wired to fire ⇒ the report flags
/// the other as never-fired.
#[test]
fn never_fired_flags_the_unhit_sometimes_point() {
    let decl = declaration(&[
        (KIND_SOMETIMES, 1, "commit_seen"),
        (KIND_SOMETIMES, 2, "rollback_seen"),
    ]);
    // Point 1 fires (an assert hit); point 2 never does.
    let ev = events(&[(10, id(NS_ASSERT, 1), vec![0, 0, 0])]);

    let report = Catalog::fold(&decl, &ev);
    assert_eq!(
        report.fired,
        ["commit_seen".to_string()].into_iter().collect()
    );
    assert_eq!(
        report.never_fired,
        ["rollback_seen".to_string()].into_iter().collect()
    );
    // fired ⊎ never_fired = declared, disjoint.
    assert!(report.fired.is_disjoint(&report.never_fired));
}

/// The fold books declared/fired correctly across every kind: an assert hit, a
/// state report, and a buggify result each mark their declared name fired; an
/// undeclared firing is ignored.
#[test]
fn fold_books_every_kind_and_ignores_undeclared() {
    let decl = declaration(&[
        (KIND_SOMETIMES, 1, "hit_point"),
        (KIND_STATE, 40, "max_lsn"),
        (KIND_BUGGIFY, 50, "slow_disk"),
        (KIND_SOMETIMES, 2, "never_hit"),
    ]);
    let mut state_payload = vec![1u8]; // STATE_MAX
    state_payload.extend_from_slice(&7u64.to_le_bytes());
    let ev = events(&[
        (10, id(NS_ASSERT, 1), vec![0, 0, 0]),   // hit_point
        (20, id(NS_STATE, 40), state_payload),   // max_lsn
        (30, id(NS_BUGGIFY, 50), vec![1]),       // slow_disk
        (40, id(NS_ASSERT, 999), vec![0, 0, 0]), // undeclared -> ignored
    ]);
    let cat = Catalog::from_declaration_bytes(&decl);
    let fired = cat.fired(&ev);
    assert_eq!(
        fired,
        ["hit_point", "max_lsn", "slow_disk"]
            .into_iter()
            .map(String::from)
            .collect::<BTreeSet<_>>()
    );
    let report = cat.report(&fired);
    assert_eq!(
        report.never_fired,
        ["never_hit".to_string()].into_iter().collect()
    );
    assert_eq!(cat.len(), 4);
}

/// An unrecognized kind byte declares a `PointKind::Unknown` point, which has NO
/// runtime namespace: it must NOT register under `NS_ASSERT` (the old wildcard
/// fallback). A real assert firing at the SAME id must not resolve to the unknown
/// point — it is declared but always never-fired.
#[test]
fn unknown_kind_point_never_registers_under_ns_assert() {
    const KIND_UNRECOGNIZED: u8 = 0xFF;
    let decl = declaration(&[(KIND_UNRECOGNIZED, 1, "mystery")]);
    // A real assert HIT fires at (NS_ASSERT, 1) — the coordinate the unknown point
    // would alias if it (wrongly) fell back to NS_ASSERT.
    let ev = events(&[(10, id(NS_ASSERT, 1), vec![0, 0, 0])]);
    let cat = Catalog::from_declaration_bytes(&decl);
    let fired = cat.fired(&ev);
    assert!(
        fired.is_empty(),
        "an assert firing must not resolve to an unknown-kind point"
    );
    assert_eq!(
        cat.report(&fired).never_fired,
        ["mystery".to_string()].into_iter().collect(),
        "the unknown-kind point is declared and always never-fired"
    );
}

/// A re-declared name drops its **stale** coordinate: a firing at the old
/// coordinate no longer resolves to the name (regression for the review's finding
/// that a redeclare left the old `by_coord` entry behind).
#[test]
fn redeclare_removes_the_stale_coordinate() {
    // "x" declared first as a sometimes point (assert ns, local 1), then
    // re-declared as a state register (state ns, local 40).
    let decl = declaration(&[(KIND_SOMETIMES, 1, "x"), (KIND_STATE, 40, "x")]);
    let cat = Catalog::from_declaration_bytes(&decl);
    assert_eq!(cat.len(), 1, "one declared name after the redeclare");

    // A firing at the OLD coordinate (assert, 1) must NOT resolve to "x".
    let old = events(&[(10, id(NS_ASSERT, 1), vec![0, 0, 0])]);
    assert!(
        cat.fired(&old).is_empty(),
        "the stale coordinate no longer resolves to the name"
    );

    // A firing at the NEW coordinate (state, 40) resolves to "x".
    let mut state_payload = vec![1u8]; // STATE_MAX
    state_payload.extend_from_slice(&7u64.to_le_bytes());
    let new = events(&[(10, id(NS_STATE, 40), state_payload)]);
    assert_eq!(cat.fired(&new), ["x".to_string()].into_iter().collect());
}

/// A **malformed** declaration can put two names at one coordinate (the SDK's
/// `init` rejects this, but link decode is total and must survive arbitrary
/// bytes). Last-writer-wins in `by_coord`, so when the FIRST name later moves it
/// must NOT evict the coordinate the SECOND name now owns.
#[test]
fn redeclare_does_not_evict_another_names_coordinate() {
    // "a" and "b" both declared at (assert, 1) — "b" wins the coordinate; then
    // "a" moves to (assert, 2). "b" must keep (assert, 1).
    let decl = declaration(&[
        (KIND_SOMETIMES, 1, "a"),
        (KIND_SOMETIMES, 1, "b"),
        (KIND_SOMETIMES, 2, "a"),
    ]);
    let cat = Catalog::from_declaration_bytes(&decl);

    // A firing at (assert, 1) still resolves to "b" — its coordinate survived the
    // move of "a" (the bug would have deleted it, orphaning "b").
    let at1 = events(&[(10, id(NS_ASSERT, 1), vec![0, 0, 0])]);
    assert_eq!(
        cat.fired(&at1),
        ["b".to_string()].into_iter().collect(),
        "b keeps the coordinate it owns; a's move must not evict it"
    );
    // "a" now lives at (assert, 2).
    let at2 = events(&[(10, id(NS_ASSERT, 2), vec![0, 0, 0])]);
    assert_eq!(cat.fired(&at2), ["a".to_string()].into_iter().collect());
}

/// The catalog records each point's kind so the report can be sliced by role.
#[test]
fn catalog_records_kinds() {
    let decl = declaration(&[(KIND_SOMETIMES, 1, "s"), (KIND_STATE, 2, "st")]);
    let cat = Catalog::from_declaration_bytes(&decl);
    let kinds: Vec<(String, PointKind)> = cat.declared().map(|(n, k)| (n.clone(), *k)).collect();
    assert!(kinds.contains(&("s".to_string(), PointKind::AssertSometimes)));
    assert!(kinds.contains(&("st".to_string(), PointKind::StateReg)));
}

/// The never-fired report round-trips through serde (task 66's format).
#[test]
fn report_round_trips_through_serde() {
    let report = CatalogReport {
        fired: ["a".to_string(), "b".to_string()].into_iter().collect(),
        never_fired: ["c".to_string()].into_iter().collect(),
    };
    let json = serde_json::to_string(&report).expect("serialize");
    let back: CatalogReport = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(report, back);
}

/// A malformed / truncated declaration never panics: it yields whatever prefix
/// parsed, or an empty catalog.
#[test]
fn malformed_declaration_is_total() {
    assert!(Catalog::from_declaration_bytes(&[]).is_empty());
    assert!(Catalog::from_declaration_bytes(b"XXXX").is_empty());
    // Valid header claiming 5 points but truncated after 1: keep the 1.
    let mut decl = declaration(&[(KIND_SOMETIMES, 1, "one")]);
    decl[5..9].copy_from_slice(&5u32.to_le_bytes()); // count = 5, but only 1 present
    let cat = Catalog::from_declaration_bytes(&decl);
    assert_eq!(cat.len(), 1);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// For any declared set and any firing subset, the report partitions the
    /// declared set exactly: fired ⊎ never_fired = declared, disjoint, and every
    /// wired point ends up fired.
    #[test]
    fn report_partitions_the_declared_set(
        points in proptest::collection::vec((1u32..50, "[a-z]{1,6}"), 1..12),
        fire_mask in any::<u64>(),
    ) {
        // Deduplicate points by local id AND by name so the declaration is
        // well-formed (unique coordinates and unique names).
        let mut seen_ids = BTreeSet::new();
        let mut seen_names = BTreeSet::new();
        let points: Vec<(u8, u32, String)> = points
            .into_iter()
            .filter(|(id, name)| seen_ids.insert(*id) && seen_names.insert(name.clone()))
            .map(|(id, name)| (KIND_SOMETIMES, id, name))
            .collect();
        prop_assume!(!points.is_empty());

        let decl_pairs: Vec<(u8, u32, &str)> =
            points.iter().map(|(k, i, n)| (*k, *i, n.as_str())).collect();
        let decl = declaration(&decl_pairs);

        // Fire a mask-selected subset.
        let mut raw = Vec::new();
        let mut expected_fired = BTreeSet::new();
        for (bit, (_k, local, name)) in points.iter().enumerate() {
            if fire_mask & (1 << (bit % 64)) != 0 {
                raw.push((bit as u64, id(NS_ASSERT, *local), vec![0u8, 0, 0]));
                expected_fired.insert(name.clone());
            }
        }
        let ev = events(&raw);
        let report = Catalog::fold(&decl, &ev);

        let declared: BTreeSet<String> = points.iter().map(|(_, _, n)| n.clone()).collect();
        let union: BTreeSet<String> =
            report.fired.union(&report.never_fired).cloned().collect();
        prop_assert_eq!(union, declared);
        prop_assert!(report.fired.is_disjoint(&report.never_fired));
        prop_assert_eq!(report.fired, expected_fired);
    }
}
