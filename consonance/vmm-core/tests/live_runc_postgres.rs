// SPDX-License-Identifier: AGPL-3.0-or-later
//! Box-only **Postgres-via-real-`runc`** gates (`#[cfg(target_os = "linux")]` **and
//! `#[ignore]`**, on `ssh <det-box>` with the LOADED patched KVM modules,
//! CPU-pinned per `docs/BOX-PINNING.md`). Task 48 — **the money-shot**: the actual
//! `runc` binary (the real Go container runtime, *not* the task-38
//! `unshare`/`chroot`/`setpriv` shim) launches the official postgres OCI container,
//! the task-42 `gen_random_uuid()`/`clock_timestamp()` workload runs against it, and
//! it comes out **bit-identical across two same-seed runs**.
//!
//! These boot the **same Postgres-in-Docker image** task 38 built (`guest/build/bzImage`
//! — the task-36 container-class kernel, unchanged — plus
//! `guest/build/initramfs-docker.cpio.gz`, built by `guest/linux/build-docker-image.sh`)
//! via [`vmm_core::bringup::boot_linux_selected`], but select the **runc** `/init`
//! with the kernel `rdinit=/runc-init` cmdline param (`runc-init.sh`). That init
//! brings up cgroup-v2 and runs the **official postgres OCI image** with
//! `runc run pg-container` on the baked `/oci` bundle — the identical bundle +
//! `config.json` (`runc spec`-generated; allow-all devices, `terminal=false`, runs
//! `/run-workload.sh`) the task-38 build already produced. The container's +
//! the loop's stdout/stderr stream to `ttyS0`.
//!
//! **The unlock (vs task 38's `unshare` workaround — see
//! `guest/linux/IMPLEMENTATION.md` + `tasks/47-deterministic-preemption-timer.md`).**
//! Under task 38's single-vCPU / V-time model, V-time advanced only at natural
//! VM-exits; `runc`/its Go container-init busy-spin (`procyield`/`osyield`) with no
//! exit → V-time froze → the LAPIC tick never fired → the Go scheduler never ran →
//! the container reached "created" but its init never execed the command (a deadlock,
//! which is *why* task 38 fell back to `unshare`). Task 47 made the V-time LAPIC timer
//! **preempt** a busy-spinning thread at the seed-deterministic V-time deadline
//! (`run_until` = PMU overflow + single-step to the exact retired-branch count), and
//! the VMM run-loop drives it automatically on the patched Linux boot
//! ([`Vmm::step`] → `preemption_deadline()` → `Backend::run_until`). So the Go runtime
//! is preempted on time, the scheduler runs, the create→exec handshake completes, and
//! the **real `runc`** runs the container — deterministically, because the preemption
//! instant is a pure function of the seed.
//!
//! **Why the container is still deterministic (unchanged from task 38).** The
//! container setup + the in-container postgres flow read kernel entropy
//! (`AT_RANDOM`/`getrandom`) only through the seeded CRNG: under the patched backend
//! RDRAND/RDSEED trap to the **seeded stream** and credit the CRNG deterministically
//! (the same root task 37's `pg_strong_random` and the build-time initdb ride). The
//! delta over task 38 is the **interleaving**: the Go runtime is now genuinely running
//! (not bypassed), but its preemption points are seed-deterministic, so the whole run
//! is a pure function of the seed — Gate 2 is the empirical proof.
//!
//! **Workload v2 (task 42).** As in `live_postgres_docker.rs`, each streamed row
//! carries a `gen_random_uuid()` id and a `clock_timestamp()` wall-clock column
//! (`row|i|count|sum|uuid|t`). They *look* nondeterministic but must come out
//! **bit-identical** across two same-seed runs. The `count`/`sum` prefix is the
//! deterministic anchor ([`FINAL_ROW_PREFIX`] = `row|20|20|210|`); the uuid + t are
//! seed-derived (checked by shape, with seed-sensitivity proven at a different seed).
//!
//! **Gate 1 — real `runc` runs Postgres + streams
//! ([`r1_runc_postgres_runs_and_streams_patched`]).** One patched boot must launch the
//! OCI container *through the real `runc` binary* (the `RUNC48:` banner + `runc run`
//! marker reach the serial, and the task-38 `DK38:`/`unshare` markers do **not** —
//! proving it is `runc`, not the shim), have postgres announce readiness, run the
//! workload (the streamed `row|…` lines, each with a valid UUID + timestamp, all 20
//! distinct), `runc run` exit 0, reach `GUEST_READY`, and power off cleanly.
//!
//! **Gate 2 — deterministic twice (the milestone,
//! [`r2_runc_postgres_deterministic_twice_patched`]).** Two same-seed patched boots
//! through real `runc` produce a **bit-identical** serial capture (including the UUIDs
//! + timestamps) **and** `state_hash`.
//!
//! **Gate 3 — seed-sensitivity ([`r3_runc_postgres_seed_sensitivity_patched`]).** A
//! run at a *different* seed produces *different* UUIDs through the container, proving
//! they are seed-driven (the seeded CRNG), not a frozen constant. Both seeds' sample
//! UUIDs are quoted.
//!
//! **Why patched, not stock.** As for task 37/38: the workload needs the live periodic
//! V-time tick and the 8250 TX must drain to stream output — both ride the V-time LAPIC
//! timer, which only advances (and only **preempts**, via `run_until`) on the patched
//! backend. On stock KVM the timer never calibrates and `runc` deadlocks (task 38), so
//! all gates run patched.
//!
//! **Gate honesty (why `#[ignore]`).** These need real + patched KVM, the built Docker
//! image, and the `det-cfl-v1` host — none in the default `cargo nextest` lane — so
//! they are `#[ignore]`d (like `live_postgres_docker.rs`); default CI shows them
//! not-run, never a vacuous green. macOS builds an empty test binary. Run on the box
//! (build the image first), patched modules loaded, CPU-pinned and wall-clock-bounded
//! — e.g.:
//!
//! ```sh
//! make -C guest fetch && make -C guest/linux docker-image    # build the image
//! # load patched kvm.ko/kvm-intel.ko, then:
//! taskset -c 4 timeout 4200 cargo test -p vmm-core --test live_runc_postgres \
//!     -- --ignored --nocapture --test-threads=1 r2_runc_postgres_deterministic_twice_patched
//! # always revert to stock KVM afterwards and verify `lsmod | grep '^kvm '` == 1396736.
//! ```
#![cfg(target_os = "linux")]

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use vmm_core::bringup::{BackendKind, boot_linux_selected};
use vmm_core::vmm::{Step, TerminalReason, Vmm};

/// 8 GiB of guest RAM: the static docker stack + the official postgres image's
/// extracted OCI rootfs live in the initramfs tmpfs, and the container writes its
/// cluster (PGDATA, RAM-backed) into that rootfs — same as `live_postgres_docker.rs`.
const GUEST_RAM_LEN: usize = 8 << 30;
/// The pinned determinism seed (same as the corpus / `live_postgres*` seed).
const SEED: u64 = 0x0028_C0FF_EE5E_EDC0;
/// A *different* seed for the seed-sensitivity gate (Gate 3) — same value
/// `live_postgres_docker.rs` uses. A different seed must yield different
/// `gen_random_uuid()` UUIDs (the seeded CRNG). Well-mixed (XOR the golden ratio).
const SEED_B: u64 = SEED ^ 0x9E37_79B9_7F4A_7C15;
/// The determinism command line. Identical to `live_postgres_docker.rs` (see that
/// file + `live_postgres.rs`), plus the one task-48 addition:
///   * **`rdinit=/runc-init`** — select the REAL-runc `/init` (`runc-init.sh`),
///     baked alongside the task-38 default `/init` (`docker-init.sh`, the unshare
///     path). A pure boot param — determinism-neutral; it only picks which init runs.
const DEFAULT_CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t,force tsc=reliable \
     no_timer_check lpj=4000000 nokaslr nosmp maxcpus=1 nox2apic hpet=disable cgroup_no_v1=all \
     rdinit=/runc-init";
/// Step budget: a high cap so a stuck guest cannot run forever (the heavy runc/Go
/// bring-up + workload is bounded by the wall budget + the external `timeout`).
const MAX_STEPS: u64 = 200_000_000_000;
/// Wall-clock budget inside the test. The real `runc`/Go path (multi-goroutine
/// container-init driven forward by V-time preemption single-stepping) is heavier
/// than task 38's `unshare` shim; this is a deliberate milestone gate, run with a
/// matching (larger) external `timeout`. Overridable via `WALL_BUDGET_SECS`.
const WALL_BUDGET_SECS_DEFAULT: u64 = 3600;

/// `runc-init.sh` prints this as it launches the container through the real `runc`.
/// Its presence (with [`UNSHARE_MARKER`]/[`DK38_MARKER`] absent) proves the run went
/// through the actual `runc` binary, not the task-38 `unshare`/`chroot` shim.
const RUNC_BANNER: &[u8] = b"via REAL runc: runc run";
/// `runc-init.sh` prints this when `runc run` returns — the rc is parsed + asserted 0.
const RUNC_EXIT_PREFIX: &[u8] = b"RUNC48: runc run exited rc=";
/// The task-38 unshare-init prefix. Must be **absent** on the runc path (a witness
/// that `rdinit=/runc-init` really selected the runc init, not `docker-init.sh`).
const DK38_MARKER: &[u8] = b"DK38:";
/// The task-38 unshare launch line. Must be **absent** on the runc path.
const UNSHARE_MARKER: &[u8] = b"unshare(mount,uts,ipc,net,pid)";
/// The in-container flow script (`pg-container-run.sh`) prints this once the OCI
/// container is up (its PID 1) and has started postgres.
const CONTAINER_UP: &[u8] = b"PGC38: starting postgres in container";
/// postgres (inside the container) announces this once accepting connections.
const PG_READY: &[u8] = b"database system is ready to accept connections";
/// The workload loop's end marker (printed by the in-container `pg-container-run.sh`).
const WORKLOAD_END: &[u8] = b"PGC38: workload end";
/// The deterministic prefix of the final workload row (iteration 20): the `row`
/// marker, loop index 20, running `count(*)` = 20, running `sum(i)` = 1+…+20 = 210 —
/// the SAME anchor `live_postgres*.rs` pins. The `uuid|t` that FOLLOW it
/// (`row|20|20|210|<uuid>|<t>`) are seed-derived (checked by shape, not value).
const FINAL_ROW_PREFIX: &[u8] = b"row|20|20|210|";
/// `runc-init.sh` prints this after a clean shutdown.
const GUEST_READY: &[u8] = b"GUEST_READY";

/// The fixed iteration count of the workload loop (`WORKLOAD_N` in
/// `build-docker-image.sh`): every run streams exactly this many `row|…` lines, each
/// with its own distinct `gen_random_uuid()`.
const WORKLOAD_N: usize = 20;

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
         box: `make -C guest fetch && make -C guest/linux docker-image`."
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

fn wall_budget() -> Duration {
    let secs = std::env::var("WALL_BUDGET_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(WALL_BUDGET_SECS_DEFAULT);
    Duration::from_secs(secs)
}

fn find(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// `true` iff `s` is a textual UUID — 36 chars, `8-4-4-4-12` hex with hyphens at the
/// canonical offsets. A lightweight shape check (no `uuid` crate): proves the streamed
/// field is a real UUID, not a constant placeholder or an error string.
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
/// `timestamptz` text form). A lightweight shape check that the streamed
/// `clock_timestamp()` field is a real timestamp, not a constant or an error.
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
/// UUID). Used to prove the UUIDs are not a frozen constant *within* a run.
fn all_row_uuids(serial: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(serial)
        .lines()
        .filter(|l| l.starts_with("row|"))
        .filter_map(|l| l.split('|').nth(4).map(str::to_string))
        .filter(|u| is_uuid(u))
        .collect()
}

/// The parsed `runc run` exit code (`RUNC48: runc run exited rc=<n>`), if present.
fn runc_exit_code(serial: &[u8]) -> Option<i32> {
    let line = line_with_prefix(serial, RUNC_EXIT_PREFIX)?;
    line.rsplit('=').next()?.trim().parse::<i32>().ok()
}

/// What a bounded run observed.
struct BootOutcome {
    reason: Option<TerminalReason>,
    steps: u64,
    /// The `runc-init.sh` banner proving the run launched the container through the
    /// real `runc` binary (`runc run`).
    runc_launched: bool,
    /// The parsed `runc run` exit code, if it returned.
    runc_rc: Option<i32>,
    /// The task-38 `unshare` shim left NO trace (its `DK38:` / `unshare(...)` markers
    /// are absent) — a witness that `rdinit=/runc-init` selected the runc path.
    no_unshare_shim: bool,
    container_up: bool,
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
fn run_bounded<B: vmm_backend::Backend>(vmm: &mut Vmm<B>) -> BootOutcome {
    // not order-observable: a test-only wall-clock watchdog (belt-and-braces with
    // the external `timeout`) — it bounds how long this `#[ignore]`d box gate runs
    // and never reaches guest state, the serial capture, or any hash.
    #[allow(clippy::disallowed_methods)]
    let start = Instant::now();
    let budget = wall_budget();
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
                eprintln!("\n[runc] step error after {steps} steps: {e}  | debug={e:?}");
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
        if steps.is_multiple_of(8192) && start.elapsed() > budget {
            eprintln!("\n[runc] wall-clock budget exceeded after {steps} steps");
            break;
        }
    }
    let serial = vmm.serial();
    BootOutcome {
        reason,
        steps,
        runc_launched: find(serial, RUNC_BANNER),
        runc_rc: runc_exit_code(serial),
        no_unshare_shim: !find(serial, DK38_MARKER) && !find(serial, UNSHARE_MARKER),
        container_up: find(serial, CONTAINER_UP),
        pg_ready: find(serial, PG_READY),
        workload_done: find(serial, WORKLOAD_END),
        final_row: find(serial, FINAL_ROW_PREFIX),
        sample_uuid_ts: final_row_uuid_ts(serial),
        row_uuids: all_row_uuids(serial),
        guest_ready: find(serial, GUEST_READY),
        step_error,
    }
}

/// Boot the Docker image with the **runc** init on the patched backend at `seed`, run
/// it to a terminal, and return (serial capture, `state_hash`, outcome). As in
/// `live_postgres_docker.rs` the [`Vmm`] — and with it the `perf_event` work counter
/// that drives V-time — is **dropped before returning**, so two same-seed runs in one
/// process don't keep two pinned PMU counters open at once (which would multiplex and
/// perturb the branch count → a divergent V-time). One counter at a time is exact.
fn boot_runc(seed: u64) -> (Vec<u8>, [u8; 32], BootOutcome) {
    let kernel = require_artifact("bzImage");
    let initramfs = require_artifact("initramfs-docker.cpio.gz");
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
        "\n[{tag}] steps={} terminal={:?} runc_launched={} runc_rc={:?} no_unshare_shim={} \
         container_up={} pg_ready={} workload_done={} final_row={} uuids={} GUEST_READY={} \
         step_error={:?}",
        out.steps,
        out.reason,
        out.runc_launched,
        out.runc_rc,
        out.no_unshare_shim,
        out.container_up,
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
/// *within* a run). Returns the final row's sample UUID for quoting + Gate 3's
/// cross-seed comparison. Panics (loud failure) on any malformed/missing field.
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

/// Assert `out` went through the **real `runc`** binary, not the task-38 shim: the
/// `runc run` banner reached the serial, `runc run` exited 0, and the `unshare`
/// path left no trace. This is the task-48 headline (Gate 1's "no unshare").
fn assert_went_through_runc(tag: &str, out: &BootOutcome) {
    assert!(
        out.runc_launched,
        "{tag}: the `runc run` banner must reach the serial — the run must launch the container \
         through the REAL runc binary (not the task-38 unshare/chroot shim)"
    );
    assert!(
        out.no_unshare_shim,
        "{tag}: the task-38 unshare-shim markers (DK38: / unshare(...)) must be ABSENT — their \
         presence would mean `rdinit=/runc-init` did not select the runc init"
    );
    assert_eq!(
        out.runc_rc,
        Some(0),
        "{tag}: `runc run` must exit 0 (a clean container run); got rc={:?}",
        out.runc_rc
    );
}

// --- Gate 1: real runc runs Postgres + streams (patched) -------------------

/// **Gate 1 — real `runc` runs Postgres and streams.** One patched boot launches the
/// OCI container *through the actual `runc` binary* (`runc run`, no `unshare` shim),
/// has postgres announce readiness, executes the workload (the `row|…` query results
/// reach `ttyS0`, each with a valid UUID + timestamp), `runc run` exits 0, and the
/// guest powers off cleanly within budget.
#[test]
#[ignore = "box-only live gate (LOADED patched KVM + built Docker image + det-cfl-v1 host); \
            run on `ssh <det-box>` with `-- --ignored --nocapture`"]
fn r1_runc_postgres_runs_and_streams_patched() {
    require_kvm();
    require_host_baseline();
    eprintln!("[runc] cmdline: {}", cmdline());
    let (_serial, _hash, out) = boot_runc(SEED);
    report("r1", &out);
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
    // The headline: it went through the REAL runc, not the task-38 unshare shim.
    assert_went_through_runc("Gate 1", &out);
    assert!(
        out.container_up,
        "Gate 1: the OCI container's PID 1 must come up and start postgres (PGC38: ...)"
    );
    assert!(
        out.pg_ready,
        "Gate 1: the containerized postgres must announce it is ready to accept connections"
    );
    assert!(
        out.workload_done,
        "Gate 1: the workload loop must run to completion (PGC38: workload end)"
    );
    assert!(
        out.final_row,
        "Gate 1: the deterministic final workload row (row|20|20|210|…) must reach the serial"
    );
    // Shape (task 42): the streamed rows carry a valid UUID + timestamp, not a
    // constant/error, and the per-iteration UUIDs are all distinct.
    let sample = assert_uuid_time_shape("Gate 1", &out);
    eprintln!("[r1] sample UUID (seed {SEED:#018x}): {sample}");
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
/// through the real `runc` binary produce a bit-identical serial capture (including
/// the query output) **and** `state_hash`.
#[test]
#[ignore = "MILESTONE gate (task 48): same-seed bit-identical real-runc Postgres-OCI run; run on \
            the box with the LOADED patched KVM and `-- --ignored --nocapture`"]
fn r2_runc_postgres_deterministic_twice_patched() {
    require_kvm();
    require_host_baseline();

    // boot_runc drops run A's Vmm (and its PMU counter) before we boot run B.
    let (serial_a, hash_a, out_a) = boot_runc(SEED);
    report("r2 run A", &out_a);
    let (serial_b, hash_b, out_b) = boot_runc(SEED);
    report("r2 run B", &out_b);

    let hex = |h: &[u8; 32]| h.iter().map(|b| format!("{b:02x}")).collect::<String>();
    eprintln!(
        "[runc] determinism: serial_len A/B = {}/{}\n  state_hash A = {}\n  state_hash B = {}",
        serial_a.len(),
        serial_b.len(),
        hex(&hash_a),
        hex(&hash_b),
    );

    // Both runs must actually have run the workload through REAL runc to GUEST_READY,
    // so two identical-but-stranded boots cannot pass this gate vacuously — and each
    // must carry well-formed, distinct UUIDs + timestamps (so the bit-identity below
    // is over *real* random/wall-clock columns, not a constant or an error string).
    for (tag, out) in [("A", &out_a), ("B", &out_b)] {
        assert!(
            out.step_error.is_none(),
            "Gate 2 run {tag}: contract violation: {:?}",
            out.step_error
        );
        assert_went_through_runc(&format!("Gate 2 run {tag}"), out);
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
        "Gate 2: two same-seed patched boots through real runc must produce a bit-identical serial \
         capture (this is what proves the UUIDs + timestamps — and the Go-runtime interleaving the \
         preemption timer now drives — are bit-identical across same-seed runs; if anything escaped, \
         the serials would differ here: a real determinization finding, not a flake)"
    );
    assert_eq!(
        hash_a, hash_b,
        "Gate 2: two same-seed patched boots through real runc must produce an identical state_hash"
    );

    // The headline witness: the random-looking UUID + wall-clock timestamp came out
    // bit-identical across the two same-seed runs, through the REAL runc OCI stack.
    let (uuid, t) = out_a
        .sample_uuid_ts
        .clone()
        .expect("final row parsed (asserted above)");
    eprintln!(
        "[runc] deterministic-twice witness (seed {SEED:#018x}): the random-looking UUID + \
         wall-clock timestamp are bit-identical in run A and run B, through REAL runc —\n  \
         uuid = {uuid}\n  t    = {t}"
    );
}

// --- Gate 3: seed-sensitivity ----------------------------------------------

/// **Gate 3 — seed-sensitivity.** A run at [`SEED`] and a run at a *different* seed
/// [`SEED_B`] must produce *different* UUIDs through the real-runc container — proving
/// they are genuinely driven by the seeded CRNG (`gen_random_uuid()` →
/// `pg_strong_random`), not a frozen constant that would sail through Gate 2 vacuously.
/// Both sample UUIDs are quoted.
#[test]
#[ignore = "box-only seed-sensitivity gate (task 42/48): different seed ⇒ different UUIDs through \
            the real-runc container; run on the box with the LOADED patched KVM and `-- --ignored \
            --nocapture`"]
fn r3_runc_postgres_seed_sensitivity_patched() {
    require_kvm();
    require_host_baseline();

    // boot_runc drops each run's Vmm (and its PMU counter) before the next boots.
    let (_serial_a, _hash_a, out_a) = boot_runc(SEED);
    report("r3 seed A", &out_a);
    let (_serial_b, _hash_b, out_b) = boot_runc(SEED_B);
    report("r3 seed B", &out_b);

    // Each run must genuinely have gone through real runc (else "different UUIDs"
    // could come from a non-runc path — not what this gate is about).
    assert_went_through_runc("Gate 3 seed A", &out_a);
    assert_went_through_runc("Gate 3 seed B", &out_b);

    let uuid_a = assert_uuid_time_shape(&format!("Gate 3 seed A ({SEED:#018x})"), &out_a);
    let uuid_b = assert_uuid_time_shape(&format!("Gate 3 seed B ({SEED_B:#018x})"), &out_b);
    eprintln!(
        "[runc] seed-sensitivity: sample UUID per seed —\n  seed {SEED:#018x} -> {uuid_a}\n  \
         seed {SEED_B:#018x} -> {uuid_b}"
    );
    assert_ne!(
        uuid_a, uuid_b,
        "Gate 3: a different seed must produce a different UUID — gen_random_uuid() is supposed to \
         draw from the seeded CRNG; identical UUIDs across seeds would mean it is a frozen constant"
    );
}
