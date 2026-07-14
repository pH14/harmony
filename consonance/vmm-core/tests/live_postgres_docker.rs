// SPDX-License-Identifier: AGPL-3.0-or-later
//! Box-only **Postgres-in-Docker** gates (`#[cfg(target_os = "linux")]` **and
//! `#[ignore]`**, on `ssh <det-box>` with the LOADED patched KVM modules,
//! CPU-pinned per `docs/BOX-PINNING.md`). Task 38 — consonance workload stream,
//! step 3 of 3, the credibility money-shot: an off-the-shelf `docker run
//! --network none postgres` runs **deterministically** in the guest.
//!
//! These boot the **Postgres-in-Docker workload image** (`guest/build/bzImage` —
//! the task-36 container-class kernel, unchanged — plus
//! `guest/build/initramfs-docker.cpio.gz`, built by
//! `guest/linux/build-docker-image.sh`) via
//! [`vmm_core::bringup::boot_linux_selected`]. The guest `/init`
//! (`docker-init.sh`) brings up cgroup-v2 and runs the **official postgres OCI
//! image** as a real container — `unshare`d mount/uts/ipc/net/pid namespaces +
//! chroot into the image rootfs + a per-container cgroup (`--network none` = a
//! fresh empty net namespace) — and the container drives the SAME fixed
//! insert/select workload as task 37 over its local unix socket. The container's
//! + the loop's stdout/stderr stream to `ttyS0`.
//!
//! **Why unshare, not runc/dockerd (the load-bearing finding — see
//! `guest/linux/IMPLEMENTATION.md`).** Under consonance's single-vCPU / V-time
//! model, V-time advances only when the guest executes RDTSC/RDMSR(IA32_TSC); any
//! busy-wait without RDTSC freezes V-time → the tick never fires → deadlock.
//! **dockerd** busy-spins on gRPC (frozen at "containerd successfully booted");
//! **runc itself** deadlocks its container-init — the container reaches "created"
//! but its Go init never execs the command (verified with a trivial `/bin/sh -c
//! echo`). So we build the container with `unshare`/`chroot`/`setpriv` (plain
//! syscalls, no Go init, no exec-fifo) and run the task-37 flow inside it. The
//! full Docker/runc stack stays baked (present, but it can't run here).
//!
//! **Why the container is deterministic (the delta over task 37).** The container
//! setup (Go-free syscalls) + the in-container postgres flow read kernel entropy
//! (`AT_RANDOM`/`getrandom`) only through the seeded CRNG: under the patched
//! backend RDRAND/RDSEED trap to the **seeded stream** and credit the CRNG
//! deterministically (the same root task 37's `pg_strong_random` and the
//! build-time initdb ride). namespace/cgroup setup + the postgres flow are
//! deterministic given V-time + seeded entropy. Gate 2 passing through the full
//! container surface is the empirical proof; `docker-init.sh` also prints
//! `boot_id` (the CRNG's UUID) as an explicit identical-twice witness.
//!
//! **Blame boundary.** Task 37 (bare Postgres) isolates the *database*
//! determinism surface; this task adds only the *container* surface on top, so a
//! future divergence localizes to a layer.
//!
//! **Workload v2 (task 42).** As in `live_postgres.rs`, each streamed row now
//! carries a `gen_random_uuid()` id and a `clock_timestamp()` wall-clock column
//! (`row|i|count|sum|uuid|t`). They *look* nondeterministic but must come out
//! **bit-identical** across two same-seed runs — `gen_random_uuid()` rides
//! `pg_strong_random` → the seeded CRNG, `clock_timestamp()` reads the V-time clock
//! — now proven through the **full container surface**. The `count`/`sum` prefix is
//! the deterministic anchor ([`FINAL_ROW_PREFIX`] = `row|20|20|210|`); the uuid + t
//! are seed-derived (checked by shape, with seed-sensitivity proven at a different
//! seed). A UUID/timestamp differing across same-seed runs would fail Gate 2 — a
//! real determinization finding, reported, not papered over.
//!
//! **Gate 1 — Postgres-in-a-container runs + streams
//! ([`p1_docker_postgres_runs_and_streams_patched`]).** One patched boot must
//! bring the OCI-image container up, have postgres announce it is ready, run the
//! workload (the streamed `row|…` lines + `database system is ready to accept
//! connections` reach the serial, each row bearing a **valid UUID + timestamp**, all
//! 20 UUIDs distinct), reach `GUEST_READY`, and power off cleanly.
//!
//! **Gate 2 — deterministic twice (the milestone,
//! [`p2_docker_postgres_deterministic_twice_patched`]).** Two same-seed patched
//! boots through the **full container stack** must produce a **bit-identical**
//! serial capture (including the UUIDs + timestamps) **and** `state_hash`.
//!
//! **Gate 3 — seed-sensitivity ([`p3_docker_postgres_seed_sensitivity_patched`]).**
//! A run at a *different* seed produces *different* UUIDs (through the container),
//! proving they are seed-driven (the seeded CRNG), not a frozen constant. Both
//! seeds' sample UUIDs are quoted.
//!
//! **Why patched, not stock.** As for task 37: the workload needs the live
//! periodic V-time tick (background workers, the Go runtimes' timers) and the
//! 8250 TX must drain to stream output — both ride the V-time LAPIC timer, which
//! only advances on the patched backend. On stock KVM the timer never calibrates,
//! so nothing streams. Both gates run patched.
//!
//! **Gate honesty (why `#[ignore]`).** These need real + patched KVM, the built
//! Docker image, and the `det-cfl-v1` host — none in the default `cargo nextest`
//! lane — so they are `#[ignore]`d (like `live_postgres.rs`); default CI shows
//! them not-run, never a vacuous green. macOS builds an empty test binary. Run on
//! the box (build the image first), patched modules loaded, CPU-pinned and
//! wall-clock-bounded — e.g.:
//!
//! ```sh
//! make -C guest fetch && make -C guest/linux docker-image    # build the image
//! # load patched kvm.ko/kvm-intel.ko, then:
//! taskset -c 2 timeout 3000 cargo test -p vmm-core --test live_postgres_docker \
//!     -- --ignored --nocapture --test-threads=1 p2_docker_postgres_deterministic_twice_patched
//! # always revert to stock KVM afterwards and verify `lsmod | grep '^kvm '` == 1396736.
//! ```
#![cfg(target_os = "linux")]

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use vmm_core::bringup::{BackendKind, boot_linux_selected};
use vmm_core::vmm::{Step, TerminalReason, Vmm};

/// 8 GiB of guest RAM: the static docker stack (~120 MiB) + the official
/// postgres image's extracted OCI rootfs (~0.5 GiB) live in the initramfs tmpfs,
/// and the container writes its cluster (PGDATA, RAM-backed) into that rootfs —
/// generous headroom over task 37's bare Postgres.
const GUEST_RAM_LEN: usize = 8 << 30;
/// The pinned determinism seed (same as the corpus / `live_postgres` seed).
const SEED: u64 = 0x0028_C0FF_EE5E_EDC0;
/// A *different* seed for the seed-sensitivity gate (Gate 3) — same value
/// `live_postgres.rs` uses. A different seed must yield different `gen_random_uuid()`
/// UUIDs (the seeded CRNG). Well-mixed away from [`SEED`] (XOR the golden ratio).
const SEED_B: u64 = SEED ^ 0x9E37_79B9_7F4A_7C15;
/// The determinism command line. Identical to `live_postgres.rs` (see that file
/// for `tsc=reliable`/`no_timer_check`/`lpj=`/`nokaslr`/`nosmp`/`maxcpus=1`/
/// `nox2apic`/`hpet=disable`, the dropped `random.trust_cpu=off`, and
/// `reboot=t,force`), plus one task-38 addition:
///   * **`cgroup_no_v1=all`** — keep every controller available to the unified
///     cgroup-v2 hierarchy `docker-init.sh` mounts (belt-and-suspenders; nothing
///     auto-mounts a v1 hierarchy here, but this guarantees no controller is
///     claimed by v1). A pure config param — determinism-neutral.
const DEFAULT_CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t,force tsc=reliable \
     no_timer_check lpj=4000000 nokaslr nosmp maxcpus=1 nox2apic hpet=disable cgroup_no_v1=all";
/// Step budget: a high cap so a stuck guest cannot run forever (the heavy docker
/// bring-up + workload is bounded by the wall budget + the external `timeout`).
const MAX_STEPS: u64 = 200_000_000_000;
/// Wall-clock budget inside the test. The full container stack (dockerd +
/// containerd + runc starting + `docker load` of a ~160 MiB image + the postgres
/// container's own multiprocess bring-up) is much heavier than task 37's bare
/// Postgres; this is a deliberate milestone gate, run with a matching external
/// `timeout`.
const WALL_BUDGET: Duration = Duration::from_secs(2700);

/// The in-container flow script (`pg-container-run.sh`) prints this once the OCI
/// container is up and has started postgres.
const CONTAINER_UP: &[u8] = b"PGC38: starting postgres in container";
/// postgres (inside the container) announces this once accepting connections.
const PG_READY: &[u8] = b"database system is ready to accept connections";
/// The workload loop's end marker (printed by the in-container `pg-container-run.sh`).
const WORKLOAD_END: &[u8] = b"PGC38: workload end";
/// The deterministic prefix of the final workload row (iteration 20): the `row`
/// marker, loop index 20, running `count(*)` = 20, running `sum(i)` = 1+…+20 = 210 —
/// the SAME pure-function-of-the-index anchor `live_postgres.rs` pins. The `uuid|t`
/// that FOLLOW it in the streamed line (`row|20|20|210|<uuid>|<t>`) are seed-derived
/// (checked by shape, not value). Matching this proves the *query results* (not just
/// "docker ran") reached the serial.
const FINAL_ROW_PREFIX: &[u8] = b"row|20|20|210|";
/// `docker-init.sh` prints this after a clean shutdown.
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

/// What a bounded run observed.
struct BootOutcome {
    reason: Option<TerminalReason>,
    steps: u64,
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
                eprintln!("\n[dk] step error after {steps} steps: {e}  | debug={e:?}");
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
            eprintln!("\n[dk] wall-clock budget exceeded after {steps} steps");
            break;
        }
    }
    let serial = vmm.serial();
    BootOutcome {
        reason,
        steps,
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

/// Boot the Docker image on the patched backend at `seed`, run it to a terminal,
/// and return (serial capture, `state_hash`, outcome). As in `live_postgres.rs`
/// the [`Vmm`] — and with it the `perf_event` work counter that drives V-time —
/// is **dropped before returning**, so two same-seed runs in one process don't
/// keep two pinned PMU counters open at once (which would multiplex and perturb
/// the branch count → a divergent V-time). One counter at a time is exact.
fn boot_docker(seed: u64) -> (Vec<u8>, [u8; 32], BootOutcome) {
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
        "\n[{tag}] steps={} terminal={:?} container_up={} pg_ready={} workload_done={} \
         final_row={} uuids={} GUEST_READY={} step_error={:?}",
        out.steps,
        out.reason,
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

// --- Gate 1: Dockerized Postgres runs + streams (patched) ------------------

/// **Gate 1 — Dockerized Postgres runs and streams.** One patched boot brings
/// dockerd up, runs `docker run --network none postgres`, has postgres announce
/// readiness, executes the workload (the `row|…` query results + postgres'
/// stdout/stderr reach `ttyS0`), and powers off cleanly within budget.
#[test]
#[ignore = "box-only live gate (LOADED patched KVM + built Docker image + det-cfl-v1 host); \
            run on `ssh <det-box>` with `-- --ignored --nocapture`"]
fn p1_docker_postgres_runs_and_streams_patched() {
    require_kvm();
    require_host_baseline();
    eprintln!("[dk] cmdline: {}", cmdline());
    let (_serial, _hash, out) = boot_docker(SEED);
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
        out.container_up,
        "Gate 1: the OCI container (runc) must come up and report postgres ready"
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
/// through the full container stack produce a bit-identical serial capture
/// (including the query output) **and** `state_hash`.
#[test]
#[ignore = "MILESTONE gate (task 38): same-seed bit-identical Postgres-in-Docker run; run on the \
            box with the LOADED patched KVM and `-- --ignored --nocapture`"]
fn p2_docker_postgres_deterministic_twice_patched() {
    require_kvm();
    require_host_baseline();

    // boot_docker drops run A's Vmm (and its PMU counter) before we boot run B.
    let (serial_a, hash_a, out_a) = boot_docker(SEED);
    report("p2 run A", &out_a);
    let (serial_b, hash_b, out_b) = boot_docker(SEED);
    report("p2 run B", &out_b);

    let hex = |h: &[u8; 32]| h.iter().map(|b| format!("{b:02x}")).collect::<String>();
    eprintln!(
        "[dk] determinism: serial_len A/B = {}/{}\n  state_hash A = {}\n  state_hash B = {}",
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
         through the full container surface; if a gen_random_uuid()/clock_timestamp() escaped, the \
         serials would differ here: a real determinization finding, not a flake)"
    );
    assert_eq!(
        hash_a, hash_b,
        "Gate 2: two same-seed patched boots must produce an identical state_hash"
    );

    // The headline witness: the random-looking UUID + wall-clock timestamp came out
    // bit-identical across the two same-seed runs, through the OCI container stack.
    let (uuid, t) = out_a
        .sample_uuid_ts
        .clone()
        .expect("final row parsed (asserted above)");
    eprintln!(
        "[dk] deterministic-twice witness (seed {SEED:#018x}): the random-looking UUID + \
         wall-clock timestamp are bit-identical in run A and run B —\n  uuid = {uuid}\n  t    = {t}"
    );
}

// --- Gate 3: seed-sensitivity ----------------------------------------------

/// **Gate 3 — seed-sensitivity.** A run at [`SEED`] and a run at a *different* seed
/// [`SEED_B`] must produce *different* UUIDs through the container — proving they are
/// genuinely driven by the seeded CRNG (`gen_random_uuid()` → `pg_strong_random`),
/// not a frozen constant that would sail through Gate 2 vacuously. Both sample UUIDs
/// are quoted.
#[test]
#[ignore = "box-only seed-sensitivity gate (task 42): different seed ⇒ different UUIDs through the \
            container; run on the box with the LOADED patched KVM and `-- --ignored --nocapture`"]
fn p3_docker_postgres_seed_sensitivity_patched() {
    require_kvm();
    require_host_baseline();

    // boot_docker drops each run's Vmm (and its PMU counter) before the next boots.
    let (_serial_a, _hash_a, out_a) = boot_docker(SEED);
    report("p3 seed A", &out_a);
    let (_serial_b, _hash_b, out_b) = boot_docker(SEED_B);
    report("p3 seed B", &out_b);

    let uuid_a = assert_uuid_time_shape(&format!("Gate 3 seed A ({SEED:#018x})"), &out_a);
    let uuid_b = assert_uuid_time_shape(&format!("Gate 3 seed B ({SEED_B:#018x})"), &out_b);
    eprintln!(
        "[dk] seed-sensitivity: sample UUID per seed —\n  seed {SEED:#018x} -> {uuid_a}\n  \
         seed {SEED_B:#018x} -> {uuid_b}"
    );
    assert_ne!(
        uuid_a, uuid_b,
        "Gate 3: a different seed must produce a different UUID — gen_random_uuid() is supposed to \
         draw from the seeded CRNG; identical UUIDs across seeds would mean it is a frozen constant"
    );
}
