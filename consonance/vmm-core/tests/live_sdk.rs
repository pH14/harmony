// SPDX-License-Identifier: AGPL-3.0-or-later
//! Box-only guest-SDK gates (task 73), `#[cfg(target_os = "linux")]` **and
//! `#[ignore]`d**: boot the SDK-instrumented `sdk-demo` payload on the **patched**
//! backend, drive it through the task-58 [`ControlServer`] with the task-73
//! doorbell seam, and assert the three frontier gates:
//!
//! - **Gate A (determinism):** the same seed twice ⇒ a byte-identical
//!   `Moment`-stamped SDK event stream and an equal `state_hash`. (Raw
//!   `(Moment, event_id, bytes)` equality ⇒ decoded `(Moment, GuestEvent)`
//!   equality, since `link::decode_events` is a pure function — asserted on the
//!   raw stream here so this consonance test needs no dissonance dependency.)
//! - **Gate B (the Bug path):** a buggify-gated `assert_always` violation surfaces
//!   `StopReason::Assertion`; `branch(genesis, bug.env)` reproduces it N/N (N ≥ 8).
//! - **Gate C (never-fired):** two declared `sometimes` points, one wired to fire
//!   (`commit_seen`) and one never (`rollback_seen`) — the fired one appears in
//!   the event stream, the other never does (the never-fired detection the
//!   portable `link::Catalog` fold reports).
//!
//! The SDK-channel snapshot/restore fix (a fork continues the seeded streams +
//! keeps the catalog) is verified **portably** — a bare payload has no
//! synchronized mid-run point to seal at (see the NOTE below gate B).
//!
//! Box-only: needs the LOADED patched `/dev/kvm`, `perf_event`, and the
//! `det-cfl-v1` host; `#[ignore]`d so a plain `cargo nextest` shows it not-run.
//! Run CPU-pinned per `docs/BOX-PINNING.md`, reverting KVM to stock afterwards:
//!   `cargo test -p vmm-core --release --test live_sdk -- --ignored --nocapture`
#![cfg(target_os = "linux")]

use std::path::PathBuf;

use control_proto::{
    Environment, HashScope, Reply, Request, SnapId, StopConditions, StopMask, StopReason, VTime,
};
use environment::{EnvSpec, FaultPolicy};
use vmm_backend::Backend;
use vmm_core::bringup::{BackendKind, boot_selected};
use vmm_core::control::{ControlServer, VmmFactory, server_caps};

type DynServer = ControlServer<Box<dyn Backend>>;

/// A captured raw SDK event stream: `(Moment, event_id, detail bytes)` triples.
type SdkEvents = Vec<(u64, u32, Vec<u8>)>;

/// 128 MiB is ample for the tiny bare-metal `sdk-demo` (multiboot, no initramfs).
const GUEST_RAM_LEN: usize = 128 << 20;
const SEED: u64 = 0x5DC0_FFEE_1234_5678;
/// A generous V-time deadline so a (bug-free) demo run always terminates without
/// an infinite loop if it ever spins.
const DEADLINE: u64 = 50_000_000;
/// The demo's buggify site (`slow_disk`) and its planted always-assertion.
const BUGGIFY_POINT: u32 = 50;
const ALWAYS_POINT: u32 = 20;
/// The two `sometimes` points: one fires every iteration, one never.
const COMMIT_SEEN: u32 = 1;
const ROLLBACK_SEEN: u32 = 2;
/// SDK wire layout (mirror of `guest/sdk/src/wire.rs`) — the assert namespace.
const NS_ASSERT: u32 = 1;
const NS_SHIFT: u32 = 24;

fn assert_event_id(point: u32) -> u32 {
    (NS_ASSERT << NS_SHIFT) | point
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn payload_bytes() -> Vec<u8> {
    let p = repo_root().join("guest/payloads/target/x86_64-unknown-none/release/sdk-demo");
    std::fs::read(&p).unwrap_or_else(|e| {
        panic!(
            "sdk-demo payload not found at {p:?} ({e}) — build it on the box first: \
             `cd guest/payloads && cargo build -p sdk-demo --release`."
        )
    })
}

fn require_kvm() {
    assert!(
        std::path::Path::new("/dev/kvm").exists(),
        "/dev/kvm absent — run this `#[ignore]`d box gate on the determinism box with the LOADED \
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

fn boot_demo(payload: &[u8], seed: u64) -> vmm_core::vmm::Vmm<Box<dyn Backend>> {
    let mut vmm = boot_selected(BackendKind::Patched, payload, GUEST_RAM_LEN, seed).expect(
        "boot_selected(Patched, sdk-demo) — needs the LOADED patched KVM + perf + det-cfl-v1 host",
    );
    vmm.wire_snapshot_hashing();
    vmm
}

fn server(seed: u64) -> DynServer {
    let payload = payload_bytes();
    let live = boot_demo(&payload, seed);
    let factory: VmmFactory<Box<dyn Backend>> = Box::new(move || Ok(boot_demo(&payload, seed)));
    ControlServer::new(live, factory)
}

/// Drive one verb, panicking loudly on a session-fatal `ServeError`.
fn drive(s: &mut DynServer, req: &Request) -> Reply {
    match s.handle(req) {
        Ok(Ok(reply)) => reply,
        Ok(Err(e)) => panic!("recoverable ControlError from {req:?}: {e:?}"),
        Err(e) => panic!("session-fatal ServeError from {req:?}: {e:?}"),
    }
}

fn hello(s: &mut DynServer) {
    assert_eq!(
        drive(s, &Request::Hello(server_caps())),
        Reply::Hello(server_caps())
    );
}

fn snapshot(s: &mut DynServer) -> SnapId {
    match drive(s, &Request::Snapshot) {
        Reply::SnapId(id) => id,
        other => panic!("expected SnapId reply, got {other:?}"),
    }
}

fn branch(s: &mut DynServer, snap: SnapId, env: Environment) {
    assert_eq!(drive(s, &Request::Branch { snap, env }), Reply::Unit);
}

fn run_once(s: &mut DynServer) -> StopReason {
    // Arm every class (moot for the seed-driven substrate — the SDK stops always
    // surface — but harmless); the deadline bounds a runaway run.
    let until = StopConditions {
        deadline: Some(VTime(DEADLINE)),
        on: StopMask(u32::MAX),
    };
    match drive(
        s,
        &Request::Run {
            until,
            resolve: None,
        },
    ) {
        Reply::Stop(stop) => stop,
        other => panic!("expected Stop reply, got {other:?}"),
    }
}

/// Run to a **terminal** stop, stepping past any `SnapshotPoint`. The bare demo
/// has no V-time intercept after `setup_complete`, so its deferred snapshot point
/// (round-4 P1) never actually surfaces here — but stay total in case it does.
fn run_to_terminal(s: &mut DynServer) -> StopReason {
    for _ in 0..64 {
        let stop = run_once(s);
        if !matches!(stop, StopReason::SnapshotPoint { .. }) {
            return stop;
        }
    }
    panic!("too many snapshot points — the demo should reach a terminal stop");
}

fn state_hash(s: &mut DynServer) -> [u8; 32] {
    match drive(
        s,
        &Request::Hash {
            scope: HashScope::Whole,
        },
    ) {
        Reply::Hash(h) => h,
        other => panic!("expected Hash reply, got {other:?}"),
    }
}

/// The demo's branch env: a pure seed, plus a buggify-only policy that either
/// leaves `slow_disk` cold (a clean full run) or makes it always fire (the bug).
fn branch_env(seed: u64, buggify_fires: bool) -> Environment {
    let mut policy = FaultPolicy::none();
    if buggify_fires {
        policy
            .set_buggify_point(BUGGIFY_POINT, 1, 1)
            .expect("den >= 1");
    }
    let spec = EnvSpec::Seeded { seed, policy };
    Environment {
        blob_version: EnvSpec::BLOB_VERSION,
        bytes: spec.encode(),
    }
}

fn events(s: &DynServer) -> SdkEvents {
    s.vmm().map(|v| v.sdk_events().to_vec()).unwrap_or_default()
}

fn hex(d: &[u8; 32]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// GATE A — determinism: same seed twice ⇒ byte-identical event stream + equal
/// `state_hash`.
#[test]
#[ignore = "box-only: needs the LOADED patched KVM + perf + det-cfl-v1 host"]
fn box_gate_a_sdk_run_is_deterministic() {
    require_kvm();
    require_host_baseline();

    let once = |seed: u64| -> (SdkEvents, [u8; 32]) {
        let mut s = server(seed);
        hello(&mut s);
        let genesis = snapshot(&mut s);
        branch(&mut s, genesis, branch_env(seed, false)); // buggify cold ⇒ clean run
        let stop = run_to_terminal(&mut s);
        assert!(
            matches!(stop, StopReason::Quiescent { .. }),
            "a buggy-free demo run reaches PASS (Quiescent), got {stop:?}"
        );
        let ev = events(&s);
        (ev, state_hash(&mut s))
    };

    let (ev1, h1) = once(SEED);
    let (ev2, h2) = once(SEED);
    eprintln!("[gate A] {} events, state_hash {}", ev1.len(), hex(&h1));
    assert_eq!(
        ev1, ev2,
        "byte-identical decoded event stream across same-seed runs"
    );
    assert_eq!(h1, h2, "equal state_hash across same-seed runs");
    assert!(!ev1.is_empty(), "the SDK demo emitted events");
    // The commit_seen sometimes point fired; rollback_seen never (gate C shape).
    let fired: Vec<u32> = ev1.iter().map(|(_, id, _)| *id).collect();
    assert!(
        fired.contains(&assert_event_id(COMMIT_SEEN)),
        "commit_seen fired"
    );
    assert!(
        !fired.contains(&assert_event_id(ROLLBACK_SEEN)),
        "rollback_seen never fired"
    );
}

/// GATE B — the Bug path: buggify-gated always-violation ⇒ `StopReason::Assertion`,
/// reproduced N/N by `branch(genesis, bug.env)`.
#[test]
#[ignore = "box-only: needs the LOADED patched KVM + perf + det-cfl-v1 host"]
fn box_gate_b_buggify_violation_replays_n_of_n() {
    require_kvm();
    require_host_baseline();
    const N: usize = 8;

    let mut s = server(SEED);
    hello(&mut s);
    let genesis = snapshot(&mut s);

    // Find the bug: buggify hot ⇒ balance goes negative ⇒ always-violation.
    branch(&mut s, genesis, branch_env(SEED, true));
    let stop = run_to_terminal(&mut s);
    let ev = match &stop {
        StopReason::Assertion { ev, .. } => ev.clone(),
        other => panic!("expected StopReason::Assertion, got {other:?}"),
    };
    assert_eq!(
        ev.id, ALWAYS_POINT,
        "the balance_nonneg (id {ALWAYS_POINT}) assertion"
    );
    eprintln!("[gate B] assertion fired: point {}", ev.id);

    // The genesis-complete reproducer.
    let bug_env = Environment {
        blob_version: EnvSpec::BLOB_VERSION,
        bytes: s.recorded_env().encode(),
    };

    // Replay N/N from genesis.
    for i in 0..N {
        branch(&mut s, genesis, bug_env.clone());
        match run_to_terminal(&mut s) {
            StopReason::Assertion { ev: ev2, .. } => {
                assert_eq!(ev2.id, ALWAYS_POINT, "replay {i} reproduced the assertion");
            }
            other => panic!("replay {i} did not reproduce the assertion: {other:?}"),
        }
    }
    eprintln!("[gate B] reproduced {N}/{N}");
}

// NOTE — the finding-1 fix (the SDK channel's seeded-stream position + event log
// survive snapshot/restore) is verified **portably**, not by a box gate: a bare
// `sdk-demo` payload has no V-time intercept (no RDTSC/RDRAND), so it never
// reaches a synchronized mid-run point to seal at. `setup_complete` rings at a
// hypercall-doorbell OUT, which is PMU-skid-tainted (`save_vm_state` reports
// `NotQuiescent` there) — so the snapshot point is **deferred** to the next
// synchronized boundary (round-4 P1), which a real RDTSC-heavy workload reaches
// but this bare demo never does (so it simply never surfaces one). The fix is
// exercised by `vmm::tests::sdk_snapshot_restore_resumes_the_seeded_streams` (the
// stream continuation), `environment`'s `stream_state_resumes_both_streams_exactly`,
// the conductor `setup_complete_yields_a_usable_seal_...` loopback gate (the
// deferred point surfaces sealably over the wire), and every mock control test
// that snapshots/branches/replays/drops with the SDK channel wired.

/// GATE C — never-fired: `commit_seen` fires, `rollback_seen` never; the fired one
/// appears in the event stream. (The `link::Catalog` fold that turns this into the
/// never-fired report is proven portably in `dissonance/link`.)
#[test]
#[ignore = "box-only: needs the LOADED patched KVM + perf + det-cfl-v1 host"]
fn box_gate_c_never_fired_detection() {
    require_kvm();
    require_host_baseline();

    let mut s = server(SEED);
    hello(&mut s);
    let genesis = snapshot(&mut s);
    branch(&mut s, genesis, branch_env(SEED, false));
    let _ = run_to_terminal(&mut s);

    let fired: Vec<u32> = events(&s).iter().map(|(_, id, _)| *id).collect();
    assert!(
        fired.contains(&assert_event_id(COMMIT_SEEN)),
        "commit_seen (the wired sometimes point) fired"
    );
    assert!(
        !fired.contains(&assert_event_id(ROLLBACK_SEEN)),
        "rollback_seen (the unwired sometimes point) never fired — the never-fired detection"
    );
    eprintln!("[gate C] commit_seen fired, rollback_seen never — never-fired detection OK");
}
