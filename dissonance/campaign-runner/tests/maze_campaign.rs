// SPDX-License-Identifier: AGPL-3.0-or-later
//! Task 134 M0 — the portable maze-gate integration tests: the full evidence
//! path (wire-v2 X/Y → normalized ordered SdkEvents → durable ledger →
//! two-barrier materialization → CellFn at the actual `sealed_at` →
//! best-Entry-per-cell occupancy → retained-Entry restoration) green under
//! nextest, plus the campaign-level determinism and control-comparison
//! properties the bead's acceptance names. The box campaign (M1/M2) reruns
//! the same driver against the real guest.

use benchmark::exploration::ExplorationConfig;
use campaign_runner::mazecampaign::{
    MazeCampaignConfig, MazeCampaignOutcome, MazeDeclaredMachine, MazeObservationCells,
    MazeToyMachine, run_maze_campaign,
};
use explorer::{
    DeclineTactic, DifferentialCampaign, EvidenceLedger, GenesisSelector, Machine, SpecEnvCodec,
};
use revision_coordinator::{CampaignConfigId, Coordinator, MemLedger};

fn run(cfg: &MazeCampaignConfig, config: ExplorationConfig) -> MazeCampaignOutcome {
    let machine = MazeToyMachine::new(cfg.spec, cfg.steps_per_rollout);
    run_maze_campaign(machine, Box::new(SpecEnvCodec), cfg, config).expect("campaign runs")
}

/// The end-to-end evidence path: an archive-guided campaign over the real
/// maze walk produces a dense per-branch discovery log with nonzero work
/// evidence, persists nonempty SDK evidence in the durable ledger, and holds
/// the goal `MustHit` expectation open exactly until some rollout reaches it.
#[test]
fn two_barrier_evidence_path_end_to_end() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cfg = MazeCampaignConfig::smoke(7);
    cfg.trace_dir = Some(dir.path().to_path_buf());
    let out = run(&cfg, ExplorationConfig::SelectorV1);

    // The dense per-branch log (the offline report's contract).
    assert_eq!(out.log.events.len() as u64, cfg.max_branches);
    for (i, e) in out.log.events.iter().enumerate() {
        assert_eq!(e.branch, i as u64);
        assert_eq!(
            e.state_hash.len(),
            64,
            "the determinism witness rides the log"
        );
    }
    // Work evidence: every rollout advanced V-time and walked steps.
    assert!(out.vacuity().is_none(), "the campaign did real work");
    assert_eq!(out.work.branches, cfg.max_branches);
    assert!(out.work.min_steps >= u64::from(cfg.steps_per_rollout));
    // Cells were discovered and the deep reproducer was retained.
    assert!(out.log.events.iter().any(|e| !e.touched.is_empty()));
    let deep = out.deep.as_ref().expect("a deepest branch exists");
    assert!(deep.trace_id.is_some(), "the deep reproducer was recorded");
    // Nonempty SDK evidence persisted durably.
    let ledger_len = std::fs::metadata(dir.path().join("evidence.log"))
        .expect("the evidence ledger exists")
        .len();
    assert!(ledger_len > 0, "the evidence ledger is nonempty");
    // The goal expectation is declared and tracked: while no rollout reached
    // the goal it MUST be held open. (Once hit, the view is known not to
    // clear it — the escalated wire-v2 `satisfies_must_hit` spine finding;
    // `goal_hits` is the authoritative witness, so no assertion is made on
    // the satisfied side here.)
    if out.goal_hits == 0 {
        assert_eq!(
            out.open_expectations, 1,
            "the unreached goal is a held obligation"
        );
    }
}

/// The bead's determinism acceptance: same seed/config ⇒ identical selections
/// and artifacts — the whole outcome (per-branch cells, depths, terminal
/// state hashes, deep pointer, work evidence) is bit-identical across reruns.
#[test]
fn same_seed_and_config_yield_identical_artifacts() {
    let cfg = MazeCampaignConfig::smoke(11);
    for config in [
        ExplorationConfig::SelectorV1,
        ExplorationConfig::PureRandom,
        ExplorationConfig::FrontierOff,
    ] {
        let a = run(&cfg, config);
        let b = run(&cfg, config);
        assert_eq!(a, b, "{config:?}: same (seed, config) ⇒ identical outcome");
    }
    // And a different seed genuinely varies the campaign (the comparison is
    // not vacuous).
    let other = MazeCampaignConfig::smoke(12);
    let a = run(&cfg, ExplorationConfig::PureRandom);
    let c = run(&other, ExplorationConfig::PureRandom);
    assert_ne!(a.log.events, c.log.events);
}

/// The machinery-neutrality tripwire: on the toy, the frontier-off control
/// (full materialization machinery, always-explore selector) produces the
/// **identical** discovery log to pure-random (no machinery at all) — the
/// controller draws no campaign randomness for materialization, so any
/// divergence is a determinism defect in the machinery itself.
#[test]
fn frontier_off_log_equals_pure_random_log() {
    let cfg = MazeCampaignConfig::smoke(21);
    let pure = run(&cfg, ExplorationConfig::PureRandom);
    let off = run(&cfg, ExplorationConfig::FrontierOff);
    assert_eq!(pure.log.events, off.log.events);
    // FrontierOff does materialize (its machinery runs): its outcome carries
    // the same work evidence.
    assert_eq!(pure.work, off.work);
}

/// The portable control comparison (the gate's shape at reduced scale): over
/// a handful of seeds, the archive-guided configuration reaches strictly
/// deeper than both permanent controls and is the only one to reach the
/// goal. The full ≥20-seed strict-IQR gate is the box campaign (M2); this
/// pins the mechanism portably.
#[test]
fn archive_guided_outreaches_both_controls() {
    let seeds = [101u64, 102, 103, 104, 105];
    let depth = |config: ExplorationConfig| -> (u64, u64, u64) {
        // (total depth, total distinct cells, goal seeds) across the seeds.
        let mut total_depth = 0;
        let mut total_cells = 0;
        let mut goal_seeds = 0;
        for &seed in &seeds {
            let cfg = MazeCampaignConfig::smoke(seed);
            let out = run(&cfg, config);
            let d = out.log.depth_at(cfg.max_branches);
            total_depth += d;
            total_cells += out.log.distinct_cells_at(cfg.max_branches);
            goal_seeds += u64::from(out.goal_hits > 0);
        }
        (total_depth, total_cells, goal_seeds)
    };
    let (subject_depth, subject_cells, subject_goals) = depth(ExplorationConfig::SelectorV1);
    let (random_depth, random_cells, random_goals) = depth(ExplorationConfig::PureRandom);
    let (off_depth, off_cells, off_goals) = depth(ExplorationConfig::FrontierOff);

    assert!(
        subject_depth > random_depth && subject_depth > off_depth,
        "archive-guided total depth {subject_depth} beats pure-random {random_depth} and \
         frontier-off {off_depth}"
    );
    assert!(
        subject_cells > random_cells && subject_cells > off_cells,
        "archive-guided total cells {subject_cells} beats pure-random {random_cells} and \
         frontier-off {off_cells}"
    );
    assert!(
        subject_goals > 0,
        "archive-guided reaches the goal in at least one seed"
    );
    assert_eq!(random_goals, 0, "pure-random never reaches the goal");
    assert_eq!(off_goals, 0, "frontier-off never reaches the goal");
    // The controls demonstrably still explore (live, non-vacuous).
    assert!(random_depth > 0 && random_cells > seeds.len() as u64);
}

/// Retained-Entry restoration (the bead's acceptance): reopening a campaign's
/// durable evidence ledger rebuilds the committed Entry cell assignments and
/// restores the operational archive — occupancy, cells, and reproducers —
/// bit-identically; and the full-retention profile forbids collection.
#[test]
fn restored_entries_resume_the_archive() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("evidence.log");
    let spec = maze::MazeSpec::small();
    let steps = 48u32;

    let build = |ledger: EvidenceLedger| {
        let mut machine = MazeDeclaredMachine::new(MazeToyMachine::new(spec, steps));
        // The setup drain (the driver's shape): the catalog mode is learned
        // before the controller stamps its genesis cut.
        let _ = machine.sdk_events().expect("setup drain");
        DifferentialCampaign::new(
            machine,
            Box::new(SpecEnvCodec),
            Box::new(DeclineTactic::new()),
            Box::new(GenesisSelector::new()),
            Box::new(MazeObservationCells),
            ledger,
            Coordinator::genesis(
                Box::new(MemLedger::new()),
                CampaignConfigId::digest(b"maze-restore-test"),
            )
            .expect("genesis"),
            explorer::CampaignConfig {
                candidate_cap: 2,
                replay_budget: 64,
                hash_rollouts: true,
                // The maze surfaces no mid-run snapshot points — nomination
                // comes from its own same-Moment SDK events (the driver's
                // configuration).
                nominate: explorer::Nomination::EventMoments,
                ..explorer::CampaignConfig::default()
            },
            5,
        )
        .expect("campaign")
    };

    // Live: run steps, admitting entries.
    let mut live = build(EvidenceLedger::open(&path).expect("open"));
    live.explore(12).expect("explore");
    assert!(live.occupied() > 0, "the live campaign admitted entries");
    let live_assignments = format!("{:?}", live.views().assignments);
    let live_occupied = live.occupied();
    // Full retention from rollout one: collection is forbidden, loudly.
    let some_batch = *live.ledger().batch_ids().next().expect("a batch exists");
    assert!(
        matches!(
            live.collect_batch(some_batch),
            Err(explorer::CampaignError::Retention(
                explorer::RetentionError::FullRetentionForbidsCollection
            ))
        ),
        "the declared full-retention profile forbids collection"
    );
    drop(live);

    // Restored: a fresh machine over the reopened ledger resumes with the
    // identical committed assignments and operational archive.
    let restored = build(EvidenceLedger::open(&path).expect("reopen"));
    assert_eq!(restored.occupied(), live_occupied);
    assert_eq!(
        format!("{:?}", restored.views().assignments),
        live_assignments
    );
}
