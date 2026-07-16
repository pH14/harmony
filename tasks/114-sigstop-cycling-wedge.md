# tasks/114 — SIGSTOP-cycling wedge (hm-440)

**Bead:** `hm-440` (P2 bug on main, nested-x86 spike finding) · **Class:** bug fix
(crate code allowed where the diagnosis leads; keep the surface minimal)

## Problem

During the nested-x86 spike's N-3 pause/resume conditions (2026-07-10),
**SIGSTOP/SIGCONT cycling of the process can wedge the stack** — a silent hang, not an
error. Evidence: `spikes/nested-x86` results on the `spike/nested-x86` branch (start
there; the spike recorded the wedging condition and its observations).

Motivating condition: any future host that suspends the process (cloud live-migration
rehearsal was the spike condition that surfaced it). A determinism substrate must
never silently hang under suspension — it either survives it or refuses it loudly.

## Task

1. **Mine the spike evidence first** (`spike/nested-x86` branch, results + README for
   the N-3 pause/resume condition): what exactly was cycled (whole process? vCPU
   thread?), at what cadence, and what wedged (KVM_RUN never returning? a waiter
   deadlock? the control server?).
2. **Reproduce on current main** — portable if the wedge is in pure run-loop/waiter
   logic (a mock-backed repro is strongly preferred); on the box under real KVM if the
   wedge needs a live KVM_RUN (`ssh hetzner`, pin per `docs/BOX-PINNING.md`, stock KVM
   should suffice — no patched window unless the repro demands it; check QUEUE first,
   release when done).
3. **Diagnose the wedge mechanism** precisely (which thread holds/waits on what).
   Beware the `pgrep`/`pkill` argv self-match landmine if scripting the cycling
   (`bd memories box-pkill-argv-landmine`).
4. **Fix or fail closed** per the bead: preferred is a real fix (suspension-safe
   waits); acceptable is detection + loud refusal (a wedge detector that turns the
   hang into a typed error). Never a silent hang. Add a regression test that drives
   the SIGSTOP/SIGCONT cycle against the repro surface (portable with a mock if
   possible; otherwise a box live test following the `live_*.rs` `#[ignore]` pattern).
5. Standard gates: workspace nextest, clippy (all targets), fmt, deny; Miri if the
   touched code has `unsafe` (allocation-backed seam, PR #99 precedent). Box gate only
   if the repro/fix is box-bound.

## Deliverables

A PR on this task's branch: the diagnosis (in the PR body — mechanism, thread/lock
chain), the fix or fail-closed guard, the regression test, gates green. `bd` note on
`hm-440` linking the PR. If the diagnosis shows the wedge lives in SPIKE-ONLY harness
code (not shipping crates), say so — the fix then lands in the spike harness and the
shipping-crate exposure gets an explicit disposition (exposed/not-exposed and why).

## Ground rules

- Minimal surface: fix where the diagnosis leads, no opportunistic refactors.
- If the wedge implicates the vmm-core run loop / control server (shipping crates),
  the fix is substantive-tier review — expect a cross-model pass; design accordingly
  (small, testable, documented).
- Escalate instead of guessing if the diagnosis is ambiguous after a bounded effort
  (~2h): post findings on the PR/bead and stop.
