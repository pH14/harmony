# Define what must remain true

An assertion turns an observation into a test. Prefer properties about accepted
work and externally visible results: an acknowledged record stays readable,
a completed transfer preserves the total balance, or a committed value is not
silently replaced by an older one.

The counter tutorial checks **the counter holds every finished increment**.
Each writer records its completed increments separately, then compares their sum
with the shared counter. The condition can pass or fail depending on how the
execution unfolds.

## Correctness and reachability answer different questions

An **Always** assertion describes a property that must hold whenever checked.
A **Sometimes** assertion describes a condition the search should reach. An
Always assertion that never executes says nothing about the application.
Pair correctness checks with evidence that the interesting operation happened.

Put checks where their inputs form a meaningful observation. A checker that
reads mutually inconsistent intermediate values can report a bug in its own
sampling logic. A check that runs only before a fault can miss recovery failures.

## Choose how to report

Use the language SDK to report properties from application code, or a workload
checker to examine application state. Keep stable property identities and include
enough evidence to understand the failure. The
[SDK implementation and language integrations](https://github.com/pH14/harmony/tree/main/consonance/harmony-linux/sdk)
are the source for API details.

Harmony deliberately pauses, kills, and restarts processes during a fault search.
A killed process is not itself evidence of an application bug. The violated
property is what makes the disturbance interesting.

Before increasing a search budget, confirm that the properties are reached and
that the checker observes recovery, not just startup.
