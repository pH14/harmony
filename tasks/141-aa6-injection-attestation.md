# Task 141 — AA-6 injection attestation: make the matrix checker structurally injection-aware

**Bead:** hm-oh3v (P2, discovered-from hm-zx3z). Portable, Mac-only. Closes the hole where
an accidentally-injection-OFF bare matrix passes every AA-6 check — today `check_aa6_matrix`
cannot distinguish an injected AA-6 run-set from a bare AA-3 one, so a config slip
(injection silently OFF) would still read PASS. This is the same evidence-integrity class as
the PR-98 "structurally unable to fall back to stock and still pass" lesson.

## Work

1. **Stamp the injection config into `run-set.json`**: which injection mode (inject-ppi /
   inject-at-work), its parameters, and an explicit ON/OFF — written by the harness from
   the *actual* runtime configuration it executed (not echoed from CLI intent).
2. **Per-record `injected` flag**: each sample/record carries whether an injection actually
   fired for it (the non-vacuity witness at record granularity).
3. **Make `check_aa6_matrix` REQUIRE the attestation**: missing stamp ⇒ FAIL (fail-closed,
   never "assume bare"); stamp says OFF for an AA-6 run-set ⇒ FAIL; per-record flags
   inconsistent with the stamped config (e.g. zero fired injections under ON) ⇒ FAIL with
   the counts enumerated.
4. **Coordinate with the just-merged masked-digest lane** (PR #142,
   `spikes/arm-altra/host/aa6-masked-digest-lane.sh` + `aa6-masked-digest-check.py`): that
   checker already enforces its own non-vacuity via `injected_landed_digest` and records
   the injection config in the evidence dir by hand. Route both through the same stamped
   attestation fields so the two checkers cannot disagree about what ran; do not weaken
   either checker's existing requirements.
5. **Negative controls (the hm-537 doctrine)**: fixture-suite cases where a planted
   injection-OFF run-set, a missing stamp, and an inconsistent per-record flag each go RED.
   A checker change without a planted-failure fixture proving it fires is not done.

## Acceptance

- Portable gates green (build + nextest + clippy native & aarch64 + fmt + deny).
- Fixture suite: PASS case + the three planted-failure cases above, all asserted.
- No on-box run required (the ARM box is down); the schema/checker changes are
  forward-compatible with the parked hm-3bwm runbook — update the runbook's artifact list
  if the run-set schema gains fields.

## Scope

Surface: `spikes/arm-altra/**` only (harness run-set emission + `check_aa6_matrix` +
masked-digest lane wiring + fixtures). No consonance/dissonance crates, no docs beyond the
runbook artifact list and comments. Keep the diff attestation-scoped.

## Environment

Mac-local only. No box, no Nimbus. Baseline model (Opus 4.8).
