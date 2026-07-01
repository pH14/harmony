# Task 93 — revisit the explorer reproducer-composition model (EnvCodec::compose vs genesis-only)

> **✅ RESOLVED (2026-07-01, PR #39).** Ruling: **keep `EnvCodec::compose`; genesis-only rejected**
> — see `docs/DISSONANCE.md` §"Ruling (task 93)", which also pins the four-point adapter contract
> (tail-completeness, blob-carried branch offset, panic-on-`UnsupportedComposition`, standing-fault
> confinement + sequencing guard). Note the un-defer paragraph below reads as *favoring*
> genesis-only; the ruling found the opposite for a code-level reason it had missed — corpus bases
> and deltas are always post-run `Recorded` artifacts, so `compose`'s fail-closed `Seeded` path is
> vacuous in the campaign flow. The header is preserved as the historical input to the ruling; do
> not implement from it.

> **UN-DEFERRED (2026-07 review) · RESOLVE BEFORE TASK 58.** Originally parked as low-priority
> pending implementation signal from task 12. The 2026-07 review (`docs/REVIEW-2026-07.md` gap #4)
> produced that signal early, and it is worse than this file anticipated: the two `EnvCodec`s do
> not bind — `explorer::EnvCodec::compose(base, branch_local)` is 2-arg and infallible
> (`dissonance/explorer/src/seam.rs:110`) while `environment::EnvCodec::compose(base, tail, at)`
> is 3-arg and fallible (`dissonance/environment/src/envcodec.rs:140`), and the real `compose`
> **fails closed on exactly the common cases** — `Seeded` bases and standing faults
> (`envcodec.rs:142-150`) — which the explorer's `SeedStrategy` produces by default. So the
> explorer's genesis-complete-reproducer gate has only ever passed against its toy codec. That is
> strong evidence for the **genesis-only** alternative below (delete `compose`). Task 58 must not
> bind the seam before this ruling lands; task 92 is no longer a prerequisite.

Read `docs/DISSONANCE.md` and `tasks/12-explorer.md` first.

## Context — the decision that was made

PR #46 (the dissonance design) went through a long cross-model hardening pass. One **architectural
question** was escalated to the integrator: how does the explorer produce a **genesis-complete**
bug reproducer when a bug is found below a *non-genesis* corpus snapshot? `Machine::recorded_env`
returns answers *since the last branch* (branch-local), and the explorer is schema-blind.

The integrator was away; per the stated default I applied my **best-guess resolution** and merged:

- **Chosen model:** corpus envs are genesis-complete; snapshots are pure perf-caches; the explorer
  composes a base env with a branch-local delta via **`EnvCodec::compose(base, branch_local)`**
  (re-indexing the delta's decision IDs onto the base) to mint the portable `Bug.env`. `EnvCodec`
  also has `seeded`/`mutate`; bound at integration to task 24's `EnvSpec` codec.
- **Alternative considered (rejected for now):** **genesis-only branching** — never branch from a
  non-genesis snapshot, so every env is genesis-complete by construction and **no `compose` is
  needed**. Simpler, but it throws away the snapshot-tree speedup the Multiverse design wants.

## What to revisit

- **Update (`docs/DISSONANCE.md` ruling):** the single `Moment` axis (retired-instruction count) now
  carries *both* host- and guest-plane overrides, so `compose`'s re-keying is one-axis arithmetic on
  `Moment` rather than a cross-plane merge — factor this in to the questions below.
- Is `EnvCodec::compose` actually pulling its weight once task 12 is implemented, or does
  genesis-only branching (drop `compose`) give a simpler engine with acceptable performance?
- Does `compose`'s decision-ID re-indexing have a clean, well-defined semantics in practice, or
  does it create replay edge cases (re-keyed overrides colliding, etc.)?
- Empirical signal: with a real coverage-guided campaign, how often are bugs found below
  non-genesis snapshots (the case `compose` exists for)? If rare, genesis-only may win on
  simplicity.

## Output

A short ruling in `docs/DISSONANCE.md` (and a `tasks/12-explorer.md` adjustment if the model
changes): keep `compose`, or switch to genesis-only and remove it. Either way, the reproducer
must stay **genesis-complete and portable** (SnapIds are ephemeral — that invariant is not up for
revisiting). If `compose` stays, add the property test that `branch(genesis, compose(base, delta))`
reproduces the run that produced `delta`.

## Non-goals

Re-opening the broader dissonance design (settled in #46); changing the genesis-complete /
portable-reproducer invariant. Low priority — do not start until task 92 and the active queue are done.
