# Task 13 — `consonance/lapic`: userspace xAPIC + V-time timer

Read `tasks/00-CONVENTIONS.md` first. Touch only `consonance/lapic/`.

## Environment

Runs on: macOS and Linux. Requires: Rust only. Does not require: `/dev/kvm`, Intel CPU,
QEMU, root. `#![no_std]` (with `extern crate alloc`) — no host deps, no syscalls, no time.

## Context

Ruling **R1** (`docs/R1-DEVICE-MODEL.md`) settled the device model: `vmm-core` runs with **no
in-kernel interrupt controller** (`KVM_IRQCHIP_NONE`) and emulates the Local APIC **in
userspace as an xAPIC** (MMIO page at `0xFEE0_0000`), with its timer driven by virtual time
rather than host wall-clock. R1 spun the LAPIC's *logic* out as a self-contained, pure-logic
crate in the mold of `vtime`/`snapshot-store` — this task. The KVM-facing glue (routing
`KVM_EXIT_MMIO` on the APIC page into this crate, calling `KVM_INTERRUPT`, the
interrupt-window handshake) stays frontier in `vmm-core` and is **not** part of this task.

This crate is a deterministic state machine: **V-time in (a `u64` nanosecond count), MMIO
register reads/writes in, and timer deadlines + deliverable interrupt vectors out.** It never
reads a clock; the caller supplies `now_vns`. It is the source of truth for the xAPIC register
file, prioritized interrupt delivery, EOI, and the initial-count→deadline timer model. Its
snapshot struct (`LapicState`) is consumed verbatim by task 09 (`vm-state`).

What R1 fixes that constrains this crate (`docs/R1-DEVICE-MODEL.md` §"The ruling",
§Constraints; `docs/CPU-MSR-CONTRACT.md` §5):

- **xAPIC only** (32-bit MMIO registers at 16-byte-aligned offsets). **No x2APIC** (no MSR
  interface). **No TSC-deadline timer** — only the classic one-shot and periodic LVT-timer
  modes via the initial-count register.
- The timer's input clock is the **frozen core crystal frequency** (CPUID `0x15`, per the
  CPU/MSR contract), divided by the divide-config register. A write to initial-count
  (`APIC_TMICT`, offset `0x380`) becomes an absolute V-time deadline; current-count
  (`APIC_TMCCT`, offset `0x390`) is **computed from `now_vns` on read**, never decremented by a
  background tick.
- Single vCPU: the only IPI destination is self. Inter-processor delivery to other LAPICs has
  nowhere to go.

## Determinism discipline (rule #4, load-bearing here)

All timer arithmetic is **integer-only**, computed in `u128` intermediates and **saturating to
`u64::MAX`** — exactly `vtime`'s house style (see `consonance/vtime/src/clock.rs`). No floating
point anywhere. No `HashMap`/`HashSet` reaching an output or the snapshot bytes. Identical
inputs ⇒ bit-identical register reads, deadlines, and `LapicState`.

## Public API

`#![no_std]` crate; `extern crate alloc` is permitted (for `Vec` in snapshot encoding only).
No `unsafe`. Add `serde` as an **optional** dependency: `serde = { version = "1", default-features
= false, features = ["derive", "alloc"] }`, behind a `serde` feature, used **only** to derive
`Serialize`/`Deserialize` on `LapicState` (the seam task 09 consumes). The core state machine
must compile and pass all gates with the feature **off**.

```rust
pub const APIC_BASE_DEFAULT: u64 = 0xFEE0_0000; // xAPIC MMIO base (relocatable via IA32_APIC_BASE; vmm-core owns that)
pub const APIC_MMIO_SIZE: usize  = 0x1000;       // one 4 KiB page

pub struct LapicConfig {
    pub apic_id: u32,
    /// Frozen APIC timer input frequency in Hz — the core crystal clock from
    /// CPUID 0x15 per `docs/CPU-MSR-CONTRACT.md`. The timer counts down at this
    /// rate divided by the divide-config setting. Must be non-zero.
    pub timer_hz: u64,
}

pub struct Lapic { /* register file + timer bookkeeping; not Copy/Clone-required */ }

impl Lapic {
    /// Power-on/reset state per the SDM: software-disabled (SVR bit 8 = 0), all
    /// LVT entries masked, IRR/ISR/TMR clear, TPR = 0, timer stopped.
    pub fn new(cfg: LapicConfig) -> Result<Lapic, LapicError>;

    /// Read a 32-bit register at `offset` (0x000..=0xFF0, must be 16-byte aligned).
    /// `now_vns` lets `APIC_TMCCT` (0x390) reflect elapsed V-time. Reads have no
    /// side effects except as the SDM specifies (e.g. none for these registers).
    pub fn mmio_read(&self, offset: u32, now_vns: u64) -> Result<u32, LapicError>;

    /// Write a 32-bit register at `offset`. May (re)arm the timer (`APIC_TMICT`),
    /// raise a self-IPI (`APIC_ICR`), set the divide (`APIC_TDCR`), retire the
    /// highest ISR bit (`APIC_EOI`), enable/disable the APIC (`APIC_SPURIOUS`), etc.
    /// Returns Ok(()); observable effects surface through the methods below.
    pub fn mmio_write(&mut self, offset: u32, value: u32, now_vns: u64) -> Result<(), LapicError>;

    /// Absolute V-time (vns) at which the armed timer next expires, or `None` if
    /// the timer is stopped, masked, or the APIC is software-disabled. vmm-core
    /// schedules this into `vtime::TimerQueue`; on a tie/rearm it re-reads.
    pub fn next_timer_deadline(&self) -> Option<u64>;

    /// Advance V-time to `now_vns`: if an armed, unmasked timer is now due, set its
    /// LVT-timer vector in IRR; rearm for the next period if periodic, else stop.
    /// Idempotent for a given `now_vns`. Returns true if any state changed (so the
    /// caller knows to re-read `next_timer_deadline`/`has_deliverable`).
    pub fn advance_to(&mut self, now_vns: u64) -> bool;

    /// Raise an edge-triggered interrupt request for `vector` (sets IRR). The path
    /// for the timer LVT and self-IPI; also the seam a future device would use.
    /// Vectors < 16 are reserved → `LapicError::ReservedVector`.
    pub fn raise(&mut self, vector: u8) -> Result<(), LapicError>;

    /// Is there a pending IRR vector whose priority exceeds the current PPR, with
    /// the APIC software-enabled? vmm-core uses this to request an interrupt window.
    pub fn has_deliverable(&self) -> bool;

    /// Deliver the highest-priority pending vector to the guest: move it IRR→ISR,
    /// update PPR, return the vector for `KVM_INTERRUPT`. `None` if nothing is
    /// deliverable above PPR. Caller must only invoke this when the guest is ready
    /// to accept (RFLAGS.IF set, no interrupt shadow) — that gate lives in vmm-core.
    pub fn take_interrupt(&mut self) -> Option<u8>;

    /// End-of-interrupt: clear the highest in-service (ISR) bit and recompute PPR.
    /// Equivalent to a guest write to the `APIC_EOI` register; exposed directly for
    /// vmm-core. No-op (not an error) if ISR is empty.
    pub fn eoi(&mut self);

    /// Plain-data snapshot of the entire register file + timer state. This is the
    /// struct task 09 (`vm-state`) embeds in the vm_state blob.
    pub fn snapshot(&self) -> LapicState;

    /// Reconstruct from a snapshot. Validates internal consistency (e.g. `timer_hz != 0`,
    /// armed-state sanity) and rejects malformed state (`InvalidState`).
    pub fn restore(state: &LapicState) -> Result<Lapic, LapicError>;
}

/// Versioned, plain-data image of the LAPIC — register file + timer bookkeeping.
/// Public fields (or a fully accessor-covered surface) so task 09 can serialize it.
/// `#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LapicState { /* id, version, tpr, svr, ldr, dfr, esr; isr/tmr/irr (8×u32 each);
                           the 6 LVT entries; icr; divide-config; timer: initial_count + arm_vns
                           + mode + period_vns (deadline_vns is derived, not stored); ... */ }

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LapicError {
    /// Offset not 16-byte aligned or outside 0x000..=0xFF0. (A read-only or reserved-but-in-range
    /// *write* is NOT an error — it is deny-ignored per the CPU/MSR contract; only a malformed
    /// offset reaches here.)
    BadOffset(u32),
    /// Vector < 16 (reserved by the architecture).
    ReservedVector(u8),
    /// `timer_hz == 0`, or a restored `LapicState` failed a consistency check.
    InvalidState,
}
```

## Semantics that must hold

- **Register file fidelity.** Model at minimum: ID (0x20), Version (0x30 — report
  **`0x0005_0014`**: xAPIC version `0x14`, max-LVT = 5), TPR (0x80), PPR (0xA0, read-only/derived),
  EOI (0xB0, write-only), LDR (0xD0), DFR (0xE0), SVR (0xF0, incl. bit 8 = software enable),
  ISR/TMR/IRR (0x100/0x180/0x200, 256 bits each as 8×u32, read-only), ESR (0x280), ICR low/high
  (0x300/0x310), the **six** LVT entries (Timer 0x320, Thermal 0x330, PerfMon 0x340, LINT0 0x350,
  LINT1 0x360, Error 0x370), initial-count (0x380), current-count (0x390, read-only/derived),
  divide-config (0x3E0 — **all 8 encodings are legal**: bits [3,1,0] select ÷1/2/4/8/16/32/64/128
  with bit 2 ignored, so no TDCR value is rejected). **LVT CMCI (0x2F0) is NOT modeled** — max-LVT = 5 excludes it and the
  CPU/MSR contract fixes 0x2F0 `allow-fixed(0)` with ignored writes, so it reads 0 and drops writes
  like any read-only register. Reads of unimplemented-but-architectural registers return 0;
  **writes to read-only or reserved-in-range registers are silently dropped (deny-ignore-write —
  log and ignore, matching the CPU/MSR contract), never an error** (PPR/ISR/IRR/TMCCT/Version/CMCI
  accept and discard a guest write). Only a misaligned or out-of-range offset is `BadOffset`. Cite
  the SDM (Vol. 3A §11.x "Advanced Programmable Interrupt Controller") in `IMPLEMENTATION.md` for
  the register map you implement.
- **Timer model (V-time-driven, the heart of this crate).** A write to initial-count with a
  non-zero value, the LVT-timer unmasked, and the APIC enabled arms the timer. The armed state
  stores **`initial_count = N` and `arm_vns = now_vns`** (not just a deadline) — that is what makes
  the TMCCT round-trip exact for *arbitrary* `timer_hz`. The firing instant is
  `deadline_vns = arm_vns + ceil_div(N * divide * 1_000_000_000, timer_hz)` (u128, saturating;
  **ceil** so the timer never fires before `N` whole ticks have elapsed). Mode comes from LVT-timer
  bits 17–18: **one-shot** (fire once, then stop) or **periodic** (on fire, re-arm with
  `arm_vns = deadline_vns`, same `N`). **TSC-deadline mode (0b10) is illegal here** — R1 masks its
  CPUID bit; if a guest selects it, hold the timer stopped (document the choice). Writing
  initial-count = 0 disarms. On `advance_to(now)` reaching the deadline of an unmasked timer, set
  the LVT-timer vector in IRR.
- **Current-count is computed, never ticked.** `APIC_TMCCT` read at `now_vns` returns
  `N.saturating_sub(elapsed_ticks)`, where
  `elapsed_ticks = (now_vns − arm_vns) * timer_hz / (divide * 1_000_000_000)` (u128 **floor**,
  saturating to `u32::MAX`), or 0 if stopped/expired. Deriving TMCCT from *elapsed* ticks since
  `arm_vns` — rather than reconstructing it from `deadline − now` — is what makes the **round-trip
  exact for every `timer_hz`**: at the arming instant `now_vns == arm_vns`, `elapsed_ticks = 0` and
  TMCCT `= N`. (The earlier `deadline`-derived formula double-floored — e.g. `timer_hz =
  24_000_000, divide = 16, N = 1` gave `0`, not `1` — which made the gate unsatisfiable.)
  **Round-trip exactness is a gate.**
- **Prioritized delivery.** Interrupt priority class = `vector >> 4`. PPR =
  `max(TPR_class, highest_ISR_class) << 4`-style per the SDM (model TPR and the in-service
  priority). `has_deliverable()` is true iff the highest IRR vector's class > PPR's class and
  the APIC is software-enabled. `take_interrupt()` selects the highest IRR vector, clears its
  IRR bit, sets its ISR bit, recomputes PPR, returns it. `eoi()` clears the highest ISR bit and
  recomputes PPR. Higher vector wins ties within a class.
- **Self-IPI via ICR.** An ICR write with fixed delivery mode targeting self (destination
  shorthand `01`, or `10` = all-including-self) calls the internal equivalent of `raise(vector)`.
  Shorthand `11` (all-excluding-self) and any non-self physical/logical destination are
  **no-ops** (single vCPU; nowhere to deliver) — document, don't error.
- **Software-disabled APIC (SVR bit 8 = 0).** No vector is deliverable; all LVTs behave as
  masked; the timer does not fire. This is the reset state.
- **Snapshot round-trips exactly.** `restore(&lapic.snapshot())` reproduces a LAPIC that is
  observationally identical: same register reads at every offset for every `now_vns`, same
  `next_timer_deadline`, same delivery decisions. Deadlines are absolute V-time, so they survive
  restore unchanged (matching `TimerQueue`/`VClock` discipline in INTEGRATION.md §4).
- **No panics on untrusted input** (rule #4): every `mmio_read`/`mmio_write`/`restore` path
  returns `Result`; out-of-range offsets and malformed state are errors, never panics.

## Acceptance gates

Beyond the standard gates (build/nextest/clippy `-D warnings`/fmt/deny):

1. **Timer round-trip proptest (core gate).** For arbitrary `(timer_hz, divide, initial_count,
   now_vns, mode)`: arm at `t0`, assert `next_timer_deadline()` equals
   `t0 + ceil_div(N·divide·1e9, timer_hz)`; **assert current-count read at `t0` equals `N` exactly**
   (the arming-instant round trip — must hold for *every* `timer_hz`, including non-dividing ones
   like `24_000_000`); read current-count at `t0 + Δ` for arbitrary `Δ` and assert it equals
   `N − floor(Δ·timer_hz/(divide·1e9))` (monotonically non-increasing, 0 at/after the deadline);
   for periodic mode, drive `advance_to` across several periods and assert the LVT-timer vector
   lands in IRR exactly once per period and the rearm instants are exact. ≥ 256 cases.
2. **Delivery-ordering proptest.** Apply an arbitrary sequence of `raise`/`take_interrupt`/`eoi`/
   TPR writes; assert against a naive model (a sorted set of pending vectors + an ISR stack) that
   `take_interrupt` always returns the highest deliverable vector above PPR and that EOI nesting
   is LIFO-correct. ≥ 256 cases.
3. **Snapshot round-trip proptest.** Build an arbitrary reachable LAPIC state (random sequence of
   MMIO writes + advances), `snapshot()` → `restore()` → assert observational equality (sweep all
   register offsets at several `now_vns` values; compare `next_timer_deadline`/`has_deliverable`).
   `snapshot()` is deterministic: equal states ⇒ equal `LapicState`. ≥ 256 cases.
4. **Kani proofs (house style, like `vtime::clock_proofs`).** Prove the deadline computation
   (`ceil_div(N·divide·1e9, timer_hz)`) and the elapsed-ticks current-count never panic and
   saturate rather than overflow for all `u32` counts / `u64` `timer_hz`; prove PPR/priority
   comparison is total and never indexes out of bounds.
   Bound the proofs so they finish in CI; record runtimes in `IMPLEMENTATION.md`.
5. **Reset-state test.** `Lapic::new` matches the SDM power-on values (software-disabled, LVTs
   masked, counts zero); no interrupt is deliverable until the guest enables the APIC.

Property tests ≥ 256 cases; keep total `cargo test` under ~3 minutes (Kani excluded).

## Non-goals

x2APIC / MSR interface (R1 forbids it); TSC-deadline timer (masked); the IOAPIC (omitted —
no device lines); PIC/PIT (separate minimal stubs, frontier — see task 09's device section);
the KVM-facing adapter (`KVM_EXIT_MMIO` routing, `KVM_INTERRUPT`, interrupt-window handshake —
frontier `vmm-core`); multi-vCPU / inter-processor IPI delivery; APICv/posted interrupts
(structurally off per R1); relocating the MMIO base (vmm-core owns `IA32_APIC_BASE`). Do not
depend on `vtime` or any sibling crate — express V-time as plain `u64` nanoseconds (rule #2).
