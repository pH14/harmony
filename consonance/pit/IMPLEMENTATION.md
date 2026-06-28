# `pit` — implementation notes

A deterministic, pure-logic i8254 PIT (Programmable Interval Timer), in the mold of
[`lapic`] and `vtime`: a state machine that takes **V-time in** + **port reads/writes
in** and produces **the next IRQ0 deadline + a pending-IRQ0 edge out**. It never reads
a clock. This is the missing primitive that gives real Linux a **tick** — see the task
spec and **Deterland** (Wu & Ford, arXiv:1504.07070 §4.1.2 *vTimer*): in virtual-wire
APIC mode Linux registers no clock-event device, so its natural tick source is the
legacy PIT (ports `0x40`–`0x43`, no ACPI/MADT). Modelling a deterministic vPIT makes
Linux register a real periodic tick whose IRQ0 `vmm-core` delivers via the
performance-counter-overflow + single-step machinery (Deterland §5.1 — task 47's
`run_until`).

## What it models

- **Three counters** (ports `0x40`/`0x41`/`0x42`) + the mode/command register
  (`0x43`). **Counter 0 drives IRQ0** — the system timer. Counters 1/2 are modelled
  for read/program fidelity but raise no interrupt.
- **Modes 0** (interrupt on terminal count, one-shot), **2** (rate generator,
  periodic — the mode Linux's `0x34` clockevent programs), **3** (square wave,
  periodic), **4** (software strobe, one-shot). Field values **6/7 alias 2/3**. Modes
  **1/5** are GATE-triggered; with counter 0's gate tied high they generate no IRQ.
- **Counter-latch** command, the **read-back** command (count and/or status, multi-
  counter), the **lobyte / hibyte / lobyte-hibyte** access modes (read + write flip-
  flops), and **BCD / binary** counting.
- The countdown decrements at **1.193182 MHz of V-time** ([`PIT_FREQ_HZ`], the
  `docs/CPU-MSR-CONTRACT.md` value — no contract change). A counter write stores
  `reload` + `arm_vns = now_vns`; the IRQ0 instant is the **derived**
  `arm_vns + ceil(N·1e9 / freq)` (ceil, so IRQ0 never fires before `N` whole ticks),
  and the current count is computed from `now_vns` on read. Deterministic by
  construction: same V-time + same port writes ⇒ same reads, deadlines, and
  [`PitState`].

## Determinism discipline

Integer-only `u128` intermediates, saturating to `u64::MAX` / `u16` (`vtime`'s house
style). No floating point, no `HashMap`/`HashSet`, no clock read. The firing decision
is made on the **un-saturated** `u128` span (a saturating deadline near `u64::MAX`
would clamp and re-fire forever — the bug `lapic` documents), and `advance_to` is
idempotent and catches up missed periods closed-form. Library code never panics on
untrusted input (every port access and every snapshot field is bounds-/range-checked).

## Deliberate simplifications (documented, not load-bearing)

- **Mode-3 read-back decrements by 1, not 2.** Real i8254 mode 3 (square wave)
  decrements the counting element by 2 per clock. Only the **IRQ rate** — one edge per
  `N` input clocks — is load-bearing for Linux's tick, and that is **exact**. The mid-
  period read-back value of a mode-3 counter is modelled as the linear `N − (ticks mod
  N)` (same as mode 2). Nothing in the boot path reads counter 0 back mid-period in
  mode 3 (Linux reads via a latch, and uses mode 2 for the periodic clockevent), so the
  by-2 nuance is not modelled. The independent reference model uses the same spec, and
  the property test verifies the closed form against a tick-by-tick simulator.
- **The status-byte OUTPUT pin (bit 7) is best-effort.** Its precise waveform phase is
  not load-bearing (the IRQ *timing* is exact and is what Linux consumes); the RW /
  mode / BCD / NULL-count fields of the status byte are exact.
- **Counter 2's GATE (port `0x61`) is tied high** (not modelled). The speaker /
  counter-2 TSC-calibration path is unused here because the contract calibrates the TSC
  from CPUID 0x15 (skipping the PIT measurement loop). Port `0x61` stays with
  `vmm-core`'s legacy-platform stub (accepted/dropped).
- **Out-of-range BCD nibbles** (`A`–`F`) are summed positionally rather than trapping —
  total and deterministic. No real i8254 input produces them (Linux uses binary mode).

## Gates

- `cargo build/test/clippy -D/fmt` green; `cargo deny` (workspace) — only whitelisted
  deps (`thiserror`, optional `serde`, dev `proptest`).
- **Unit tests** (`src/device/tests.rs`, 25): every mode, access mode, latch/read-back,
  BCD, idempotence, catch-up, snapshot/restore + restore validation, saturation.
- **Property tests** (`tests/reference.rs`, ≥512 cases each) vs an **independent
  tick-stepping reference**: current-count read-back, IRQ0 edge timing, the deadline
  ceiling at the real frequency, snapshot/restore transparency, and a no-panic fuzz.
- **Kani proofs** (`src/device_proofs.rs`): decode range, period no-panic + exact ceil,
  periodic current-count range, `advance` idempotence at the `u64::MAX` saturation
  boundary, the no-deadline-on-overflow saturation contract, and mode-decode totality.
- **No `unsafe`** ⇒ no Miri requirement (the unsafe⇒Miri rule does not apply).

## Integration (in `vmm-core`, outside this crate)

`vmm-core` routes the `0x40`–`0x43` port exits into [`Pit`], sources its
`preemption_deadline()` / idle-resume target from [`Pit::next_irq0_deadline`] (the
active clock-event — re-keyed off the now-dormant LAPIC timer), and injects IRQ0
(vector `0x30`) through the 8259 ExtINT path when `RFLAGS.IF == 1`, acknowledging the
edge with [`Pit::ack_irq0`]. See `consonance/vmm-core/IMPLEMENTATION.md` (task 53).

[`lapic`]: ../lapic
