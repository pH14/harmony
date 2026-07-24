# Task 156 — MockBackend: fold lateness into the script entry's reached (hm-j16h)

**Bead:** `hm-j16h` (P2, PR #145 F2+F3 judge-pooled family — `bd show hm-j16h` and
read the PR #145 adjudication's F2/F3 sections first). **Surface:** the MockBackend
double (`mock.rs`, non-default mock feature), its doc, tests, IMPLEMENTATION.md.
Test-double fidelity work — no production backend, wire, or contract change.

## Problem (both halves, one structural closure — judge-verified viable)

- **F2**: `mock.rs:434` `late_landings.pop_front().unwrap_or(deadline)` uses the
  popped value UNCHECKED — a scripted at-or-before value violates the frozen
  `CommonExit::Deadline` invariant (exit.rs:87-89, "reached >= requested deadline")
  and the trait's late-only-stop freeze (backend.rs:113-114). Latent (test-only, no
  in-tree misuse) but a real fidelity regression in the double; the
  `push_late_landing` doc sentence mischaracterizes it.
- **F3**: the parallel `late_landings` queue duplicates the script's
  `Deadline::reached` channel (~90 LOC + a queue-misalignment hazard).

## Fix (as adjudicated)

Encode lateness in the script entry itself: `run_until` returns
`max(scripted_reached, deadline)` — making `reached < deadline` UNREPRESENTABLE in
the double by construction. Delete the `late_landings` queue and the
`push_late_landing` public method. This preserves every existing gate (the
arbitrary-reached proptests only exercise `run()`, per the adjudication's viability
check — verify that claim yourself and quote which tests you checked).

## Required regressions

1. A script entry with `reached < deadline` produces `reached == deadline` at the
   exit (the max-fold) — the invariant violation is now unrepresentable.
2. A genuinely late entry (`reached > deadline`) still lands late, exactly as before.
3. Every existing MockBackend consumer test stays green unmodified — if any test used
   `push_late_landing`, migrate it to the script-entry form and list the migrations
   in IMPLEMENTATION.md.

## Gates

The mock feature's test suite + the crates that consume the mock (run their nextest
with the feature enabled — identify and list them), clippy `-D warnings`, fmt,
`cargo mutants --in-diff` 0 missed. Public API: `push_late_landing` deletion is a
public-surface REMOVAL on the mock feature — check whether the public-api snapshot
covers feature-gated surface; if it does, regenerate on the pinned nightly and flag
the removal prominently in the PR description (it is the point of the change, not
drift).

## Deliverable

PR from `task/mockbackend-lateness-fold` closing `hm-j16h` with the merge.
