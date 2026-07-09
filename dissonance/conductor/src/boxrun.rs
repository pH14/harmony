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
use conductor::campaign::{CampaignConfig, run_campaign as run_campaign_loop};
use conductor::record::{RecordConfig, run_recording};
use conductor::stopwatch::{Phase, PhaseStats, mark};
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
/// `conductor::stopwatch`'s module doc), or a failing [`ExitCode`] with a
/// loud reason (never a vacuous success). Shared verbatim by the sweep
/// ([`run`](run)) and the campaign ([`run_campaign`](run_campaign)) so both
/// boot the guest identically.
fn boot_server(
    kernel_name: &str,
    initramfs_name: &str,
    ready_marker: &str,
) -> Result<(ControlServer<Box<dyn Backend>>, u64), ExitCode> {
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
    let (Some(kernel), Some(initramfs)) = (artifact(kernel_name), artifact(initramfs_name)) else {
        eprintln!(
            "[conductor] guest image missing ({kernel_name} / {initramfs_name}) — build it \
             first: `make -C guest fetch && make -C guest/linux campaign-image` (or \
             `postgres-image`), or pass --initramfs for an image already on the box."
        );
        return Err(ExitCode::FAILURE);
    };

    // "Boot" starts here (task 96 §4: "from server boot start to the readiness
    // marker") — before `boot_linux_selected`, so the phase covers KVM backend
    // creation + guest RAM load + the initial restore, not just the
    // post-boot drive to the marker.
    let boot_t0 = mark();
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
    let boot_us = match drive_to_marker(&mut live, ready_marker.as_bytes()) {
        Ok(steps) => {
            let boot_us = boot_t0.elapsed_us();
            println!(
                "\n[conductor] readiness marker reached at step {steps}; the base snapshot \
                 will be sealed at the next snapshottable boundary at/after this point, wall \
                 {}s.\n",
                boot_us / 1_000_000
            );
            boot_us
        }
        Err(e) => {
            eprintln!("\n[conductor] failed to reach the readiness marker: {e}");
            return Err(ExitCode::FAILURE);
        }
    };

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
    Ok((ControlServer::new(live, factory), boot_us))
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
    // `SweepReport` carries no timing (task 96 only extends `CampaignReport`)
    // — the boot duration is already in the printed readiness line above.
    let (mut server, _boot_us) =
        match boot_server(&args.kernel, &args.initramfs, &args.ready_marker) {
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
    let (mut server, boot_us) = match boot_server(&args.kernel, &args.initramfs, &args.ready_marker)
    {
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
    let mut report = match client {
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
    // Fold the boot-to-ready measurement (task 96) into the report's `Boot`
    // phase: it happened before `run_campaign_loop`'s own `Stopwatch` existed
    // (boot brackets `boot_server`, well outside the campaign loop), so it
    // cannot go through `Stopwatch::time` — merged in here instead.
    report
        .timing
        .insert(Phase::Boot, PhaseStats::single(boot_us));
    finish_campaign("box", &report, n)
}
