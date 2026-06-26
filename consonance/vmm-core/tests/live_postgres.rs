// SPDX-License-Identifier: AGPL-3.0-or-later
//! Box-only **bare-Postgres workload** gates (`#[cfg(target_os = "linux")]` **and
//! `#[ignore]`**, on `ssh <det-box>` with the LOADED patched KVM modules,
//! CPU-pinned per `docs/BOX-PINNING.md`). Task 37 — consonance workload stream,
//! step 2 of 3.
//!
//! These boot the **Postgres workload image** (`guest/build/bzImage` — the task-36
//! container-class kernel, unchanged — plus `guest/build/initramfs-postgres.cpio.gz`,
//! built by `guest/linux/build-postgres-image.sh`) via
//! [`vmm_core::bringup::boot_linux_selected`]. The guest `/init` (`pg-init.sh`)
//! loop-mounts a RAM-backed ext4 holding a pre-`initdb`'d cluster, starts a real
//! PostgreSQL 17 server, and drives a fixed insert/select workload loop whose
//! per-iteration query results stream to `ttyS0` interleaved with postgres' own
//! stdout/stderr.
//!
//! **Gate 1 — Postgres runs + streams (`[p1_postgres_runs_and_streams_patched`]).**
//! One patched boot must start postgres, execute the workload (the streamed
//! `row|…` aggregate lines + `database system is ready to accept connections`
//! appear on the serial), reach `GUEST_READY`, and power off cleanly within budget.
//!
//! **Gate 2 — deterministic twice (the milestone,
//! [`p2_postgres_deterministic_twice_patched`]).** Two same-seed patched boots must
//! produce a **bit-identical** serial capture (including the query output) **and**
//! `state_hash`. This is the headline: a sophisticated, real, stateful server —
//! multiprocess postmaster + background workers, WAL, fsync, `pg_strong_random`
//! cancel keys — runs bit-for-bit identically because every nondeterminism source
//! (TSC, RNG, fork order, timers) is determinized from below by the patched backend
//! + V-time. See `guest/linux/IMPLEMENTATION.md` for the determinism closure.
//!
//! **Why patched, not stock.** Postgres needs a live periodic tick (background
//! workers, timed waits) and the 8250 TX must drain to stream output — both ride
//! the V-time LAPIC timer, which only advances on the patched backend (in-guest
//! RDTSC traps to V-time). On stock KVM the timer never calibrates (task 30/34), so
//! the workload never streams. Both gates therefore run patched.
//!
//! **Gate honesty (why `#[ignore]`).** These need real + patched KVM, the built
//! Postgres image, and the `det-cfl-v1` host — none in the default `cargo nextest`
//! lane — so they are `#[ignore]`d (like `live_linux_boot.rs`); default CI shows
//! them not-run, never a vacuous green. Every missing precondition is a loud panic.
//! macOS builds an empty test binary. Run on the box (build the image first), with
//! the patched modules loaded, CPU-pinned and wall-clock-bounded — e.g.:
//!
//! ```sh
//! make -C guest fetch && make -C guest/linux postgres-image    # build the image
//! # load patched kvm.ko/kvm-intel.ko, then:
//! taskset -c 2 timeout 1500 cargo test -p vmm-core --test live_postgres \
//!     -- --ignored --nocapture --test-threads=1 p2_postgres_deterministic_twice_patched
//! # always revert to stock KVM afterwards and verify `lsmod | grep '^kvm '` == 1396736.
//! ```
#![cfg(target_os = "linux")]

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use vmm_core::bringup::{BackendKind, boot_linux_selected};
use vmm_core::vmm::{Step, TerminalReason, Vmm};

/// 2 GiB of guest RAM: room for the unpacked Postgres rootfs (busybox + the
/// PostgreSQL install + zoneinfo + locale) + the RAM-backed ext4 PGDATA image +
/// postgres' shared memory and per-backend processes.
const GUEST_RAM_LEN: usize = 2 << 30;
/// The pinned determinism seed (same shape as the corpus / `live_linux_boot` seed).
const SEED: u64 = 0x0028_C0FF_EE5E_EDC0;
/// The determinism command line. Shares the `live_linux_boot` base (see that file's
/// doc comment for `tsc=reliable`/`no_timer_check`/`lpj=`/`nokaslr`/`nosmp`/
/// `maxcpus=1`/`nox2apic`/`hpet=disable`), with two task-37 changes:
///   * **`random.trust_cpu=off` is dropped.** Under deterministic V-time there is
///     no entropy jitter, so with the CPU RNG distrusted the kernel CRNG never
///     initializes and postgres' first blocking `getrandom` (pg_strong_random)
///     hangs. Trusting the (patched-backend-trapped, seeded) RDRAND/RDSEED seeds
///     the CRNG deterministically — the same seeded stream the contract specifies.
///   * **`reboot=t` → `reboot=t,force`.** A plain poweroff strands in the kernel's
///     device_shutdown once block I/O has been used; `reboot=force` skips it and
///     the triple-fault (`t`) becomes a clean KVM_EXIT_SHUTDOWN terminal.
const DEFAULT_CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t,force tsc=reliable \
     no_timer_check lpj=4000000 nokaslr nosmp maxcpus=1 nox2apic hpet=disable";
/// Step budget: a high cap so a stuck guest cannot run forever (the heavy postgres
/// boot+workload is bounded by the wall budget + the external `timeout`).
const MAX_STEPS: u64 = 50_000_000_000;
/// Wall-clock budget inside the test. The consonance VMM services every VM-exit in
/// Rust and traps every in-guest RDTSC, so a full postgres bring-up + workload is
/// far heavier than the minimal `GUEST_READY` boot; this is a deliberate milestone
/// gate, run with a matching external `timeout`.
const WALL_BUDGET: Duration = Duration::from_secs(1200);

/// postgres announces this once the cluster is accepting connections.
const PG_READY: &[u8] = b"database system is ready to accept connections";
/// The workload loop's end marker (printed by `pg-init.sh`).
const WORKLOAD_END: &[u8] = b"PG37: workload end";
/// The final workload row: iteration 20, v = 20*20+7 = 407, count = 20, running
/// sum = 3010. A pure function of the loop index — pinning it proves the *query
/// results* (not just "postgres ran") reached the serial.
const FINAL_ROW: &[u8] = b"row|20|407|20|3010";
/// `pg-init.sh` prints this after a clean shutdown.
const GUEST_READY: &[u8] = b"GUEST_READY";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// Read a built guest artifact, trying `guest/build/<name>` then `guest/linux/<name>`.
/// Panics loudly (with the build command) if absent — these `#[ignore]`d gates run
/// only on the box, where the image is built first.
fn require_artifact(name: &str) -> Vec<u8> {
    for p in [
        repo_root().join("guest/build").join(name),
        repo_root().join("guest/linux").join(name),
    ] {
        if let Ok(bytes) = std::fs::read(&p) {
            return bytes;
        }
    }
    panic!(
        "guest artifact `{name}` not found in guest/build or guest/linux — build it first on the \
         box: `make -C guest fetch && make -C guest/linux postgres-image`."
    );
}

fn require_kvm() {
    assert!(
        std::path::Path::new("/dev/kvm").exists(),
        "/dev/kvm absent — run this `#[ignore]`d box gate on `ssh <det-box>` with the LOADED \
         patched KVM modules, CPU-pinned per docs/BOX-PINNING.md."
    );
}

/// Require the §1.1 `det-cfl-v1` host baseline, else **panic** with the report
/// (`boot_linux` would also refuse such a host).
fn require_host_baseline() {
    let report = vmm_core::hostassert::report();
    let mut all = true;
    eprintln!("[host-assert] CPU-MSR-CONTRACT §1.1 baseline:");
    for o in &report {
        eprintln!(
            "[host-assert]   {}  {}: expected {}, observed {}",
            if o.pass { "PASS" } else { "FAIL" },
            o.key,
            o.expected,
            o.actual
        );
        all &= o.pass;
    }
    assert!(
        all,
        "host CPU is not the det-cfl-v1 baseline — boot_linux cannot run the frozen contract here. \
         Run on the determinism box (i9-9900K) per docs/BOX-PINNING.md."
    );
}

fn cmdline() -> String {
    std::env::var("BOOT_CMDLINE").unwrap_or_else(|_| DEFAULT_CMDLINE.to_string())
}

fn find(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// What a bounded run observed.
struct BootOutcome {
    reason: Option<TerminalReason>,
    steps: u64,
    pg_ready: bool,
    workload_done: bool,
    final_row: bool,
    guest_ready: bool,
    step_error: Option<String>,
}

impl BootOutcome {
    fn terminated_cleanly(&self) -> bool {
        self.reason.is_some() && self.step_error.is_none()
    }
}

/// Drive `vmm` to a terminal state (or the step / wall-clock budget), streaming the
/// serial console to stderr as it is captured so the boot log is visible live and a
/// hang shows the last line reached.
fn run_bounded<B: vmm_backend::Backend>(vmm: &mut Vmm<B>) -> BootOutcome {
    // not order-observable: a test-only wall-clock watchdog (belt-and-braces with
    // the external `timeout`) — it bounds how long this `#[ignore]`d box gate runs
    // and never reaches guest state, the serial capture, or any hash.
    #[allow(clippy::disallowed_methods)]
    let start = Instant::now();
    let mut printed = 0usize;
    let mut steps = 0u64;
    let mut reason = None;
    let mut step_error = None;
    let stderr = std::io::stderr();
    while steps < MAX_STEPS {
        match vmm.step() {
            Ok(Step::Continued) => {}
            Ok(Step::Terminal(r)) => {
                reason = Some(r);
                break;
            }
            Err(e) => {
                eprintln!("\n[pg] step error after {steps} steps: {e}  | debug={e:?}");
                let mut msg = format!("{e}");
                let mut src = std::error::Error::source(&e);
                while let Some(s) = src {
                    msg.push_str(&format!(" | {s}"));
                    src = s.source();
                }
                step_error = Some(msg);
                break;
            }
        }
        steps += 1;
        let serial = vmm.serial();
        if serial.len() > printed {
            let mut h = stderr.lock();
            let _ = h.write_all(&serial[printed..]);
            let _ = h.flush();
            printed = serial.len();
        }
        if steps.is_multiple_of(8192) && start.elapsed() > WALL_BUDGET {
            eprintln!("\n[pg] wall-clock budget exceeded after {steps} steps");
            break;
        }
    }
    let serial = vmm.serial();
    BootOutcome {
        reason,
        steps,
        pg_ready: find(serial, PG_READY),
        workload_done: find(serial, WORKLOAD_END),
        final_row: find(serial, FINAL_ROW),
        guest_ready: find(serial, GUEST_READY),
        step_error,
    }
}

/// Boot the Postgres image on the patched backend at `seed`, run it to a terminal,
/// and return (serial capture, `state_hash`, outcome). The [`Vmm`] — and with it
/// the `perf_event` work counter that drives V-time — is **dropped before
/// returning**. That matters for the deterministic-twice gate: keeping the first
/// run's `Vmm` alive while a second boots in the same process leaves two pinned PMU
/// counters open, which multiplex and perturb the branch count → a few-step V-time
/// skid → a divergent printk timestamp. One counter at a time keeps it exact.
fn boot_pg(seed: u64) -> (Vec<u8>, [u8; 32], BootOutcome) {
    let kernel = require_artifact("bzImage");
    let initramfs = require_artifact("initramfs-postgres.cpio.gz");
    let cmdline = cmdline();
    let mut vmm = boot_linux_selected(
        BackendKind::Patched,
        &kernel,
        &initramfs,
        GUEST_RAM_LEN,
        &cmdline,
        seed,
    )
    .expect("boot_linux_selected (patched) — needs the LOADED patched KVM modules");
    let out = run_bounded(&mut vmm);
    (vmm.serial().to_vec(), vmm.state_hash(), out)
}

fn report(tag: &str, out: &BootOutcome) {
    eprintln!(
        "\n[{tag}] steps={} terminal={:?} pg_ready={} workload_done={} final_row={} \
         GUEST_READY={} step_error={:?}",
        out.steps,
        out.reason,
        out.pg_ready,
        out.workload_done,
        out.final_row,
        out.guest_ready,
        out.step_error,
    );
}

// --- Gate 1: Postgres runs + streams (patched) -----------------------------

/// **Gate 1 — Postgres runs and streams.** One patched boot starts postgres,
/// executes the workload, streams the query results + postgres' stdout/stderr to
/// `ttyS0`, and powers off cleanly within budget.
#[test]
#[ignore = "box-only live gate (LOADED patched KVM + built Postgres image + det-cfl-v1 host); \
            run on `ssh <det-box>` with `-- --ignored --nocapture`"]
fn p1_postgres_runs_and_streams_patched() {
    require_kvm();
    require_host_baseline();
    eprintln!("[pg] cmdline: {}", cmdline());
    let (_serial, _hash, out) = boot_pg(SEED);
    report("p1", &out);
    assert!(
        out.step_error.is_none(),
        "Gate 1: the VMM must not trip a contract violation during the run — got {:?} after {} steps",
        out.step_error,
        out.steps,
    );
    assert!(
        out.reason.is_some(),
        "Gate 1: must reach a terminal, not hang ({} steps)",
        out.steps
    );
    assert!(
        out.pg_ready,
        "Gate 1: postgres must announce it is ready to accept connections"
    );
    assert!(
        out.workload_done,
        "Gate 1: the workload loop must run to completion (PG37: workload end)"
    );
    assert!(
        out.final_row,
        "Gate 1: the final workload row (row|20|407|20|3010) must reach the serial"
    );
    assert!(
        out.guest_ready,
        "Gate 1: the guest must announce GUEST_READY after a clean shutdown"
    );
    assert!(
        out.terminated_cleanly(),
        "Gate 1: the guest must power off cleanly within budget"
    );
}

// --- Gate 2: deterministic twice (the milestone) ---------------------------

/// **Gate 2 — deterministic twice (the milestone).** Two same-seed patched boots
/// produce a bit-identical serial capture (including the query output) **and**
/// `state_hash`.
#[test]
#[ignore = "MILESTONE gate (task 37): same-seed bit-identical Postgres run; run on the box with \
            the LOADED patched KVM and `-- --ignored --nocapture`"]
fn p2_postgres_deterministic_twice_patched() {
    require_kvm();
    require_host_baseline();

    // boot_pg drops run A's Vmm (and its PMU counter) before we boot run B.
    let (serial_a, hash_a, out_a) = boot_pg(SEED);
    report("p2 run A", &out_a);
    let (serial_b, hash_b, out_b) = boot_pg(SEED);
    report("p2 run B", &out_b);

    let hex = |h: &[u8; 32]| h.iter().map(|b| format!("{b:02x}")).collect::<String>();
    eprintln!(
        "[pg] determinism: serial_len A/B = {}/{}\n  state_hash A = {}\n  state_hash B = {}",
        serial_a.len(),
        serial_b.len(),
        hex(&hash_a),
        hex(&hash_b),
    );

    // Both runs must actually have run the workload to GUEST_READY, so two
    // identical-but-stranded boots cannot pass this gate vacuously.
    for (tag, out) in [("A", &out_a), ("B", &out_b)] {
        assert!(
            out.step_error.is_none(),
            "Gate 2 run {tag}: contract violation: {:?}",
            out.step_error
        );
        assert!(
            out.final_row,
            "Gate 2 run {tag}: the workload's final row must reach the serial"
        );
        assert!(out.guest_ready, "Gate 2 run {tag}: must reach GUEST_READY");
    }
    assert_eq!(
        serial_a, serial_b,
        "Gate 2: two same-seed patched boots must produce a bit-identical serial capture"
    );
    assert_eq!(
        hash_a, hash_b,
        "Gate 2: two same-seed patched boots must produce an identical state_hash"
    );
}
