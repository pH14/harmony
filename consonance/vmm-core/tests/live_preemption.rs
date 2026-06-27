// SPDX-License-Identifier: AGPL-3.0-or-later
//! Box-only **gate 2** for task 47 (`#[cfg(target_os = "linux")]` + `#[ignore]`):
//! a **busy-spinning** guest that takes no natural VM-exit is preempted at the
//! V-time LAPIC-timer deadline, the timer vector is injected, the guest's ISR runs
//! and it makes progress — **deterministic twice**.
//!
//! The payload is the existing **`irq-landing`** C1 corpus item, which was
//! explicitly *deferred* (`box_corpus.rs`: "needs LAPIC-timer interrupt injection …
//! a later vmm-core phase, the 'LAPIC timer interrupt landing' hard core") — exactly
//! the primitive task 47 delivers. It arms a one-shot LAPIC timer in V-time, then
//! `pause`-spins (only conditional-branch work events, **no** IO/MMIO/HLT exit)
//! until the interrupt lands, for eight deadlines bracketing `skid_margin = 128`.
//! Under `KVM_IRQCHIP_NONE` + the userspace xAPIC, the timer can only fire when the
//! VMM injects it at a boundary — and a non-exiting spin reaches none, so **without
//! preemption the FAILSAFE trips** (`payload::fail` → `DebugExit { code: 1 }`); with
//! `run_until` the timer lands mid-spin and all eight deadlines report → a clean
//! `DebugExit { code: 0 }`. That clean PASS, **bit-identical on a re-run at the same
//! seed** (and with a seed-independent control flow — same serial across seeds, while
//! the seeded-entropy state keys the hash), is the proof that busy-waiting guest code
//! is now deterministically tolerable.
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

/// Two seeds: the preemption *instant* is seed-deterministic, so a single seed must
/// be bit-identical twice. A different seed is run too, to show the run completes
/// regardless (the timer-landing control flow does not depend on the RNG seed).
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

/// The built `irq-landing` payload ELF, or a loud panic with the build command.
fn irq_landing_payload() -> Vec<u8> {
    let p = repo_root().join("guest/payloads/target/x86_64-unknown-none/release/irq-landing");
    std::fs::read(&p).unwrap_or_else(|e| {
        panic!(
            "irq-landing payload not built ({e}) at {} — build it on the box first: \
             `cd guest/payloads && cargo build --release` (target x86_64-unknown-none).",
            p.display()
        )
    })
}

/// Boot `irq-landing` on the **patched** backend with the userspace xAPIC wired,
/// run to terminal, and return (state_hash, serial, terminal reason). Panics loudly
/// if the box is not ready (the same posture as `live_determinism.rs`).
fn run_irq_landing(seed: u64) -> ([u8; 32], Vec<u8>, TerminalReason) {
    assert!(
        std::path::Path::new("/dev/kvm").exists(),
        "/dev/kvm absent — run this `#[ignore]`d gate on the box with the LOADED patched KVM \
         (KVM_CAP_X86_DETERMINISTIC_INTERCEPTS) + perf, CPU-pinned core 2 (see the file header)."
    );
    let payload = irq_landing_payload();
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

    let r = vmm.run().expect("irq-landing run to terminal");
    (vmm.state_hash(), r.serial, r.reason)
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
    let (hash_a1, serial_a1, reason_a1) = run_irq_landing(SEED_A);
    assert_eq!(
        reason_a1,
        TerminalReason::DebugExit { code: 0 },
        "irq-landing must reach a CLEAN PASS — every armed LAPIC-timer deadline landed mid-spin \
         via run_until preemption. A `DebugExit {{ code: 1 }}` is the payload's FAILSAFE \
         (\"lapic timer never fired\"): preemption did not deliver the timer. serial:\n{}",
        String::from_utf8_lossy(&serial_a1)
    );
    eprintln!(
        "[gate2] seed A: irq-landing PASS — busy-spin preempted, all 8 timer deadlines landed.\n\
         [gate2]   state_hash = {}\n[gate2]   serial = {:?}",
        hex32(&hash_a1),
        String::from_utf8_lossy(&serial_a1),
    );

    // --- Deterministic twice: a second run at the SAME seed is bit-identical
    // (serial + state_hash). The preemption instant is a pure function of the seed,
    // so the interleaving — and thus all observable state — repeats exactly. ---
    let (hash_a2, serial_a2, reason_a2) = run_irq_landing(SEED_A);
    assert_eq!(reason_a2, TerminalReason::DebugExit { code: 0 });
    assert_eq!(
        hash_a1,
        hash_a2,
        "deterministic-twice: same-seed state_hash must be bit-identical (a={}, b={})",
        hex32(&hash_a1),
        hex32(&hash_a2)
    );
    assert_eq!(
        serial_a1, serial_a2,
        "deterministic-twice: same-seed serial must be bit-identical"
    );
    eprintln!(
        "[gate2] deterministic-twice CONFIRMED at seed {SEED_A:#018x}: state_hash {} == {}",
        hex32(&hash_a1),
        hex32(&hash_a2)
    );

    // --- Real seed-sensitivity (P2 round-9): the OLD leg only asserted seed B reaches
    // PASS — vacuous (it would pass even if seed B were byte-identical to seed A). Assert
    // BOTH halves of what the seed actually controls:
    //  (1) `state_hash` DIFFERS — the seed keys the VM's seeded-entropy stream, which is
    //      part of the hashed state, so a different seed yields a genuinely different VM
    //      state. (The payload consumes NO RNG, but the entropy state is still seeded.)
    //  (2) `serial` is IDENTICAL — `irq-landing` is O3:**pure**: its preemption instants
    //      and observable output are TIMER/branch-driven, independent of the RNG seed.
    // Together this proves the seed genuinely matters (state differs) AND the preemption
    // control flow does NOT leak seed-dependence (serial identical) — neither of which
    // the old PASS-only assertion checked. ---
    let (hash_b, serial_b, reason_b) = run_irq_landing(SEED_B);
    assert_eq!(
        reason_b,
        TerminalReason::DebugExit { code: 0 },
        "irq-landing must also reach a CLEAN PASS at a different seed"
    );
    assert_ne!(
        hash_b,
        hash_a1,
        "seed-sensitivity: a different seed must produce a DIFFERENT state_hash (the seeded \
         entropy stream is part of the state) — identical hashes would mean the seed is \
         ignored. seed A = {}, seed B = {}",
        hex32(&hash_a1),
        hex32(&hash_b)
    );
    assert_eq!(
        serial_b, serial_a1,
        "purity: the pure payload's serial / preemption control flow must be IDENTICAL across \
         seeds (the deadline landings do not depend on the RNG seed)"
    );
    eprintln!(
        "[gate2] seed B {SEED_B:#018x}: PASS, state_hash = {} != seed A (seed keys the state); \
         serial == seed A (pure control flow)",
        hex32(&hash_b)
    );
}
