# Task 103 — Harden the box-gate CLI against vacuous-pass inputs

**Bead:** `hm-9wa` (P1 bug). **Source:** PR #93 round-9 cross-model pass (GPT-5.6 Sol),
accepted in the foreman's merge disposition as an immediate follow-up: the recorded M0
evidence is sound, but the gate machinery can print green on degenerate inputs it never
exercised. A gate that can pass vacuously is a standing hazard for every FUTURE run
(gate-integrity class — same family as the tasks/102 evidence-integrity countermeasures:
a done-marker or an empty workload is never a success condition).

Read first: the PR #93 round-9 comments (`gh pr view 93 --comments`, the "three real
hardening gaps" in the foreman disposition), `bd show hm-9wa`,
`docs/history/IMPLEMENTATION-task-93.md` context if present, and the merged M0 machinery in
`dissonance/conductor` (`gamecampaign.rs`, `boxrun.rs`, `exploration.rs`, the
`live_film.rs`/determinism gate callers).

## The three findings (all [blocking], each needs a fix + a regression test)

1. **Degenerate budgets pass the determinism gate.** `--max-branches 0` /
   `--deadline-delta 0` can print `DETERMINISM PASS` without doing any work. Fix both
   ends: (a) input validation — reject non-positive `max_branches` / `deadline_delta`
   (and any other budget parameter whose zero value hollows the gate) with a loud usage
   error; (b) gate-side vacuity guard — before printing PASS, assert the run produced
   nonzero work evidence (per-rep executed-step/frame counts > 0, nonzero branch count),
   so a future degenerate path fails even if a new flag combination sneaks past (a).
2. **Malformed pre-setup billboard bypasses `BillboardMissing`.** A zero-length or
   overflowing billboard range at registration escapes the round-8 `BillboardMissing`
   check. Validate `gpa`/`len` at registration: nonzero, in guest-RAM range,
   non-overflowing (`gpa + len` checked arithmetic). Malformed is the same loud failure
   class as missing — never a silent fallback.
3. **Start script can exceed `max_frames` while settling.** The scripted
   start-to-gameplay sequence can overrun the `max_frames` bound during settle. Bound
   the settle loop by the budget and fail loudly (`BadScript`-style typed error, like
   the round-5 cadence-overflow fix) — never a silent overrun.

## Constraints

- Fixes live in the conductor CLI/gate layer; do NOT touch harmony-linux/play-agent behavior or
  the sealed campaign semantics — this task changes what counts as a valid gate
  invocation, not what a campaign does.
- Every fix gets a regression test whose fixture is the vacuous input and whose
  assertion is that the gate FAILS loudly (exit code / typed error), mirroring
  `SealsThenCrashes`-style fixtures from round 8.
- Hash-neutrality: none of this may touch the shared draw stream or recorded envs —
  validation happens before/around runs, never inside them.

## Gates (Mac-portable; the box is NOT needed and is currently under the nested-posture
re-cert window — do not touch the box)

- `cargo nextest run -p conductor` (plus any crate the fixes touch), `cargo clippy -D
  warnings` (mac + linux target), `cargo fmt --check`, `cargo deny check`.
- Public-api snapshot regenerated if the typed-error surface changes
  (`cargo test -p conductor --test public_api -- --ignored`).
- New `unsafe`: none expected; if any appears it must be Miri-reachable per the
  tasks/98 discipline.

Done = all three regression tests red-before/green-after, gates green, PR opened with
the finding-by-finding mapping in the description.
