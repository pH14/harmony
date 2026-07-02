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
  into the current Modulation's recording, and `recorded_env` emits the branch-local delta — always
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

**The box milestone gate ran and PASSED**, on the determinism box (core 2 per the
`docs/BOX-PINNING.md` frontier ruling; patched KVM loaded for the run; **reverted to stock `1396736` +
verified** after; the ht42 bare-Postgres image `initramfs-postgres.cpio.gz` + the current SMP bzImage).
First green on head `0b28d3f`; **re-verified green on head `cab4120`** after the round-5 revert of the
deadline force-exit (round 4's `step_until` regressed determinism — see the limitation note below — and
the intermediate head `30417d8` diverged one run; the revert restored the byte-identical golden run,
same hashes as `0b28d3f`). Run table, verbatim (`cab4120`, log `/root/pr44-gate3.log`):

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

---

# IMPLEMENTATION — task 60 (first campaign: find a planted bug, reproduce it N/N)

**FRONTIER · the milestone the project exists for.** Task 60 extends this bin (the spec's "extend
task 58's demo bin") with the whole first campaign: a workload with a **planted, fault-triggerable
bug**, a crash oracle, the seed-driven outer loop searching, and the emitted `Recorded` environment
replaying the find bit-identically, N/N. It validates the Modulation/Progression mechanism on a real
seeded bug — the deliberate first step of the fuzzer-validation discipline ("prove the finder against
seeded bugs before investing in search cleverness"). Depends on tasks **58** (the loop) and **59**
(host-fault enforcement).

## Surface (and why it merges cleanly after 59)

Task 60's changes are confined to **`dissonance/conductor/` + `guest/`** — vmm-core, explorer,
environment, and control-proto are **untouched**. The campaign stages its host-fault schedule
entirely through the *existing* `Machine::branch` env (an `EnvSpec` carrying `Action::Host`
overrides): task-59's server decodes that env, stages each fault, and applies it between instructions
at its `Moment`. So the campaign code depends on nothing new in the substrate — it compiles against
`main` today and runs for real on the box once 59 is merged. The `Machine` seam is unchanged; there is
no new verb.

| File | Change | Role |
|---|---|---|
| `dissonance/conductor/src/campaign.rs` (**new**) | `run_campaign` + `CampaignOracle` + `mint_fault_env` + `verify_campaign` | the campaign loop, the workload-aware crash oracle mapping, the seeded fault-schedule minter, the N/N gate |
| `dissonance/conductor/src/planted.rs` (**new**) | `ToyPlantedMachine` + `Trigger` | the portable planted-bug guest (a controllable `Machine`) the campaign is proven against |
| `dissonance/conductor/src/main.rs` | `campaign {mock,box}` subcommands; `boxrun` refactor | the demo/milestone bin; `boot_server` factored out so the sweep and campaign box paths boot identically |
| `guest/linux/campaign-super.c` (**new**) | the supervised process with the planted bug + the isa-debug-exit crash channel | the box workload's added buggy component |
| `guest/linux/campaign-init.sh` (**new**) | the `/init`: postgres workload → the supervisor | seals the base at `CAMPAIGN_READY`, runs the fault-sensitive loop |
| `guest/linux/build-campaign-image.sh` (**new**) + Makefile `campaign-image` | builds `initramfs-campaign.cpio.gz` | `build-postgres-image.sh` + the static `campaign-super` + the campaign `/init` |

## The planted bug (exact trigger, both guests)

The bug is the same *finder-visible contract* on the toy and the box: a supervised process keeps a
small **ledger** (a canary word + a signed **retry budget**) and runs a bounded retry loop whose
bookkeeping invariant — canary intact, `0 ≤ budget < BUDGET_MAX` — holds on **every nominal
iteration**, so the branch guarded by it is dead code nominally. A **single-event upset** — a
`CorruptMemory { gpa, mask }` that flips the budget's sign bit (or the canary) at a `Moment` inside
the loop — is the *only* way to reach the guarded branch, which the supervisor reports through a
**distinctive terminal**:

- **Box** (`guest/linux/campaign-super.c`): a byte `OUT FAIL_CODE(0x60), 0xF4` → vmm-core's
  isa-debug-exit → `TerminalReason::DebugExit{0x60}` → `map_terminal` → **`Crash{Panic}`**, preceded
  by the serial marker `CAMPAIGN_BUG: retry-budget invariant violated (…)` (the SDK-less "assertion
  rides the serial text"). No upset ⇒ `CAMPAIGN_DONE` + a forced reboot ⇒ backend `Shutdown` ⇒
  **`Crash{Shutdown}`** — the *clean* terminal of this workload.
- **Toy** (`planted.rs`): the same two terminals, `Crash{Panic}` (planted bug) vs `Crash{Shutdown}`
  (benign), so the *identical* oracle mapping runs against both.

**Exact trigger conditions.** The bug fires iff the branched env's host schedule contains a
`CorruptMemory` whose `gpa` is the ledger word, whose `mask` is the guard/sign bit, and whose `Moment`
is inside the loop's live window — i.e. a specific `(gpa, mask, Moment-window)` point. The campaign is
built **with no knowledge of this point**; it searches `(gpa, mask, Moment)` schedules until one
crashes. The toy's point is `Trigger::toy()` (gpa `0x3000`, bit `31`, offset `3`), a single point of a
128-combination search space (4 gpa × 8 Moment slots × 4 mask bits). On the box, the ledger's
guest-physical address is pinned by nokaslr + `MAP_FIXED` + `mlock`; the operator scopes the campaign's
`--gpa-*` around it (read via `/proc/self/pagemap` in a `CAMPAIGN_DEBUG` bring-up boot — the supervisor
prints `CAMPAIGN_LEDGER_GPA`).

**Why it is a genuine bug, not a fault detector.** The guarded branch encodes an assumption the code
relies on (the budget word is monotone-bounded) that is true in every nominal execution; the injected
upset makes the assumption false, exercising a code path that was never meant to run — the planted
defect. The "detection + report" is how the SDK-less guest surfaces the defect over the serial.

## The crash oracle mapping (workload-aware)

`CampaignOracle` (proptested, `tests/oracle_proptest.rs`) is the one piece of workload knowledge:
a `Crash` whose leading kind byte is **not** the benign reboot terminal (`CRASH_KIND_SHUTDOWN = 2`),
or an `Assertion`, is a bug; the benign reboot and every non-terminal stop are not. The kind byte is
the one the R2 adapter prepends to `Crash.info` (`stop_from_wire`: Panic→0, TripleFault→1, Shutdown→2).
The emitted `Bug`'s fingerprint is the explorer's canonical one (delegated to `TerminalOracle`), so a
campaign bug dedups like any other. Interpreting the Shutdown-is-clean convention is the workload-aware
caller's job (task-58 IMPLEMENTATION.md said as much) — which is why the mapping lives in the campaign,
not the substrate.

## Acceptance gates

### Gate 2 — portable (macOS + Linux, toy path): **PASS**

The identical `run_campaign` loop the box drives, against `ToyPlantedMachine`:

- **`tests/campaign.rs`** — the milestone's letter on the toy: the campaign, started with no knowledge
  of the trigger, **finds the planted bug and replays the identical crash (same `StopReason` + same
  `state_hash`) 25/25**, and a nominal-seed control does not crash (`verify_campaign` clean). Plus:
  the campaign is a pure function of `(seed, machine)` (a rerun finds at the identical branch with
  identical hashes); the finder **adapts to a replanted bug** (not hard-coded to one trigger); and an
  out-of-search-space trigger **fails the gate loudly** (no silent pass).
- **`tests/oracle_proptest.rs`** (≥512 cases) — the crash-oracle mapping proved total and consistent:
  a crash is a bug iff its kind ≠ the benign terminal, assertions are always bugs, non-bug stops never
  are, the mapping is parametric in the benign kind, and a reported bug carries the canonical
  fingerprint.
- **Planted-bug logic unit-tested** (`planted.rs`): the exact upset crashes, each near-miss (wrong
  gpa / wrong bit / outside the window) is inert, a fixed reproducer replays a byte-identical
  `(stop, state_hash)`, and distinct envs diverge.

Demo (`cargo run -p conductor -- campaign mock`), verbatim:

```
base snapshot: sealed at V-time 1000 (1 attempt), capture state_hash fbacdab8…2db5aeb1
planted bug found at branch 867 (seed 0x850bcc59a5668e35) after exploring 868 branches
  finding stop Crash@1962[2B], state_hash 85480f41…47fc39a7
  fingerprint 738172063f9232bb3fb0113cfe1e79b4d0ead6019d27f3126c2a3defe4060596
  replay verification: 25/25 identical (crash reproduced bit-for-bit)
nominal control (seed only, no faults): Crash@2716[4B] — no bug (adversity-gated, as required)
[conductor] campaign mock GATES PASS: planted bug found, reproduced 25/25, nominal control clean.
```

Found at branch **867** of a 128-combination space (naive geometric expectation ~128; the fixed
campaign stream's first hit). This is the naive seed-search order the spec asks for (~10²–10³).

### Gate 1 — box (the milestone): **handed to the foreman** (needs 58 + 59 merged + the built image)

Box-only: patched KVM, det-cfl-v1 host, `/dev/kvm`, and `initramfs-campaign.cpio.gz`. The **identical**
`run_campaign` loop drives the real socket `Machine` against vmm-core's control server (with task-59's
host-fault enforcement) booting the campaign image. `conductor campaign box` prints the run table and
sets the exit code from `verify_campaign`. Runbook (per `docs/BOX-PINNING.md`; lease via
`scripts/box-window.sh`):

```sh
# Build the image (root, on the box / a linux-amd64 container):
make -C guest fetch && make -C guest/linux campaign-image     # → guest/build/initramfs-campaign.cpio.gz
# Bring-up (once): pin the ledger gpa to scope the search tightly.
#   Boot with CAMPAIGN_DEBUG=1 in the env and read the `CAMPAIGN_LEDGER_GPA:` serial line.
# The campaign (patched KVM loaded for THIS run; coordinate — only load/revert when
# `lsmod | awk '$1=="kvm_intel"{print $3}'` == 0):
taskset -c <core> timeout 3600 cargo run -p conductor --release -- campaign box \
    --gpa-base <pinned_gpa_page> --gpa-count 8 --gpa-stride 0x1000 \
    --window-lo 0 --window-hi 2000000000 \
    --max-branches 4096 --replay-n 25
# ALWAYS revert KVM to stock afterwards and verify: lsmod | grep '^kvm ' == 1396736
```

The gate asserts: the campaign **finds the planted bug** (a `Crash{Panic}` the oracle calls) and the
emitted reproducer **replays the identical crash — same `state_hash` at the terminal stop — 25/25**; a
nominal-seed control run does not crash. **Record in the table below** (foreman fills from the box run):
branches explored, wall-clock, **branches/hour** (the D5 snapshot-performance trigger — cite it here
when D5 is specced).

```
=== TASK-60 BOX GATE — RESULT (foreman to fill) ===
image: initramfs-campaign.cpio.gz   kernel: <bzImage sha>   head: <commit>
base sealed at V-time <…>, capture state_hash <…>
planted bug found at branch <N> (seed <…>) after exploring <B> branches
  finding stop Crash{Panic}@<…>, state_hash <…>
  replay verification: 25/25 identical
nominal control: Crash{Shutdown}@<…> — no bug
branches explored: <B>   wall-clock: <…>   branches/hour: <…>   (D5 trigger)
```

### Gate 3 — standard suite + existing gates byte-identical: **PASS**

`build` / `nextest` (28 tests: 12 lib, 4 campaign, 6 oracle-proptest, 6 pre-existing task-58 loopback +
determinism) / `clippy -D warnings` / `fmt --check` / `cargo deny` / public-api snapshot all green on
`conductor`. **No existing crate touched** (vmm-core/explorer/environment/control-proto unchanged), so
every other crate's gate is byte-identical; the task-58 loopback + determinism proptests pass unchanged.
The guest kernel/initramfs golden (`MANIFEST.sha256`) is untouched — the campaign image is an additive
build target, not part of `run-tests.sh`'s repro/boot gate. `cargo deny` OK (`sha2` — the toy's
`state_hash` digest — is whitelisted and already in the lock via explorer/vmm-core; `Cargo.lock` gained
only the dependency edge, no version churn).

## Deviations considered

- **Portable path is a toy `Machine`, not the socket adapter against the mock `ControlServer`.** The
  mock server on `main` rejects a host-fault-carrying env as `Unsupported` (task-59 enforcement is not
  merged), so a mock-server campaign with faults cannot branch until 59 lands. The toy is the honest
  portable altitude anyway: gate 2 is about the **campaign loop + oracle mapping + planted-bug logic +
  N/N verification**, not the wire (task 58 already proves the wire). The toy makes the planted trigger
  fully controllable, so the finder is exercised deterministically. The box gate exercises the real
  server + 59 + wire.
- **The fault schedule rides the branch env, not a separate `perturb` call.** The spec's
  "branch(seed′ + a small seeded host-fault schedule)" maps directly onto task-59's branch-env host
  overrides (`EnvSpec::perturb` into the blob), so the campaign needs **no new `Machine` verb** and my
  surface stays `conductor` + `guest`. A `perturb`-verb path would have forced an explorer-seam change.
- **isa-debug-exit (`Crash{Panic}`) as the distinctive terminal.** On this substrate a triple fault and
  a `poweroff`/reboot both surface as backend `Shutdown` (`Crash{Shutdown}`), which is the workload's
  *clean* terminal — so the bug cannot signal through Shutdown. `DebugExit{≠0}` → `Crash{Panic}` is the
  only distinct non-Shutdown terminal available, hence the `outb 0x60, 0xF4` crash channel. Documented
  as needing `ioperm` (CONFIG_X86_IOPL_IOPERM, default y) — the supervisor falls back to a nonzero
  `_exit` + serial marker where it is absent (the foreman then uses the `/dev/port` path).
- **Wall-clock measured by `time`, not in-process.** `Instant::now`/`SystemTime::now` are
  determinism-lint-disallowed; the campaign reports the **branch count** and the operator wraps the box
  run in `time`, so branches/hour is computed for the record without a nondeterminism source in the code.

## Known limitations / integrator notes

- **Box gate is foreman-executed** (the established frontier pattern; every box gate in tasks 58/59/63/65
  is). The portable gates prove the identical `run_campaign` + oracle + verification code against a
  deterministic guest; the box run swaps the toy for the real Postgres-campaign guest, workload-blind.
- **The guest payload (`campaign-super.c` + the two scripts) is box-built and box-validated.** It builds
  and lints cleanly (shellcheck clean; C is Linux-only, not compilable on the dev Mac), but the trigger
  gpa pinning, `ioperm` availability, mmap-determinism, and search-space sizing are **box-iterated** by
  the foreman — the CLI `--gpa-*`/`--window-*` flags exist exactly so the search is tuned to the real
  image within the lease (the spec's "make the trigger threshold tunable").
- **Triage/minimization is deliberately not built** (task-60 non-goal). The natural follow-on named by
  the spec is **ddmin over the `Moment` schedule** (and the `(gpa, mask)` space): shrink the emitted
  `Bug`'s host-fault schedule to the minimal upset that still reproduces the crash. It plugs in above
  `run_campaign` (post-find), consuming the emitted `Bug.env` — no loop/oracle change. Spec it as a
  follow-on; do not build it here.
- **One fault per branch, `CorruptMemory` only.** The first campaign searches single-upset schedules
  (the tested task-59 fault). `InjectInterrupt`-timing bugs and multi-fault schedules are a strict
  superset the same loop drives (widen `mint_fault_env`); out of scope for the milestone.
