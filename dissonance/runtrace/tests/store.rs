// SPDX-License-Identifier: AGPL-3.0-or-later
//! `TraceStore` discipline and the retention knob (the store half of gate 5;
//! the campaign-level "no reads / same verbs" half lives in the conductor's
//! recording gate). Plus the telemetry NDJSON `Console` ingest path.

use explorer::{Environment, Moment, Record, RunTrace, StopReason, StreamId, VTime};
use runtrace::{
    Retain, RetentionPolicy, TraceId, TraceStore, decode_chunks, ingest_ndjson, retain_for,
};

/// A minimal trace whose env bytes (hence [`TraceId`]) are `tag`, terminating
/// with `terminal`.
fn trace(tag: &[u8], terminal: StopReason) -> RunTrace {
    RunTrace {
        terminal,
        env: Environment {
            blob_version: 3,
            bytes: tag.to_vec(),
        },
        coverage: None,
        events: vec![],
        records: vec![(
            Moment(1),
            Record {
                stream: StreamId(0),
                line: b"hello\n".to_vec(),
            },
        )],
    }
}

fn quiescent() -> StopReason {
    StopReason::Quiescent { vtime: VTime(10) }
}
fn crash() -> StopReason {
    StopReason::Crash {
        vtime: VTime(10),
        info: vec![1],
    }
}

#[test]
fn full_writes_both_files_env_only_writes_one() {
    let dir = tempfile::tempdir().unwrap();
    let store = TraceStore::open(dir.path()).unwrap();

    let full = trace(b"aaa", quiescent());
    let eo = trace(b"bbb", quiescent());
    let full_id = store.record(&full, Retain::Full).unwrap();
    let eo_id = store.record(&eo, Retain::EnvOnly).unwrap();

    assert!(store.has_journal(full_id), "Full retains the journal");
    assert!(!store.has_journal(eo_id), "EnvOnly retains no journal");

    // Both envs load back.
    assert_eq!(store.env(full_id).unwrap(), full.env);
    assert_eq!(store.env(eo_id).unwrap(), eo.env);

    // The full trace loads; the env-only one is NotRetained (regenerate by replay).
    assert_eq!(store.load(full_id).unwrap(), full);
    assert!(matches!(
        store.load(eo_id),
        Err(runtrace::TraceError::NotRetained(_))
    ));
}

#[test]
fn unknown_ids_are_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let store = TraceStore::open(dir.path()).unwrap();
    let ghost = TraceId([0xAB; 32]);
    assert!(matches!(
        store.env(ghost),
        Err(runtrace::TraceError::NotFound(_))
    ));
    assert!(matches!(
        store.load(ghost),
        Err(runtrace::TraceError::NotFound(_))
    ));
}

#[test]
fn ids_are_listed_in_deterministic_sorted_order() {
    let dir = tempfile::tempdir().unwrap();
    let store = TraceStore::open(dir.path()).unwrap();

    // Record several distinct-env traces in a scrambled order; a couple env-only.
    let mut recorded = Vec::new();
    for tag in [b"m3", b"m1", b"m4", b"m2", b"m0"] {
        let retain = if tag[1] % 2 == 0 {
            Retain::EnvOnly
        } else {
            Retain::Full
        };
        recorded.push(store.record(&trace(tag, quiescent()), retain).unwrap());
    }

    let listed = store.ids().unwrap();
    // Every recorded id is listed — env-only or full alike (the env sidecar is
    // the source of truth).
    let mut expected = recorded.clone();
    expected.sort_unstable();
    expected.dedup();
    assert_eq!(listed, expected);

    // And the order is sorted (deterministic across runs and platforms).
    let mut sorted = listed.clone();
    sorted.sort_unstable();
    assert_eq!(listed, sorted);
}

#[test]
fn recording_the_same_run_twice_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let store = TraceStore::open(dir.path()).unwrap();
    let t = trace(b"same", crash());
    let a = store.record(&t, Retain::Full).unwrap();
    let b = store.record(&t, Retain::Full).unwrap();
    assert_eq!(a, b, "same env ⇒ same TraceId");
    assert_eq!(store.ids().unwrap(), vec![a], "no duplicate listing");
}

#[test]
fn env_only_re_record_removes_a_prior_journal() {
    // A reused store: a run first recorded Full, then re-recorded EnvOnly. The
    // stale (content-identical) journal is removed, so has_journal/load reflect
    // the last-recorded, weaker policy.
    let dir = tempfile::tempdir().unwrap();
    let store = TraceStore::open(dir.path()).unwrap();
    let t = trace(b"reused", crash());

    let id = store.record(&t, Retain::Full).unwrap();
    assert!(store.has_journal(id), "Full wrote the journal");

    let id2 = store.record(&t, Retain::EnvOnly).unwrap();
    assert_eq!(id, id2, "same env ⇒ same TraceId");
    assert!(!store.has_journal(id), "the stale journal was removed");
    assert!(matches!(
        store.load(id),
        Err(runtrace::TraceError::NotRetained(_))
    ));
    assert!(store.env(id).is_ok(), "the env sidecar still loads");
}

#[test]
fn retain_for_maps_the_policy() {
    // `all` → always Full.
    assert_eq!(
        retain_for(RetentionPolicy::All, &quiescent(), false),
        Retain::Full
    );
    assert_eq!(
        retain_for(RetentionPolicy::All, &crash(), false),
        Retain::Full
    );

    // `env-only` → always EnvOnly.
    assert_eq!(
        retain_for(RetentionPolicy::EnvOnly, &crash(), true),
        Retain::EnvOnly
    );

    // `interesting` → Full iff the terminal is a bug, or caller-flagged.
    assert_eq!(
        retain_for(RetentionPolicy::Interesting, &crash(), false),
        Retain::Full
    );
    assert_eq!(
        retain_for(RetentionPolicy::Interesting, &quiescent(), false),
        Retain::EnvOnly
    );
    assert_eq!(
        retain_for(RetentionPolicy::Interesting, &quiescent(), true),
        Retain::Full,
        "a caller-flagged run is retained even on a non-bug terminal"
    );
}

#[test]
fn retention_policy_flag_round_trips() {
    for p in [
        RetentionPolicy::All,
        RetentionPolicy::Interesting,
        RetentionPolicy::EnvOnly,
    ] {
        assert_eq!(RetentionPolicy::parse(p.as_str()), Some(p));
    }
    assert_eq!(RetentionPolicy::parse("nonsense"), None);
    assert_eq!(RetentionPolicy::default(), RetentionPolicy::Interesting);
}

#[test]
fn trace_id_hex_round_trips() {
    let id = TraceId([0x12; 32]);
    assert_eq!(TraceId::from_hex(&id.to_hex()), Some(id));
    assert_eq!(id.to_hex().len(), 64);
    assert_eq!(TraceId::from_hex("nothex"), None);
    assert_eq!(TraceId::from_hex(&"z".repeat(64)), None);
}

#[test]
fn ingest_ndjson_extracts_console_chunks_in_order() {
    // A telemetry recording: two console lines (different vns), an interleaved
    // non-console event that must be skipped, and a blank line.
    let ndjson = r#"{"seq":1,"work":100,"vns":50,"kind":{"Console":{"text":"boot\n"}}}
{"seq":2,"work":200,"vns":60,"kind":{"Io":{"port":1016,"size":1,"value":7,"write":true}}}

{"seq":3,"work":300,"vns":70,"kind":{"Console":{"text":"ready\n"}}}"#;

    let chunks = ingest_ndjson(ndjson).expect("ingest");
    assert_eq!(
        chunks,
        vec![
            (Moment(50), b"boot\n".to_vec()),
            (Moment(70), b"ready\n".to_vec()),
        ]
    );

    // And it feeds straight into the scrape decoder.
    let records = decode_chunks(StreamId(0), &chunks);
    let lines: Vec<&[u8]> = records.iter().map(|(_, r)| r.line.as_slice()).collect();
    assert_eq!(lines, vec![b"boot\n".as_slice(), b"ready\n".as_slice()]);
    assert_eq!(records[0].0, Moment(50));
    assert_eq!(records[1].0, Moment(70));
}

#[test]
fn ingest_ndjson_is_loud_on_a_malformed_line() {
    assert!(matches!(
        ingest_ndjson("not json at all"),
        Err(runtrace::TraceError::Ingest(_))
    ));
}
