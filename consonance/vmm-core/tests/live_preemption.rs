// SPDX-License-Identifier: AGPL-3.0-or-later
//! Box-only **gate 2** for task 47 (`#[cfg(target_os = "linux")]` + `#[ignore]`):
//! a **busy-spinning** guest that takes no natural VM-exit is preempted at the
//! V-time LAPIC-timer deadline, the timer vector is injected, the guest's ISR runs
//! and it makes progress — **deterministic twice**.
//!
//! Two payloads pin **both directions** of the preemption contract:
//!
//!  - **`busy_spin_guest_is_preempted_and_timer_lands_deterministic_twice`** runs the
//!    existing **`irq-landing`** C1 corpus item (deferred in `box_corpus.rs`: "needs
//!    LAPIC-timer interrupt injection … the 'LAPIC timer interrupt landing' hard core" —
//!    exactly the primitive task 47 delivers). It arms a one-shot LAPIC timer in V-time,
//!    then `pause`-spins (only conditional-branch work events, **no** IO/MMIO/HLT exit)
//!    until the interrupt lands, for eight FIXED deadlines bracketing `skid_margin`.
//!    Under `KVM_IRQCHIP_NONE` + the userspace xAPIC the timer can only fire when the VMM
//!    injects it at a boundary — and a non-exiting spin reaches none, so **without
//!    preemption the FAILSAFE trips** (`payload::fail` → `DebugExit { code: 1 }`); with
//!    `run_until` the timer lands mid-spin and all eight deadlines report → a clean
//!    `DebugExit { code: 0 }`. That clean PASS is **bit-identical on a re-run at the same
//!    seed**; because its deadlines are fixed, its preemption is seed-INVARIANT — the eight
//!    reported deadlines (the report stream = the preemption instants) are IDENTICAL across
//!    seeds, while the seeded-entropy state keys the hash. That is the proof that
//!    busy-waiting guest code is now deterministically tolerable.
//!
//!  - **`preemption_instant_is_a_pure_function_of_the_seed`** runs the seed-consuming
//!    **`irq-landing-rng`**, whose deadlines are derived from seeded RDRAND draws, so the
//!    preemption *instant* is a pure function of the RNG seed: bit-identical twice at one
//!    seed, yet **seed-DEPENDENT** across seeds — the reported deadlines (the preemption
//!    branch counts) genuinely DIFFER. This is the direction the pure payload cannot
//!    exercise. Both gates assert on the **report stream** (the deadlines), not the
//!    START/OK/PASS serial banner, which is seed-invariant for both payloads.
//!
//! Needs the **LOADED patched KVM** (preemption is gated on `deterministic_tsc`),
//! `perf_event`, the `det-cfl-v1` host, and the built payload. Run on the box,
//! CPU-pinned **core 2** (PR #12 owns core 4), bounded by an on-box `timeout`, then
//! revert KVM to stock:
//!
//! ```sh
//! cd guest/payloads && cargo build --release && cd ../..
//! # load patched KVM per consonance/vmm-backend/kvm-patches/BUILD.md, then:
//! taskset -c 2 timeout 150 cargo test -p vmm-core --test live_preemption \
//!     -- --ignored --nocapture --test-threads=1
//! # then ALWAYS: rmmod kvm_intel kvm; modprobe kvm_intel; lsmod | grep '^kvm ' (== 1396736)
//! ```
//!
//! Fail-fast, never skip: a missing `/dev/kvm`, an unbuilt payload, or a non-patched
//! backend is a loud panic. macOS builds an empty test binary.
#![cfg(target_os = "linux")]

use std::path::PathBuf;

use lapic::{Lapic, LapicConfig};
use vmm_core::bringup::{BackendKind, boot_selected};
use vmm_core::vmm::TerminalReason;

/// Two seeds. The preemption *instant* is a pure, deterministic function of the seed,
/// so a single seed must be bit-identical twice. The two seeds drive both directions of
/// the contract: on the pure `irq-landing` (fixed deadlines) the preemption is
/// seed-INVARIANT (identical reported deadlines across seeds); on the seed-consuming
/// `irq-landing-rng` (RDRAND-derived deadlines) it is seed-DEPENDENT (different reported
/// deadlines across seeds).
const SEED_A: u64 = 0x5EED_D31E_2026;
const SEED_B: u64 = 0x0BAD_C0DE_1234;
/// 256 MiB — the size the C1 payloads (incl. the `common` boot shim's long-mode
/// page tables) were validated under.
const GUEST_RAM_LEN: usize = 256 << 20;
/// The LAPIC config the Linux boot uses (`bringup.rs`).
const LAPIC_TIMER_HZ: u64 = 24_000_000;
const BSP_APIC_ID: u32 = 0;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// The built payload ELF for `name`, or a loud panic with the build command.
fn payload_elf(name: &str) -> Vec<u8> {
    let p = repo_root()
        .join("guest/payloads/target/x86_64-unknown-none/release")
        .join(name);
    std::fs::read(&p).unwrap_or_else(|e| {
        panic!(
            "{name} payload not built ({e}) at {} — build it on the box first: \
             `cd guest/payloads && cargo build --release` (target x86_64-unknown-none).",
            p.display()
        )
    })
}

/// Boot `irq-landing` on the **patched** backend with the userspace xAPIC wired,
/// run to terminal, and return (state_hash, serial, terminal reason). Panics loudly
/// if the box is not ready (the same posture as `live_determinism.rs`).
fn run_irq_landing(seed: u64) -> Run {
    run_payload("irq-landing", seed)
}

/// As [`run_irq_landing`], for the seed-consuming `irq-landing-rng` payload (its
/// deadlines are derived from seeded RDRAND draws → seed-dependent preemption).
fn run_irq_landing_rng(seed: u64) -> Run {
    run_payload("irq-landing-rng", seed)
}

/// What a gate run observes. `reports` is the ordered report stream — for these
/// payloads the **armed LAPIC deadlines** (each `report(u64)` = a `[lo, hi]` dword
/// pair; the deadlines are < 2³² so `hi == 0`). The deadline *is* the preemption
/// instant (the IRQ lands ~`deadline` branches after arming), so `reports` is the
/// directly-observable **preemption branch counts** — and it is NOT part of
/// `state_hash` (the seeded-entropy state is), which is exactly why the seed legs
/// assert on `reports`, not the entropy-laden hash.
struct Run {
    state_hash: [u8; 32],
    reports: Vec<u32>,
    serial: Vec<u8>,
    reason: TerminalReason,
}

/// Shared boot-and-run for the LAPIC-preemption gate payloads.
fn run_payload(name: &str, seed: u64) -> Run {
    assert!(
        std::path::Path::new("/dev/kvm").exists(),
        "/dev/kvm absent — run this `#[ignore]`d gate on the box with the LOADED patched KVM \
         (KVM_CAP_X86_DETERMINISTIC_INTERCEPTS) + perf, CPU-pinned core 2 (see the file header)."
    );
    let payload = payload_elf(name);
    // `boot_selected(Patched)` installs the determinism path (V-time + seeded RNG);
    // `wire_lapic` adds the userspace xAPIC so the timer arms and drives preemption.
    let mut vmm = boot_selected(BackendKind::Patched, &payload, GUEST_RAM_LEN, seed).unwrap_or_else(
        |e| {
            panic!(
                "boot_selected(Patched) failed: {e}. Needs the LOADED patched KVM + perf + \
                 det-cfl-v1 host (consonance/vmm-backend/kvm-patches/BUILD.md), then revert to stock."
            )
        },
    );
    let lapic = Lapic::new(LapicConfig {
        apic_id: BSP_APIC_ID,
        timer_hz: LAPIC_TIMER_HZ,
    })
    .expect("lapic init (non-zero timer_hz)");
    vmm.wire_lapic(lapic);

    let r = vmm
        .run()
        .unwrap_or_else(|e| panic!("{name} run to terminal: {e:?}"));
    Run {
        state_hash: vmm.state_hash(),
        reports: vmm.report_stream().to_vec(),
        serial: r.serial,
        reason: r.reason,
    }
}

fn hex32(b: &[u8; 32]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
#[ignore = "box-only gate 2: LOADED patched KVM + perf + built irq-landing; run on the box with \
            `-- --ignored --nocapture` (see header)"]
fn busy_spin_guest_is_preempted_and_timer_lands_deterministic_twice() {
    // --- Run 1 (seed A): the busy-spin must be preempted so every armed timer
    // deadline lands, reaching a CLEAN pass (not the FAILSAFE fail). ---
    let a1 = run_irq_landing(SEED_A);
    assert_eq!(
        a1.reason,
        TerminalReason::DebugExit { code: 0 },
        "irq-landing must reach a CLEAN PASS — every armed LAPIC-timer deadline landed mid-spin \
         via run_until preemption. A `DebugExit {{ code: 1 }}` is the payload's FAILSAFE \
         (\"lapic timer never fired\"): preemption did not deliver the timer. serial:\n{}",
        String::from_utf8_lossy(&a1.serial)
    );
    // The report stream is the eight armed deadlines (each `report(u64)` is a [lo, hi]
    // dword pair, hi == 0): the directly-observable preemption instants. Non-empty so the
    // seed comparison below is not a vacuous empty-vs-empty.
    assert_eq!(
        a1.reports.len(),
        16,
        "irq-landing reports 8 deadlines as 16 dwords; got {:?}",
        a1.reports
    );
    eprintln!(
        "[gate2] seed A: irq-landing PASS — busy-spin preempted, all 8 timer deadlines landed.\n\
         [gate2]   state_hash = {}\n[gate2]   reports = {:?}",
        hex32(&a1.state_hash),
        a1.reports,
    );

    // --- Deterministic twice: a second run at the SAME seed is bit-identical
    // (reports + serial + state_hash). The preemption instant is a pure function of the
    // seed, so the interleaving — and thus all observable state — repeats exactly. ---
    let a2 = run_irq_landing(SEED_A);
    assert_eq!(a2.reason, TerminalReason::DebugExit { code: 0 });
    assert_eq!(
        a1.reports, a2.reports,
        "deterministic-twice: same-seed preemption deadlines (reports) must be bit-identical"
    );
    assert_eq!(
        a1.state_hash,
        a2.state_hash,
        "deterministic-twice: same-seed state_hash must be bit-identical (a={}, b={})",
        hex32(&a1.state_hash),
        hex32(&a2.state_hash)
    );
    assert_eq!(
        a1.serial, a2.serial,
        "deterministic-twice: same-seed serial must be bit-identical"
    );
    eprintln!(
        "[gate2] deterministic-twice CONFIRMED at seed {SEED_A:#018x}: state_hash {} == {}",
        hex32(&a1.state_hash),
        hex32(&a2.state_hash)
    );

    // --- Seed-PURITY of the preemption primitive (P2 round-10). `irq-landing` is
    // O3:**pure** — it consumes NO RNG, and its deadlines are FIXED, so its preemption
    // instants are seed-INVARIANT *by construction*. There is therefore no honest way to
    // assert "preemption branch counts DIFFER across seeds" on THIS payload — they
    // provably do not (that seed-DEPENDENT direction is the separate
    // `preemption_instant_is_a_pure_function_of_the_seed` gate below, which uses the
    // seed-consuming `irq-landing-rng`). What a different seed controls HERE splits into
    // two honestly-labelled halves:
    //  (1) `reports` are IDENTICAL — the actual preemption instants (the eight armed
    //      deadlines) are seed-independent. This is the load-bearing check: it asserts on
    //      the preemption branch counts THEMSELVES (not the START/OK/PASS serial banner),
    //      and would FAIL if `run_until` leaked the seed into a deadline (e.g. one computed
    //      off a seed-keyed clock) — pinning the primitive seed-pure for a pure guest.
    //  (2) `state_hash` DIFFERS — the seed keys the VM's seeded-ENTROPY stream, which is
    //      part of the hashed state (the report stream is NOT). This proves the seed plumbs
    //      THROUGH to the VM; it is an ENTROPY signal, **not** a preemption one. ---
    let b = run_irq_landing(SEED_B);
    assert_eq!(
        b.reason,
        TerminalReason::DebugExit { code: 0 },
        "irq-landing must also reach a CLEAN PASS at a different seed"
    );
    assert_eq!(
        b.reports, a1.reports,
        "seed-purity of the preemption primitive: the pure payload's preemption deadlines \
         (reports) must be IDENTICAL across seeds — a difference would mean `run_until` leaked \
         the RNG seed into its preemption branch counts. seed A = {:?}, seed B = {:?}",
        a1.reports, b.reports
    );
    assert_ne!(
        b.state_hash,
        a1.state_hash,
        "the seed must plumb THROUGH to the VM: a different seed keys the seeded-entropy \
         stream (part of the hashed state), so the state_hash differs — identical hashes \
         would mean the seed is ignored entirely. (Entropy signal, NOT a preemption one.) \
         seed A = {}, seed B = {}",
        hex32(&a1.state_hash),
        hex32(&b.state_hash)
    );
    eprintln!(
        "[gate2] seed B {SEED_B:#018x}: PASS, reports == seed A (preemption deadlines are \
         seed-pure for a pure guest); state_hash = {} != seed A (seed keys the entropy state)",
        hex32(&b.state_hash)
    );
}

#[test]
#[ignore = "box-only gate 2: LOADED patched KVM + perf + built irq-landing-rng; run on the box \
            with `-- --ignored --nocapture` (see header)"]
fn preemption_instant_is_a_pure_function_of_the_seed() {
    // The seed-DEPENDENT preemption gate (P2 round-10). `irq-landing-rng` derives each
    // armed deadline from a seeded RDRAND draw, so the PREEMPTION INSTANT (the work
    // retired before each IRQ lands) is a pure function of the RNG **seed**. This is the
    // direction the pure `irq-landing` gate cannot exercise (its deadlines are fixed →
    // seed-INVARIANT preemption); together the two gates pin BOTH directions of the
    // contract: the primitive never leaks the seed on a pure guest, yet faithfully tracks
    // it when the guest's branch stream genuinely depends on it.
    //
    // The report stream is the sequence of seed-derived deadlines — the directly-observable
    // preemption branch counts (each `report(u64)` = a [lo, hi] dword pair; deadlines are
    // < 2¹⁴+64 so every `hi == 0`). NOTE the serial banner (START/OK/PASS) is seed-INVARIANT
    // for this payload too — the seed shows up ONLY in the reported deadlines — so the
    // assertions are on `reports`, not `serial`. We assert:
    //  (1) deterministic-twice — same seed ⇒ bit-identical reports AND state_hash (the
    //      seed-derived deadlines are a *pure function* of the seed, so they repeat); and
    //  (2) seed-DEPENDENT preemption — a different seed ⇒ DIFFERENT reports, i.e. the
    //      preemption branch counts (the deadlines, hence the IRQ-landing instants) DIFFER
    //      across seeds. A `run_until` that ignored the seed for preemption would produce
    //      identical reports here and FAIL this leg — the non-vacuous check the reviewer
    //      asked for.

    // (1) deterministic-twice at seed A.
    let a1 = run_irq_landing_rng(SEED_A);
    assert_eq!(
        a1.reason,
        TerminalReason::DebugExit { code: 0 },
        "irq-landing-rng must reach a CLEAN PASS — every seed-derived LAPIC deadline landed \
         mid-spin via run_until preemption. serial:\n{}",
        String::from_utf8_lossy(&a1.serial)
    );
    // ROUNDS = 4 deadlines → 8 dwords. Non-empty so the seed comparison is not vacuous.
    assert_eq!(
        a1.reports.len(),
        8,
        "irq-landing-rng reports 4 seed-derived deadlines as 8 dwords; got {:?}",
        a1.reports
    );
    let a2 = run_irq_landing_rng(SEED_A);
    assert_eq!(a2.reason, TerminalReason::DebugExit { code: 0 });
    assert_eq!(
        a1.reports, a2.reports,
        "deterministic-twice: the seed-derived preemption deadlines (reports) must be \
         bit-identical at a fixed seed — they are a pure function of the seed"
    );
    assert_eq!(
        a1.state_hash,
        a2.state_hash,
        "deterministic-twice: same-seed state_hash must be bit-identical (a={}, b={})",
        hex32(&a1.state_hash),
        hex32(&a2.state_hash)
    );
    eprintln!(
        "[gate2] irq-landing-rng deterministic-twice CONFIRMED at seed {SEED_A:#018x}: \
         reports {:?} repeat, state_hash {} == {}",
        a1.reports,
        hex32(&a1.state_hash),
        hex32(&a2.state_hash)
    );

    // (2) seed-DEPENDENT preemption: a different seed ⇒ different deadlines (reports).
    let b = run_irq_landing_rng(SEED_B);
    assert_eq!(
        b.reason,
        TerminalReason::DebugExit { code: 0 },
        "irq-landing-rng must also PASS at a different seed (every seed-derived deadline \
         still lands via preemption). serial:\n{}",
        String::from_utf8_lossy(&b.serial)
    );
    assert_ne!(
        b.reports, a1.reports,
        "seed-DEPENDENT preemption: a different seed must yield DIFFERENT seed-derived \
         deadlines — i.e. DIFFERENT preemption branch counts / IRQ-landing instants. \
         Identical reports would mean `run_until` ignores the seed for preemption.\n\
         seed A deadlines (dwords): {:?}\nseed B deadlines (dwords): {:?}",
        a1.reports, b.reports
    );
    assert_ne!(
        b.state_hash,
        a1.state_hash,
        "a different seed must also yield a different state_hash (seed A = {}, seed B = {})",
        hex32(&a1.state_hash),
        hex32(&b.state_hash)
    );
    eprintln!(
        "[gate2] irq-landing-rng seed B {SEED_B:#018x}: PASS, reports {:?} != seed A {:?} \
         (preemption branch counts DIFFER across seeds — seed-dependent preemption); \
         state_hash = {}",
        b.reports,
        a1.reports,
        hex32(&b.state_hash)
    );
}
