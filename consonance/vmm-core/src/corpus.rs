// SPDX-License-Identifier: AGPL-3.0-or-later
//! The VMM-backed [`unison::Subject`] bridge — "the VMM running a payload" as a
//! logical guest the `acceptance-suite` oracles drive (corpus box-integration, task 28).
//!
//! `acceptance-suite` (#48) runs its O1/O2/O3 oracles over any [`unison::Subject`];
//! `consonance/acceptance-suite/payloads` (#49) ships the C1 instruction-sweep payloads. This module is
//! the third piece: a [`CorpusMachine`] that wraps a [`Vmm`] running one payload,
//! so the determinism/conformance corpus actually executes on the patched backend.
//! It is the frontier glue the dissonance ruling places **in vmm-core** (above the
//! `Backend` trait): the bridge is generic over the backend, so the stream-digest
//! and `Subject`-contract logic is unit-tested on macOS against a scripted
//! `MockBackend`, while the live patched path (a vendor composition root —
//! `vendor::x86::bringup::boot_patched_corpus`) is box-only.
//!
//! ## Two digests, two oracles
//!
//! The bridge keeps the O2/O3 distinction (`docs/DETERMINISM-CORPUS.md`): O1
//! (determinism) compares [`Subject::state_hash`] — the full V-time/RAM/entropy
//! state (`Vmm::state_hash`, unchanged from #45) **folded with the report-stream
//! `observable_digest`**, so O1 also catches a divergence confined to the report
//! channel — while [`Subject::observable_digest`] hashes **only** the **report
//! stream + the serial banner** ([`Vmm::observable_digest`]), the guest-observable
//! conformance output O2 pins to a golden. A payload that is perfectly
//! deterministic but reports a constant still has a meaningful (and, for an RNG
//! payload, seed-sensitive) observable digest, exactly what O2/O3 need.
//!
//! Folding the report stream into `Subject::state_hash` (not into `Vmm::state_hash`)
//! is the key soundness fix: `Vmm::state_hash` stays byte-identical for M1/M2/P6,
//! yet two same-seed runs that differ only in `REPORT_PORT` values hash differently
//! here, so the O1 determinism oracle is not blind to the report channel.
//!
//! ## `run_to` granularity (a deliberate, documented limitation)
//!
//! A C1 payload is a short bare-metal program that always runs to a terminal
//! (`isa-debug-exit` / `HLT`); intra-run V-time work-targeting (stopping the vCPU
//! at an arbitrary work count) needs the `run_until` deadline path, a later phase.
//! So [`CorpusMachine::run_to`] runs the payload to terminal on its first call and
//! then reports [`RunOutcome::Halted`] — the same shape as the M2 adapter
//! (`tests/live_m1_m2.rs`). This is exactly what the determinism oracle needs: two
//! runs at one seed are compared at the terminal checkpoint, where O1 asserts
//! bit-identical `state_hash`. A run that fails (a payload that trips a contract
//! violation on the patched backend) does **not** panic here — the error is
//! captured ([`CorpusMachine::run_error`]) so the box runner can fail loudly on it
//! rather than mistake a deterministic *failure* for a deterministic *pass*.

use sha2::Digest as _;
use unison::{RunOutcome, Subject, SubjectError};
use vmm_backend::Backend;

use crate::vendor::Vendor;

use crate::vmm::Vmm;

/// A [`unison::Subject`] over a [`Vmm`] running one corpus payload. Generic over
/// the backend so the bridge is exercised by both the macOS `MockBackend` unit
/// tests and the box-only patched backend (composed by
/// `vendor::x86::bringup::boot_patched_corpus` — box-only, so not an intra-doc link).
///
/// `run_to` runs the payload to terminal on first call (see the module docs on
/// granularity); `work` is `0` before that run and `1` after (a fresh machine
/// starts at work `0`, per the `SubjectFactory` contract). `state_hash` is the O1
/// hash — the full `Vmm::state_hash` folded with the report-stream digest, so O1
/// sees the report channel; `observable_digest` is the report-stream + serial
/// O2/O3 digest.
pub struct CorpusMachine<B: Backend<A: Vendor>> {
    vmm: Vmm<B>,
    ran: bool,
    /// The stringified [`crate::vmm::VmmError`] if the terminal run failed; `None`
    /// on a clean run. Surfaced via [`CorpusMachine::run_error`] so a failed run
    /// is caught explicitly instead of masquerading as a (deterministic) pass.
    error: Option<String>,
}

impl<B: Backend<A: Vendor>> CorpusMachine<B> {
    /// Wrap a configured, ready-to-run [`Vmm`] (the payload already loaded and the
    /// backend composed). The machine has not run yet (`work() == 0`); the first
    /// [`Subject::run_to`] drives it to terminal.
    pub fn new(vmm: Vmm<B>) -> Self {
        Self {
            vmm,
            ran: false,
            error: None,
        }
    }

    /// The error from the terminal run, if it failed (`None` on a clean run, and
    /// `None` until the first [`Subject::run_to`]). The box runner checks this so a
    /// run that errored deterministically is reported as a failure, not a pass.
    pub fn run_error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The underlying VMM, for inspection after a run (e.g. the report stream).
    pub fn vmm(&self) -> &Vmm<B> {
        &self.vmm
    }
}

impl<B: Backend<A: Vendor>> Subject for CorpusMachine<B> {
    fn run_to(&mut self, target: u64) -> Result<RunOutcome, SubjectError> {
        // Per the `Subject` contract, a rewind (`target < work()`) is an error checked
        // **before anything else**, even on a halted machine — e.g. `run_to(0)` after a
        // terminal run set `work() == 1`. (Machines cannot run backwards.)
        let current = self.work();
        if target < current {
            return Err(SubjectError::TargetBehind { target, current });
        }
        if !self.ran {
            // Run to terminal regardless of `target` (no intra-run work-targeting
            // yet — see the module docs). A failure is captured, not propagated as
            // a panic: the box runner inspects `run_error` and fails loudly there.
            self.ran = true;
            if let Err(e) = self.vmm.run() {
                self.error = Some(e.to_string());
            }
        }
        Ok(RunOutcome::Halted)
    }

    fn work(&self) -> u64 {
        // 0 before the run, 1 at terminal — a fresh machine starts at 0 (the
        // `SubjectFactory` contract) and the single terminal checkpoint is where
        // O1 compares state.
        u64::from(self.ran)
    }

    fn state_hash(&self) -> [u8; 32] {
        // Fold the report stream (via `observable_digest`) into the O1 hash so
        // `acceptance-suite` determinism DIRECTLY observes the report channel: a
        // same-seed run that diverges ONLY in `REPORT_PORT` values must fail O1,
        // not pass it (`Vmm::state_hash` deliberately excludes the report stream).
        // `Vmm::state_hash` itself is left UNCHANGED — this composition lives only
        // in the corpus Subject adapter, so M1/M2/P6 stay byte-identical.
        let mut h = sha2::Sha256::new();
        h.update(self.vmm.state_hash());
        h.update(self.vmm.observable_digest());
        h.finalize().into()
    }

    fn observable_digest(&self) -> [u8; 32] {
        self.vmm.observable_digest()
    }
}

/// The domain-separated digest of an ordered report stream and a serial banner —
/// the same bytes [`Vmm::observable_digest`] hashes, exposed as a free function so
/// a host-side tool can recompute an O2 golden from a captured report stream
/// without a live VMM. Pure, length-prefixed (`OBSV`); each report dword is hashed
/// little-endian in stream order.
pub fn observable_digest_of(report_stream: &[u32], serial: &[u8]) -> [u8; 32] {
    let mut hasher = sha2::Sha256::new();
    hasher.update(b"OBSV");
    hasher.update((report_stream.len() as u64).to_le_bytes());
    for v in report_stream {
        hasher.update(v.to_le_bytes());
    }
    hasher.update((serial.len() as u64).to_le_bytes());
    hasher.update(serial);
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    //! The bridge's pure logic — the stream digest and the `Subject` contract —
    //! driven by a scripted `MockBackend` on every platform (no `/dev/kvm`). The
    //! live patched path (`vendor::x86::bringup::boot_patched_corpus`) is box-only and covered by the
    //! `box_corpus` integration test.

    use super::*;
    use crate::vendor::x86::devices::{ISA_DEBUG_EXIT_PORT, REPORT_PORT, UART_PORT_BASE};
    use crate::vmm::GuestRam;
    use unison::{SubjectFactory, Verdict, compare_runs};
    use vmm_backend::{CommonExit, Exit, MockBackend, X86, X86Exit, X86Policy};

    /// A scripted exit sequence: emit `name`'s serial banner over the UART, report
    /// `values` (each as two report-port dwords, low then high), then a clean
    /// isa-debug-exit PASS.
    fn script(name: &str, values: &[u64]) -> Vec<Exit<X86>> {
        let mut exits = Vec::new();
        for &b in format!("PAYLOAD {name} PASS\n").as_bytes() {
            exits.push(Exit::Arch(X86Exit::Io {
                port: UART_PORT_BASE,
                size: 1,
                write: Some(u32::from(b)),
            }));
        }
        for &v in values {
            exits.push(Exit::Arch(X86Exit::Io {
                port: REPORT_PORT,
                size: 4,
                write: Some(v as u32),
            }));
            exits.push(Exit::Arch(X86Exit::Io {
                port: REPORT_PORT,
                size: 4,
                write: Some((v >> 32) as u32),
            }));
        }
        exits.push(Exit::Arch(X86Exit::Io {
            port: ISA_DEBUG_EXIT_PORT,
            size: 1,
            write: Some(0),
        }));
        exits
    }

    /// A deterministic [`SubjectFactory`] that replays a fixed scripted run — a
    /// stand-in for "the VMM running a payload" with no `/dev/kvm`.
    struct ScriptedFactory {
        name: String,
        values: Vec<u64>,
    }

    impl SubjectFactory for ScriptedFactory {
        type M = CorpusMachine<MockBackend>;
        fn spawn(&self, _seed: u64) -> Self::M {
            let mut backend = MockBackend::with_exits(script(&self.name, &self.values));
            backend
                .set_policy(&X86Policy::default())
                .expect("set_policy");
            CorpusMachine::new(Vmm::new(backend, GuestRam::new(0x1000).unwrap()))
        }
    }

    #[test]
    fn machine_runs_to_terminal_and_reports_halted() {
        let f = ScriptedFactory {
            name: "x".to_string(),
            values: vec![0xDEAD_BEEF_0000_0001],
        };
        let mut m = f.spawn(0);
        assert_eq!(m.work(), 0, "a fresh machine starts at work 0");
        assert_eq!(m.run_to(64).unwrap(), RunOutcome::Halted);
        assert_eq!(m.work(), 1, "work is 1 at terminal");
        assert!(m.run_error().is_none(), "a clean run has no error");
        // The reassembled report stream is (low, high) of the reported value.
        assert_eq!(m.vmm().report_stream(), [0x0000_0001, 0xDEAD_BEEF]);
    }

    #[test]
    fn run_to_rejects_a_rewind_below_current_work() {
        // Per the `Subject` contract, `run_to(target)` with `target < work()` is a
        // `TargetBehind` error (machines cannot run backwards), checked before the
        // halted no-op. After a terminal run `work() == 1`, so `run_to(0)` must error;
        // `run_to(>= 1)` is the halted no-op.
        let f = ScriptedFactory {
            name: "x".to_string(),
            values: vec![1],
        };
        let mut m = f.spawn(0);
        m.run_to(64).unwrap();
        assert_eq!(m.work(), 1);
        assert_eq!(
            m.run_to(0),
            Err(SubjectError::TargetBehind {
                target: 0,
                current: 1,
            }),
            "a rewind below current work must error, not silently no-op"
        );
        // target >= current work is the halted no-op.
        assert_eq!(m.run_to(1).unwrap(), RunOutcome::Halted);
        assert_eq!(m.run_to(5).unwrap(), RunOutcome::Halted);
    }

    #[test]
    fn observable_digest_overrides_state_hash_for_o3() {
        // The override hashes the report stream + serial, NOT state_hash — so two
        // runs whose reported values differ have distinct observable digests.
        let mut a = ScriptedFactory {
            name: "p".to_string(),
            values: vec![1],
        }
        .spawn(0);
        a.run_to(u64::MAX).unwrap();
        let mut b = ScriptedFactory {
            name: "p".to_string(),
            values: vec![2],
        }
        .spawn(0);
        b.run_to(u64::MAX).unwrap();
        assert_ne!(
            a.observable_digest(),
            b.observable_digest(),
            "different reported values ⇒ different observable digest"
        );
        // It matches the free-function recomputation from the captured stream +
        // the (known) serial banner — the host-side O2-golden recompute path.
        assert_eq!(
            a.observable_digest(),
            observable_digest_of(a.vmm().report_stream(), b"PAYLOAD p PASS\n"),
            "observable_digest equals observable_digest_of(report_stream, serial)"
        );
    }

    #[test]
    fn o1_catches_a_report_stream_only_divergence() {
        // Two runs identical in RAM / regs / serial but differing ONLY in the
        // report stream. `Vmm::state_hash` is (correctly) blind to the report
        // channel — but the folded `CorpusMachine::state_hash` that O1 compares
        // must catch it, so a same-seed divergence confined to REPORT_PORT fails
        // O1 instead of passing falsely.
        let fa = ScriptedFactory {
            name: "x".to_string(),
            values: vec![1, 2],
        };
        let fb = ScriptedFactory {
            name: "x".to_string(),
            values: vec![1, 3],
        };
        let mut a = fa.spawn(0);
        a.run_to(u64::MAX).unwrap();
        let mut b = fb.spawn(0);
        b.run_to(u64::MAX).unwrap();

        // The underlying Vmm hash is identical (the report stream is not in it —
        // M1/M2/P6 stay byte-identical)...
        assert_eq!(
            a.vmm().state_hash(),
            b.vmm().state_hash(),
            "Vmm::state_hash must stay blind to the report channel"
        );
        // ...but the Subject's folded O1 hash differs (it folds observable_digest).
        assert_ne!(
            a.state_hash(),
            b.state_hash(),
            "CorpusMachine::state_hash must fold the report stream so O1 sees it"
        );
        // And the O1 engine itself (compare_runs over the folded hash) reports
        // Diverged for a report-stream-only difference.
        let report = compare_runs(&fa, &fb, 0, 1, 100).unwrap();
        assert!(
            matches!(report.verdict, Verdict::Diverged { .. }),
            "O1 must catch a report-stream-only divergence: {report:?}"
        );
    }

    #[test]
    fn determinism_oracle_sees_a_clean_pass() {
        // O1: two runs at one seed are bit-identical. compare_runs drives the
        // bridge end-to-end (spawn → run_to → state_hash) and must report Identical.
        let f = ScriptedFactory {
            name: "det".to_string(),
            values: vec![10, 20, 30],
        };
        let report = compare_runs(&f, &f, 0xABCD, 64, 100_000).unwrap();
        assert_eq!(report.verdict, Verdict::Identical, "{report:?}");
        assert_eq!(
            report.halted_at,
            Some(1),
            "both halt at the terminal checkpoint"
        );
    }

    // -----------------------------------------------------------------------
    // Coexistence regression (PR #51 box O1). `acceptance_suite::check_determinism` →
    // `unison::compare_runs` spawns BOTH machines and THEN runs each in turn, while
    // the box `perf_event` work counter is a shared vCPU-thread resource — so the
    // second-spawned VM's counter accumulated the first VM's guest branches,
    // inflating its work-derived V-time (`last_intercept_work` → the hashed
    // `vtim:eff-vns`) and diverging two same-seed runs that differ only in spawn/run
    // ordering. `Vmm::run` now resets the work counter at the first guest entry
    // (`WorkSource::start_run`), making each run self-contained. Modelled here on the
    // mock with a shared-thread work source, driven through the real `compare_runs`.
    // -----------------------------------------------------------------------

    use crate::vendor::x86::contract_vclock_config;
    use crate::vmm::VtimeWiring;
    use crate::work::{WorkError, WorkSource};
    use std::cell::Cell;
    use std::rc::Rc;

    /// A `WorkSource` modelling the box `perf_event` counter's shared-thread
    /// semantics: one process-shared "thread guest-branch tally" that every live
    /// counter observes, each with a baseline captured at open. A `work()` read
    /// advances the shared tally (one retired guest branch), so a counter opened
    /// before another VM runs sees that VM's branches too — exactly
    /// `PerfWorkCounter`'s coexistence contamination — unless `start_run`
    /// re-baselines it at run-start (`reset_on_start`, the fix under test; with it
    /// `false` the test models the pre-fix counter).
    struct SharedThreadWork {
        thread: Rc<Cell<u64>>,
        base: u64,
        reset_on_start: bool,
    }
    impl SharedThreadWork {
        fn open(thread: Rc<Cell<u64>>, reset_on_start: bool) -> Self {
            let base = thread.get();
            Self {
                thread,
                base,
                reset_on_start,
            }
        }
    }
    impl WorkSource for SharedThreadWork {
        fn work(&self) -> Result<u64, WorkError> {
            // Reading models one retired guest branch on the shared thread.
            let raw = self.thread.get();
            self.thread.set(raw.saturating_add(1));
            Ok(raw.saturating_sub(self.base))
        }
        fn reset(&mut self) -> Result<(), WorkError> {
            self.base = self.thread.get();
            Ok(())
        }
        fn start_run(&mut self) -> Result<(), WorkError> {
            // The fix: re-baseline so work() counts only from here — the faithful
            // mock of `PerfWorkCounter`'s `IOC_RESET` at run start.
            if self.reset_on_start {
                self.base = self.thread.get();
            }
            Ok(())
        }
    }

    /// A factory whose spawned `CorpusMachine`s SHARE one thread work counter — the
    /// box reality `compare_runs` exposes (two coexisting patched VMs on one pinned
    /// vCPU thread). Each spawn is otherwise determinism-identical (same seed, fresh
    /// entropy via `VtimeWiring::new`); only the work counter's thread tally is shared.
    struct SharedWorkFactory {
        thread: Rc<Cell<u64>>,
        exits: Vec<Exit<X86>>,
        reset_on_start: bool,
    }
    impl SubjectFactory for SharedWorkFactory {
        type M = CorpusMachine<MockBackend>;
        fn spawn(&self, seed: u64) -> Self::M {
            let mut backend = MockBackend::with_exits(self.exits.clone());
            backend
                .set_policy(&X86Policy::default())
                .expect("set_policy");
            let mut vmm = Vmm::new(backend, GuestRam::new(0x1000).unwrap());
            vmm.wire_vtime(
                VtimeWiring::new(
                    contract_vclock_config(),
                    Box::new(SharedThreadWork::open(
                        self.thread.clone(),
                        self.reset_on_start,
                    )),
                    seed,
                )
                .unwrap(),
            );
            CorpusMachine::new(vmm)
        }
    }

    /// An entropy-/V-time-consuming script: RDRAND/RDSEED + RDTSC are V-time
    /// intercepts that read the work counter and set the hashed `last_intercept_work`.
    fn vtime_consuming_script() -> Vec<Exit<X86>> {
        vec![
            Exit::Arch(X86Exit::Rdrand { width: 8 }),
            Exit::Arch(X86Exit::Rdtsc),
            Exit::Arch(X86Exit::Rdseed { width: 8 }),
            Exit::Arch(X86Exit::Rdtsc),
            Exit::Common(CommonExit::Idle),
        ]
    }

    #[test]
    fn coexisting_spawns_are_determinism_identical_despite_a_shared_work_counter() {
        // WITH the run-start reset (`Vmm::run` → `WorkSource::start_run`): two
        // coexisting CorpusMachines sharing the thread work counter are
        // determinism-identical under the real `compare_runs` (spawn both, run both).
        let f = SharedWorkFactory {
            thread: Rc::new(Cell::new(0u64)),
            exits: vtime_consuming_script(),
            reset_on_start: true,
        };
        let report = compare_runs(&f, &f, 0x0028_C0FF_EE5E_EDC0, 4096, 1_000_000).unwrap();
        assert_eq!(
            report.verdict,
            Verdict::Identical,
            "coexisting VMs must be determinism-identical once run() re-baselines the \
             shared work counter at run-start: {report:?}"
        );
    }

    #[test]
    fn shared_work_counter_without_run_start_reset_diverges() {
        // Non-vacuity guard: WITHOUT the run-start reset (the pre-fix counter), the
        // SAME setup diverges — the second run's counter carries the first's branches.
        // This proves the positive test above actually exercises the contamination.
        let g = SharedWorkFactory {
            thread: Rc::new(Cell::new(0u64)),
            exits: vtime_consuming_script(),
            reset_on_start: false,
        };
        let report = compare_runs(&g, &g, 0x0028_C0FF_EE5E_EDC0, 4096, 1_000_000).unwrap();
        assert!(
            matches!(report.verdict, Verdict::Diverged { .. }),
            "without a run-start reset the shared counter must contaminate the second \
             run (else the regression test is vacuous): {report:?}"
        );
    }
}
