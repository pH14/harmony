// SPDX-License-Identifier: AGPL-3.0-or-later
//! `conductor` — the task-58 close-the-loop demo binary.
//!
//! Runs the explorer's socket [`Machine`] against vmm-core's control-transport
//! server over a socketpair, driving the outer loop N steps: snapshot once,
//! branch across seeds, run, hash, replay-check, and print a run table with the
//! task-58 gate verdicts.
//!
//! Two modes:
//!
//! - **`mock`** (default, portable — macOS + Linux, no `/dev/kvm`): a scripted
//!   `MockBackend` guest. Proves the whole wire path end-to-end and that
//!   `branch(seed) → run → hash` is per-seed reproducible and cross-seed
//!   divergent, with no box.
//! - **`box`** (Linux + patched KVM + the built Postgres image + the det-cfl-v1
//!   host): boots the real Postgres workload via `boot_linux_selected`, seals a
//!   mid-workload snapshot, and runs the same sweep against the real guest —
//!   the milestone gate. Every missing precondition is a loud error, never a
//!   vacuous success.
//!
//! ```sh
//! # portable:
//! cargo run -p conductor -- mock --seeds 8 --runs 2
//! # box (per docs/BOX-PINNING.md; use your assigned core):
//! taskset -c <core> cargo run -p conductor --release -- box --seeds 8 --runs 2
//! ```

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use conductor::campaign::{
    CampaignConfig, CampaignReport, render_campaign_table, run_campaign, verify_campaign,
};
use conductor::gamecampaign::{GameCampaignConfig, GameToyMachine, run_game_campaign};
use conductor::planted::{ToyPlantedMachine, Trigger};
use conductor::record::{
    RecordConfig, RecordReport, render_record_table, run_recording, verify_record,
    verify_store_reload,
};
use conductor::{SweepConfig, render_table, run_session, sweep_client, verify};
use environment::{EnvSpec, FaultPolicy};
use explorer::{SpecEnvCodec, StreamId};
use runtrace::{RetentionPolicy, TraceStore};

#[derive(Parser)]
#[command(
    name = "conductor",
    about = "task-58 close-the-loop demo: the explorer's socket Machine vs. vmm-core's control server"
)]
struct Cli {
    #[command(subcommand)]
    mode: Mode,
}

#[derive(Subcommand)]
enum Mode {
    /// Portable scripted-MockBackend guest (no /dev/kvm).
    Mock(SweepArgs),
    /// Box-only: the real Postgres workload on patched KVM.
    Box(BoxArgs),
    /// The task-60 first campaign: find a planted, fault-triggerable bug and
    /// reproduce it N/N.
    #[command(subcommand)]
    Campaign(CampaignMode),
    /// Task-68 lazy-materialization chain demo over the mock guest (portable;
    /// the box-side gates run via `cargo test -p conductor --test
    /// live_materialization -- --ignored` on the determinism box).
    Materialize(MatArgs),
    /// Task 69 M2 (GO/NO-GO #2): ONE benchmark campaign — a `(bug, config, seed)`
    /// signal-vs-baseline run against a real planted-bug guest on patched KVM,
    /// emitting the `CampaignLog` + finding state hashes as JSON. Box-only; one
    /// campaign per invocation so the operator parallelizes across leased cores and
    /// compares solo vs co-tenant outputs (the determinism stress-test).
    BenchCampaign(BenchBoxArgs),
    /// The task-86 SMB game-workload exploration campaign (quiet arm — zero
    /// faults, entropy-only search).
    #[command(subcommand)]
    Game(GameMode),
}

/// Task 69 M2 box benchmark-campaign arguments.
#[derive(Parser)]
struct BenchBoxArgs {
    /// Which planted bug (manifest `BugId`): `1` fault-timing, `2`
    /// ordering-interrupt, `3` rare-entropy.
    #[arg(long)]
    bug: u16,
    /// The configuration: `signal` (the real task-67 `LogSensor` + `CellFnV1`) or
    /// `baseline` (task 60's blind seed search).
    #[arg(long)]
    config: String,
    /// The campaign seed. Klees discipline: run ≥20 distinct seeds per config (one
    /// invocation each), so the offline report meets its per-bug/config floor.
    #[arg(long)]
    seed: u64,
    /// Branch budget: the campaign logs to this many branches even after a find, so
    /// measure 1 (discovery at equal budget) is comparable. Must be > 0 — a
    /// zero-branch campaign is a measurement of nothing.
    #[arg(long, default_value_t = 4096, value_parser = parse_positive_u64)]
    max_branches: u64,
    /// Replays to certify a find (bit-identical crash). Floored at the spec
    /// [`REPLAY_BAR`] (25) — the flag may raise it, never lower it.
    #[arg(long, default_value_t = REPLAY_BAR)]
    replay_n: usize,
    /// V-time each branch runs past the sealed base before its deadline — far
    /// enough for the fault to land and the guest to react, but bounded so a
    /// non-triggering / hung branch cannot wedge the campaign. Scope it to the
    /// workload's fault-sensitive loop, well under any real hang. Must be > 0.
    #[arg(long, default_value_t = 5_000_000_000, value_parser = parse_positive_u64)]
    deadline_delta: u64,
    /// Optional **box-calibrated** manifest JSON (a serialized `Benchmark` whose
    /// per-bug trigger params are the pinned real gpa / window-offset / prefix).
    /// Absent = the `wave5()` fixture, whose windows are toy stand-ins — calibrate
    /// for the box (see IMPLEMENTATION-task69-m2.md's runbook).
    #[arg(long)]
    calibration: Option<PathBuf>,
    /// Kernel bzImage filename under guest/build (or guest/linux).
    #[arg(long, default_value = "bzImage")]
    kernel: String,
    /// The bug's initramfs (default the fault-timing campaign image; override
    /// per bug, e.g. `initramfs-order.cpio.gz` / `initramfs-uuid.cpio.gz`).
    #[arg(long, default_value = "initramfs-campaign.cpio.gz")]
    initramfs: String,
    /// The readiness marker after which the base snapshot is sealed (mid-workload,
    /// post-readiness) — per bug (`CAMPAIGN_READY` / `ORDER_READY` / `UUID_READY`).
    #[arg(long, default_value = "CAMPAIGN_READY")]
    ready_marker: String,
    /// Where to write the `CampaignLog` JSON (the offline `benchmark-report` input).
    #[arg(long)]
    out: PathBuf,
    /// Optional: write every branch's `RunTrace` (ordered `(branch, trace)` JSON) to
    /// this path — the **retained re-key substrate** (M2 amendment / `docs/SCORING.md`
    /// R1/R2): the offline evaluation set a future `CellFn` replays through its pure
    /// fold. A first-class deliverable of every M2 campaign; absent = not retained.
    #[arg(long)]
    record: Option<PathBuf>,
    /// Signal-config explore period (every Nth step explores; the rest exploit). The
    /// PR#90 ablation sets `1` (explore-only). An **explicit, recorded** knob (lands
    /// in the CampaignLog) — no ambient env, so a same-seed result is self-describing
    /// (PR#90 round-2 replaced the old `BENCH_EXPLORE_PERIOD` env read). Must be > 0.
    #[arg(long, default_value_t = 4, value_parser = parse_positive_u64)]
    explore_period: u64,
    /// Bug-2 (`OrderingInterrupt`) mint fault-offset search width. Same recorded,
    /// no-env rule as `--explore-period` (replaced `BENCH_ORDER_RANGE`). Irrelevant
    /// to bugs 1/3.
    #[arg(long, default_value_t = 64)]
    order_range: u64,
}

/// The two game-campaign paths (task 86 M0): the portable SMB-shaped toy, and
/// the box campaign against the real game image (QuickNES + the user-supplied
/// ROM under the play-agent).
#[derive(Subcommand)]
enum GameMode {
    /// Portable SMB-shaped toy machine (no /dev/kvm, no ROM) — the laptop
    /// smoke for the campaign loop + host-side cell key.
    Mock(GameArgs),
    /// Box-only: the real game image on patched KVM. `--repeat 25` reruns the
    /// identical campaign and enforces the per-branch state_hash determinism
    /// gate (task 86 gate 2).
    Box(GameBoxArgs),
}

/// Shared game-campaign knobs (both modes).
#[derive(Parser)]
struct GameArgs {
    /// The configuration: `pure-random` (the task-84-ruled primary baseline) or
    /// `selector-v1` (the existing default novelty search — the attribution
    /// column). `signal` is refused until an M1 selector artifact exists.
    #[arg(long, default_value = "pure-random")]
    config: String,
    /// Branch budget (identical across configurations — task 84's ruling).
    /// Must be > 0: a zero-branch campaign logs nothing, and the determinism
    /// gate would find two empty logs identical (task 103 finding 1).
    #[arg(long, default_value_t = 64, value_parser = parse_positive_u64)]
    max_branches: u64,
    /// The campaign stream seed; the whole campaign is a pure function of it.
    #[arg(long, default_value_t = 0x0086_5F5C_0770_0001)]
    campaign_seed: u64,
    /// SelectorV1 only: every Nth step explores fresh from the base.
    #[arg(long, default_value_t = 4, value_parser = parse_positive_u64)]
    explore_period: u64,
    /// Append the campaign's ExplorationLog (JSON array) here — the input
    /// `exploration-report` renders (task 86 gate 3).
    #[arg(long)]
    logs_out: Option<PathBuf>,
    /// The sha256 of the ROM the image carries (echoed by `game-image` and on
    /// the boot serial as GAME_ROM_SHA256) — stamped into the log so the
    /// offline report refuses logs from a different dump.
    #[arg(long)]
    rom_sha256: Option<String>,
    /// Record the deepest branch's reproducer (canonical env + full journal,
    /// incl. the REG_FRAME moments film needs) into this task-65 trace-store
    /// directory — the hm-5sv day-one retention discipline.
    #[arg(long)]
    trace_out: Option<PathBuf>,
}

/// Box-game arguments: the image/marker knobs plus the rollout deadline and
/// the determinism-gate repeat count.
#[derive(Parser)]
struct GameBoxArgs {
    #[command(flatten)]
    game: GameArgs,
    /// V-time (ns) each rollout runs past the sealed base before its deadline
    /// (the play-agent never exits on its own). Must be > 0: a zero deadline is
    /// already met at the base, so every rollout would hash the sealed base
    /// itself and the determinism gate would compare it to itself (task 103
    /// finding 1).
    #[arg(long, default_value_t = 5_000_000_000, value_parser = parse_positive_u64)]
    deadline_delta: u64,
    /// V-time (ns) allowed for the agent to reach its `setup_complete`
    /// snapshot point past the GAME_READY marker. Must be > 0 (a zero
    /// allowance can never reach the snapshot point).
    #[arg(long, default_value_t = 30_000_000_000, value_parser = parse_positive_u64)]
    setup_deadline_delta: u64,
    /// Rerun the identical campaign this many times (fresh boot each) and
    /// require bit-identical logs — the per-branch state_hash sequence gate is
    /// `--repeat 25`.
    #[arg(long, default_value_t = 1, value_parser = parse_positive_usize)]
    repeat: usize,
    /// Kernel bzImage filename under guest/build (or guest/linux).
    #[arg(long, default_value = "bzImage")]
    kernel: String,
    /// Initramfs filename — defaults to the task-86 game image.
    #[arg(long, default_value = "initramfs-game.cpio.gz")]
    initramfs: String,
    /// The serial marker the boot is driven to before the campaign attaches
    /// (the base itself seals at the agent's setup_complete snapshot point).
    #[arg(long, default_value = "GAME_READY")]
    ready_marker: String,
}

#[derive(Parser)]
struct MatArgs {
    /// Chain seals below the base (>= 3: gate (b) needs a retained ancestor
    /// above the evicted parent).
    #[arg(long, default_value_t = 3)]
    hops: usize,
    /// Requested V-time per hop (the landed boundary keys the exemplar).
    #[arg(long, default_value_t = 250)]
    hop_delta: u64,
    /// The reproducer leg's requested run past the deepest seal.
    #[arg(long, default_value_t = 250)]
    tail_delta: u64,
    /// The chain seed (all hops branch with it — chains are same-seed).
    #[arg(long, default_value_t = 0x1234_5678_9ABC_DEF0)]
    seed: u64,
}

/// The two campaign paths (task 60): the portable toy planted-bug machine, and
/// the box milestone against the real Postgres-campaign image.
#[derive(Subcommand)]
enum CampaignMode {
    /// Portable toy planted-bug machine (no /dev/kvm) — gate 2.
    Mock(CampaignArgs),
    /// Box-only: the real Postgres-campaign image on patched KVM — gate 1.
    Box(CampaignBoxArgs),
}

/// The task-60 milestone replay bar: the emitted reproducer must replay the
/// identical crash (same `state_hash` at the terminal stop) **25/25** (spec gate
/// 1). The `--replay-n` flag may only **raise** this bar, never lower it — a
/// `--replay-n 1` run must not be able to print `GATES PASS` at 1/1 below the
/// spec, so every campaign path floors `replay_n` at `REPLAY_BAR`.
const REPLAY_BAR: usize = 25;

/// Shared campaign knobs (both modes).
#[derive(Parser)]
struct CampaignArgs {
    /// Branch budget: give up **loudly** if the planted bug is not found within
    /// this many branches (a no-find is a gate failure, never a silent pass).
    /// Must be > 0 — a campaign with no branches searches nothing.
    #[arg(long, default_value_t = 4096, value_parser = parse_positive_u64)]
    max_branches: u64,
    /// Replays of the emitted reproducer to prove bit-identical reproduction.
    /// Floored at the spec's [`REPLAY_BAR`] (25) — the flag may raise the bar,
    /// never lower it.
    #[arg(long, default_value_t = REPLAY_BAR)]
    replay_n: usize,
    /// The campaign stream seed. The whole campaign is a pure function of it, so
    /// a rerun explores the identical branch sequence.
    #[arg(long)]
    campaign_seed: Option<u64>,
}

#[derive(Parser)]
struct SweepArgs {
    /// Number of entropy seeds to branch (the divergent futures).
    #[arg(long, default_value_t = 8)]
    seeds: usize,
    /// Runs per seed (>= 2 proves bit-identical reproduction).
    #[arg(long, default_value_t = 2)]
    runs: usize,
    /// Record each run's `RunTrace` into this directory (task 65). **Absent** =
    /// the task-58 socket sweep (no recording). **Present** = the in-process
    /// recording session, which drives `ControlServer::handle` directly so it can
    /// drain the guest console between verbs.
    #[arg(long)]
    record: Option<PathBuf>,
    /// Journal retention when `--record` is set: `all` | `interesting` |
    /// `env-only` (default `interesting`). The tiny env sidecar is *always*
    /// written; this gates the full journal bytes.
    #[arg(long, default_value = "interesting")]
    retain: String,
}

#[derive(Parser)]
struct BoxArgs {
    #[command(flatten)]
    sweep: SweepArgs,
    /// V-time (ns) each branch runs past the snapshot before its deadline.
    /// Must be > 0: a zero-V-time branch runs no workload past the base.
    #[arg(long, default_value_t = 5_000_000_000, value_parser = parse_positive_u64)]
    deadline_delta: u64,
    /// Kernel bzImage filename under guest/build (or guest/linux).
    #[arg(long, default_value = "bzImage")]
    kernel: String,
    /// Initramfs filename under guest/build (or guest/linux). Defaults to the
    /// bare-Postgres image; point at `initramfs-docker.cpio.gz` to reuse the
    /// runc-Postgres image already staged on the box.
    #[arg(long, default_value = "initramfs-postgres.cpio.gz")]
    initramfs: String,
    /// The serial marker after which the base snapshot is sealed (the
    /// mid-workload, post-readiness point the gate wants). Default: the
    /// postmaster-ready banner.
    #[arg(long, default_value = "database system is ready to accept connections")]
    ready_marker: String,
}

/// Box-campaign arguments: the image/marker knobs of a `box` sweep, plus the
/// **seeded fault-search space** the campaign explores.
///
/// The search space is deliberately CLI-tunable: the box operator (the foreman)
/// narrows `--gpa-*` once the planted supervisor's ledger guest-physical address
/// is pinned (read via `/proc/self/pagemap` during a bring-up boot — see
/// `guest/linux/campaign-init.sh`), keeping the naive search inside the box
/// lease. The defaults are a broad, page-strided sweep — a genuine "no knowledge
/// of the trigger" search that completes only once the space is scoped.
#[derive(Parser)]
struct CampaignBoxArgs {
    #[command(flatten)]
    campaign: CampaignArgs,
    /// V-time (ns) each branch runs past the base snapshot before its deadline —
    /// far enough for the fault to land and the supervisor to react. Must be > 0.
    #[arg(long, default_value_t = 5_000_000_000, value_parser = parse_positive_u64)]
    deadline_delta: u64,
    /// Lowest candidate guest-physical fault address (page-aligned). Accepts
    /// decimal or `0x`-hex (a gpa is naturally written in hex).
    #[arg(long, default_value_t = 0x0100_0000, value_parser = parse_u64_flexible)]
    gpa_base: u64,
    /// Number of page-strided candidate addresses.
    #[arg(long, default_value_t = 256, value_parser = parse_u64_flexible)]
    gpa_count: u64,
    /// Stride between candidate addresses (default one 4 KiB page). Decimal or
    /// `0x`-hex.
    #[arg(long, default_value_t = 0x1000, value_parser = parse_u64_flexible)]
    gpa_stride: u64,
    /// Lowest fault-Moment offset past the base V-time (ns). Decimal or `0x`-hex.
    #[arg(long, default_value_t = 0, value_parser = parse_u64_flexible)]
    window_lo: u64,
    /// One past the highest fault-Moment offset past the base V-time (ns).
    /// Decimal or `0x`-hex.
    ///
    /// **Bound this to the workload's fault-sensitive phase, NOT the deadline.**
    /// The server (task 59) applies a staged fault by *exact arrival* at its
    /// `Moment`; if the guest reaches its natural terminal with a fault still
    /// staged — i.e. the `Moment` lands *beyond* where this run ends — it fails
    /// loud with `ScheduleUnsatisfiable` and the campaign aborts. So a `Moment`
    /// must fall inside `[base, base + natural-terminal-span]` (for the campaign
    /// image that is the supervisor loop, ~10⁶ ns past the base — well under any
    /// deadline). The default is a loop-scale `1_000_000` ns; scope it to the
    /// pinned workload's loop (`CAMPAIGN_READY` → the loop's end), never to the
    /// far deadline.
    #[arg(long, default_value_t = 1_000_000, value_parser = parse_u64_flexible)]
    window_hi: u64,
    /// Kernel bzImage filename under guest/build (or guest/linux).
    #[arg(long, default_value = "bzImage")]
    kernel: String,
    /// Initramfs filename — defaults to the planted-bug campaign image.
    #[arg(long, default_value = "initramfs-campaign.cpio.gz")]
    initramfs: String,
    /// The serial marker after which the base snapshot is sealed (mid-workload,
    /// post-readiness).
    #[arg(long, default_value = "CAMPAIGN_READY")]
    ready_marker: String,
}

/// Parse a `u64` from a CLI flag as **decimal or `0x`-prefixed hex** — clap's
/// built-in `u64` parser is decimal-only, which makes a guest-physical address
/// like `0x3ff9a000` a hard error (the box milestone's `--gpa-base` is written
/// in hex). Accepts either form; underscores are permitted as digit separators.
fn parse_u64_flexible(s: &str) -> Result<u64, String> {
    let t = s.trim().replace('_', "");
    let parsed = match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => t.parse::<u64>(),
    };
    parsed.map_err(|e| format!("expected a u64 (decimal or 0x-hex), got {s:?}: {e}"))
}

/// The message every zero-budget rejection carries: what a gate over a
/// no-work run would actually be worth.
const ZERO_BUDGET: &str = "must be greater than 0 — a zero budget runs no workload, and a gate \
                           over a run that did nothing can only pass vacuously";

/// Parse a **budget** flag (task 103 finding 1a): a [`parse_u64_flexible`]
/// value that must be positive. Zero is refused with a loud clap usage error,
/// at parse time — before a box lease is spent, and long before a determinism
/// gate can compare two runs that each did nothing (`--max-branches 0` compares
/// empty state_hash sequences; `--deadline-delta 0` hashes the sealed base
/// itself, twice).
///
/// This is the first of the two ends the finding asks for; the second is
/// [`conductor::gamecampaign::determinism_verdict`]'s vacuity guard, which
/// catches a hollow run that reaches the gate by some route this parser never
/// saw.
fn parse_positive_u64(s: &str) -> Result<u64, String> {
    match parse_u64_flexible(s)? {
        0 => Err(format!("{s:?} {ZERO_BUDGET}")),
        n => Ok(n),
    }
}

/// [`parse_positive_u64`] for the `usize`-typed budgets (`--repeat`).
fn parse_positive_usize(s: &str) -> Result<usize, String> {
    let n = parse_positive_u64(s)?;
    usize::try_from(n).map_err(|_| format!("{s:?} does not fit in a usize on this host"))
}

/// Distinct, non-boot branch seeds (a multiplicative hash folded into a base) —
/// the same shape `live_branching_demo.rs` uses.
fn seeds(n: usize) -> Vec<u64> {
    (0..n)
        .map(|k| 0x0028_C0FF_EE5E_EDC0u64 ^ 0x9E37_79B9_7F4A_7C15u64.wrapping_mul(k as u64 + 1))
        .collect()
}

/// Reject too few seeds up front, before the sweep runs, so a valid-looking
/// run cannot print `GATES PASS` below the required bar. `min` is the mode's
/// floor: the mock demo uses `2` (the divergence gate's minimum — fewer can
/// never diverge), the **box milestone uses `8`** (the task-58 gate is
/// `N >= 8`, so a `--seeds 2..7` box run that happened to produce ≥ 2 distinct
/// hashes must NOT be able to report a milestone PASS). Returns `true` if the
/// count meets `min`.
fn seeds_ok(n: usize, min: usize) -> bool {
    if n < min {
        eprintln!(
            "[conductor] --seeds must be >= {min} here: got {n}. The divergence gate needs at \
             least 2 distinct futures, and the box milestone gate is N >= 8."
        );
        return false;
    }
    true
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.mode {
        Mode::Mock(args) => run_mock(args),
        Mode::Box(args) => run_box(args),
        Mode::Campaign(CampaignMode::Mock(args)) => run_campaign_mock(args),
        Mode::Campaign(CampaignMode::Box(args)) => run_campaign_box(args),
        Mode::Materialize(args) => run_mock_materialize(args),
        Mode::BenchCampaign(args) => run_benchcampaign_box(args),
        Mode::Game(GameMode::Mock(args)) => run_game_mock(args),
        Mode::Game(GameMode::Box(args)) => run_game_box(args),
    }
}

/// Task 69 M2 box benchmark-campaign (GO/NO-GO #2). Linux-only; refuses loudly off
/// Linux + patched KVM.
#[cfg(target_os = "linux")]
fn run_benchcampaign_box(args: BenchBoxArgs) -> ExitCode {
    boxrun::run_bench_campaign_box(args)
}

#[cfg(not(target_os = "linux"))]
fn run_benchcampaign_box(_args: BenchBoxArgs) -> ExitCode {
    eprintln!(
        "[conductor] benchcampaign box needs Linux + patched KVM + a built planted-bug image + \
         the det-cfl-v1 host (docs/BOX-PINNING.md). This is not a Linux host."
    );
    ExitCode::FAILURE
}

/// Parse the game-campaign configuration name. `signal` parses (so the CLI can
/// surface the loud refusal from the campaign itself); unknown names fail here.
fn parse_game_config(s: &str) -> Result<benchmark::ExplorationConfig, String> {
    match s {
        "pure-random" => Ok(benchmark::ExplorationConfig::PureRandom),
        "selector-v1" => Ok(benchmark::ExplorationConfig::SelectorV1),
        "signal" => Ok(benchmark::ExplorationConfig::Signal),
        other => Err(format!(
            "unknown game configuration {other:?} (expected pure-random | selector-v1 | signal)"
        )),
    }
}

/// The manifest sidecar of a logs file: `smb-logs.json` →
/// `smb-logs.manifest.json`.
fn game_manifest_path(logs: &Path) -> PathBuf {
    logs.with_extension("manifest.json")
}

/// Summarize a finished game campaign and (optionally) append its log to the
/// JSON array at `--logs-out` — the input the offline `exploration-report`
/// renders. Printing is a summary; the log is the artifact.
///
/// When logs are written, the matching [`GameManifest`] is emitted beside
/// them (`<logs>.manifest.json`, round-3 P2): the report binary needs both,
/// and emitting the pair from the same run means its inputs can never drift.
/// Across appended runs the manifest must MATCH the existing sidecar — a
/// budget or ROM drift between appends is a loud error, never a silently
/// mixed measurement. (The manifest's input-shaping fields are the
/// play-agent defaults, which `game-init.sh` deliberately does not override;
/// changing the shaping means changing both in one place,
/// `GameManifest::smb`.)
fn finish_game(
    log: &benchmark::ExplorationLog,
    out: Option<&PathBuf>,
    manifest: &benchmark::GameManifest,
) -> ExitCode {
    let budget = log.events.len() as u64;
    let distinct = log.distinct_cells_at(budget);
    let depth = log.depth_at(budget);
    println!(
        "[conductor] game campaign done: config={:?} seed={:#x} branches={} distinct_cells={} \
         max_depth={}",
        log.config, log.seed, budget, distinct, depth
    );
    if let Some(path) = out {
        // The manifest consistency check comes FIRST, so a drifting run can
        // never contaminate the log array before failing.
        let mpath = game_manifest_path(path);
        match std::fs::read_to_string(&mpath) {
            Ok(raw) => match serde_json::from_str::<benchmark::GameManifest>(&raw) {
                Ok(existing) if existing == *manifest => {}
                Ok(existing) => {
                    eprintln!(
                        "[conductor] manifest drift: {} records budget={} deadline={:?} rom={:?}, \
                         this run is budget={} deadline={:?} rom={:?} — one logs file measures \
                         ONE manifest",
                        mpath.display(),
                        existing.branch_budget,
                        existing.deadline_delta,
                        existing.rom_sha256,
                        manifest.branch_budget,
                        manifest.deadline_delta,
                        manifest.rom_sha256,
                    );
                    return ExitCode::FAILURE;
                }
                Err(e) => {
                    eprintln!(
                        "[conductor] {} exists but is not a GameManifest: {e}",
                        mpath.display()
                    );
                    return ExitCode::FAILURE;
                }
            },
            Err(_) => {
                let json = match serde_json::to_string_pretty(manifest) {
                    Ok(j) => j,
                    Err(e) => {
                        eprintln!("[conductor] cannot serialize the manifest: {e}");
                        return ExitCode::FAILURE;
                    }
                };
                if let Err(e) = std::fs::write(&mpath, json) {
                    eprintln!("[conductor] cannot write {}: {e}", mpath.display());
                    return ExitCode::FAILURE;
                }
                println!("[conductor] wrote the manifest to {}", mpath.display());
            }
        }

        let mut logs: Vec<benchmark::ExplorationLog> = match std::fs::read_to_string(path) {
            Ok(raw) => match serde_json::from_str(&raw) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!(
                        "[conductor] {} exists but is not a log array: {e}",
                        path.display()
                    );
                    return ExitCode::FAILURE;
                }
            },
            Err(_) => Vec::new(),
        };
        logs.push(log.clone());
        let json = match serde_json::to_string_pretty(&logs) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("[conductor] cannot serialize logs: {e}");
                return ExitCode::FAILURE;
            }
        };
        if let Err(e) = std::fs::write(path, json) {
            eprintln!("[conductor] cannot write {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
        println!("[conductor] appended the log to {}", path.display());
    }
    ExitCode::SUCCESS
}

/// The portable game campaign: the SMB-shaped toy machine through the real
/// wire-decode → LinkSensor → cell-key path (no /dev/kvm, no ROM).
fn run_game_mock(args: GameArgs) -> ExitCode {
    let config = match parse_game_config(&args.config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[conductor] {e}");
            return ExitCode::FAILURE;
        }
    };
    let cfg = GameCampaignConfig {
        campaign_seed: args.campaign_seed,
        max_branches: args.max_branches,
        explore_period: args.explore_period,
        rom_sha256: args.rom_sha256.clone(),
        trace_dir: args.trace_out.clone(),
        ..GameCampaignConfig::smoke(args.campaign_seed)
    };
    let manifest = benchmark::GameManifest::smb(
        args.rom_sha256.clone(),
        args.max_branches,
        cfg.deadline_delta,
    );
    let mut machine = GameToyMachine::new();
    match run_game_campaign(&mut machine, &SpecEnvCodec, &cfg, config) {
        Ok(outcome) => {
            // The same vacuity guard the box gate applies (task 103 finding
            // 1b): a campaign that did no work must not be appended to a logs
            // artifact either — the offline report would score a run that never
            // happened.
            if let Some(vacuity) = outcome.vacuity() {
                eprintln!(
                    "[conductor] game mock VACUOUS RUN — refusing to report it: {vacuity}\n\
                     [conductor] evidence: {:?}",
                    outcome.work
                );
                return ExitCode::FAILURE;
            }
            print_game_artifacts(&outcome);
            finish_game(&outcome.log, args.logs_out.as_ref(), &manifest)
        }
        Err(e) => {
            eprintln!("[conductor] game mock campaign failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Print the campaign's film/re-key artifacts: the retained deep reproducer
/// and the billboard window from the setup prefix (round-8 P1 — film's
/// inputs, from campaign output alone).
fn print_game_artifacts(outcome: &conductor::gamecampaign::GameCampaignOutcome) {
    if let Some(deep) = &outcome.deep {
        match &deep.trace_id {
            Some(id) => println!(
                "[conductor] deep reproducer: branch {} depth {} -> trace {id}",
                deep.branch, deep.depth
            ),
            None => println!(
                "[conductor] deep branch: {} depth {} (no --trace-out; NOT retained)",
                deep.branch, deep.depth
            ),
        }
    }
    if let Some((gpa, len)) = outcome.billboard {
        println!("[conductor] billboard window: gpa={gpa:#x} len={len}");
    }
}

/// The box game campaign (task 86 M0). Linux-only; refuses loudly elsewhere.
#[cfg(target_os = "linux")]
fn run_game_box(args: GameBoxArgs) -> ExitCode {
    boxrun::run_game(args)
}

#[cfg(not(target_os = "linux"))]
fn run_game_box(_args: GameBoxArgs) -> ExitCode {
    eprintln!(
        "[conductor] game box mode needs Linux + patched KVM + the built game image (make -C \
         guest/linux game-image, HARMONY_SMB_ROM set) — see docs/BOX-PINNING.md. This is not a \
         Linux host."
    );
    ExitCode::FAILURE
}

/// The portable campaign (task 60, gate 2): the seed-driven search over the toy
/// planted-bug machine, the emit-and-verify N/N step, and the nominal control —
/// the identical [`run_campaign`] loop the box milestone drives.
fn run_campaign_mock(args: CampaignArgs) -> ExitCode {
    let cfg = CampaignConfig {
        max_branches: args.max_branches,
        // Floor at the spec bar (25/25): the flag can raise it, never lower it.
        replay_n: args.replay_n.max(REPLAY_BAR),
        campaign_seed: args
            .campaign_seed
            .unwrap_or(CampaignConfig::toy().campaign_seed),
        ..CampaignConfig::toy()
    };
    let mut machine = ToyPlantedMachine::new(Trigger::toy());
    println!(
        "[conductor] campaign mock: seed-driven search over a toy planted bug \
         (budget {} branches, verify {}×)\n",
        cfg.max_branches, cfg.replay_n
    );
    match run_campaign(&mut machine, &SpecEnvCodec, &cfg) {
        Ok(report) => finish_campaign("mock", &report, cfg.replay_n),
        Err(e) => {
            eprintln!("[conductor] campaign mock failed (backend): {e}");
            ExitCode::FAILURE
        }
    }
}

/// The box campaign milestone (task 60, gate 1). Linux-only; refuses to run off
/// Linux + patched KVM loudly.
#[cfg(target_os = "linux")]
fn run_campaign_box(args: CampaignBoxArgs) -> ExitCode {
    boxrun::run_campaign(args)
}

#[cfg(not(target_os = "linux"))]
fn run_campaign_box(_args: CampaignBoxArgs) -> ExitCode {
    eprintln!(
        "[conductor] campaign box mode needs Linux + patched KVM + the built Postgres-campaign \
         image + the det-cfl-v1 host (see docs/BOX-PINNING.md). This is not a Linux host."
    );
    ExitCode::FAILURE
}

/// Print a campaign run table and set the exit code from the task-60 gates.
fn finish_campaign(mode: &str, report: &CampaignReport, n: usize) -> ExitCode {
    print!("{}", render_campaign_table(report, n));
    let failures = verify_campaign(report, n);
    if failures.is_empty() {
        println!(
            "\n[conductor] campaign {mode} GATES PASS: planted bug found, reproduced {n}/{n}, \
             nominal control clean."
        );
        ExitCode::SUCCESS
    } else {
        eprintln!("\n[conductor] campaign {mode} GATES FAILED:");
        for f in &failures {
            eprintln!("  - {f}");
        }
        ExitCode::FAILURE
    }
}

/// The portable task-68 demo: the chain protocol (build a seal chain, then
/// the three materialization gates) against the scripted mock guest, over the
/// real wire path.
fn run_mock_materialize(args: MatArgs) -> ExitCode {
    use conductor::materialize::{MaterializeConfig, render_materialize_table, verify_materialize};
    if args.hops < 3 {
        eprintln!("[conductor] --hops must be >= 3 (gate (b) needs a retained grandparent)");
        return ExitCode::FAILURE;
    }
    // Script capacity: the longest single replay is the from-genesis worst
    // case + the reproducer tail; each mock intercept is 100 ns of V-time.
    let intercepts = ((args.hops as u64 + 2) * (args.hop_delta + 200) + args.tail_delta) / 100 + 8;
    let mut server = match conductor::mock::server(conductor::mock::chain_fork_script(
        intercepts as usize,
        false,
    )) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[conductor] failed to compose the mock server: {e}");
            return ExitCode::FAILURE;
        }
    };
    let cfg = MaterializeConfig {
        seed: args.seed,
        hops: args.hops,
        hop_delta: args.hop_delta,
        tail_delta: args.tail_delta,
        snapshot_retry_step: 100,
        snapshot_max_attempts: 64,
    };
    let initial = EnvSpec::Seeded {
        seed: conductor::mock::BOOT_SEED,
        policy: FaultPolicy::none(),
    };
    println!(
        "[conductor] materialize (mock): {}-hop chain, hop_delta {}, tail {}\n",
        cfg.hops, cfg.hop_delta, cfg.tail_delta
    );
    let (served, report) = run_session(&mut server, move |stream| {
        conductor::materialize_client(stream, initial, cfg)
    });
    let report = match report {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[conductor] materialize: the chain protocol failed: {e}");
            if let Err(se) = served {
                eprintln!("[conductor] server session ended with: {se}");
            }
            return ExitCode::FAILURE;
        }
    };
    if let Err(se) = served {
        eprintln!("[conductor] server session ended with a fatal error: {se}");
        return ExitCode::FAILURE;
    }
    print!("{}", render_materialize_table(&report));
    let failures = verify_materialize(&report, None);
    if failures.is_empty() {
        println!(
            "\n[conductor] materialize GATES PASS: parent-rooted hot path, bit-identical \
             eviction round-trip (folded + worst case), composed reproducer replays."
        );
        ExitCode::SUCCESS
    } else {
        eprintln!("\n[conductor] materialize GATES FAILED:");
        for f in &failures {
            eprintln!("  - {f}");
        }
        ExitCode::FAILURE
    }
}

/// Parse the `--retain` flag, reporting an unknown value loudly.
fn parse_retain(s: &str) -> Option<RetentionPolicy> {
    match RetentionPolicy::parse(s) {
        Some(p) => Some(p),
        None => {
            eprintln!("[conductor] unknown --retain {s:?}: use all | interesting | env-only");
            None
        }
    }
}

/// Print a recording run table and set the exit code from the task-65 gates: the
/// pure report checks plus the post-campaign store-reload check (kept out of the
/// recording loop, which stays write-only to the store).
fn finish_recording(
    mode: &str,
    report: &RecordReport,
    min_distinct: usize,
    store: &TraceStore,
    dir: &Path,
) -> ExitCode {
    print!("{}", render_record_table(report));
    let mut failures = verify_record(report, min_distinct);
    failures.extend(verify_store_reload(store, report));
    if failures.is_empty() {
        println!(
            "\n[conductor] {mode} RECORDING GATES PASS: per-seed determinism (state_hash + \
             byte-identical journal), >= {min_distinct} distinct guest state_hashes, non-empty \
             monotone records, lossless reload. Traces under {}",
            dir.display()
        );
        ExitCode::SUCCESS
    } else {
        eprintln!("\n[conductor] {mode} RECORDING GATES FAILED:");
        for f in &failures {
            eprintln!("  - {f}");
        }
        ExitCode::FAILURE
    }
}

/// The portable recording demo: the mock guest's console is scraped into a
/// `RunTrace` per run and persisted, then the task-65 gates are checked.
fn run_mock_recording(args: &SweepArgs, dir: PathBuf, retain: RetentionPolicy) -> ExitCode {
    let store = match TraceStore::open(&dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[conductor] cannot open trace store {}: {e}", dir.display());
            return ExitCode::FAILURE;
        }
    };
    let mut server = match conductor::mock::server(conductor::mock::recording_fork_script()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[conductor] failed to compose the mock recording server: {e}");
            return ExitCode::FAILURE;
        }
    };
    let cfg = RecordConfig {
        sweep: SweepConfig {
            seeds: seeds(args.seeds),
            runs_per_seed: args.runs.max(2),
            deadline_delta: None, // run each fork to its clean Hlt terminal
            ..SweepConfig::default()
        },
        retain,
        stream: StreamId(0),
    };
    println!(
        "[conductor] mock recording: {} seeds x {} runs, retain={}, into {}\n",
        cfg.sweep.seeds.len(),
        cfg.sweep.runs_per_seed,
        retain.as_str(),
        dir.display()
    );
    match run_recording(&mut server, &store, &cfg) {
        Ok(report) => finish_recording("mock", &report, 2, &store, &dir),
        Err(e) => {
            eprintln!("[conductor] mock recording failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The portable demo: scripted mock guest, sweep, table, verdicts.
fn run_mock(args: SweepArgs) -> ExitCode {
    if !seeds_ok(args.seeds, 2) {
        return ExitCode::FAILURE;
    }
    if let Some(dir) = args.record.clone() {
        let Some(retain) = parse_retain(&args.retain) else {
            return ExitCode::FAILURE;
        };
        return run_mock_recording(&args, dir, retain);
    }
    let cfg = SweepConfig {
        seeds: seeds(args.seeds),
        runs_per_seed: args.runs.max(2),
        deadline_delta: None, // run each fork to its clean Hlt terminal
        ..SweepConfig::default()
    };
    let mut server = match conductor::mock::server(conductor::mock::default_fork_script()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[conductor] failed to compose the mock server: {e}");
            return ExitCode::FAILURE;
        }
    };
    // The mock live VM boots under BOOT_SEED with the never-fault policy.
    let initial = EnvSpec::Seeded {
        seed: conductor::mock::BOOT_SEED,
        policy: FaultPolicy::none(),
    };
    println!(
        "[conductor] mock mode: {} seeds x {} runs over a scripted MockBackend guest\n",
        cfg.seeds.len(),
        cfg.runs_per_seed
    );
    let (served, client) = run_session(&mut server, move |stream| {
        sweep_client(stream, initial, cfg)
    });
    finish("mock", served, client)
}

/// The box demo. Compiled everywhere (so `--help` and the crate build are
/// portable), but refuses to run off Linux + patched KVM — loudly, never a
/// vacuous pass.
#[cfg(target_os = "linux")]
fn run_box(args: BoxArgs) -> ExitCode {
    boxrun::run(args)
}

#[cfg(not(target_os = "linux"))]
fn run_box(_args: BoxArgs) -> ExitCode {
    eprintln!(
        "[conductor] box mode needs Linux + patched KVM + the built Postgres image + the \
         det-cfl-v1 host (see docs/BOX-PINNING.md). This is not a Linux host."
    );
    ExitCode::FAILURE
}

/// Print the outcome of a sweep session and set the exit code from the gates.
fn finish(
    mode: &str,
    served: Result<(), vmm_core::control::ServeError>,
    client: Result<conductor::SweepReport, explorer::MachineError>,
) -> ExitCode {
    let report = match client {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[conductor] {mode}: the sweep failed (transport/backend): {e}");
            // A client-side failure usually means the server tore down too.
            if let Err(se) = served {
                eprintln!("[conductor] {mode}: server session ended with: {se}");
            }
            return ExitCode::FAILURE;
        }
    };
    if let Err(se) = served {
        eprintln!("[conductor] {mode}: server session ended with a fatal error: {se}");
        return ExitCode::FAILURE;
    }
    print!("{}", render_table(&report));
    let failures = verify(&report, 2);
    if failures.is_empty() {
        println!(
            "\n[conductor] {mode} GATES PASS: per-seed reproducible, >= 2 distinct futures, \
             replay == capture."
        );
        ExitCode::SUCCESS
    } else {
        eprintln!("\n[conductor] {mode} GATES FAILED:");
        for f in &failures {
            eprintln!("  - {f}");
        }
        ExitCode::FAILURE
    }
}

/// The box composition root (`src/boxrun.rs`, Linux-only). Split into its own
/// file (issue #69) so the coverage job can exclude it by name — every
/// function in it needs a real `/dev/kvm` + patched KVM + a built guest
/// image, which no portable test (or the coverage job's own runner) can
/// provide.
#[cfg(target_os = "linux")]
mod boxrun;

#[cfg(test)]
mod tests {
    use super::*;
    use explorer::{EnvCodec, Oracle};

    // --- the game logs+manifest pair (round-3 P2) ---------------------------

    fn game_args(
        config: &str,
        max_branches: u64,
        seed: u64,
        logs_out: PathBuf,
        rom: &str,
    ) -> GameArgs {
        GameArgs {
            config: config.to_string(),
            max_branches,
            campaign_seed: seed,
            explore_period: 4,
            logs_out: Some(logs_out),
            rom_sha256: Some(rom.to_string()),
            trace_out: None,
        }
    }

    /// `--logs-out` must emit the matching manifest from the same run, and the
    /// emitted pair must round-trip through the report path the
    /// `exploration-report` binary wraps (parse both files → compute →
    /// render) — so the report's inputs can never drift from the logs.
    #[test]
    fn game_logs_out_emits_a_manifest_that_round_trips_the_report() {
        let dir = tempfile::tempdir().unwrap();
        let logs_path = dir.path().join("smb-logs.json");
        // ≥ MIN_SEEDS seeds per available config, all appended to one file.
        for seed in 0..benchmark::report::MIN_SEEDS {
            for config in ["pure-random", "selector-v1"] {
                let args = game_args(config, 8, seed, logs_path.clone(), "cafe");
                assert_eq!(run_game_mock(args), ExitCode::SUCCESS);
            }
        }

        // The emitted pair, read back exactly as the report binary reads it.
        let logs: Vec<benchmark::ExplorationLog> =
            serde_json::from_str(&std::fs::read_to_string(&logs_path).unwrap()).unwrap();
        assert_eq!(logs.len(), 2 * benchmark::report::MIN_SEEDS as usize);
        let mpath = game_manifest_path(&logs_path);
        let manifest: benchmark::GameManifest =
            serde_json::from_str(&std::fs::read_to_string(&mpath).unwrap()).unwrap();
        assert_eq!(manifest.branch_budget, 8);
        assert_eq!(manifest.rom_sha256.as_deref(), Some("cafe"));

        // The pair computes + renders (the report bin's exact library path).
        // Signal is M1's, so the verdict is the missing-configuration
        // Incomplete — but every validation (workload, ROM, dense budget,
        // seed floor) passed, which is the drift-proof this test pins.
        let report = benchmark::ExplorationReport::compute(&manifest, &logs, (1, 1000)).unwrap();
        match &report.verdict {
            benchmark::Verdict::Incomplete { reason } => assert!(reason.contains("signal")),
            other => panic!("expected the missing-signal Incomplete, got {other:?}"),
        }
        assert!(report.render_markdown().contains("cafe"));

        // Drift protection: appending a run with a different budget (or ROM)
        // to the same logs file fails loudly, before touching the log array.
        let drifted = game_args("pure-random", 9, 999, logs_path.clone(), "cafe");
        assert_eq!(run_game_mock(drifted), ExitCode::FAILURE);
        let drifted_rom = game_args("pure-random", 8, 999, logs_path.clone(), "beef");
        assert_eq!(run_game_mock(drifted_rom), ExitCode::FAILURE);
        let after: Vec<benchmark::ExplorationLog> =
            serde_json::from_str(&std::fs::read_to_string(&logs_path).unwrap()).unwrap();
        assert_eq!(
            after.len(),
            2 * benchmark::report::MIN_SEEDS as usize,
            "a drifting run must not contaminate the log array"
        );
    }

    // --- degenerate budgets (task 103 finding 1a) ---------------------------

    /// The vacuous-pass fixtures from PR #93's round-9 pass, driven through the
    /// REAL CLI grammar: each is an invocation the box driver would have
    /// accepted and reported `DETERMINISM PASS` for, having done nothing. Every
    /// one must now die at parse time — before a box lease is spent — with a
    /// usage error, not an exit-0 gate.
    #[test]
    fn degenerate_budgets_are_refused_by_the_cli() {
        let refused = |args: &[&str]| {
            let err = Cli::try_parse_from(args)
                .err()
                .unwrap_or_else(|| panic!("{args:?} must be REFUSED, not accepted"));
            assert_eq!(
                err.kind(),
                clap::error::ErrorKind::ValueValidation,
                "{args:?} must fail as a bad VALUE (a usage error), got {:?}",
                err.kind()
            );
            assert!(
                err.to_string().contains("greater than 0"),
                "the error must say why: {err}"
            );
        };

        // The two the finding names, on the path it names (the box game gate).
        refused(&["conductor", "game", "box", "--max-branches", "0"]);
        refused(&["conductor", "game", "box", "--deadline-delta", "0"]);
        // …and the rest of that path's budget knobs, whose zero is just as hollow.
        refused(&["conductor", "game", "box", "--setup-deadline-delta", "0"]);
        refused(&["conductor", "game", "box", "--repeat", "0"]);
        refused(&["conductor", "game", "box", "--explore-period", "0"]);
        // The same budgets on every other campaign path (a zero budget is never
        // a valid campaign, whichever gate is downstream of it).
        refused(&["conductor", "game", "mock", "--max-branches", "0"]);
        refused(&["conductor", "box", "--deadline-delta", "0"]);
        refused(&["conductor", "campaign", "box", "--max-branches", "0"]);
        refused(&["conductor", "campaign", "box", "--deadline-delta", "0"]);
        refused(&["conductor", "campaign", "mock", "--max-branches", "0"]);
        refused(&[
            "conductor",
            "bench-campaign",
            "--bug",
            "1",
            "--config",
            "signal",
            "--seed",
            "1",
            "--out",
            "/dev/null",
            "--max-branches",
            "0",
        ]);
        // Hex is a budget too: `0x0` is the same zero.
        refused(&["conductor", "game", "box", "--deadline-delta", "0x0"]);

        // The positive values these flags actually take still parse, in both
        // radixes — the guard rejects zero, not the flag.
        let cli = Cli::try_parse_from([
            "conductor",
            "game",
            "box",
            "--max-branches",
            "32",
            "--deadline-delta",
            "0x77359400",
            "--repeat",
            "25",
        ])
        .expect("a real box gate invocation still parses");
        match cli.mode {
            Mode::Game(GameMode::Box(args)) => {
                assert_eq!(args.game.max_branches, 32);
                assert_eq!(args.deadline_delta, 2_000_000_000);
                assert_eq!(args.repeat, 25);
            }
            _ => panic!("expected the game box mode"),
        }
    }

    #[test]
    fn parse_positive_u64_rejects_zero_in_either_radix() {
        assert_eq!(parse_positive_u64("1"), Ok(1));
        assert_eq!(parse_positive_u64("0x10"), Ok(16));
        assert!(parse_positive_u64("0").is_err());
        assert!(parse_positive_u64("0x0").is_err());
        assert!(parse_positive_u64("0_0").is_err());
        assert!(parse_positive_u64("garbage").is_err());
        assert_eq!(parse_positive_usize("25"), Ok(25));
        assert!(parse_positive_usize("0").is_err());
    }

    // --- parse_u64_flexible -------------------------------------------------

    #[test]
    fn parse_u64_flexible_accepts_decimal_hex_and_underscored() {
        assert_eq!(parse_u64_flexible("1234"), Ok(1234));
        assert_eq!(parse_u64_flexible("0x3ff9a000"), Ok(0x3ff9a000));
        assert_eq!(parse_u64_flexible("0X3FF"), Ok(0x3ff));
        assert_eq!(parse_u64_flexible("1_000_000"), Ok(1_000_000));
        assert_eq!(parse_u64_flexible("0x1_000"), Ok(0x1000));
        assert_eq!(
            parse_u64_flexible("  42  "),
            Ok(42),
            "trims surrounding whitespace"
        );
    }

    #[test]
    fn parse_u64_flexible_rejects_garbage() {
        assert!(parse_u64_flexible("not-a-number").is_err());
        assert!(parse_u64_flexible("0xzz").is_err());
        assert!(parse_u64_flexible("").is_err());
    }

    // --- seeds / seeds_ok ----------------------------------------------------

    #[test]
    fn seeds_are_distinct_and_deterministic() {
        let a = seeds(8);
        let b = seeds(8);
        assert_eq!(a, b, "the same n always mints the same seeds");
        assert_eq!(a.len(), 8);
        let distinct: std::collections::BTreeSet<u64> = a.iter().copied().collect();
        assert_eq!(distinct.len(), 8, "every seed is distinct");
    }

    #[test]
    fn seeds_ok_enforces_the_floor() {
        assert!(!seeds_ok(1, 2), "below the floor must fail");
        assert!(seeds_ok(2, 2), "exactly the floor must pass");
        assert!(seeds_ok(8, 2), "above the floor must pass");
        assert!(
            !seeds_ok(7, 8),
            "the box milestone's stricter floor (8) rejects 7"
        );
    }

    // --- parse_retain --------------------------------------------------------

    #[test]
    fn parse_retain_parses_known_values_and_rejects_unknown() {
        assert_eq!(parse_retain("all"), Some(RetentionPolicy::All));
        assert_eq!(
            parse_retain("interesting"),
            Some(RetentionPolicy::Interesting)
        );
        assert_eq!(parse_retain("env-only"), Some(RetentionPolicy::EnvOnly));
        assert_eq!(parse_retain("bogus"), None);
    }

    // --- run_mock / run_campaign_mock / run_mock_materialize ----------------

    fn sweep_args(seeds: usize, record: Option<PathBuf>, retain: &str) -> SweepArgs {
        SweepArgs {
            seeds,
            runs: 2,
            record,
            retain: retain.to_string(),
        }
    }

    #[test]
    fn run_mock_reports_gates_pass_for_a_valid_sweep() {
        assert_eq!(
            run_mock(sweep_args(8, None, "interesting")),
            ExitCode::SUCCESS
        );
    }

    #[test]
    fn run_mock_rejects_too_few_seeds_before_running_anything() {
        assert_eq!(
            run_mock(sweep_args(1, None, "interesting")),
            ExitCode::FAILURE
        );
    }

    #[test]
    fn run_mock_with_record_persists_a_trace_store_and_passes() {
        let dir = tempfile::tempdir().unwrap();
        let code = run_mock(sweep_args(8, Some(dir.path().to_path_buf()), "all"));
        assert_eq!(code, ExitCode::SUCCESS);
        assert!(
            std::fs::read_dir(dir.path()).unwrap().next().is_some(),
            "the recording session must have written into the store dir"
        );
    }

    #[test]
    fn run_mock_rejects_an_unknown_retain_value() {
        let dir = tempfile::tempdir().unwrap();
        let code = run_mock(sweep_args(8, Some(dir.path().to_path_buf()), "bogus"));
        assert_eq!(code, ExitCode::FAILURE);
    }

    #[test]
    fn run_campaign_mock_finds_the_planted_bug_and_reports_gates_pass() {
        let args = CampaignArgs {
            max_branches: CampaignConfig::toy().max_branches,
            replay_n: REPLAY_BAR,
            campaign_seed: None,
        };
        assert_eq!(run_campaign_mock(args), ExitCode::SUCCESS);
    }

    #[test]
    fn run_mock_materialize_rejects_too_few_hops() {
        let args = MatArgs {
            hops: 2,
            hop_delta: 250,
            tail_delta: 250,
            seed: 0x1234_5678_9ABC_DEF0,
        };
        assert_eq!(run_mock_materialize(args), ExitCode::FAILURE);
    }

    #[test]
    fn run_mock_materialize_reports_gates_pass_for_a_valid_chain() {
        let args = MatArgs {
            hops: 3,
            hop_delta: 250,
            tail_delta: 250,
            seed: 0x1234_5678_9ABC_DEF0,
        };
        assert_eq!(run_mock_materialize(args), ExitCode::SUCCESS);
    }

    // --- finish / finish_campaign / finish_recording -------------------------

    fn sweep_report_of(hash_a: [u8; 32], hash_b: [u8; 32]) -> conductor::SweepReport {
        use conductor::{RunRow, SeedRow, SweepReport};
        let run = |hash: [u8; 32]| RunRow {
            stop: explorer::StopReason::Quiescent {
                vtime: explorer::VTime(10),
            },
            hash,
        };
        SweepReport {
            snapshot_vtime: 0,
            snapshot_attempts: 1,
            base_hash: [0; 32],
            rows: vec![
                SeedRow {
                    seed: 1,
                    runs: vec![run(hash_a), run(hash_a)],
                },
                SeedRow {
                    seed: 2,
                    runs: vec![run(hash_b), run(hash_b)],
                },
            ],
            replay_hash: [0; 32],
        }
    }

    #[test]
    fn finish_reports_pass_when_served_ok_and_gates_pass() {
        let report = sweep_report_of([1; 32], [2; 32]); // 2 distinct hashes
        assert_eq!(finish("test", Ok(()), Ok(report)), ExitCode::SUCCESS);
    }

    #[test]
    fn finish_reports_failure_when_the_gates_fail() {
        let report = sweep_report_of([1; 32], [1; 32]); // no divergence
        assert_eq!(finish("test", Ok(()), Ok(report)), ExitCode::FAILURE);
    }

    #[test]
    fn finish_reports_failure_when_the_client_errored() {
        let served: Result<(), vmm_core::control::ServeError> = Ok(());
        let client: Result<conductor::SweepReport, explorer::MachineError> =
            Err(explorer::MachineError::Transport("boom".into()));
        assert_eq!(finish("test", served, client), ExitCode::FAILURE);
    }

    #[test]
    fn finish_reports_failure_when_the_server_session_errored_even_if_the_client_ok() {
        let report = sweep_report_of([1; 32], [2; 32]);
        let served: Result<(), vmm_core::control::ServeError> =
            Err(vmm_core::control::ServeError::Poisoned);
        assert_eq!(finish("test", served, Ok(report)), ExitCode::FAILURE);
    }

    fn campaign_report_of(found: bool, nominal_is_bug: bool) -> CampaignReport {
        use conductor::campaign::{CRASH_KIND_SHUTDOWN, FoundBug, NominalRow};
        let stop = explorer::StopReason::Crash {
            vtime: explorer::VTime(5),
            info: vec![CRASH_KIND_SHUTDOWN],
        };
        let bug = explorer::TerminalOracle::new()
            .judge(&explorer::RunTrace {
                terminal: stop.clone(),
                env: explorer::SpecEnvCodec.seeded(1),
                coverage: None,
                events: Vec::new(),
                records: Vec::new(),
            })
            .unwrap();
        CampaignReport {
            base_vtime: 0,
            snapshot_attempts: 1,
            base_hash: [0; 32],
            branches_explored: 1,
            found: found.then(|| FoundBug {
                branch_index: 0,
                seed: 1,
                env: explorer::SpecEnvCodec.seeded(1),
                stop: stop.clone(),
                hash: [7; 32],
                bug,
            }),
            replays: if found {
                (0..REPLAY_BAR)
                    .map(|_| conductor::RunRow {
                        stop: stop.clone(),
                        hash: [7; 32],
                    })
                    .collect()
            } else {
                Vec::new()
            },
            nominal: NominalRow {
                stop: explorer::StopReason::Quiescent {
                    vtime: explorer::VTime(1),
                },
                hash: [0; 32],
                is_bug: nominal_is_bug,
            },
            timing: std::collections::BTreeMap::new(),
            wall_secs: 0,
            branches_per_hour_x10: 0,
        }
    }

    #[test]
    fn finish_campaign_reports_pass_when_the_gates_pass() {
        let report = campaign_report_of(true, false);
        assert_eq!(
            finish_campaign("test", &report, REPLAY_BAR),
            ExitCode::SUCCESS
        );
    }

    #[test]
    fn finish_campaign_reports_failure_when_no_bug_was_found() {
        let report = campaign_report_of(false, false);
        assert_eq!(
            finish_campaign("test", &report, REPLAY_BAR),
            ExitCode::FAILURE
        );
    }

    #[test]
    fn finish_recording_reports_pass_then_failure_on_a_broken_gate() {
        use conductor::record::run_recording;
        let dir = tempfile::tempdir().unwrap();
        let store = TraceStore::open(dir.path()).unwrap();
        let mut server = conductor::mock::server(conductor::mock::recording_fork_script())
            .expect("compose mock recording server");
        let cfg = RecordConfig {
            sweep: SweepConfig {
                seeds: seeds(4),
                runs_per_seed: 2,
                deadline_delta: None,
                ..SweepConfig::default()
            },
            retain: RetentionPolicy::All,
            stream: StreamId(0),
        };
        let report = run_recording(&mut server, &store, &cfg).expect("mock recording runs");
        assert_eq!(
            finish_recording("test", &report, 2, &store, dir.path()),
            ExitCode::SUCCESS
        );

        // Break a gate behind finish_recording's back (delete a retained
        // journal) and confirm it reports failure, not a silent pass.
        let victim = report.rows[0].trace_id;
        std::fs::remove_file(dir.path().join(format!("{victim}.trace"))).unwrap();
        assert_eq!(
            finish_recording("test", &report, 2, &store, dir.path()),
            ExitCode::FAILURE
        );
    }

    // --- off-Linux stubs ------------------------------------------------------
    //
    // `run_box`/`run_campaign_box` resolve to a DIFFERENT function per platform
    // (`#[cfg(target_os = "linux")]` picks the real `boxrun`-backed one, which
    // needs `/dev/kvm` + the built guest images + a CPU-pinned host — never
    // something a portable coverage run should invoke). These two tests only
    // exist to pin the non-Linux stub's "loud refusal" contract, so they are
    // gated identically to it — on Linux they simply do not compile/run.

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn run_box_refuses_to_run_off_linux() {
        let args = BoxArgs {
            sweep: sweep_args(8, None, "interesting"),
            deadline_delta: 5_000_000_000,
            kernel: "bzImage".to_string(),
            initramfs: "initramfs-postgres.cpio.gz".to_string(),
            ready_marker: "ready".to_string(),
        };
        assert_eq!(run_box(args), ExitCode::FAILURE);
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn run_campaign_box_refuses_to_run_off_linux() {
        let args = CampaignBoxArgs {
            campaign: CampaignArgs {
                max_branches: 4096,
                replay_n: REPLAY_BAR,
                campaign_seed: None,
            },
            deadline_delta: 5_000_000_000,
            gpa_base: 0x0100_0000,
            gpa_count: 8,
            gpa_stride: 0x1000,
            window_lo: 0,
            window_hi: 1_000_000,
            kernel: "bzImage".to_string(),
            initramfs: "initramfs-campaign.cpio.gz".to_string(),
            ready_marker: "CAMPAIGN_READY".to_string(),
        };
        assert_eq!(run_campaign_box(args), ExitCode::FAILURE);
    }
}
