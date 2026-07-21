# Task 134 — First cooperative Differential exploration gate: the maze vertical

**Bead:** hm-cs5 (P1, Paul-authored 2026-07-09; description + notes are BINDING — this spec adds mechanics, it does not override them; on any conflict the bead wins and a `[question]` goes to the integrator). **Sequencing:** deliberately begins after the Differential migration epic (hm-bbx, closed 2026-07-21) so the first campaign uses the declared full-retention evaluation profile from rollout one. Downstream: hm-yjf (mechanism ratification before transfer) consumes this task's report.

## Goal

Implement the first production cooperative vertical on the merged Differential plane: a **deterministic Linux-guest maze workload** with explicit bounded-integer X/Y instrumentation over binary wire v2, driven end-to-end through the generic Explorer's two-barrier materialization path, with an archive-guided selector compared against permanent controls.

## Scope (from the bead, restated)

1. **Maze guest workload** — no maze implementation exists in the repo today. Build it through the repository image/init path (the task-86/SMB-era guest build machinery and the harmony-linux environment tier from #133 are the reference paths). It declares bounded integer X/Y state over **wire v2** (no unresolved-v1 state semantics, no floating-point guidance).
2. **Evidence path** — normalized ordered SdkEvents; lineage-complete server-captured evidence cuts; independent persistent X/Y observations; CellFn keyed at **actual sealed_at**; Differential best-Entry-per-cell occupancy; second-revision actual-seal admission (provisional first-pass transitions stay non-authoritative until then); retained-Entry restoration.
3. **Campaign gate** — simple deterministic selector (archive-guided) vs **equal-budget pure-random and frontier-off controls**, across the ruled seed count, multi-seed trial discipline preserved.
4. **Explicit non-goals** — no LinkSensor, no LINK_STATE_CHANNEL, no packed FeatureId, no legacy CoverageArchive, no task-70 assumptions, no advanced selector, no STADS-as-selector, no seal-relative console evidence.

## Acceptance (bead criteria, verbatim authority)

The deterministic maze guest builds through the repository image/init path and declares bounded integer X/Y state over wire v2. From rollout one, the generic Explorer uses the declared full-retention evaluation profile, persists nonempty SDK evidence, materializes simultaneous X/Y through lineage-complete same-Moment cuts, performs the second-revision actual-seal admission, restores retained Entries, and compares the simple archive-guided configuration against equal-budget pure-random and frontier-off controls across the ruled seed count. Same seed/config yields identical selections and artifacts. The report measures cells/depth plus held bug/progress evidence.

## Mechanics

- Portable-first: the maze machine and the full evidence path must run as a portable (mock/toy) configuration under nextest before any box spend; the box campaign is the live gate.
- Box (ssh hetzner): lease via `bash scripts/box-window.sh acquire t134`, pin per `docs/BOX-PINNING.md`, bundle-transfer for code. **Smoke-fire-once** before the multi-seed campaign budget; report the smoke before proceeding.
- Determinism bar: record→replay bit-identical `state_hash` on the live path; same seed/config ⇒ identical selections and artifacts (this is an acceptance criterion, not aspiration).
- Authorities to read before coding: `bd show hm-cs5` (binding), `docs/DISSONANCE-STRATEGY.md`, `docs/GLOSSARY.md` + `docs/LAYERS.md` (vocabulary/layering are binding on new code), `docs/SCORING.md` (cost stays out of Reward; epoch-wise granularity), and the merged child surfaces (#120 ingress, #124 revision coordinator, #129 seal cuts, #130 Explorer integration, #131 retention, #134 vertical).
- Milestone the work: M0 = portable maze machine + wire-v2 declarations + evidence path green under nextest; M1 = guest image/init wiring + live smoke; M2 = the controlled multi-seed campaign + report. Open the PR after M0 for early review; live gates land on the same PR.
