// SPDX-License-Identifier: AGPL-3.0-or-later
//! Box-only task-110 gates for the paravirt work-derived clock
//! (`docs/PARAVIRT-CLOCK.md` §6): **G1** same-seed bit-identical `state_hash`
//! with the page on, **G2** page-stamp == RDTSC-trap-oracle function equality
//! at refresh Moments, **G3** busy-wait-on-time liveness within Δ, and the
//! **N-4-style perf measurement** (RDTSC-exit rate page-off vs page-on, boot
//! wall ratio) that judges kill condition 3 — reported honestly either way,
//! never asserted into a pass.
//!
//! Portable analogues of G1/G2/G3 (mock backend + `ScriptedWork`, incl. the
//! mandated deliberate-fault coverage) live in `src/vmm.rs`; this file is the
//! real-KVM half.
//!
//! # Environment (everything box-specific, in one place)
//!
//! - **Host**: the determinism box (`ssh hetzner`), det-cfl-v1 CPU, LOADED
//!   patched KVM modules (`KVM_CAP_X86_DETERMINISTIC_INTERCEPTS`), perf_event;
//!   CPU-pinned per `docs/BOX-PINNING.md`:
//!   `taskset -c 2 cargo test -p vmm-core --release --test live_pvclock -- --ignored --test-threads=1`
//! - **Kernel image**: the task-110 pvclock build —
//!   `make -C guest fetch && make -C guest/linux kernel` (applies the kernel
//!   diff under `patches/`, runs the armed counter-opcode scan). Pinned
//!   against `guest/linux/MANIFEST.sha256` (regenerate + commit via
//!   `guest/linux/run-tests.sh` after the first box build); override
//!   deliberately with `BZIMAGE_SHA256=<hex>` (hm-xdp: never a bare path).
//! - **Initramfs images**: minimal `initramfs.cpio.gz` (MANIFEST-pinned),
//!   Postgres `initramfs-postgres.cpio.gz` (const-pinned, the task-78-proven
//!   build), exec `initramfs-exec.cpio.gz` (`make -C guest/linux exec-image`;
//!   supply `INITRAMFS_EXEC_SHA256=<hex>` — no committed pin yet).
//! - **Knobs**: `PVCLOCK_DELTA_WORK` (Δ, default
//!   [`vmm_core::vmm::PVCLOCK_DEFAULT_DELTA_WORK`]), `BOOT_CMDLINE` (base
//!   cmdline; the page-on arm appends ` harmony_pvclock` itself),
//!   `PV_G1_FIRST_VNS`/`PV_G1_STEP_VNS`/`PV_G1_SEALS` (the G1 seal schedule),
//!   `PV_PERF_WINDOW_VNS` (the Postgres steady-state measurement window).
//!
//! **Smoke-fire-once**: `g0_smoke_boot_registers_and_reads_sane_time` is the
//! minutes-long probe of the riskiest live assumptions (kernel builds, guest
//! registers the page, reads sane time, reaches `GUEST_READY`) — run it (and
//! only it) before spending the G1/perf budget.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::time::Duration;

use control_proto::{
    HashScope, Moment, Reply, Reproducer, Request, SnapId, StopConditions, StopMask, StopReason,
};
use environment::{EnvSpec, FaultPolicy};
use vmm_backend::{Backend, X86};
use vmm_core::control::{ControlServer, server_caps};
use vmm_core::exec::ExecSession;
use vmm_core::vendor::x86::bringup::{BackendKind, boot_linux_selected};
use vmm_core::vmm::{PVCLOCK_DEFAULT_DELTA_WORK, TerminalReason, Vmm};

type DynVmm = Vmm<Box<dyn Backend<A = X86>>>;

const GUEST_RAM_LEN: usize = 2 << 30;
const SEED: u64 = 0x0110_5EED_C10C_4B17;
/// The kernel's clocksource-SELECTION line: Linux prints this once it makes the
/// harmony page the ACTIVE timekeeping source — not merely registers it. A page
/// that registers but fails selection (a rating/initcall regression) would keep
/// the guest on the TSC, so a perf arm that checked only registration could emit
/// a "page-on" ratio for an effectively page-OFF run (r19).
const CLOCKSOURCE_SWITCH_MARKER: &[u8] = b"Switched to clocksource harmony-pvclock";
const DEFAULT_CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t,force tsc=reliable \
     no_timer_check lpj=4000000 nokaslr nosmp maxcpus=1 nox2apic hpet=disable";

/// The task-78-proven Postgres initramfs (the `/root/harmony-pr44` build; see
/// `live_dirty_remap.rs` for the pin-by-content-hash ruling, hm-xdp).
const PINNED_PG_INITRAMFS_SHA256: &str =
    "3c4a7f2f0db4b59aaf4dee55d43a42c57fc0d10ac25441de88128c61be0778c2";

fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn require_kvm() {
    assert!(
        std::path::Path::new("/dev/kvm").exists(),
        "/dev/kvm absent — run this `#[ignore]`d box gate on the determinism box with the \
         LOADED patched KVM modules, CPU-pinned per docs/BOX-PINNING.md."
    );
}

fn require_host_baseline() {
    let report = vmm_core::vendor::x86::hostassert::report();
    let mut all = true;
    for o in &report {
        if !o.pass {
            eprintln!(
                "[host-assert] FAIL {}: expected {}, observed {}",
                o.key, o.expected, o.actual
            );
        }
        all &= o.pass;
    }
    assert!(
        all,
        "host CPU is not the det-cfl-v1 baseline — run on the determinism box."
    );
}

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
        "guest artifact `{name}` not found in guest/build or guest/linux — build it on the box \
         first (`make -C guest fetch && make -C guest/linux kernel` + the image target; see the \
         Environment section of this file)."
    );
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

fn verify_pin(name: &str, bytes: &[u8], expected_sha256: &str) {
    let observed = sha256_hex(bytes);
    assert_eq!(
        observed, expected_sha256,
        "guest artifact `{name}` does not match its pinned content hash (hm-xdp: gates \
         reference images BY HASH, never a mutable path). Rebuild the pinned artifact, \
         regenerate+commit guest/linux/MANIFEST.sha256, or override DELIBERATELY via the \
         *_SHA256 env vars."
    );
}

/// The committed `guest/linux/MANIFEST.sha256` pin for `name`, if present.
fn manifest_pin(name: &str) -> Option<String> {
    let manifest = std::fs::read_to_string(repo_root().join("guest/linux/MANIFEST.sha256")).ok()?;
    manifest.lines().find_map(|l| {
        let mut it = l.split_whitespace();
        let hash = it.next()?;
        (it.next()? == name).then(|| hash.to_string())
    })
}

/// Load + pin-verify an artifact: env override (`var`) wins, else the
/// MANIFEST pin, else a loud refusal (never an unpinned run).
fn pinned_artifact(name: &str, var: &str) -> Vec<u8> {
    let bytes = require_artifact(name);
    let pin = std::env::var(var)
        .ok()
        .or_else(|| manifest_pin(name))
        .unwrap_or_else(|| {
            panic!(
                "no content pin for `{name}`: not in guest/linux/MANIFEST.sha256 and {var} not \
                 set. After the first box build, run guest/linux/run-tests.sh to regenerate the \
                 MANIFEST and commit it — or supply {var}=<sha256> deliberately. Observed hash \
                 of the staged file (verify before trusting!): {}",
                sha256_hex(&bytes)
            )
        });
    verify_pin(name, &bytes, &pin);
    bytes
}

/// The pvclock kernel (MANIFEST-pinned; `BZIMAGE_SHA256` overrides).
fn pvclock_kernel() -> Vec<u8> {
    pinned_artifact("bzImage", "BZIMAGE_SHA256")
}

/// The minimal boot-and-poweroff initramfs (MANIFEST-pinned).
fn minimal_initramfs() -> Vec<u8> {
    pinned_artifact("initramfs.cpio.gz", "INITRAMFS_MIN_SHA256")
}

/// The Postgres workload initramfs (const-pinned; env-overridable).
fn pg_initramfs() -> Vec<u8> {
    let bytes = require_artifact("initramfs-postgres.cpio.gz");
    let pin = std::env::var("INITRAMFS_PG_SHA256")
        .unwrap_or_else(|_| PINNED_PG_INITRAMFS_SHA256.to_string());
    verify_pin("initramfs-postgres.cpio.gz", &bytes, &pin);
    bytes
}

/// The exec-capable initramfs (`INITRAMFS_EXEC_SHA256` required — no
/// committed pin yet; the panic quotes the staged file's hash to review).
fn exec_initramfs() -> Vec<u8> {
    pinned_artifact("initramfs-exec.cpio.gz", "INITRAMFS_EXEC_SHA256")
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn delta_work() -> u64 {
    env_u64("PVCLOCK_DELTA_WORK", PVCLOCK_DEFAULT_DELTA_WORK)
}

/// The guest's periodic tick interval in counted work units: `CONFIG_HZ=100` ⇒
/// 10 ms, and the contract clock counts ≈1 work unit per V-time ns (which is why
/// [`PVCLOCK_DEFAULT_DELTA_WORK`] = 10_000_000 is documented as "≈ 10 ms").
const GUEST_TICK_WORK: u64 = 10_000_000;

/// **G3's Δ: deliberately BELOW the guest tick** (cross-model r4 P1). At the
/// default Δ ≈ 10 ms the 100 Hz tick already forces a `Deadline` — and hence a
/// page refresh — about every 10 ms, so `max_gap ≤ Δ` would hold with the
/// forced-refresh deadline *deleted* and the gate would pass vacuously. A Δ of
/// one tenth of a tick cannot be met by the tick: ten of every eleven refreshes
/// must come from the Δ bound itself, and [`Vmm::pvclock_forced_landings`]
/// counts them, so G3 now fails in both ways if the mechanism is removed.
fn g3_delta_work() -> u64 {
    env_u64("PV_G3_DELTA_WORK", GUEST_TICK_WORK / 10)
}

fn base_cmdline() -> String {
    std::env::var("BOOT_CMDLINE").unwrap_or_else(|_| DEFAULT_CMDLINE.to_string())
}

fn hex(d: &[u8; 32]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// Boot the pvclock kernel on patched KVM. `page_on` is the ONE knob: it
/// appends ` harmony_pvclock` to the cmdline (the host's offer advertisement)
/// and offers the page host-side (`enable_pvclock`). One body, one knob — the
/// A/B arms cannot drift apart in wiring.
fn boot(kernel: &[u8], initramfs: &[u8], seed: u64, page_on: bool) -> DynVmm {
    boot_with_delta(kernel, initramfs, seed, page_on, delta_work())
}

/// [`boot`] with an explicit Δ — G3 runs below the guest tick (see
/// [`g3_delta_work`]); every other gate takes the documented default.
fn boot_with_delta(
    kernel: &[u8],
    initramfs: &[u8],
    seed: u64,
    page_on: bool,
    delta: u64,
) -> DynVmm {
    let cmdline = if page_on {
        format!("{} harmony_pvclock", base_cmdline())
    } else {
        base_cmdline()
    };
    let mut vmm = boot_linux_selected(
        BackendKind::Patched,
        kernel,
        initramfs,
        GUEST_RAM_LEN,
        &cmdline,
        seed,
    )
    .expect("patched Linux boot — needs the LOADED patched KVM + perf + det-cfl-v1 host");
    if page_on {
        vmm.enable_pvclock(delta);
    }
    vmm
}

/// What a bounded direct-drive run observed.
struct RunObs {
    reason: Option<TerminalReason>,
    steps: u64,
    wall: Duration,
    step_error: Option<String>,
}

/// Drive `vmm` to a terminal (or the step/wall budget), invoking `on_step`
/// after every step (for mid-run oracle checks / serial scans); `on_step`
/// returning `false` stops the run early (e.g. "reached GUEST_READY", "the
/// measurement window closed"). The wall budget bounds an `#[ignore]`d box
/// gate even if the guest hangs.
fn run_bounded(
    vmm: &mut DynVmm,
    max_steps: u64,
    wall_budget: Duration,
    mut on_step: impl FnMut(&mut DynVmm, u64) -> bool,
) -> RunObs {
    #[allow(clippy::disallowed_methods)] // test-only budget; never reaches state/hash
    let t0 = std::time::Instant::now();
    let mut steps = 0u64;
    let mut reason = None;
    let mut step_error = None;
    while steps < max_steps && t0.elapsed() < wall_budget {
        match vmm.step() {
            Ok(vmm_core::vmm::Step::Terminal(r)) => {
                reason = Some(r);
                break;
            }
            Ok(_) => {}
            Err(e) => {
                step_error = Some(format!("{e}"));
                break;
            }
        }
        steps += 1;
        if !on_step(vmm, steps) {
            break;
        }
    }
    RunObs {
        reason,
        steps,
        wall: t0.elapsed(),
        step_error,
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

/// A wide backstop on the bounded drive to the next synchronized boundary in
/// [`sample_at_sync`]: a live guest reaches a V-time intercept within a tick
/// (≈10 ms) or Δ, so this only bounds a wedged guest.
const SYNC_SAMPLE_CAP: u64 = 10_000_000;

/// Sample the perf denominators — the exit counters AND V-time — at a
/// SYNCHRONIZED boundary (cross-model r15 P2). At a marker stop the guest has
/// usually just taken a serial PIO exit, where `effective_vns()` is only a
/// last-intercept LOWER BOUND; and the two arms exit on different intercept
/// mixes (page-off takes far more RDTSC intercepts than page-on), so that
/// lower-bound gap is NOT common between them and biases the reported
/// rate/reduction. Advancing to the next synchronized intercept — where
/// `effective_vns` is exact and the counters and the clock agree — removes the
/// bias. Samples in place if already synchronized (never steps past a boundary
/// that already holds).
fn sample_at_sync(vmm: &mut DynVmm) -> (vmm_backend::ExitCounts, u64) {
    if !vmm.is_synchronized() {
        let _ = run_bounded(vmm, SYNC_SAMPLE_CAP, Duration::from_secs(120), |vmm, _| {
            !vmm.is_synchronized()
        });
    }
    // FAIL rather than sample a lower bound (cross-model r16 P2): if the drive
    // could not reach a synchronized boundary (the guest terminated or wedged
    // first), `effective_vns()` is a last-intercept LOWER BOUND, and returning it
    // silently would re-introduce the very per-arm bias r15 removed. A vacuous
    // "denominator" is worse than a loud gate failure.
    assert!(
        vmm.is_synchronized(),
        "sample_at_sync could not reach a synchronized V-time boundary within {SYNC_SAMPLE_CAP} \
         steps — the guest terminated or wedged before an intercept. Refusing to sample the \
         last-intercept lower-bound V-time (it would bias the reported rate/reduction)."
    );
    (vmm.exit_counts(), vmm.effective_vns().unwrap_or(0))
}

// ---------------------------------------------------------------------------
// G0 — the smoke-fire-once probe (run FIRST, alone, before any long gate).
// ---------------------------------------------------------------------------

/// The riskiest live assumptions, probed in minutes: the pvclock kernel
/// boots, the guest registers the page over the doorbell, the page publishes
/// sane monotonic time that tracks the trap oracle, and the guest still
/// reaches `GUEST_READY` + clean poweroff.
#[test]
#[ignore = "box-only: needs /dev/kvm (patched), perf_event, det-cfl-v1, and the pvclock kernel build (see the Environment section)"]
fn g0_smoke_boot_registers_and_reads_sane_time() {
    require_kvm();
    require_host_baseline();
    let kernel = pvclock_kernel();
    let initramfs = minimal_initramfs();

    let mut vmm = boot(&kernel, &initramfs, SEED, true);
    let obs = run_bounded(&mut vmm, 50_000_000, Duration::from_secs(600), |_, _| true);

    let serial = String::from_utf8_lossy(vmm.serial()).into_owned();
    eprintln!(
        "[smoke] steps={} wall={:?} reason={:?}",
        obs.steps, obs.wall, obs.reason
    );
    // Surface the registration line (`harmony_pvclock:`) AND the kernel's
    // clocksource-selection line (`Switched to clocksource harmony-pvclock`) — the
    // two are spelled differently (underscore vs hyphen), and confirming the
    // *switch*, not just registration, is what tells the perf story apart (page
    // selected as the timekeeping source vs merely registered-and-unused).
    for line in serial
        .lines()
        .filter(|l| l.contains("harmony_pvclock") || l.contains("clocksource"))
    {
        eprintln!("[smoke] guest: {line}");
    }
    assert!(obs.step_error.is_none(), "step error: {:?}", obs.step_error);
    assert!(
        vmm.pvclock_registration().is_some(),
        "the guest never registered the clock page — check the guest 'harmony_pvclock:' log \
         lines above (doorbell/ABI mismatch?)"
    );
    // r13 P2: registration is NOT selection. If the page registers and stamps but
    // Linux keeps timekeeping on the TSC, every other G0/G3/perf assertion can
    // still pass (they read the page directly or only report a ratio) while the
    // guest never actually uses the clock — the RDTSC exits the perf lever counts
    // on never leave the hot path. Require the kernel's clocksource-SWITCH line, so
    // a TSC-still-active guest fails the smoke gate loudly.
    assert!(
        find(serial.as_bytes(), CLOCKSOURCE_SWITCH_MARKER),
        "the guest registered the page but never SELECTED harmony-pvclock as its \
         active clocksource — it is still on the TSC (registered-but-unused). Check \
         the guest 'clocksource' log lines above."
    );
    // The page tracks the oracle at the terminal boundary.
    vmm.pvclock_check_oracle()
        .expect("G2 oracle equality at the smoke terminal");
    // Sane, monotonic refresh evidence.
    let refreshes = vmm.pvclock_refreshes();
    assert!(
        refreshes.len() >= 2,
        "expected at least two page refreshes over a full boot; got {}",
        refreshes.len()
    );
    assert!(
        refreshes
            .windows(2)
            .all(|p| p[0].1 <= p[1].1 && p[0].2 <= p[1].2),
        "page-published time went backwards"
    );
    assert!(
        find(serial.as_bytes(), b"GUEST_READY"),
        "guest did not reach GUEST_READY with the page on"
    );
    // Clean poweroff = any terminal with no step error, matching the canonical
    // `live_linux_boot` gate's `terminated_cleanly()`. The minimal image's
    // `poweroff` surfaces as `Idle` (the kernel's post-poweroff `cli; hlt`,
    // task-52), NOT `Shutdown` — asserting `Shutdown` specifically was stricter
    // than the project's own definition of a clean end.
    assert!(
        matches!(
            obs.reason,
            Some(TerminalReason::Shutdown | TerminalReason::Idle)
        ),
        "unclean end: reason={:?} (expected a clean halt — Shutdown or the post-poweroff Idle)",
        obs.reason
    );
    eprintln!(
        "[REPORT] smoke: registered gpa={:#x?} refreshes={} vns_last={} RESULT: PASS",
        vmm.pvclock_registration().unwrap(),
        refreshes.len(),
        refreshes.last().unwrap().1
    );
}

// ---------------------------------------------------------------------------
// G1 — same-seed bit-identical state_hash, page on (the primary gate).
// ---------------------------------------------------------------------------

fn call<B: Backend<A = X86>>(
    s: &mut ControlServer<B>,
    req: &Request,
) -> Result<Reply, control_proto::ControlError> {
    s.handle(req)
        .expect("session-fatal ServeError from the control server")
}

fn expect_ok<B: Backend<A = X86>>(s: &mut ControlServer<B>, req: &Request) -> Reply {
    match call(s, req) {
        Ok(reply) => reply,
        Err(e) => panic!("verb {req:?} answered a ControlError: {e:?}"),
    }
}

fn run_until<B: Backend<A = X86>>(s: &mut ControlServer<B>, deadline: u64) -> StopReason {
    match expect_ok(
        s,
        &Request::Run {
            until: StopConditions {
                deadline: Some(Moment(deadline)),
                on: StopMask::NONE,
            },
            resolve: None,
        },
    ) {
        Reply::Stop(stop) => stop,
        other => panic!("run answered {other:?}"),
    }
}

fn hash_whole<B: Backend<A = X86>>(s: &mut ControlServer<B>) -> [u8; 32] {
    match expect_ok(
        s,
        &Request::Hash {
            scope: HashScope::Whole,
        },
    ) {
        Reply::Hash(h) => h,
        other => panic!("hash answered {other:?}"),
    }
}

fn seal_with_retry<B: Backend<A = X86>>(
    s: &mut ControlServer<B>,
    start_vt: u64,
    step: u64,
) -> (SnapId, u64) {
    let mut vt = start_vt;
    let mut attempts = 0usize;
    loop {
        attempts += 1;
        assert!(
            attempts < 100_000,
            "no snapshottable boundary within budget"
        );
        match call(s, &Request::Snapshot) {
            Ok(Reply::SnapId(id)) => return (id, vt),
            Ok(other) => panic!("snapshot answered {other:?}"),
            Err(control_proto::ControlError::NotQuiescent) => {
                match run_until(s, vt.saturating_add(step)) {
                    StopReason::Deadline { vtime } => vt = vtime.0,
                    other => panic!("guest ended before a sealable boundary: {other:?}"),
                }
            }
            Err(e) => panic!("snapshot answered a ControlError: {e:?}"),
        }
    }
}

#[allow(dead_code)]
fn seeded_env(seed: u64) -> Reproducer {
    Reproducer {
        blob_version: EnvSpec::BLOB_VERSION,
        bytes: EnvSpec::Seeded {
            seed,
            policy: FaultPolicy::none(),
        }
        .encode(),
    }
}

/// One G1 arm: boot the Postgres workload page-on, run to the shared seal
/// schedule, seal K times, hashing after each seal. Returns the hashes + the
/// registered GPA (the registration itself must be deterministic too).
fn g1_arm(kernel: &[u8], initramfs: &[u8], seals: u64, v0: u64, dv: u64) -> (Vec<[u8; 32]>, u64) {
    let vmm = boot(kernel, initramfs, SEED, true);
    let factory: vmm_core::control::VmmFactory<Box<dyn Backend<A = X86>>> = Box::new(|| {
        Err(vmm_core::vmm::VmmError::ContractViolation(
            "G1 never branches".into(),
        ))
    });
    let mut s = ControlServer::new(vmm, factory);
    // The server refuses every verb until the Hello handshake, so negotiate it
    // before driving Run/Snapshot (cross-model r6 P1) — otherwise the first
    // `Run` comes back `ControlError::Unsupported` and `expect_ok` panics before
    // the gate ever reaches its hash comparison.
    expect_ok(&mut s, &Request::Hello(server_caps()));
    let mut hashes = Vec::new();
    let mut vt = 0u64;
    for k in 0..seals {
        let target = v0 + k * dv;
        if vt < target {
            match run_until(&mut s, target) {
                StopReason::Deadline { vtime } => vt = vtime.0,
                other => panic!("guest ended before the seal schedule: {other:?}"),
            }
        }
        let (_, boundary) = seal_with_retry(&mut s, vt, dv / 10);
        vt = boundary;
        hashes.push(hash_whole(&mut s));
    }
    let gpa = s
        .vmm()
        .expect("session VM alive")
        .pvclock_registration()
        .expect("the Postgres guest registered the clock page");
    (hashes, gpa)
}

/// **G1.** Two same-seed Postgres boots with the page on produce
/// bit-identical `state_hash` at every sealed Moment — the refresh schedule +
/// §1.1 canonicalization leak zero entropy into guest RAM (which the hash
/// covers in full, page bytes included).
#[test]
#[ignore = "box-only (see g0); run g0 first — smoke-fire-once"]
fn g1_same_seed_state_hash_bit_identical_page_on() {
    require_kvm();
    require_host_baseline();
    let kernel = pvclock_kernel();
    let initramfs = pg_initramfs();
    // Defaults sized to the pinned Postgres artifact's ~0.46 virtual-second
    // lifetime (cross-model r8 P2): first seal at 0.1 s, then every 0.05 s, so
    // all three seals land while the guest is alive. A 2 s first seal — the old
    // default — reaches terminal before any seal and the DEFAULT G1 invocation
    // fails; override for a longer-lived image.
    let seals = env_u64("PV_G1_SEALS", 3);
    let v0 = env_u64("PV_G1_FIRST_VNS", 100_000_000);
    let dv = env_u64("PV_G1_STEP_VNS", 50_000_000);

    let (ha, gpa_a) = g1_arm(&kernel, &initramfs, seals, v0, dv);
    let (hb, gpa_b) = g1_arm(&kernel, &initramfs, seals, v0, dv);

    assert_eq!(
        gpa_a, gpa_b,
        "registration GPA diverged across same-seed runs"
    );
    let mut pass = true;
    for (k, (a, b)) in ha.iter().zip(&hb).enumerate() {
        let eq = a == b;
        pass &= eq;
        eprintln!(
            "[REPORT] G1 seal {k}: A={} B={} {}",
            hex(a),
            hex(b),
            if eq { "MATCH" } else { "DIVERGED" }
        );
    }
    eprintln!("RESULT: {}", if pass { "PASS" } else { "FAIL" });
    assert!(pass, "G1: same-seed page-on hashes diverged");
}

// ---------------------------------------------------------------------------
// G2 — page-stamp vs the RDTSC-trap oracle at refresh Moments (x86 only).
// ---------------------------------------------------------------------------

/// **G2.** Function equality between the page and the trap: at **every**
/// synchronized registered boundary (r18 — not a `step % N` sample) the page's
/// stable frame publishes exactly the values the RDTSC-trap completion would
/// return at the current skid-free anchor
/// (`Vmm::pvclock_check_oracle`, backed by the per-stamp read-back inside the
/// refresh itself — a wrong-offset/wrong-endian/torn stamp fails the run, not
/// just this gate). NOT whole-hash equality across page-on/page-off — §6
/// rules that out (the two configs legitimately differ in RAM bytes and clock
/// resolution). Ends with the live deliberate-fault re-check: a corrupted
/// page MUST fail the checker (the gate can fail ⇒ its pass means something).
#[test]
#[ignore = "box-only (see g0); run g0 first — smoke-fire-once"]
fn g2_page_matches_trap_oracle_at_refresh_moments() {
    require_kvm();
    require_host_baseline();
    let kernel = pvclock_kernel();
    let initramfs = minimal_initramfs();

    let mut vmm = boot(&kernel, &initramfs, SEED, true);
    let mut checked = 0u64;
    let obs = run_bounded(
        &mut vmm,
        50_000_000,
        Duration::from_secs(600),
        |vmm, step| {
            // Every synchronized boundary is a refresh Moment (the step-tail
            // stamp just ran), so check EVERY one — not a `step % N` sample (r18
            // P1). A sampled check passes a regression that leaves the page STALE
            // at an unsampled synchronized boundary (a refresh that should have
            // fired and didn't) and repairs it before the next sampled step; the
            // gate's claim is stamp==oracle at every refresh Moment, so it must
            // read back at every one. `pvclock_check_oracle` is O(1) and
            // synchronized boundaries are the intercept steps (not every step), so
            // this stays cheap over a full boot.
            if vmm.is_synchronized() && vmm.pvclock_registration().is_some() {
                vmm.pvclock_check_oracle()
                    .unwrap_or_else(|e| panic!("G2 diverged at step {step}: {e}"));
                checked += 1;
            }
            true
        },
    );
    assert!(obs.step_error.is_none(), "step error: {:?}", obs.step_error);
    assert!(
        vmm.pvclock_registration().is_some(),
        "guest never registered — G2 checked nothing (vacuous); see g0"
    );
    assert!(checked > 0, "no synchronized boundary sampled — vacuous");
    // Terminal check + the refresh log is monotonic on both published fields.
    vmm.pvclock_check_oracle()
        .expect("oracle equality at terminal");
    let refreshes = vmm.pvclock_refreshes().to_vec();
    assert!(
        refreshes.windows(2).all(|p| p[0].1 <= p[1].1),
        "published vns went backwards"
    );
    // Deliberate fault, live: corrupt the page and require the checker to
    // catch it (then this VM is discarded).
    let gpa = vmm.pvclock_registration().unwrap();
    vmm.apply_host_fault(&environment::HostFault::CorruptMemory {
        gpa: gpa + 0x10, // the guest_clock field
        mask: environment::BitMask(0xFF),
    })
    .expect("test corruption applies");
    assert!(
        vmm.pvclock_check_oracle().is_err(),
        "a corrupted page passed the oracle check — the gate cannot fail (vacuity)"
    );
    eprintln!(
        "[REPORT] G2: {} oracle checks (EVERY synchronized registered boundary) + terminal check, \
         {} logged refreshes, deliberate-fault detected. RESULT: PASS",
        checked,
        refreshes.len()
    );
}

// ---------------------------------------------------------------------------
// G3 — busy-wait-on-time liveness within Δ.
// ---------------------------------------------------------------------------

/// **G3.** A guest userspace busy-wait on the clock terminates: the shell
/// spins on `date +%s` (clock_gettime → the pvclock clocksource → the page)
/// until 2 virtual seconds elapse — pure compute, no timer sleep — which can
/// only complete if the §2.4 staleness-bound forced refresh keeps advancing
/// the page. Asserts completion, that no gap between consecutive refresh anchors
/// exceeded Δ, AND that the refreshes are actually **attributable to the Δ
/// deadline**.
///
/// **Non-vacuity** (cross-model r4 P1). This guest's 100 Hz tick forces a
/// `Deadline` — and so a refresh — every ~10 ms all by itself, which is also the
/// *default* Δ: at that Δ the gap assertion would hold with the forced-refresh
/// deadline deleted, and the gate would prove nothing. Two changes close that:
/// G3 runs at [`g3_delta_work`] (one tenth of a tick — a bound the tick cannot
/// meet), and it asserts [`Vmm::pvclock_forced_landings`] — landings at which
/// neither the timer nor an arrival was due — dominate the window. Delete
/// `pvclock_refresh_deadline` and both assertions fail.
#[test]
#[ignore = "box-only (see g0); needs initramfs-exec.cpio.gz + INITRAMFS_EXEC_SHA256; run g0 first"]
fn g3_busy_wait_on_page_time_terminates_within_delta() {
    require_kvm();
    require_host_baseline();
    let kernel = pvclock_kernel();
    let initramfs = exec_initramfs();
    let delta = g3_delta_work();
    assert!(
        delta < GUEST_TICK_WORK,
        "G3's Δ ({delta}) must be strictly below the guest tick ({GUEST_TICK_WORK}), else the \
         periodic tick alone satisfies the gap bound and the gate is vacuous"
    );

    let mut vmm = boot_with_delta(&kernel, &initramfs, SEED, true, delta);
    // Boot to the interactive shell (GUEST_READY on the console), checking
    // the serial only periodically (a byte-scan per step would be O(n²)).
    let obs = run_bounded(
        &mut vmm,
        50_000_000,
        Duration::from_secs(600),
        |vmm, step| !(step % 10_000 == 0 && find(vmm.serial(), b"GUEST_READY")),
    );
    assert!(
        obs.step_error.is_none(),
        "boot step error: {:?}",
        obs.step_error
    );
    assert!(
        find(vmm.serial(), b"GUEST_READY"),
        "exec image did not reach its shell"
    );
    let gpa = vmm
        .pvclock_registration()
        .expect("guest never registered the clock page");

    // The busy-wait: `pvclock-spin` mmaps the clock page and spins on it for 2
    // virtual seconds, making NO syscalls and taking NO counter traps inside the
    // loop (guest/linux/pvclock-spin.c).
    //
    // The obvious shell version — `while [ $(date +%s) -lt N ]` — is NOT a test
    // of this mechanism (cross-model r5 P1). Every `date` is a syscall, and this
    // kernel randomizes the kernel stack offset on syscall entry by reading the
    // TSC: `do_syscall_64` carries an `rdtsc` (it is in the reviewed allowlist).
    // That is a V-time intercept, so each syscall exits, advances the anchor and
    // refreshes the page for free — the loop would terminate whether or not the Δ
    // staleness bound did anything, and the constant intercepts can even keep the
    // pvclock deadline from ever landing, zeroing the attribution count. The
    // gate would be vacuous in both directions.
    //
    // The host already knows where the page is, so there is nothing to parse.
    let spin = format!("/bin/pvclock-spin {gpa:#x} 2000000000");
    let mut session = ExecSession::new(&spin, 1);
    let input = session.input().to_vec();
    vmm.inject_serial_input(&input);
    // Capture the page's CURRENT publication as the window BASELINE before
    // clearing (r20 P2). Clearing the log to isolate the spin window drops the
    // baseline, so the FIRST logged spin refresh has no predecessor and a first
    // refresh delayed past Δ would escape the max-gap check while later Δ-spaced
    // ones pass. Prepending this baseline makes the first interval (baseline →
    // first spin refresh) count. It is the most-recent publication = the page's
    // live state; boot produced thousands, so it is always present.
    let g3_baseline = vmm
        .pvclock_refreshes()
        .last()
        .copied()
        .expect("boot published at least one refresh before the spin");
    // Re-arm the refresh log at the measured window's start (cross-model r1
    // P1): a boot-saturated trace would record nothing during the spin and
    // `max_gap` below would vacuously read 0.
    vmm.pvclock_clear_refreshes();
    let mut cursor = vmm.serial_output().len();
    let obs = run_bounded(&mut vmm, 200_000_000, Duration::from_secs(900), |vmm, _| {
        let out = vmm.serial_output();
        if out.len() > cursor {
            session.feed(&out[cursor..]);
            cursor = out.len();
        }
        !session.is_done()
    });
    assert!(
        obs.step_error.is_none(),
        "spin step error: {:?}",
        obs.step_error
    );
    let done = session.is_done();
    let outcome = session.into_outcome();
    assert!(
        done && outcome.ok,
        "the busy-wait on page time did NOT terminate (frozen page / Δ refresh not engaging?): \
         status={:?} output tail={:?}",
        outcome.status,
        String::from_utf8_lossy(&outcome.output[outcome.output.len().saturating_sub(200)..])
    );
    // The spinner reports how it ended: PVSPIN_DONE means the page clock really
    // advanced 2 virtual seconds under its own (syscall-free) reads. A
    // PVSPIN_FAIL — bad ABI, /dev/mem, mmap — must never be mistaken for a pass,
    // and the exit status alone would not tell them apart from a shell error.
    assert!(
        find(&outcome.output, b"PVSPIN_DONE"),
        "pvclock-spin did not report PVSPIN_DONE — it never observed 2 virtual seconds pass by \
         reading the page directly. Output tail: {:?}",
        String::from_utf8_lossy(&outcome.output[outcome.output.len().saturating_sub(300)..])
    );
    // Staleness bound: no two consecutive refresh anchors in the spin window
    // more than Δ apart (the §6 G3 assertion, on the work axis). The window
    // is the cleared-at-spin-start log; a saturated or near-empty window is a
    // measurement FAILURE, never a pass (cross-model r1 P1 — a full log
    // proves only that ≥cap refreshes happened, not that any bound held, and
    // an empty `windows(2)` would report a vacuous zero gap).
    let window = vmm.pvclock_refreshes();
    assert!(
        window.len() >= 2,
        "only {} refreshes recorded during a 2-virtual-second spin — no gap can be measured \
         (expected ~2s/Δ refreshes); the G3 bound was NOT established",
        window.len()
    );
    assert!(
        window.len() < vmm_core::vmm::PVCLOCK_REFRESH_TRACE_CAP,
        "the refresh log saturated during the spin window — gaps beyond the cap are unobserved, \
         so the ≤Δ bound cannot be asserted; raise the cap or shorten the window"
    );
    // Prepend the pre-clear BASELINE so the FIRST interval of the spin (baseline →
    // first logged refresh) counts against both the monotonicity and max-gap
    // checks (r20 P2) — a first refresh delayed past Δ would otherwise escape.
    let gapped: Vec<(u64, u64, u64)> = std::iter::once(g3_baseline)
        .chain(window.iter().copied())
        .collect();
    // The published vns must be MONOTONE across the window, INCLUDING from the
    // baseline (r15 P2 + r20 P2). The guest spinner now fails on a backward step,
    // and asserting it host-side too means a backward page cannot slip past even
    // here — the `max_gap` `saturating_sub` on the work anchor would otherwise
    // clamp it to 0 and hide it.
    assert!(
        gapped.windows(2).all(|p| p[0].1 <= p[1].1),
        "the pvclock page published a BACKWARD vns during the G3 spin window (baseline included) — \
         a determinism/ABA bug the max-gap saturating_sub would mask"
    );
    let max_gap = gapped
        .windows(2)
        .map(|p| p[1].0.saturating_sub(p[0].0))
        .max()
        .expect("baseline + >= 2 window refreshes ⇒ >= 3 points");
    // ATTRIBUTION (r4 P1): how many of those refreshes did the Δ deadline
    // itself force — as opposed to the guest's periodic tick, which refreshes
    // the page for free every ~10 ms and would otherwise carry this gate?
    let forced = vmm.pvclock_forced_landings();
    let refreshes = window.len() as u64;
    eprintln!(
        "[REPORT] G3: spin completed (status {:?}), {refreshes} refreshes in the window, \
         {forced} of them forced by the Δ deadline (tick ≈ {GUEST_TICK_WORK} work, \
         Δ = {delta}), max anchor gap {max_gap}. RESULT: {}",
        outcome.status,
        if max_gap <= delta && forced * 2 >= refreshes {
            "PASS"
        } else {
            "FAIL"
        }
    );
    assert!(
        max_gap <= delta,
        "a refresh gap of {max_gap} work units exceeded Δ = {delta}"
    );
    // At Δ = tick/10 the staleness bound must supply the large majority of the
    // refreshes. A window the tick could have carried on its own is not evidence
    // that the forced refresh works — it is the vacuity this assertion rules out.
    assert!(
        forced * 2 >= refreshes,
        "only {forced} of {refreshes} refreshes were attributable to the Δ deadline (Δ = {delta}, \
         tick ≈ {GUEST_TICK_WORK}) — the periodic tick, not the staleness bound, kept this page \
         fresh, so G3 proves nothing about the forced refresh"
    );
}

// ---------------------------------------------------------------------------
// N-4 perf — RDTSC exit rates + boot ratio, page-off vs page-on.
// ---------------------------------------------------------------------------

struct PerfArm {
    rdtsc_exits: u64,
    total_exits: u64,
    deadline_exits: u64,
    vns: u64,
    wall: Duration,
    reached_ready: bool,
}

fn perf_arm(kernel: &[u8], initramfs: &[u8], page_on: bool) -> PerfArm {
    let mut vmm = boot(kernel, initramfs, SEED, page_on);
    // Sample the counters AND V-time at the LAST SYNCHRONIZED boundary of the
    // run, not at the (unsynchronized) post-poweroff terminal (r15 P2): a
    // whole-boot run ends at a terminal we cannot step past, so capture the last
    // synchronized sample as it goes. Both denominators come from the same
    // synchronized moment, so `effective_vns` is exact and the two arms compare
    // like-for-like (page-off's extra RDTSC intercepts don't skew its V-time).
    let mut last_sync: Option<(vmm_backend::ExitCounts, u64)> = None;
    let obs = run_bounded(&mut vmm, 100_000_000, Duration::from_secs(900), |vmm, _| {
        if vmm.is_synchronized() {
            last_sync = Some((vmm.exit_counts(), vmm.effective_vns().unwrap_or(0)));
        }
        true
    });
    assert!(obs.step_error.is_none(), "step error: {:?}", obs.step_error);
    // Clean poweroff = a terminal with no step error (the minimal image ends on
    // the post-poweroff `Idle` halt, not `Shutdown` — see g0_smoke).
    assert!(
        matches!(
            obs.reason,
            Some(TerminalReason::Shutdown | TerminalReason::Idle)
        ),
        "minimal boot did not power off cleanly: reason={:?}",
        obs.reason
    );
    if page_on {
        assert!(
            vmm.pvclock_registration().is_some(),
            "page-on arm never registered — the measurement would be page-off vs page-off"
        );
        // Registration is NOT selection (r19 P1): a page that registers but never
        // becomes the active clocksource (a rating/initcall regression) leaves the
        // guest on the TSC, so this "page-on" arm would report a page-OFF ratio.
        // G0 checks the switch, but it is an independent ignored test that a
        // perf-only invocation does not run — assert it here too, before any ratio.
        assert!(
            find(vmm.serial(), CLOCKSOURCE_SWITCH_MARKER),
            "page-on arm registered the clock page but Linux never SELECTED \
             harmony-pvclock as the active clocksource (still on the TSC) — the ratio \
             would mislabel an effectively page-OFF run. Check the guest 'clocksource' lines."
        );
    }
    // FAIL rather than fall back to the (unsynchronized) terminal sample (r16
    // P2): `last_sync` is `Some` iff the run passed through at least one
    // synchronized boundary — which a real boot always does (every RDTSC
    // intercept). If it never did, the terminal `effective_vns()` is a lower
    // bound, and reporting it would bias the arm; refuse loudly instead.
    let (counts, vns) = last_sync.expect(
        "perf_arm never observed a synchronized V-time boundary during the whole-boot run — the \
         terminal effective_vns is only a lower bound and must not be reported as the denominator",
    );
    PerfArm {
        rdtsc_exits: counts.rdtsc + counts.rdtscp,
        total_exits: counts.total(),
        deadline_exits: counts.deadline,
        vns,
        wall: obs.wall,
        reached_ready: find(vmm.serial(), b"GUEST_READY"),
    }
}

/// The **N-4-style perf measurement** (§6): RDTSC-exit rate per virtual
/// second and boot wall time, page-off vs page-on, over the SAME kernel image
/// (the `harmony_pvclock` parameter is the one knob). Reports the reduction
/// ratio against **kill condition 3** (<2× = "not worth it on x86") honestly
/// either way — this test asserts only measurement sanity, never the ratio:
/// the verdict is the reader's, per the task spec.
#[test]
#[ignore = "box-only (see g0); run g0 first — smoke-fire-once"]
fn n4_perf_rdtsc_exit_rate_page_off_vs_page_on() {
    require_kvm();
    require_host_baseline();
    let kernel = pvclock_kernel();
    let initramfs = minimal_initramfs();

    let off = perf_arm(&kernel, &initramfs, false);
    let on = perf_arm(&kernel, &initramfs, true);
    assert!(
        off.reached_ready && on.reached_ready,
        "an arm missed GUEST_READY"
    );

    let rate = |exits: u64, vns: u64| -> f64 {
        // Test-reporting arithmetic only — never reaches state or a hash.
        exits as f64 / (vns.max(1) as f64 / 1e9)
    };
    let off_rate = rate(off.rdtsc_exits, off.vns);
    let on_rate = rate(on.rdtsc_exits, on.vns);
    let reduction = off_rate / on_rate.max(f64::MIN_POSITIVE);
    eprintln!(
        "[REPORT] perf page-OFF: rdtsc_exits={} ({:.0}/vsec) total_exits={} deadline_exits={} \
         vns={} wall={:?}",
        off.rdtsc_exits, off_rate, off.total_exits, off.deadline_exits, off.vns, off.wall
    );
    eprintln!(
        "[REPORT] perf page-ON:  rdtsc_exits={} ({:.0}/vsec) total_exits={} deadline_exits={} \
         vns={} wall={:?}",
        on.rdtsc_exits, on_rate, on.total_exits, on.deadline_exits, on.vns, on.wall
    );
    eprintln!(
        "[REPORT] perf: rdtsc-exit-rate reduction = {reduction:.2}x (kill condition 3 threshold: \
         2x); boot wall ratio on/off = {:.3}; exits-per-vsec delta = {:.0}",
        on.wall.as_secs_f64() / off.wall.as_secs_f64().max(f64::MIN_POSITIVE),
        rate(off.total_exits, off.vns) - rate(on.total_exits, on.vns),
    );
    eprintln!(
        "RESULT: MEASURED ({} kill condition 3)",
        if reduction >= 2.0 {
            "clears"
        } else {
            "DOES NOT clear"
        }
    );
    // Sanity only: both arms measured something real.
    assert!(
        off.rdtsc_exits > 0,
        "page-off arm saw no rdtsc exits — not a real measurement"
    );
    assert!(on.vns > 0 && off.vns > 0, "virtual time did not advance");
}

/// Steady-state exit rates over a bounded Postgres window (the workload half
/// of the §6 numbers; the full det-corpus + campaign smoke runs via the
/// existing box_corpus / campaign-runner tooling — see IMPLEMENTATION.md).
#[test]
#[ignore = "box-only (see g0); long — run after g0/g1; needs initramfs-postgres.cpio.gz"]
fn n4_perf_postgres_window_page_off_vs_page_on() {
    require_kvm();
    require_host_baseline();
    let kernel = pvclock_kernel();
    let initramfs = pg_initramfs();
    // Optional early cap on the workload window, in V-time ns. Default: the WHOLE
    // workload (`workload begin` → `workload end`), which self-sizes to the
    // pinned artifact — so the default invocation works without any override
    // (cross-model r8 P2, the "default within the pinned workload" concern). Set
    // it to measure a shorter fixed sub-window of a longer workload.
    let cap = env_u64("PV_PERF_WINDOW_VNS", u64::MAX);
    // Refuse a ZERO cap before either arm runs (r21 P2): the window loop stops as
    // soon as `span >= cap`, so `cap == 0` closes the window at the first step —
    // a one-step sample would pass as the steady-state workload measurement. A
    // zero override is an operator error, not a valid measurement.
    assert!(
        cap > 0,
        "PV_PERF_WINDOW_VNS=0 is refused — a zero window would report a one-step \
         sample as the workload measurement; set a positive ns cap or leave it unset \
         (the default measures the whole workload)."
    );

    // (rdtsc+rdtscp delta, total delta, V-time span) measured OVER THE WORKLOAD.
    let arm = |page_on: bool| -> (u64, u64, u64) {
        let side = if page_on { "on" } else { "off" };
        let mut vmm = boot(&kernel, &initramfs, SEED, page_on);
        // 1. Boot to workload READINESS. Everything before `PG37: workload begin`
        //    is kernel + Postgres startup, NOT steady state — counting from VM
        //    boot would report mostly-startup as a workload measurement
        //    (cross-model r8 P2). Fail loudly if the workload never begins
        //    (Postgres crashed / hung), rather than reporting startup.
        let boot_obs = run_bounded(&mut vmm, u64::MAX, Duration::from_secs(900), |vmm, _| {
            !find(vmm.serial(), b"PG37: workload begin")
        });
        assert!(
            boot_obs.step_error.is_none(),
            "page-{side} boot step error before workload readiness: {:?}",
            boot_obs.step_error
        );
        assert!(
            find(vmm.serial(), b"PG37: workload begin"),
            "page-{side} arm never reached 'PG37: workload begin' — Postgres did not start its \
             workload (terminal={:?}). A run that never enters the workload cannot be reported \
             as a steady-state Postgres measurement.",
            boot_obs.reason
        );
        // 2. BASELINE the counters and V-time at a SYNCHRONIZED boundary at/after
        //    readiness (r15 P2). `PG37: workload begin` is seen on a serial PIO
        //    exit, where `effective_vns` is a last-intercept lower bound; the arms
        //    take different intercept mixes, so that bound biases the ratio.
        let (base, base_vns) = sample_at_sync(&mut vmm);
        // 3. Measure to `PG37: workload end` (or the optional V-time cap).
        let win_obs = run_bounded(&mut vmm, u64::MAX, Duration::from_secs(900), |vmm, _| {
            !(find(vmm.serial(), b"PG37: workload end")
                || vmm.effective_vns().unwrap_or(0).saturating_sub(base_vns) >= cap)
        });
        assert!(
            win_obs.step_error.is_none(),
            "page-{side} arm step error mid-workload: {:?}",
            win_obs.step_error
        );
        // Sample the END counters and V-time at a synchronized boundary too
        // (r15 P2), so both endpoints are exact and the span/deltas are unbiased.
        let (now, end_vns) = sample_at_sync(&mut vmm);
        let span = end_vns.saturating_sub(base_vns);
        // 4. The window must CLOSE on the end marker or the cap, with a non-trivial
        //    span — not on a mid-workload guest terminal (a truncated window is not
        //    valid evidence; cross-model r6/r8).
        let reached_end = find(vmm.serial(), b"PG37: workload end");
        assert!(
            reached_end || span >= cap,
            "page-{side} workload window truncated: span={span} vns, end_marker={reached_end}, \
             terminal={:?} — the guest ended mid-workload, not valid steady-state evidence",
            win_obs.reason
        );
        assert!(span > 0, "page-{side} workload window had zero V-time span");
        if page_on {
            assert!(
                vmm.pvclock_registration().is_some(),
                "page-{side} arm never registered the clock page"
            );
            // Registration is NOT selection (r19 P1): assert the guest actually
            // SELECTED harmony-pvclock before this arm's counts feed a ratio, else
            // a registered-but-unused page would report a page-OFF workload as
            // page-on.
            assert!(
                find(vmm.serial(), CLOCKSOURCE_SWITCH_MARKER),
                "page-{side} arm registered the clock page but Linux never SELECTED \
                 harmony-pvclock as the active clocksource (still on the TSC) — the ratio \
                 would mislabel an effectively page-OFF run"
            );
        }
        (
            (now.rdtsc + now.rdtscp) - (base.rdtsc + base.rdtscp),
            now.total() - base.total(),
            span,
        )
    };
    let (off_rdtsc, off_total, off_span) = arm(false);
    let (on_rdtsc, on_total, on_span) = arm(true);
    // Each arm's rate is its OWN workload-span delta (boot excluded, deltas from
    // the readiness baseline). The two spans differ slightly — the page changes
    // the timekeeping instruction stream, hence the retired-branch V-time of the
    // same logical SQL — so the fair comparison is the RATE (exits per vsec),
    // which normalizes for that.
    let per_vsec = |n: u64, span: u64| n as f64 / (span.max(1) as f64 / 1e9);
    eprintln!(
        "[REPORT] pg workload page-OFF: rdtsc={off_rdtsc} ({:.0}/vsec) total={off_total} span={off_span} vns",
        per_vsec(off_rdtsc, off_span)
    );
    eprintln!(
        "[REPORT] pg workload page-ON:  rdtsc={on_rdtsc} ({:.0}/vsec) total={on_total} span={on_span} vns",
        per_vsec(on_rdtsc, on_span)
    );
    eprintln!(
        "[REPORT] pg workload: rdtsc-exit-rate reduction = {:.2}x (workload begin→end, boot \
         excluded; kill condition 3 threshold: 2x)",
        per_vsec(off_rdtsc, off_span) / per_vsec(on_rdtsc, on_span).max(f64::MIN_POSITIVE)
    );
    assert!(
        off_rdtsc > 0,
        "not a real measurement (page-off workload took no RDTSC exits)"
    );
}
