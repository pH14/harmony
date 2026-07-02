# Task 74 — Phase I: OpenTelemetry as a first-class deterministic signal channel

> **FRONTIER · Phase I of `docs/EXPLORATION.md`.** THE RULING (2026-07-01): OpenTelemetry is a
> first-class **zero-recompile** sensor channel, exported via an **in-guest** OTLP bridge over the
> `Event` hypercall service — **not** host-side network capture. Mallory had to intercept the
> network to reconstruct a happens-before graph; we read it off span parent/child + link edges
> natively. And because the emitting SDKs and the bridge run *inside* the deterministic guest, trace
> IDs (seeded guest entropy) and span timestamps (the V-time-backed guest clock) are themselves
> deterministic — **span forests are byte-diffable across sibling branches**: this channel's
> superpower, and the property the box gate proves.
>
> Depends on **task 64** (the spine: `Matchable`/`Sensor`/`Record`/`RunTrace.records`), **task 65**
> (Record plumbing — the raw event stream this crate decodes), **task 66** (the matcher DSL — the
> consumer of `Matchable for Span`), **task 73** (the Event-service transport conventions the
> bridge follows); the box gate additionally needs task 58's live `Machine`.

Read first: `tasks/00-CONVENTIONS.md`; `docs/EXPLORATION.md` (signal tiers, the matcher DSL, the
Phase I row); `docs/DISSONANCE.md` (the `Event` service, guest planes, enforcement-determinism
discipline); `tasks/64-explorer-spine-refactor.md` (`Matchable`/`Sensor`/`Record` — the traits you
implement) plus `tasks/65/66/73-*.md` as landed; `consonance/hypercall-proto/src/lib.rs`
(`ServiceId::Event`, `event_emit`, `MAX_PAYLOAD`); `tasks/61-net-vertical.md` (the guest-agent
precedent) and `tasks/29-telemetry-console.md` (the std-only-server precedent); `guest/linux/`
(`runc-init.sh`, `build-initramfs.sh` — the image conventions).

## Environment

Portable-logic surface: everything in `dissonance/otel` — chunk reassembly, OTLP decode, the
canonical span forest, `Matchable for Span`, the HB-summary sensor — is pure, macOS+Linux, gated
over committed fixtures; no box needed. Box-only: the bridge under a real instrumented workload
(patched KVM, the built guest image, task 58's server). Pin per `docs/BOX-PINNING.md`; always
revert KVM to stock **1396736** and verify after any patched run.

Surface list (frontier waiver of hard rule 1): `dissonance/otel/` (new crate; depends on
`dissonance/explorer` for the spine traits — the sanctioned plugin-crate exception to rule 2 per
task 64, call it out in `IMPLEMENTATION.md`); `guest/otel-bridge/` (new; lives under the root
workspace's existing `guest` exclude, so no root-file edit); `guest/linux/` (init-script + image
additions per the existing `build-*.sh` conventions); `consonance/vmm-core/tests/live_otel_channel.rs`
(box harness only — **no production vmm-core changes**). If task 43 has landed by dispatch, read
`guest/` as `harmony-linux/`.

## Context

Of the three signal tiers, **scrape** is the primary channel, and OTel is its richest signal: any
already-instrumented workload emits a structured causal record with **zero recompile** — point
`OTEL_EXPORTER_OTLP_ENDPOINT` at the bridge and it flows. The load-bearing insight (Mallory's):
**spans' parent/child + link edges ARE the happens-before graph.** The HB-summary sensor below is
Mallory-over-spans — its novelty signal without its network interception, and deterministic besides.

## The guest bridge (`guest/otel-bridge`)

A static Linux binary baked into the workload initramfs, started by the init script before the
workload (guest-resident code — Linux-only per the task-61 precedent; note it in the PR).

- **OTLP/http+protobuf, not gRPC.** gRPC means HTTP/2 framing, trailers, and a client-streaming
  surface — a large implementation for a guest daemon. OTLP/http is one `POST /v1/traces` per batch
  with a protobuf body; every SDK supports it (`OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf`); a
  `std::net::TcpListener` thread-per-connection HTTP/1.1 server (task-29 precedent: no async
  runtime, no frameworks) suffices. Listen on `4318`; reply `200`, empty body (a valid empty
  `ExportTraceServiceResponse`).
- **The bridge is a dumb pipe.** It never parses protobuf: strip HTTP, forward the raw body bytes
  over the `Event` service. All decode lives host-side in `dissonance/otel`, where it is portable
  and proptestable; the bridge stays tiny and dependency-free.
- **Chunking is mandatory.** `event_emit` caps one event at `MAX_PAYLOAD − 4` = 4068 bytes and
  explicitly never fragments, and OTLP batches routinely exceed that. Follow task 73's Event-id
  namespace + framing conventions; if 73 has not pinned a chunked-stream convention at dispatch,
  define one here and hand it back (proposed: one event id from 73's registry; payload = 8-byte
  header `{batch_seq: u32, chunk_idx: u16, flags: u16 (bit0 = LAST)}` + bytes). Issue `event_emit`
  via the task-73 SDK transport — the path task 61's flow agent uses; never invent a second one.
- **AlwaysOn sampling.** Workloads run `OTEL_TRACES_SAMPLER=always_on`; the bridge never samples,
  drops, or reorders. Determinism holds by the enforcement-determinism discipline: every bridge
  input is determinized (intra-guest loopback TCP, V-time clocks, seeded entropy) — no wall-clock,
  no unseeded RNG, anywhere in it.

## The host plugin (`dissonance/otel`)

- **Reassembly + decode**: consume the raw id-tagged event stream in the shape task 65 pins,
  reassemble chunks, decode OTLP, yield `(Moment, Record)` entries for `RunTrace.records` (a
  record's `Moment` = its batch's final chunk). Hostile/incomplete chunk sequences are dropped and
  counted — never a panic (rule 4).
- **Canonical span forest**: an owned, serde `Span` (trace/span/parent ids, name, kind,
  start/end nanos, attributes as `BTreeMap`, links, status) and a `SpanForest` with a canonical
  byte encoding — sorted deterministically, **invariant under batch and chunk boundaries** — the
  unit the box gate byte-diffs.
- **`Matchable for Span`** (task 64's trait): `kind()` = the span name; `attr()` = the span's
  attributes plus reserved `otel.*` intrinsics (`otel.service`, `otel.status`, `otel.trace_id`,
  `otel.parent.name`); `moment()` = the record's Moment. Parent/child and link edges are first-class
  on `Span` (`parent()`, `links()`). Task 66's DSL then matches spans with **zero new Rust**
  (`match: { span: "txn.commit", attr: { error: true } }`).
- **The HB-summary `Sensor`** — Mallory-over-spans, specced concretely, not hand-waved:
  1. **Labels**: an open-vocabulary codebook **internal to this plugin** maps
     `(service.name, span name)` → a stable small label; never trace/span ids (those vary across
     interleavings by design) and never leaked into core.
  2. **Edge features**: for each HB edge (parent→child, link→target), `FeatureId` =
     `blake3(label_u, label_v, edge_kind)`, emitted at the Moment the edge becomes known (the
     batch Moment of its later-arriving endpoint) — the graph-so-far as a timestamped stream, so
     timeline admission sees it.
  3. **Bounded path n-grams** (n ≤ 3): label sequences along HB paths ending at each new span,
     each hashed to a `FeatureId`; at most K = 8 predecessors per node, chosen as the
     lexicographically least by `(start, trace_id, span_id)`, so fan-in can't explode the stream.
  4. **Determinism discipline**: the summary is a pure function of the span forest — identical
     forests ⇒ identical feature streams; all iteration BTree-ordered, keyed hashes only. The recipe
     may be tuned, but purity and stable IDs are the contract. The Progression sees only opaque
     `FeatureId`s (blindness preserved).

## The dependency ruling: hand-rolled OTLP decode, no `prost`

OTLP protobuf decode is not whitelist-covered, and the ruling is **hand-roll the spans-only
subset** rather than grant `prost` + `opentelemetry-proto`. Justification: the subset is six
message types (`ExportTraceServiceRequest` → `ResourceSpans` → `ScopeSpans` → `Span`, plus
`KeyValue`/`AnyValue`/`Link`/`Status`) over protobuf's four wire types; the OTLP trace proto is
stable and evolves additively, and unknown fields skip cleanly by wire type, so the decoder is
forward-compatible by construction. Hand-rolling keeps whitelist purity, gives rule-4 panic-free
control over hostile bytes, and keeps `prost-derive`/`syn`/`quote` plus a generated surface ~50×
the subset out of the tree. Write a **test-only encoder** in the same crate so proptests round-trip
generated span trees; commit real-SDK fixture bytes besides. Metrics/logs later = ask-by-comment.

## Prior art

- **Mallory** (Meng et al., CCS 2023) `[beyond]` — happens-before summaries of observed distributed
  events as the novelty signal (54% more distinct states than Jepsen, 22 zero-days). It rebuilds
  the HB graph by network interception; we read it off span edges natively and deterministically.
- **Elle** (Kingsbury & Alvaro, VLDB 2020) `[beyond]` — the downstream consumer: op-history spans
  from this channel feed the isolation-checking oracle in task 75.
- **OpenTelemetry / OTLP** `[eng]` — the stable wire format + env-var configuration that make the
  channel zero-recompile; the ecosystem instruments the workloads for us.

## Acceptance gates

1. **Standard suite** green on `dissonance/otel` (build / nextest / clippy `-D warnings` / fmt /
   deny), macOS + Linux; the bridge crate builds as a static Linux binary.
2. **Portable proptests (≥256):** decode round-trips the test-only encoder; decoder + reassembly
   never panic on arbitrary/truncated bytes and arbitrary chunk interleavings; the canonical forest
   is invariant under re-batching/re-chunking; identical span forests ⇒ identical HB feature
   streams, and two committed known-different-interleaving fixtures ⇒ **different** feature sets;
   the `Matchable` adapter round-trips attributes; committed real-SDK fixture bytes decode.
3. **Box gate A (determinism):** an OTel-instrumented workload — preferred: a small instrumented
   app in the guest image (two processes, client→server over loopback, one traced request path with
   a cross-process parent edge and one link); Postgres + an OTel-emitting sidecar acceptable —
   run same-seed-twice ⇒ **byte-identical** canonical span forest (and `state_hash` equal).
4. **Box gate B (the superpower):** two sibling branches from one snapshot with one perturbed
   `Moment` ⇒ span forests byte-identical before the divergence `Moment`, first diff at/after it.
   Record the run table in `IMPLEMENTATION.md`.

## Box-safety (CRITICAL)

Stock KVM = **1396736**; the patched module is larger. ALWAYS leave the box on stock + verified after
every run: `pkill -9 -f live_otel_channel` (and any `live_*`) FIRST → wait `lsmod | grep '^kvm_intel'`
users=0 → `rmmod kvm_intel kvm; modprobe kvm; modprobe kvm_intel` → verify size 1396736 on a FRESH
ssh connection. SSH drops (exit 255) on pkill/rmmod are normal — reconnect + verify. Pin builds/tests
to `taskset -c 2` (`docs/BOX-PINNING.md`). Run gates in the foreground and READ results before
reporting; no detached pollers + idle.

## Non-goals

- OTLP **metrics/logs** signals — spans first; the decoder skips their fields cleanly. Sampling
  policies beyond AlwaysOn (head/tail sampling would trade determinism for nothing).
- **Host-side network sniffing** — Mallory's approach; ours is in-guest and deterministic, better.
- The Elle checker itself (task 75) — but arrange nothing that blocks op-history spans feeding it.
- A k8s-events scrape channel (follow-on, same `Record` plumbing); the matcher DSL engine itself
  (task 66 — this task supplies its `Matchable` input, not the engine).
