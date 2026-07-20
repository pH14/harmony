// SPDX-License-Identifier: AGPL-3.0-or-later
//! Box-only corpus gate (`#[cfg(target_os = "linux")]` **and `#[ignore]`d**): run
//! the C1 conformance corpus on the **patched** backend as a `acceptance-suite`
//! `Subject` — the proof point the whole corpus box-integration (task 28) exists
//! for. For every **conformance** item in `docs/corpus-manifest.toml` it drives the
//! VMM-backed [`vmm_core::corpus::CorpusMachine`] and asserts:
//!
//! - **O1 (determinism):** `acceptance_suite::check_determinism` — two runs at one seed
//!   produce a bit-identical `state_hash` (localized on failure by the bisector);
//! - **O2 (conformance):** the run's `observable_digest` (the report-stream + serial
//!   digest) equals the committed 64-hex golden at `consonance/acceptance-suite/golden/<name>.digest`;
//! - and the whole sweep is **deterministic twice** — an aggregate digest over
//!   every item's (O1 pass, O2 digest) is identical across two back-to-back sweeps.
//!
//! The six conformance items are the C1 payloads that run to a clean
//! isa-debug-exit PASS on vmm-core's current event loop (trapped instructions /
//! MSR dispositions / in-guest faults). Four payloads are **O2-deferred** — they
//! can't reach a clean PASS today: insn-hlt, irq-landing, pit-pic-stub need
//! PIT/LAPIC-timer interrupt injection + LAPIC MMIO + idle-skip (a later vmm-core
//! phase, the "LAPIC timer interrupt landing" hard core), and insn-mwait exits
//! `DebugExit { code: 1 }` (MONITOR/MWAIT are unmodeled on the event loop). They
//! are logged and skipped, never run through the gate.
//!
//! [`c1_corpus_o1_diagnostic`] is the companion localizer: for each conformance
//! item it runs two fresh same-seed VMs and prints `Vmm::state_hash` and
//! `observable_digest` **separately** per run, so an O1 failure is pinned to
//! architectural state vs. the report channel. On an **architectural** divergence
//! (`state_hash` differs) it dumps the `Vmm::state_components` per-component
//! breakdown and prints **which** field group diverges (a RAM region, an XSAVE
//! sub-area, MSRs, segments, …); on a **report** divergence it dumps the
//! report-stream deltas (start-offset vs. per-read jitter). The first box capture
//! (PR #51) showed the report channel is clean (`observable_digest` MATCH for all
//! six) while `state_hash` diverges **intermittently** — a host-dependent value
//! leaking into the saved architectural state; this breakdown localizes it.
//!
//! Box-only because it needs the LOADED patched `/dev/kvm`
//! (`KVM_CAP_X86_DETERMINISTIC_INTERCEPTS`), `perf_event`, and the `det-cfl-v1`
//! host; `#[ignore]`d out of the default lane (like `live_determinism.rs`) so a
//! plain `cargo nextest` shows it **not-run**, never a vacuous green. Run on the
//! box (patched modules loaded, then reverted to stock afterwards), CPU-pinned:
//!
//! ```sh
//! cd consonance/acceptance-suite/payloads && cargo build --release          # build the C1 payloads
//! cd ../..
//! taskset -c 2 cargo test -p vmm-core --test box_corpus -- --ignored --nocapture
//! ```
//!
//! **Blessing the O2 goldens (one-time, on the box).** The report-stream digests
//! are V-time/seeded-PRNG-derived, so they can only be captured on the patched
//! box. Capture/refresh them with `DETCORPUS_BLESS=1` (writes each
//! `consonance/acceptance-suite/golden/<name>.digest`), review the diff, commit, then run the gate
//! (without the env var) to verify:
//!
//! ```sh
//! DETCORPUS_BLESS=1 taskset -c 2 cargo test -p vmm-core --test box_corpus \
//!     -- --ignored --nocapture
//! ```
//!
//! Every precondition that would prevent a real run — no `/dev/kvm`, the
//! determinism cap absent (stock modules), a non-baseline host, an unbuilt
//! payload, or an unblessed golden — is a **loud panic (test FAILURE)**, never an
//! early-return `Ok`. macOS builds an empty test binary; the bridge logic is
//! covered there by the `MockBackend` unit tests in `src/corpus.rs`.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::path::PathBuf;

use acceptance_suite::{CorpusItem, Oracle, check_determinism, load_manifest};
use sha2::{Digest, Sha256};
use unison::{RunOutcome, Subject, SubjectFactory};
use vmm_core::corpus::CorpusMachine;
use vmm_core::vendor::x86::bringup::boot_patched_corpus;
use vmm_core::vmm::TerminalReason;

/// 256 MiB of guest RAM — the size the C1 payloads were validated under (the
/// task-04 QEMU `-m 256` gate and the live M1/M2 gate).
const GUEST_RAM_LEN: usize = 256 << 20;
/// The pinned corpus seed: O1's replay seed and the seed the O2 goldens are
/// captured at (the seeded entropy stream `insn-rng` draws from). Fixed so the
/// goldens are reproducible.
const CORPUS_SEED: u64 = 0x0028_C0FF_EE5E_EDC0;
/// O1 checkpoint cadence / work limit. The VMM `Subject` runs each payload to
/// terminal (no intra-run work-targeting yet — see `corpus.rs`), so any cadence
/// ≥ 1 and limit ≥ 1 compares the terminal checkpoint; the defaults match the CLI.
const CHECKPOINT_EVERY: u64 = 4096;
const LIMIT: u64 = 1_000_000;

/// Repo root, from this crate's manifest dir (`consonance/vmm-core`).
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// The built payload ELF for a corpus item
/// (`consonance/acceptance-suite/payloads/target/x86_64-unknown-none/release/<name>`).
fn payload_path(name: &str) -> PathBuf {
    repo_root()
        .join("consonance/acceptance-suite/payloads/target/x86_64-unknown-none/release")
        .join(name)
}

/// The committed O2 golden path for a conformance item, taken from the **manifest's**
/// `golden` field (resolved against the repo root) so the gate and the manifest cannot
/// drift to different files. Panics if a conformance item has no golden — `acceptance-suite
/// validate` forbids that, so it would be a manifest bug.
fn golden_path(item: &CorpusItem) -> PathBuf {
    let rel = item.golden.as_deref().unwrap_or_else(|| {
        panic!(
            "conformance item `{}` has no `golden` in docs/corpus-manifest.toml \
             (acceptance-suite validate should have rejected this)",
            item.name
        )
    });
    repo_root().join(rel)
}

/// A `unison::SubjectFactory` for one payload over the patched backend. `spawn`
/// boots a fresh patched VM at `seed`; a boot failure is a genuine box-setup
/// failure (no patched `/dev/kvm`, non-baseline host) and panics loudly — the
/// same posture as the live M1/M2 `PayloadFactory`.
struct PatchedPayloadFactory {
    name: String,
    payload: Vec<u8>,
}

impl SubjectFactory for PatchedPayloadFactory {
    type M = CorpusMachine<Box<dyn vmm_backend::Backend<A = vmm_backend::X86>>>;
    fn spawn(&self, seed: u64) -> Self::M {
        boot_patched_corpus(&self.payload, GUEST_RAM_LEN, seed).unwrap_or_else(|e| {
            panic!(
                "boot_patched_corpus({}) failed: {e}. Needs the LOADED patched KVM \
                 (KVM_CAP_X86_DETERMINISTIC_INTERCEPTS), perf_event, and the det-cfl-v1 host. \
                 Build + load per consonance/vmm-backend/kvm-patches/BUILD.md, then revert to stock after.",
                self.name
            )
        })
    }
}

/// Require `/dev/kvm`, else **panic (loud FAILURE)** — never an early-return that
/// nextest counts as a vacuous pass.
fn require_box() {
    assert!(
        std::path::Path::new("/dev/kvm").exists(),
        "/dev/kvm absent — run this `#[ignore]`d box gate on the patched determinism box with the \
         patched KVM modules loaded (consonance/vmm-backend/kvm-patches/BUILD.md), CPU-pinned per \
         docs/BOX-PINNING.md."
    );
}

/// Read a built payload, else **panic** with the build command.
fn require_payload(name: &str) -> Vec<u8> {
    std::fs::read(payload_path(name)).unwrap_or_else(|e| {
        panic!(
            "payload `{name}` not built ({e}) — build it first on the box: \
             `cd consonance/acceptance-suite/payloads && cargo build --release` (target x86_64-unknown-none)."
        )
    })
}

/// Lowercase 64-char hex of a digest (the `acceptance-suite` / contract idiom).
fn hex32(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Run one payload to terminal at `CORPUS_SEED` and return its `observable_digest`
/// (hex) — the O2 conformance signal. Panics loudly if the run errors or does not
/// end on a clean isa-debug-exit PASS (a corpus payload that fails to run is a real
/// failure, not a silent skip).
fn run_observable_digest(factory: &PatchedPayloadFactory) -> String {
    let mut m = factory.spawn(CORPUS_SEED);
    assert_eq!(
        m.run_to(LIMIT).expect("run_to is infallible"),
        RunOutcome::Halted,
        "{}: payload must run to terminal",
        factory.name
    );
    assert!(
        m.run_error().is_none(),
        "{}: payload run errored: {}",
        factory.name,
        m.run_error().unwrap_or("")
    );
    assert_eq!(
        m.vmm().terminal_reason(),
        Some(TerminalReason::DebugExit { code: 0 }),
        "{}: payload must end on a clean isa-debug-exit PASS",
        factory.name
    );
    hex32(&m.observable_digest())
}

/// One item's verdict for the aggregate determinism check.
struct ItemVerdict {
    name: String,
    o1_pass: bool,
    o2_digest: String,
}

/// Run O1 + O2 for every **conformance** manifest item once, printing a per-item
/// line. Returns the verdicts (for the deterministic-twice aggregate). `bless`
/// writes each item's O2 digest to its golden file instead of comparing (one-time
/// capture on the box).
///
/// Items that do NOT declare `conformance` (the timer/IRQ payloads — insn-hlt,
/// irq-landing, pit-pic-stub) are **O2-deferred**: they depend on PIT/LAPIC-timer
/// interrupt injection + LAPIC MMIO + the idle-skip protocol, which vmm-core does
/// not model yet (a later phase), so they cannot reach a clean PASS on this event
/// loop. They are logged and skipped — never run through the gate (which would
/// hit a `TerminalReason::Idle` / MMIO `ContractViolation`).
fn sweep(items: &[CorpusItem], bless: bool) -> Vec<ItemVerdict> {
    let mut verdicts = Vec::new();
    for item in items {
        let declares_o2 = item
            .oracles
            .iter()
            .any(|o| matches!(o, Oracle::Conformance));
        if !declares_o2 {
            eprintln!(
                "[box-corpus] {:<14} O2-deferred (timer/IRQ payload — needs vmm-core LAPIC/PIT/IRQ \
                 modeling, a later phase); not box-gated",
                item.name
            );
            continue;
        }

        let factory = PatchedPayloadFactory {
            name: item.name.clone(),
            payload: require_payload(&item.name),
        };

        // O1 — determinism: two runs at one seed, bit-identical state_hash.
        let o1 = check_determinism(&factory, CORPUS_SEED, CHECKPOINT_EVERY, LIMIT)
            .expect("check_determinism");

        // O2 — conformance: the observable_digest at CORPUS_SEED.
        let o2_digest = run_observable_digest(&factory);

        let o2_pass = if bless {
            // Never bless a golden from a run that fails O1 — a digest captured
            // from a non-deterministic run is meaningless. Skip + warn loudly so a
            // partial/garbage golden is never committed (PR #51 box-review fix).
            if o1.passed {
                let path = golden_path(item);
                std::fs::write(&path, format!("{o2_digest}\n"))
                    .unwrap_or_else(|e| panic!("write golden {}: {e}", path.display()));
                eprintln!("[box-corpus] blessed {} -> {o2_digest}", item.name);
            } else {
                eprintln!(
                    "[box-corpus] {} O1=FAIL — NOT blessing (a golden from a non-deterministic run \
                     is meaningless; run the c1_corpus_o1_diagnostic test to localize)",
                    item.name
                );
            }
            true
        } else {
            let golden = std::fs::read_to_string(golden_path(item)).unwrap_or_else(|e| {
                panic!(
                    "O2 golden for `{}` missing ({e}) — capture it on the box first with \
                     DETCORPUS_BLESS=1 (see the module docs), review, and commit.",
                    item.name
                )
            });
            let golden = golden.trim();
            assert_eq!(
                golden.len(),
                64,
                "O2 golden for `{}` is not 64 hex chars (got {golden:?}) — it has not been \
                 captured on the box yet; run with DETCORPUS_BLESS=1, review, and commit.",
                item.name
            );
            golden == o2_digest
        };

        eprintln!(
            "[box-corpus] {:<14} O1={} O2={} digest={o2_digest} {}",
            item.name,
            if o1.passed { "PASS" } else { "FAIL" },
            if o2_pass { "PASS" } else { "FAIL" },
            o1.detail,
        );

        verdicts.push(ItemVerdict {
            name: item.name.clone(),
            o1_pass: o1.passed,
            o2_digest,
        });
        if !bless {
            assert!(
                o1.passed,
                "O1 (determinism) FAILED for `{}`: {}",
                item.name, o1.detail
            );
            assert!(
                o2_pass,
                "O2 (conformance) FAILED for `{}` (digest != golden)",
                item.name
            );
        }
    }
    verdicts
}

/// A stable aggregate digest over the whole sweep — `sha256` of every item's
/// (name, O1 pass, O2 digest), so "the two sweeps agree" is one comparison.
fn aggregate(verdicts: &[ItemVerdict]) -> String {
    let mut h = Sha256::new();
    for v in verdicts {
        h.update(v.name.as_bytes());
        h.update([u8::from(v.o1_pass)]);
        h.update(v.o2_digest.as_bytes());
    }
    let out: [u8; 32] = h.finalize().into();
    hex32(&out)
}

#[test]
#[ignore = "box-only: needs the LOADED patched KVM + perf + det-cfl-v1 host + built C1 payloads; \
            run on the box with `-- --ignored --nocapture`"]
fn c1_corpus_o1_o2_on_the_patched_backend() {
    require_box();
    let manifest = std::fs::read_to_string(repo_root().join("docs/corpus-manifest.toml"))
        .expect("read docs/corpus-manifest.toml");
    let items = load_manifest(&manifest).expect("parse corpus manifest");
    assert!(!items.is_empty(), "the corpus manifest must not be empty");

    let bless = std::env::var_os("DETCORPUS_BLESS").is_some();
    if bless {
        eprintln!("[box-corpus] DETCORPUS_BLESS set — capturing O2 goldens (not gating)");
        let _ = sweep(&items, true);
        eprintln!(
            "[box-corpus] goldens written; review `git diff consonance/acceptance-suite/golden/*.digest` and commit"
        );
        return;
    }

    // Two back-to-back sweeps: each gates O1+O2 over the conformance items, and the
    // aggregate over both must match (deterministic twice — the headline property).
    eprintln!("[box-corpus] === sweep 1 ===");
    let run1 = sweep(&items, false);
    eprintln!("[box-corpus] === sweep 2 ===");
    let run2 = sweep(&items, false);
    assert!(
        !run1.is_empty(),
        "no conformance items gated — every manifest item was O2-deferred (check the manifest)"
    );
    let agg1 = aggregate(&run1);
    let agg2 = aggregate(&run2);
    eprintln!("[box-corpus] aggregate sweep 1 = {agg1}");
    eprintln!("[box-corpus] aggregate sweep 2 = {agg2}");
    assert_eq!(
        agg1, agg2,
        "the corpus sweep must be deterministic across two runs (identical aggregate)"
    );
    eprintln!(
        "[box-corpus] PASS: {} conformance item(s) O1+O2 green, deterministic twice; {} \
         item(s) O2-deferred (aggregate {agg1})",
        run1.len(),
        items.len() - run1.len(),
    );
}

/// Box-only **O1 localizer** (`#[ignore]`d): for each conformance item, run two
/// fresh same-seed VMs and print `Vmm::state_hash` and `observable_digest`
/// **separately** for both runs, so an O1 divergence (e.g. `insn-rdtsc`) can be
/// pinned to architectural state vs. the report channel — and, on a report
/// divergence, dump the per-entry deltas to characterize it (a CONSTANT delta is a
/// start-offset in the work count; a VARYING delta is per-read work jitter). Pure
/// diagnostic — it asserts nothing, so it always runs to completion and prints the
/// full picture for every item.
#[test]
#[ignore = "box-only O1 localizer (needs patched KVM + built C1 payloads); run on the box with \
            `-- --ignored --nocapture`"]
fn c1_corpus_o1_diagnostic() {
    require_box();
    let manifest = std::fs::read_to_string(repo_root().join("docs/corpus-manifest.toml"))
        .expect("read docs/corpus-manifest.toml");
    let items = load_manifest(&manifest).expect("parse corpus manifest");

    for item in &items {
        if !item
            .oracles
            .iter()
            .any(|o| matches!(o, Oracle::Conformance))
        {
            continue;
        }
        let factory = PatchedPayloadFactory {
            name: item.name.clone(),
            payload: require_payload(&item.name),
        };
        // Two fresh patched VMs at the same seed — exactly what acceptance-suite O1 does.
        let mut a = factory.spawn(CORPUS_SEED);
        a.run_to(LIMIT).expect("run_to");
        let mut b = factory.spawn(CORPUS_SEED);
        b.run_to(LIMIT).expect("run_to");

        let (sa, sb) = (a.vmm().state_hash(), b.vmm().state_hash());
        let (oa, ob) = (a.vmm().observable_digest(), b.vmm().observable_digest());
        let tag = |eq: bool| if eq { "MATCH  " } else { "DIVERGE" };
        eprintln!(
            "[diag] {:<14} state_hash={} observable_digest={}  terminal A={:?} B={:?}  err A={:?} B={:?}",
            item.name,
            tag(sa == sb),
            tag(oa == ob),
            a.vmm().terminal_reason(),
            b.vmm().terminal_reason(),
            a.run_error(),
            b.run_error(),
        );
        eprintln!(
            "[diag]   state_hash         A={} B={}",
            hex32(&sa),
            hex32(&sb)
        );
        eprintln!(
            "[diag]   observable_digest  A={} B={}",
            hex32(&oa),
            hex32(&ob)
        );

        // Bisect WHICH state_hash component diverges (PR #51 box-review): a named
        // per-component digest breakdown across both runs, printing only the ones
        // that differ. This pins the architectural non-determinism the corpus
        // caught to a specific field group (a RAM region, an XSAVE sub-area, MSRs,
        // segments, …).
        if sa != sb {
            let (ca, cb) = (a.vmm().state_components(), b.vmm().state_components());
            let mut any = false;
            for ((la, da), (_lb, db)) in ca.iter().zip(cb.iter()) {
                if da != db {
                    any = true;
                    eprintln!(
                        "[diag]   component DIVERGE: {la:<16} A={} B={}",
                        hex32(da),
                        hex32(db)
                    );
                }
            }
            if !any {
                eprintln!(
                    "[diag]   (state_hash diverged but every named component matched — a \
                     state_blob field is not broken out; widen state_components)"
                );
            }
        }

        if oa != ob {
            let (ra, rb) = (a.vmm().report_stream(), b.vmm().report_stream());
            eprintln!(
                "[diag]   report streams differ (len A={} B={}):",
                ra.len(),
                rb.len()
            );
            let mut first: Option<usize> = None;
            let (mut diffs, mut delta0, mut constant_delta) = (0usize, 0i64, true);
            for (i, (x, y)) in ra.iter().zip(rb.iter()).enumerate() {
                if x != y {
                    let d = i64::from(*y) - i64::from(*x);
                    match first {
                        None => {
                            first = Some(i);
                            delta0 = d;
                        }
                        Some(_) if d != delta0 => constant_delta = false,
                        _ => {}
                    }
                    diffs += 1;
                    if diffs <= 16 {
                        eprintln!("[diag]     report[{i:>3}]: A={x:#010x} B={y:#010x}  Δ={d}");
                    }
                }
            }
            eprintln!(
                "[diag]   => {diffs} diverging entr(ies); first at index {first:?}; Δ-pattern: {} \
                 (CONSTANT ⇒ a start-offset in the work count; VARYING ⇒ per-read work jitter)",
                if constant_delta {
                    "CONSTANT"
                } else {
                    "VARYING"
                },
            );
        }
    }
    eprintln!(
        "[diag] done. state_hash DIVERGE ⇒ architectural divergence (the `component DIVERGE` lines \
         say which field group: vtim:eff-vns ⇒ the hashed effective V-time; xsave-* ⇒ FPU host-leak; \
         RAM:* ⇒ guest scratch; vtim:work-raw / vtim:last-intercept are diagnostic-only and NOT in \
         the hash); observable_digest DIVERGE only ⇒ report-stream (V-time TSC) divergence. Re-run \
         a few times — the divergence is intermittent (see c1_corpus_o1_repeat_diagnostic for the \
         N-run localizer on insn-rdtsc / insn-rng)."
    );
}

/// Box-only **N-run O1 localizer for the two payloads PR #51 still fails**
/// (`#[ignore]`d, non-asserting). After task 27 (#53) made `Vmm::state_hash`'s `VTIM`
/// chunk skid-free, `insn-rdtsc` / `insn-rng` still report O1=FAIL with the telling
/// `acceptance-suite` detail "diverged in (0, 1] but bisection could not localize it: state
/// hashes are equal at hi = 1: no divergence to bisect". That message is dispositive:
/// `check_determinism` → `compare_runs` reached the `(Halted, Halted)` branch with
/// **equal `work()`** (else it would be a `HaltMismatch`, not "diverged"), so the
/// divergence signal is the terminal `CorpusMachine::state_hash` — and the bisector's
/// *next* re-spawn found it equal, i.e. the inequality is **intermittent**, not the
/// deterministic work counter. The four passing items never hit a V-time intercept (so
/// their `VTIM` is trivially constant); only these two advance `last_intercept_work` /
/// the entropy stream, which is why the residual shows up only here.
///
/// This test repeats the exact O1 comparison `N` times for **just those two payloads**
/// and, per run, logs every quantity the oracle uses, so the box operator can read off
/// which case holds:
///
/// - `CorpusMachine::state_hash` is **always MATCH** across all `N` direct pairs yet
///   `check_determinism` still FAILs ⇒ the failure is in the oracle's *detection* (a
///   flake the single-shot diagnostic missed but `compare_runs`' separate spawns hit) —
///   make the O1 comparison skid-robust; or
/// - it is **intermittently DIVERGE** ⇒ residual run-to-run non-determinism, and the
///   per-component histogram names the culprit (`vtim:eff-vns` ⇒ the V-time anchor
///   `last_intercept_work`; `xsave-*` ⇒ an FPU init/host-leak; a `RAM:*` region ⇒ guest
///   scratch). `vtim:work-raw` / `vtim:last-intercept` are diagnostic-only (NOT hashed);
///   `vtim:work-raw` DIVERGE *without* a hashed component is the post-intercept skid #53
///   excludes by design — not a failure.
///
/// Per run it prints both machines' run outcome + `work()` (the work/run_to comparison
/// `compare_runs` makes first), both `CorpusMachine::state_hash` values (the exact
/// terminal-checkpoint quantity), the underlying `Vmm::state_hash` + `observable_digest`,
/// and — when the architectural hash diverges — the diverging `state_components`. It
/// also runs the real `acceptance_suite::check_determinism` each iteration (its own spawns +
/// bisect) and logs the verdict next to the raw values. A final SUMMARY gives the
/// divergence counts and a per-component histogram.
#[test]
#[ignore = "box-only N-run O1 localizer for insn-rdtsc / insn-rng (needs patched KVM + built C1 \
            payloads); run on the box with `-- --ignored --nocapture`"]
fn c1_corpus_o1_repeat_diagnostic() {
    use std::collections::BTreeMap;

    require_box();
    // How many O1 comparisons to run per payload — enough samples to surface an
    // intermittent inequality without an unreasonable number of full VM boots.
    const N: usize = 20;
    let targets = ["insn-rdtsc", "insn-rng"];

    for name in targets {
        let factory = PatchedPayloadFactory {
            name: name.to_string(),
            payload: require_payload(name),
        };
        eprintln!("[repeat] ===== {name}: {N} O1 comparisons at seed {CORPUS_SEED:#018x} =====");

        let (mut machine_diverged, mut vmm_diverged, mut obs_diverged) = (0usize, 0usize, 0usize);
        let (mut work_mismatch, mut oracle_failed) = (0usize, 0usize);
        // Component label -> times it diverged (BTreeMap: deterministic, ordered output).
        let mut comp_hist: BTreeMap<String, usize> = BTreeMap::new();

        for i in 0..N {
            // A direct same-seed pair — exactly compare_runs' (Halted, Halted) inputs.
            let mut a = factory.spawn(CORPUS_SEED);
            let oa = a.run_to(LIMIT).expect("run_to A");
            let mut b = factory.spawn(CORPUS_SEED);
            let ob = b.run_to(LIMIT).expect("run_to B");

            let (wa, wb) = (a.work(), b.work());
            // The EXACT quantity compare_runs compares at the terminal checkpoint.
            let (mha, mhb) = (a.state_hash(), b.state_hash());
            let (sva, svb) = (a.vmm().state_hash(), b.vmm().state_hash());
            let (ova, ovb) = (a.vmm().observable_digest(), b.vmm().observable_digest());

            let (work_eq, machine_eq) = (wa == wb, mha == mhb);
            let (vmm_eq, obs_eq) = (sva == svb, ova == ovb);
            work_mismatch += usize::from(!work_eq);
            machine_diverged += usize::from(!machine_eq);
            vmm_diverged += usize::from(!vmm_eq);
            obs_diverged += usize::from(!obs_eq);

            let tag = |eq: bool| if eq { "MATCH  " } else { "DIVERGE" };
            eprintln!(
                "[repeat] {name} run {i:>2}: outcome A={oa:?} B={ob:?}  work A={wa} B={wb} [{}]  \
                 machine.state_hash=[{}]  vmm.state_hash=[{}]  observable_digest=[{}]",
                tag(work_eq),
                tag(machine_eq),
                tag(vmm_eq),
                tag(obs_eq),
            );
            eprintln!(
                "[repeat]   machine.state_hash A={} B={}",
                hex32(&mha),
                hex32(&mhb)
            );
            if !vmm_eq {
                let (ca, cb) = (a.vmm().state_components(), b.vmm().state_components());
                for ((la, da), (_lb, db)) in ca.iter().zip(cb.iter()) {
                    if da != db {
                        *comp_hist.entry((*la).to_string()).or_default() += 1;
                        eprintln!(
                            "[repeat]   component DIVERGE: {la:<18} A={} B={}",
                            hex32(da),
                            hex32(db)
                        );
                    }
                }
            }

            // The real oracle path (its own spawns + bisect) for cross-confirmation —
            // this is what the box gate actually runs; logging it next to the direct
            // pair correlates "the pair diverged" with "the oracle declared FAIL".
            let o1 = check_determinism(&factory, CORPUS_SEED, CHECKPOINT_EVERY, LIMIT)
                .expect("check_determinism");
            oracle_failed += usize::from(!o1.passed);
            eprintln!(
                "[repeat]   check_determinism: {} — {}",
                if o1.passed { "PASS" } else { "FAIL" },
                o1.detail
            );
        }

        eprintln!(
            "[repeat] {name} SUMMARY over {N}: machine.state_hash DIVERGE {machine_diverged}, \
             vmm.state_hash DIVERGE {vmm_diverged}, observable_digest DIVERGE {obs_diverged}, \
             work mismatch {work_mismatch}, check_determinism FAIL {oracle_failed}"
        );
        if comp_hist.is_empty() {
            eprintln!(
                "[repeat] {name}: NO architectural component ever diverged across {N} direct pairs \
                 (if check_determinism still FAILed, the failure is in the oracle's detection, not \
                 the terminal state_hash)"
            );
        } else {
            eprintln!("[repeat] {name}: component divergence histogram (out of {N}):");
            for (label, count) in &comp_hist {
                eprintln!("[repeat]     {label:<18} {count}");
            }
        }
    }
    eprintln!(
        "[repeat] done. READ-OFF: machine.state_hash DIVERGE == 0 but check_determinism FAIL > 0 \
         ⇒ ORACLE DETECTION issue (make the O1 comparison skid-robust). machine.state_hash DIVERGE \
         > 0 ⇒ RESIDUAL NON-DETERMINISM — the histogram names the hashed component (vtim:eff-vns ⇒ \
         V-time anchor; xsave-* ⇒ FPU host-leak; RAM:* ⇒ guest scratch). vtim:work-raw / \
         vtim:last-intercept are diagnostic-only (NOT in the hash)."
    );
}
