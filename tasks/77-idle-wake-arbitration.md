# Task 77 — `consonance/vmm-core`: unify HLT idle-wake arbitration with IRQ service

> **Delegable (single crate, Mac-gated) · quality tier · LAND BEFORE TASK 61.** Integrator
> feedback (2026-07-02, verified against `vmm.rs`): `idle_action` (vmm.rs ~2018) enumerates only
> **LAPIC** wake sources when deciding whether a `HLT` is terminal — `lapic.peek_interrupt()`
> for a pending wake, `lapic.next_timer_deadline()` for a future one — while
> `service_pending_irqs` (vmm.rs ~2137) arbitrates **both** planes:
> `lapic_vector.or_else(pending_serial_vector)`. The serial ExtINT line is not in the idle
> discriminator.

## The defect

A guest halted with `IF=1`, **no armed LAPIC timer**, and the serial THRE interrupt as its
**only** pending wake is classified `IdleAction::Terminal` despite a legitimate deliverable
wake. Reachable in principle via `cli; …; sti; hlt` where the queued serial vector never found
an open interrupt window (KVM surfaces the HLT exit rather than the window-open exit for a
userspace-irqchip halt). It is deterministic (same seed → same wrong terminal) and narrow —
Linux idle almost always has a timer armed post-MADT/ARAT, and there is no RX side to wait
on — so this is **not urgent**. The deeper problem is structural: **the wake-source set is
duplicated in two places with different membership.** Every future device IRQ (task 61's net
vertical is the first) would have to remember to extend `idle_action` too — a latent
determinism trap.

## What to build

Make `idle_action` and `service_pending_irqs` consume **one shared wake-source arbitration**
(one function/iterator that yields the deliverable-vector arbitration both paths use — LAPIC
outranks ExtINT serial, exactly as `service_pending_irqs` orders it today). `idle_action`'s
three-way answer (`DeliverPending` / `JumpToDeadline` / `Terminal`) keeps its shape; only the
"is there a pending wake?" predicate changes source. If full unification is disproportionate,
the minimum bar is: `idle_action` consults `pending_serial_vector()` alongside the LAPIC peek
**and** a debug assertion that the two paths' pending-wake answers agree at every HLT exit.

## Acceptance gates

1. **The regression test**: a scripted `MockBackend` run where the guest halts with `IF=1`, no
   armed timer, and a pending serial (THRE) vector — must classify as a deliverable wake (not
   `Terminal`) and deliver the serial vector on re-entry. Pin the delivered vector exactly.
2. **The symmetry gate**: a test (or Kani-style exhaustive check over the small wake-state
   space) that `idle_action == Terminal` ⇒ `service_pending_irqs`-style arbitration yields no
   vector — the two membership sets can never diverge again.
3. **No behavior change outside the narrow case**: the standard suite green on `vmm-core`;
   existing `live_*` gates byte-identical (no golden re-blessing — Linux guests always have a
   timer armed at idle, so no shipped gate's hash may move). Any hash movement is a red flag,
   not a re-bless.
4. Standard suite (build / nextest / clippy `-D warnings` / fmt / deny) on `vmm-core`.

## Non-goals

New wake sources; RX-side serial modeling; touching the LAPIC arbitration order (LAPIC over
ExtINT stands); task-61 device IRQs (this task just makes their arrival safe).
