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

`cargo mutants --no-shuffle --in-diff <(git diff origin/main...HEAD)` (CI's exact
command) reports **0 missed, 0 timeout** (60 caught, 12 unviable). The first run
left 17 missed + 2 timeouts; all are now caught by exact-value/observable-post-
condition tests, with one documented equivalent excluded:

- **The web server made testable by factoring, not by I/O.** `read_request` now
  delegates to a generic `parse_request<R: BufRead>`, unit-tested against a
  `Cursor` that asserts the header drain stops **exactly** at the blank line
  (leaving the body) — pinning the `n == 0` / `== "\r\n"` / `== "\n"` boundary.
  The parser is **bounded** (`Read::take(MAX_REQUEST_BYTES)`) and fails closed on
  EOF/bound-before-terminator, so the `|| → &&` terminator mutant (which made the
  drain loop spin on EOF — the CI **timeout** that counts as not-caught) now runs
  to the bound and returns a fast `Err` the `Cursor` test asserts. That bound is
  also a real robustness win: an unbounded HTTP request read is a DoS vector. The
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

## Deflaking `streams_events_as_sse_frames` (task 143, hm-ftok; follow-ups task 147, hm-3r2k/hm-gfi2/hm-gnxr)

`server::tests::streams_events_as_sse_frames` panicked once on PR #138's `gates`
run (run `29887364236`, diff-unrelated) and passed on re-run; the recoverable log
only shows that passing re-run, so the failing assertion is unambiguous from the
source alone — `assert!(frame.contains("data: "), "SSE data prefix: {frame:?}")`
— the read of the first SSE frame came back with no `data:` line, not merely a
late one. One observed occurrence is evidence the race exists; it supports no
failure-rate claim.

### The race (a lost frame, not a slow one)

The old `serve_events` wrote and flushed the response header **before** calling
`hub.subscribe()`. `EventHub::publish` fans an event out only to the subscribers
present *at that instant* — there is no replay for a connection that subscribes
later — so a client whose header read completes before the subscribe runs can
have its first event published to nobody and dropped: a genuine lost frame, not
a late one, and no amount of test-side waiting could have recovered it.

The fix (see the `serve_events` rustdoc above) reorders to **subscribe before
announcing the stream**: `hub.subscribe()` runs before a single response byte is
written, establishing the missing happens-before (the header flush is a release
the client's header read acquires, so the subscribe happens-before any event the
client emits in response). The SSE wire format is unchanged — the header bytes
and every `data: <ndjson>\n\n` frame are byte-identical to before.

### Was this a real telemetry bug?

It was a genuine losable-event defect in the live SSE path — not a protocol or
framing bug; no frame was ever split or corrupted, and delivery order was
preserved — and the in-PR reorder is a proportionate fix for it: a real, if
narrow, loss window closed at zero wire-format cost.

### The narrower follow-up race (hm-3r2k) and the helper (hm-gfi2)

The deflake's phase-2 wait (`read_until(&mut c, "\n\n")`) is also satisfiable by
the periodic `: keepalive\n\n` comment frame — a narrower, load-dependent
re-opening of the same class of race. `streams_events_as_sse_frames` now loops
past any frame that doesn't contain `data: ` so the assertion phase only starts
once a genuine event frame is present.

`announce_and_stream` had exactly one caller; per task 147's decision rule (no
second caller in-tree ⇒ inline rather than build speculative generality), it is
folded back into `serve_events`.

### Verification

- `cargo nextest run -p telemetry --all-features` → 28/28 pass.
- Stress (direct test-binary invocation, no cargo overhead per run):
  `streams_events_as_sse_frames` looped **500×** and the `server_loopback`
  integration test `serves_html_and_streams_a_scripted_run_in_order` looped
  **200×** — both **0 failures** (exceeds PR #146's 200×/100× record).
- Portable gates green: build, nextest, clippy (`-D warnings`, exit 0 — the three
  `clippy.toml` `rand::*` "unreachable" notices are pre-existing workspace-config
  warnings, not from this crate), fmt `--check`, `cargo deny check`.

## Bounded, cumulative phase-2 wait (task 149, hm-38kv)

PR #149's adjudication (judge-CONFIRMED, empirical repro) found the hm-3r2k
retry loop above — `while !frame.contains("data: ") { frame = read_until(&mut
c, "\n\n"); }` — still had three live defects, all rooted in the same
structural flaw: each `read_until` call starts a fresh accumulator and the
outer loop just replaces `frame`, so nothing is preserved across attempts and
nothing bounds the number of attempts.

- **F1a — unbounded outer loop.** If the server regresses and never emits a
  `data: ` frame, the `while` has no attempt/deadline budget of its own: with
  keepalives arriving it retries forever (hangs to the CI job timeout, since
  there is no per-test timeout configured); on a closed connection
  `read_until` returns `""` instantly, so the same loop hot-spins (observed:
  10k iterations in 19ms).
- **F1b — accumulation lost across attempts.** A single `read()` returning
  `": keepalive\n\ndat"` already contains a `"\n\n"` (the keepalive's own
  terminator), so that call returns immediately — discarding the trailing
  `"dat"`, which was the start of the *next* frame's `"data: "` marker. The
  bytes are gone; the connection hangs permanently waiting for a marker that
  will never appear again (empirically confirmed).
- **F1c — a substring check mistaken for frame completeness.** The outer
  `while` condition tests `frame.contains("data: ")`, not "does `frame` end in
  a terminated `data: …\n\n` frame". A read that stops exactly after the
  `"data: "` marker (before the JSON payload or the terminator arrives)
  already satisfies that condition, so the loop exits with an incomplete
  frame and the payload assertions flake.

**Fix: one structural closure, not three patches.** The retry is replaced with
a pure frame-scanner, `extract_data_frame(buf: &[u8]) -> Option<String>`, plus
one bounded I/O loop, `read_sse_data_frame`, that accumulates into a single
buffer across attempts (never resets it) and calls the scanner after every
read:

- `extract_data_frame` walks `buf` frame-by-frame (each `"\n\n"`-terminated
  span), skipping any span that isn't `data: `-prefixed (comments/keepalives),
  and returns the first complete data frame it finds. It returns `None` — not
  a false match — when the buffer ends mid-frame, whether that's right after
  `"data: "` (closes F1c) or partway through a comment frame whose terminator
  hasn't arrived. Because the *caller* owns the accumulator and only appends
  to it, a marker split across two reads recombines correctly on the next
  scan instead of being discarded (closes F1b).
- `read_sse_data_frame` bounds the whole wait to a fixed attempt budget (same
  200 ms-timeout / 50-attempt shape as the pre-existing `read_until` helper,
  ≤10 s worst case) covering every attempt, not per-`read_until`-call as
  before, so a stalled regression fails fast with a panic that prints the
  accumulated bytes instead of hanging or hot-spinning (closes F1a).

**F2 (folded): deterministic keepalive-skip coverage.** The skip-a-comment-
frame branch had zero positive coverage — `KEEPALIVE_EVERY(600) ×
POLL(5 ms) = 3 s` of real idle time is needed before the live server ever
emits a keepalive, and the test emits its scripted event immediately, so the
branch never ran in a normal suite. Waiting out 3 s of wall-clock per run to
force it was rejected (slows the suite by seconds for one branch, and the
result would still be timing-dependent/flaky under load). Extracting the scan
into the pure `extract_data_frame` makes the branch trivially and
deterministically testable against a synthetic byte buffer with no socket, no
real server, and no wall-clock dependency at all:
`extract_data_frame_skips_a_leading_keepalive` feeds
`b": keepalive\n\ndata: hello\n\n"` directly and asserts the keepalive is
skipped — exercising exactly the branch F2 asked for, in under a millisecond.
Companion unit tests pin the rest of the scanner's contract directly (immediate
frame, several stacked keepalives, an unterminated marker, comments-only-so-far,
and the F1b split-marker recombination) — all pure, all synchronous.

### Verification

- `cargo nextest run -p telemetry --all-features` → 34/34 pass (28 prior + 6
  new `extract_data_frame` unit tests).
- Stress (direct test-binary invocation, release profile, no cargo overhead
  per run): `streams_events_as_sse_frames` looped **500×**, matching PR #149's
  record — **0 failures**.
- Portable gates green: build, nextest, clippy (`-D warnings`, exit 0 — same
  three pre-existing `clippy.toml` `rand::*` notices as before, not from this
  crate), fmt `--check`, `cargo deny check`.
- Diff scoped entirely to `consonance/telemetry/src/server.rs`'s `#[cfg(test)]
  mod tests` — no server, dependency, wire-format, or public-API change.

## Scanner hardening: generic `io::Read` + phase-1 anchor (task 151, hm-b5km, hm-8c5m)

PR #152's adjudication parked two follow-ups from the bounded-wait work above.

**hm-8c5m — phase-1 header anchor.** `streams_events_as_sse_frames` waited for
phase 1 (the response header) with `read_until(&mut c, "text/event-stream")`.
That token appears mid-header, before the blank-line terminator, so the wait
could in principle be satisfied with a residual header tail (e.g.
`Cache-Control: …\r\n\r\n`) still unconsumed ahead of the data-frame scanner —
closing over that hole given the emit-after-phase-1 ordering (the scripted
event isn't emitted until phase 1 returns, so the trigger isn't realizable
today, per the bead's own note, but the anchor should be the actual boundary,
not a token that happens to precede it). Fixed by anchoring on `"\r\n\r\n"`,
the real header terminator; both existing `head.contains(...)` assertions
(`"text/event-stream"`, `"no-cache"`) still hold since the full header is
still captured.

**hm-b5km — genericize `read_sse_data_frame` over `io::Read`.** The helper
took `&mut TcpStream` and called `set_read_timeout` on it directly, which
meant its cross-read accumulation (the fix for F1b above) could only be
driven through a real socket — no unit test exercised the "acc retained,
never cleared, across reads" contract in isolation, so a mutant that clears
`acc` between reads (or drops the `0..50` attempt bound) stayed green on the
common coalesced-read path, catchable only via adverse interleaving with the
server's multi-`write_all` frame emission.

Fix: generic over `R: io::Read`; timeout setup moves to the caller (the sole
production call site, in `streams_events_as_sse_frames`, now calls
`c.set_read_timeout(...)` itself immediately before `read_sse_data_frame(&mut
c)` — a bare `io::Read` has no read-timeout notion of its own). Two scripted
tests drive it directly against a mock reader, no socket, no real server, no
wall-clock wait:

- `read_sse_data_frame_retains_bytes_across_reads` — `(&b": keepalive\n\ndat"[..]).chain(&b"a: hello\n\n"[..])`
  (std's `Read::chain`, exhausting the first slice before switching to the
  second) returns `": keepalive\n\ndat"` (a complete keepalive frame plus the
  start of the *next* frame's `"data: "` marker, split mid-word) on the first
  read and `"a: hello\n\n"` on the second, and asserts the combined result is
  the complete `"data: hello\n\n"` frame. If `acc` were cleared between reads,
  the `"dat"` prefix would be lost and the second read alone (`"a: hello\n\n"`,
  which does not start with `"data: "`) would never be recognized as a data
  frame — directly catching the mutant PR #152 flagged.
- `read_sse_data_frame_panics_with_accumulated_bytes_on_budget_exhaustion` — a
  `NeverDataReader` returns the same `": keepalive\n\n"` comment frame from
  every `read` call, forever (never `Ok(0)`, never a `data: ` frame), so the
  fixed `0..50` attempt budget is what ends the loop, not an EOF break. The
  test asserts (`#[should_panic(expected = "keepalive")]`) that this panics
  with the accumulated bytes visible in the message — pinning both the
  budget bound itself (drop it and this hangs instead of panicking) and the
  diagnosability the panic message exists for (hm-3r2k, hm-38kv).

**Rejected: the optional `split_inclusive` reshape (PR #152 F-E).** Considered
while genericizing, per the bead's "take it only if it stays net-simpler"
framing. `slice::split_inclusive` splits on a predicate over single elements;
`"\n\n"` is a two-byte delimiter, so expressing the frame walk with it would
need pairing/lookahead logic no simpler than the existing `windows(2).position`
loop. Left as-is.

**PR #154 review follow-ups.** A dedicated `ScriptedReader` type (struct +
constructor + `Read` impl, ~28 LOC) originally drove the cross-read-retention
test; it had exactly one user and is equivalent to chaining two `&[u8]`
readers with std's own `Read::chain`, so it was deleted in favor of the
inline `chain` call above. The phase-1 header-anchor test now also asserts
`head.contains("\r\n\r\n")` directly (previously only `"text/event-stream"`
and `"no-cache"` were asserted), so a server regression that omitted the
terminator would fail fast instead of silently passing once `read_until`'s
attempt budget exhausts and returns its partial accumulator unconditionally.

### Verification

- `cargo nextest run -p telemetry --all-features` → 36/36 pass (34 prior + 2
  new `read_sse_data_frame` scripted-read tests).
- Stress (direct release-profile test-binary invocation, no cargo overhead per
  run): `streams_events_as_sse_frames` looped **250×** — **0 failures**
  (exceeds this task's ≥200× floor).
- Portable gates green: build, nextest, clippy (`-D warnings`, exit 0 — same
  pre-existing `clippy.toml` `rand::*` notices as before, not from this
  crate), fmt `--check`, `cargo deny check` (advisories/bans/licenses/sources
  all ok).
- Diff scoped entirely to `consonance/telemetry/src/server.rs`'s
  `#[cfg(test)] mod tests` — no server, dependency, wire-format, or
  public-API change.
