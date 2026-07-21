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
//!    VMM-MEASURED preemption landings are IDENTICAL across seeds, while the seeded-entropy
//!    state keys the hash. That is the proof that busy-waiting guest code is now
//!    deterministically tolerable.
//!
//!  - **`preemption_instant_is_a_pure_function_of_the_seed`** runs the seed-consuming
//!    **`irq-landing-rng`**, whose deadlines are derived from seeded RDRAND draws, so the
//!    preemption *instant* is a pure function of the RNG seed: bit-identical twice at one
//!    seed, yet **seed-DEPENDENT** across seeds — `run_until` preempts at DIFFERENT
//!    MEASURED retired-branch counts. This is the direction the pure payload cannot
//!    exercise. Both gates assert on the VMM-MEASURED **preemption landings**
//!    (`vmm.preemption_landings()` — the work where `run_until` actually delivered each
//!    timer), NOT the guest's self-reported ICR (which differs by seed for any backend,
//!    since the RDRAND inputs differ) and NOT the seed-invariant START/OK/PASS banner.
//!
//! Needs the **LOADED patched KVM** (preemption is gated on `deterministic_tsc`),
//! `perf_event`, the `det-cfl-v1` host, and the built payload. Run on the box,
//! CPU-pinned **core 2** (PR #12 owns core 4), bounded by an on-box `timeout`, then
//! revert KVM to stock:
//!
//! ```sh
//! cd consonance/acceptance-suite/payloads && cargo build --release && cd ../..
//! # load patched KVM per consonance/vmm-backend/kvm-patches/BUILD.md, then:
//! taskset -c 2 timeout 150 cargo test -p vmm-core --test live_preemption \
//!     -- --ignored --nocapture --test-threads=1
//! # then ALWAYS: rmmod kvm_intel kvm; modprobe kvm_intel; lsmod | grep '^kvm ' (== 1396736)
//! ```
//!
//! Fail-fast, never skip: a missing `/dev/kvm`, an unbuilt payload, or a non-patched
//! backend is a loud panic. macOS builds an empty test binary.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::path::PathBuf;

use lapic::{Lapic, LapicConfig};
use vmm_core::vendor::x86::bringup::{BackendKind, boot_selected};
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
        .join("consonance/acceptance-suite/payloads/target/x86_64-unknown-none/release")
        .join(name);
    std::fs::read(&p).unwrap_or_else(|e| {
        panic!(
            "{name} payload not built ({e}) at {} — build it on the box first: \
             `cd consonance/acceptance-suite/payloads && cargo build --release` (target x86_64-unknown-none).",
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

/// What a gate run observes.
///
/// `landings` is the VMM-MEASURED preemption work (`vmm.preemption_landings()`): the
/// retired-branch count at which `run_until` actually delivered each LAPIC timer
/// (`CommonExit::Deadline { reached }`). This is the **load-bearing** seed signal (P2 round-13):
/// it is what the backend measured, NOT the ICR the guest programmed — a backend that
/// ignored the deadline but still delivered IRQs would have seed-varying *reports* anyway
/// (the RDRAND inputs differ), so only the measured LANDING work proves seed-dependent
/// *preemption*.
///
/// `reports` is the guest's report stream — the ICR values the payload PROGRAMMED — kept
/// for context only (the guest's self-report, not the measured preemption).
///
/// Neither is part of `state_hash` (the seeded-entropy state is), so the seed legs assert on
/// `landings`, not the entropy-laden hash.
struct Run {
    state_hash: [u8; 32],
    landings: Vec<u64>,
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
        landings: vmm.preemption_landings().to_vec(),
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
    // `landings` is the VMM-MEASURED preemption work (one `reached` per timer firing):
    // the eight armed deadlines deliver eight preemptions. Non-empty so the seed
    // comparison below is not a vacuous empty-vs-empty.
    assert_eq!(
        a1.landings.len(),
        8,
        "irq-landing's 8 armed timers must produce 8 MEASURED preemption landings; got {:?}",
        a1.landings
    );
    eprintln!(
        "[gate2] seed A: irq-landing PASS — busy-spin preempted, all 8 timer deadlines landed.\n\
         [gate2]   state_hash = {}\n[gate2]   landings (measured) = {:?}\n[gate2]   reports (programmed ICRs) = {:?}",
        hex32(&a1.state_hash),
        a1.landings,
        a1.reports,
    );

    // --- Deterministic twice: a second run at the SAME seed is bit-identical (landings +
    // serial + state_hash). The preemption instant is a pure function of the seed, so the
    // interleaving — and thus all observable state — repeats exactly. ---
    let a2 = run_irq_landing(SEED_A);
    assert_eq!(a2.reason, TerminalReason::DebugExit { code: 0 });
    assert_eq!(
        a1.landings, a2.landings,
        "deterministic-twice: same-seed MEASURED preemption landings must be bit-identical"
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

    // --- Seed-PURITY of the preemption primitive. `irq-landing` is O3:**pure** — it
    // consumes NO RNG, its deadlines are FIXED, so its preemption instants are
    // seed-INVARIANT by construction (the seed-DEPENDENT direction is the separate
    // `preemption_instant_is_a_pure_function_of_the_seed` gate, on `irq-landing-rng`). What
    // a different seed controls HERE splits into two honestly-labelled halves:
    //  (1) `landings` are IDENTICAL — the MEASURED preemption work (where `run_until`
    //      actually delivered each timer) is seed-independent. This is the load-bearing
    //      check: it would FAIL if `run_until` leaked the seed into a deadline, pinning the
    //      primitive seed-pure for a pure guest.
    //  (2) `state_hash` DIFFERS — the seed keys the VM's seeded-ENTROPY stream, part of the
    //      hashed state (the landings are NOT). This proves the seed plumbs THROUGH to the
    //      VM; it is an ENTROPY signal, **not** a preemption one. ---
    let b = run_irq_landing(SEED_B);
    assert_eq!(
        b.reason,
        TerminalReason::DebugExit { code: 0 },
        "irq-landing must also reach a CLEAN PASS at a different seed"
    );
    assert_eq!(
        b.landings, a1.landings,
        "seed-purity of the preemption primitive: the pure payload's MEASURED preemption \
         landings must be IDENTICAL across seeds — a difference would mean `run_until` leaked \
         the RNG seed into its preemption work. seed A = {:?}, seed B = {:?}",
        a1.landings, b.landings
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
        "[gate2] seed B {SEED_B:#018x}: PASS, landings == seed A (MEASURED preemption work is \
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
    // The load-bearing signal is `landings` — the VMM-MEASURED preemption work (the
    // retired-branch count at which `run_until` ACTUALLY delivered each timer), NOT the ICR
    // the guest reported. P2 round-13: comparing the guest's reported ICRs would be
    // vacuous — a backend that IGNORED the deadline but still delivered IRQs would have
    // seed-varying reports anyway (the RDRAND inputs differ), so only the MEASURED landing
    // work proves seed-dependent *preemption*. We assert:
    //  (1) deterministic-twice — same seed ⇒ bit-identical landings AND state_hash (the
    //      seed-derived deadlines are a *pure function* of the seed, so they repeat); and
    //  (2) seed-DEPENDENT preemption — a different seed ⇒ DIFFERENT measured landings, i.e.
    //      `run_until` preempted at DIFFERENT retired-branch counts. A `run_until` that
    //      ignored the seed for preemption would land at the SAME work here and FAIL this
    //      leg — the non-vacuous check the reviewer asked for.

    // (1) deterministic-twice at seed A.
    let a1 = run_irq_landing_rng(SEED_A);
    assert_eq!(
        a1.reason,
        TerminalReason::DebugExit { code: 0 },
        "irq-landing-rng must reach a CLEAN PASS — every seed-derived LAPIC deadline landed \
         mid-spin via run_until preemption. serial:\n{}",
        String::from_utf8_lossy(&a1.serial)
    );
    // ROUNDS = 4 seed-derived timers → 4 MEASURED landings. Non-empty so the comparison is
    // not vacuous.
    assert_eq!(
        a1.landings.len(),
        4,
        "irq-landing-rng's 4 seed-derived timers must produce 4 MEASURED preemption \
         landings; got {:?}",
        a1.landings
    );
    let a2 = run_irq_landing_rng(SEED_A);
    assert_eq!(a2.reason, TerminalReason::DebugExit { code: 0 });
    assert_eq!(
        a1.landings, a2.landings,
        "deterministic-twice: the MEASURED preemption landings must be bit-identical at a \
         fixed seed — they are a pure function of the seed"
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
         landings {:?} repeat, state_hash {} == {}",
        a1.landings,
        hex32(&a1.state_hash),
        hex32(&a2.state_hash)
    );

    // (2) seed-DEPENDENT preemption: a different seed ⇒ DIFFERENT MEASURED landings.
    let b = run_irq_landing_rng(SEED_B);
    assert_eq!(
        b.reason,
        TerminalReason::DebugExit { code: 0 },
        "irq-landing-rng must also PASS at a different seed (every seed-derived deadline \
         still lands via preemption). serial:\n{}",
        String::from_utf8_lossy(&b.serial)
    );
    assert_ne!(
        b.landings, a1.landings,
        "seed-DEPENDENT preemption: a different seed must make `run_until` preempt at \
         DIFFERENT MEASURED retired-branch counts (the IRQ-landing work, from the VMM/backend \
         — NOT the guest's self-reported ICR). Identical landings would mean `run_until` \
         ignores the seed for preemption.\n\
         seed A landings (measured): {:?}\nseed B landings (measured): {:?}",
        a1.landings, b.landings
    );
    assert_ne!(
        b.state_hash,
        a1.state_hash,
        "a different seed must also yield a different state_hash (seed A = {}, seed B = {})",
        hex32(&a1.state_hash),
        hex32(&b.state_hash)
    );
    eprintln!(
        "[gate2] irq-landing-rng seed B {SEED_B:#018x}: PASS, MEASURED landings {:?} != seed A \
         {:?} (run_until preempted at DIFFERENT retired-branch counts — seed-dependent \
         preemption); state_hash = {}",
        b.landings,
        a1.landings,
        hex32(&b.state_hash)
    );
}
