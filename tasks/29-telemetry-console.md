# Task 29 — telemetry console: out-of-band observation tap + std-only web viewer (live + replay)

Read `tasks/00-CONVENTIONS.md`, `docs/INTEGRATION.md` (§2 run-loop ownership, §1 doorbell, and
task 28's report-channel section), then the pieces this observes: `consonance/vmm-core/src/vmm.rs`
(`Vmm::step()`, `state_hash()`, `RunResult`), `consonance/vmm-core/src/devices.rs` (`Uart8250` —
the COM1 capture buffer), `consonance/vmm-backend/src/exit.rs` (`Exit` + `ExitCounts`),
`consonance/vtime/src/clock.rs` (`VClock::vns`), and `consonance/hypercall-proto` (the `Event`
service, id 4). This builds a **read-only telemetry lane**: a no-op-by-default `Observer` tap that
vmm-core calls after each serviced exit, a structured V-time-stamped event stream off the back of
it, and a **std-only web console** that renders the system under test live — stdout/stderr, the
event timeline, exit rates, guest event signals, and per-checkpoint state hashes — and re-renders
a recorded run identically.

> **Why it's out-of-band, and how it differs from what already exists.** Three guest→host lanes
> already carry data and they are all **in-band and deterministic**: the serial console (`Uart8250`,
> hashed into M2), the `Event` hypercall service (id 4, guest-pushed test/coverage signals), and
> task 28's **report channel** (`0x0CA2`, guest-pushed conformance values folded into
> `observable_digest`). This task adds **none** of those — it adds a **host-side observation tap**
> that watches the exit stream `vmm-core` already services and copies it out for a human. It is
> **never hashed**, **never part of `observable_digest` or `state_hash`**, **default-off**, and
> **carries no per-host input** — so it adds no contract rows and leaves `contract_hash` unchanged.
> Telemetry is for the operator; the hashes remain the source of truth. The console *surfaces* the
> three in-band lanes (it renders console bytes and id-4 events) but does not own them.

## Part 1 — the `Observer` tap (the one new seam)

In a new **leaf** crate `consonance/telemetry` (no sibling deps; vmm-core consumes it later, like
it consumes `vtime`):

- **`Event`** — the unit of telemetry: `{ seq: u64, work: u64, vns: u64, kind: EventKind }`. `seq`
  is a per-run monotonic counter; `work` is the retired-branch work counter read at the exit; `vns`
  is `VClock::vns(work)`. (`work`/`vns` give the console a deterministic timeline that is identical
  on replay.)
- **`EventKind`** (serde, non-exhaustive): `Console { text: String }` (COM1 writes, UTF-8-lossy —
  display fidelity only; byte-exact fidelity lives in the M2 hash), `GuestEvent { id: u32, data:
  Vec<u8> }` (hypercall `Event` service), `Io { port, size, value, write }`, `Mmio { addr, size,
  value, write }`, `Hypercall { service: u8, opcode: u16, status: u16 }`, `Msr { index, value,
  write }`, `Tsc { value }`, `Rng { value }`, `Cpuid { leaf, subleaf }`, `Inject { vector: u8 }`
  (interrupt delivered at this V-time), `Checkpoint { state_hash: [u8; 32] }`, `Counts(ExitCounts)`
  (periodic — `ExitCounts` is **defined locally** in `telemetry`, a mirror of vmm-backend's exit
  tally, **not** a sibling import: this is a leaf crate, per conventions rule 2; the frontier seam
  maps vmm-backend's counts into it), `Terminal { reason: String }`.
- **`Observer` trait**: `fn emit(&mut self, ev: &Event)`. **Read-only contract** — an `Observer`
  must never draw entropy, advance `work`, or mutate any guest/VMM state; it sees an already-built
  `Event` and returns `()`. Provide **`NullObserver`** (the default; `emit` is a no-op).
- **Sinks** (both impl `Observer`):
  - `NdjsonRecorder<W: Write>` — **lossless**; writes one `serde_json` line per event. This is the
    persisted recording (the replay source of truth).
  - `LiveSink` — **lossy, never blocks**; pushes onto a bounded queue drained by the web server.
    On overflow it drops and counts; the drop count is surfaced as a synthetic event rather than
    stalling the run. (V-time is work-based, so even a blocking observer can't perturb the run —
    but a live UI shouldn't add wall-clock pauses, hence drop-don't-block for the live lane and the
    lossless recorder for the authoritative record.)

The `Observer` is constructed by whoever drives the VMM; `vmm-core` defaults to `NullObserver`.

## Part 2 — the std-only web console (the `console` bin)

A `console` bin target in `consonance/telemetry` (`clap` args; bins-only per the whitelist).
**No async runtime, no framework, no npm, no build step** — `std::net::TcpListener`, a thread per
connection:

- `GET /` → the embedded UI: a single `assets/index.html` pulled in with `include_str!` (vanilla
  JS + inline CSS, **no CDN/npm** so it works offline on the box).
- `GET /events` → **Server-Sent Events** (`Content-Type: text/event-stream`, `Cache-Control:
  no-cache`): each event forwarded as `data: <ndjson>\n\n`, draining the `LiveSink` queue. The
  browser consumes it with `new EventSource('/events')` — one line, no library.
- `GET /recording` → streams a persisted `NdjsonRecorder` file for **replay**; the page `fetch`es
  it and scrubs the V-time timeline entirely client-side (same renderer as live).
- **Event source** is selected by flag: `--source stdin` (so `vmm … --events - | console`),
  `--source unix:<path>` (the VMM connects and writes NDJSON), or `--source file:<path>` (replay).

The page renders, all keyed by `vns`: a **stdout/stderr** pane (append `Console.text`), an **event
timeline** (virtualized list with a V-time scrubber — a range input that filters to `vns ≤ cursor`),
**live exit-rate counters** (from `Counts`), a **guest-events** pane (id-4 `GuestEvent`), and the
**state hash at each `Checkpoint`** with a vns→wall-clock readout. Graphs (exit rate, vns-vs-wall)
are hand-drawn on a `<canvas>` — no chart library.

## Part 3 — the vmm-core seam (frontier; documented here, wired by the integrator)

`Vmm::step()` (`vmm-core/src/vmm.rs`) gains an `&mut dyn Observer` (default `NullObserver`). After
each exit is fully serviced — at the existing quiescent point where `work` is already read for
V-time — it builds the matching `Event` (`vns = vclock.vns(work)`, `seq += 1`) and calls
`observer.emit(&ev)`. `Uart8250` writes map to `Console`, the id-4 `Event` service to `GuestEvent`,
the `Exit` variants to their `EventKind`, `InjectionPlanner` deliveries to `Inject`, the periodic
`state_hash()` checkpoints to `Checkpoint`. This per-exit wiring is **frontier** (integrator-owned,
like §2's run-loop inversion) — **the worker does not touch `vmm-core`**; this task delivers the
crate, the sinks, the bin, and the browser, all driven in tests by a scripted `Vec<Event>` with no
KVM. Document the seam as a new **`docs/INTEGRATION.md` §8 "Telemetry tap (out-of-band
observation)"** stating the read-only/default-off/never-hashed invariants.

## Gates

Mac: `build`/`nextest`/`clippy -D warnings`/`fmt` for `telemetry`. Pure-logic, fully testable
without KVM: NDJSON encode↔decode round-trip (proptest, ≥256 cases); `NullObserver::emit` is a
no-op; `LiveSink` drops-don't-block under a full queue and the drop count is surfaced; the web
server tested over an **in-process loopback** (bind `127.0.0.1:0`, drive a scripted event vector
through `LiveSink`, connect a client, assert the served HTML and the `data: …\n\n` SSE framing).
**No new crate deps** beyond the whitelist (std TCP + `serde_json` + `clap`); **no npm, no build
step** (UI is `include_str!`'d) — call this out in `IMPLEMENTATION.md`. `contract_hash` unchanged
(host-side only; no contract rows). Cross-model pass.

**Determinism identity (the load-bearing gate, proven structurally here + on the box at integration):**
attaching an `Observer` must not change the run. The crate proves it by construction — `Observer`
has no `&mut` access to anything feeding `state_hash`, and `emit` returns `()`. The integrator's
box check (noted for §8, not built here): a payload's `state_hash` is **byte-identical** under
`NullObserver` vs `NdjsonRecorder`, deterministic-twice.

## Deliverables

The leaf `consonance/telemetry` crate: the `Event`/`EventKind` schema + NDJSON wire (round-trip
proptested), the `Observer` trait with `NullObserver`/`NdjsonRecorder`/`LiveSink`, the std-only
`console` bin (SSE `/events`, `/recording` replay, `--source` selection), and the embedded
vanilla-JS `index.html` (live + replay, V-time scrubber, no npm). Plus `INTEGRATION.md` §8 pinning
the read-only/default-off/never-hashed tap invariants for the frontier wiring. No sibling deps; no
`vmm-core` edits in this task.
