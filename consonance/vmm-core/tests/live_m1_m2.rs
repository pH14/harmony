// SPDX-License-Identifier: AGPL-3.0-or-later
//! Box-only live M1/M2 gates (`#[cfg(target_os = "linux")]` **and `#[ignore]`**, on
//! `ssh <det-box>`, CPU-pinned per `docs/BOX-PINNING.md`, against the real
//! `KvmBackend`).
//!
//! - **M1 — boots & prints.** `boot(KvmBackend::new(), hello, ram)` then `run()`:
//!   the serial capture equals `guest/golden/hello.txt` byte-for-byte **and** the
//!   terminal reason is a clean isa-debug-exit `PASS` (`DebugExit { code: 0 }`).
//! - **M2 — deterministic twice.** A `unison::SubjectFactory` builds a fresh
//!   `Vmm<KvmBackend>` per payload; for both `hello` and `compute`, two runs
//!   produce identical `state_hash` **and** identical serial; `compute`'s serial
//!   also equals `guest/golden/compute.txt`.
//!
//! **Gate honesty (why `#[ignore]`).** These tests need real KVM, the built
//! payloads, and a host that matches the frozen `det-cfl-v1` baseline — none of
//! which exist in the default `cargo nextest` / coverage lane. So they are
//! `#[ignore]`d (out of the default lane, exactly like the task-14 KVM integration
//! tests): default CI shows them **not-run**, never a vacuous green. They run only
//! when invoked explicitly on the box:
//!
//! ```sh
//! cd guest/payloads && cargo build --release           # build the payloads first
//! taskset -c 1 cargo test -p vmm-core --test live_m1_m2 -- --ignored --test-threads=1
//! ```
//!
//! When run, every precondition that would prevent a *real* boot — no `/dev/kvm`,
//! an unbuilt payload, or a host that fails the §1.1 baseline — is a **loud panic
//! (test FAILURE)**, never an early-return `Ok` that nextest counts as passed. As of
//! the `det-cfl-v1` re-baseline (contract-v3, task 11) the box (an i9-9900K, Coffee
//! Lake-S) **matches** the §1.1 baseline, so `host_assert_report` shows all PASS and
//! the host-baseline precondition no longer blocks M1/M2 (see
//! `consonance/vmm-core/IMPLEMENTATION.md` host-baseline note).
//!
//! The whole file compiles only on Linux (`KvmBackend` is Linux-only); on macOS it
//! is an empty test binary.
#![cfg(target_os = "linux")]

use std::path::PathBuf;

use unison::{RunOutcome, Subject, SubjectFactory};
use vmm_backend::KvmBackend;
use vmm_core::bringup::boot;
use vmm_core::vmm::{TerminalReason, Vmm};

/// 256 MiB of guest RAM (matches the task-04 QEMU `-m 256` gate).
const GUEST_RAM_LEN: usize = 256 << 20;

/// Repo root, derived from this crate's manifest dir (`consonance/vmm-core`).
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// The built payload ELF (`guest/payloads/target/x86_64-unknown-none/release/<name>`).
fn payload_path(name: &str) -> PathBuf {
    repo_root()
        .join("guest/payloads/target/x86_64-unknown-none/release")
        .join(name)
}

/// The byte-exact serial oracle (`guest/golden/<name>.txt`).
fn golden(name: &str) -> Vec<u8> {
    std::fs::read(repo_root().join("guest/golden").join(format!("{name}.txt")))
        .unwrap_or_else(|e| panic!("read golden {name}.txt: {e}"))
}

/// Require a built payload, else **panic (loud FAILURE)**. These tests are
/// `#[ignore]`d and run only explicitly on the box, where the M1/M2 step builds
/// `guest/payloads` first — so an unbuilt payload is a real failure to surface,
/// never an early-return `Ok` that counts as a vacuous pass.
fn require_payload(name: &str) -> Vec<u8> {
    std::fs::read(payload_path(name)).unwrap_or_else(|e| {
        panic!(
            "payload `{name}` not built ({e}) — build it first: \
             `cd guest/payloads && cargo build --release` (target x86_64-unknown-none) on the box."
        )
    })
}

/// Require `/dev/kvm` + a constructible vCPU (Intel VMX + perf_event), else
/// **panic (loud FAILURE)**. Run on `ssh <det-box>` (bare-metal Intel, not nested).
fn require_kvm() {
    assert!(
        std::path::Path::new("/dev/kvm").exists(),
        "/dev/kvm absent — run this `#[ignore]`d box gate on `ssh <det-box>` (Intel VMX, \
         perf_event), CPU-pinned `taskset -c 1` per docs/BOX-PINNING.md."
    );
    if let Err(e) = KvmBackend::new() {
        panic!(
            "KvmBackend::new() failed ({e}) — needs bare-metal Intel VMX + /dev/kvm access on \
             `ssh <det-box>` (not nested virtualization)."
        );
    }
}

/// Print the CPU-MSR-CONTRACT §1.1 host-baseline assertion report; return whether
/// **every** assertion passes. Pure diagnostic — it does not decide pass/fail.
fn print_host_baseline_report() -> bool {
    let report = vmm_core::hostassert::report();
    let mut all_pass = true;
    eprintln!("[host-assert] CPU-MSR-CONTRACT §1.1 host-baseline report:");
    for o in &report {
        let tag = if o.pass { "PASS" } else { "FAIL" };
        eprintln!(
            "[host-assert]   {tag}  {}: expected {}, observed {}",
            o.key, o.expected, o.actual
        );
        all_pass &= o.pass;
    }
    all_pass
}

/// Require the live host to satisfy the §1.1 `det-cfl-v1` baseline, else **panic
/// (loud FAILURE)** with the full per-assertion report. A host outside the frozen
/// determinism domain cannot run the contract faithfully, so this is a real,
/// visible failure — never a silent skip-as-pass. `boot` itself also refuses such
/// a host (`VmmError::HostAssert`). As of contract-v3 the determinism box (i9-9900K,
/// Coffee Lake-S) **matches** this baseline, so on the box this precondition passes;
/// it still fails-loud on any other host (the assert is never loosened to fake a pass).
fn require_host_baseline() {
    if !print_host_baseline_report() {
        panic!(
            "host CPU does not match the det-cfl-v1 baseline (CPU-MSR-CONTRACT §1.1) — M1/M2 \
             cannot run the frozen contract faithfully here. Run on the det-cfl-v1 determinism \
             box (i9-9900K, microcode 0xf8) per docs/BOX-PINNING.md; see \
             consonance/vmm-core/IMPLEMENTATION.md host-baseline note. The assert is NOT loosened \
             to fake a pass."
        );
    }
}

/// Standalone host-baseline reporting harness (`#[ignore]`d, box-only): prints the
/// per-assertion disposition for the integrator. A pure diagnostic — it never
/// asserts pass/fail (that decision is the integrator's), so it does not claim
/// anything about whether M1/M2 boot.
#[test]
#[ignore = "box-only host-baseline diagnostic; run on `ssh <det-box>` with `-- --ignored`"]
fn host_assert_report() {
    let _ = print_host_baseline_report();
}

// --- M1 -------------------------------------------------------------------

#[test]
#[ignore = "box-only live gate (real KVM + built payloads + det-cfl-v1 host); run on \
            `ssh <det-box>` with `-- --ignored`"]
fn m1_hello_boots_and_prints() {
    require_kvm();
    require_host_baseline();
    let hello = require_payload("hello");

    let backend = KvmBackend::new().expect("KvmBackend::new");
    let mut vmm = boot(backend, &hello, GUEST_RAM_LEN).expect("boot hello");
    let result = vmm.run().expect("run hello");

    assert_eq!(
        result.serial,
        golden("hello"),
        "M1 serial must equal guest/golden/hello.txt byte-for-byte"
    );
    assert_eq!(
        result.reason,
        TerminalReason::DebugExit { code: 0 },
        "M1 must end on a clean isa-debug-exit PASS — not Hlt, not a non-zero code"
    );
}

// --- M2 -------------------------------------------------------------------

/// A `unison::Subject` over a live `Vmm<KvmBackend>`. Work-counting /
/// `run_to(target)` bisection is a later-phase concern (V-time): this milestone
/// runs the payload to terminal on the first `run_to`, then reports `Halted`.
struct VmmMachine {
    vmm: Vmm<KvmBackend>,
    ran: bool,
    serial: Vec<u8>,
    reason: Option<TerminalReason>,
}

impl Subject for VmmMachine {
    fn run_to(&mut self, _target: u64) -> Result<RunOutcome, unison::SubjectError> {
        if !self.ran {
            let r = self.vmm.run().expect("live run to terminal");
            self.serial = r.serial;
            self.reason = Some(r.reason);
            self.ran = true;
        }
        Ok(RunOutcome::Halted)
    }

    fn work(&self) -> u64 {
        // 0 before the run, 1 at terminal (no intra-run work counter yet).
        u64::from(self.ran)
    }

    fn state_hash(&self) -> [u8; 32] {
        self.vmm.state_hash()
    }
}

/// Builds a fresh `Vmm<KvmBackend>` for one payload (the only place a concrete
/// backend is named, per the trait-seam discipline).
struct PayloadFactory {
    payload: Vec<u8>,
}

impl SubjectFactory for PayloadFactory {
    type M = VmmMachine;

    fn spawn(&self, _seed: u64) -> VmmMachine {
        let backend = KvmBackend::new().expect("KvmBackend::new");
        let vmm = boot(backend, &self.payload, GUEST_RAM_LEN).expect("boot payload");
        VmmMachine {
            vmm,
            ran: false,
            serial: Vec::new(),
            reason: None,
        }
    }
}

/// Run one payload twice via the unison adapter and assert determinism +
/// (optionally) the golden serial.
fn assert_deterministic_twice(name: &str, payload: Vec<u8>, check_golden: bool) {
    let factory = PayloadFactory { payload };

    let mut a = factory.spawn(0);
    a.run_to(u64::MAX).expect("run a");
    let mut b = factory.spawn(0);
    b.run_to(u64::MAX).expect("run b");

    assert_eq!(
        a.state_hash(),
        b.state_hash(),
        "M2 {name}: two runs must produce identical state_hash over all observable state"
    );
    assert_eq!(
        a.serial, b.serial,
        "M2 {name}: two runs must produce identical serial output"
    );
    assert_eq!(
        a.reason,
        Some(TerminalReason::DebugExit { code: 0 }),
        "M2 {name}: must end on a clean PASS"
    );
    if check_golden {
        assert_eq!(
            a.serial,
            golden(name),
            "M2 {name}: serial must equal guest/golden/{name}.txt"
        );
    }
}

#[test]
#[ignore = "box-only live gate (real KVM + built payloads + det-cfl-v1 host); run on \
            `ssh <det-box>` with `-- --ignored`"]
fn m2_hello_deterministic_twice() {
    require_kvm();
    require_host_baseline();
    let hello = require_payload("hello");
    assert_deterministic_twice("hello", hello, true);
}

#[test]
#[ignore = "box-only live gate (real KVM + built payloads + det-cfl-v1 host); run on \
            `ssh <det-box>` with `-- --ignored`"]
fn m2_compute_deterministic_twice() {
    require_kvm();
    require_host_baseline();
    let compute = require_payload("compute");
    assert_deterministic_twice("compute", compute, true);
}
