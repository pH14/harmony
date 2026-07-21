// SPDX-License-Identifier: AGPL-3.0-or-later
//! The box composition root. Linux-only (`boot_linux_selected` + `perf_event`).
//!
//! The one piece of **workload-aware policy** in the whole path lives here (the
//! server and adapter stay workload-blind): the live guest is driven to a
//! readiness marker on its serial *before* the sweep seals the base, so the
//! snapshot lands **mid-workload, post-readiness** (the gate's point) rather
//! than at boot entry. Choosing *where* to snapshot is a property of the guest;
//! the snapshot *mechanism* (the verb) is not.
//!
//! Split into its own file (issue #69 coverage recovery) so the coverage job
//! can exclude it by name via `--ignore-filename-regex`, like the other
//! box-only substrate files (`kvm.rs`, `patched_kvm.rs`, `pmu_sys.rs`,
//! `work_perf.rs`): every function here needs a real `/dev/kvm`, patched KVM,
//! and a built guest image, so no portable test can drive it, and the
//! coverage job's self-hosted runner does not boot live KVM. `main.rs`'s
//! portable dispatch logic is unit-tested directly (see its `#[cfg(test)]`
//! module); this file's real coverage is the box gates recorded in
//! IMPLEMENTATION.md.

use std::io::Write;
use std::process::ExitCode;

// Aliased: the module's own `pub fn run_campaign` (the box entry point)
// would otherwise collide with the imported campaign loop (E0255), and the
// call below would silently resolve to the 1-arg local fn (E0061). This code
// is `cfg(target_os = "linux")`, so the collision is invisible to a Mac
// `cargo check` — the Linux-target check in the gate list catches it.
use campaign_runner::campaign::{CampaignConfig, run_campaign as run_campaign_loop};
use campaign_runner::record::{RecordConfig, run_recording};
use campaign_runner::stopwatch::{Phase, PhaseStats, mark};
use campaign_runner::{SweepConfig, run_session, sweep_client};
use environment::{EnvSpec, FaultPolicy};
use explorer::adapter::SocketMachine;
use explorer::{SpecEnvCodec, StreamId};
use runtrace::TraceStore;
use vmm_backend::{Backend, X86};
use vmm_core::control::{ControlServer, VmmFactory};
use vmm_core::vendor::x86::bringup::boot_linux_selected;
use vmm_core::vmm::{PVCLOCK_DEFAULT_DELTA_WORK, Step, Vmm};

use campaign_runner::gamecampaign::{GameCampaignConfig, run_game_campaign};
use campaign_runner::mazecampaign::{run_maze_campaign, serial_maze_spec};

use super::{
    ArmThroughput, BenchBoxArgs, BoxArgs, CampaignBoxArgs, GameBoxArgs, MazeBoxArgs, X86_64_BOOT,
    ab_report, finish, finish_campaign, finish_game, finish_maze, finish_recording, maze_cfg,
    maze_manifest, parse_game_config, parse_maze_config, parse_retain, print_game_artifacts,
    pvclock_cmdline, render_sweep_throughput, require_page_on_active, seeds,
};

/// 2 GiB guest RAM (matches `live_postgres.rs` / `live_branching_demo.rs`).
const GUEST_RAM_LEN: usize = 2 << 30;
/// The boot seed the live VM runs under (matches the branching demo).
const BOOT_SEED: u64 = 0x0028_C0FF_EE5E_EDC0;
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
        repo_root().join("harmony-linux/build").join(name),
        repo_root().join("harmony-linux/linux").join(name),
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
fn drive_to_marker(vmm: &mut Vmm<Box<dyn Backend<A = X86>>>, marker: &[u8]) -> Result<u64, String> {
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
            // A cooperating-SDK stop (task 73) — an assertion violation — is a
            // premature stop here, just like a terminal: the readiness marker
            // never appeared.
            Ok(Step::SdkStop) => {
                return Err(format!(
                    "guest hit an SDK stop (assertion) at step {steps} before the readiness \
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
/// [`ControlServer`] ready to serve plus the boot-to-ready wall-clock
/// duration in microseconds (task 96 — observation-only, see
/// `campaign_runner::stopwatch`'s module doc), or a failing [`ExitCode`] with a
/// loud reason (never a vacuous success). Shared verbatim by the sweep
/// ([`run`](run)) and the campaign ([`run_campaign`](run_campaign)) so both
/// boot the guest identically.
/// What [`boot_server`] hands back: the composed control server, the
/// boot-to-ready wall-clock (µs, task 96 — observation only), and the boot
/// serial transcript up to the readiness marker (the game path's ROM-hash
/// cross-check input).
type BootedServer = (ControlServer<Box<dyn Backend<A = X86>>>, u64, Vec<u8>);

fn boot_server(
    kernel_name: &str,
    initramfs_name: &str,
    ready_marker: &str,
    page_on: bool,
) -> Result<BootedServer, ExitCode> {
    if !std::path::Path::new("/dev/kvm").exists() {
        eprintln!(
            "[campaign-runner] /dev/kvm absent — run on the determinism box with the LOADED patched \
             KVM modules, CPU-pinned per docs/BOX-PINNING.md."
        );
        return Err(ExitCode::FAILURE);
    }
    // The frozen contract cannot run off the det-cfl-v1 baseline.
    let report = vmm_core::vendor::x86::hostassert::report();
    if let Some(bad) = report.iter().find(|o| !o.pass) {
        eprintln!(
            "[campaign-runner] host is not the det-cfl-v1 baseline (first failing assertion: {} \
             expected {}, observed {}). Run on the box.",
            bad.key, bad.expected, bad.actual
        );
        return Err(ExitCode::FAILURE);
    }
    let (Some(kernel), Some(initramfs)) = (artifact(kernel_name), artifact(initramfs_name)) else {
        eprintln!(
            "[campaign-runner] guest image missing ({kernel_name} / {initramfs_name}) — build it \
             first: `make -C harmony-linux fetch && make -C harmony-linux/linux campaign-image` (or \
             `postgres-image`), or pass --initramfs for an image already on the box."
        );
        return Err(ExitCode::FAILURE);
    };

    // The page-on composition (task 110 deliverable 7): the guest cmdline
    // advertises ` harmony_pvclock` AND the host offers the page
    // (`enable_pvclock`) — both from the ONE `page_on` knob, on the live VM and
    // every forked branch, so the A/B arms cannot drift (the live_pvclock.rs
    // discipline). Page-off is byte-for-byte today's boot.
    let cmdline = pvclock_cmdline(X86_64_BOOT.cmdline, page_on);
    // "Boot" starts here (task 96 §4: "from server boot start to the readiness
    // marker") — before `boot_linux_selected`, so the phase covers KVM backend
    // creation + guest RAM load + the initial restore, not just the
    // post-boot drive to the marker.
    let boot_t0 = mark();
    let mut live = match boot_linux_selected(
        X86_64_BOOT.backend,
        &kernel,
        &initramfs,
        GUEST_RAM_LEN,
        &cmdline,
        BOOT_SEED,
    ) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[campaign-runner] boot_linux_selected (patched) failed: {e}");
            return Err(ExitCode::FAILURE);
        }
    };
    if page_on {
        live.enable_pvclock(PVCLOCK_DEFAULT_DELTA_WORK);
    }
    println!("[campaign-runner] box: booting the guest to the readiness marker {ready_marker:?} …");
    let boot_us = match drive_to_marker(&mut live, ready_marker.as_bytes()) {
        Ok(steps) => {
            let boot_us = boot_t0.elapsed_us();
            println!(
                "\n[campaign-runner] readiness marker reached at step {steps}; the base snapshot \
                 will be sealed at the next snapshottable boundary at/after this point, wall \
                 {}s.\n",
                boot_us / 1_000_000
            );
            boot_us
        }
        Err(e) => {
            eprintln!("\n[campaign-runner] failed to reach the readiness marker: {e}");
            return Err(ExitCode::FAILURE);
        }
    };
    // r15 P1: a `--page-on` run must have ACTUALLY reached the page-on
    // composition before it reports anything — the host registered the guest's
    // page AND Linux selected `harmony-pvclock` as the active clocksource.
    // Otherwise a stale/non-pvclock image, a failed registration, or a
    // TSC-still-active guest would sail to the readiness marker and get reported
    // as a page-on determinism/throughput result that is effectively page-OFF.
    if page_on
        && let Err(why) =
            require_page_on_active(live.pvclock_registration().is_some(), live.serial())
    {
        eprintln!(
            "[campaign-runner] --page-on requested but the page is not active: {why}. Check the \
             guest 'harmony_pvclock:' / 'clocksource' console lines above; rebuild the pvclock \
             guest image if it is stale."
        );
        return Err(ExitCode::FAILURE);
    }

    // The fork factory: fresh, equivalently-composed patched VMs whose
    // boot-loaded image the restore immediately overwrites. `live` is
    // already booted (it owns its guest RAM), so **move** the sole
    // kernel/initramfs copies into the closure rather than cloning them —
    // an initramfs is tens/hundreds of MB, and cloning would keep two
    // copies resident for the whole run.
    let factory_cmdline = cmdline.clone();
    let factory: VmmFactory<Box<dyn Backend<A = X86>>> = Box::new(move || {
        let mut v = boot_linux_selected(
            X86_64_BOOT.backend,
            &kernel,
            &initramfs,
            GUEST_RAM_LEN,
            &factory_cmdline,
            BOOT_SEED,
        )?;
        // A forked branch must be composed EXACTLY like the live VM (same-state
        // ⇒ same-future): offer the page here too when page-on, else a restored
        // branch would answer the guest's registration `UnknownService` where the
        // source accepted it (the pvclock_validate_restore capability check).
        if page_on {
            v.enable_pvclock(PVCLOCK_DEFAULT_DELTA_WORK);
        }
        Ok(v)
    });
    // The serial transcript up to readiness rides back to the caller: the
    // game path cross-checks the operator's `--rom-sha256` against the
    // guest's own `GAME_ROM_SHA256:` line (round-9 P1 — the content-hash
    // discipline; an operator-typed hash is a claim, the booted image is the
    // fact). Boot-sized (tens of KB), snapshotted once.
    let serial = live.serial().to_vec();
    Ok((ControlServer::new(live, factory), boot_us, serial))
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
    // Deliverable 7's A/B campaign-throughput comparison: run BOTH arms over the
    // same sweep and emit the validated page-on/page-off ratio in one invocation.
    if args.page_on_ab {
        return run_ab(args);
    }
    // `--record` is its own single-arm mode (a comparison records nothing).
    if args.sweep.record.is_some() {
        return run_recording_box(&args);
    }
    // A single sweep arm (page-on per `--page-on`, else page-off).
    match sweep_arm(&args, args.page_on) {
        Ok((_arm, code)) => code,
        Err(code) => code,
    }
}

/// Postgres is interrupt-driven; the snapshot search may need many steps to find
/// a sealable boundary at/after readiness (task 41 made mid-workload points
/// snapshottable). Shared by every box sweep mode.
const SNAPSHOT_RETRY: (u64, usize) = (1_000_000, 100_000);

/// One campaign sweep arm: boot the guest page-{on,off}, seal the base, run the
/// sweep, print the per-arm throughput REPORT line, and run the determinism gate
/// (via [`finish`]). Returns the arm's throughput record plus the gate `ExitCode`,
/// or a boot-failure `ExitCode`. Shared by the single-arm [`run`] and the two-arm
/// [`run_ab`], so the arms are composed identically.
fn sweep_arm(args: &BoxArgs, page_on: bool) -> Result<(ArmThroughput, ExitCode), ExitCode> {
    let (mut server, _boot_us, _serial) =
        boot_server(&args.kernel, &args.initramfs, &args.ready_marker, page_on)?;
    let (snapshot_retry_step, snapshot_max_attempts) = SNAPSHOT_RETRY;
    let cfg = SweepConfig {
        seeds: seeds(args.sweep.seeds),
        runs_per_seed: args.sweep.runs.max(2),
        deadline_delta: Some(args.deadline_delta),
        snapshot_retry_step,
        snapshot_max_attempts,
    };
    let seeds_n = cfg.seeds.len();
    let runs_n = cfg.runs_per_seed;
    let initial = boot_env();
    println!(
        "[campaign-runner] box mode (page-{}): {seeds_n} seeds x {runs_n} runs; each branch runs \
         {} ns of V-time past the snapshot.\n",
        if page_on { "on" } else { "off" },
        args.deadline_delta
    );
    // Time the sweep so deliverable 7's campaign-throughput comparison is
    // producible (r15): wall duration + branch count → a page-labelled throughput
    // line. The stopwatch is print-only (task 96 — nothing in the search loop
    // branches on a duration), so this never touches state or a hash.
    let sweep_t0 = mark();
    let (served, client) = run_session(&mut server, move |stream| {
        sweep_client(stream, initial, cfg)
    });
    let sweep_us = sweep_t0.elapsed_us();
    // Actual completed branches from the report (a partial sweep reports what it
    // finished; 0 if it errored — `finish` tells that story).
    let branches: u64 = client
        .as_ref()
        .ok()
        .map(|r| r.rows.iter().map(|s| s.runs.len() as u64).sum())
        .unwrap_or(0);
    println!("{}", render_sweep_throughput(page_on, branches, sweep_us));
    let code = finish("box", served, client);
    Ok((
        ArmThroughput {
            branches,
            sweep_us,
            seeds: seeds_n,
            runs_per_seed: runs_n,
            deadline_delta: args.deadline_delta,
        },
        code,
    ))
}

/// The `--record` single-arm box mode: boot, seal, and record each run's RunTrace,
/// checking byte-stability (the task-65 gate). The readiness banner is already
/// confirmed present by `boot_server`, so the recorded per-run console is the
/// post-snapshot workload.
fn run_recording_box(args: &BoxArgs) -> ExitCode {
    let (mut server, _boot_us, _serial) = match boot_server(
        &args.kernel,
        &args.initramfs,
        &args.ready_marker,
        args.page_on,
    ) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let Some(dir) = args.sweep.record.clone() else {
        return ExitCode::FAILURE; // only reached via run()'s is_some() guard
    };
    let Some(retain) = parse_retain(&args.sweep.retain) else {
        return ExitCode::FAILURE;
    };
    let store = match TraceStore::open(&dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "[campaign-runner] cannot open trace store {}: {e}",
                dir.display()
            );
            return ExitCode::FAILURE;
        }
    };
    let (snapshot_retry_step, snapshot_max_attempts) = SNAPSHOT_RETRY;
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
        "[campaign-runner] box recording: {} seeds x {} runs, retain={}, into {}\n",
        cfg.sweep.seeds.len(),
        cfg.sweep.runs_per_seed,
        retain.as_str(),
        dir.display()
    );
    match run_recording(&mut server, &store, &cfg) {
        Ok(report) => finish_recording("box", &report, 2, &store, &dir),
        Err(e) => {
            eprintln!("[campaign-runner] box recording failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Deliverable 7's A/B campaign-throughput comparison, in one invocation: run the
/// page-OFF baseline and the page-ON arm over the **same** sweep, then emit the
/// validated ratio ([`ab_report`]). Both arms must pass their determinism gates —
/// a ratio over a broken arm is meaningless — and use matching sweep parameters
/// (true by construction here, one `BoxArgs`, and re-checked in `ab_report`). This
/// is the runnable path the A/B formatter is wired into (r17).
fn run_ab(args: BoxArgs) -> ExitCode {
    if args.sweep.record.is_some() {
        eprintln!(
            "[campaign-runner] --page-on-ab is a throughput comparison and records nothing — \
             drop --record."
        );
        return ExitCode::FAILURE;
    }
    println!(
        "[campaign-runner] box A/B: the page-OFF baseline, then the page-ON arm, over the same \
         sweep.\n"
    );
    let (off, off_code) = match sweep_arm(&args, false) {
        Ok(pair) => pair,
        Err(code) => return code,
    };
    if off_code != ExitCode::SUCCESS {
        eprintln!("[campaign-runner] A/B: the page-OFF arm failed its gates — no ratio emitted.");
        return off_code;
    }
    let (on, on_code) = match sweep_arm(&args, true) {
        Ok(pair) => pair,
        Err(code) => return code,
    };
    if on_code != ExitCode::SUCCESS {
        eprintln!("[campaign-runner] A/B: the page-ON arm failed its gates — no ratio emitted.");
        return on_code;
    }
    match ab_report(&off, &on) {
        Ok(line) => {
            println!("{line}");
            ExitCode::SUCCESS
        }
        Err(why) => {
            eprintln!("[campaign-runner] A/B report refused: {why}");
            ExitCode::FAILURE
        }
    }
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
/// The task-86 M0 box game campaign: boot the game image (QuickNES + the
/// user-supplied ROM under the play-agent), drive to `GAME_READY`, seal the
/// base at the agent's `setup_complete` snapshot point, and run the quiet-arm
/// exploration campaign. `--repeat N` reruns the **identical** campaign from a
/// fresh boot each time and requires bit-identical logs (the per-branch
/// `state_hash` sequence gate is `--repeat 25` — task 86 gate 2).
pub fn run_game(args: GameBoxArgs) -> ExitCode {
    let config = match parse_game_config(&args.game.config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[campaign-runner] {e}");
            return ExitCode::FAILURE;
        }
    };
    let cfg = GameCampaignConfig {
        campaign_seed: args.game.campaign_seed,
        max_branches: args.game.max_branches,
        deadline_delta: Some(args.deadline_delta),
        explore_period: args.game.explore_period,
        snapshot_retry_step: 1_000_000,
        snapshot_max_attempts: 100_000,
        setup_deadline_delta: args.setup_deadline_delta,
        rom_sha256: args.game.rom_sha256.clone(),
        // The box workload gate (round-5 P1): no setup_complete ⇒ a dead
        // play-agent ⇒ loud failure, never a sealed dead base.
        require_snapshot_point: true,
        trace_dir: args.game.trace_out.clone(),
        // The billboard-range bound (task 103 finding 2), from the RAM this
        // composition root actually boots the guest with.
        guest_ram_len: GUEST_RAM_LEN as u64,
        // The two-barrier controller's materialization knobs (task 132): a
        // small per-step cap — each materialized candidate replays a real
        // rollout on the box — and a campaign budget of one replay per
        // branch on average.
        candidate_cap: 2,
        replay_budget: args.game.max_branches,
        // The task-136 blocking sub-gate: on the box every admitted entry
        // must re-materialize bit-identically from its ledger env alone, so
        // a retry-forward seal's extra live legs are proven state-neutral.
        // Part of the outcome, so `--repeat` also compares the reseal hashes.
        verify_reseal: true,
    };
    let repeat = args.repeat.max(1);
    let mut first: Option<campaign_runner::gamecampaign::GameCampaignOutcome> = None;
    for rep in 0..repeat {
        // A fresh, identically-seeded boot per repetition: the determinism
        // claim is over the whole boot → seal → campaign pipeline, so nothing
        // is allowed to carry over between repeats.
        // `boot_us` is the task-96 stopwatch's boot-to-ready wall-clock —
        // host-side observation only (printed, never fed to the campaign).
        let (mut server, boot_us, serial) =
            match boot_server(&args.kernel, &args.initramfs, &args.ready_marker, false) {
                Ok(s) => s,
                Err(code) => return code,
            };
        // Round-9 P1 — the ROM content-hash discipline: the operator's
        // `--rom-sha256` is a claim; the booted image's own
        // `GAME_ROM_SHA256:` serial line (printed by game-init before
        // GAME_READY, from the hash baked in at image build) is the fact.
        // Every repetition boots fresh, so every boot is checked.
        let booted = campaign_runner::gamecampaign::serial_rom_sha256(&serial);
        match (&args.game.rom_sha256, &booted) {
            (Some(claim), Some(fact)) if !claim.eq_ignore_ascii_case(fact) => {
                eprintln!(
                    "[campaign-runner] game box: ROM hash mismatch — --rom-sha256 {claim} but the \
                     booted image reports {fact}; the log would be stamped with the wrong dump. \
                     Rebuild/point at the right image or fix the flag."
                );
                return ExitCode::FAILURE;
            }
            (Some(claim), None) => {
                eprintln!(
                    "[campaign-runner] game box: --rom-sha256 {claim} was claimed but the booted image \
                     printed no GAME_ROM_SHA256 line (ROM-less image?) — refusing to stamp."
                );
                return ExitCode::FAILURE;
            }
            (None, Some(fact)) => {
                eprintln!(
                    "[campaign-runner] game box: the booted image reports GAME_ROM_SHA256 {fact} but \
                     no --rom-sha256 was passed — an unstamped log defeats the mixed-dump check; \
                     rerun with --rom-sha256 {fact}."
                );
                return ExitCode::FAILURE;
            }
            _ => {}
        }
        let mut rep_cfg = cfg.clone();
        // Repetitions must be INDEPENDENT for the determinism gate: each gets
        // its own trace/evidence directory (a shared durable evidence ledger
        // would seed repetition N's archive with repetition N-1's committed
        // assignments — resumption, not repetition; task 132). The retained
        // deep trace is content-addressed, so identical reps still yield the
        // identical trace_id.
        rep_cfg.trace_dir = cfg
            .trace_dir
            .as_ref()
            .map(|d| d.join(format!("rep-{}", rep + 1)));
        let initial = boot_env();
        println!(
            "[campaign-runner] game box: campaign {}/{repeat} (config={:?}, {} branches, {} ns per \
             rollout; boot-to-ready {} ms)…",
            rep + 1,
            config,
            rep_cfg.max_branches,
            args.deadline_delta,
            boot_us / 1_000,
        );
        let (served, client) = run_session(&mut server, move |stream| {
            let machine = SocketMachine::connect(stream, initial)?;
            run_game_campaign(machine, Box::new(SpecEnvCodec), &rep_cfg, config)
                .map_err(|e| explorer::MachineError::Transport(e.to_string()))
        });
        let outcome = match client {
            Ok(o) => o,
            Err(e) => {
                eprintln!("[campaign-runner] game box: campaign failed: {e}");
                if let Err(se) = served {
                    eprintln!("[campaign-runner] game box: server session ended with: {se}");
                }
                return ExitCode::FAILURE;
            }
        };
        if let Err(se) = served {
            eprintln!("[campaign-runner] game box: server session ended with a fatal error: {se}");
            return ExitCode::FAILURE;
        }
        match &first {
            None => first = Some(outcome),
            Some(f) => {
                if *f != outcome {
                    // The determinism gate's loud failure: name the first
                    // diverging branch so the transcript pins it.
                    let at = f
                        .log
                        .events
                        .iter()
                        .zip(&outcome.log.events)
                        .position(|(a, b)| a != b)
                        .map_or_else(
                            || "event count/artifacts".to_string(),
                            |i| format!("branch {i}"),
                        );
                    eprintln!(
                        "[campaign-runner] game box DETERMINISM FAILED: repetition {}/{repeat} \
                         diverged from the first at {at}.",
                        rep + 1
                    );
                    return ExitCode::FAILURE;
                }
                println!(
                    "[campaign-runner] game box: repetition {}/{repeat} bit-identical to the first.",
                    rep + 1
                );
            }
        }
    }
    let outcome = first.expect("repeat >= 1 always produces a first outcome");
    // The gate's verdict — the 25/25 floor (round-8 P1) AND the vacuity guard
    // (task 103 finding 1b): bit-identical repetitions of a campaign that did
    // no work are still bit-identical, so identity is never enough. A run with
    // no branches / no V-time / no frames fails here, before any banner, no
    // matter which flags produced it.
    let verdict = match campaign_runner::gamecampaign::determinism_verdict(&outcome, repeat) {
        Ok(v) => v,
        Err(vacuity) => {
            eprintln!(
                "[campaign-runner] game box VACUOUS RUN — refusing the determinism gate: {vacuity}\n\
                 [campaign-runner] evidence: {:?}",
                outcome.work
            );
            return ExitCode::FAILURE;
        }
    };
    println!(
        "[campaign-runner] game box work evidence: {} branches, weakest rollout {} ns of V-time / {} \
         COMPLETED frames.",
        outcome.work.branches, outcome.work.min_vtime_span, outcome.work.min_completed_frames
    );
    // Task-136 reseal-identity evidence (hm-esfd): each admitted entry
    // re-materialized bit-identically from its ledger env alone, with its
    // recorded reseed markers all at-or-below the seal (asserted in the
    // campaign; a violation failed the run before this line).
    println!(
        "[campaign-runner] game box reseal evidence: {} entries re-materialized bit-identically; \
         {} NotQuiescent seal refusals hit by the task-136 run-forward (dropped candidates included).",
        outcome.reseals.len(),
        outcome.seal_retries
    );
    for r in &outcome.reseals {
        println!(
            "[campaign-runner]   reseal entry={} sealed_at={} markers={:?} state_hash={}",
            r.entry, r.sealed_at, r.markers, r.state_hash
        );
    }
    if let Some(banner) = verdict.banner() {
        println!("{banner}");
    }
    print_game_artifacts(&outcome);
    let manifest = benchmark::GameManifest::smb(
        args.game.rom_sha256.clone(),
        args.game.max_branches,
        Some(args.deadline_delta),
    );
    finish_game(&outcome.log, args.game.logs_out.as_ref(), &manifest)
}

/// The box maze campaign (task 134 M1/M2): the maze image on patched KVM
/// through the two-barrier controller — the run_game shape minus the
/// ROM/billboard machinery, plus the MAZE_SPEC serial cross-check.
pub fn run_maze(args: MazeBoxArgs) -> ExitCode {
    let config = match parse_maze_config(&args.maze.config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[campaign-runner] {e}");
            return ExitCode::FAILURE;
        }
    };
    let cfg = campaign_runner::mazecampaign::MazeCampaignConfig {
        deadline_delta: Some(args.deadline_delta),
        setup_deadline_delta: args.setup_deadline_delta,
        snapshot_retry_step: 1_000_000,
        snapshot_max_attempts: 100_000,
        // The box workload gate: no setup_complete ⇒ a dead maze agent ⇒
        // loud failure, never a sealed dead base — and a rollout ending
        // anywhere but its deadline DIED.
        require_snapshot_point: true,
        ..maze_cfg(&args.maze)
    };
    let repeat = args.repeat.max(1);
    let mut first: Option<campaign_runner::mazecampaign::MazeCampaignOutcome> = None;
    for rep in 0..repeat {
        // A fresh, identically-seeded boot per repetition: the determinism
        // claim is over the whole boot → seal → campaign pipeline.
        let (mut server, boot_us, serial) =
            match boot_server(&args.kernel, &args.initramfs, &args.ready_marker, false) {
                Ok(s) => s,
                Err(code) => return code,
            };
        // The maze-spec discipline (the ROM content-hash check, transplanted):
        // the operator's spec flags are a claim; the booted agent's MAZE_SPEC
        // serial line is the fact. Logs from a different maze are not
        // comparable — refuse a mismatch on every boot.
        match serial_maze_spec(&serial) {
            Some(fact) if fact != cfg.spec => {
                eprintln!(
                    "[campaign-runner] maze box: spec mismatch — flags say {:?} but the booted \
                     image reports {fact:?}; rebuild the image or fix the flags.",
                    cfg.spec
                );
                return ExitCode::FAILURE;
            }
            None => {
                eprintln!(
                    "[campaign-runner] maze box: the booted image printed no MAZE_SPEC line \
                     before {} — a dead or foreign agent; refusing to campaign against it.",
                    args.ready_marker
                );
                return ExitCode::FAILURE;
            }
            Some(_) => {}
        }
        let mut rep_cfg = cfg.clone();
        // Repetitions must be INDEPENDENT for the determinism gate: each gets
        // its own trace/evidence directory (a shared durable ledger would
        // seed repetition N's archive with N-1's committed assignments —
        // resumption, not repetition).
        rep_cfg.trace_dir = cfg
            .trace_dir
            .as_ref()
            .map(|d| d.join(format!("rep-{}", rep + 1)));
        let initial = boot_env();
        println!(
            "[campaign-runner] maze box: campaign {}/{repeat} (config={:?}, {} branches, {} ns per \
             rollout; boot-to-ready {} ms)…",
            rep + 1,
            config,
            rep_cfg.max_branches,
            args.deadline_delta,
            boot_us / 1_000,
        );
        let (served, client) = run_session(&mut server, move |stream| {
            let machine = SocketMachine::connect(stream, initial)?;
            run_maze_campaign(machine, Box::new(SpecEnvCodec), &rep_cfg, config)
                .map_err(|e| explorer::MachineError::Transport(e.to_string()))
        });
        let outcome = match client {
            Ok(o) => o,
            Err(e) => {
                eprintln!("[campaign-runner] maze box: campaign failed: {e}");
                if let Err(se) = served {
                    eprintln!("[campaign-runner] maze box: server session ended with: {se}");
                }
                return ExitCode::FAILURE;
            }
        };
        if let Err(se) = served {
            eprintln!("[campaign-runner] maze box: server session ended with a fatal error: {se}");
            return ExitCode::FAILURE;
        }
        match &first {
            None => first = Some(outcome),
            Some(f) => {
                if *f != outcome {
                    let at = f
                        .log
                        .events
                        .iter()
                        .zip(&outcome.log.events)
                        .position(|(a, b)| a != b)
                        .map_or_else(
                            || "event count/artifacts".to_string(),
                            |i| format!("branch {i}"),
                        );
                    eprintln!(
                        "[campaign-runner] maze box DETERMINISM FAILED: repetition {}/{repeat} \
                         diverged from the first at {at}.",
                        rep + 1
                    );
                    return ExitCode::FAILURE;
                }
                println!(
                    "[campaign-runner] maze box: repetition {}/{repeat} bit-identical to the first.",
                    rep + 1
                );
            }
        }
    }
    let outcome = first.expect("repeat >= 1 always produces a first outcome");
    // The vacuity guard before any banner: bit-identical repetitions of a
    // campaign that did no work are still bit-identical.
    if let Some(vacuity) = outcome.vacuity() {
        eprintln!(
            "[campaign-runner] maze box VACUOUS RUN — refusing the determinism gate: {vacuity}\n\
             [campaign-runner] evidence: {:?}",
            outcome.work
        );
        return ExitCode::FAILURE;
    }
    println!(
        "[campaign-runner] maze box work evidence: {} branches, weakest rollout {} ns of V-time / \
         {} walk steps.",
        outcome.work.branches, outcome.work.min_vtime_span, outcome.work.min_steps
    );
    if repeat >= 25 {
        println!(
            "[campaign-runner] maze box DETERMINISM PASS: {repeat}/{repeat} identical per-branch \
             state_hash sequences (gate floor 25)."
        );
    } else if repeat > 1 {
        println!(
            "[campaign-runner] maze box determinism smoke: {repeat}/{repeat} identical — BELOW \
             the 25-run gate floor, NOT the determinism gate."
        );
    }
    let manifest = maze_manifest(&cfg, Some(args.deadline_delta));
    finish_maze(&outcome, args.maze.logs_out.as_ref(), &manifest)
}

pub fn run_campaign(args: CampaignBoxArgs) -> ExitCode {
    let (mut server, boot_us, _serial) =
        match boot_server(&args.kernel, &args.initramfs, &args.ready_marker, false) {
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
        "[campaign-runner] campaign box: searching {} branches over {} gpa candidates × window \
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
    let mut report = match client {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "[campaign-runner] campaign box: the campaign failed (transport/backend): {e}"
            );
            if let Err(se) = served {
                eprintln!("[campaign-runner] campaign box: server session ended with: {se}");
            }
            return ExitCode::FAILURE;
        }
    };
    if let Err(se) = served {
        eprintln!("[campaign-runner] campaign box: server session ended with a fatal error: {se}");
        return ExitCode::FAILURE;
    }
    // Fold the boot-to-ready measurement (task 96) into the report's `Boot`
    // phase: it happened before `run_campaign_loop`'s own `Stopwatch` existed
    // (boot brackets `boot_server`, well outside the campaign loop), so it
    // cannot go through `Stopwatch::time` — merged in here instead.
    report
        .timing
        .insert(Phase::Boot, PhaseStats::single(boot_us));
    finish_campaign("box", &report, n)
}

/// Lowercase-hex of a 32-byte state hash, for the finding certificate line the
/// determinism stress-test compares solo vs co-tenant.
fn hex32(h: &[u8; 32]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(64);
    for b in h {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Task 69 M2 (GO/NO-GO #2): run ONE benchmark campaign — a `(bug, config, seed)`
/// signal-vs-baseline run — against a real planted-bug guest on patched KVM, and
/// emit its `CampaignLog` (+ per-find state hashes) as JSON.
///
/// This is the gate-deciding run. It boots the bug's image to its readiness marker,
/// seals a mid-workload base, and drives the **identical** [`run_bench_campaign`]
/// loop the portable gate drives against the toy — only the backing guest swapped,
/// with the two M2 prerequisites live: (a) the real guest console is captured through
/// the task-69 `Console` wire verb so the real `LogSensor`/`CellFnV1` signal sees
/// guest logs, and (b) fault moments are rebased onto the sealed base's V-time so a
/// planted fault lands in its vulnerable window.
///
/// One campaign per invocation (isolated in its own process on its own leased core),
/// so the operator runs up to 3 concurrently on distinct cores and compares each
/// finding's `state_hash` solo vs co-tenant — the determinism stress-test (a solo≠
/// co-tenant hash is a P0 determinism leak, not a speed hiccup).
#[cfg(target_os = "linux")]
pub fn run_bench_campaign_box(args: BenchBoxArgs) -> ExitCode {
    use benchmark::manifest::{Benchmark, BugId};
    use benchmark::report::Configuration;
    use campaign_runner::benchcampaign::{BenchConfig, run_bench_campaign};

    // 1. The (optionally box-calibrated) benchmark manifest + the target bug/config.
    let bench = match &args.calibration {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(s) => match serde_json::from_str::<Benchmark>(&s) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!(
                        "[campaign-runner] benchcampaign box: calibration {} is not a valid Benchmark: {e}",
                        path.display()
                    );
                    return ExitCode::FAILURE;
                }
            },
            Err(e) => {
                eprintln!(
                    "[campaign-runner] benchcampaign box: cannot read calibration {}: {e}",
                    path.display()
                );
                return ExitCode::FAILURE;
            }
        },
        None => Benchmark::wave5(),
    };
    let Some(spec) = bench.get(BugId(args.bug)).cloned() else {
        eprintln!(
            "[campaign-runner] benchcampaign box: no bug {} in the manifest",
            args.bug
        );
        return ExitCode::FAILURE;
    };
    let config = match args.config.as_str() {
        "signal" => Configuration::Signal,
        "baseline" => Configuration::Baseline,
        other => {
            eprintln!(
                "[campaign-runner] benchcampaign box: --config must be `signal` or `baseline`, got {other:?}"
            );
            return ExitCode::FAILURE;
        }
    };

    // 2. Boot the bug's guest to its readiness marker + seal a mid-workload base.
    //    `boot_server` also returns the boot-to-ready wall-clock (task 96 stopwatch,
    //    hash-neutral) — surfaced here so the box operator sees per-campaign boot
    //    cost. The campaign run + the zero-cell hard-fail below are NOT inside a
    //    timed phase (no PhaseStats report is built on this measurement path), so
    //    there is nothing further to time; the boot phase is the only timed segment.
    let (mut server, boot_us, _serial) =
        match boot_server(&args.kernel, &args.initramfs, &args.ready_marker, false) {
            Ok(t) => t,
            Err(code) => return code,
        };
    println!(
        "[campaign-runner] benchcampaign box: boot-to-ready {} ms (hash-neutral).",
        boot_us / 1_000
    );

    // 3. The box campaign config: fault moments rebased onto the sealed base
    //    (M2 prereq 2), replay bar floored at 25/25 (the flag can only raise it).
    //    The search knobs come from the RECORDED `--explore-period` / `--order-range`
    //    flags (env fallback resolved by clap), and flow into the CampaignLog, so a
    //    same-seed artifact is self-describing (PR#90 round-2 — no ambient env read).
    let mut cfg = BenchConfig::box_campaign(
        args.seed,
        args.max_branches,
        args.replay_n.max(super::REPLAY_BAR),
        args.deadline_delta,
    );
    cfg.explore_period = args.explore_period.max(1);
    cfg.order_range = args.order_range.max(1);
    println!(
        "[campaign-runner] benchcampaign box: bug {} ({}) / {config:?} / seed {} — {} branches, \
         verify {}×, fault-rebase {}, explore-period {}, order-range {}.\n",
        spec.id.0,
        spec.name,
        args.seed,
        cfg.max_branches,
        cfg.replay_n,
        cfg.fault_rebase,
        cfg.explore_period,
        cfg.order_range,
    );

    // 4. Drive the campaign over the wire (SocketMachine → real KVM). The real guest
    //    console rides the `Console` verb into RunTrace.records → the signal.
    let initial = boot_env();
    let spec_run = spec.clone();
    let (served, outcome) = run_session(&mut server, move |stream| {
        let mut machine = SocketMachine::connect(stream, initial)?;
        run_bench_campaign(&mut machine, &spec_run, &cfg, config)
    });
    let outcome = match outcome {
        Ok(o) => o,
        Err(e) => {
            eprintln!(
                "[campaign-runner] benchcampaign box: campaign failed (transport/backend): {e}"
            );
            if let Err(se) = served {
                eprintln!("[campaign-runner] benchcampaign box: server session ended with: {se}");
            }
            return ExitCode::FAILURE;
        }
    };
    if let Err(se) = served {
        eprintln!("[campaign-runner] benchcampaign box: server session ended fatally: {se}");
        return ExitCode::FAILURE;
    }

    // 5. Emit the CampaignLog JSON (the offline `benchmark-report` input).
    match serde_json::to_string_pretty(&outcome.log) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&args.out, json) {
                eprintln!(
                    "[campaign-runner] benchcampaign box: write {}: {e}",
                    args.out.display()
                );
                return ExitCode::FAILURE;
            }
        }
        Err(e) => {
            eprintln!("[campaign-runner] benchcampaign box: serialize CampaignLog: {e}");
            return ExitCode::FAILURE;
        }
    }

    // 5b. Emit the retained re-key substrate (M2 amendment / `docs/SCORING.md`
    // R1/R2): every branch's `RunTrace` in order, so a future `CellFn` candidate can
    // re-key THIS campaign offline (a pure fold over the retained timelines) without
    // re-running it. A first-class M2 deliverable — a write failure FAILS the
    // campaign rather than silently dropping the substrate.
    if let Some(rec) = &args.record {
        match serde_json::to_string(&outcome.traces) {
            Ok(json) => {
                if let Err(e) = std::fs::write(rec, json) {
                    eprintln!(
                        "[campaign-runner] benchcampaign box: write traces {}: {e}",
                        rec.display()
                    );
                    return ExitCode::FAILURE;
                }
                println!(
                    "[campaign-runner] benchcampaign box: retained {} branch traces -> {}.",
                    outcome.traces.len(),
                    rec.display()
                );
            }
            Err(e) => {
                eprintln!("[campaign-runner] benchcampaign box: serialize traces: {e}");
                return ExitCode::FAILURE;
            }
        }
    }

    // The signal GUARDRAIL (user 2026-07-06): the REAL LogSensor/CellFnV1 must
    // actually produce cells on the box path — a signal campaign that makes ZERO
    // cells is measuring nothing, and must NOT be quietly accepted as a valid gate
    // input. Surface the count loudly.
    let distinct_cells: std::collections::BTreeSet<u64> = outcome
        .log
        .events
        .iter()
        .flat_map(|e| e.touched.iter().copied())
        .collect();
    println!(
        "[campaign-runner] benchcampaign box: {} branches logged, {} distinct signal cells, {} certified find(s). Wrote {}.",
        outcome.log.events.len(),
        distinct_cells.len(),
        outcome.certs.len(),
        args.out.display(),
    );
    // The finding certificates: `bug branch state_hash` per certified find — the
    // determinism stress-test compares these solo vs co-tenant (a mismatch is a P0
    // leak). Printed so a wrapper script can diff them across co-tenancy. (Printed
    // BEFORE the zero-cell hard-fail so the evidence — CampaignLog + traces already
    // written above, plus these certs — is preserved even on the guardrail exit.)
    for c in &outcome.certs {
        println!(
            "[campaign-runner] FIND bug {} branch {} state_hash {}",
            c.bug.0,
            c.branch,
            hex32(&c.state_hash),
        );
    }
    // The signal GUARDRAIL — HARD FAIL (Paul's CellFn ruling, 2026-07-06 + the
    // PR#90 round-1 finding): the REAL LogSensor/CellFnV1 must actually produce
    // cells on the box path. A signal campaign that makes ZERO distinct cells is
    // measuring NOTHING (the sensor saw no guest console, so the signal has nothing
    // to steer on) and must NEVER be silently accepted as a valid gate input.
    // Outputs are already written above, so the evidence is preserved for triage,
    // but the campaign EXITS FAILURE so a wrapper/orchestrator cannot fold it into
    // the ruling. (Baseline makes no cells by design, so this fires on Signal only.)
    if matches!(config, Configuration::Signal) && distinct_cells.is_empty() {
        eprintln!(
            "[campaign-runner] benchcampaign box: FATAL — the SIGNAL config produced ZERO cells. The \
             real LogSensor/CellFnV1 saw no guest console, so the signal has nothing to steer on. \
             This is NOT a valid signal campaign — failing hard (fix the console capture / guest \
             logging before re-running); the written log MUST NOT be used in the ruling."
        );
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
