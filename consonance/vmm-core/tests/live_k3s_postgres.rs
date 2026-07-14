// SPDX-License-Identifier: AGPL-3.0-or-later
//! Box-only **Postgres-on-k3s, client pod -> server pod, intra-guest** gates
//! (`#[cfg(target_os = "linux")]` **and `#[ignore]`**, on `ssh <det-box>` with the
//! LOADED patched KVM modules, CPU-pinned per `docs/BOX-PINNING.md`). Task 49 — the
//! determinism stress test at full stack height: a single guest VM (single-vCPU)
//! runs a **single-node lightweight Kubernetes cluster (k3s)**, a `postgres` Pod
//! serves the task-42 workload, and a separate `client` Pod connects to it **over
//! the in-guest CNI** (pod -> ClusterIP -> kube-proxy DNAT -> the server pod, all
//! intra-guest — NO host networking, pv-net unused), runs the
//! `gen_random_uuid()`/`clock_timestamp()` workload, and it comes out
//! **bit-identical across two same-seed runs**.
//!
//! These boot `guest/build/bzImage` (the *unchanged* task-36 container-class
//! kernel) + `guest/build/initramfs-k3s.cpio.gz` (built by
//! `guest/linux/build-k3s-image.sh`) via [`vmm_core::bringup::boot_linux_selected`],
//! selecting the k3s `/init` with `rdinit=/k3s-init` (`k3s-init.sh`). That init
//! brings up the cluster, waits for the postgres pod Ready, applies the client pod,
//! and streams the client's workload output to `ttyS0`.
//!
//! **The unlock (tasks 47/52/54).** kubelet + containerd + apiserver + scheduler +
//! controller-manager + kube-proxy + flannel are all Go/multi-goroutine services
//! that busy-spin and depend on preemption. The V-time LAPIC timer **preempts** a
//! busy-spinning thread at the seed-deterministic V-time deadline (`run_until`), the
//! idle-HLT resume warps to the next deadline (task 52), and the xAPIC MMIO is routed
//! to the deterministic LAPIC model (task 54). So the Go schedulers run, the cluster
//! converges, and the whole interleaving is a pure function of the seed.
//!
//! **Why it is deterministic.** k3s mints its certs/tokens/SA-keys/object-UIDs from
//! `getrandom` -> the **seeded CRNG** (RDRAND/RDSEED trap to the seeded stream) and
//! stamps every resource/lease/event from the **V-time** clock; the sqlite datastore
//! lives on the RAM-backed rootfs. So two same-seed boots are bit-identical — incl.
//! the workload's "random" UUIDs + wall-clock timestamps (Gate `k2`).
//!
//! **Gate `k1` — cluster up + both pods + intra-guest client->server call + streams
//! ([`k1_k3s_cluster_postgres_client_streams_patched`]).** One patched boot reaches a
//! Ready single-node cluster, the `postgres` and `client` pods schedule + run, the
//! client connects to the postgres pod **over the CNI** (witnessed by both pod IPs in
//! the pod CIDR `10.42.0.0/16` and the postgres connection log), runs the workload
//! (the `row|…` lines, each a valid UUID + timestamp, all 20 distinct), and powers
//! off cleanly at `GUEST_READY`.
//!
//! **Gate `k2` — deterministic twice (the milestone,
//! [`k2_k3s_postgres_deterministic_twice_patched`]).** Two same-seed patched boots
//! produce a **bit-identical** serial capture (incl. the UUIDs + timestamps) **and**
//! `state_hash`.
//!
//! **Gate `k3` — seed-sensitivity
//! ([`k3_k3s_postgres_seed_sensitivity_patched`]).** A run at a *different* seed
//! produces *different* UUIDs through the cluster, proving they are seed-driven (the
//! seeded CRNG), not a frozen constant.
//!
//! **Gate honesty (why `#[ignore]`).** These need real + patched KVM, the built k3s
//! image, and the `det-cfl-v1` host — none in the default `cargo nextest` lane — so
//! they are `#[ignore]`d (like `live_runc_postgres.rs`); default CI shows them
//! not-run, never a vacuous green. macOS builds an empty test binary. Run on the box
//! (build the image first), patched modules loaded, CPU-pinned, wall-clock-bounded:
//!
//! ```sh
//! make -C guest fetch && make -C guest/linux k3s-image     # build the image
//! # load patched kvm.ko/kvm-intel.ko (the ORIGINAL stable module), then:
//! taskset -c 2 timeout 14400 cargo test -p vmm-core --test live_k3s_postgres \
//!     -- --ignored --nocapture --test-threads=1 k2_k3s_postgres_deterministic_twice_patched
//! # always revert to stock KVM afterwards and verify `lsmod | grep '^kvm '` == 1396736.
//! ```
//!
//! **Watch the run (telemetry recording).** Each boot writes an out-of-band
//! [`telemetry::NdjsonRecorder`] recording to `<$K3S_NDJSON|k3s-run>.<tag>.ndjson`
//! (e.g. `k3s-run.k1.ndjson`, `k3s-run.k2_run_a.ndjson`) — the console serial as
//! `Console` events + periodic exit-count snapshots + a final `Terminal` event.
//! Replay it in the web console to watch k3s boot:
//! `cargo run -p telemetry --bin console -- --source file:k3s-run.k1.ndjson`. The
//! recorder is **read-only / out-of-band**: it is fed only from `serial()` /
//! `exit_counts()` and writes its own file, never touching
//! `state_hash`/`observable_digest`, so it cannot perturb the deterministic run (the
//! `Observer` contract). A viewer artifact only — a failure to open/write it is a
//! warning, never a gate failure.
#![cfg(target_os = "linux")]

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use telemetry::{Event, EventKind, NdjsonRecorder, Observer};
use vmm_core::bringup::{BackendKind, boot_linux_selected};
use vmm_core::vmm::{Step, TerminalReason, Vmm};

/// The telemetry recording sink for a single boot: a lossless [`NdjsonRecorder`]
/// over a buffered file the `console` bin replays. Out-of-band + read-only: it is
/// fed only from `serial()` / `exit_counts()` and writes a file, so it CANNOT
/// perturb determinism (`state_hash`/`observable_digest` never see it) — a viewer
/// artifact, exactly the `NullObserver`-default Observer contract.
type Recorder = NdjsonRecorder<BufWriter<File>>;

/// How often (in VMM steps) to snapshot the per-reason exit tally into the
/// recording — drives the console's exit-rate counters/graph as k3s boots.
const COUNTS_EVERY: u64 = 8192;

/// 16 GiB of guest RAM: k3s (containerd + the control plane + the agent) plus the
/// pre-imported images, the self-extracted k3s data dir, the two pod rootfs layers
/// and the RAM-backed PGDATA + sqlite all live in the initramfs tmpfs. 16 GiB
/// exercises the task-54 xAPIC-page memslot hole (RAM spans past `0xFEE00000`).
/// Overridable via `GUEST_RAM_GIB` for a tighter/looser box.
const GUEST_RAM_GIB_DEFAULT: usize = 16;
/// The pinned determinism seed (same as the corpus / `live_postgres*` seed).
const SEED: u64 = 0x0028_C0FF_EE5E_EDC0;
/// A *different* seed for the seed-sensitivity gate (Gate `k3`) — same value the
/// other `live_postgres*` gates use. Well-mixed (XOR the golden ratio).
const SEED_B: u64 = SEED ^ 0x9E37_79B9_7F4A_7C15;
/// The determinism command line. Identical to `live_runc_postgres.rs` save the one
/// task-49 change: **`rdinit=/k3s-init`** selects the k3s `/init` (`k3s-init.sh`).
/// `cgroup_no_v1=all` forces the unified cgroup-v2 hierarchy k3s/kubelet want.
const DEFAULT_CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t,force tsc=reliable \
     no_timer_check lpj=4000000 nokaslr maxcpus=1 nox2apic hpet=disable cgroup_no_v1=all \
     rdinit=/k3s-init";
/// Step budget: a high cap so a stuck guest cannot run forever (the heavy k3s
/// bring-up is bounded by the wall budget + the external `timeout`).
const MAX_STEPS: u64 = 2_000_000_000_000;
/// Wall-clock budget inside the test. k3s is FAR heavier than bare runc (the whole
/// Go control plane + agent, driven forward by V-time preemption single-stepping);
/// this is a deliberate milestone gate, run with a matching (larger) external
/// `timeout`. Overridable via `WALL_BUDGET_SECS`.
const WALL_BUDGET_SECS_DEFAULT: u64 = 14_400;

/// `k3s-init.sh` prints this once the single-node cluster reaches Ready.
const CLUSTER_UP: &[u8] = b"CLUSTER_UP";
/// `k3s-init.sh` prints this once the postgres pod is Running + Ready.
const POSTGRES_READY: &[u8] = b"POSTGRES_READY";
/// The client pod prints this (in its own stdout, streamed via `kubectl logs`) once
/// it has connected to the postgres pod over the CNI.
const CLIENT_CONNECTED: &[u8] = b"client connected to the postgres pod over the CNI";
/// The client workload's begin/end markers (its own stdout).
const WORKLOAD_END: &[u8] = b"K8S49: workload end";
/// The deterministic prefix of the final workload row (iteration 20): `row`, loop
/// index 20, running `count(*)` = 20, running `sum(i)` = 210 — the SAME anchor the
/// other `live_postgres*` gates pin. The `uuid|t` that FOLLOW it
/// (`row|20|20|210|<uuid>|<t>`) are seed-derived (checked by shape, not value).
const FINAL_ROW_PREFIX: &[u8] = b"row|20|20|210|";
/// The `k3s-init.sh` line carrying the two pod IPs (the intra-guest CNI witness).
const POD_IPS_PREFIX: &[u8] = b"CNI pod IPs: postgres=";
/// The postgres connection log line (proves a TCP connection was accepted over the
/// CNI; its `host=` is the client pod's source IP).
const CONN_LOG: &[u8] = b"connection received: host=";
/// `k3s-init.sh` prints this after a clean shutdown.
const GUEST_READY: &[u8] = b"GUEST_READY";

/// The fixed iteration count of the workload loop (`WORKLOAD_N` in
/// `build-k3s-image.sh`): every run streams exactly this many `row|…` lines.
const WORKLOAD_N: usize = 20;
/// The Kubernetes pod CIDR (k3s default) — both pod IPs must be in it, proving the
/// client->server traffic is intra-guest over the CNI (no host networking).
const POD_CIDR_PREFIX: &str = "10.42.";

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
         box: `make -C guest fetch && make -C guest/linux k3s-image`."
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

fn guest_ram_len() -> usize {
    let gib = std::env::var("GUEST_RAM_GIB")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(GUEST_RAM_GIB_DEFAULT);
    gib << 30
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
/// canonical offsets. A lightweight shape check (no `uuid` crate).
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
/// `timestamptz` text form).
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

/// Parse the final workload row (`row|20|20|210|<uuid>|<t>`): return its `(uuid, t)`.
fn final_row_uuid_ts(serial: &[u8]) -> Option<(String, String)> {
    let line = line_with_prefix(serial, FINAL_ROW_PREFIX)?;
    let fields: Vec<&str> = line.split('|').collect();
    if fields.len() != 6 {
        return None;
    }
    Some((fields[4].to_string(), fields[5].to_string()))
}

/// Every per-iteration row's UUID (field 5 of each `row|…` line that parses as a UUID).
fn all_row_uuids(serial: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(serial)
        .lines()
        .filter(|l| l.starts_with("row|"))
        .filter_map(|l| l.split('|').nth(4).map(str::to_string))
        .filter(|u| is_uuid(u))
        .collect()
}

/// Parse the `K8S49: CNI pod IPs: postgres=<ip> client=<ip> (...)` line into the two
/// pod IPs. `None` if absent/malformed.
fn pod_ips(serial: &[u8]) -> Option<(String, String)> {
    let line = line_with_prefix(serial, POD_IPS_PREFIX)?;
    // ...: postgres=<ip> client=<ip> (...)
    let pg = line.split("postgres=").nth(1)?.split_whitespace().next()?;
    let cl = line.split("client=").nth(1)?.split_whitespace().next()?;
    Some((pg.to_string(), cl.to_string()))
}

/// What a bounded run observed.
struct BootOutcome {
    reason: Option<TerminalReason>,
    steps: u64,
    /// The single-node k3s cluster reached Ready.
    cluster_up: bool,
    /// The postgres pod reached Running + Ready.
    postgres_ready: bool,
    /// The client pod connected to the postgres pod over the CNI.
    client_connected: bool,
    workload_done: bool,
    /// The deterministic final-row prefix `row|20|20|210|` reached the serial.
    final_row: bool,
    /// The final row's seed-derived `(uuid, t)` fields, if streamed + parsed.
    sample_uuid_ts: Option<(String, String)>,
    /// Every per-iteration UUID streamed (distinctness / not-a-constant check).
    row_uuids: Vec<String>,
    /// The two pod IPs `(postgres, client)`, if the witness line was streamed.
    pod_ips: Option<(String, String)>,
    /// A postgres `connection received: host=` log line reached the serial.
    conn_log: bool,
    guest_ready: bool,
    step_error: Option<String>,
}

impl BootOutcome {
    fn terminated_cleanly(&self) -> bool {
        self.reason.is_some() && self.step_error.is_none()
    }
    /// Both pod IPs are in the pod CIDR (`10.42.0.0/16`): the proof the
    /// client->server traffic is intra-guest over the CNI (no host networking).
    fn pods_intra_guest(&self) -> bool {
        self.pod_ips.as_ref().is_some_and(|(pg, cl)| {
            pg.starts_with(POD_CIDR_PREFIX) && cl.starts_with(POD_CIDR_PREFIX)
        })
    }
}

/// Drive `vmm` to a terminal state (or the step / wall-clock budget), streaming the
/// serial console to stderr live so a hang shows the last line reached.
///
/// `rec` is the **out-of-band, read-only** telemetry recording (an
/// [`NdjsonRecorder`] over a file; [`None`] if it could not be opened). It is fed
/// ONLY from `vmm.serial()` (already read for the live stream) and
/// `vmm.exit_counts()` (a read-only accessor) and writes its own file — it never
/// reads or feeds `state_hash`/`observable_digest`, so attaching it CANNOT perturb
/// the deterministic run (the `Observer` read-only contract). A viewer artifact.
fn run_bounded<B: vmm_backend::Backend>(
    vmm: &mut Vmm<B>,
    rec: &mut Option<Recorder>,
) -> BootOutcome {
    // not order-observable: a test-only wall-clock watchdog (belt-and-braces with
    // the external `timeout`) — it bounds how long this `#[ignore]`d box gate runs
    // and never reaches guest state, the serial capture, or any hash.
    #[allow(clippy::disallowed_methods)]
    let start = Instant::now();
    let budget = wall_budget();
    let mut printed = 0usize;
    let mut steps = 0u64;
    let mut ev_seq = 0u64; // per-run monotonic telemetry event counter
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
                eprintln!("\n[k3s] step error after {steps} steps: {e}  | debug={e:?}");
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
            let new = &serial[printed..];
            let mut h = stderr.lock();
            let _ = h.write_all(new);
            let _ = h.flush();
            // Record the new console bytes as a Console event (display fidelity
            // only; the byte-exact capture is `vmm.serial()`, hashed elsewhere).
            if let Some(r) = rec.as_mut() {
                r.emit(&Event::new(
                    ev_seq,
                    steps,
                    steps,
                    EventKind::Console {
                        text: String::from_utf8_lossy(new).into_owned(),
                    },
                ));
                ev_seq += 1;
            }
            printed = serial.len();
        }
        if steps.is_multiple_of(COUNTS_EVERY) {
            // Snapshot the per-reason exit tally into the recording (read-only
            // accessor) — drives the console's exit-rate counters/graph.
            if let Some(r) = rec.as_mut() {
                r.emit(&Event::new(
                    ev_seq,
                    steps,
                    steps,
                    EventKind::Counts(map_counts(&vmm.exit_counts())),
                ));
                ev_seq += 1;
            }
            if start.elapsed() > budget {
                eprintln!("\n[k3s] wall-clock budget exceeded after {steps} steps");
                break;
            }
        }
    }
    // A Terminal event closes the recording with the human-readable outcome.
    if let Some(r) = rec.as_mut() {
        let reason_str = if let Some(e) = &step_error {
            format!("step error after {steps} steps: {e}")
        } else if let Some(t) = &reason {
            format!("terminal: {t:?} after {steps} steps")
        } else {
            format!("budget/step-cap reached after {steps} steps")
        };
        r.emit(&Event::new(
            ev_seq,
            steps,
            steps,
            EventKind::Terminal { reason: reason_str },
        ));
    }
    let serial = vmm.serial();
    BootOutcome {
        reason,
        steps,
        cluster_up: find(serial, CLUSTER_UP),
        postgres_ready: find(serial, POSTGRES_READY),
        client_connected: find(serial, CLIENT_CONNECTED),
        workload_done: find(serial, WORKLOAD_END),
        final_row: find(serial, FINAL_ROW_PREFIX),
        sample_uuid_ts: final_row_uuid_ts(serial),
        row_uuids: all_row_uuids(serial),
        pod_ips: pod_ips(serial),
        conn_log: find(serial, CONN_LOG),
        guest_ready: find(serial, GUEST_READY),
        step_error,
    }
}

/// Map `vmm-backend`'s per-reason exit tally into the telemetry crate's mirror
/// (defined separately there, a leaf crate — conventions rule 2). Field-for-field.
fn map_counts(c: &vmm_backend::ExitCounts) -> telemetry::ExitCounts {
    telemetry::ExitCounts {
        io: c.io,
        mmio: c.mmio,
        rdmsr: c.rdmsr,
        wrmsr: c.wrmsr,
        hypercall: c.hypercall,
        cpuid: c.cpuid,
        rdtsc: c.rdtsc,
        rdtscp: c.rdtscp,
        rdrand: c.rdrand,
        rdseed: c.rdseed,
        hlt: c.idle,
        shutdown: c.shutdown,
        deadline: c.deadline,
    }
}

/// Open the out-of-band telemetry recording for a boot tagged `tag`. The file is
/// `<base>.<tag>.ndjson`, where `<base>` is `$K3S_NDJSON` (default `k3s-run`, i.e.
/// relative to the run's CWD — `/root/ht49` under the box wrapper). Always-on for
/// this `#[ignore]`d box gate; a viewer artifact only, so a failure to open it is a
/// warning, never a test failure. Prints the `console` replay command.
fn open_recorder(tag: &str) -> Option<Recorder> {
    let base = std::env::var("K3S_NDJSON").unwrap_or_else(|_| "k3s-run".to_string());
    let path = format!("{base}.{tag}.ndjson");
    match File::create(&path) {
        Ok(f) => {
            eprintln!(
                "[telemetry] recording this run to {path}\n[telemetry]   watch it: \
                 cargo run -p telemetry --bin console -- --source file:{path}"
            );
            Some(NdjsonRecorder::new(BufWriter::new(f)))
        }
        Err(e) => {
            eprintln!("[telemetry] could not open recording {path}: {e} (continuing without)");
            None
        }
    }
}

/// Boot the k3s image on the patched backend at `seed`, run it to a terminal, and
/// return (serial capture, `state_hash`, outcome). `tag` names the per-boot
/// telemetry recording (a viewer artifact; see [`open_recorder`]). As in
/// `live_runc_postgres.rs` the [`Vmm`] — and its `perf_event` work counter — is
/// **dropped before returning**, so two same-seed runs in one process don't keep two
/// pinned PMU counters open at once (which would multiplex and perturb the branch
/// count). One counter at a time is exact.
fn boot_k3s(seed: u64, tag: &str) -> (Vec<u8>, [u8; 32], BootOutcome) {
    let kernel = require_artifact("bzImage");
    let initramfs = require_artifact("initramfs-k3s.cpio.gz");
    let cmdline = cmdline();
    let mut vmm = boot_linux_selected(
        BackendKind::Patched,
        &kernel,
        &initramfs,
        guest_ram_len(),
        &cmdline,
        seed,
    )
    .expect("boot_linux_selected (patched) — needs the LOADED patched KVM modules");
    let mut rec = open_recorder(tag);
    let out = run_bounded(&mut vmm, &mut rec);
    if let Some(r) = rec.as_mut() {
        let _ = r.flush();
        if let Some(e) = r.error() {
            eprintln!(
                "[telemetry] recording `{tag}` had a write error (viewer artifact only): {e}"
            );
        }
    }
    (vmm.serial().to_vec(), vmm.state_hash(), out)
}

fn report(tag: &str, out: &BootOutcome) {
    eprintln!(
        "\n[{tag}] steps={} terminal={:?} cluster_up={} postgres_ready={} client_connected={} \
         workload_done={} final_row={} uuids={} pod_ips={:?} conn_log={} GUEST_READY={} \
         step_error={:?}",
        out.steps,
        out.reason,
        out.cluster_up,
        out.postgres_ready,
        out.client_connected,
        out.workload_done,
        out.final_row,
        out.row_uuids.len(),
        out.pod_ips,
        out.conn_log,
        out.guest_ready,
        out.step_error,
    );
    if let Some((uuid, t)) = &out.sample_uuid_ts {
        eprintln!("[{tag}] final-row sample: uuid={uuid} t={t}");
    }
}

/// Assert the workload's UUID/time columns are well-formed in `out`: the final row
/// carries a valid UUID + timestamp, all [`WORKLOAD_N`] per-iteration UUIDs reached
/// the serial, and they are pairwise distinct. Returns the final row's sample UUID.
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
        "{tag}: the {WORKLOAD_N} streamed UUIDs must be pairwise distinct — only {} were distinct",
        distinct.len()
    );
    uuid
}

/// Assert `out` ran the whole cluster path and the client->server call stayed
/// **intra-guest over the CNI**: cluster up, postgres ready, client connected, both
/// pod IPs in the pod CIDR, and a postgres connection log line present.
fn assert_intra_guest_cluster(tag: &str, out: &BootOutcome) {
    assert!(
        out.cluster_up,
        "{tag}: the single-node k3s cluster must reach Ready (CLUSTER_UP)"
    );
    assert!(
        out.postgres_ready,
        "{tag}: the postgres pod must reach Running + Ready (POSTGRES_READY)"
    );
    assert!(
        out.client_connected,
        "{tag}: the client pod must connect to the postgres pod over the CNI"
    );
    assert!(
        out.pods_intra_guest(),
        "{tag}: both pod IPs must be in the pod CIDR {POD_CIDR_PREFIX}0.0/16 (intra-guest CNI, no \
         host networking) — got {:?}",
        out.pod_ips
    );
    assert!(
        out.conn_log,
        "{tag}: the postgres pod must log a `connection received: host=` line (a real TCP \
         connection accepted over the CNI)"
    );
}

// --- Gate k1: cluster up + both pods + intra-guest client->server + streams --

/// **Gate `k1`.** One patched boot reaches a Ready single-node k3s cluster, both pods
/// run, the client connects to the postgres pod over the CNI, runs the workload
/// (the `row|…` results reach `ttyS0`, each with a valid UUID + timestamp), and the
/// guest powers off cleanly within budget.
#[test]
#[ignore = "box-only live gate (LOADED patched KVM + built k3s image + det-cfl-v1 host); \
            run on `ssh <det-box>` with `-- --ignored --nocapture`"]
fn k1_k3s_cluster_postgres_client_streams_patched() {
    require_kvm();
    require_host_baseline();
    eprintln!("[k3s] cmdline: {}", cmdline());
    let (_serial, _hash, out) = boot_k3s(SEED, "k1");
    report("k1", &out);
    assert!(
        out.step_error.is_none(),
        "Gate k1: the VMM must not trip a contract violation during the run — got {:?} after {} steps",
        out.step_error,
        out.steps,
    );
    assert!(
        out.reason.is_some(),
        "Gate k1: must reach a terminal, not hang ({} steps)",
        out.steps
    );
    assert_intra_guest_cluster("Gate k1", &out);
    assert!(
        out.workload_done,
        "Gate k1: the client workload must run to completion (K8S49: workload end)"
    );
    assert!(
        out.final_row,
        "Gate k1: the deterministic final workload row (row|20|20|210|…) must reach the serial"
    );
    let sample = assert_uuid_time_shape("Gate k1", &out);
    eprintln!("[k1] sample UUID (seed {SEED:#018x}): {sample}");
    if let Some((pg, cl)) = &out.pod_ips {
        eprintln!("[k1] intra-guest CNI: postgres pod {pg} <- client pod {cl}");
    }
    assert!(
        out.guest_ready,
        "Gate k1: the guest must announce GUEST_READY after a clean shutdown"
    );
    assert!(
        out.terminated_cleanly(),
        "Gate k1: the guest must power off cleanly within budget"
    );
}

// --- Gate k2: deterministic twice (the milestone) ---------------------------

/// **Gate `k2` — deterministic twice (the milestone).** Two same-seed patched boots
/// of the whole k3s + client->server stack produce a bit-identical serial capture
/// (incl. the UUIDs + timestamps) **and** `state_hash`.
#[test]
#[ignore = "MILESTONE gate (task 49): same-seed bit-identical k3s Postgres client->server run; run \
            on the box with the LOADED patched KVM and `-- --ignored --nocapture`"]
fn k2_k3s_postgres_deterministic_twice_patched() {
    require_kvm();
    require_host_baseline();

    // boot_k3s drops run A's Vmm (and its PMU counter) before we boot run B.
    let (serial_a, hash_a, out_a) = boot_k3s(SEED, "k2_run_a");
    report("k2 run A", &out_a);
    let (serial_b, hash_b, out_b) = boot_k3s(SEED, "k2_run_b");
    report("k2 run B", &out_b);

    let hex = |h: &[u8; 32]| h.iter().map(|b| format!("{b:02x}")).collect::<String>();
    eprintln!(
        "[k3s] determinism: serial_len A/B = {}/{}\n  state_hash A = {}\n  state_hash B = {}",
        serial_a.len(),
        serial_b.len(),
        hex(&hash_a),
        hex(&hash_b),
    );

    // Both runs must actually have run the whole intra-guest path to GUEST_READY,
    // so two identical-but-stranded boots cannot pass vacuously — and each must
    // carry well-formed, distinct UUIDs + timestamps (so the bit-identity below is
    // over *real* random/wall-clock columns, not a constant or an error string).
    for (tag, out) in [("A", &out_a), ("B", &out_b)] {
        assert!(
            out.step_error.is_none(),
            "Gate k2 run {tag}: contract violation: {:?}",
            out.step_error
        );
        assert_intra_guest_cluster(&format!("Gate k2 run {tag}"), out);
        assert!(
            out.final_row,
            "Gate k2 run {tag}: the deterministic final workload row (row|20|20|210|…) must reach the serial"
        );
        assert!(out.guest_ready, "Gate k2 run {tag}: must reach GUEST_READY");
        assert_uuid_time_shape(&format!("Gate k2 run {tag}"), out);
    }
    assert_eq!(
        serial_a, serial_b,
        "Gate k2: two same-seed patched boots through the k3s stack must produce a bit-identical \
         serial capture (this is what proves the UUIDs + timestamps — and the whole k8s Go-runtime \
         interleaving the preemption timer now drives — are bit-identical across same-seed runs; if \
         anything escaped, the serials would differ here: a real determinization finding, not a flake)"
    );
    assert_eq!(
        hash_a, hash_b,
        "Gate k2: two same-seed patched boots through the k3s stack must produce an identical state_hash"
    );

    let (uuid, t) = out_a
        .sample_uuid_ts
        .clone()
        .expect("final row parsed (asserted above)");
    eprintln!(
        "[k3s] deterministic-twice witness (seed {SEED:#018x}): the random-looking UUID + \
         wall-clock timestamp are bit-identical in run A and run B, through the k3s cluster —\n  \
         uuid = {uuid}\n  t    = {t}"
    );
}

// --- Gate k3: seed-sensitivity ----------------------------------------------

/// **Gate `k3` — seed-sensitivity.** A run at [`SEED`] and a run at a *different*
/// seed [`SEED_B`] must produce *different* UUIDs through the cluster — proving they
/// are genuinely driven by the seeded CRNG (`gen_random_uuid()` -> `pg_strong_random`),
/// not a frozen constant that would sail through Gate `k2` vacuously.
#[test]
#[ignore = "box-only seed-sensitivity gate (task 42/49): different seed ⇒ different UUIDs through \
            the k3s cluster; run on the box with the LOADED patched KVM and `-- --ignored --nocapture`"]
fn k3_k3s_postgres_seed_sensitivity_patched() {
    require_kvm();
    require_host_baseline();

    let (_serial_a, _hash_a, out_a) = boot_k3s(SEED, "k3_seed_a");
    report("k3 seed A", &out_a);
    let (_serial_b, _hash_b, out_b) = boot_k3s(SEED_B, "k3_seed_b");
    report("k3 seed B", &out_b);

    // Each run must genuinely have gone through the intra-guest cluster path (else
    // "different UUIDs" could come from a non-cluster path — not what this proves).
    assert_intra_guest_cluster("Gate k3 seed A", &out_a);
    assert_intra_guest_cluster("Gate k3 seed B", &out_b);

    let uuid_a = assert_uuid_time_shape(&format!("Gate k3 seed A ({SEED:#018x})"), &out_a);
    let uuid_b = assert_uuid_time_shape(&format!("Gate k3 seed B ({SEED_B:#018x})"), &out_b);
    eprintln!(
        "[k3s] seed-sensitivity: sample UUID per seed —\n  seed {SEED:#018x} -> {uuid_a}\n  \
         seed {SEED_B:#018x} -> {uuid_b}"
    );
    assert_ne!(
        uuid_a, uuid_b,
        "Gate k3: a different seed must produce a different UUID — gen_random_uuid() draws from the \
         seeded CRNG; identical UUIDs across seeds would mean it is a frozen constant"
    );
}
