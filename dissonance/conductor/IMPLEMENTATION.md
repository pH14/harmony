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
16 tests) and the adapter's (`explorer/src/adapter.rs`, 10 tests incl. the three ruling panics).

Demo (`cargo run -p conductor -- mock --seeds 8 --runs 2`): 8 seeds × 2 runs, every seed bit-identical
across its runs, **8 distinct futures**, replay == capture, GATES PASS.

### Gate 2 — box (the milestone): **handed to the foreman** (see below)

### Gate 3 — standard suite on every touched crate: **PASS**

`build` / `nextest` / `clippy -D warnings` / `fmt` / `cargo deny` green on control-proto (55),
environment (91, untouched), explorer (57), vmm-core (285), conductor (6). No golden re-blessing —
the server is additive; existing `live_*` gates are byte-identical (nothing in the determinism path
changed; `effective_vns` and `state_blob` are unchanged). Public-API snapshots refreshed for vmm-core,
explorer, and the new conductor crate.

## The box gate — runbook for the foreman

Not run in this session, for three converging reasons, none of them a code gap:

1. **No-push constraint.** This worktree is not pushed (per the task instructions), so `conductor box`
   cannot be built on the box until the foreman checks the branch out there.
2. **Shared patched KVM is in use.** At the time of writing, PR-33's `live_runc_postgres` gate is
   running on core 2 holding the patched KVM module (`lsmod kvm` = 1400832). The box's `run-patched.sh`
   flow refuses to (re)load or revert KVM while `kvm_intel` is in use, and reverting mid-run would break
   PR-33 — so a patched gate cannot be sequenced until that run finishes and reverts.
3. **Image.** The bare-Postgres image (`initramfs-postgres.cpio.gz`) is not built on the box; only the
   runc-Postgres (`initramfs-docker.cpio.gz`) and k3s images are staged. The box mode takes
   `--initramfs` so it can reuse the docker image, or the foreman can build the bare image.

The complete live path is delivered and **cross-compiles + clippies clean for `x86_64-unknown-linux-gnu`**
(the box-only `boot_linux_selected` + `perf_event` path). To run gate 2 on the box, per
`docs/BOX-PINNING.md` (use a spare core — 1 or 3 — while core 2 is occupied; never core 4/5–7):

```sh
# On the box, with the branch checked out and the patched KVM loaded for THIS run
# (coordinate: only load/revert when `lsmod | awk '$1=="kvm_intel"{print $3}'` == 0):
taskset -c 3 timeout 3600 cargo run -p conductor --release -- box \
    --seeds 8 --runs 2 \
    --initramfs initramfs-docker.cpio.gz \
    --ready-marker 'database system is ready to accept connections'
# ALWAYS revert KVM to stock afterwards and verify: lsmod | grep '^kvm ' == 1396736
```

**What gate 2 asserts** (the milestone bar): one snapshot **mid-workload, post-readiness** (the
`--ready-marker` step drives the live guest there before the sweep seals — this is the *only*
workload-aware policy in the path; the server + adapter stay workload-blind), then **N ≥ 8 seeds**, each
run **twice** → bit-identical `state_hash` per seed, **≥ 2 distinct hashes across seeds**, and
`replay(base)` → identical hash to the capture. `conductor box` prints exactly this run table and the
gate verdicts (`verify(&report, 2)`); **record the printed table here** once it runs.

> **Honest expectation on divergence (from the task-40 branching demo).** A snapshot sealed *after* the
> kernel CRNG is seeded (which happens before the first console byte, so any post-readiness seal is)
> makes the entropy fork surface only in the host-side entropy bookkeeping — the branches are otherwise
> byte-identical guests. That still satisfies gate 2's letter: the entropy stream position rides the
> `VTIM` hash chunk (`wire_snapshot_hashing` is on), so distinct seeds still produce **distinct
> `state_hash`es**. Gate 2 asks for ≥ 2 distinct hashes, not guest-observable divergence (unlike the
> stricter task-40 demo). This is the expected, correct shape for a mid-workload seal; it is not a
> weaker gate, it is the honest one for this snapshot phase.

**Box run table (to be filled by the foreman):**

```
base snapshot: sealed at V-time <…> (<…> attempts), capture state_hash <…>
seed                  run  stop                     state_hash
<…>
replay(base): state_hash <…> (== capture)
GATES: <PASS/FAIL>
```

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
- **The box gate has not been executed** — see the runbook. The portable gates prove the *identical*
  server + adapter + sweep code against a deterministic guest; the box run swaps the mock guest for the
  real Postgres guest via `boot_linux_selected`, workload-blind. When it runs, paste the run table above.
