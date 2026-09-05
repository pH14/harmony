# telemetry

`telemetry` provides a read-only observation tap and a small standard-library
web console for the deterministic VMM. It observes events already produced by
the VMM; it does not feed data back into guest state or participate in state
hashes. The per-exit VMM wiring is owned by `vmm-core`.

## Events and observers

`Event` carries a per-run sequence, exit count, V-time stamp, and an
`EventKind`. Kinds cover console and guest events, I/O and MMIO, hypercalls,
MSRs, TSC/RNG/CPUID activity, interrupts, checkpoints, exit counts, and
terminal status. The enum is non-exhaustive so producers can add observations
without requiring consumers to assume a closed set. `to_ndjson` and
`from_ndjson` provide the externally tagged JSON/NDJSON representation.

`Observer::emit` receives a shared event after an exit has been serviced.
`NullObserver` is the no-op default. `NdjsonRecorder` writes one lossless NDJSON
line per event; write failures are retained for the caller to inspect after the
run and do not panic from `emit`.

`LiveSink` is a cloneable, bounded, never-blocking observer for live display.
When full it drops and counts events, then emits a synthetic `Dropped` notice
on the next drain. The lossless recorder remains the replay source of truth.

## Web console

`serve` runs a std-only TCP server with an embedded browser page. It serves `/`
for the UI, `/config` for mode metadata, `/recording` for an optional NDJSON
file, and `/events` as Server-Sent Events. Each connected event stream has a
bounded client backlog and is unsubscribed when the connection ends.

The `console` binary accepts `stdin`, `unix:<path>`, or `file:<path>` sources,
binds an HTTP address, and either displays the live sink or replays a recording.
The Unix socket source is intentionally Unix-only; the library itself is
otherwise standard-library based.

The observer interface is read-only, so attaching telemetry cannot advance
V-time, draw entropy, or mutate VM state. Event/NDJSON, sink, server loopback,
and CLI tests cover ordering, bounded queues, replay, and HTTP/SSE behavior.
