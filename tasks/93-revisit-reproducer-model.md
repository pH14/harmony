# Task 93 — revisit the explorer reproducer-composition model (EnvCodec::compose vs genesis-only)

> **DEFERRED FOLLOW-ON · DO NOT AUTO-SPAWN · LOW PRIORITY.** Revisit **tomorrow / after the
> current queue clears**, and after task 92. This is a design re-validation, not a defect — the
> current model is shipped and coherent; this task exists to confirm (or simplify) it once we have
> implementation signal from task 12.

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
