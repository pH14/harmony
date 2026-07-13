// SPDX-License-Identifier: AGPL-3.0-or-later
//! Box-only **moment-address** gate (task 80): prove that a
//! `(genesis-complete Reproducer, Moment)` pair materializes to a live session
//! at exactly that instruction, twice, byte-identically — the substrate for
//! everything in `docs/RESOLUTION.md`. Driven directly against the
//! [`ControlServer`] verbs (`hello`/`snapshot`/`branch`/`run`/`read`/`regs`/`hash`)
//! on the real patched-KVM Postgres workload.
//!
//! The spec's box gates (`tasks/80-inspection-verbs.md`):
//!   2. **The moment address.** Pick ≥ 4 mid-workload `Moment`s. For each,
//!      materialize `(env, moment)` **twice from genesis** → identical `regs`
//!      (including `rip` and the reported `Moment`), identical `read` of ≥ 3 probe
//!      regions, identical `hash(Whole)`.
//!   3. **Observation invariance.** A full inspection pass (regs + several reads)
//!      mid-materialization does not perturb the run: continuing to a later
//!      `Moment` yields the same `hash(Whole)` as an uninspected control run.
//!
//! The **materialization procedure** is the client-side helper [`materialize`]:
//! `branch(genesis, env)` then `run(until = moment)`, landing — via the
//! deterministic exact-`Moment` force-exit machinery (tasks 47/55, the patched
//! KVM) — with retired count == `moment` (a `StopReason::Deadline { vtime: moment }`).
//! `env` is a genesis-complete `Seeded` reproducer, so the whole trajectory is
//! recoverable from genesis.
//!
//! Run on `ssh <det-box>` with the LOADED patched KVM modules + the built Postgres
//! image, CPU-pinned per `docs/BOX-PINNING.md` (lease a core via `box-window.sh`;
//! never touch another lease's cores or its patched-KVM window). ALWAYS revert KVM
//! to stock **1396736** + verify after any patched run.
//! ```text
//! make -C guest fetch && make -C guest/linux postgres-image     # or copy a prebuilt image
//! taskset -c <core> cargo test -p vmm-core --release --test live_moment_address -- --ignored --nocapture
//! ```
//! Tunable via env (defaults below): `MA_GENESIS_STEP` (V-time ns to nudge past a
//! non-snapshottable genesis boundary), `MA_MOMENTS` (comma-separated V-time ns
//! offsets past genesis for the ≥ 4 addressed Moments), `MA_LATE_OFF` (the later
//! Moment for the invariance gate), `MA_PROBES` (comma-separated guest-physical
//! probe addresses for `read`), `MA_SEED` (the genesis env seed).
//!
//! Every precondition that would prevent a real run — no `/dev/kvm`, the
//! determinism cap absent (stock modules), or a non-baseline host — is a **loud
//! panic (test FAILURE)**, never an early-return `Ok`. macOS builds an empty test
//! binary; the verbs + the materialization/observation *logic* are covered
//! portably by the `src/control.rs` unit tests + the observation-invariance
//! proptest (`observations_never_change_hash_or_stop_outcomes`) and the
//! `moment_address_materializes_identically_twice` mock demonstration.
#![cfg(target_os = "linux")]

use control_proto::{
    HashScope, HostFault as WireHostFault, Moment, RegsView, Reply, Reproducer, Request, SnapId,
    StopConditions, StopMask, StopReason,
};
use environment::{BitMask, EnvSpec, FaultPolicy, HostFault as EnvHostFault};
use vmm_backend::Backend;
use vmm_core::bringup::{BackendKind, boot_linux_selected};
use vmm_core::control::{ControlServer, VmmFactory, server_caps};

type DynVmm = vmm_core::vmm::Vmm<Box<dyn Backend>>;

const GUEST_RAM_LEN: usize = 2 << 30;
const GENESIS_SEED: u64 = 0x0080_0080_C0FF_EE80;
const DEFAULT_CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t,force tsc=reliable \
     no_timer_check lpj=4000000 nokaslr nosmp maxcpus=1 nox2apic hpet=disable";
/// The per-call read cap the server enforces; probes are one machine word each,
/// well under it.
const PROBE_LEN: u32 = 32;

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

fn env_list(key: &str, default: &[u64]) -> Vec<u64> {
    std::env::var(key)
        .ok()
        .map(|v| v.split(',').filter_map(|s| s.trim().parse().ok()).collect())
        .filter(|v: &Vec<u64>| !v.is_empty())
        .unwrap_or_else(|| default.to_vec())
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

fn regs<B: Backend>(s: &mut ControlServer<B>) -> RegsView {
    match expect_ok(s, &Request::Regs) {
        Reply::Regs(v) => v,
        other => panic!("regs answered {other:?}"),
    }
}

fn read<B: Backend>(s: &mut ControlServer<B>, gpa: u64, len: u32) -> Vec<u8> {
    match expect_ok(s, &Request::Read { gpa, len }) {
        Reply::Bytes(b) => b,
        other => panic!("read answered {other:?}"),
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

/// A **no-op** host fault (`CorruptMemory` with a zero XOR mask at gpa 0) — it
/// changes no guest byte, but staging it at a `Moment` **arms the exact-count
/// arrival** so the following `run` lands there precisely. This is how the
/// deterministic force-exit machinery (tasks 47/55, the patched KVM) is invoked at
/// an exact `Moment`: the `run` deadline alone is *opportunistic* (task 58 reverted
/// the hard force-exit, so a bare deadline stops at the first boundary at-or-past
/// it — it overshoots), whereas a staged `Moment ≤ deadline` makes the run
/// `arm_arrival` → `run_until` to that exact retired count. XOR-0 leaves state
/// untouched, so the materialized point observed after the landing is the true one.
/// Both materializations of a `Moment` stage the identical marker, so the
/// twice-from-genesis comparison stays byte-exact.
fn arrival_marker(at: u64) -> Request {
    Request::Perturb {
        fault: WireHostFault(
            EnvHostFault::CorruptMemory {
                gpa: 0,
                mask: BitMask(0),
            }
            .encode(),
        ),
        at: Moment(at),
    }
}

/// One observation of a materialized point: the register view, the probe reads,
/// and the whole-state hash.
#[derive(Clone, PartialEq, Eq)]
struct Observation {
    regs: RegsView,
    probes: Vec<Vec<u8>>,
    hash: [u8; 32],
}

#[test]
#[ignore = "box-only moment-address gate (LOADED patched KVM + built Postgres image + \
            det-cfl-v1 host); run per docs/BOX-PINNING.md"]
fn moment_address_materializes_identically_twice() {
    require_kvm();
    require_host_baseline();
    let kernel = require_artifact("bzImage");
    let initramfs = std::env::var("INITRAMFS")
        .map(|n| require_artifact(&n))
        .unwrap_or_else(|_| require_artifact("initramfs-postgres.cpio.gz"));
    let seed = env_u64("MA_SEED", GENESIS_SEED);

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

    // 1. Seal the **genesis** snapshot, nudging past non-snapshottable boundaries
    //    (the task-58 retry: a NotQuiescent refusal ⇒ run a little further, retry).
    let retry_step = env_u64("MA_GENESIS_STEP", 1_000_000);
    let mut vt = match run_until(&mut s, 0) {
        StopReason::Deadline { vtime } => vtime.0,
        other => panic!("vtime probe stopped non-Deadline: {other:?}"),
    };
    let mut attempts = 0usize;
    let genesis: SnapId = loop {
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
    let genesis_vt = vt;
    eprintln!("[ma] genesis sealed at V-time {genesis_vt} after {attempts} attempt(s)");

    let env = seeded_env(seed);
    let probes = env_list("MA_PROBES", &[0x0010_0000, 0x0100_0000, 0x1000_0000]);
    let moments: Vec<u64> = env_list("MA_MOMENTS", &[500_000, 2_000_000, 8_000_000, 20_000_000])
        .into_iter()
        .map(|off| genesis_vt + off)
        .collect();
    assert!(
        moments.len() >= 4,
        "the gate requires >= 4 mid-workload Moments"
    );

    // The materialization procedure: branch(genesis, env) then run(until = moment).
    // `inspect` runs a full inspection pass immediately after landing (used by the
    // invariance check to prove observation does not perturb the continuation).
    let materialize = |s: &mut ControlServer<Box<dyn Backend>>, moment: u64| -> Observation {
        assert_eq!(
            expect_ok(
                s,
                &Request::Branch {
                    snap: genesis,
                    env: env.clone(),
                }
            ),
            Reply::Unit,
            "branch(genesis, env)"
        );
        // Arm the exact-count arrival at `moment`, then advance to it.
        expect_ok(s, &arrival_marker(moment));
        match run_until(s, moment) {
            StopReason::Deadline { vtime } => assert_eq!(
                vtime.0, moment,
                "run(until = moment) must land exactly at the addressed Moment"
            ),
            other => panic!("materialization to {moment} stopped non-Deadline: {other:?}"),
        }
        let regs = regs(s);
        assert_eq!(
            regs.moment.0, moment,
            "regs reports the retired count == the addressed Moment"
        );
        Observation {
            regs,
            probes: probes.iter().map(|&g| read(s, g, PROBE_LEN)).collect(),
            hash: hash_whole(s),
        }
    };

    println!("\n[REPORT] task80 moment-address box gate");
    println!("  genesis_vt={genesis_vt} attempts={attempts} seed={seed:#x}");
    println!("  probes={probes:x?}");
    let mut all_pass = true;
    // Column headers bound to locals so the width-formatted columns are not string
    // literal args (clippy::print_literal under the box's `-D warnings` gate).
    let (c_moment, c_rip, c_hash, c_regs, c_read) = ("moment", "rip", "hash8", "regs", "read");
    println!("  {c_moment:>14}  {c_rip:>18}  {c_hash:>10}  {c_regs:>4}  {c_read}");
    for &moment in &moments {
        let a = materialize(&mut s, moment);
        let b = materialize(&mut s, moment);
        let regs_eq = a.regs == b.regs;
        let read_eq = a.probes == b.probes;
        let hash_eq = a.hash == b.hash;
        let pass = regs_eq && read_eq && hash_eq;
        all_pass &= pass;
        println!(
            "  {moment:>14}  {:>#18x}  {:>10}  {:>4}  {}   => {}",
            a.regs.rip,
            &hex(&a.hash)[..8],
            if regs_eq { "ok" } else { "DIFF" },
            if read_eq { "ok" } else { "DIFF" },
            if pass { "IDENTICAL" } else { "DIVERGED" },
        );
        assert!(
            pass,
            "moment {moment}: two materializations from genesis diverged \
             (regs_eq={regs_eq} read_eq={read_eq} hash_eq={hash_eq}): \
             a.hash={} b.hash={}",
            hex(&a.hash),
            hex(&b.hash)
        );
    }

    // Gate 3 — observation invariance: a full inspection pass at an intermediate
    // Moment must not perturb the continuation to a later Moment. Reach `late`
    // through `mid` twice; once inspect at `mid`, once do not — the `hash(Whole)`
    // at `late` must match.
    let mid = moments[0];
    let late = genesis_vt + env_u64("MA_LATE_OFF", 40_000_000);
    let continue_to_late = |s: &mut ControlServer<Box<dyn Backend>>, inspect: bool| -> [u8; 32] {
        assert_eq!(
            expect_ok(
                s,
                &Request::Branch {
                    snap: genesis,
                    env: env.clone(),
                }
            ),
            Reply::Unit
        );
        // Advance to `mid` via an exact-count arrival, inspect (or not), then
        // continue to `late` the same way.
        expect_ok(s, &arrival_marker(mid));
        assert!(matches!(run_until(s, mid), StopReason::Deadline { .. }));
        if inspect {
            let _ = regs(s);
            for &g in &probes {
                let _ = read(s, g, PROBE_LEN);
            }
            let _ = regs(s);
        }
        expect_ok(s, &arrival_marker(late));
        assert!(matches!(run_until(s, late), StopReason::Deadline { .. }));
        hash_whole(s)
    };
    let inspected = continue_to_late(&mut s, true);
    let control = continue_to_late(&mut s, false);
    let invariance_ok = inspected == control;
    all_pass &= invariance_ok;
    println!(
        "  invariance: inspect@{mid}→{late} {} vs control {}   => {}",
        &hex(&inspected)[..8],
        &hex(&control)[..8],
        if invariance_ok { "SAME" } else { "PERTURBED" },
    );
    println!("  RESULT: {}", if all_pass { "PASS" } else { "FAIL" });
    assert!(
        invariance_ok,
        "an inspection pass mid-materialization perturbed the continuation: {} vs {}",
        hex(&inspected),
        hex(&control)
    );
}
