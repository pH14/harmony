# telemetry — implementation notes

Out-of-band, **read-only** observation tap + a **std-only** web console (live +
replay) for the deterministic VMM (task 29). Leaf crate: no sibling-crate
dependencies. The tap is a no-op by default, never hashed, and carries no
per-host input, so attaching it cannot change a run. Builds and passes every gate
on macOS and Linux; fully testable without KVM (driven by a scripted `Vec<Event>`
over an in-process loopback).

## What was built

- **Schema + wire** (`event.rs`): `Event { seq, work, vns, kind }` and the
  non-exhaustive `EventKind` (every variant the spec lists: `Console`,
  `GuestEvent`, `Io`, `Mmio`, `Hypercall`, `Msr`, `Tsc`, `Rng`, `Cpuid`,
  `Inject`, `Checkpoint`, `Counts(ExitCounts)`, `Terminal`). `ExitCounts` is a
  **local mirror** of vmm-backend's tally (leaf crate, no sibling import). NDJSON
  codec `to_ndjson`/`from_ndjson` (externally-tagged `serde_json`, one line per
  event), round-trip proptested (512 cases, > the ≥256 floor).
- **The seam** (`observer.rs`): the `Observer` trait (`emit(&mut self,
  ev: &Event)` → `()`), `NullObserver` (default, zero-sized no-op), and
  `NdjsonRecorder<W: Write>` (lossless; records the first write error rather than
  panicking, since `emit` returns `()`).
- **Live lane** (`sink.rs`): `LiveSink` — a `Clone` handle over one bounded
  `Arc<Mutex<VecDeque<Event>>>`; `emit` pushes if there's room, else **drops and
  counts** (never blocks). The drop count is surfaced as a synthetic
  `EventKind::Dropped` on the next `drain`.
- **Std-only web server** (`server.rs`): `std::net::TcpListener`, thread per
  connection. `GET /` (embedded UI), `GET /events` (SSE `data: <ndjson>\n\n`,
  fanned out from the `LiveSink` by a single pump thread), `GET /recording`
  (streams a recording file for replay), `GET /config` (one-line JSON telling the
  static page live vs replay). No async runtime, no framework.
- **The `console` bin** (`main.rs`): `clap` args; `--source stdin|unix:<path>|file:<path>`.
- **The UI** (`assets/index.html`): vanilla JS + inline CSS, `include_str!`'d.
  Same renderer for live (`EventSource('/events')`) and replay
  (`fetch('/recording')`): a stdout/stderr pane, a virtualized V-time-scrubbed
  event timeline, live exit-rate counters + a hand-drawn `<canvas>` graph, a
  guest-events pane (id-4), a report-channel pane (`0x0CA2`), and per-`Checkpoint`
  `state_hash` with a `vns`→wall-clock readout. No CDN, no npm, no build step.
- **`docs/INTEGRATION.md` §8**: pins the read-only / default-off / never-hashed
  invariants and the per-exit `EventKind` mapping for the frontier wiring.

### Module layout

`event.rs` (schema + NDJSON wire) · `observer.rs` (`Observer`, `NullObserver`,
`NdjsonRecorder`) · `sink.rs` (`LiveSink`) · `server.rs` (the std-only HTTP/SSE
server + `EventHub` fan-out) · `lib.rs` (crate doc, re-exports) · `main.rs` (the
`console` bin) · `assets/index.html` (the embedded UI).

## Gate-relevant notes (called out per the spec)

- **No new crate deps beyond the whitelist.** Library deps: `serde` + `serde_json`
  (std-only, the entire wire), `thiserror`. Bin-only dep: `clap`, gated behind the
  `cli` feature and the bin's `required-features = ["cli"]` (clap is bins-only on
  the whitelist), so the library pulls no clap. Dev-dep: `proptest`. The server is
  **std TCP only**. No tokio/hyper/axum, no chart/JS library.
- **No npm, no build step.** The UI is a single `assets/index.html` compiled in
  with `include_str!`; it runs offline on the box with no toolchain.
- **`contract_hash` unchanged.** Host-side only; the tap adds no contract rows and
  no per-host input (telemetry mirrors the §1.1 report-channel ruling).
- **Determinism by construction.** `Observer::emit` takes `&Event` and returns
  `()`; no observer has `&mut` access to anything feeding `state_hash`. The
  default is `NullObserver`. The crate proves the structural half; the
  byte-identical-`state_hash`-under-`NullObserver`-vs-`NdjsonRecorder` box check
  is the integrator's (noted in §8), as the run loop is frontier.
- **No determinism-lint hits.** The server uses **no wall-clock** in Rust
  (`Instant::now`/`SystemTime::now` are never called); V-time stamps ride on the
  events, and the only wall-clock readout is drawn browser-side in JS (display
  only, never hashed). No `HashMap`/`HashSet`.

## Key design decisions

- **Two lanes, two guarantees.** `NdjsonRecorder` is lossless (the replay source
  of truth); `LiveSink` is lossy-but-never-blocking. V-time is work-based, so even
  a blocking observer can't perturb the run — drop-don't-block exists purely so a
  live UI adds no wall-clock pause. The authoritative record is always the
  recorder file.

- **One renderer, keyed on `vns`.** Live and replay share the browser renderer.
  Everything is filtered/plotted by `vns` (a pure function of the run), so a
  recording re-renders identically to the live view — that is what makes
  record→replay faithful, and it is why the box can capture while the Mac scrubs.

- **`Dropped` is an additive, telemetry-internal `EventKind` variant.** The spec
  says the live-lane drop count is "surfaced as a synthetic event"; `EventKind` is
  `#[non_exhaustive]`, so adding `Dropped { count }` is additive (no specified
  variant removed/renamed). It is never produced by the frontier wiring and never
  appears in a lossless recording — only the `LiveSink` emits it.

- **`/config` instead of two pages.** The static UI learns live-vs-replay from a
  one-line JSON endpoint, so a single `include_str!`'d page serves both modes
  with no build-time templating.

- **Externally-tagged JSON.** `{"Console":{"text":"…"}}` round-trips losslessly
  (incl. the `[u8;32]` checkpoint hash and arbitrary-UTF-8 console text) and the
  browser reads the variant with `Object.keys(ev.kind)[0]` — no schema library.

- **Accepted sockets forced to blocking mode.** The listener is non-blocking (so
  the accept loop can poll a shutdown flag); macOS/BSD let an accepted socket
  inherit that, which made a handler read racing ahead of the client's request
  return `WouldBlock` and abort the connection under load. `read_request` calls
  `set_nonblocking(false)` so reads honor their timeout uniformly on both
  platforms — no `#[cfg(target_os)]` fork.

## Deviations considered and rejected

- **A per-client backlog as the only queue (no separate `LiveSink` queue).**
  Rejected: the spec wants `LiveSink` testable standalone ("drops-don't-block
  under a full queue, drop count surfaced") independent of any server. So
  `LiveSink` owns its own bounded ring; the server's `EventHub` is a separate
  fan-out so multiple browser tabs each get the full stream from connect-time.
- **Hashing/justifying the tap into the determinism contract.** Rejected by
  design — the whole point is that it is *not* hashed and adds no contract rows
  (§8). Hashing it would couple the frozen contract to an operator convenience.
- **A chart library / WebSocket / async server.** Rejected: the spec mandates
  std-only, no npm, no framework. Graphs are hand-drawn on `<canvas>`; the live
  transport is SSE (`new EventSource(...)`, one line, no library).

## Mutation testing (quality-c)

The first `cargo mutants --in-diff` run left 17 missed + 2 timeouts; all are now
killed by exact-value/observable-post-condition tests, with one documented
equivalent excluded:

- **The web server made testable by factoring, not by I/O.** `read_request` now
  delegates to a generic `parse_request<R: BufRead>`, unit-tested against a
  `Cursor` that asserts the header drain stops **exactly** at the blank line
  (leaving the body) — pinning the `n == 0 || == "\r\n" || == "\n"` boundary. The
  idle/keepalive counter is a pure `advance_idle(idle) -> (u32, Option<&[u8]>)`,
  unit-tested across the cadence (kills the `+= 1` and the `>=` boundary without
  waiting real time). `EventHub` is unit-tested directly (same-file `mod tests`):
  publish delivers to every subscriber, and after `unsubscribe` the removed
  client stops receiving while the rest keep receiving (kills `push`/`drain`/
  `unsubscribe` and the `!Arc::ptr_eq`). Lifecycle: `dropping_the_server_stops_the_listener`
  asserts a dropped/shut-down server stops accepting connections (kills `stop`/`drop`).
- **`is_empty`/`capacity`/`flush` pinned to exact values.** A capacity of `7`
  (distinct from the `0`/`1` a mutant returns); `is_empty()` asserted both true
  (fresh) and false (after one event); a `DeferredWriter` proves
  `NdjsonRecorder::flush` actually forwards (staged bytes only become visible
  after the flush call).
- **Two equivalences removed by simplification rather than excluded.** The accept
  loop's `WouldBlock` match-guard arm did the same `sleep(POLL)` as the generic
  error arm, so the guard was meaningless — the arms are collapsed into one
  (deletes the three guard mutants). `serve_recording` now streams via `io::copy`
  instead of a hand-rolled `[0u8; 64 * 1024]` loop, deleting the equivalent
  `64 * 1024` → `64 + 1024` mutant (any non-zero chunk size yields identical
  output).
- **One genuinely-equivalent mutant excluded** in `.cargo/mutants.toml` (entry
  (i)): `RunningServer::shutdown -> ()`. `shutdown(mut self)` only calls
  `self.stop()`, and dropping `self` runs `Drop::drop`, which calls `self.stop()`
  too (idempotent) — so an empty `shutdown` body has the identical observable
  post-condition. `shutdown` is by design just an eager `drop`; the `stop`/`drop`
  bodies stay mutation-gated.

## Known limitations (for the integrator)

- **Live is from-subscribe-onward.** A browser that connects after the run starts
  sees only events emitted from its connect time (standard SSE). The lossless
  history is the recorder file → `/recording` replay; open the console *before*
  starting the run for a full live view. (For a demo, attach an `NdjsonRecorder`
  too and load it via `/config { hasRecording: true }`.)
- **Per-client SSE backpressure drops oldest silently.** If one browser falls
  >16k events behind, its own oldest buffered events are dropped to keep it at the
  live edge. The surfaced drop notice is the `LiveSink`-level one (the run's lossy
  lane); per-client backpressure is best-effort and not separately reported.
- **`--source unix:` is Unix-only** (`std::os::unix::net::UnixListener`). Both
  supported platforms (macOS, Linux) are Unix; there is no Windows target in
  scope, so this needs no `cfg` fork.
- **The `vmm-core` per-exit wiring is frontier and not in this crate** (§8). This
  task delivers the crate, the sinks, the bin, and the browser, all driven in
  tests by a scripted `Vec<Event>` with no KVM.
