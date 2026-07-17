// SPDX-License-Identifier: AGPL-3.0-or-later
//! The arrangement-sharing / incremental-cost measurement (a first-class
//! deliverable of tasks/120). Reproducible by one documented command:
//!
//! ```sh
//! cargo run --release --example bench
//! ```
//!
//! (Wrap with `/usr/bin/time -l` on macOS for the process peak footprint.)
//!
//! Two tree shapes are grown one rollout per revision, then sealed, then one
//! extra late seal lands on the deepest rollout — the marginal cost of a
//! single later materialization. Each formulation runs in isolation
//! (`BuildOpts`), so its per-revision update counts are attributable. The
//! direct-recompute baseline re-derives every view from the genesis replay at
//! each revision, which is what a non-incremental backend would do.

use std::process::Command;

use differential_lineage::data::{Cut, Fixture, OrderScope, Payload, ReduceOp, Replay, Revision};
use differential_lineage::dataflow::{BuildOpts, Captured, run};
use differential_lineage::generate::{Builder, SplitMix64, cut_moment};
use differential_lineage::referee::Referee;

/// Wall-clock sampling for benchmark reporting only.
#[allow(clippy::disallowed_methods)] // not order-observable: bench wall-time reporting only
fn now() -> std::time::Instant {
    std::time::Instant::now()
}

/// Current resident set size (kilobytes) of this process, via `ps` — no
/// unsafe, adequate for spike-grade footprint reporting.
fn rss_kb() -> u64 {
    let out = Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .expect("run ps");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0)
}

struct Shape {
    name: &'static str,
    rollouts: u32,
    /// Chain (each rollout forks off the previous at its tip) or wide
    /// (random parent, random valid cut).
    chain: bool,
    events_per_segment: u64,
    cuts_per_rollout: u32,
    seed: u64,
}

struct BenchFixture {
    fixture: Fixture,
    replay: Replay,
    /// Revisions carrying one new rollout's evidence, in order.
    evidence_revs: Vec<Revision>,
    /// The seal-wave revision.
    seal_rev: Revision,
    /// The late single-seal revision.
    late_seal_rev: Revision,
}

fn build(shape: &Shape) -> BenchFixture {
    let mut rng = SplitMix64(shape.seed);
    let mut b = Builder::new(shape.name, 0);
    for i in 0..6u32 {
        let op = match i % 4 {
            0 => ReduceOp::Set,
            1 => ReduceOp::Max,
            2 => ReduceOp::Min,
            _ => ReduceOp::Accumulate,
        };
        b.reg(1, 100 + i, op);
    }
    b.source(1, 0, OrderScope::RolloutGlobal);

    let mut rollouts = Vec::new();
    let mut evidence_revs = Vec::new();
    for i in 0..shape.rollouts {
        let rev = 2 + Revision::from(i);
        evidence_revs.push(rev);
        let r = if rollouts.is_empty() {
            b.genesis()
        } else {
            let parent = if shape.chain {
                *rollouts.last().expect("nonempty")
            } else {
                rollouts[rng.below(rollouts.len() as u64) as usize]
            };
            let plen = b.vector(parent).len() as u64;
            let count = if shape.chain {
                plen
            } else {
                let pstart = b.start_of(parent);
                pstart + rng.below(plen - pstart + 1)
            };
            let moment = cut_moment(b.vector(parent), count);
            b.fork(rev, parent, Cut { moment, count })
        };
        rollouts.push(r);
        let mut moment = b.moment(r);
        for _ in 0..shape.events_per_segment {
            moment += rng.below(3);
            let payload = if rng.below(5) == 0 {
                Payload::Note {
                    tag: rng.below(2) as u32,
                }
            } else {
                Payload::Register {
                    reg: 100 + rng.below(6) as u32,
                    value: rng.below(1000) as i64 - 500,
                }
            };
            b.push(rev, r, 0, moment, payload);
        }
        let start = b.start_of(r);
        let len = b.vector(r).len() as u64;
        // Obs-cut counts are a record identity per rollout: dedup the draws.
        let mut cut_counts = std::collections::BTreeSet::new();
        for _ in 0..shape.cuts_per_rollout {
            cut_counts.insert(start + rng.below(len - start + 1));
        }
        for count in cut_counts {
            b.obs_cut(
                rev,
                r,
                Cut {
                    moment: cut_moment(b.vector(r), count),
                    count,
                },
            );
        }
    }

    // The seal wave: one candidate seal per rollout at its tip.
    let seal_rev = 2 + Revision::from(shape.rollouts);
    for (i, r) in rollouts.iter().enumerate() {
        let len = b.vector(*r).len() as u64;
        b.seal(
            seal_rev,
            *r,
            i as u32,
            Cut {
                moment: cut_moment(b.vector(*r), len),
                count: len,
            },
        );
    }

    // One late materialization on the deepest rollout, mid-segment.
    let late_seal_rev = seal_rev + 1;
    let deepest = *rollouts.last().expect("nonempty");
    let start = b.start_of(deepest);
    let len = b.vector(deepest).len() as u64;
    let count = start + (len - start) / 2;
    b.seal(
        late_seal_rev,
        deepest,
        shape.rollouts,
        Cut {
            moment: cut_moment(b.vector(deepest), count),
            count,
        },
    );

    let (fixture, replay) = b.finish();
    BenchFixture {
        fixture,
        replay,
        evidence_revs,
        seal_rev,
        late_seal_rev,
    }
}

struct Formulation {
    label: &'static str,
    prefix: &'static str,
    opts: BuildOpts,
}

/// A formulation's attributable updates at one revision: its own stages PLUS
/// the lineage stages (`lineage.anc` + `lineage.step`) — the ancestry work is
/// part of evaluating observations under either formulation, and excluding
/// it understated depth-dependent cost (r1 metering correction; the summing
/// itself fixed in r5).
fn attributable_at(cap: &Captured, prefix: &str, rev: Revision) -> u64 {
    cap.delta_at(prefix, rev) + cap.delta_at("lineage.", rev)
}

fn attributable_total(cap: &Captured, prefix: &str) -> u64 {
    cap.delta_total(prefix) + cap.delta_total("lineage.")
}

fn per_branch_deltas(cap: &Captured, prefix: &str, revs: &[Revision]) -> Vec<u64> {
    revs.iter()
        .map(|r| attributable_at(cap, prefix, *r))
        .collect()
}

/// Everything a judge needs to recompute the table figures: raw per-stage
/// per-revision update counts for each isolated run, the marker revisions,
/// and the derived figures as printed. Written to `bench-results.json`
/// (committed from the canonical run). Update counts are deterministic;
/// wall times are environment-dependent and included for context only.
#[derive(serde::Serialize)]
struct ShapeArtifact {
    shape: String,
    events: usize,
    branches: u32,
    candidate_seals: usize,
    evaluation_points: usize,
    evidence_revs: Vec<Revision>,
    seal_rev: Revision,
    late_seal_rev: Revision,
    runs: Vec<RunArtifact>,
    recompute_wall_ms_all_revisions: u128,
    recompute_wall_ms_final_revision: u128,
    recompute_rows_final: usize,
}

#[derive(serde::Serialize)]
struct RunArtifact {
    formulation: String,
    stage_prefix: String,
    /// Raw metered updates: (stage, revision, count). The attributable
    /// figures are recomputed as
    /// `sum(count where stage starts_with(stage_prefix) or "lineage.")`.
    deltas: Vec<(String, Revision, u64)>,
    wall_ms: u128,
    derived_attributable_total: u64,
    derived_per_branch: Vec<u64>,
    derived_seal_wave: u64,
    derived_late_seal: u64,
}

fn main() {
    let mut artifacts: Vec<ShapeArtifact> = Vec::new();
    let shapes = [
        Shape {
            name: "deep-chain",
            rollouts: 40,
            chain: true,
            events_per_segment: 400,
            cuts_per_rollout: 3,
            seed: 7,
        },
        Shape {
            name: "wide-tree",
            rollouts: 60,
            chain: false,
            events_per_segment: 200,
            cuts_per_rollout: 3,
            seed: 11,
        },
    ];
    let formulations = [
        Formulation {
            label: "naive (per-point prefix join)",
            prefix: "naive.",
            opts: BuildOpts {
                naive: true,
                shared: false,
                prefix: false,
            },
        },
        Formulation {
            label: "shared (segment aggregates)",
            prefix: "shared.",
            opts: BuildOpts {
                naive: false,
                shared: true,
                prefix: false,
            },
        },
    ];

    println!("# differential-lineage benchmark\n");
    println!("Command: `cargo run --release --example bench`\n");

    for shape in &shapes {
        let bf = build(shape);
        let fx = &bf.fixture;
        let seals = fx.seals.len();
        let points = fx.obs_cuts.len() + seals;
        println!(
            "## {} — {} events, {} branches, {} candidate seals, {} evaluation points\n",
            shape.name,
            fx.events.len(),
            shape.rollouts,
            seals,
            points,
        );
        println!(
            "| formulation | total updates | wall | first branch | median branch | deepest branch | seal wave | late seal | rss after |"
        );
        println!("|---|---|---|---|---|---|---|---|---|");

        let mut run_artifacts: Vec<RunArtifact> = Vec::new();
        for f in &formulations {
            let t0 = now();
            let cap = run(fx, f.opts, shape.seed).expect("valid fixture");
            let wall = t0.elapsed();
            let per_branch = per_branch_deltas(&cap, f.prefix, &bf.evidence_revs);
            run_artifacts.push(RunArtifact {
                formulation: f.label.to_owned(),
                stage_prefix: f.prefix.to_owned(),
                deltas: cap
                    .deltas
                    .iter()
                    .map(|((stage, rev), count)| (stage.clone(), *rev, *count))
                    .collect(),
                wall_ms: wall.as_millis(),
                derived_attributable_total: attributable_total(&cap, f.prefix),
                derived_per_branch: per_branch.clone(),
                derived_seal_wave: attributable_at(&cap, f.prefix, bf.seal_rev),
                derived_late_seal: attributable_at(&cap, f.prefix, bf.late_seal_rev),
            });
            let mut sorted = per_branch.clone();
            sorted.sort_unstable();
            let median = sorted[sorted.len() / 2];
            println!(
                "| {} | {} | {:.2?} | {} | {} | {} | {} | {} | {} MB |",
                f.label,
                attributable_total(&cap, f.prefix),
                wall,
                per_branch.first().copied().unwrap_or(0),
                median,
                per_branch.last().copied().unwrap_or(0),
                attributable_at(&cap, f.prefix, bf.seal_rev),
                attributable_at(&cap, f.prefix, bf.late_seal_rev),
                rss_kb() / 1024,
            );
            println!(
                "|   of which lineage stages | {} | | {} | | {} | {} | {} | |",
                cap.delta_total("lineage."),
                cap.delta_at("lineage.", bf.evidence_revs[0]),
                cap.delta_at("lineage.", *bf.evidence_revs.last().expect("nonempty")),
                cap.delta_at("lineage.", bf.seal_rev),
                cap.delta_at("lineage.", bf.late_seal_rev),
            );
            let downstream =
                cap.delta_total("") - attributable_total(&cap, f.prefix) - cap.delta_total("base.");
            println!(
                "|   base ingestion (common, excluded above) | {} | | | | | | | |",
                cap.delta_total("base."),
            );
            println!(
                "|   downstream views (cells/transitions/…, common, excluded above) | {downstream} | | | | | | | |",
            );
        }

        // Direct-recompute baseline (r6): ONE coherent snapshot per revision —
        // each evaluation point's prefix is folded exactly once and every
        // view derives from that fold (Referee::snapshot; equality with the
        // individual views is asserted across the parity suite). The old
        // baseline called obs/cells/transitions/occupancy independently,
        // re-replaying each prefix 3-4x, which overstated recompute cost.
        let referee = Referee::new(fx, &bf.replay).expect("valid fixture");
        let t0 = now();
        let mut rows_final = 0usize;
        for rev in 0..=fx.max_rev() {
            let snap = referee.snapshot(rev);
            rows_final =
                snap.obs.len() + snap.cells.len() + snap.transitions.len() + snap.occupancy.len();
        }
        let wall_every = t0.elapsed();
        let t0 = now();
        let _ = referee.snapshot(fx.max_rev());
        let wall_once = t0.elapsed();
        println!(
            "| direct recompute (plain Rust, per revision) | {} rows final | {:.2?} (once: {:.2?}) | — | — | — | — | — | {} MB |",
            rows_final,
            wall_every,
            wall_once,
            rss_kb() / 1024,
        );
        println!();

        artifacts.push(ShapeArtifact {
            shape: shape.name.to_owned(),
            events: fx.events.len(),
            branches: shape.rollouts,
            candidate_seals: seals,
            evaluation_points: points,
            evidence_revs: bf.evidence_revs.clone(),
            seal_rev: bf.seal_rev,
            late_seal_rev: bf.late_seal_rev,
            runs: std::mem::take(&mut run_artifacts),
            recompute_wall_ms_all_revisions: wall_every.as_millis(),
            recompute_wall_ms_final_revision: wall_once.as_millis(),
            recompute_rows_final: rows_final,
        });

        // Determinism spot check: the shared run's update stream is identical
        // across reruns.
        let a = run(fx, formulations[1].opts, shape.seed).expect("valid fixture");
        let b = run(fx, formulations[1].opts, shape.seed).expect("valid fixture");
        assert_eq!(a.deltas, b.deltas, "nondeterministic update counts");
        println!(
            "shared-formulation rerun determinism: OK (identical per-revision update counts)\n"
        );
    }

    let artifact_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("bench-results.json");
    let mut json = serde_json::to_string_pretty(&artifacts).expect("serialize artifacts");
    json.push('\n');
    std::fs::write(&artifact_path, json).expect("write bench artifact");
    println!(
        "Raw measurement artifact (recompute any table figure from it): {}\n",
        artifact_path.display()
    );

    println!("## Arrangement sharing (static)\n");
    println!(
        "In the full both-formulations graph: one `measures-by-rollout` arrangement \
         feeds three consumers (naive own-segment join, naive ancestor join, shared \
         interval assignment); one `points-by-rollout` arrangement feeds four (naive \
         own/ancestor, shared inherited/own); one `evidence-by-rollout` arrangement \
         feeds the two seal-prefix joins and is built only when that view is on. \
         Isolated benchmark runs build the same arrangements with only their own \
         consumers attached. Cloning an `Arranged` shares the trace — each index is \
         built and maintained once per run."
    );
}
