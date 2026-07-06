# Task 85 — run the EnvCodec::compose rekeying flake to ground (issue #72)

> **Quality / determinism triage.** During #69 coverage work (PR #71), a proptest in
> `dissonance/environment/tests/envcodec.rs` failed **intermittently** on the compose
> rekeying path (fault-schedule offsets at particular positions), then passed clean on 3
> reruns after clearing proptest's auto-generated regression file. `EnvCodec::compose` is
> the determinism-critical genesis-complete reproducer path (task 78): an intermittent
> failure there is either (a) a flaky/underspecified test or (b) a **real rare
> non-determinism in compose's rekeying** — and (b) is exactly the class of bug this
> system exists to catch (a reproducer that doesn't reproduce). This task decides which,
> with evidence, and fixes what it finds. See issue #72 for the original report.

Read first: `tasks/00-CONVENTIONS.md`, issue #72 (`gh issue view 72`),
`dissonance/environment/tests/envcodec.rs` (the compose gate — especially
`compose_rekeys_overrides_at_any_offset` and its siblings),
`dissonance/environment/src/envcodec.rs` (`compose`, the rekeying arithmetic),
`dissonance/environment/src/envcodec_proofs.rs`, and
`tasks/78-reseed-aware-compose.md` (what compose promises).

## Environment

Fully macOS-portable — pure logic + proptests; no box, no KVM, no network. Do not touch
the box.

## Surface (hard rule 1)

- `dissonance/environment/tests/**` — test changes (regression cases, generator fixes,
  case-count tuning).
- `dissonance/environment/src/envcodec.rs` (+ `envcodec_proofs.rs`) — **only if** a real
  counterexample proves a compose/rekeying bug; the fix must be minimal and the
  counterexample must land as a permanent regression test first.

Nothing else. Do not refactor neighboring code.

## The work

1. **Hunt the counterexample.** Run the envcodec proptests with the cached regression
   seeds removed and a high case count (e.g. `PROPTEST_CASES=100000`, several distinct
   `PROPTEST_RNG_SEED`s / repeated runs). Target the compose/rekeying properties
   specifically. Capture any failing seed + minimal input verbatim — the concrete
   counterexample is the whole value of this task.
2. **Classify.**
   - **Real bug**: the counterexample shows `compose` violating its contract (prefix
     preserved at its Moments, tail rekeyed by exactly the offset, seed/policy carried,
     standing-fault rejection, or replay bit-identity). Land the counterexample as a
     deterministic `#[test]` regression (concrete inputs, like
     `compose_rekeys_at_nonzero_concrete`), then fix compose minimally until the full
     suite + the new test are green.
   - **Flaky test**: the generator or property is underspecified (e.g. an implicit
     disjointness/ordering assumption the generator can violate rarely). Fix the *test*
     to state the property correctly — never weaken the property to make it pass; if the
     "fix" would weaken what compose promises, STOP and report instead (that means it's
     a real bug in disguise).
   - **No reproduction** after an honest hunt (document the exact commands, case counts,
     seeds, and total cases run — order of ≥10⁶ cases across runs): write the negative
     result into the PR description and add a comment on issue #72; keep any hardening
     that falls out naturally (e.g. pinning the generator's assumptions with explicit
     `prop_assume`/doc comments), but do not invent changes.
3. **Determinism cross-check.** Whatever the outcome, run the full
   `dissonance/environment` test suite (including `determinism.rs`, `replay.rs`,
   `golden.rs`) green at least twice back-to-back to confirm no residual flake.

## Gates

- `cargo test -p environment` (or the crate's actual package name) fully green, twice
  consecutively, with the regression file present.
- The targeted proptests green at elevated case count (document the count used).
- If a bug was fixed: the new concrete regression test fails on the pre-fix code
  (demonstrate in the PR description) and passes post-fix.
- Standard repo gates (fmt, clippy) on the touched crate.
- Never weaken a property, delete a proptest, or lower default case counts to get green.

## Deliverable

Branch `task/compose-rekey-flake`, PR titled for issue #72 with: the classification
((a)/(b)/no-repro) stated in the first line, the evidence (seed, minimal input, or the
hunt log), and `Closes #72` only if classified (a real fix or a test fix); a no-repro
outcome comments on #72 but leaves it open.
