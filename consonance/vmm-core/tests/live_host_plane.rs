// SPDX-License-Identifier: AGPL-3.0-or-later
//! Box-only **host-plane enforcement** gate (task 59): the record → replay closure
//! for `CorruptMemory` + `InjectInterrupt` staged at chosen `Moment`s, on the real
//! patched-KVM Postgres workload, driving the [`ControlServer`] verbs directly
//! (`hello`/`snapshot`/`branch`/`perturb`/`run`/`hash`) — the explorer's typed
//! `Machine` adapter deliberately cannot express `perturb`, so this gate speaks the
//! server in-process.
//!
//! The spec's box gate (`tasks/59-host-plane-enforcement.md`):
//!   (a) the same host-fault schedule run twice ⇒ bit-identical `state_hash`;
//!   (b) `replay` of the emitted `Recorded` env ⇒ the same hash again (record →
//!       replay closure);
//!   (c) a schedule-absent control run differs (the faults are actually landing).
//!
//! Run on `ssh <det-box>` with the LOADED patched KVM modules + the built Postgres
//! image, CPU-pinned per `docs/BOX-PINNING.md` (lease a core via `box-window.sh`):
//! ```text
//! make -C guest fetch && make -C guest/linux postgres-image     # or copy a prebuilt image
//! taskset -c <core> cargo test -p vmm-core --release --test live_host_plane -- --ignored --nocapture
//! ```
//! Tunable via env (defaults below): `HP_M1_OFF` / `HP_M2_OFF` (V-time ns past the
//! snapshot for the two fault Moments), `HP_DELTA` (V-time ns each run advances past
//! the snapshot), `HP_GPA` (the CorruptMemory guest-physical address), `HP_VECTOR`.

#![cfg(target_os = "linux")]

use control_proto::{
    HashScope, HostFault as WireHostFault, Moment, Reply, Reproducer, Request, SnapId,
    StopConditions, StopMask, StopReason,
};
use environment::{BitMask, EnvSpec, FaultPolicy, HostFault};
use vmm_backend::Backend;
use vmm_core::bringup::{BackendKind, boot_linux_selected};
use vmm_core::control::{ControlServer, VmmFactory, server_caps};

type DynVmm = vmm_core::vmm::Vmm<Box<dyn Backend>>;

const GUEST_RAM_LEN: usize = 2 << 30;
const BASE_SEED: u64 = 0x0028_C0FF_EE5E_EDC0;
/// The seed each branched run reseeds to (the recorded env carries it).
const BRANCH_SEED: u64 = 0x595C_05EE_D005_9059;
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

/// Drive one `ControlServer` verb, panicking loudly on a session-fatal `ServeError`
/// (never a silent skip). Returns the recoverable `Result<Reply, ControlError>`.
fn call<B: Backend>(
    s: &mut ControlServer<B>,
    req: &Request,
) -> Result<Reply, control_proto::ControlError> {
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

fn wire(fault: HostFault) -> WireHostFault {
    WireHostFault(fault.encode())
}

#[test]
#[ignore = "box-only host-plane enforcement gate (LOADED patched KVM + built Postgres image + \
            det-cfl-v1 host); run per docs/BOX-PINNING.md"]
fn host_plane_record_replay_closure() {
    require_kvm();
    require_host_baseline();
    let kernel = require_artifact("bzImage");
    let initramfs = std::env::var("INITRAMFS")
        .map(|n| require_artifact(&n))
        .unwrap_or_else(|_| require_artifact("initramfs-postgres.cpio.gz"));

    // Compose the control server around a live Postgres VM; the factory boots a
    // fresh, equivalently-composed VM for each branch/replay restore target.
    let live = boot_pg(&kernel, &initramfs, BASE_SEED);
    let (fk, fi) = (kernel.clone(), initramfs.clone());
    let factory: VmmFactory<Box<dyn Backend>> = Box::new(move || {
        boot_linux_selected(
            BackendKind::Patched,
            &fk,
            &fi,
            GUEST_RAM_LEN,
            &cmdline(),
            BASE_SEED,
        )
    });
    let mut s = ControlServer::new(live, factory);
    assert_eq!(
        expect_ok(&mut s, &Request::Hello(server_caps())),
        Reply::Hello(server_caps())
    );

    // 1. Seal the base snapshot, nudging past non-snapshottable boundaries (the
    //    task-58 retry: a NotQuiescent refusal ⇒ run a little further and retry).
    let retry_step = env_u64("HP_SNAP_STEP", 1_000_000);
    let mut vt = match run_until(&mut s, 0) {
        StopReason::Deadline { vtime } => vtime.0,
        other => panic!("vtime probe stopped non-Deadline: {other:?}"),
    };
    let mut attempts = 0usize;
    let base: SnapId = loop {
        attempts += 1;
        match call(&mut s, &Request::Snapshot) {
            Ok(Reply::SnapId(id)) => break id,
            Ok(other) => panic!("snapshot answered {other:?}"),
            Err(control_proto::ControlError::NotQuiescent) => {
                assert!(
                    attempts < 100_000,
                    "no snapshottable boundary within budget"
                );
                match run_until(&mut s, vt.saturating_add(retry_step)) {
                    StopReason::Deadline { vtime } => vt = vtime.0,
                    other => panic!("guest ended before a sealable boundary: {other:?}"),
                }
            }
            Err(e) => panic!("snapshot answered a ControlError: {e:?}"),
        }
    };
    let snapshot_vtime = vt;
    eprintln!("[hp] base sealed at V-time {snapshot_vtime} after {attempts} attempt(s)");

    // 2. The staged schedule: a CorruptMemory and an InjectInterrupt at chosen
    //    Moments past the snapshot, both reached before the run's deadline.
    let m1 = snapshot_vtime + env_u64("HP_M1_OFF", 500_000);
    let m2 = snapshot_vtime + env_u64("HP_M2_OFF", 1_500_000);
    let deadline = snapshot_vtime + env_u64("HP_DELTA", 5_000_000);
    let gpa = env_u64("HP_GPA", 0x0100_0000);
    let vector = env_u64("HP_VECTOR", 0x60) as u32;
    let corrupt = wire(HostFault::CorruptMemory {
        gpa,
        mask: BitMask(0xD15EA5ED_C0FFEE01),
    });
    let inject = wire(HostFault::InjectInterrupt { vector });

    // Run one branch → (optional perturbs) → run(deadline) → hash cycle from `base`.
    // Returns (stop, hash, recorded-env bytes).
    let schedule_run = |s: &mut ControlServer<Box<dyn Backend>>,
                        with_faults: bool|
     -> (StopReason, [u8; 32], Vec<u8>) {
        assert_eq!(
            expect_ok(
                s,
                &Request::Branch {
                    snap: base,
                    env: seeded_env(BRANCH_SEED)
                }
            ),
            Reply::Unit
        );
        if with_faults {
            assert_eq!(
                expect_ok(
                    s,
                    &Request::Perturb {
                        fault: corrupt.clone(),
                        at: Moment(m1)
                    }
                ),
                Reply::Unit
            );
            assert_eq!(
                expect_ok(
                    s,
                    &Request::Perturb {
                        fault: inject.clone(),
                        at: Moment(m2)
                    }
                ),
                Reply::Unit
            );
        }
        let stop = run_until(s, deadline);
        let h = hash_whole(s);
        let recorded = s.recorded_env().encode();
        (stop, h, recorded)
    };

    // (a) same schedule twice ⇒ bit-identical.
    let (stop_a1, h_a1, recorded) = schedule_run(&mut s, true);
    let (stop_a2, h_a2, _) = schedule_run(&mut s, true);

    // (b) replay the emitted Recorded env ⇒ same hash again.
    let rec_env = Reproducer {
        blob_version: EnvSpec::BLOB_VERSION,
        bytes: recorded.clone(),
    };
    assert_eq!(
        expect_ok(
            &mut s,
            &Request::Branch {
                snap: base,
                env: rec_env
            }
        ),
        Reply::Unit
    );
    let stop_b = run_until(&mut s, deadline);
    let h_b = hash_whole(&mut s);

    // (c) schedule-absent control ⇒ differs.
    let (stop_c, h_c, _) = schedule_run(&mut s, false);

    let a_eq = h_a1 == h_a2 && stop_a1 == stop_a2;
    let b_eq = h_b == h_a1 && stop_b == stop_a1;
    let c_diff = h_c != h_a1;
    let pass = a_eq && b_eq && c_diff;

    println!("\n[REPORT] task59-host-plane box gate");
    println!("  snapshot_vtime={snapshot_vtime} attempts={attempts}");
    println!(
        "  schedule: CorruptMemory{{gpa={gpa:#x}}}@{m1}, InjectInterrupt{{vector={vector:#x}}}@{m2}, deadline={deadline}"
    );
    println!("  (a) run#1 {} {}", hex(&h_a1), vmm_core_fmt_stop(&stop_a1));
    println!(
        "      run#2 {} {}   => IDENTICAL: {}",
        hex(&h_a2),
        vmm_core_fmt_stop(&stop_a2),
        a_eq
    );
    println!(
        "  (b) replay {} {}   => MATCHES run#1: {}",
        hex(&h_b),
        vmm_core_fmt_stop(&stop_b),
        b_eq
    );
    println!(
        "  (c) control {} {}   => DIFFERS: {}",
        hex(&h_c),
        vmm_core_fmt_stop(&stop_c),
        c_diff
    );
    println!(
        "  RESULT: {}  rc={}",
        if pass { "PASS" } else { "FAIL" },
        if pass { 0 } else { 1 }
    );

    assert!(
        a_eq,
        "(a) two runs of the same schedule diverged: {} vs {}",
        hex(&h_a1),
        hex(&h_a2)
    );
    assert!(
        b_eq,
        "(b) replay of the recorded env did not reproduce: {} vs {}",
        hex(&h_b),
        hex(&h_a1)
    );
    assert!(
        c_diff,
        "(c) the schedule-absent control matched the faulted run — the faults did not land"
    );
}

/// Compact stop rendering (local — `campaign_runner::fmt_stop` is not a dep here).
fn vmm_core_fmt_stop(stop: &StopReason) -> String {
    match stop {
        StopReason::Deadline { vtime } => format!("Deadline@{}", vtime.0),
        StopReason::Quiescent { vtime } => format!("Quiescent@{}", vtime.0),
        StopReason::Crash { vtime, info } => format!("Crash@{}[{}B]", vtime.0, info.detail.len()),
        other => format!("{other:?}"),
    }
}
