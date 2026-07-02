# IMPLEMENTATION — task 58 (close the loop: control-transport server + socket-backed `Machine`)

This is the Wave-4 keystone: the two missing hops that make `dissonance/` depend on
`consonance/` for the first time, and the first time any of the eight R2 control verbs is
served. It is a **frontier** task spanning the spec's surface list plus the implementer-chosen
demo crate. Everything is Mac-first (portable gates green locally); the end-to-end proof is
box-only and handed to the foreman (see **The box gate** below).

## What landed, where

| Crate | Change | Role |
|---|---|---|
| `consonance/vmm-core` (`src/control.rs`) | the **control-transport server** | serves the verbs over `control-proto`'s codec against a live `Vmm` + `SnapshotEngine` |
| `dissonance/explorer` (`src/adapter.rs`) | the socket-backed **`SocketMachine`** + **`SpecEnvCodec`** | the first non-toy `Machine`; the `EnvCodec` seam bound to `environment`'s real codec per the task-93 ruling |
| `dissonance/conductor` (**new bin crate**) | the demo binary + the portable loopback gates | closes the loop: the socket `Machine` driven against the server; the run table + gate verdicts |
| `dissonance/control-proto` (`src/error.rs`, `src/codec.rs`) | the additive `ControlError::Unsupported` variant | the wire code the seed-driven server answers `perturb` / non-`Whole` hash / pre-`hello` with |

The demo bin crate is named **`conductor`** (the spec's implementer's choice — "in `dissonance/explorer`
or a new `dissonance/conductor` bin crate"). It also holds the library the loopback gates share.

## The seam binding (task-93 ruling), concretely

The ruling (`docs/DISSONANCE.md` §"Ruling (task 93)": keep `EnvCodec::compose`) binds the frontier
adapter on four points. All four are implemented in `dissonance/explorer/src/adapter.rs`:

- **Tail-completeness.** `SocketMachine::run` stamps every resolved decision at its stop `Moment`
  into the current Timeline's recording, and `recorded_env` emits the branch-local delta — always
  the `EnvSpec::Recorded` variant (a `Seeded` base is promoted to `Recorded` with no overrides), so
  the production `compose`'s variant check can never fire on an adapter-minted artifact. Seed-driven
  runs surface no decisions, so the delta is trivially tail-complete.
- **`at` provenance.** The adapter blob (`AdapterEnv`, magic `R2A1`) wraps the task-24 `EnvSpec`
  with `base_offset` (the absolute `Moment` the delta is keyed from — the production analogue of the
  toy blob's `base_offset`) and `pos` (the capture point a `mutate` slices at — the toy's `pos`), so
  `compose` recovers `at` from the delta alone. Only the inner `EnvSpec` bytes travel on the wire, so
  the server never learns the wrapper.
- **Fallibility → panic (the ruling's default).** `SpecEnvCodec::{compose,mutate}` **panic** on
  `UnsupportedComposition`/`Overflow` or a malformed adapter blob. The ruling permits either the panic
  or making the seam fallible; the panic is chosen because the seam receives only adapter-minted
  artifacts, so a failure is an invariant violation (a defect in the adapter/contract), not a run
  outcome — the campaign aborts loudly rather than minting a reproducer that does not replay. Proven
  by three `#[should_panic]` tests (seed mismatch, re-key overflow, non-adapter blob).
- **Standing-fault confinement.** Vacuous in the v1 fault vocabulary (no standing faults exist), but
  `SpecEnvCodec::mutate` still panics on a standing-fault-carrying base rather than slicing one into a
  branch-local delta — the confinement rule is enforced here the day standing faults appear.

The ruling's end-to-end acceptance gate — mint a bug below a non-genesis base, `branch(genesis,
bug.env)`, require the same stop + hash — is the box gate's job (the toy-machine analogue is already
pinned in `explorer/tests/replay.rs::compose_rebase_replays_from_genesis`).

## The server (workload-agnostic substrate, task 43 F5)

`ControlServer` owns one live `Vmm<B>` + a `SnapshotEngine`, and dispatches:

- `hello(caps)` → protocol 1, `Environment` blob version exactly `EnvSpec::BLOB_VERSION`, **zero-width
  coverage geometry** (no producer exists), `GUEST_HAS_SDK` off. Any verb before `hello` → `Unsupported`.
- `snapshot` → base-seal (memory + `vm_state`) + a pool-wide handle. The remaining fail-closed
  boundaries (an RNG mid-exit completion, a non-V-time-synchronized point) answer `NotQuiescent`; the
  caller runs a little further and retries. (Task 41's non-quiescent capture is merged, so mid-workload
  points are sealable.)
- `drop(snap)` → release + GC via the store.
- `branch(snap, env)` → restore into a **fresh** VM (from the `VmmFactory`) + `reseed_entropy` from the
  env's seed (the proven divergence mechanism, tasks 40/42). An env carrying overrides or standing
  faults is **rejected** `Unsupported` (not silently run without them — they need the task-59/61
  enforcement loops).
- `replay(snap)` → verbatim restore, no reseed.
- `run(until)` → step to a terminal stop or the V-time deadline; workload-blind terminal mapping
  (`Hlt`/`DebugExit{0}` → `Quiescent`; `DebugExit{≠0}` → `Crash{Panic}`; backend `Shutdown` →
  `Crash{Shutdown}`). `resolve` is always `ResolveWithoutDecision` (no decision surfaces seed-driven).
- `hash(Whole)` → `state_hash`; `Disk`/`Region` → `Unsupported`.
- `perturb` → `Unsupported` (task 59 lights it up).

**Fresh-VM restore discipline.** `branch`/`replay` never restore in place — a serviced exit usually
leaves a staged completion in the backend that `restore_vm_state` correctly refuses to restore across,
and the box allows one open `perf_event` counter at a time. So every restore drops the live VM first,
then boots a fresh one via the `VmmFactory` and restores into it — exactly the task-40/41 box-demo
pattern, within budget because snapshot performance is a task-58 non-goal (D5).

**Two result categories, fail-loud.** A guest outcome is a `StopReason`; a recoverable control failure
is a `ControlError` reply; an unrecoverable substrate failure (mid-run `VmmError`, a store invariant, a
factory that cannot boot) is a `ServeError` that tears the session down. Never misclassified across
categories.

## The `Moment` axis on this substrate (a documented stand-in)

`environment::Moment` is a retired-*instruction* count; vmm-core's only deterministic axis today is
**effective V-time** (ns ≡ retired conditional branches, 1 ns/branch under the contract clock). Until
the task-59 exact-count machinery exists, the adapter keys its offsets by effective V-time as stamped
in every `StopReason` — a deterministic, monotone anchor recoverable from the delta alone. This is
sound for the seed-driven contract (there are no overrides to re-key yet), and the axis choice is
confined to `adapter.rs`. A new `Vmm::effective_vns` accessor exposes the skid-free VTIM-chunk V-time
(never a live counter read) for the server's deadline check.

## Acceptance gates

### Gate 1 — portable (macOS + Linux, mock backend): **PASS**

`conductor/tests/loopback.rs` drives the explorer's `SocketMachine` against the vmm-core server over an
in-process unix socketpair, mock guest:

- **Every verb over the wire** — `hello`/`snapshot`/`branch`/`replay`/`run`/`hash`/`drop` through the
  typed adapter; `perturb`, non-`Whole` hash scopes, a pre-`hello` verb, and error cases (`UnknownSnapshot`,
  `ResolveWithoutDecision`) through a raw-frame client (the adapter cannot express those by design).
- **Determinism** — `branch(s, seed) → run → hash` reproducible per seed and divergent across seeds.
- **Replay** — `replay(base)` reproduces the pre-snapshot hash after arbitrary interleaved verbs.
- **Snapshot retry** — a first-point-unsnappable server; the sweep advances to a sealable boundary.

`conductor/tests/determinism_proptest.rs` proves the branch/run/hash + replay properties (and
session-independence) over **256 cases**. Plus the server's own unit tests (`vmm-core/src/control.rs`,
16 tests) and the adapter's (`explorer/src/adapter.rs`, incl. the ruling panics — compose
seed/overflow/non-genesis, mutate non-genesis, non-adapter blob — and the scripted-stream regression
tests for the connect-origin probe, the replay-origin restore, the tail-completeness recording, and
the coverage-geometry guard).

Demo (`cargo run -p conductor -- mock --seeds 8 --runs 2`): 8 seeds × 2 runs, every seed bit-identical
across its runs, **8 distinct futures**, replay == capture, GATES PASS.

### Gate 2 — box (the milestone): **PASS** (foreman-executed — run table below)

### Gate 3 — standard suite on every touched crate: **PASS**

`build` / `nextest` / `clippy -D warnings` / `fmt` / `cargo deny` green on control-proto (55),
environment (91, untouched), explorer (63), vmm-core (285), conductor (6). No golden re-blessing —
the server is additive; existing `live_*` gates are byte-identical (nothing in the determinism path
changed; `effective_vns` and `state_blob` are unchanged). Public-API snapshots refreshed for vmm-core,
explorer, and the new conductor crate.

## The box gate — result (PASS) + runbook

**The box milestone gate ran and PASSED**, executed by the foreman on the determinism box (core 2
per the `docs/BOX-PINNING.md` frontier ruling; patched KVM loaded for the run; **reverted to stock
`1396736` + verified** after; head `0b28d3f`; the ht42 bare-Postgres image `initramfs-postgres.cpio.gz`
+ the current SMP bzImage; log `/root/pr44-gate.log`). Run table, verbatim:

```
readiness marker at step 98985; base sealed at the next snapshottable boundary
base snapshot: sealed at V-time 442905523 (2 attempts), capture state_hash 7dcb1690…b236621c
seed                  run  stop                     state_hash
0x9e1fb946911491d5    0,1  Crash@463031443[65B]     64378902…  (both runs identical)
0x3c46338d10ca15ea    0,1  Crash@463031443[65B]     bb3e5b4d…
0xda8eadd3938199ff    0,1  Crash@463031443[65B]     b90384b4…
0x78f5261a13771d94    0,1  Crash@463031443[65B]     ccae2fdf…
0x173da060922a81a9    0,1  Crash@463031443[65B]     5dc1d145…
0xb5641aa715e005be    0,1  Crash@463031443[65B]     711adf8a…
0x53ac94ed95578953    0,1  Crash@463031443[65B]     a337632b…
0xf1930d34140d0d68    0,1  Crash@463031443[65B]     2cbf688c…
replay(base): state_hash 7dcb1690…b236621c (== capture)
GATES PASS: per-seed reproducible, >= 2 distinct futures, replay == capture
```

**Bar exceeded:** 8/8 seeds bit-identical across their run pairs, **8/8 distinct futures** (needed ≥ 2),
verbatim base replay. The uniform `Crash@463031443` stop is the documented workload convention — the
Postgres image's clean terminal is a forced reboot → `Shutdown` → `Crash{Shutdown}` under the
workload-blind mapping — at an identical Moment across seeds because post-CRNG-seal guests are
byte-identical (the honest-divergence note below, observed exactly); the entropy fork surfaces in the
`VTIM` hash chunk → 8 distinct hashes.

The command (per `docs/BOX-PINNING.md`), for reproduction:

```sh
# On the box, patched KVM loaded for THIS run (coordinate: only load/revert when
# `lsmod | awk '$1=="kvm_intel"{print $3}'` == 0):
taskset -c 2 timeout 3600 cargo run -p conductor --release -- box \
    --seeds 8 --runs 2 \
    --ready-marker 'database system is ready to accept connections'
# ALWAYS revert KVM to stock afterwards and verify: lsmod | grep '^kvm ' == 1396736
```

**What gate 2 asserts** (the milestone bar): one snapshot **mid-workload, post-readiness** (the
`--ready-marker` step drives the live guest there before the sweep seals — this is the *only*
workload-aware policy in the path; the server + adapter stay workload-blind), then **N ≥ 8 seeds**
(the box path enforces `--seeds >= 8`), each run **twice** → bit-identical `state_hash` per seed,
**≥ 2 distinct hashes across seeds**, and `replay(base)` → identical hash to the capture. `conductor
box` prints exactly this run table and the gate verdicts (`verify(&report, 2)`).

> **Honest expectation on divergence (from the task-40 branching demo).** A snapshot sealed *after* the
> kernel CRNG is seeded (which happens before the first console byte, so any post-readiness seal is)
> makes the entropy fork surface only in the host-side entropy bookkeeping — the branches are otherwise
> byte-identical guests. That still satisfies gate 2's letter: the entropy stream position rides the
> `VTIM` hash chunk (`wire_snapshot_hashing` is on), so distinct seeds still produce **distinct
> `state_hash`es** (observed: 8 distinct). Gate 2 asks for ≥ 2 distinct hashes, not guest-observable
> divergence (unlike the stricter task-40 demo). This is the expected, correct shape for a mid-workload
> seal; it is not a weaker gate, it is the honest one for this snapshot phase.

## Deviations considered

- **Touching `dissonance/control-proto`** (one variant, `ControlError::Unsupported`) is beyond the
  spec's literal surface list (`vmm-core` + `explorer` + optionally `environment`). Justified: the spec
  text itself pins "`perturb` → `ControlError::Unsupported`" and task 59's header states "`perturb`
  returns `Unsupported` after task 58", so the variant *is* part of the task-58 wire contract. The
  alternatives — reusing `Protocol(...)` (a framing error it is not) or inventing an out-of-band
  encoding — are worse. The change is additive (wire discriminant `CE_UNSUPPORTED = 10`); every existing
  golden is byte-identical, and the golden/roundtrip/public-api coverage was extended in step.
- **Not making `EnvCodec::compose` fallible in `environment`.** The ruling permits either the
  adapter-panic default or plumbing `Result` through the explorer. The panic is chosen (the ruling's
  stated default), so `dissonance/environment` is **untouched** — the smallest surface that satisfies
  the contract. Making the seam fallible remains a clean future adjustment if `Result` plumbing is ever
  preferred.
- **A monotone portable work source (`mock::TickingWork`).** The mock gates need V-time to actually
  advance so the deadline check and snapshot boundaries are exercised; `ScriptedWork::at` is constant.
  `TickingWork` advances a fixed step per read — deterministic for a fixed exit script, which is all
  the portable gates need. Confined to `conductor::mock`.

## Known limitations / integrator notes

- **The `Moment` axis is effective V-time, not the retired-instruction count** (documented above). When
  task 59 lands the exact-count machinery, the adapter's offset keying should move to it; the change is
  confined to `adapter.rs`, and the seed-driven contract has no overrides to re-key today, so nothing
  downstream depends on the current stand-in.
- **`resolve` / reactive `run` is a hard no-op here** (seed-driven only, per the spec): the reactive
  loop is task 61. The wire and the adapter carry the machinery (a `Decision` stop stamps
  `pending_decision`; the next `run(resolve)` records the answer tail-completely), so task 61 lights it
  up without reshaping the seam.
- **One session = one VM.** `ControlServer` is not `Send` (a `Vmm`'s work source is a thread-affine
  counter on the box), so `run_session` keeps the server on the calling thread and the client on a
  spawned thread. A multi-VM explorer (N concurrent sessions) is out of scope; the fresh-VM restore
  discipline already anticipates it (each restore re-baselines the work counter).
- **`run(until)`'s deadline is enforced opportunistically, not as a hard force-exit.** It is observed at
  each step's V-time boundary; a guest that keeps taking VM-exits (any real workload — a compute-bound
  one is preempted by task-47's LAPIC-timer force-exit) is bounded within one exit of the deadline. A
  hard `run_until` force-exit at an arbitrary deadline was tried (PR #44 round 4) and **reverted**: on
  the box it armed `run_until` at the far sweep deadline every step, and because every run terminates
  before that deadline (the workload reboots first) each left an un-hit PMU/planner arm behind — stale
  state that accumulated across restores and diverged a `state_hash` on the 16th run (the `#34`/`#55`
  stale-arm class). Making the deadline a hard bound needs the backend to reset the `run_until` arm
  across runs — a `patched_kvm`/`pmu_sys` change **outside task-58's surface**, flagged for the task
  that owns the planner.
- **The box gate PASSED** (foreman-executed — run table above). The portable gates prove the *identical*
  server + adapter + sweep code against a deterministic guest; the box run swapped the mock guest for the
  real Postgres guest via `boot_linux_selected`, workload-blind, and cleared the milestone bar (8/8
  reproducible, 8 distinct futures, verbatim base replay).
