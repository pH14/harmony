# Process protocol

`process-proto` owns the generic process fault target and standing-window wire
contract. It has no workload or policy dependencies. The target starts with a
little-endian node id and uses the established action tags for pause, kill,
restart, hooks, instruction parking, and instrumented event kill or park.
Standing responses use a little-endian moment, count, and `(class, target,
start, end)` windows.

The `events` module owns the fixed control and report frames exchanged between
the supervisor and an optional instrumented process runtime. It validates the
shared 0 through 63 rarity width, exact command acknowledgements, runtime hello,
kill provenance, and event-park status without naming a workload policy.

The `registers` module owns the reserved lifecycle-register IDs published by
the platform supervisor and decoded by workload policy. Keeping these IDs with
the process wire contract prevents platform and workload views of lifecycle
progress from drifting apart.

Workload policy crates translate their semantic fault enums to
`ProcessAction`; the platform supervisor consumes the generic action and window
types. Invalid targets are ignored when filtering a standing response so an
unrelated or future decision class cannot stop process supervision.

```sh
cargo test -p process-proto
cargo clippy -p process-proto --all-targets -- -D warnings
```
