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

fn server() -> vmm_core::control::ControlServer<conductor::mock::CountingBackend> {
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

    // The strong guest-state property (mirroring the task-58 sweep): per seed
    // the state_hash is identical across runs, and across seeds it diverges.
    use std::collections::{BTreeMap, BTreeSet};
    let mut by_seed: BTreeMap<u64, Vec<[u8; 32]>> = BTreeMap::new();
    for r in &a.rows {
        by_seed.entry(r.seed).or_default().push(r.state_hash);
    }
    for (seed, hashes) in &by_seed {
        assert!(
            hashes.windows(2).all(|w| w[0] == w[1]),
            "seed {seed:#018x}: state_hash not reproducible across runs"
        );
    }
    let distinct: BTreeSet<[u8; 32]> = a.rows.iter().map(|r| r.state_hash).collect();
    assert!(
        distinct.len() >= 2,
        "guest states did not diverge across seeds ({} distinct)",
        distinct.len()
    );
    // Cross-run state_hash reproducibility holds between whole campaigns too.
    let hashes = |r: &conductor::record::RecordReport| {
        r.rows.iter().map(|x| x.state_hash).collect::<Vec<_>>()
    };
    assert_eq!(hashes(&a), hashes(&b), "state hashes are bit-reproducible");
}

#[test]
fn verify_record_flags_non_diverging_guest_state() {
    // Non-vacuity of the state_hash divergence gate: a synthetic report whose
    // seeds share one state_hash (an RDRAND-seeding regression: identical guest
    // state despite distinct seeds/journals) must FAIL divergence, even though
    // per-seed determinism holds.
    use conductor::record::{RecordReport, RecordedRun};
    use explorer::{StopReason, VTime};
    let row = |seed: u64, run: usize, id_byte: u8| RecordedRun {
        seed,
        run,
        trace_id: runtrace::TraceId([id_byte; 32]),
        stop: StopReason::Quiescent { vtime: VTime(1) },
        state_hash: [0xEE; 32], // SAME across every seed — no guest divergence
        records_len: 1,
        journal_len: 10,
        journal_digest: [id_byte; 32],
        retained: true,
        stamps_monotone: true,
        journal_matches_first_run: true,
        console_head: vec![],
    };
    let report = RecordReport {
        snapshot_vtime: 0,
        rows: vec![
            row(0x1111, 0, 1),
            row(0x1111, 1, 1),
            row(0x2222, 0, 2),
            row(0x2222, 1, 2),
        ],
    };
    let failures = verify_record(&report, 2);
    assert!(
        failures
            .iter()
            .any(|f| f.contains("state_hash(es)") && f.contains("did not diverge")),
        "identical guest state across seeds must fail divergence, got {failures:?}"
    );
}

#[test]
fn verify_record_flags_folded_reproducers() {
    // Mirror non-vacuity of the TraceId divergence gate (the spec's letter): the
    // guest states diverge across seeds, but the envs (hence TraceIds) collapsed
    // to one — the content-addressed store would fold N reproducers into one,
    // losing env-only replay. Must FAIL the TraceId check even though the
    // state_hash divergence passes.
    use conductor::record::{RecordReport, RecordedRun};
    use explorer::{StopReason, VTime};
    let row = |seed: u64, run: usize, hash_byte: u8| RecordedRun {
        seed,
        run,
        trace_id: runtrace::TraceId([0x11; 32]), // SAME across every seed — envs folded
        stop: StopReason::Quiescent { vtime: VTime(1) },
        state_hash: [hash_byte; 32], // distinct per seed — guest states DO diverge
        records_len: 1,
        journal_len: 10,
        journal_digest: [hash_byte; 32],
        retained: true,
        stamps_monotone: true,
        journal_matches_first_run: true,
        console_head: vec![],
    };
    let report = RecordReport {
        snapshot_vtime: 0,
        rows: vec![
            row(0x1111, 0, 0xA),
            row(0x1111, 1, 0xA),
            row(0x2222, 0, 0xB),
            row(0x2222, 1, 0xB),
        ],
    };
    let failures = verify_record(&report, 2);
    assert!(
        failures
            .iter()
            .any(|f| f.contains("TraceId(s)") && f.contains("folded")),
        "collapsed TraceIds across seeds must fail divergence, got {failures:?}"
    );
    // The failure is specifically the TraceId check — state_hash divergence passes.
    assert!(
        !failures.iter().any(|f| f.contains("state_hash(es)")),
        "state_hash divergence should pass (distinct hashes), got {failures:?}"
    );
}

/// Non-vacuity of the per-run gates `verify_record_flags_non_diverging_guest_state`
/// and `verify_record_flags_folded_reproducers` don't reach: a lone run per
/// seed, empty records, non-monotone stamps, and a journal that drifted from
/// the seed's first run — each must fail on its own, even when every other
/// gate in the report passes.
#[test]
fn verify_record_flags_each_per_run_gate_independently() {
    use conductor::record::{RecordReport, RecordedRun};
    use explorer::{StopReason, VTime};
    let row = |seed: u64, run: usize, id_byte: u8| RecordedRun {
        seed,
        run,
        trace_id: runtrace::TraceId([id_byte; 32]),
        stop: StopReason::Quiescent { vtime: VTime(1) },
        state_hash: [id_byte; 32],
        records_len: 1,
        journal_len: 10,
        journal_digest: [id_byte; 32],
        retained: true,
        stamps_monotone: true,
        journal_matches_first_run: true,
        console_head: vec![],
    };
    let clean = || {
        vec![
            row(0x1111, 0, 1),
            row(0x1111, 1, 1),
            row(0x2222, 0, 2),
            row(0x2222, 1, 2),
        ]
    };

    // A lone run per seed cannot demonstrate reproducibility.
    let mut rows = clean();
    rows.truncate(1); // only seed 0x1111 run 0 remains
    let failures = verify_record(
        &RecordReport {
            snapshot_vtime: 0,
            rows,
        },
        1,
    );
    assert!(
        failures.iter().any(|f| f.contains("only 1 run")),
        "a seed with a single run must fail reproducibility, got {failures:?}"
    );

    // Empty records.
    let mut rows = clean();
    rows[0].records_len = 0;
    let failures = verify_record(
        &RecordReport {
            snapshot_vtime: 0,
            rows,
        },
        2,
    );
    assert!(
        failures.iter().any(|f| f.contains("no records")),
        "a run with no records must be flagged, got {failures:?}"
    );

    // Non-monotone stamps.
    let mut rows = clean();
    rows[0].stamps_monotone = false;
    let failures = verify_record(
        &RecordReport {
            snapshot_vtime: 0,
            rows,
        },
        2,
    );
    assert!(
        failures.iter().any(|f| f.contains("stamps not monotone")),
        "a non-monotone run must be flagged, got {failures:?}"
    );

    // A journal that drifted from the seed's first run.
    let mut rows = clean();
    rows[1].journal_matches_first_run = false;
    let failures = verify_record(
        &RecordReport {
            snapshot_vtime: 0,
            rows,
        },
        2,
    );
    assert!(
        failures
            .iter()
            .any(|f| f.contains("journal bytes differ from run 0")),
        "a journal mismatch within a seed must be flagged, got {failures:?}"
    );
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
fn verify_store_reload_catches_a_report_row_that_drifted_from_the_stored_trace() {
    // Non-vacuity: the reload gate compares the RELOADED trace against the
    // report row field-by-field, not merely that *something* reloads. Perturb
    // each field the report claims independently (leaving the store itself
    // untouched) and confirm each one is its own failure.
    let dir = tempfile::tempdir().unwrap();
    let store = TraceStore::open(dir.path()).unwrap();
    let mut server = server();
    let report = run_recording(&mut server, &store, &cfg(RetentionPolicy::All)).unwrap();
    assert!(
        verify_store_reload(&store, &report).is_empty(),
        "the clean report reloads"
    );

    let mut terminal_mismatch = report.clone();
    terminal_mismatch.rows[0].stop = explorer::StopReason::Deadline {
        vtime: explorer::VTime(u64::MAX),
    };
    let failures = verify_store_reload(&store, &terminal_mismatch);
    assert!(
        failures
            .iter()
            .any(|f| f.contains("reloaded terminal != recorded stop")),
        "a terminal claimed by the report but not by the store must fail, got {failures:?}"
    );

    let mut records_mismatch = report.clone();
    records_mismatch.rows[0].records_len += 1;
    let failures = verify_store_reload(&store, &records_mismatch);
    assert!(
        failures
            .iter()
            .any(|f| f.contains("reloaded records") && f.contains("!=")),
        "a record-count claim that disagrees with the store must fail, got {failures:?}"
    );

    let mut journal_len_mismatch = report.clone();
    journal_len_mismatch.rows[0].journal_len += 1;
    let failures = verify_store_reload(&store, &journal_len_mismatch);
    assert!(
        failures
            .iter()
            .any(|f| f.contains("reloaded journal") && f.contains("bytes !=")),
        "a journal-length claim that disagrees with the store must fail, got {failures:?}"
    );

    let mut digest_mismatch = report.clone();
    digest_mismatch.rows[0].journal_digest = [0xFF; 32];
    let failures = verify_store_reload(&store, &digest_mismatch);
    assert!(
        failures
            .iter()
            .any(|f| f.contains("reloaded journal digest != recorded digest")),
        "a digest claim that disagrees with the store must fail, got {failures:?}"
    );

    // A row the report claims is NOT retained, but whose journal the store
    // actually holds (a stale/inconsistent report), must also fail.
    let mut retained_flag_flipped = report.clone();
    retained_flag_flipped.rows[0].retained = false;
    let failures = verify_store_reload(&store, &retained_flag_flipped);
    assert!(
        failures
            .iter()
            .any(|f| f.contains("retained=false but a journal is present")),
        "retained=false while the store holds a journal must fail, got {failures:?}"
    );
}

#[test]
fn verify_store_reload_catches_an_id_the_store_never_recorded() {
    // A report referencing a TraceId the store never saw (a construction bug
    // that mints report rows disconnected from what was actually stored) must
    // fail to reload the env sidecar, rather than silently passing.
    use conductor::record::{RecordReport, RecordedRun};
    let dir = tempfile::tempdir().unwrap();
    let store = TraceStore::open(dir.path()).unwrap();
    let report = RecordReport {
        snapshot_vtime: 0,
        rows: vec![RecordedRun {
            seed: 0x1111,
            run: 0,
            trace_id: runtrace::TraceId([0xAB; 32]),
            stop: explorer::StopReason::Quiescent {
                vtime: explorer::VTime(1),
            },
            state_hash: [1; 32],
            records_len: 1,
            journal_len: 10,
            journal_digest: [1; 32],
            retained: true,
            stamps_monotone: true,
            journal_matches_first_run: true,
            console_head: vec![],
        }],
    };
    let failures = verify_store_reload(&store, &report);
    assert!(
        failures.iter().any(|f| f.contains("env did not reload")),
        "an unknown TraceId must fail the env-reload check, got {failures:?}"
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
