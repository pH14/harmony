---
name: harmony-properties
description: Derive workload-specific correctness properties and observation criteria from assigned source for Harmony investigations.
---

# Harmony properties

Use this skill when deciding what behavior a workload should expose to Harmony
as a correctness property, progress signal, or diagnostic observation. Begin
with the assigned source, its documented contract, state transitions, failure
handling, and externally visible effects. For every candidate, write the
preconditions, the behavior that should hold, the evidence that can establish
a violation or hit, and the limits of the claim.

Choose an oracle that does not simply read the implementation’s own status.
Depending on the contract, that may be an external checker, an independent
calculation, or a structural invariant. Exercise a valid benign fixture and a
deliberately invalid but well-formed fixture so the checker’s positive and
negative behavior are known. Treat setup, transport, checker-launch, and
execution failures as integration failures unless the contract explicitly
defines them as the property result. A checker that never ran, emitted no
telemetry, or saw no applicable state is unevaluated; it is not a pass.

Make preconditions nonvacuous. Use source-derived reachability or progress
observations to show that the state in which a property applies was actually
visited, while keeping “not observed,” “not reached,” “passed on this path,”
and “not evaluated” distinct. Do not add a fixed property set, oracle,
mutation strategy, or search policy merely because Harmony can represent it.

When representing a property with `harmony-sdk`, choose the point kind from the
source semantics. `assert_always` emits only when its condition is false;
`assert_unreachable` emits a violation when its point is reached;
`assert_sometimes` emits a hit on every satisfied pass; and
`assert_reachable` emits a hit when reached. `state_set` and `state_max` report
raw register operations and values, whose novelty the host interprets.

Keep catalog names unique, point coordinates non-colliding within their event
namespace, local IDs within the SDK’s 24-bit field, and the catalog within one
event payload. Treat lifecycle signals and scheduling handshakes as evidence
of those events, not as an unearned correctness oracle. Finish with a compact
property map containing source location, meaning, preconditions, oracle,
expected report behavior, controls exercised, and known limits.
