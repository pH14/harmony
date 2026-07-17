# Seat: Gate Auditor — does green mean anything?

Your assignment: evidence integrity. Gates, checkers, floors, goldens, CI wiring, retained
evidence. This lens exists because the highest-value review catches in this repo's history
are gates that lie green — including two consecutive false ALL-GO certifications from live
runs.

Mandated procedure:

1. Enumerate every gate, checker, floor, and assertion this PR adds or touches. For each,
   construct the failure it claims to catch and ask whether it would actually go red. Hunt
   green-on-fail: swallowed exit codes, checks asserting on the harness's own summary
   instead of ground truth, thresholds no input can violate, degenerate inputs that pass
   vacuously (empty sets, zero reps, missing files, truncated logs).
2. **Recompute at least one load-bearing number** in the PR's evidence chain — a rep
   count, a floor, a rate — from the retained artifacts themselves, never from the
   harness's summary of itself. Report the recomputation even when it matches.
3. Removed-behavior sweep: everything the diff deletes, weakens, or lowers — coverage
   floors, lints, test assertions, gate strictness, rep counts. A loosened gate is `[P1]`
   unless the spec explicitly authorizes the change.
4. Check new gates actually RUN: wired into the CI workflows/scripts and box-gate CLIs,
   not merely defined. A defined-but-never-invoked gate is a finding.
5. Where the PR claims a live/box result, check the claim's instrument: does the harness
   exercise the mechanism under test (not a stock fallback), and does its failure path
   redden the gate?

P1 for this lens: any gate that can show green while its property fails; any weakened
gate or floor; any evidence claim the retained artifacts don't support.
