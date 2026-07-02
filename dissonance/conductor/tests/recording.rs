// SPDX-License-Identifier: AGPL-3.0-or-later
//! Task-65 recording gates over the portable mock composition.
//!
//! - **Store discipline (gate 5):** under `env-only`, the campaign persists zero
//!   journals yet every `TraceId` is listable and its env loadable; the
//!   retention knob never changes the campaign's report — the store is
//!   write-only to the loop.
//! - **Determinism (the portable shape of the box gate 6):** per seed, runs are
//!   byte-identical (same `TraceId`, identical journal); distinct seeds diverge;
//!   records are non-empty and monotone; every trace reloads losslessly.
//!
//! Set `UPDATE_FIXTURES=1` to (re)write the committed mock-recording fixture the
//! `runtrace` crate decodes.

use conductor::SweepConfig;
use conductor::mock;
use conductor::record::{RecordConfig, run_recording, verify_record, verify_store_reload};
use explorer::StreamId;
use runtrace::{RetentionPolicy, TraceStore};

const CONSOLE: StreamId = StreamId(0);

fn cfg(retain: RetentionPolicy) -> RecordConfig {
    RecordConfig {
        sweep: SweepConfig {
            seeds: vec![0x1111, 0x2222, 0x3333, 0x4444],
            runs_per_seed: 2,
            deadline_delta: None, // run each fork to its clean Hlt terminal
            ..SweepConfig::default()
        },
        retain,
        stream: CONSOLE,
    }
}

fn server() -> vmm_core::control::ControlServer<vmm_backend::MockBackend> {
    mock::server(mock::recording_fork_script()).expect("compose mock recording server")
}

#[test]
fn env_only_persists_no_journals_yet_lists_and_loads_every_env() {
    let dir = tempfile::tempdir().unwrap();
    let store = TraceStore::open(dir.path()).unwrap();
    let mut server = server();

    let report = run_recording(&mut server, &store, &cfg(RetentionPolicy::EnvOnly)).unwrap();
    assert!(
        verify_record(&report, 2).is_empty(),
        "{:?}",
        verify_record(&report, 2)
    );
    // The post-campaign store-reload gate passes too (env sidecars reload and
    // are content-addressed; no journals to reload under env-only).
    assert!(
        verify_store_reload(&store, &report).is_empty(),
        "{:?}",
        verify_store_reload(&store, &report)
    );

    // Zero journals on disk, but every TraceId is listable and its env loads.
    let ids = store.ids().unwrap();
    assert_eq!(ids.len(), 4, "one distinct TraceId per seed");
    for id in &ids {
        assert!(!store.has_journal(*id), "env-only retains no journal");
        assert!(store.env(*id).is_ok(), "the env sidecar loads back");
        assert!(
            matches!(store.load(*id), Err(runtrace::TraceError::NotRetained(_))),
            "the journal is not retained (regenerate by replay)"
        );
    }
    // Every row recorded a non-empty console (the recording fork's banner).
    assert!(report.rows.iter().all(|r| r.records_len > 0));
    assert!(report.rows.iter().all(|r| !r.retained));
}

#[test]
fn the_retention_knob_never_changes_the_campaigns_report() {
    // The store is write-only to the loop: swapping the retention policy changes
    // only which journal *bytes* land on disk — never the TraceIds, stops,
    // record counts, or journal sizes the campaign produces.
    let d1 = tempfile::tempdir().unwrap();
    let d2 = tempfile::tempdir().unwrap();
    let d3 = tempfile::tempdir().unwrap();
    let mut s1 = server();
    let mut s2 = server();
    let mut s3 = server();

    let env_only = run_recording(
        &mut s1,
        &TraceStore::open(d1.path()).unwrap(),
        &cfg(RetentionPolicy::EnvOnly),
    )
    .unwrap();
    let interesting = run_recording(
        &mut s2,
        &TraceStore::open(d2.path()).unwrap(),
        &cfg(RetentionPolicy::Interesting),
    )
    .unwrap();
    let all = run_recording(
        &mut s3,
        &TraceStore::open(d3.path()).unwrap(),
        &cfg(RetentionPolicy::All),
    )
    .unwrap();

    let shape = |r: &conductor::record::RecordReport| -> Vec<(u64, usize, runtrace::TraceId, usize, usize)> {
        r.rows
            .iter()
            .map(|row| (row.seed, row.run, row.trace_id, row.records_len, row.journal_len))
            .collect()
    };
    assert_eq!(
        shape(&env_only),
        shape(&interesting),
        "policy must not change the report"
    );
    assert_eq!(
        shape(&interesting),
        shape(&all),
        "policy must not change the report"
    );
    assert_eq!(env_only.snapshot_vtime, all.snapshot_vtime);
}

#[test]
fn a_recording_campaign_is_deterministic_per_seed_and_divergent_across_seeds() {
    let d1 = tempfile::tempdir().unwrap();
    let d2 = tempfile::tempdir().unwrap();
    let store_a = TraceStore::open(d1.path()).unwrap();
    let store_b = TraceStore::open(d2.path()).unwrap();
    let mut s1 = server();
    let mut s2 = server();
    let a = run_recording(&mut s1, &store_a, &cfg(RetentionPolicy::All)).unwrap();
    let b = run_recording(&mut s2, &store_b, &cfg(RetentionPolicy::All)).unwrap();

    // Two whole campaigns produce identical TraceIds in identical order.
    let ids =
        |r: &conductor::record::RecordReport| r.rows.iter().map(|x| x.trace_id).collect::<Vec<_>>();
    assert_eq!(ids(&a), ids(&b), "the campaign is bit-reproducible");

    // Gate checks pass: per-seed identical, >=2 distinct, records non-empty & monotone.
    assert!(
        verify_record(&a, 2).is_empty(),
        "{:?}",
        verify_record(&a, 2)
    );
    // And the post-campaign store-reload gate (retained journals reload; envs
    // are content-addressed) passes.
    assert!(
        verify_store_reload(&store_a, &a).is_empty(),
        "{:?}",
        verify_store_reload(&store_a, &a)
    );
    assert_eq!(a.rows.len(), 8, "4 seeds x 2 runs");
    assert!(a.rows.iter().all(|r| r.journal_matches_first_run));
    assert!(a.rows.iter().all(|r| r.stamps_monotone));
}

#[test]
fn verify_store_reload_catches_a_missing_retained_journal() {
    // Non-vacuity: the reload gate must actually LOAD each retained journal, not
    // merely check presence. Delete one behind the loop's back and confirm the
    // gate fails (a `record` regression that stops writing journals is caught).
    let dir = tempfile::tempdir().unwrap();
    let store = TraceStore::open(dir.path()).unwrap();
    let mut server = server();
    let report = run_recording(&mut server, &store, &cfg(RetentionPolicy::All)).unwrap();
    assert!(
        verify_store_reload(&store, &report).is_empty(),
        "a clean store reloads: {:?}",
        verify_store_reload(&store, &report)
    );

    let victim = report.rows[0].trace_id;
    std::fs::remove_file(dir.path().join(format!("{victim}.trace"))).unwrap();
    let failures = verify_store_reload(&store, &report);
    assert!(
        failures.iter().any(|f| f.contains("did not load")),
        "a deleted retained journal must fail the reload gate, got {failures:?}"
    );
}

#[test]
fn update_the_committed_mock_recording_fixture() {
    // Not a fixture-drift guard (the mock journal is regenerated when the format
    // bumps); it exists so `UPDATE_FIXTURES=1` refreshes the committed artifact
    // the `runtrace` crate decodes. A no-op otherwise.
    if std::env::var_os("UPDATE_FIXTURES").is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let store = TraceStore::open(dir.path()).unwrap();
    let mut server = server();
    let report = run_recording(&mut server, &store, &cfg(RetentionPolicy::All)).unwrap();
    // The first run's full journal — a real mock-mode conductor recording.
    let id = report.rows[0].trace_id;
    let journal = runtrace::encode(&store.load(id).unwrap()).expect("mock trace encodes");
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../runtrace/tests/fixtures/mock_recording.trace"
    );
    std::fs::write(path, &journal).expect("write mock fixture");
    eprintln!("updated {path} ({} bytes)", journal.len());
}
