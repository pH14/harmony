# Process protocol

`process-proto` owns the generic process fault target and standing-window wire
contract. It has no workload or policy dependencies. The target starts with a
little-endian node id and uses the established action tags for pause, kill,
restart, hooks, instruction parking, and instrumented event kill or park.
Standing responses use a little-endian moment, count, and `(class, target,
start, end)` windows.

The `events` module owns the fixed control and report frames exchanged between
the supervisor and an optional instrumented process runtime. It validates the
shared 0 through 63 kill rarity width, the park edge-count range, the optional
park target (a nonempty range of module offsets), exact command acknowledgements, runtime hello,
kill provenance, event-park armed and held status, and bucketed edge-coverage status
without naming a workload policy.

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

## Guest debug terminal

The `debug` module owns the bounded namespace-10 command protocol: append script
bytes, execute, open shell, send input, close input and resize a PTY. Messages have
monotonic nonzero sequence numbers; the guest acknowledges and deduplicates them.
Script buffers are limited to 1 MiB and individual payloads to 3000 bytes. Output
and changed status arrive through dedicated SDK events. The protocol contains no
workload-specific fault or controller actions.

Supervised-process service handlers answer an inactive debug poll with empty
data (or a nominal response) without advancing their standing-action schedule.
The poll carries a `debug::Status`, independently of the standing request.

Event protocol version 6 additionally reserves bit 31 of the park selector for an
exact, nonzero 31-bit application marker. Other selectors remain weighted edge
counts through `EVENT_PARK_EDGE_LIMIT`. Exact markers fire once per arm and ignore
hotness and module-offset targeting. This uses the existing control frame; the
versioned hello prevents an older runtime from silently interpreting the selector
as an ordinary edge budget.
