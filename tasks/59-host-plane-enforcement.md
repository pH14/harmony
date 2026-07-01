# Task 59 — host-plane enforcement: apply `HostFault` at a `Moment` (light up `perturb`)

> **FRONTIER · the other half of task 45.** Task 45 delivered the `HostFault` types, the
> `Moment`-keyed `Action` recording, and the `perturb` wire verb — all pure logic
> (`dissonance/environment/src/host.rs:71-73` is explicit that enforcement is frontier). Nothing
> in vmm-core applies a `HostFault`; `perturb` returns `Unsupported` after task 58. This task
> builds the enforcement: run to the override's `Moment` using the machinery that already exists
> for exact-count arrival (task 47's `run_until` / `InjectionPlanner`), apply the fault between
> instructions, record it, and prove bit-identical replay.
>
> Depends on **task 58** (the server that carries `perturb` and the adapter that replays a
> `Recorded` env).

Read first: `tasks/00-CONVENTIONS.md`, `docs/DISSONANCE.md` ("The host control plane"),
`tasks/45-host-control-plane.md`, `consonance/vmm-core/src/vmm.rs` (`step`/`run_until` dispatch,
`service_pending_irqs` ~2120), `dissonance/environment/src/host.rs` (`HostFault`, `BitMask`),
`dissonance/environment/src/recorded.rs` (`record(at, action)` / `perturb(fault, at)` stamping).

## Environment

Portable logic (deadline scheduling of staged faults, the apply-at-Moment ordering rules,
Recorded-env bookkeeping) is mock-testable on macOS + Linux. The end-to-end proof is **box-only**
(patched KVM; exact-count arrival needs the PMU path). Standard box pinning + revert discipline.

## Scope: two faults now, two staged

Deliver **`CorruptMemory { gpa, mask }`** and **`InjectInterrupt { vector }`** — both are pure
"arrive at `Moment`, act, resume" applications of existing machinery (a guest-RAM write; an IRQ
line into the existing injection path). **`SkewTime` / `SetClockRate` are explicitly out of
scope** — they mutate the V-time clock itself (epoch/ratio) and interact with the armed-deadline
machinery; spec them as a follow-on once the two simple faults have proven the apply-at-Moment
seam. Record the deferral in `IMPLEMENTATION.md`.

## What to build

1. **Staged-fault schedule in the server/Vmm:** `perturb(fault, at)` stages `(Moment, HostFault)`
   into an ordered queue; `run(until)` becomes "run to `min(next staged Moment, until)`" —
   reusing the `run_until` deadline path — apply every fault staged at that count, then continue.
   Multiple faults at one `Moment` apply in stage order (deterministic; pin it with a test).
2. **Apply:**
   - `CorruptMemory`: XOR/AND the `BitMask` at `gpa` in guest RAM between instructions. Fail
     loud (`ControlError`) on out-of-range `gpa` — never silently clip.
   - `InjectInterrupt`: assert the vector through the existing userspace-LAPIC/IRQ arbitration
     path at the arrival point, so delivery ordering vs. the timer stays deterministic.
3. **Record:** every applied fault lands in the active `Recorded` env via task 45's stamping —
   the env that `recorded_env()` returns must replay to the identical `state_hash`.

## Acceptance gates

1. **Portable:** mock-backend tests — staging order, multiple-faults-per-Moment determinism,
   out-of-range rejection, and a proptest (≥256 cases) that an arbitrary staged schedule applied
   twice yields identical state evolution.
2. **Box gate:** against the Postgres workload — a `CorruptMemory` and an `InjectInterrupt`
   staged at chosen `Moment`s: (a) same schedule run twice → bit-identical `state_hash`;
   (b) `replay` of the emitted `Recorded` env → same hash again (record → replay closure);
   (c) schedule-absent control run differs (the faults are actually landing).
3. Standard suite green; existing `live_*` gates byte-identical (enforcement is additive —
   an empty schedule changes nothing).

## Non-goals

`SkewTime`/`SetClockRate` (follow-on); guest-plane faults (task 61); campaign policy (task 60);
any new fault classes beyond task 45's four.
