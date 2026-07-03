// SPDX-License-Identifier: AGPL-3.0-or-later
//! **Portable campaign gate (task 60, acceptance gate 2).** The whole first
//! campaign — snapshot once, seed-driven fault search, crash-oracle judging,
//! `Bug` emission, and N/N replay verification — driven against the in-crate
//! [`ToyPlantedMachine`] (a planted bug we own the trigger of), with no
//! `/dev/kvm`. The **identical** [`run_campaign`] loop drives the real socket
//! `Machine` + Postgres-campaign image on the box (gate 1).
//!
//! The milestone's letter, on the toy: the campaign, started with **no
//! knowledge of the trigger**, finds the planted bug, and the emitted reproducer
//! replays the identical crash (same terminal `StopReason`, same `state_hash`)
//! 25/25; a nominal-seed control run does not crash.

use conductor::campaign::{CampaignConfig, render_campaign_table, run_campaign, verify_campaign};
use conductor::planted::{ToyPlantedMachine, Trigger};
use explorer::{SpecEnvCodec, StopReason};

/// The milestone, on the portable toy: found, reproduced 25/25, nominal clean.
#[test]
fn campaign_finds_planted_bug_and_reproduces_25_of_25() {
    let mut m = ToyPlantedMachine::new(Trigger::toy());
    let cfg = CampaignConfig::toy();
    let report = run_campaign(&mut m, &SpecEnvCodec, &cfg).expect("campaign runs to completion");

    let failures = verify_campaign(&report, cfg.replay_n);
    assert!(
        failures.is_empty(),
        "campaign gates failed: {failures:?}\n{}",
        render_campaign_table(&report, cfg.replay_n)
    );

    let found = report.found.as_ref().expect("a bug was found");
    // The bug terminal is a Crash (the guest rebooted); the clean control halts
    // (Quiescent), which is what makes it distinguishable.
    assert!(
        matches!(found.stop, StopReason::Crash { .. }),
        "expected a Crash, got {:?}",
        found.stop
    );
    assert!(
        matches!(report.nominal.stop, StopReason::Quiescent { .. }),
        "nominal control should halt (Quiescent), got {:?}",
        report.nominal.stop
    );
    // Found within the naive-search order the spec asks for (~10²–10³ branches).
    assert!(
        found.branch_index < 2_000,
        "planted bug should be found within a naive budget, was at branch {}",
        found.branch_index
    );
    // Every one of the N replays is present.
    assert_eq!(report.replays.len(), cfg.replay_n);
}

/// The campaign is a pure function of `(campaign_seed, machine)`: a rerun
/// explores the identical branch sequence and finds the bug at the identical
/// branch, with identical hashes.
#[test]
fn campaign_is_deterministic() {
    let run = || {
        let mut m = ToyPlantedMachine::new(Trigger::toy());
        run_campaign(&mut m, &SpecEnvCodec, &CampaignConfig::toy()).expect("campaign runs")
    };
    let a = run();
    let b = run();
    assert_eq!(a.base_hash, b.base_hash);
    let (fa, fb) = (a.found.unwrap(), b.found.unwrap());
    assert_eq!(fa.branch_index, fb.branch_index);
    assert_eq!(fa.seed, fb.seed);
    assert_eq!(fa.hash, fb.hash);
    assert_eq!(fa.env, fb.env);
}

/// The finder is not hard-coded to one trigger: replanting the bug at a
/// different `(gpa, mask, window)` still finds and reproduces it — the campaign
/// searches, it does not cheat.
#[test]
fn finder_adapts_to_a_replanted_bug() {
    let replanted = Trigger {
        gpa: 0x1000,
        mask: 1 << 7,
        window: (
            conductor::planted::BASE_VTIME,
            conductor::planted::BASE_VTIME + 8,
        ),
    };
    let mut m = ToyPlantedMachine::new(replanted);
    let cfg = CampaignConfig::toy();
    let report = run_campaign(&mut m, &SpecEnvCodec, &cfg).expect("campaign runs");
    let failures = verify_campaign(&report, cfg.replay_n);
    assert!(
        failures.is_empty(),
        "replanted-bug gates failed: {failures:?}\n{}",
        render_campaign_table(&report, cfg.replay_n)
    );
}

/// A trigger the search space cannot express (a gpa outside the candidate set)
/// is never found — the campaign fails loud (no silent pass), and the nominal
/// control still does not crash.
#[test]
fn an_unreachable_trigger_fails_loud() {
    let unreachable = Trigger {
        gpa: 0xDEAD_0000, // not in CampaignConfig::toy().gpa_candidates
        mask: 1 << 31,
        window: (
            conductor::planted::BASE_VTIME + 3,
            conductor::planted::BASE_VTIME + 4,
        ),
    };
    let mut m = ToyPlantedMachine::new(unreachable);
    // Keep the budget small so the no-find test is quick.
    let cfg = CampaignConfig {
        max_branches: 256,
        ..CampaignConfig::toy()
    };
    let report = run_campaign(&mut m, &SpecEnvCodec, &cfg).expect("campaign runs");
    assert!(
        report.found.is_none(),
        "an out-of-space trigger cannot be found"
    );
    let failures = verify_campaign(&report, cfg.replay_n);
    assert!(
        failures.iter().any(|f| f.contains("no planted bug found")),
        "a no-find campaign must fail the gate loudly, got {failures:?}"
    );
    assert!(
        !report.nominal.is_bug,
        "the nominal control still must not crash"
    );
}
