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

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use conductor::campaign::{
    CampaignConfig, CampaignReport, render_campaign_table, run_campaign, verify_campaign,
};
use conductor::planted::{ToyPlantedMachine, Trigger};
use conductor::{SweepConfig, render_table, run_session, sweep_client, verify};
use environment::{EnvSpec, FaultPolicy};
use explorer::SpecEnvCodec;

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

/// Shared campaign knobs (both modes).
#[derive(Parser)]
struct CampaignArgs {
    /// Branch budget: give up **loudly** if the planted bug is not found within
    /// this many branches (a no-find is a gate failure, never a silent pass).
    #[arg(long, default_value_t = 4096)]
    max_branches: u64,
    /// Replays of the emitted reproducer to prove bit-identical reproduction —
    /// the milestone bar is 25.
    #[arg(long, default_value_t = 25)]
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
    #[arg(long, default_value_t = 5_000_000_000)]
    deadline_delta: u64,
    /// Lowest candidate guest-physical fault address (page-aligned).
    #[arg(long, default_value_t = 0x0100_0000)]
    gpa_base: u64,
    /// Number of page-strided candidate addresses.
    #[arg(long, default_value_t = 256)]
    gpa_count: u64,
    /// Stride between candidate addresses (default one 4 KiB page).
    #[arg(long, default_value_t = 0x1000)]
    gpa_stride: u64,
    /// Lowest fault-Moment offset past the base V-time (ns).
    #[arg(long, default_value_t = 0)]
    window_lo: u64,
    /// One past the highest fault-Moment offset past the base V-time (ns).
    #[arg(long, default_value_t = 2_000_000_000)]
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
    }
}

/// The portable campaign (task 60, gate 2): the seed-driven search over the toy
/// planted-bug machine, the emit-and-verify N/N step, and the nominal control —
/// the identical [`run_campaign`] loop the box milestone drives.
fn run_campaign_mock(args: CampaignArgs) -> ExitCode {
    let cfg = CampaignConfig {
        max_branches: args.max_branches,
        replay_n: args.replay_n.max(1),
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

/// The portable demo: scripted mock guest, sweep, table, verdicts.
fn run_mock(args: SweepArgs) -> ExitCode {
    if !seeds_ok(args.seeds, 2) {
        return ExitCode::FAILURE;
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

    use conductor::campaign::{CampaignConfig, run_campaign};
    use conductor::{SweepConfig, run_session, sweep_client};
    use environment::{EnvSpec, FaultPolicy};
    use explorer::SpecEnvCodec;
    use explorer::adapter::SocketMachine;
    use vmm_backend::Backend;
    use vmm_core::bringup::{BackendKind, boot_linux_selected};
    use vmm_core::control::{ControlServer, VmmFactory};
    use vmm_core::vmm::{Step, Vmm};

    use super::{BoxArgs, CampaignBoxArgs, finish, finish_campaign, seeds};

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

        let cfg = SweepConfig {
            seeds: seeds(args.sweep.seeds),
            runs_per_seed: args.sweep.runs.max(2),
            deadline_delta: Some(args.deadline_delta),
            // Postgres is interrupt-driven; the snapshot search may need many
            // steps to find a sealable boundary at/after readiness. Generous
            // retry budget (task 41 made mid-workload points snapshottable).
            snapshot_retry_step: 1_000_000,
            snapshot_max_attempts: 100_000,
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
            replay_n: args.campaign.replay_n.max(1),
            deadline_delta: Some(args.deadline_delta),
            gpa_candidates,
            moment_window: (args.window_lo, args.window_hi),
            // Single-event upsets on byte boundaries (the naive upset alphabet).
            mask_bits: vec![7, 15, 23, 31, 39, 47, 55, 63],
            snapshot_retry_step: 1_000_000,
            snapshot_max_attempts: 100_000,
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
            run_campaign(&mut machine, &SpecEnvCodec, &cfg)
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
