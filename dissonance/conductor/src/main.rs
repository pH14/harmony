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
    #[arg(long, default_value_t = 4096)]
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
    #[arg(long, default_value_t = 5_000_000_000)]
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
    /// far enough for the fault to land and the supervisor to react.
    #[arg(long, default_value_t = 5_000_000_000, value_parser = parse_u64_flexible)]
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
    }
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

/// The box composition root. Linux-only (`boot_linux_selected` + `perf_event`).
///
/// The one piece of **workload-aware policy** in the whole path lives here (the
/// server and adapter stay workload-blind): the live guest is driven to a
/// readiness marker on its serial *before* the sweep seals the base, so the
/// snapshot lands **mid-workload, post-readiness** (the gate's point) rather
/// than at boot entry. Choosing *where* to snapshot is a property of the guest;
/// the snapshot *mechanism* (the verb) is not.
#[cfg(target_os = "linux")]
mod boxrun {
    use std::io::Write;
    use std::process::ExitCode;

    // Aliased: the module's own `pub fn run_campaign` (the box entry point)
    // would otherwise collide with the imported campaign loop (E0255), and the
    // call below would silently resolve to the 1-arg local fn (E0061). This code
    // is `cfg(target_os = "linux")`, so the collision is invisible to a Mac
    // `cargo check` — the Linux-target check in the gate list catches it.
    use conductor::campaign::{CampaignConfig, run_campaign as run_campaign_loop};
    use conductor::record::{RecordConfig, run_recording};
    use conductor::{SweepConfig, run_session, sweep_client};
    use environment::{EnvSpec, FaultPolicy};
    use explorer::adapter::SocketMachine;
    use explorer::{SpecEnvCodec, StreamId};
    use runtrace::TraceStore;
    use vmm_backend::Backend;
    use vmm_core::bringup::{BackendKind, boot_linux_selected};
    use vmm_core::control::{ControlServer, VmmFactory};
    use vmm_core::vmm::{Step, Vmm};

    use super::{
        BoxArgs, CampaignBoxArgs, finish, finish_campaign, finish_recording, parse_retain, seeds,
    };

    /// 2 GiB guest RAM (matches `live_postgres.rs` / `live_branching_demo.rs`).
    const GUEST_RAM_LEN: usize = 2 << 30;
    /// The boot seed the live VM runs under (matches the branching demo).
    const BOOT_SEED: u64 = 0x0028_C0FF_EE5E_EDC0;
    /// The determinism command line (identical to the branching demo).
    const CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t,force tsc=reliable no_timer_check \
                           lpj=4000000 nokaslr nosmp maxcpus=1 nox2apic hpet=disable";
    /// A safety cap on the boot-to-marker drive (the external `timeout` is the
    /// real bound; this stops a wedged guest from looping forever).
    const MAX_BOOT_STEPS: u64 = 50_000_000_000;

    fn repo_root() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
    }

    fn artifact(name: &str) -> Option<Vec<u8>> {
        for p in [
            repo_root().join("guest/build").join(name),
            repo_root().join("guest/linux").join(name),
        ] {
            if let Ok(bytes) = std::fs::read(&p) {
                return Some(bytes);
            }
        }
        None
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
    }

    /// Drive the live guest until `marker` appears on the serial, streaming new
    /// serial bytes to stderr so a hang shows the last line reached. Returns the
    /// number of steps taken, or an error string if the guest terminated first.
    ///
    /// The marker is scanned **only over newly-emitted bytes** (with a
    /// `marker.len()-1` overlap so a match straddling the boundary is not
    /// missed), gated on the serial actually growing. Rescanning the whole
    /// ever-growing buffer every step would be `O(steps × serial_len)` — on a
    /// real Postgres boot (millions of steps, a large console) that alone can
    /// make the drive look hung.
    fn drive_to_marker(vmm: &mut Vmm<Box<dyn Backend>>, marker: &[u8]) -> Result<u64, String> {
        let stderr = std::io::stderr();
        let mut printed = vmm.serial().len();
        // Where the next marker scan starts: keep a marker-1 overlap behind
        // `printed` so a match split across two batches of new bytes is seen.
        let overlap = marker.len().saturating_sub(1);
        let mut scan_from = printed.saturating_sub(overlap);
        let mut steps = 0u64;
        while steps < MAX_BOOT_STEPS {
            match vmm.step() {
                Ok(Step::Continued) => {}
                Ok(Step::Terminal(r)) => {
                    return Err(format!(
                        "guest reached a terminal ({r:?}) at step {steps} before the readiness \
                         marker appeared"
                    ));
                }
                Err(e) => return Err(format!("step error at {steps}: {e}")),
            }
            steps += 1;
            let serial = vmm.serial();
            if serial.len() > printed {
                let mut h = stderr.lock();
                let _ = h.write_all(&serial[printed..]);
                let _ = h.flush();
                printed = serial.len();
                // Only scan the fresh tail (plus the overlap) — not the whole buffer.
                if contains(&serial[scan_from..], marker) {
                    return Ok(steps);
                }
                scan_from = serial.len().saturating_sub(overlap);
            }
        }
        Err(format!("marker not seen within {MAX_BOOT_STEPS} steps"))
    }

    /// Boot the live guest on patched KVM and drive it to `ready_marker`, so the
    /// base snapshot a later sweep/campaign seals lands **mid-workload,
    /// post-readiness** (the gate's point) — the one workload-aware step; the
    /// server and adapter after it stay workload-blind. Returns the composed
    /// [`ControlServer`] ready to serve, or a failing [`ExitCode`] with a loud
    /// reason (never a vacuous success). Shared verbatim by the sweep
    /// ([`run`](run)) and the campaign ([`run_campaign`](run_campaign)) so both
    /// boot the guest identically.
    fn boot_server(
        kernel_name: &str,
        initramfs_name: &str,
        ready_marker: &str,
    ) -> Result<ControlServer<Box<dyn Backend>>, ExitCode> {
        if !std::path::Path::new("/dev/kvm").exists() {
            eprintln!(
                "[conductor] /dev/kvm absent — run on the determinism box with the LOADED patched \
                 KVM modules, CPU-pinned per docs/BOX-PINNING.md."
            );
            return Err(ExitCode::FAILURE);
        }
        // The frozen contract cannot run off the det-cfl-v1 baseline.
        let report = vmm_core::hostassert::report();
        if let Some(bad) = report.iter().find(|o| !o.pass) {
            eprintln!(
                "[conductor] host is not the det-cfl-v1 baseline (first failing assertion: {} \
                 expected {}, observed {}). Run on the box.",
                bad.key, bad.expected, bad.actual
            );
            return Err(ExitCode::FAILURE);
        }
        let (Some(kernel), Some(initramfs)) = (artifact(kernel_name), artifact(initramfs_name))
        else {
            eprintln!(
                "[conductor] guest image missing ({kernel_name} / {initramfs_name}) — build it \
                 first: `make -C guest fetch && make -C guest/linux campaign-image` (or \
                 `postgres-image`), or pass --initramfs for an image already on the box."
            );
            return Err(ExitCode::FAILURE);
        };

        let mut live = match boot_linux_selected(
            BackendKind::Patched,
            &kernel,
            &initramfs,
            GUEST_RAM_LEN,
            CMDLINE,
            BOOT_SEED,
        ) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[conductor] boot_linux_selected (patched) failed: {e}");
                return Err(ExitCode::FAILURE);
            }
        };
        println!("[conductor] box: booting the guest to the readiness marker {ready_marker:?} …");
        match drive_to_marker(&mut live, ready_marker.as_bytes()) {
            Ok(steps) => println!(
                "\n[conductor] readiness marker reached at step {steps}; the base snapshot will be \
                 sealed at the next snapshottable boundary at/after this point.\n"
            ),
            Err(e) => {
                eprintln!("\n[conductor] failed to reach the readiness marker: {e}");
                return Err(ExitCode::FAILURE);
            }
        }

        // The fork factory: fresh, equivalently-composed patched VMs whose
        // boot-loaded image the restore immediately overwrites. `live` is
        // already booted (it owns its guest RAM), so **move** the sole
        // kernel/initramfs copies into the closure rather than cloning them —
        // an initramfs is tens/hundreds of MB, and cloning would keep two
        // copies resident for the whole run.
        let factory: VmmFactory<Box<dyn Backend>> = Box::new(move || {
            boot_linux_selected(
                BackendKind::Patched,
                &kernel,
                &initramfs,
                GUEST_RAM_LEN,
                CMDLINE,
                BOOT_SEED,
            )
        });
        Ok(ControlServer::new(live, factory))
    }

    /// The initial environment the box's live VM boots under (the seed/policy the
    /// adapter reports as its starting environment).
    fn boot_env() -> EnvSpec {
        EnvSpec::Seeded {
            seed: BOOT_SEED,
            policy: FaultPolicy::none(),
        }
    }

    pub fn run(args: BoxArgs) -> ExitCode {
        // The box milestone gate is N >= 8 — enforce it so a smaller box run
        // can never print a milestone PASS below the bar.
        if !super::seeds_ok(args.sweep.seeds, 8) {
            return ExitCode::FAILURE;
        }
        let mut server = match boot_server(&args.kernel, &args.initramfs, &args.ready_marker) {
            Ok(s) => s,
            Err(code) => return code,
        };

        // Postgres is interrupt-driven; the snapshot search may need many steps
        // to find a sealable boundary at/after readiness. Generous retry budget
        // (task 41 made mid-workload points snapshottable).
        let (snapshot_retry_step, snapshot_max_attempts) = (1_000_000u64, 100_000usize);

        // The task-65 box gate: record each run's RunTrace and check byte-
        // stability. The readiness banner is already confirmed present above (the
        // boot drive only returns Ok once the marker is seen), so the recorded
        // per-run console is the post-snapshot workload.
        if let Some(dir) = args.sweep.record.clone() {
            let Some(retain) = parse_retain(&args.sweep.retain) else {
                return ExitCode::FAILURE;
            };
            let store = match TraceStore::open(&dir) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[conductor] cannot open trace store {}: {e}", dir.display());
                    return ExitCode::FAILURE;
                }
            };
            let cfg = RecordConfig {
                sweep: SweepConfig {
                    seeds: seeds(args.sweep.seeds),
                    runs_per_seed: args.sweep.runs.max(2),
                    deadline_delta: Some(args.deadline_delta),
                    snapshot_retry_step,
                    snapshot_max_attempts,
                },
                retain,
                stream: StreamId(0),
            };
            println!(
                "[conductor] box recording: {} seeds x {} runs, retain={}, into {}\n",
                cfg.sweep.seeds.len(),
                cfg.sweep.runs_per_seed,
                retain.as_str(),
                dir.display()
            );
            return match run_recording(&mut server, &store, &cfg) {
                Ok(report) => finish_recording("box", &report, 2, &store, &dir),
                Err(e) => {
                    eprintln!("[conductor] box recording failed: {e}");
                    ExitCode::FAILURE
                }
            };
        }

        let cfg = SweepConfig {
            seeds: seeds(args.sweep.seeds),
            runs_per_seed: args.sweep.runs.max(2),
            deadline_delta: Some(args.deadline_delta),
            snapshot_retry_step,
            snapshot_max_attempts,
        };
        let initial = boot_env();
        println!(
            "[conductor] box mode: {} seeds x {} runs; each branch runs {} ns of V-time past the \
             snapshot.\n",
            cfg.seeds.len(),
            cfg.runs_per_seed,
            args.deadline_delta
        );
        let (served, client) = run_session(&mut server, move |stream| {
            sweep_client(stream, initial, cfg)
        });
        finish("box", served, client)
    }

    /// The task-60 box milestone: boot the Postgres-**campaign** image (the
    /// planted-bug workload), seal a mid-workload base, and run the seed-driven
    /// fault campaign against it — the **identical** [`run_campaign`] loop the
    /// portable gate drives against the toy, only the backing guest swapped.
    ///
    /// The host-fault schedule the campaign mints rides the branch env and is
    /// enforced by task-59's server between instructions at the fault's `Moment`;
    /// the emitted `Bug`'s env replays it bit-for-bit (the record → replay
    /// closure). The search space is CLI-scoped — the operator narrows `--gpa-*`
    /// once the supervisor's ledger gpa is pinned (see `CampaignBoxArgs`).
    pub fn run_campaign(args: CampaignBoxArgs) -> ExitCode {
        let mut server = match boot_server(&args.kernel, &args.initramfs, &args.ready_marker) {
            Ok(s) => s,
            Err(code) => return code,
        };

        let gpa_candidates: Vec<u64> = (0..args.gpa_count)
            .map(|i| {
                args.gpa_base
                    .saturating_add(i.saturating_mul(args.gpa_stride))
            })
            .collect();
        let cfg = CampaignConfig {
            campaign_seed: args
                .campaign
                .campaign_seed
                .unwrap_or(CampaignConfig::toy().campaign_seed),
            max_branches: args.campaign.max_branches,
            // Floor at the spec bar (25/25): the flag can raise it, never lower it.
            replay_n: args.campaign.replay_n.max(super::REPLAY_BAR),
            deadline_delta: Some(args.deadline_delta),
            gpa_candidates,
            moment_window: (args.window_lo, args.window_hi),
            // Single-event upsets on byte boundaries (the naive upset alphabet).
            mask_bits: vec![7, 15, 23, 31, 39, 47, 55, 63],
            // A fine retry step so the base seals *close to* CAMPAIGN_READY (early
            // in the supervisor loop), maximizing the remaining fault window — a
            // coarse step overshoots a short loop into the halt tail (the base
            // gate proved a coarse step + short loop leaves the loop unreachable).
            snapshot_retry_step: 10_000,
            snapshot_max_attempts: 200_000,
            nominal_seed: CampaignConfig::toy().nominal_seed,
        };
        let n = cfg.replay_n;
        let initial = boot_env();
        println!(
            "[conductor] campaign box: searching {} branches over {} gpa candidates × window \
             [{}, {}) ns × {} mask bits; each branch runs {} ns past the base.\n",
            cfg.max_branches,
            cfg.gpa_candidates.len(),
            cfg.moment_window.0,
            cfg.moment_window.1,
            cfg.mask_bits.len(),
            args.deadline_delta,
        );
        let (served, client) = run_session(&mut server, move |stream| {
            let mut machine = SocketMachine::connect(stream, initial)?;
            run_campaign_loop(&mut machine, &SpecEnvCodec, &cfg)
        });
        let report = match client {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[conductor] campaign box: the campaign failed (transport/backend): {e}");
                if let Err(se) = served {
                    eprintln!("[conductor] campaign box: server session ended with: {se}");
                }
                return ExitCode::FAILURE;
            }
        };
        if let Err(se) = served {
            eprintln!("[conductor] campaign box: server session ended with a fatal error: {se}");
            return ExitCode::FAILURE;
        }
        finish_campaign("box", &report, n)
    }
}
