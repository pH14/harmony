# Reviewing Harmony

Start with the change's stated intent and select only the review lenses that
match the affected code. Report concrete scenarios with file and line
locations; a review with no substantive findings should say so plainly.

## General correctness

Confirm that the change implements its stated behavior, handles relevant edge
cases, preserves unrelated behavior, and is exercised by an appropriate test.

## Deterministic execution

Use this lens when a change affects state transitions, ordering, clocks,
scheduling, entropy, serialization, snapshots, or observable output. Trace each
new input to the state it can influence and confirm that replay reconstructs the
same value and ordering. Use executable comparisons or planted differences when
they make the claim materially stronger.

## External input

Use this lens for decoders, transports, host responses, and externally supplied
lengths, indexes, tags, or enum values. Follow each value through arithmetic,
allocation, indexing, and conversion, and confirm malformed inputs produce a
normal error rather than a panic or partial state transition.

## Compatibility

Use this lens when public APIs, wire frames, snapshot bytes, persisted records,
or machine-readable contracts change. Identify every producer and consumer,
make version behavior explicit, and verify that the relevant golden or
round-trip checks observe the change.

## Unsafe code

Use this lens whenever `unsafe` code or one of its assumptions changes. Check
the local safety argument against alignment, lifetime, aliasing, initialization,
bounds, and platform requirements. Exercise the unsafe logic under Miri through
an interpreter-reachable path.

## Test strength

Use this lens when a change adds or alters a regression gate. Demonstrate that
the test observes the behavior it names, including a planted failure when a
green result could otherwise be vacuous. Treat coverage as reachability
evidence rather than proof of assertion strength.
