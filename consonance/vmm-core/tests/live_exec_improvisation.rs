// SPDX-License-Identifier: AGPL-3.0-or-later
//! Box-only **improvisation** gate (task 81): prove that `exec`-in-a-fork runs a
//! real command inside a live guest and that the **taint guard** keeps that
//! improvised timeline out of the reproducer story — while the *original* timeline
//! it forked from is provably unaffected. Driven directly against the
//! [`ControlServer`] verbs (`hello`/`snapshot`/`branch`/`replay`/`run`/`hash`/`exec`/
//! `recorded_env`) on the real patched-KVM Postgres workload.
//!
//! The spec's box gates (`tasks/81-improvisations.md`):
//!   2. **The improvisation.** From a mid-workload Postgres snapshot: `branch` a
//!      fork, `exec` a real command (`ls /` / `ps aux`), capture **non-empty**
//!      output; the **original** timeline, continued to a later `Moment`, hashes
//!      **identically** to a control run that never forked — the improvisation
//!      observably cost the search nothing.
//!   3. **The guard.** On the exec'd fork, `recorded_env` fails `Tainted`; a
//!      snapshot taken there reports `tainted: true`; a `branch` from it also
//!      refuses `recorded_env`.
//!   4. (Byte-identity of the existing `live_*` gates is covered by *those* gates —
//!      the serial-input path is inert when no `exec` session is active; see the
//!      portable `devices::tests::serial_input_is_inert_until_injected` unit test
//!      and the unchanged `state_hash` on every non-`exec` run.)
//!
//! **Guest prerequisite (gate 2's non-empty output).** `exec` injects bytes on the
//! guest's serial input (ttyS0) as if typed at a root shell, and detects completion
//! by a sentinel `echo`. That needs a **root shell reading ttyS0** in the guest at
//! the snapshot point. The stock Postgres workload image drives postgres and does
//! not read the serial, so gate 2's *output* half needs the **exec-capable** image
//! variant (`guest/linux/exec-init.sh`; build `make -C guest/linux exec-image`,
//! `INITRAMFS=initramfs-exec.cpio.gz`). The **determinism** half of gate 2 (the
//! original timeline is unaffected) and *all* of gate 3 (the taint guard) hold
//! against **any** image — `exec` taints and the guard fires regardless of whether
//! a shell answered.
//!
//! **The output assertion is strict by default** (gate 2 says "capture non-empty
//! output" — a by-the-docs dispatch must not pass vacuously): the run **fails**
//! unless the `exec` completed with non-empty output. Strictness is **forced** on
//! the exec-capable image (basename contains `exec`). To run ONLY the guard half
//! (gate 3 + gate-2 determinism) against a shell-less image, set `EXEC_TAINT_ONLY=1`
//! — which the run announces does **not** satisfy gate 2.
//!
//! Run on `ssh <det-box>` with the LOADED patched KVM modules + a built image,
//! CPU-pinned per `docs/BOX-PINNING.md` (lease a core via `box-window.sh`; never
//! touch another lease's cores or its patched-KVM window). ALWAYS revert KVM to
//! stock **1396736** + verify after any patched run.
//! ```text
//! # Full gate 2 + gate 3 (the exec-capable image — strict is forced):
//! make -C guest fetch && make -C guest/linux exec-image
//! INITRAMFS=initramfs-exec.cpio.gz taskset -c <core> \
//!   cargo test -p vmm-core --release --test live_exec_improvisation -- --ignored --nocapture
//! # Guard half only, against the real Postgres workload (does NOT satisfy gate 2):
//! make -C guest/linux postgres-image
//! EXEC_TAINT_ONLY=1 taskset -c <core> \
//!   cargo test -p vmm-core --release --test live_exec_improvisation -- --ignored --nocapture
//! ```
//! Tunable via env (defaults below): `EI_GENESIS_STEP` (V-time ns to nudge past a
//! non-snapshottable boundary), `EI_MID` (V-time ns past genesis for the mid-
//! workload snapshot), `EI_LATE` (the later `Moment` the original continues to),
//! `EI_BUDGET` (V-time ns the `exec` may run), `EI_CMD` (the command to `exec`),
//! `EXEC_TAINT_ONLY=1` (relax the output half — guard-only, shell-less image; forced
//! strict anyway on an `exec`-named image), `EI_SEED` (the genesis env seed).
//!
//! Every precondition that would prevent a real run — no `/dev/kvm`, stock modules,
//! a non-baseline host — is a **loud panic (test FAILURE)**, never an early-return
//! `Ok`. macOS builds an empty test binary; the `exec` state machine and the taint
//! guard are covered portably by the `src/exec.rs` unit tests and the
//! `src/control.rs` taint-guard proptest + unit tests.
#![cfg(target_os = "linux")]

use control_proto::{
    ControlError, HashScope, Moment, Reply, Reproducer, Request, SnapId, StopConditions,
    StopMask, StopReason,
};
use environment::{EnvSpec, FaultPolicy};
use vmm_backend::Backend;
use vmm_core::bringup::{BackendKind, boot_linux_selected};
use vmm_core::control::{ControlServer, VmmFactory, server_caps};

type DynVmm = vmm_core::vmm::Vmm<Box<dyn Backend>>;

const GUEST_RAM_LEN: usize = 2 << 30;
const GENESIS_SEED: u64 = 0x0080_0080_C0FF_EE80;
const DEFAULT_CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t,force tsc=reliable \
     no_timer_check lpj=4000000 nokaslr nosmp maxcpus=1 nox2apic hpet=disable";

fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
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
        "guest artifact `{name}` not found in guest/build or guest/linux — build it first on the \
         box: `make -C guest fetch && make -C guest/linux postgres-image` (or `exec-image`)."
    );
}

fn require_kvm() {
    assert!(
        std::path::Path::new("/dev/kvm").exists(),
        "/dev/kvm absent — run this `#[ignore]`d box gate on `ssh <det-box>` with the LOADED \
         patched KVM modules, CPU-pinned per docs/BOX-PINNING.md."
    );
}

fn require_host_baseline() {
    let report = vmm_core::hostassert::report();
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

fn cmdline() -> String {
    std::env::var("BOOT_CMDLINE").unwrap_or_else(|_| DEFAULT_CMDLINE.to_string())
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn hex(d: &[u8; 32]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

fn boot_pg(kernel: &[u8], initramfs: &[u8], seed: u64) -> DynVmm {
    boot_linux_selected(
        BackendKind::Patched,
        kernel,
        initramfs,
        GUEST_RAM_LEN,
        &cmdline(),
        seed,
    )
    .expect("boot_linux_selected (patched) — needs the LOADED patched KVM + perf + det-cfl-v1 host")
}

fn call<B: Backend>(s: &mut ControlServer<B>, req: &Request) -> Result<Reply, ControlError> {
    s.handle(req)
        .expect("session-fatal ServeError from the control server")
}

fn expect_ok<B: Backend>(s: &mut ControlServer<B>, req: &Request) -> Reply {
    match call(s, req) {
        Ok(reply) => reply,
        Err(e) => panic!("verb {req:?} answered a ControlError: {e:?}"),
    }
}

fn run_until<B: Backend>(s: &mut ControlServer<B>, deadline: u64) -> StopReason {
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

fn hash_whole<B: Backend>(s: &mut ControlServer<B>) -> [u8; 32] {
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

/// Seal the current point, retrying past non-snapshottable boundaries (the task-58
/// nudge). Returns `(SnapId, tainted, V-time)`.
fn seal<B: Backend>(s: &mut ControlServer<B>, mut vt: u64, retry_step: u64) -> (SnapId, bool, u64) {
    let mut attempts = 0usize;
    loop {
        attempts += 1;
        match call(s, &Request::Snapshot) {
            Ok(Reply::SnapId(id)) => return (id, false, vt),
            Ok(Reply::Snapshot { id, tainted }) => return (id, tainted, vt),
            Ok(other) => panic!("snapshot answered {other:?}"),
            Err(ControlError::NotQuiescent) => {
                assert!(
                    attempts < 100_000,
                    "no snapshottable boundary within budget"
                );
                match run_until(s, vt.saturating_add(retry_step)) {
                    StopReason::Deadline { vtime } => vt = vtime.0,
                    other => panic!("guest ended before a sealable boundary: {other:?}"),
                }
            }
            Err(e) => panic!("snapshot answered a ControlError: {e:?}"),
        }
    }
}

fn recorded_env<B: Backend>(s: &mut ControlServer<B>) -> Result<Reply, ControlError> {
    call(s, &Request::RecordedEnv)
}

/// The original timeline, restored from `snap` and continued to `late` — the
/// determinism anchor for gate 2. A verbatim `replay` (no reseed) so it is the
/// exact same trajectory each time.
fn replay_to_late<B: Backend>(s: &mut ControlServer<B>, snap: SnapId, late: u64) -> [u8; 32] {
    assert_eq!(
        expect_ok(s, &Request::Replay(snap)),
        Reply::Unit,
        "replay(original snapshot)"
    );
    match run_until(s, late) {
        StopReason::Deadline { .. } => {}
        other => panic!("continuing the original to `late` stopped non-Deadline: {other:?}"),
    }
    hash_whole(s)
}

#[test]
#[ignore = "box-only improvisation gate (LOADED patched KVM + built image + det-cfl-v1 host); \
            run per docs/BOX-PINNING.md"]
fn exec_improvisation_is_off_the_record_and_costs_the_search_nothing() {
    require_kvm();
    require_host_baseline();
    let kernel = require_artifact("bzImage");
    let initramfs_name =
        std::env::var("INITRAMFS").unwrap_or_else(|_| "initramfs-postgres.cpio.gz".to_string());
    let initramfs = require_artifact(&initramfs_name);
    let seed = env_u64("EI_SEED", GENESIS_SEED);

    let live = boot_pg(&kernel, &initramfs, seed);
    let (fk, fi) = (kernel.clone(), initramfs.clone());
    let factory: VmmFactory<Box<dyn Backend>> = Box::new(move || {
        boot_linux_selected(
            BackendKind::Patched,
            &fk,
            &fi,
            GUEST_RAM_LEN,
            &cmdline(),
            seed,
        )
    });
    let mut s = ControlServer::new(live, factory);
    assert_eq!(
        expect_ok(&mut s, &Request::Hello(server_caps())),
        Reply::Hello(server_caps())
    );

    // 1. Seal genesis, then run to a mid-workload point and seal the ORIGINAL
    //    snapshot `mid_snap` — the timeline the improvisation forks off (untainted).
    let retry_step = env_u64("EI_GENESIS_STEP", 1_000_000);
    let vt0 = match run_until(&mut s, 0) {
        StopReason::Deadline { vtime } => vtime.0,
        other => panic!("vtime probe stopped non-Deadline: {other:?}"),
    };
    let (genesis, _gt, genesis_vt) = seal(&mut s, vt0, retry_step);
    let env = seeded_env(seed);

    // Materialize the mid point from genesis, then seal it.
    assert_eq!(
        expect_ok(
            &mut s,
            &Request::Branch {
                snap: genesis,
                env: env.clone(),
            }
        ),
        Reply::Unit
    );
    let mid_target = genesis_vt + env_u64("EI_MID", 8_000_000);
    let vt_mid = match run_until(&mut s, mid_target) {
        StopReason::Deadline { vtime } => vtime.0,
        other => panic!("run to mid stopped non-Deadline: {other:?}"),
    };
    let (mid_snap, mid_taint, mid_vt) = seal(&mut s, vt_mid, retry_step);
    assert!(
        !mid_taint,
        "the mid-workload snapshot is untainted (no exec yet)"
    );
    let late = mid_vt + env_u64("EI_LATE", 40_000_000);

    println!("\n[REPORT] task81 improvisation box gate");
    println!("  genesis_vt={genesis_vt} mid_vt={mid_vt} late={late} seed={seed:#x}");

    // 2a. CONTROL: continue the ORIGINAL from `mid_snap` to `late` with NO fork/exec
    //     anywhere — the reference hash.
    let control_hash = replay_to_late(&mut s, mid_snap, late);
    println!("  control  hash8={}", &hex(&control_hash)[..8]);

    // 2b. IMPROVISE on a FORK: branch off `mid_snap`, exec a real command.
    assert_eq!(
        expect_ok(
            &mut s,
            &Request::Branch {
                snap: mid_snap,
                env: env.clone(),
            }
        ),
        Reply::Unit,
        "branch a fork off the mid snapshot"
    );
    let budget = env_u64("EI_BUDGET", 200_000_000);
    let cmd = std::env::var("EI_CMD").unwrap_or_else(|_| "ls /".to_string());
    let (output, ok) = match expect_ok(
        &mut s,
        &Request::Exec {
            cmd: cmd.clone(),
            deadline: Moment(mid_vt.saturating_add(budget)),
        },
    ) {
        Reply::ExecResult { output, ok } => (output, ok),
        other => panic!("exec answered {other:?}"),
    };
    println!(
        "  exec `{cmd}` ok={ok} output_len={} sample={:?}",
        output.len(),
        String::from_utf8_lossy(&output[..output.len().min(120)]),
    );
    // Gate 2 requires "capture non-empty output" — so the output half is
    // **strict by default**, not opt-in (a by-the-docs dispatch must not pass
    // vacuously). Two escape hatches, both structural:
    //   - `EXEC_TAINT_ONLY=1` relaxes it, for running ONLY the guard half (gate 3 +
    //     gate-2 determinism) against a shell-less image (e.g. stock Postgres). This
    //     explicitly does NOT satisfy gate 2 — the run must say so out loud.
    //   - …UNLESS the resolved image is the exec-capable one (basename contains
    //     `exec`), in which case strictness is FORCED — there is no legitimate
    //     reason to relax the output proof on the very image that ships a shell.
    let image_is_exec = initramfs_name.contains("exec");
    let taint_only = std::env::var("EXEC_TAINT_ONLY").ok().as_deref() == Some("1");
    let strict = image_is_exec || !taint_only;
    if strict {
        assert!(
            !output.is_empty() && ok,
            "gate 2: exec `{cmd}` produced no output / did not complete against image \
             `{initramfs_name}` — a root shell must be reading ttyS0 (build the exec-capable image: \
             `make -C guest/linux exec-image`, INITRAMFS=initramfs-exec.cpio.gz). To run ONLY the \
             taint-guard half against a shell-less image, set EXEC_TAINT_ONLY=1 (this does NOT \
             satisfy gate 2's 'capture non-empty output')."
        );
    } else {
        println!(
            "  NOTE: EXEC_TAINT_ONLY=1 — output half SKIPPED (image `{initramfs_name}` has no \
             serial shell); gate 2's non-empty-output requirement is NOT proven by this run."
        );
    }

    // 3. THE GUARD, on the exec'd fork:
    //    - recorded_env is a loud Tainted;
    assert_eq!(
        recorded_env(&mut s),
        Err(ControlError::Tainted),
        "gate 3: recorded_env on the exec'd fork must fail Tainted"
    );
    //    - a snapshot taken here reports tainted: true. The exec'd fork sits at an
    //      opportunistic deadline stop (not a clean boundary), so nudge to a
    //      snapshottable point FIRST — staying on the (still-tainted) timeline;
    //      running forward never clears taint. A Quiescent stop (the shell going
    //      idle after the command) is itself a sealable boundary, so tolerate it.
    let mut vt_fork = match run_until(&mut s, 0) {
        StopReason::Deadline { vtime } => vtime.0,
        other => panic!("post-exec position probe stopped non-Deadline: {other:?}"),
    };
    let mut tries = 0usize;
    let (dirty_snap, dirty_taint) = loop {
        tries += 1;
        match call(&mut s, &Request::Snapshot) {
            Ok(Reply::SnapId(id)) => break (id, false),
            Ok(Reply::Snapshot { id, tainted }) => break (id, tainted),
            Ok(other) => panic!("snapshot on the exec'd fork answered {other:?}"),
            Err(ControlError::NotQuiescent) => {
                assert!(
                    tries < 100_000,
                    "exec'd fork never reached a snapshottable boundary"
                );
                match run_until(&mut s, vt_fork.saturating_add(retry_step)) {
                    StopReason::Deadline { vtime } | StopReason::Quiescent { vtime } => {
                        vt_fork = vtime.0
                    }
                    other => panic!("exec'd fork ended non-sealably: {other:?}"),
                }
            }
            Err(e) => panic!("snapshot on the exec'd fork answered a ControlError: {e:?}"),
        }
    };
    assert!(
        dirty_taint,
        "gate 3: a snapshot taken on the exec'd fork must report tainted: true"
    );
    //    - a branch from that tainted snapshot also refuses recorded_env.
    assert_eq!(
        expect_ok(
            &mut s,
            &Request::Branch {
                snap: dirty_snap,
                env: env.clone(),
            }
        ),
        Reply::Unit
    );
    assert_eq!(
        recorded_env(&mut s),
        Err(ControlError::Tainted),
        "gate 3: a branch from the tainted snapshot must also refuse recorded_env"
    );
    println!(
        "  guard    recorded_env=Tainted  dirty_snapshot.tainted=true  branch-of-tainted=Tainted  => PASS"
    );

    // 2c. The ORIGINAL, continued AFTER the improvisation, must hash identically to
    //     the control — the fork's exec cost the search nothing.
    let after_hash = replay_to_late(&mut s, mid_snap, late);
    println!("  after    hash8={}", &hex(&after_hash)[..8]);
    assert_eq!(
        after_hash,
        control_hash,
        "gate 2: the original timeline continued past the improvisation diverged from the control \
         (control={} after={}) — the fork's exec must NOT touch the original's trajectory",
        hex(&control_hash),
        hex(&after_hash)
    );
    println!("  determinism: original continuation IDENTICAL before/after the fork => PASS");
    println!("[REPORT] task81 improvisation box gate: ALL PASS");
}

/// **Smoke probe (fire-once, minutes-long): the riskiest live assumption.** Boot
/// the exec-capable image, seal one mid-workload snapshot, `branch` a fork, `exec`
/// ONE command, and assert the completion sentinel was scraped with non-empty
/// output. This isolates the end-to-end serial channel — host RX injection → the
/// guest's interactive busybox ash → the printable-marker echo → THR capture → the
/// scanner — the one path that cannot be proven off-box, and the one PR #86 r2
/// fixed (SOH → printable marker). Run this BEFORE the full gate spend; if it is
/// green the channel works and the full `exec_improvisation_*` gate is worth the
/// boot cost. It does **not** exercise the determinism or taint gates (those are
/// the full test's job) — it is deliberately the cheapest faithful channel probe.
#[test]
#[ignore = "box-only smoke probe of the exec serial channel (exec-capable image); \
            run per docs/BOX-PINNING.md before the full gate"]
fn smoke_exec_channel_boots_injects_and_scrapes_a_sentinel() {
    require_kvm();
    require_host_baseline();
    let kernel = require_artifact("bzImage");
    // The smoke probes the SHELL channel, so it wants the exec-capable image by
    // default (honor INITRAMFS if the operator points it elsewhere).
    let initramfs_name =
        std::env::var("INITRAMFS").unwrap_or_else(|_| "initramfs-exec.cpio.gz".to_string());
    let initramfs = require_artifact(&initramfs_name);
    let seed = env_u64("EI_SEED", GENESIS_SEED);

    let live = boot_pg(&kernel, &initramfs, seed);
    let (fk, fi) = (kernel.clone(), initramfs.clone());
    let factory: VmmFactory<Box<dyn Backend>> = Box::new(move || {
        boot_linux_selected(
            BackendKind::Patched,
            &fk,
            &fi,
            GUEST_RAM_LEN,
            &cmdline(),
            seed,
        )
    });
    let mut s = ControlServer::new(live, factory);
    assert_eq!(
        expect_ok(&mut s, &Request::Hello(server_caps())),
        Reply::Hello(server_caps())
    );

    // Seal genesis, run to a mid point, seal it, branch a fork.
    let retry_step = env_u64("EI_GENESIS_STEP", 1_000_000);
    let vt0 = match run_until(&mut s, 0) {
        StopReason::Deadline { vtime } => vtime.0,
        other => panic!("vtime probe stopped non-Deadline: {other:?}"),
    };
    let (genesis, _gt, genesis_vt) = seal(&mut s, vt0, retry_step);
    let env = seeded_env(seed);
    assert_eq!(
        expect_ok(
            &mut s,
            &Request::Branch {
                snap: genesis,
                env: env.clone(),
            }
        ),
        Reply::Unit
    );
    let mid_target = genesis_vt + env_u64("EI_MID", 8_000_000);
    let vt_mid = match run_until(&mut s, mid_target) {
        StopReason::Deadline { vtime } => vtime.0,
        other => panic!("run to mid stopped non-Deadline: {other:?}"),
    };
    let (mid_snap, _t, mid_vt) = seal(&mut s, vt_mid, retry_step);
    assert_eq!(
        expect_ok(
            &mut s,
            &Request::Branch {
                snap: mid_snap,
                env,
            }
        ),
        Reply::Unit,
        "branch a fork to sacrifice"
    );

    // Inject ONE command and scrape the sentinel — the whole point of the smoke.
    let budget = env_u64("EI_BUDGET", 200_000_000);
    let cmd = std::env::var("EI_CMD").unwrap_or_else(|_| "echo HELLO-SMOKE-42".to_string());
    let (output, ok) = match expect_ok(
        &mut s,
        &Request::Exec {
            cmd: cmd.clone(),
            deadline: Moment(mid_vt.saturating_add(budget)),
        },
    ) {
        Reply::ExecResult { output, ok } => (output, ok),
        other => panic!("exec answered {other:?}"),
    };
    println!(
        "\n[SMOKE] exec `{cmd}` ok={ok} output_len={} sample={:?}",
        output.len(),
        String::from_utf8_lossy(&output[..output.len().min(200)]),
    );
    assert!(
        ok && !output.is_empty(),
        "[SMOKE FAIL] the exec channel did not complete/capture on image `{initramfs_name}` — the \
         sentinel was never scraped (boot / serial-injection / marker / echo / capture path). \
         STOP: do not spend the full gate until the channel works."
    );
    println!("[SMOKE PASS] the exec serial channel works end-to-end on the box.");
}
