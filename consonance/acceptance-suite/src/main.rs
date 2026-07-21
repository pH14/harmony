// SPDX-License-Identifier: AGPL-3.0-or-later
//! `acceptance-suite` CLI: run the determinism oracles for a corpus manifest and
//! print one JSON object with the per-item reports.
//!
//! Exit codes: `0` if every oracle passed, `2` if any oracle failed (it is a
//! detector, not a failure), `1` on an operational error (bad manifest, I/O).
//!
//! For this task the factory registry is the **toy** registry: each item's
//! `source` is interpreted as a `unison` program-generator seed (a decimal
//! integer, or any other string hashed deterministically to one), so the CLI is
//! self-contained with no payload files. The real-VMM registry is wired at
//! integration without touching the library.

use std::process::ExitCode;

use acceptance_suite::{
    CorpusItem, ItemReport, RunConfig, load_manifest, run_item, to_manifest, toy_factory, validate,
};
use clap::{Parser, Subcommand};
use serde::Serialize;
use unison::toy::ZERO_SEED_STATE;

#[derive(Parser)]
#[command(name = "acceptance-suite", version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the applicable oracles for each manifest item; print a JSON report.
    Run {
        /// Path to the corpus manifest (TOML).
        #[arg(long)]
        manifest: String,
        /// Only run the item with this name.
        #[arg(long)]
        item: Option<String>,
        /// Primary seed (O1/O2, and seed_a for O3).
        #[arg(long, default_value_t = 0)]
        seed: u64,
        /// Second, distinct seed for O3. Defaults to `seed` perturbed by the
        /// golden-ratio constant.
        #[arg(long)]
        seed_b: Option<u64>,
        /// O1 checkpoint cadence in work units.
        #[arg(long, default_value_t = 4096)]
        checkpoint_every: u64,
        /// Upper bound on work units per run.
        #[arg(long, default_value_t = 1_000_000)]
        limit: u64,
    },
    /// Round-trip the manifest and check every Conformance item has a golden.
    Validate {
        /// Path to the corpus manifest (TOML).
        #[arg(long)]
        manifest: String,
    },
}

/// The single JSON object the `run` subcommand prints.
#[derive(Serialize)]
struct RunSummary {
    items: Vec<ItemReport>,
    all_passed: bool,
}

/// The toy registry normalizes seed 0 to [`ZERO_SEED_STATE`]; mirror that so the
/// CLI can pick a `seed_b` whose *effective* PRNG state actually differs from
/// `seed_a`'s. Without this, the default `seed = 0` collides with a `seed_b` that
/// equals `ZERO_SEED_STATE`, and O3 would compare two identical effective seeds —
/// a faked failure for honest RNG payloads and a vacuous pass for the seed-leak
/// negative.
fn effective_toy_seed(seed: u64) -> u64 {
    if seed == 0 { ZERO_SEED_STATE } else { seed }
}

/// Default `seed_b`: XOR `seed` with a constant chosen so the *effective* toy
/// seeds never collide. The collision `effective(seed) == effective(seed ^ K)`
/// requires `K ∈ {0, ZERO_SEED_STATE}`; this `K` is neither, so the pair is
/// provably distinct after normalization for every `seed`.
fn default_seed_b(seed: u64) -> u64 {
    const K: u64 = 0xD1B5_4A32_D192_ED03; // != 0 and != ZERO_SEED_STATE
    seed ^ K
}

fn cmd_run(
    manifest: &str,
    item_filter: Option<&str>,
    cfg: &RunConfig,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(manifest)?;
    let items: Vec<CorpusItem> = load_manifest(&text)?;
    if items.is_empty() {
        return Err("manifest has no items: an empty corpus tests nothing".into());
    }
    let read_golden = |path: &str| std::fs::read_to_string(path).ok();

    let mut reports = Vec::new();
    for it in &items {
        if let Some(want) = item_filter
            && it.name != want
        {
            continue;
        }
        // A registered item that declares no oracles would run nothing yet
        // aggregate as green; reject it loudly before it can.
        if it.oracles.is_empty() {
            return Err(format!(
                "item {:?} declares no oracles, so it would test nothing",
                it.name
            )
            .into());
        }
        let factory = toy_factory(&it.source);
        reports.push(run_item(it, &factory, cfg, read_golden)?);
    }

    // Fail loudly if nothing actually ran (e.g. `--item <typo>`): otherwise the
    // empty `.all(..)` below is vacuously true and reports all_passed.
    if reports.is_empty() {
        return Err(format!(
            "--item {:?} matched no item in the manifest ({} present): nothing ran",
            item_filter.unwrap_or(""),
            items.len()
        )
        .into());
    }

    let all_passed = reports.iter().all(ItemReport::passed);
    println!(
        "{}",
        serde_json::to_string(&RunSummary {
            items: reports,
            all_passed
        })?
    );
    Ok(if all_passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}

fn cmd_validate(manifest: &str) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(manifest)?;
    let items = load_manifest(&text)?;
    // Round-trip: re-serialize and re-parse must reproduce the items exactly.
    let round_tripped = load_manifest(&to_manifest(&items))?;
    if round_tripped != items {
        eprintln!("manifest does not round-trip (serialize/parse is not stable)");
        return Ok(ExitCode::from(2));
    }
    match validate(&items) {
        Ok(()) => {
            println!(
                "ok: {} item(s), manifest round-trips, goldens present",
                items.len()
            );
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            eprintln!("invalid manifest: {e}");
            Ok(ExitCode::from(2))
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode, Box<dyn std::error::Error>> {
    match cli.cmd {
        Cmd::Run {
            manifest,
            item,
            seed,
            seed_b,
            checkpoint_every,
            limit,
        } => {
            // A zero limit verifies nothing (compare_runs compares 0 checkpoints,
            // every run halts after 0 work), so it could only ever be vacuously
            // green; reject it.
            if limit == 0 {
                return Err("--limit must be at least 1 (a zero-work run verifies nothing)".into());
            }
            let seed_b = seed_b.unwrap_or_else(|| default_seed_b(seed));
            // Guard: O3 needs two distinct *effective* toy seeds. The default is
            // provably distinct, but a user-supplied --seed-b might collide
            // (e.g. --seed 0 --seed-b 11400714819323198485 == ZERO_SEED_STATE).
            if effective_toy_seed(seed) == effective_toy_seed(seed_b) {
                return Err(format!(
                    "--seed {seed} and --seed-b {seed_b} map to the same effective toy PRNG \
                     state ({}); O3 needs two distinct effective seeds",
                    effective_toy_seed(seed)
                )
                .into());
            }
            let cfg = RunConfig {
                seed,
                seed_b,
                checkpoint_every,
                limit,
            };
            cmd_run(&manifest, item.as_deref(), &cfg)
        }
        Cmd::Validate { manifest } => cmd_validate(&manifest),
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}
