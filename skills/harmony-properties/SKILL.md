---
name: harmony-properties
description: Turn an application's documented guarantees into properties Harmony can check. Use when preparing an application for Harmony, deciding what to assert, or reviewing a set of proposed checks. Covers the three kinds of claim, choosing independent oracles, stating assumptions and preconditions, and the traps that produce checks which cannot fail.
---

# Choosing properties

A property is a claim about the application that a check can evaluate during a
run. Harmony explores executions; properties decide what counts as a bug.

Start from the feature's own contract — its documentation, its interface, the
invariants it promises callers. Do not start from the source of a suspected
defect, and do not write checks around a mechanism you have not confirmed.

## Three kinds of claim, three different meanings

| Kind | Claim | A run with no report means |
| --- | --- | --- |
| `always` | This must hold every time it is evaluated | The check never ran, or it ran and passed. You cannot tell which |
| `sometimes` | This must hold at least once across the campaign | The situation was never reached; the campaign did not test what you meant |
| `reachable` | This point must be reached | The point was never reached |

Write them down separately. "The index is complete" is a safety claim.
"A concurrent build finished while updates were running" is a reachability
claim about whether the interesting situation happened at all. A campaign that
never reached the second says nothing about the first.

A failure-only check that stayed silent is unevaluated, not satisfied.
`harmony -w W inspect` lists properties a point holds no verdict for, for
exactly this reason. Plan for at least one `sometimes` or `reachable` claim per
`always` claim, so silence can be told apart from success.

## An oracle must not share the bug

A check that reads the same data structure the feature maintains agrees with it
by construction. Prefer, in order:

1. An external checker the application already ships that reads the artifact
   independently, such as a consistency checker or verifier command.
2. A second computation of the same answer by a different route, compared
   against the first.
3. A structural invariant that holds regardless of how the feature is
   implemented — counts, orderings, referential closure.

Reading a status flag the feature sets is not an oracle. Neither is a check
that passes whenever the application returns success.

## State the preconditions and exercise them

Every property has conditions under which it applies. Write them as text beside
the property, and write a `reachable` or `sometimes` claim for each one you
need. If a property only applies while two operations overlap, the campaign
must be able to make them overlap, and you must be able to show that it did.

Say plainly which claims you are not making. A property list that covers the
part of the feature you understood is useful. A list that implies coverage you
did not achieve is not.

## Prioritize

Rank by what a violation would cost and how likely a bug is to hide there:
silent data corruption above a crash, a crash above a hang, a hang above a
performance change. Put the cheap checks that can run often ahead of expensive
ones that can only run at the end.

An expensive checker is still worth having — it becomes a hook the search runs
at chosen moments rather than continuously. See [`harmony-instrument`](../harmony-instrument/SKILL.md).

## Traps

- **A check that cannot fail.** Before trusting a check, make it fail on
  purpose against a deliberately broken input and confirm Harmony reports it.
- **Asserting the implementation.** Claims about internal state break when the
  implementation changes and say nothing about the contract.
- **Changing the application to make a check pass.** Preserve the application's
  semantics. A check exists to observe behavior, not to alter it.
- **One id for two claims.** Each property gets one stable id whose meaning
  never changes. Reusing an id makes every recorded finding ambiguous.
- **Timing as a property.** Wall-clock thresholds do not survive deterministic
  virtual time. Express progress as a bound on work, not on seconds.

## What you leave behind

A prioritized list where each entry has: the claim, its kind, its assumptions,
its preconditions, how it will be evaluated, and what the check reads that the
feature does not maintain.
