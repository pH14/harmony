# Task 65 — `dissonance/runtrace`: RunTrace journal + scrape-tier decode (Phase B, "trace")

> **FRONTIER · Wave-5 Phase B.** A run stops being opaque: after task 58's loop stops, the
> conductor assembles a `RunTrace` — the versioned, serialized bundle the whole replay plane
> (Sensors, Oracles, re-derivation) works over offline. Build the journal format, the store, and
> the scrape-tier decoder (console bytes → timestamped `Record`s), and wire the conductor to
> populate and persist it. No sensing, matching, or search — this makes runs *recordable*.
>
> Depends on **task 58** (the `ControlServer`/conductor this extends — the **unmerged branch
> `task/close-the-loop`**; the foreman lands 58 first, and the Read-first paths
> `consonance/vmm-core/src/control.rs` + `dissonance/conductor/` exist only on that branch until
> it merges) and **task 64** (the spine vocab: `RunTrace` lives in
> `dissonance/explorer/src/spine.rs`; cite it, never redefine it — `docs/EXPLORATION.md`'s
> critical path lists B before C, but B serializes C's vocab, so the foreman lands 64's
> `spine.rs` first). Independent of task 63's GO/NO-GO and of 59/60/61.

Read first: `tasks/00-CONVENTIONS.md`, `docs/EXPLORATION.md` ("live plane vs. replay plane" — the
data-volume ruling this store implements — and "RunTrace"), `docs/DISSONANCE.md` (the reproducer),
`tasks/64-explorer-spine-refactor.md` (the vocabulary), `tasks/58-close-the-loop.md` +
`dissonance/conductor/` (`run_sweep`/`run_session` — the loop being wired),
`consonance/vmm-core/src/control.rs` (`ControlServer::handle`/`vmm()` — the read-only seam that
makes recording possible without touching vmm-core), `tasks/29-telemetry-console.md` +
`docs/INTEGRATION.md` §8 (the stamped `Console`-chunk convention), and
`dissonance/control-proto/src/types.rs` + its `codec.rs`/golden tests (`StopReason`, `VTime`,
`CoverageGeometry`; the versioning discipline to copy).

## Environment

Portable-logic surface (macOS + Linux, laptop-gated): the journal codec, `TraceStore`, scrape
decoder, and all proptests — driven by synthetic streams plus **committed fixtures** (a mock-mode
conductor recording + a trimmed real-guest slice committed by the box gate). The box surface is
**one gate**: live population + byte-stability against the Postgres workload (patched KVM,
det-cfl-v1 host). Pin per `docs/BOX-PINNING.md`; revert KVM to stock **1396736** + verify after
any patched run. Surface list (frontier waiver of hard rule 1):

- `dissonance/runtrace` — **new crate**: journal codec, `TraceStore`, scrape decoder.
- `dissonance/conductor` — wiring: recording session, `RunTrace` assembly, the retention flag.
- `dissonance/explorer` — **additive `spine.rs` edit only**: task 64's `RunTrace.records` names a
  `Record` its fixed vocab list does not pin; this task pins its concrete shape (and `StreamId`)
  there, next to `RunTrace`. No engine/Progression/strategy edits.
- Read-only: `consonance/vmm-core` (`ControlServer`, `Vmm::serial`), `consonance/telemetry`.

**Why a new crate, not more `explorer`:** task 64 refactors `explorer` in parallel (merge
contention); the journal is replay-plane *infrastructure*, not search policy; it establishes the
Wave-5 plugin direction (depend on `explorer`, consume spine vocab) that tasks 66/67/70+ reuse;
and compat surfaces get a focused crate with golden fixtures (`control-proto` precedent).

## Context

Today a finished run evaporates: `run_sweep` keeps a hash and a one-line stop string.
`docs/EXPLORATION.md` rules the replay plane works over a **versioned, serialized** `RunTrace`
and the store is not a data lake: **always persist the tiny `Environment`** (the genesis-complete
reproducer; same env ⇒ same run, the rest regenerates by replay); serialize full traces only for
a retained subset. Raw material: `Vmm::serial()` is the ordered console capture; the task-29
convention stamps drained chunks with the deterministic counter at the drain boundary.

## What to build

### 1. The versioned journal

`encode(&RunTrace) -> Vec<u8>` / `decode(&[u8]) -> Result<RunTrace, TraceError>`, modeled on
`control-proto`'s codec discipline: a header (magic + `TRACE_FORMAT_VERSION: u16` + the env
`blob_version`) over a **canonical** payload — equal traces encode to equal bytes (`BTreeMap`
only, no floats, no wall-clock; hard rule 4). `decode` fails loudly (`TraceError::Version`) on an
unknown version — never a silent reinterpretation; golden fixtures pin the bytes. All five fields
serialize from day one: `events` is present-and-empty until task 73, `coverage` is `None` under
task 58's empty negotiated geometry — neither is a format bump later. `TraceId = blake3(canonical
env bytes)`: content-addressed, so determinism makes byte-stability id-stability for free.

### 2. The scrape-tier decoder (the concrete `Record` decode lives HERE)

`decode_chunks(stream, &[(Moment, bytes)]) -> Vec<(Moment, Record)>` in `dissonance/runtrace` —
not in the conductor (it must run offline over recorded chunk streams, incl. a telemetry NDJSON
`Console` recording via a small ingest helper) and not in a plugin (task 67's codebook *consumes*
`Record`s, never produces them). A `Record` is raw and structural: `{ stream: StreamId, line:
Vec<u8> }` — exact newline-split bytes (UTF-8-lossy is display-only), stamped with the arrival
`Moment` of the chunk that completed the line. **Total and lossless**: never panics on
torn/non-UTF-8 input; every input byte lands in exactly one record; a trailing unterminated line
is emitted at the terminal stamp. Stamps are stop-granular in v1 (see §4).

### 3. The `TraceStore` + the retention knob

Directory-backed: `record(&RunTrace, Retain) -> TraceId` **always** writes the env sidecar;
`Retain::Full` additionally writes the journal; `load`/`env` read back; `ids()` iterates in
deterministic (sorted) order. The campaign knob (conductor flag `--retain
{all|interesting|env-only}`, default `interesting`) maps runs to `Retain`: v1 "interesting" =
terminal ∈ {`Crash`, `Assertion`} ∪ caller-flagged. Regenerating an unretained trace = replay from
its env + re-record (documented path; no batch regenerator required). Retention economics: task 68.

### 4. Conductor wiring

A recording session in `dissonance/conductor` driving `ControlServer::handle` in-process (the
socket path stays for remote use): after each `Run` reply it drains **new** serial bytes from
`server.vmm().serial()`, stamped with that stop's deterministic counter, mapped onto the spine's
`Moment` axis in **exactly one** documented `stamp()` function (never a second time axis; flag
any `Moment`-vs-`VTime` unit ruling to the foreman, don't decide it locally). At run end it
assembles `RunTrace { terminal: the StopReason, env: machine.recorded_env(), coverage:
snapshot-at-run-end (None today; a terminal signal, never a timeline key), events: vec![],
records: decode_chunks(...) }` and calls `TraceStore::record`.

**Invariants restated (and gated below):** the recorder is a **pure sink** — no `Tactic` or
live-plane code reads Sensor/store output mid-run (open-loop Modulation), and no explorer engine
code learns the store exists (Progression blindness). Determinism discipline everywhere: stamps
only from deterministic counters, canonical encodings, seeded randomness only.

## Prior art (design anchors, not a bibliography)

- **Nyx (USENIX Sec'21) [eng]** — the canonical snapshot-fuzzer control plane. Take: the artifact
  is defined by the restore path — anything replay cannot regenerate must be in it; here only
  `env` is load-bearing, which is why env-only retention is sound.
- **Nyx-Net (EuroSys'22) [eng]** — incremental mid-sequence capture; validates parent-rooted
  exemplars. Take: store what `recorded_env` hands you (tail-complete deltas compose later); never
  assume genesis-rooted artifacts in the journal.
- **Snapchange / wtf (practitioner)** — the recording truths papers omit. Take: version on-disk
  formats from day one; make the decoder total over torn/binary console streams; persist
  selectively and regenerate by replay.

## Acceptance gates

1. **Standard suite** (build / nextest / clippy `-D warnings` / fmt / deny) green on
   `dissonance/runtrace` + `dissonance/conductor` (+ `explorer` if `spine.rs` touched), macOS + Linux.
2. **Decode proptests (≥256):** totality + losslessness (arbitrary byte streams, torn lines,
   non-UTF-8: no panic, bytes partition exactly into records); re-chunking within identical stamps
   never changes decoded records; incremental ≡ batch decode. Run the committed fixtures too.
3. **Roundtrip proptests (≥256):** `decode(encode(t)) == t`, `encode(decode(b)) == b` (canonical
   form); serialize→reload→**re-derive** stability — a test-local `Sensor` (marker-hit line counter,
   not a shipped sensor) yields identical `(Moment, Feature)` streams over the reloaded and
   original trace.
4. **Version-bump compatibility test:** a golden journal fixture pinned byte-for-byte; a
   synthetic bumped-version envelope decodes to a loud `TraceError::Version`; the bump procedure
   documented as `control-proto` does it.
5. **Store discipline:** under `--retain env-only`, a mock-mode campaign persists zero journals
   yet every `TraceId` is listable and its env loadable; the retention knob never changes the
   campaign's verb sequence or report (the store is write-only to the loop — assert no reads).
6. **Box gate (live population + byte-stability):** conductor box mode, Postgres workload, one
   mid-workload post-readiness snapshot, ≥4 seeds × 2 runs: per seed, byte-identical serialized
   RunTraces (same `TraceId`, identical journal bytes); ≥2 distinct `TraceId`s across seeds;
   `records` non-empty, stamps monotone, the readiness banner present; re-derive (gate 3's sensor)
   identical across each pair. Commit a trimmed journal + raw-chunk slice as the portable fixture;
   record the run table in `IMPLEMENTATION.md`.

## Box-safety (CRITICAL)

Stock KVM = **1396736**; the patched module is larger. ALWAYS leave the box on stock + verified
after every run: `pkill -9 -f conductor` (and any `live_*`) FIRST → wait `lsmod | grep
'^kvm_intel'` users=0 → `rmmod kvm_intel kvm; modprobe kvm; modprobe kvm_intel` → verify size
1396736 on a FRESH ssh connection. SSH drops (exit 255) on pkill/rmmod are normal — reconnect +
verify. Pin builds/tests to `taskset -c 2` (`docs/BOX-PINNING.md`). Run gates in the foreground
and READ results before reporting; no detached pollers + idle.

## Non-goals

- Sensors / `CellFn` / template clustering (task 67), the matcher DSL (66) — raw `Record`s only.
- Link-tier decode / SDK `GuestEvent`s (73), OTel span decode (74) — `events` stays empty; the
  field merely serializes.
- Snapshot-retention economics / lazy materialization (68) — `Retain` gates journal bytes, never
  snapshots.
- Wiring `telemetry::Observer` into `Vmm::step` (INTEGRATION §8, integrator-owned) or any other
  `consonance/vmm-core` change; per-exit stamp granularity waits for it.
- Any search/scoring change — the Progression never learns the store exists.
