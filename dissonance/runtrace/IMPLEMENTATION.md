# Task 65 — `dissonance/runtrace`: RunTrace journal + scrape decode

Frontier / Wave-5 Phase B ("trace"). A finished run stops being opaque: the
conductor assembles a versioned, serialized `RunTrace`, and this crate is where
that bundle becomes bytes and back. Dependencies 58 (`ControlServer`) and 64
(`spine.rs`) are **already merged into `main`**, so this branch builds directly
on them (the spec was written when they were unmerged).

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
`TraceId`; distinct seeds ⇒ distinct. It is not "genesis-complete" in the strong
sense (the mid-workload base is an ephemeral `SnapId`, not a composable env
prefix — a task-58-substrate limitation, task-68 territory); neither is
`recorded_env` (it is snapshot-rooted, `base_offset = snapshot_vtime`). This is
the honest reproducer the substrate can express today.

### 3. Exactly one stamp axis — and the unit ruling escalated

`record::stamp(vtime) -> Moment` is the **single** V-time→`Moment` mapping
(`Moment(vtime.0)`, one-for-one, mirroring the spine's toy machine and
`control-proto`). Stamps are stop-granular in v1: a run's whole console is
drained under one stop `Moment` (per-exit stamps wait on the `telemetry::Observer`
wiring — a non-goal). **Escalated to the foreman, not decided locally:** whether
`Moment` is the retired-instruction count or the retired-branch V-time it is
derived from. Nothing in these crates bakes in more than the one-for-one
identity the spine already documents, so the ruling can land with zero code
change here.

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
- Genesis-completeness of `RunTrace.env` is bounded by the task-58 substrate (see
  decision 2); a fully genesis-rooted reproducer needs task-68 chain composition.
- Stamps are stop-granular until the `telemetry::Observer` per-exit wiring lands.

## Box gate (gate 6) — handed to the foreman

I could not run the box from this worktree. The box path is **wired and
type-checks for Linux** (`cargo check -p conductor --target
x86_64-unknown-linux-gnu` — Finished), and its portable analog (the mock
recording gate) is green. To run the one box gate:

```sh
# On the determinism box, per docs/BOX-PINNING.md (assigned core, patched KVM):
taskset -c 2 cargo run -p conductor --release -- box \
    --seeds 8 --runs 2 --record /tmp/runtrace-box --retain interesting
```

Expected (the binary asserts these and prints `box RECORDING GATES PASS`):
per-seed byte-identical serialized RunTraces (same `TraceId`, identical journal
bytes); ≥2 distinct `TraceId`s across seeds; `records` non-empty and stamps
monotone; every trace reloads losslessly. The **readiness banner** is confirmed
present by the boot drive (`drive_to_marker` only returns Ok once
`database system is ready to accept connections` is seen) *before* recording
starts.

**To finish the gate**, commit a trimmed real-guest slice as the portable
fixture: `cp` one `.trace` from `/tmp/runtrace-box` to
`dissonance/runtrace/tests/fixtures/real_guest_slice.trace` (trim if large); the
already-committed `runtrace::fixtures_mod::real_guest_slice_decodes_and_rederives_when_present`
test will then decode + re-derive over it (it skips loudly until then). Paste the
`render_record_table` output into the "Box run table" section below.

**Box-safety (CRITICAL, per the spec):** stock KVM = **1396736**. After *any*
patched run: `pkill -9 -f conductor` → wait `lsmod | grep '^kvm_intel'` users=0
→ `rmmod kvm_intel kvm; modprobe kvm; modprobe kvm_intel` → verify size 1396736
on a **fresh** ssh connection. SSH exit-255 on pkill/rmmod is normal — reconnect
and verify.

### Mock run table (portable analog, laptop — `conductor mock --record`)

```
base snapshot sealed at V-time 100
seed                 run  stop            recs journal  retain   trace_id (head)
0x9e1fb946911491d5   0    Quiescent@400    1     146    env      1c5964040f27080d…
0x9e1fb946911491d5   1    Quiescent@400    1     146    env      1c5964040f27080d…   (== run 0)
0x3c46338d10ca15ea   0    Quiescent@400    1     146    env      382a287f0cbc3bbd…
…4 seeds × 2 runs → 4 distinct TraceIds, per-seed byte-identical, records non-empty.
```

### Box run table (fill in from the box run)

_(pending the foreman's box run)_

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
| 6. Box gate (live population + byte-stability) | ⏳ wired + Linux-checked; handed to the foreman |
