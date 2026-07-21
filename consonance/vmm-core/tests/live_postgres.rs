// SPDX-License-Identifier: AGPL-3.0-or-later
//! Box-only **bare-Postgres workload** gates (`#[cfg(target_os = "linux")]` **and
//! `#[ignore]`**, on `ssh <det-box>` with the LOADED patched KVM modules,
//! CPU-pinned per `docs/BOX-PINNING.md`). Task 37 — consonance workload stream,
//! step 2 of 3.
//!
//! These boot the **Postgres workload image** (`harmony-linux/build/bzImage` — the task-36
//! container-class kernel, unchanged — plus `harmony-linux/build/initramfs-postgres.cpio.gz`,
//! built by `harmony-linux/linux/build-postgres-image.sh`) via
//! [`vmm_core::vendor::x86::bringup::boot_linux_selected`]. The guest `/init` (`pg-init.sh`)
//! loop-mounts a RAM-backed ext4 holding a pre-`initdb`'d cluster, starts a real
//! PostgreSQL 17 server, and drives a fixed insert/select workload loop whose
//! per-iteration query results stream to `ttyS0` interleaved with postgres' own
//! stdout/stderr.
//!
//! **Workload v2 (task 42).** Each row now carries a `gen_random_uuid()` id and a
//! `clock_timestamp()` wall-clock column, streamed as `row|i|count|sum|uuid|t`.
//! These *look* nondeterministic — a random UUID, a per-call wall-clock time — but
//! must come out **bit-identical** across two same-seed runs: `gen_random_uuid()`
//! draws from `pg_strong_random` → the seeded CRNG, and `clock_timestamp()` reads
//! the V-time-driven clock. The `count`/`sum` prefix stays a pure function of the
//! loop index (the deterministic anchor [`FINAL_ROW_PREFIX`] = `row|20|20|210|`),
//! while the uuid + t are seed-derived (deterministic but not predictable), so the
//! gates check them by *shape* and prove seed-sensitivity at a different seed. If a
//! UUID or timestamp differed across same-seed runs, the determinism would have
//! escaped and Gate 2 would fail — a real determinization finding, not papered over.
//!
//! **Gate 1 — Postgres runs + streams (`[p1_postgres_runs_and_streams_patched`]).**
//! One patched boot must start postgres, execute the workload (the streamed
//! `row|…` aggregate lines + `database system is ready to accept connections`
//! appear on the serial, each row bearing a **valid UUID + timestamp**, all 20 UUIDs
//! distinct), reach `GUEST_READY`, and power off cleanly within budget.
//!
//! **Gate 2 — deterministic twice (the milestone,
//! [`p2_postgres_deterministic_twice_patched`]).** Two same-seed patched boots must
//! produce a **bit-identical** serial capture (including the UUIDs + timestamps)
//! **and** `state_hash`. This is the headline: a sophisticated, real, stateful
//! server — multiprocess postmaster + background workers, WAL, fsync,
//! `pg_strong_random` cancel keys, *random UUIDs and wall-clock timestamps* — runs
//! bit-for-bit identically because every nondeterminism source (TSC, RNG, fork
//! order, timers, the clock) is determinized from below by the patched backend +
//! V-time. See `harmony-linux/linux/IMPLEMENTATION.md` for the determinism closure.
//!
//! **Gate 3 — seed-sensitivity ([`p3_postgres_seed_sensitivity_patched`]).** A run
//! at a *different* seed produces *different* UUIDs — proving they are genuinely
//! seed-driven (the seeded CRNG), not a frozen constant that would pass Gate 2
//! vacuously. The two seeds' sample UUIDs are quoted.
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
//! make -C harmony-linux fetch && make -C harmony-linux/linux postgres-image    # build the image
//! # load patched kvm.ko/kvm-intel.ko, then:
//! taskset -c 2 timeout 1500 cargo test -p vmm-core --test live_postgres \
//!     -- --ignored --nocapture --test-threads=1 p2_postgres_deterministic_twice_patched
//! # always revert to stock KVM afterwards and verify `lsmod | grep '^kvm '` == 1396736.
//! ```
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use vmm_core::vendor::x86::bringup::{BackendKind, boot_linux_selected};
use vmm_core::vmm::{Step, TerminalReason, Vmm};

/// 2 GiB of guest RAM: room for the unpacked Postgres rootfs (busybox + the
/// PostgreSQL install + zoneinfo + locale) + the RAM-backed ext4 PGDATA image +
/// postgres' shared memory and per-backend processes.
const GUEST_RAM_LEN: usize = 2 << 30;
/// The pinned determinism seed (same shape as the corpus / `live_linux_boot` seed).
const SEED: u64 = 0x0028_C0FF_EE5E_EDC0;
/// A *different* determinism seed for the seed-sensitivity gate (Gate 3). Because
/// `gen_random_uuid()` draws from the seeded CRNG, a different seed must yield
/// different UUIDs. Well-mixed away from [`SEED`] (XOR the golden-ratio constant) so
/// the entropy stream is unambiguously distinct, not a one-bit neighbor.
const SEED_B: u64 = SEED ^ 0x9E37_79B9_7F4A_7C15;
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
/// The deterministic prefix of the final workload row (iteration 20): the `row`
/// marker, loop index 20, running `count(*)` = 20, running `sum(i)` = 1+…+20 = 210.
/// This prefix is a pure function of the loop index, so matching it proves the
/// *query results* (not just "postgres ran") reached the serial. The `uuid|t` that
/// FOLLOW this prefix in the streamed line (`row|20|20|210|<uuid>|<t>`) are
/// seed-derived — checked by shape ([`is_uuid`]/[`is_timestamp`]), not by value.
const FINAL_ROW_PREFIX: &[u8] = b"row|20|20|210|";
/// `pg-init.sh` prints this after a clean shutdown.
const GUEST_READY: &[u8] = b"GUEST_READY";

/// The fixed iteration count of the workload loop (`WORKLOAD_N` in
/// `build-postgres-image.sh`): every run streams exactly this many `row|…` lines,
/// each with its own distinct `gen_random_uuid()`.
const WORKLOAD_N: usize = 20;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// Read a built guest artifact, trying `harmony-linux/build/<name>` then `harmony-linux/linux/<name>`.
/// Panics loudly (with the build command) if absent — these `#[ignore]`d gates run
/// only on the box, where the image is built first.
fn require_artifact(name: &str) -> Vec<u8> {
    for p in [
        repo_root().join("harmony-linux/build").join(name),
        repo_root().join("harmony-linux/linux").join(name),
    ] {
        if let Ok(bytes) = std::fs::read(&p) {
            return bytes;
        }
    }
    panic!(
        "guest artifact `{name}` not found in harmony-linux/build or harmony-linux/linux — build it first on the \
         box: `make -C harmony-linux fetch && make -C harmony-linux/linux postgres-image`."
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
    let report = vmm_core::vendor::x86::hostassert::report();
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

/// `true` iff `s` is a textual UUID — 36 chars, `8-4-4-4-12` hex with hyphens at the
/// canonical offsets (e.g. `2f681fb6-1c0a-4d4e-9b8e-0c7b3a9f1e22`). A lightweight
/// shape check (no `uuid` crate): it proves the streamed field is a real UUID, not a
/// constant placeholder or an error string.
fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 36 {
        return false;
    }
    b.iter().enumerate().all(|(i, &c)| match i {
        8 | 13 | 18 | 23 => c == b'-',
        _ => c.is_ascii_hexdigit(),
    })
}

/// `true` iff `s` opens with an ISO `YYYY-MM-DD HH:MM:SS` timestamp (postgres
/// `timestamptz` text form, e.g. `2026-06-26 12:34:56.789012+00`). A lightweight
/// shape check that the streamed `clock_timestamp()` field is a real timestamp, not a
/// constant or an error.
fn is_timestamp(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 19 {
        return false;
    }
    let d = |i: usize| b[i].is_ascii_digit();
    d(0) && d(1)
        && d(2)
        && d(3)
        && b[4] == b'-'
        && d(5)
        && d(6)
        && b[7] == b'-'
        && d(8)
        && d(9)
        && b[10] == b' '
        && d(11)
        && d(12)
        && b[13] == b':'
        && d(14)
        && d(15)
        && b[16] == b':'
        && d(17)
        && d(18)
}

/// The streamed line that begins with `prefix`, from the prefix to the next newline
/// (trailing `\r` trimmed), as UTF-8. `None` if the prefix never appears.
fn line_with_prefix<'a>(serial: &'a [u8], prefix: &[u8]) -> Option<&'a str> {
    let start = serial.windows(prefix.len()).position(|w| w == prefix)?;
    let rest = &serial[start..];
    let end = rest.iter().position(|&b| b == b'\n').unwrap_or(rest.len());
    std::str::from_utf8(&rest[..end])
        .ok()
        .map(|s| s.trim_end_matches('\r'))
}

/// Parse the final workload row (`row|20|20|210|<uuid>|<t>`): return its `(uuid, t)`
/// fields as owned strings. `None` if the row is absent or malformed.
fn final_row_uuid_ts(serial: &[u8]) -> Option<(String, String)> {
    let line = line_with_prefix(serial, FINAL_ROW_PREFIX)?;
    let fields: Vec<&str> = line.split('|').collect();
    // row | i | count | sum | uuid | t
    if fields.len() != 6 {
        return None;
    }
    Some((fields[4].to_string(), fields[5].to_string()))
}

/// Every per-iteration row's UUID (field 5 of each `row|…` line that parses as a
/// UUID). Used to prove the UUIDs are not a frozen constant *within* a run — all
/// [`WORKLOAD_N`] of them must be distinct.
fn all_row_uuids(serial: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(serial)
        .lines()
        .filter(|l| l.starts_with("row|"))
        .filter_map(|l| l.split('|').nth(4).map(str::to_string))
        .filter(|u| is_uuid(u))
        .collect()
}

/// What a bounded run observed.
struct BootOutcome {
    reason: Option<TerminalReason>,
    steps: u64,
    pg_ready: bool,
    workload_done: bool,
    /// The deterministic final-row prefix `row|20|20|210|` reached the serial.
    final_row: bool,
    /// The final row's seed-derived `(uuid, t)` fields, if it was streamed + parsed.
    sample_uuid_ts: Option<(String, String)>,
    /// Every per-iteration UUID streamed (for the distinctness / not-a-constant check).
    row_uuids: Vec<String>,
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
fn run_bounded<B: vmm_backend::Backend<A = vmm_backend::X86>>(vmm: &mut Vmm<B>) -> BootOutcome {
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
            // A cooperating-SDK stop (task 73) is a terminal here — mirror
            // vmm.rs's own run loop, which maps it to `TerminalReason::SdkStop`.
            Ok(Step::SdkStop) => {
                reason = Some(TerminalReason::SdkStop);
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
        final_row: find(serial, FINAL_ROW_PREFIX),
        sample_uuid_ts: final_row_uuid_ts(serial),
        row_uuids: all_row_uuids(serial),
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
         uuids={} GUEST_READY={} step_error={:?}",
        out.steps,
        out.reason,
        out.pg_ready,
        out.workload_done,
        out.final_row,
        out.row_uuids.len(),
        out.guest_ready,
        out.step_error,
    );
    if let Some((uuid, t)) = &out.sample_uuid_ts {
        eprintln!("[{tag}] final-row sample: uuid={uuid} t={t}");
    }
}

/// Assert the workload's UUID/time columns are *well-formed* in `out`: the final row
/// carries a valid UUID + timestamp, all [`WORKLOAD_N`] per-iteration UUIDs reached
/// the serial, and they are pairwise distinct (so the column is not a frozen constant
/// *within* a run). Returns the final row's sample UUID, for quoting + the cross-seed
/// comparison Gate 3 makes. Panics (loud failure) on any malformed/missing field.
fn assert_uuid_time_shape(tag: &str, out: &BootOutcome) -> String {
    let (uuid, t) = out.sample_uuid_ts.clone().unwrap_or_else(|| {
        panic!("{tag}: the final workload row (row|20|20|210|<uuid>|<t>) must reach the serial")
    });
    assert!(
        is_uuid(&uuid),
        "{tag}: final-row id `{uuid}` is not a valid UUID (gen_random_uuid escaped or errored)"
    );
    assert!(
        is_timestamp(&t),
        "{tag}: final-row t `{t}` is not a valid timestamp (clock_timestamp escaped or errored)"
    );
    assert_eq!(
        out.row_uuids.len(),
        WORKLOAD_N,
        "{tag}: expected {WORKLOAD_N} per-iteration UUIDs on the serial, saw {}",
        out.row_uuids.len()
    );
    let mut distinct = out.row_uuids.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        WORKLOAD_N,
        "{tag}: the {WORKLOAD_N} streamed UUIDs must be pairwise distinct (a frozen constant would \
         collapse them) — only {} were distinct",
        distinct.len()
    );
    uuid
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
        "Gate 1: the deterministic final workload row (row|20|20|210|…) must reach the serial"
    );
    // Shape (task 42): the streamed rows carry a valid UUID + timestamp, not a
    // constant/error, and the per-iteration UUIDs are all distinct.
    let sample = assert_uuid_time_shape("Gate 1", &out);
    eprintln!("[p1] sample UUID (seed {SEED:#018x}): {sample}");
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
    // identical-but-stranded boots cannot pass this gate vacuously — and each must
    // carry well-formed, distinct UUIDs + timestamps (so the bit-identity below is
    // over *real* random/wall-clock columns, not a constant or an error string).
    for (tag, out) in [("A", &out_a), ("B", &out_b)] {
        assert!(
            out.step_error.is_none(),
            "Gate 2 run {tag}: contract violation: {:?}",
            out.step_error
        );
        assert!(
            out.final_row,
            "Gate 2 run {tag}: the deterministic final workload row (row|20|20|210|…) must reach \
             the serial"
        );
        assert!(out.guest_ready, "Gate 2 run {tag}: must reach GUEST_READY");
        assert_uuid_time_shape(&format!("Gate 2 run {tag}"), out);
    }
    assert_eq!(
        serial_a, serial_b,
        "Gate 2: two same-seed patched boots must produce a bit-identical serial capture \
         (this is what proves the UUIDs + timestamps are bit-identical across same-seed runs — \
         if a gen_random_uuid()/clock_timestamp() escaped V-time/the seeded CRNG, the serials \
         would differ here: a real determinization finding, not a flake)"
    );
    assert_eq!(
        hash_a, hash_b,
        "Gate 2: two same-seed patched boots must produce an identical state_hash"
    );

    // The headline witness: the UUIDs + timestamps that LOOK random/wall-clock came
    // out bit-identical across the two same-seed runs. Quote a sample for the record.
    let (uuid, t) = out_a
        .sample_uuid_ts
        .clone()
        .expect("final row parsed (asserted above)");
    eprintln!(
        "[pg] deterministic-twice witness (seed {SEED:#018x}): the random-looking UUID + \
         wall-clock timestamp are bit-identical in run A and run B —\n  uuid = {uuid}\n  t    = {t}"
    );
}

// --- Gate 3: seed-sensitivity ----------------------------------------------

/// **Gate 3 — seed-sensitivity.** A run at [`SEED`] and a run at a *different* seed
/// [`SEED_B`] must produce *different* UUIDs — proving the UUIDs are genuinely driven
/// by the seeded CRNG (`gen_random_uuid()` → `pg_strong_random`), not a frozen
/// constant that would sail through Gate 2 vacuously. Both sample UUIDs are quoted.
#[test]
#[ignore = "box-only seed-sensitivity gate (task 42): different seed ⇒ different UUIDs; run on the \
            box with the LOADED patched KVM and `-- --ignored --nocapture`"]
fn p3_postgres_seed_sensitivity_patched() {
    require_kvm();
    require_host_baseline();

    // boot_pg drops each run's Vmm (and its PMU counter) before the next boots.
    let (_serial_a, _hash_a, out_a) = boot_pg(SEED);
    report("p3 seed A", &out_a);
    let (_serial_b, _hash_b, out_b) = boot_pg(SEED_B);
    report("p3 seed B", &out_b);

    let uuid_a = assert_uuid_time_shape(&format!("Gate 3 seed A ({SEED:#018x})"), &out_a);
    let uuid_b = assert_uuid_time_shape(&format!("Gate 3 seed B ({SEED_B:#018x})"), &out_b);
    eprintln!(
        "[pg] seed-sensitivity: sample UUID per seed —\n  seed {SEED:#018x} -> {uuid_a}\n  \
         seed {SEED_B:#018x} -> {uuid_b}"
    );
    assert_ne!(
        uuid_a, uuid_b,
        "Gate 3: a different seed must produce a different UUID — gen_random_uuid() is supposed to \
         draw from the seeded CRNG; identical UUIDs across seeds would mean it is a frozen constant"
    );
}
