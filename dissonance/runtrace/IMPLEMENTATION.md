# Task 65 — `dissonance/runtrace`: RunTrace journal + scrape decode

Frontier / Wave-5 Phase B ("trace"). A finished run stops being opaque: the
conductor assembles a versioned, serialized `RunTrace`, and this crate is where
that bundle becomes bytes and back. Dependencies 58 (`ControlServer`) and 64
(`spine.rs`) are **already merged into `main`**, so this branch builds directly
on them (the spec was written when they were unmerged).

## Round-6 review response (PR #48)

One blocking item that changed *what gate 6 proves*: the `--record` path bypassed
the sweep's `state_hash` checks, so journal byte-identity certified only
console/terminal determinism, not **guest-state** determinism.

- The recording loop now captures `hash(Whole)` after each run into
  `RecordedRun.state_hash` (the same primitive, at the same point, as the task-58
  sweep). `verify_record` gates on **per-seed `state_hash` equality across runs**
  (guest-state reproducibility) and **cross-seed `state_hash` divergence**
  (≥ `min_distinct` distinct — the real property: an RDRAND-seeding regression
  leaves the console identical but is caught here). Divergence moved off the
  env-bearing journal digest (which diverges by construction) onto `state_hash`.
  New non-vacuity test (`verify_record_flags_non_diverging_guest_state`) proves
  the gate fails when seeds share a state. The mock certifies the same strong
  property (its seeded RDRAND reaches VM state → distinct per-seed hashes), so
  one box cycle now certifies guest-state determinism, not just console/terminal.

## Round-4 review response (PR #48)

One blocking item from the round-3 cross-model pass:

- **Content-address verification on read** — `TraceStore::load`/`env` now re-hash
  the decoded env and reject it with `TraceError::IdMismatch { requested, found }`
  unless `TraceId::of(&env) == id`. The store is content-addressed, so a renamed,
  swapped, or tampered file (whose bytes decode fine but hash to a different id)
  is no longer served on the strength of its filename. Renamed-file test added;
  the conductor's now-redundant explicit re-check was removed (the store enforces
  it).

## Round-3 review response (PR #48)

Four small blocking items from the round-2 cross-model pass:

- **Oversize encode** — `encode` now returns `Result<Vec<u8>, TraceError>` and
  rejects any field whose length overflows the `u32` prefix (> 4 GiB) with
  `TraceError::Oversize`, validated *before* any byte is written (mirrors
  `control-proto`'s `BadLength`). `TraceStore::record` encodes up front so an
  unrepresentable `Full` trace persists nothing. `encode_env` is exempt (no
  length prefix).
- **Decode preallocation bound** — `read_events`/`read_records` now reserve
  `n.min(remaining / MIN_*_WIRE_LEN)` (16 B / 14 B minimum element), so a
  malformed huge count can't reserve gigabytes before validation.
- **Divergence on observed output** — `verify_record` compares distinct **journal
  digests** (the serialized run: terminal + records + coverage + env), not
  `TraceId`s (which diverge by construction since the env embeds the seed). Guest-
  *state* divergence stays the task-58 sweep's job (`state_hash`); the `RunTrace`
  carries none, so a seed-independent-output workload (the mock) still diverges
  only via its env-bearing journal — documented at the check.
- **Reload gate compares the row** — `verify_store_reload` now checks a retained
  row's reloaded `terminal`/`records.len()`/journal digest against the report row
  (not just env), so a stale/corrupted `.trace` for the same reproducer fails.

## Round-2 review response (PR #48)

All three blocking items fixed and all four suggestions actioned:

- **[B1] Stale conductor `public-api.txt`** — regenerated on the pinned nightly;
  `pub mod record`'s surface is now snapshotted.
- **[B2] Codec canonicality** — `read_events` now rejects non-canonical journals:
  an event's `attrs` keys must be **strictly increasing** (a `BTreeMap` would
  otherwise silently re-sort/last-wins-dedup, so `encode(decode(b)) != b` for
  accepted bytes). New typed `TraceError::NonCanonical` + an adversarial inline
  test (out-of-order and duplicate keys → rejected; canonical order re-encodes
  byte-identically).
- **[B3] runtrace public-API snapshot** — added `tests/public_api.rs` +
  committed `tests/public-api.txt` (matching every sibling dissonance crate).
  Also made `mod codec` private (its four fns are re-exported at the crate root),
  so the surface no longer double-lists `runtrace::codec::*`.
- **[S1] "store is write-only to the loop"** — moved the reload readback *out* of
  the recording loop. `run_recording` now touches the store only via
  `store.record` (a true pure sink); the lossless-reload / re-derive half of
  gate 3 is the new post-campaign `verify_store_reload(store, report)`, called by
  `finish_recording` and the tests.
- **[S2] Store semantics** — (1) writes are now **atomic** (temp file + rename);
  (2) a re-record under a *weaker* retention (`EnvOnly` after `Full`) **removes**
  the prior journal, so `has_journal`/`load` reflect the last policy rather than
  serving a stale (content-identical) journal; new test covers it.
- **[S3] AdapterEnv genesis-completeness** — documented the regeneration path
  (genesis under `spec` → `run(deadline = base_offset)` → seal reaches the base
  state under substrate determinism) at the env construction and in decision 2,
  so it is not re-litigated.
- **Moment-vs-VTime** — the foreman **ratified** the one-for-one `stamp` identity
  as the v1 contract; no code change, doc note kept (see decision 3).

## What shipped

- **`dissonance/runtrace`** (new crate)
  - `codec.rs` — the versioned canonical journal `encode`/`decode` (magic `TRC1`
    + `TRACE_FORMAT_VERSION` + env `blob_version` header over a canonical
    payload), the env sidecar `encode_env`/`decode_env`, modeled on
    `control-proto`'s Reader/Writer discipline. `decode` is strict, total, and
    fails loudly with `TraceError::Version` on an unknown version.
  - `scrape.rs` — the concrete `Record` decode: `ChunkDecoder` + `decode_chunks`,
    total and lossless over torn/non-UTF-8 console streams.
  - `store.rs` — `TraceStore` (directory-backed: always the env sidecar, journal
    only under `Retain::Full`), `Retain`, `RetentionPolicy`, `retain_for`.
  - `ingest.rs` — `ingest_ndjson`: telemetry NDJSON `Console` recording → chunk
    stream (the offline path).
  - `error.rs` — `TraceError`, `TraceId` (`blake3` of canonical env bytes).
- **`dissonance/explorer/src/spine.rs`** (additive edit, on-charter — see below):
  pinned the concrete `Record` shape and added `StreamId`; re-exported
  `StreamId`; refreshed the frozen `tests/public-api.txt`.
- **`dissonance/conductor`** — `record.rs`: the in-process recording session
  (`run_recording`, `stamp`, `verify_record`, `render_record_table`);
  `mock::recording_fork_script`; a `--record <dir>`/`--retain` CLI flag on both
  `mock` and `box`; `tests/recording.rs` (the store-discipline gate).

## Key decisions

### 1. `Record` shape pinned in `spine.rs` (the one non-trivial cross-crate edit)

Task 64's spec lists `RunTrace.records: Vec<(Moment, Record)>` but its Public-API
code block **never defines `Record`'s fields** — and task 65's surface list says
in as many words: *"task 64's `RunTrace.records` names a `Record` its fixed
vocab list does not pin; this task pins its concrete shape (and `StreamId`)
there."* The task-64 worker added a placeholder `Record { kind: String, attrs:
BTreeMap<String, Value> }` to make it compile. This task replaces that
placeholder with the shape task 65 §2 mandates:

```rust
pub struct StreamId(pub u16);
pub struct Record { pub stream: StreamId, pub line: Vec<u8> }
```

This is the intended, chartered edit — not a redefinition of a settled type.
Verified nothing else depended on the placeholder: no `Matchable` impl for
`Record` exists, and no code outside `spine.rs` read `Record.kind`/`.attrs` (the
one `.kind` in `adapter.rs` is on `CrashInfo`). The spine's own round-trip test
and the frozen `public-api.txt` were updated accordingly; all 78 explorer tests
+ the public-api snapshot pass.

Losslessness rule made concrete: `line` **retains its trailing `\n`**, so
`concat(records.line) == concat(chunk bytes)` — every input byte lands in
exactly one record (task 65 §2). A trailing unterminated line keeps no
terminator and is stamped at the terminal `Moment`.

### 2. `RunTrace.env` = `recorded_env()`-equivalent, built from public seams

§4 says `env: machine.recorded_env()`, but the recording session drives
`ControlServer::handle` **in-process** (the only way to reach
`server.vmm().serial()` between verbs — a socket client cannot), so there is no
`SocketMachine` to call `recorded_env()` on. The session instead builds the
byte-identical value from the public `SpecEnvCodec`/`AdapterEnv` seams:

```
AdapterEnv { base_offset: snapshot_vtime, pos: terminal_vtime, spec: seeded(seed) }.encode()
```

For a v1 seed-driven run (no surfaced decisions) this is *exactly* what
`SocketMachine::recorded_env` emits — the branch env re-wrapped with the
snapshot/terminal `Moment`s. Same seed ⇒ identical env bytes ⇒ identical
`TraceId`; distinct seeds ⇒ distinct.

**This env IS genesis-complete** despite the ephemeral base `SnapId`: the
snapshot regenerates by deterministic replay — boot genesis under `spec`,
`run(deadline = base_offset)`, seal — reaches the identical base state (substrate
determinism, the premise task 63 validates), so `{base_offset, pos, spec}` fully
reproduces the run from genesis. This is the load-bearing premise of env-only
retention (the Nyx take: the artifact is what the restore path cannot regenerate;
here only the env is). Flagged in round 1 by two readers and by the GPT-5.5 pass;
the foreman rejected the "not genesis-complete" reading — documented at the env
construction so it is not re-litigated.

### 3. Exactly one stamp axis — unit ruling **ratified**

`record::stamp(vtime) -> Moment` is the **single** V-time→`Moment` mapping
(`Moment(vtime.0)`, one-for-one, mirroring the spine's toy machine and
`control-proto`). Stamps are stop-granular in v1: a run's whole console is
drained under one stop `Moment` (per-exit stamps wait on the `telemetry::Observer`
wiring — a non-goal). The foreman **ratified** this one-for-one identity as the
v1 contract: on the v1 substrate `Moment` values are V-time units
(retired-branch-derived); a distinct instruction-count reading, if it ever
arrives, lives in the adapter's stamp function (the single seam isolated here)
with no spine or trace-format change. Zero code change required.

### 4. Retention

`RetentionPolicy` (`all` | `interesting` | `env-only`, default `interesting`) →
`Retain` via `retain_for`. v1 "interesting" = terminal `is_bug()`
(`Crash`/`Assertion`) ∪ caller-flagged. The env sidecar is *always* written; the
knob gates only journal bytes (never snapshots — task 68). The recorder is a pure
sink: `run_recording` only ever calls `store.record`, and the gate
`the_retention_knob_never_changes_the_campaigns_report` proves the campaign's
`TraceId`s / stops / record counts / journal sizes are identical across all three
policies — the store is write-only to the loop.

## Deviations considered and rejected

- **Keeping `Record { kind, attrs }` and encoding a raw line into `attrs`** —
  rejected: contradicts §2's explicit `{ stream, line }` and "raw and
  structural", and the surface list orders the concrete pin here.
- **Driving the recording session over a socket via `SocketMachine`** — rejected:
  `serve()` blocks the thread, so the session could not drain
  `server.vmm().serial()` between verbs; §4 calls for the in-process `handle`
  path precisely for this.
- **Excluding `\n` from `line`** — rejected: it breaks the "every input byte
  lands in exactly one record" invariant; keeping the terminator makes
  losslessness a clean `concat` equality.
- **`serde_json` for the journal** — rejected in favor of a `control-proto`-style
  binary codec: tighter canonical guarantees, golden-fixture precedent, and no
  string-key map ambiguity.
- **Depending on the `telemetry` crate for NDJSON ingest** — rejected: mirrored
  the wire locally with a minimal `serde_json` view (conventions rule 2), so
  `runtrace` stays a pure dissonance replay-plane crate with no `consonance`
  edge.
- **`record()` returning a bare `TraceId`** (as §3 shorthand writes) — it writes
  files, so it returns `Result<TraceId, TraceError>` (no-panic library rule).

## Known limitations

- `events` is serialized but always empty (task 73); `coverage` is always `None`
  under task 58's zero-width negotiated geometry. Both are day-one format slots,
  not future bumps.
- `RunTrace.env` is genesis-complete via deterministic replay (decision 2), but
  is **snapshot-rooted** (`base_offset = snapshot_vtime`), not a folded
  suffix-chain; task-68 chain composition is what turns a below-a-corpus-snapshot
  run into a directly-replayable genesis artifact.
- Stamps are stop-granular until the `telemetry::Observer` per-exit wiring lands.

## Box gate (gate 6) — PASSED on the determinism box (2026-07-02)

Ran on `hypervizor` (det-cfl-v1 host, patched KVM), head `44d3f14`, CPU-pinned to
the coordinator-leased **core 2** via `scripts/box-window.sh`:

```sh
taskset -c 2 cargo run -p conductor --release -- box \
    --seeds 8 --runs 2 --record /root/runtrace-box --retain interesting
```

**Verdict:** `box RECORDING GATES PASS` (rc=0). Postgres booted to the readiness
banner (`database system is ready to accept connections`, step 98985); the base
snapshot sealed mid-workload at V-time 442905523; 8 seeds × 2 runs recorded.

- **Per-seed determinism (guest state):** each seed's two runs share one
  `state_hash` **and** one `TraceId` **and** byte-identical 3116-byte journal.
- **Cross-seed divergence (strong):** all **8** `state_hash`es distinct — real
  guest-state divergence, not just env labels (the round-6 fix; `state_hash` is
  the same primitive the task-58 sweep gates on).
- `records` = 34 per run (non-empty), stamps monotone, every trace reloads
  losslessly (`verify_store_reload` clean), readiness banner present.

**Box-safety:** the window coordinator reverted to stock on release
(`REVERT OK`), and KVM was independently confirmed stock **1396736** (kvm_intel
users=0, no leases) on a **fresh** ssh connection after the run.

### Box run table

```
base snapshot sealed at V-time 442905523
seed                 run  stop                  recs journal retain state_hash    trace_id
0x9e1fb946911491d5   0    Crash@463031443[65B]   34   3116   full   643789026c6c  ff0c5b2d4235
0x9e1fb946911491d5   1    Crash@463031443[65B]   34   3116   full   643789026c6c  ff0c5b2d4235
0x3c46338d10ca15ea   0    Crash@463031443[65B]   34   3116   full   bb3e5b4d3853  c80631024ec5
0x3c46338d10ca15ea   1    Crash@463031443[65B]   34   3116   full   bb3e5b4d3853  c80631024ec5
0xda8eadd3938199ff   0/1  Crash@463031443[65B]   34   3116   full   b90384b46c10  130687b93852
0x78f5261a13771d94   0/1  Crash@463031443[65B]   34   3116   full   ccae2fdf2899  61e7465528a5
0x173da060922a81a9   0/1  Crash@463031443[65B]   34   3116   full   5dc1d1459983  04cd2bd897c8
0xb5641aa715e005be   0/1  Crash@463031443[65B]   34   3116   full   711adf8a69ff  6d16ecdea7b5
0x53ac94ed95578953   0/1  Crash@463031443[65B]   34   3116   full   a337632b74d2  cc841f4a1efc
0xf1930d34140d0d68   0/1  Crash@463031443[65B]   34   3116   full   2cbf688c615f  f4b3baa2e225
```

(The Postgres image terminates by convention via `reboot -f`, which the
workload-blind server maps to `Crash{Shutdown}` — interpreting that convention is
the caller's job, per `control.rs`.) One trimmed 3116-byte journal from this run
is committed as `tests/fixtures/real_guest_slice.trace`; the
`real_guest_slice_decodes_and_rederives` test decodes it, re-encodes canonically,
and re-derives ≥1 feature (marker `database system is shut down`, present in the
post-snapshot console — the readiness banner is pre-snapshot).

### Mock run table (portable analog, laptop — `conductor mock --record`)

```
base snapshot sealed at V-time 100
seed                 run  stop            recs journal  retain   trace_id (head)
0x9e1fb946911491d5   0    Quiescent@400    1     146    env      1c5964040f27080d…
0x9e1fb946911491d5   1    Quiescent@400    1     146    env      1c5964040f27080d…   (== run 0)
…4 seeds × 2 runs → 4 distinct state_hashes, per-seed byte-identical, records non-empty.
```

## Regeneration procedures

- **Journal format bump:** change the `encode`/`decode` layout → bump
  `TRACE_FORMAT_VERSION` (lib.rs) → `UPDATE_FIXTURES=1 cargo test -p runtrace
  --test version_bump` (refreshes `golden_v1.trace`) → `UPDATE_FIXTURES=1 cargo
  test -p conductor --test recording` (refreshes `mock_recording.trace`). The
  bump is exactly `control-proto`'s `PROTO_VERSION` discipline: old journals then
  fail `decode` with a loud `TraceError::Version`.
- **Explorer public API:** after a `spine.rs` public change,
  `UPDATE_PUBLIC_API=1 cargo test -p explorer --test public_api`.

## Gate status

| Gate | Status |
|---|---|
| 1. Standard suite (build/nextest/clippy `-D warnings`/fmt/deny), runtrace + conductor + explorer | ✅ macOS; ✅ Linux via `cargo check --target` |
| 2. Decode proptests (totality, losslessness, re-chunking, incremental≡batch) + fixtures | ✅ (512 cases each) |
| 3. Roundtrip proptests + re-derive sensor stability (encode/decode + store) | ✅ (512 cases) |
| 4. Version-bump: byte-pinned golden + loud `TraceError::Version` | ✅ |
| 5. Store discipline (env-only ⇒ 0 journals, all listable/loadable; knob never changes the report) | ✅ |
| 6. Box gate (live population + byte-stability + guest-state determinism) | ✅ PASSED on `hypervizor` (patched KVM, det-cfl-v1), head `44d3f14`, core 2; reverted to stock + verified |
