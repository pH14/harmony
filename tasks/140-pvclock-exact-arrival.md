# Task 140 — exact-arrival misses staged Moments on pvclock guests (maze @3e7 repro)

**Bead:** hm-zwhi (P2 bug, found by task 134 M2 box validation 2026-07-22). Design home for
the root-cause class: hm-x1ss (schedule-closure). The ARM `arm_arrival` port (hm-sp8v) must
inherit whatever this task finds — note it on the bead, don't build it.

## Symptom

A maze SelectorV1 exploit rollout run (`StopMask::NONE`, single deadline origin+delta) with
delta = 3e7 (multi-quantum) fails: the server refuses with `control error: run overshot
staged Moment <M> (now at V-time <M'>); schedule unsatisfiable`. The exact-arrival machinery
expected the run to STOP exactly at the staged reseed Moment M (drain it), but the run
sailed past to the next intercept-grid boundary — overshoot < 1 quantum.

## Evidence (recorded on the bead; box artifacts under /root/qcpp-evidence)

- @3e7 as-configured: overshot staged Moment 237012404 → stopped 244341956 (+7,329,552 ns
  ≈ 0.7 quantum). @3e7 with the marker window bounded to one intercept quantum: STILL
  overshoots (+1,034,202 ns) — **not a marker-placement problem**.
- @1e7 (one-quantum): green, 11/11 exploits drained in-window reseeds at exact Moments,
  bit-identical.
- **Key discriminator:** PR #138 re-materialized MULTI-QUANTUM candidate-seal legs with
  mid-leg reseed markers drained bit-identically on the 2s game path (~192 quanta). So
  multi-quantum legs are NOT inherently broken — the failing ingredient is the maze being a
  **pvclock guest** (task-110 paravirt clock; the ~10,416,667 ns quantum IS the guest's
  pvclock refresh grid).

## Leading hypothesis (verify FIRST, before any fix)

The control.rs run-loop arms `vmm.arm_arrival(m)` for each staged Moment and **drops the
bool return**. `arm_arrival` silently declines (returns false, no-op) when
`work_for_vns(m) < last_intercept_work`. Hypothesis: after a pvclock re-anchor (skid
correction) the vns↔work conversion disagrees with the published `effective_vns`; a future
staged Moment maps to a work value below the current anchor, so the arm silently no-ops and
the guest free-runs to the next natural boundary. **Verify by logging the arm_arrival bool
+ the (vns, work, effective_vns) triple at each staged-Moment arm** on a failing @3e7-shaped
leg. If the hypothesis is wrong, STOP and record what the instrumentation actually shows
before choosing a different fix — do not fix blind.

## Fix direction (determinism-core)

The rollout run must marker-clamp staged reseeds the way `materialize_candidate` already
does for the seal (stop AT each staged Moment, drain, continue), and/or `arm_arrival`'s
decline must stop being silently dropped (typed error or loud refusal at the arm site — a
declined arm on a pvclock guest is a schedule-integrity event, not a no-op). Determinism
discipline: the fix must be hash-neutral for all currently-green paths (the @1e7 maze lane
and the PR #138 game-path legs are the regression suite's oracle).

## Scope guards

- Do NOT build the maze-driver deadline_delta guard (the accepted @1e7 regime is Paul's
  Option (a)); a loud rejection is noted as follow-up on the bead only.
- Do NOT take on the hm-x1ss schedule-closure design; feed findings to that bead.
- Surface: `consonance/vmm-core` (control run-loop, arm_arrival seam) + the maze/campaign
  test surface that exercises it. No spike code, no ARM code.

## Acceptance

- Portable: a deterministic regression test reproducing the decline/overshoot shape at the
  vns↔work seam (mock/sim level — the task-134 maze sim surface or a focused unit around
  the anchor math), red before the fix, green after; the full portable gate suite green.
- The @1e7 maze path and the PR #138 marker-clamp paths provably unchanged (hash-neutral —
  existing tests + any golden digests stay bit-identical).
- Box leg: the @3e7 repro on the x86 determinism box IF the box is reachable (test
  `ssh hetzner` first — access fluctuates; pin per docs/BOX-PINNING.md, smoke-fire-once,
  revert KVM to stock when done). If unreachable, deliver the exact live runbook +
  sim-level evidence and hand the box leg to the foreman — do not block the PR on it.

## Environment

Mac-local development; x86 determinism box (`ssh hetzner`) optional as above. The prior
@1e7/@3e7 evidence lives on the box under `/root/qcpp-evidence` (read-only audit tool
alongside) if reachable. No ARM box (it is spun down), no Nimbus.
