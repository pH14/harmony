# Task 168 — hm-c8ho: draw-probe hardening batch (PR #167 parks F3+F4+F6)

**Bead:** `hm-c8ho` (P2). Read it first — `bd show hm-c8ho` from the main workspace — it
carries the three items verbatim with sites and fix shapes. Provenance: the PR #167
adjudication record (read it on the PR before coding; do not re-litigate refuted items).

Three campaign-runner-local changes, one editing session, no box access required:

1. **F3** — `materialize.rs:458`-area: `saturating_add(1)` → `checked_add(1)`; when no
   representable post-boundary settle point exists, fail LOUD with
   `MachineError::Transport` (mirror the existing `reseed_probe_env` guard in the same
   file). Ship the regression in the same commit (portable — a mock at the pinned top of
   the V-time axis; `mock.rs:520-556` pins those semantics).
2. **F4** — correct the `hop_draws`/`tail_draws` doc comments (`materialize.rs:135-153`):
   the probe answers over the half-open window at the arrival micro-position, not an
   `iff` over the closed window. Doc-only; do NOT attempt the optional box skid-draw
   payload in this lane.
3. **F6** — `live_draw_probe_pair.rs:270`: tighten the positive arm from
   `any(hop) || tail` to the full measured pattern (`hops == [false,false,true] && tail`)
   per the lane-record table — pre-proven to pass on the recorded box run. Test-only;
   the gate is box-only `#[ignore]`, so this compiles portably and runs on the next
   box window.

## Gates

`cargo nextest run -p campaign-runner`, clippy `-D warnings`, fmt. The F3 regression must
be shown RED against the pre-fix `saturating_add` (state the fail-before in the PR body).
Do not weaken any existing check; do not touch anything outside the three named items.
