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
small **ledger** (a canary word + a signed **retry budget**) in a fixed-address, `mlock`'d,
`volatile` page and runs a bounded retry loop whose bookkeeping invariant — canary intact,
`0 ≤ budget < BUDGET_MAX` — holds on **every nominal iteration**, so the branch guarded by it is dead
code nominally. A **single-event upset** — a `CorruptMemory { gpa, mask }` that flips the canary (or
the budget's sign bit) at a `Moment` inside the loop — is the *only* way to reach the guarded branch,
which the supervisor detects and reports (`CAMPAIGN_BUG:` on the serial).

**The distinctive terminal (box reality, discovered during the box gate).** The original design had
the supervisor signal via isa-debug-exit (`OUT 0x60, 0xF4` → `Crash{Panic}`). **The box proved this
impossible**: the kata-derived container kernel has **no `CONFIG_X86_IOPL_IOPERM` and no
`CONFIG_DEVPORT`**, so a guest *process* cannot reach an I/O port at all — `campaign-super`'s boot
self-test reports `CAMPAIGN_IOPERM: FAILED`, `CAMPAIGN_IOPL: FAILED`, `CAMPAIGN_DEVPORT: FAILED`. So
the distinctive terminal is the **terminal path itself**, using what the guest *kernel* can produce
(`guest/linux/campaign-init.sh`):

- **bug** (supervisor exits non-zero) → `reboot -f` → triple-fault → `KVM_EXIT_SHUTDOWN` →
  **`Crash{Shutdown}`** — the reportable bug;
- **clean** (supervisor exits 0) → `halt -f` → the boot CPU HLTs → **`Quiescent`** — the benign
  terminal.

Both use the `-f` force path that skips `device_shutdown` (which strands once block I/O has been used —
see `pg-init.sh`). `campaign-super` still tries every port route (ioperm/iopl/`/dev/port`) so the same
image works on a kernel that *does* configure one, but the init terminal is the load-bearing signal.
The **toy** (`planted.rs`) mirrors exactly: triggered → `Crash{Shutdown}`, clean → `Quiescent`.

**Exact trigger conditions.** The bug fires iff the branched env's host schedule contains a
`CorruptMemory` whose `gpa` is the ledger word, whose `mask` is any single bit (a canary flip always
trips the guard), and whose `Moment` lands **while the loop is live** (`[base, base + loop_span]`). The
campaign is built **with no knowledge of this point**; it searches `(gpa, mask, Moment)` schedules until
one crashes. The toy's point is `Trigger::toy()` (gpa `0x3000`, bit `31`, offset `3`), a single point
of a 128-combination search space. On the box the ledger's guest-physical address is deterministic per
image (nokaslr + `MAP_FIXED` + `MAP_POPULATE` + `mlock`); the operator **pins** it from the boot
`CAMPAIGN_LEDGER_GPA:` line (read via `/proc/self/pagemap` under `CAMPAIGN_DEBUG`) and scopes
`--gpa-base` to it, so a targeted fault only ever corrupts the ledger — making any resulting crash
*the* planted bug (see the oracle limitation below).

**Why it is a genuine bug, not a fault detector.** The guarded branch encodes an assumption the code
relies on (the budget word is monotone-bounded) that is true in every nominal execution; the injected
upset makes the assumption false, exercising a code path that was never meant to run — the planted
defect. The "detection + report" is how the SDK-less guest surfaces the defect.

## The crash oracle mapping (workload-aware)

`CampaignOracle` (proptested, `tests/oracle_proptest.rs`) keys on the terminal **class**: **any `Crash`
or `Assertion` is the bug; a `Quiescent` (halt) or `Deadline` terminal is the clean run** — the
standard `TerminalOracle` rule, which applies because `/init` arranges the two terminals above (bug →
`Crash{Shutdown}`, clean → `Quiescent`). The emitted `Bug`'s fingerprint is the explorer's canonical
one, so a campaign bug dedups like any other.

**Limitation (documented):** the oracle sees only the terminal, not the serial, so it cannot tell the
*planted-invariant* crash from an *incidental* one (a fault that corrupts kernel memory and panics →
reboot → `Crash`). The campaign relies on the **pinned ledger gpa** so a targeted fault only ever
corrupts the supervisor's bookkeeping; the box gate therefore runs `--gpa-count 1` (pin exactly to the
ledger), making every found `Crash` unambiguously the planted bug — the `CAMPAIGN_BUG:` serial marker
is the human-visible confirmation. A wider `--gpa` search is possible but risks incidental crashes; a
serial-reading oracle (a future wire verb) would lift the restriction.

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
  **any crash (whatever its kind) or assertion is the bug; the clean `Quiescent` halt and every
  non-terminal stop are not**, and a reported bug carries the explorer's canonical fingerprint.
- **Planted-bug logic unit-tested** (`planted.rs`): the exact upset crashes (reboot → `Crash`), a
  clean run halts (`Quiescent`), each near-miss (wrong gpa / wrong bit / outside the window) is inert,
  a fixed reproducer replays a byte-identical `(stop, state_hash)`, and distinct envs diverge.

Demo (`cargo run -p conductor -- campaign mock`), verbatim:

```
base snapshot: sealed at V-time 1000 (1 attempt), capture state_hash 8fa7e948…4ec31a3a
planted bug found at branch 867 (seed 0x850bcc59a5668e35) after exploring 868 branches
  finding stop Crash@2025[2B], state_hash b2583c05…31359728
  fingerprint 7f3999e5d138ce223a347b398865cc44c43dbd40c4f3b067436c8da84e8c783a
  replay verification: 25/25 identical (crash reproduced bit-for-bit)
nominal control (seed only, no faults): Quiescent@2779 — no bug (adversity-gated, as required)
[conductor] campaign mock GATES PASS: planted bug found, reproduced 25/25, nominal control clean.
```

Found at branch **867** of a 128-combination space (naive geometric expectation ~128; the fixed
campaign stream's first hit). This is the naive seed-search order the spec asks for (~10²–10³). The
bug reboots to a `Crash`, the clean control halts to `Quiescent` — the same terminal convention the box
guest uses.

### Gate 1 — box (the milestone): **PASS** (run on the determinism box)

Box-only: patched KVM, det-cfl-v1 host, `/dev/kvm`, and `initramfs-campaign.cpio.gz`. The **identical**
`run_campaign` loop drives the real socket `Machine` against vmm-core's control server (with task-59's
host-fault enforcement) booting the campaign image. `conductor campaign box` prints the run table and
sets the exit code from `verify_campaign`. Runbook (per `docs/BOX-PINNING.md`; lease via
`scripts/box-window.sh`, which loads patched KVM on the first lease and reverts to stock `1396736` on
the last release):

```sh
# 1. Build the image (root, on the box / a linux-amd64 container). The kernel is
#    the shared task-36 bzImage (no kernel change); only the initramfs is built.
make -C guest fetch && make -C guest/linux campaign-image     # → guest/build/initramfs-campaign.cpio.gz
# 2. Bring-up (once per image): read the ledger gpa AND the loop span. campaign-init.sh
#    exports CAMPAIGN_DEBUG=1, so the boot serial prints `CAMPAIGN_LEDGER_GPA: canary=0x…`;
#    a --max-branches 1 run's nominal control prints Quiescent@<t>, so the loop span is
#    <t> − <base V-time>. Scope --window-hi below that span (a Moment past the natural
#    terminal poisons — see the known-limitations note).
# 3. The gate — lease a core, pin the ledger gpa (gpa-count 1 → every fault hits
#    the ledger, so any Crash is unambiguously the planted bug), window covering
#    the supervisor loop:
CORE=$(bash scripts/box-window.sh acquire t60gate)
taskset -c "$CORE" timeout 3600 target/release/conductor campaign box \
    --gpa-base 0x1fc9000 --gpa-count 1 --gpa-stride 0x1000 \
    --window-lo 0 --window-hi 700000000 --deadline-delta 5000000000 \
    --max-branches 4096 --replay-n 25
bash scripts/box-window.sh release t60gate   # reverts KVM to stock on last lease + verifies
```

The gate asserts: the campaign, started with no knowledge of which `(mask, Moment)` fires, **finds the
planted bug** — a `Crash` (the guest rebooted; the supervisor's `CAMPAIGN_BUG:` marker is on the
serial) the oracle calls — and the emitted reproducer **replays the identical crash (same terminal
`StopReason` and `state_hash`) 25/25**; the nominal-seed control run **halts (`Quiescent`), not a
crash**.

**RESULT — PASS** (determinism box, core 2, patched KVM loaded via `box-window.sh` then reverted to
stock `1396736` + verified; head `6ca8414`, shared task-36 `bzImage` `f06a34a790…`, ledger gpa
`0x1fc9000`):

```
base snapshot: sealed at V-time 473415720 (2 attempts), capture state_hash f6d23b75…8ce8b1de
planted bug found at branch 0 (seed 0x550e9f9e4d395f0b) after exploring 1 branches
  finding stop Crash@817175834 [65B], state_hash a8e08cef3d319355bf58b708121859159f33d8b60acbda6a1e00bbe74d7801b3
  fingerprint 897f7187443416687c923d9d30ef2e498630e50ef29db6a96a59c2871b0495cc
  replay verification: 25/25 identical (crash reproduced bit-for-bit)
nominal control (seed only, no faults): Quiescent@1265091633 — no bug (adversity-gated, as required)
campaign box GATES PASS: planted bug found, reproduced 25/25, nominal control clean.   (rc=0)
```

**What it shows.** With the ledger gpa pinned (bring-up) and the window `[0, 7×10⁸)` covering the ~7.9×10⁸-ns
supervisor loop (base `473415720` → clean-halt terminal `1265091633`), the **first** injected upset landed
inside the loop, flipped the canary, and the supervisor detected the impossible state and rebooted →
`Crash{Shutdown}` at V-time `817175834` — so the campaign found the planted bug at **branch 0**. The
emitted `Bug`'s reproducer (the recorded env: `seed 0x550e9f9e…` + the `CorruptMemory` at the ledger gpa,
composed genesis-complete) then **replayed the identical crash — same terminal `state_hash`
`a8e08cef…` — 25/25**, and the nominal-seed control (no fault) halted clean at `Quiescent@1265091633`.

**Branches / D5 note.** The search explored **1 branch to find** (the pinned-gpa scoping means the first
in-loop fault triggers), plus 25 replays + 1 control = 27 verification VM runs; the run completed well
inside the box lease. Because the find is at branch 0, this run does not exercise a large search space, so
it is not a meaningful **branches/hour** snapshot-throughput figure — that number wants an unpinned/wider
search (a `--gpa-count > 1` sweep, deferred with the D5 snapshot-performance work — the campaign already
supports it via the CLI). Cite this run for the *find + N/N reproduction* milestone; cite a future wide
sweep for the D5 branches/hour trigger.

### Gate 3 — standard suite + existing gates byte-identical: **PASS**

`build` / `nextest` (37 tests: lib incl. the toy exact-arrival proptests, campaign, oracle-proptest, and
the pre-existing task-58 loopback + determinism + task-65 recording) / `clippy -D warnings` (host **and**
`x86_64-unknown-linux-gnu`) / `fmt --check` / `cargo deny` / public-api snapshot all green on `conductor`.
**No existing crate touched** (vmm-core/explorer/environment/control-proto unchanged), so every other
crate's gate is byte-identical; the task-58 loopback + determinism proptests pass unchanged.
The guest kernel/initramfs golden (`MANIFEST.sha256`) is untouched — the campaign image is an additive
build target, not part of `run-tests.sh`'s repro/boot gate. `cargo deny` OK (`sha2` — the toy's
`state_hash` digest — is whitelisted and already in the lock via explorer/vmm-core; `Cargo.lock` gained
only the dependency edge, no version churn).

**Linux-target compile check is part of the gate.** The box binary lives behind `#[cfg(target_os =
"linux")]` (the `boxrun` module), which a Mac `cargo check` never compiles — so a Linux-only break is
invisible to the default gates. The gate list therefore includes, run from the Mac:

```sh
cargo check  -p conductor --all-features --target x86_64-unknown-linux-gnu
cargo clippy -p conductor --all-features --all-targets --target x86_64-unknown-linux-gnu -- -D warnings
```

(round-1 review caught an E0255/E0061 in the `boxrun` campaign path that the Mac gates could not see.)

## Round-1 review fixes (PR #55)

- **Linux-target build break (blocking).** In `boxrun`, `use conductor::campaign::run_campaign` collided
  (E0255) with the module's own `pub fn run_campaign`, and the call then bound to the 1-arg local fn
  (E0061). Aliased the import to `run_campaign_loop`; added the Linux-target `cargo check`/`clippy` to the
  gate list (above) so cfg(linux) code is compile-checked from the Mac.
- **`campaign-super.c` ledger is now `volatile` (blocking).** The ledger is mutated from outside the TU
  (the host `CorruptMemory` flip); without `volatile` `-O2` could hoist the `canary`/`budget` guards to
  constants and delete the planted mechanism. `volatile struct ledger *l` forces a real load on every
  access, so the injected flip is always observed.
- **Milestone replay bar floored at 25/25 (blocking).** `--replay-n` now floors at `REPLAY_BAR = 25` on
  both campaign paths — the flag can only *raise* the bar, never lower it, so a `--replay-n 1` run can no
  longer print `GATES PASS` at 1/1 below the spec gate.
- **Toy `run` honors a future deadline (blocking, mock fidelity).** A future deadline that falls before
  the run's terminal now returns `Deadline` at the deadline (not an overshoot to the terminal), matching
  the real `Machine`'s `StopConditions` semantics — pinned by
  `planted::tests::a_future_deadline_before_the_terminal_stops_there`.
- **SPDX header on `campaign-init.sh` (nit).** Added.

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
- **Distinct terminals via `reboot -f` (Crash) vs `halt -f` (Quiescent), NOT isa-debug-exit.** The
  first design signalled the bug through isa-debug-exit (`outb 0x60, 0xF4` → `Crash{Panic}`). The box
  gate proved a guest *process* cannot reach any I/O port on this kernel (no `CONFIG_X86_IOPL_IOPERM`
  / `CONFIG_DEVPORT`; the self-test confirms all three routes fail). So the distinctive terminal moved
  to the *terminal path*: the bug reboots (`Crash{Shutdown}`) and a clean run halts (`Quiescent`).
  This is strictly more robust — it depends only on the kernel's reboot/halt, which every kernel has —
  and it flips the oracle to the standard "a Crash is the bug." A `poweroff` was avoided (it strands in
  `device_shutdown` once block I/O is used — the pg-init lesson); both terminals use `-f` (force).
- **Wall-clock measured by `time`, not in-process.** `Instant::now`/`SystemTime::now` are
  determinism-lint-disallowed; the campaign reports the **branch count** and the operator wraps the box
  run in `time`, so branches/hour is computed for the record without a nondeterminism source in the code.

## Known limitations / integrator notes

- **The fault window must fall inside the run's natural terminal, or the run poisons.** Task-59's server
  applies a staged fault by *exact arrival* and (round-6) fails loud with `ScheduleUnsatisfiable` if the
  guest reaches its terminal with a fault still staged — i.e. a `Moment` **beyond** where the run ends
  can never land, so it aborts the campaign. Therefore `--window-hi` is bounded to the workload's
  fault-sensitive span past the base (the supervisor loop, ~10⁶ ns), **not** the far `--deadline-delta`.
  The CLI default is loop-scale (`1_000_000`); the box gate scopes it from the observed
  `CAMPAIGN_READY`→halt span. (The loud abort is the correct signal that the window is mis-scoped — it
  is not swallowed.)
- **The supervisor loop must out-span the snapshot seal point.** The base is sealed at the first
  snapshottable boundary at/after `CAMPAIGN_READY`, which the retry search overshoots by up to
  `snapshot_retry_step` ns. If the loop is shorter than that overshoot the seal lands *past* it (in the
  halt tail) and no injected fault can reach the fault-sensitive guard — the box gate proved this with the
  original 2×10⁶-iteration loop (a fault at base+0 did not trigger). The fix: `ITERS = 2×10⁸` (the loop
  spans ~8×10⁸ ns, base seals deep inside it) and a fine `snapshot_retry_step` (10⁴ ns, so the seal is
  close to `CAMPAIGN_READY`). The window (~7×10⁸ ns) then covers most of the loop.
- **The guest payload (`campaign-super.c` + the two scripts) is box-built and box-validated.** It builds
  and lints cleanly (shellcheck clean; C is Linux-only, not compilable on the dev Mac). The distinctive
  terminal is `reboot -f`/`halt -f` (not port I/O — the kernel has no `CONFIG_X86_IOPL_IOPERM` /
  `CONFIG_DEVPORT`, proven by the boot self-test), and the ledger gpa is pinned per image from the
  `CAMPAIGN_LEDGER_GPA:` boot line.
- **Triage/minimization is deliberately not built** (task-60 non-goal). The natural follow-on named by
  the spec is **ddmin over the `Moment` schedule** (and the `(gpa, mask)` space): shrink the emitted
  `Bug`'s host-fault schedule to the minimal upset that still reproduces the crash. It plugs in above
  `run_campaign` (post-find), consuming the emitted `Bug.env` — no loop/oracle change. Spec it as a
  follow-on; do not build it here.
- **One fault per branch, `CorruptMemory` only.** The first campaign searches single-upset schedules
  (the tested task-59 fault). `InjectInterrupt`-timing bugs and multi-fault schedules are a strict
  superset the same loop drives (widen `mint_fault_env`); out of scope for the milestone.

---

# task 68 — the chain protocol: live materialization gates over the socket

`src/materialize.rs` + `tests/materialize_loopback.rs` (portable) +
`tests/live_materialization.rs` (box-only, `#[ignore]`) + the `materialize`
bin subcommand. The engine itself is `dissonance/explorer`'s `Materializer`
(its IMPLEMENTATION.md §task 68); this crate drives it over the task-58
socket with the production `SpecEnvCodec` and real `recorded_env` — vmm-core
untouched (read-only surface), except the two foreman-assigned P2 hardenings
in `consonance/vmm-core/tests/seal_rate_sweep.rs` (unknown seal-error classes
now abort instead of miscounting as `Unrepresentable`; the §4 replay legs
assert they really reached `to_vtime`).

## The protocol (`run_materialize`, workload-blind)

Seal the base → build an `n ≥ 3` seal chain below it (`branch →
run(deadline) → seal` per hop, exemplars keyed by the **landed** synchronized
boundary per the GO grid-restricted ruling; every suffix a real
`recorded_env`) → gate (a): evict the deep exemplar's seal, materialize
(must be parent-rooted, suffix-only) → gate (b): evict the retained ancestor,
re-materialize (compose-folded, deeper) then evict everything (the
from-genesis worst case) — both must hash **bit-identically** → gate (c): run
a tail below the chain, fold its delta into a `bug_env` exactly as
`Explorer::report` would, `branch(base, bug_env)` → identical stop + hash.
`verify_materialize(report, baseline_ppm)` is a pure gate; the box passes
`TASK63_BASELINE_PPM = 15_463` (task-63 §4: 1.5463 % suffix cost — gate (a)
must beat it and quotes both numbers).

Portable status: **all gates green over the real wire** (mock guest,
`chain_gates_pass_over_the_socket`), with visible grid overshoot (off-grid
250-ns targets land on 100-ns boundaries and are keyed by the landing).

## FINDING (escalate to the foreman/integrator): the sequential-entropy splice

The substrate's `branch` **reseeds the entropy stream at every hop**
(`ControlServer::restore` → `reseed_entropy(seed)`; the stream is sequential,
`SeededEntropy::new`). A compose-fold collapses the intermediate reseed
points — `EnvCodec::compose` can re-key overrides but cannot express "reseed
at `Moment` m". Consequence: a folded materialization (or a composed
genesis-complete reproducer) is bit-identical to its hop-by-hop original
**iff no RDRAND/RDSEED draw lands inside a collapsed interval** (a draw
desyncs the stream sequence and its VMST-hashed position).

- Pinned portably:
  `materialize_loopback.rs::sequential_entropy_splice_diverges_a_collapsed_fold_documented_limit`
  (a draw-carrying mock script; the two-hop leg reproduces itself, the fold
  diverges — so it is the splice, not nondeterminism). If a substrate change
  (e.g. the task-93 ruling's named future option: Moment-keyed counter-mode
  entropy) makes reseeds splice-invariant, the pin fails loudly — retire it
  together with this note.
- Live blast radius: task-63 §2/§4/§4b never measured a *mid-chain reseed*
  (its §4 chain was a single no-reseed trajectory), so this is the first task
  whose gates exercise it. Post-readiness Postgres spans of a few M ns are
  expected draw-free (the kernel crng reseeds on ~minute timescales), so the
  box gates are expected green; a gate (b)/(c) hash mismatch on the box IS
  this finding materializing — `verify_materialize` stamps the diagnostic on
  exactly those failures. Escalate with the log; do not patch from this
  surface.

## Box runbook (gates 3–5; hand to the foreman if ssh is down)

```sh
# on the box, this branch, patched KVM loaded, image built:
taskset -c 2 timeout 7200 cargo test -p conductor --test live_materialization \
    -- --ignored --nocapture --test-threads=1 2>&1 | tee /tmp/live_materialization.log
# knobs: HOPS (3) · HOP_DELTA_VNS (2_000_000) · TAIL_DELTA_VNS (1_000_000)
#        CHAIN_SEED · READY_MARKER · KERNEL/INITRAMFS
```

Transcribe the `[REPORT]` block here. Core 2 is the standing frontier-gate
core (`docs/BOX-PINNING.md` is authoritative; note task-60's campaign gate
may hold a core — serialize). **Box-safety:** stock KVM = 1396736 — `pkill
-9 -f live_materialization` FIRST (separate ssh call, expect exit 255), wait
`lsmod | grep '^kvm_intel'` users=0, `rmmod kvm_intel kvm; modprobe kvm;
modprobe kvm_intel`, verify size on a FRESH connection.

## Known limitations

- **The mock restarts its exit script on every `branch`** (fork VMs replay
  the script from position 0), so a fold changes the *script phase* at a
  given V-time — an artifact the real guest does not have (its instruction
  stream is positionally continuous). The splice pin therefore takes **no
  seal inside the fold** (it hashes at deadline stops), isolating the
  entropy effect from the phase artifact.
- **Gate (c)'s "bug" is a deadline stop, not a crash**: the seed-driven v1
  vocabulary cannot mint a guest crash (no fault enforcement below the
  chain's suffixes yet — that is task 60/69 territory). The task-93 contract
  verified is exactly the gate's: the compose-folded, genesis-complete,
  `SnapId`-free artifact replays to an **identical stop + `state_hash`** on
  the production codec and real `recorded_env`.
- The chain uses one seed for every hop — not a harness convenience but the
  compose contract itself (`compose` fails closed on seed mismatches), i.e.
  exactly what `Explorer`'s exploit path produces (`mutate` preserves the
  base seed).

## Box-gate status: PASSED (2026-07-03, determinism box)

Portable gates: **all green** (explorer 91 + conductor 18 + the full
workspace's 1105, clippy `-D warnings`, fmt, deny; Linux cross-check of the
box-only harnesses). Box gates (a)/(b)/(c): **PASSED** — run via
`scripts/box-window.sh` (leased core 2, patched KVM 1400832, det-cfl-v1
host, release build; gate wall time 54.6 s), window released and **stock KVM
1396736 re-verified** after. The transcribed `[REPORT]`:

```
[REPORT] task-68 live_materialization (box)
base: sealed at V-time 442905523 (2 attempts)
hop         requested     landed(at)  overshoot  attempts
0           444905523      445147970     242447         1
1           447147970      447148315        345         1
2           449148315      449165676      17361         1
hot     base_at      447148315 -> at      449165676  depth      2017361  ratio   4491 ppm  folds 0  from_genesis false  state_hash c4ad3e0108510603dc116959e907ee49afa7bb7f79aec43efb232d868694c4a6
folded  base_at      445147970 -> at      449165676  depth      4017706  ratio   8944 ppm  folds 1  from_genesis false  state_hash c4ad3e0108510603dc116959e907ee49afa7bb7f79aec43efb232d868694c4a6
worst   base_at      442905523 -> at      449165676  depth      6260153  ratio  13937 ppm  folds 0  from_genesis true   state_hash c4ad3e0108510603dc116959e907ee49afa7bb7f79aec43efb232d868694c4a6
round-trip: folded == hot, worst == hot
reproducer: leg    Deadline@450224167       state_hash b1990e89370f4db934c68baa21c036fce92e527007fc58234746d309fd632cd0
reproducer: replay Deadline@450224167       state_hash b1990e89370f4db934c68baa21c036fce92e527007fc58234746d309fd632cd0 (== leg; bug_env 91 bytes, genesis-complete)
baseline: task-63 s4 = 15463 ppm (1.5463%); measured hot = 4491 ppm
[REPORT] GATES PASS: (a) parent-rooted depth beats the task-63 baseline; (b) eviction round-trip bit-identical (folded + from-genesis worst case); (c) composed reproducer replays with identical stop + state_hash.
```

Reading: **(a)** the deep exemplar materialized from its direct parent at
**4,491 ppm** (0.449 %) of a full from-scratch re-execution — beating the
task-63 §4 baseline (15,463 ppm / 1.5463 %) by ~3.4×. **(b)** the folded
(one collapsed hop, 2× depth) and from-genesis worst-case (3.1× depth)
re-materializations hash **bit-identically** to the hot seal — and, load-
bearing for the escalated finding: the real post-readiness Postgres spans
were **draw-free**, so the sequential-entropy splice did not materialize
live (exactly as predicted; the portable pin remains the documentation of
the boundary). **(c)** the 91-byte genesis-complete `bug_env` replayed below
the 3-deep chain to an identical stop + `state_hash` — the task-93
end-to-end gate on the production codec and real `recorded_env`. The grid
restriction is visible live (overshoots 242,447 / 345 / 17,361 ns; every
exemplar keyed by its landed boundary).

---

# IMPLEMENTATION — task 78 (reseed-aware compose: bit-identical folds under entropy draws)

The ruled fix for the task-68 escalated **sequential-entropy-splice** finding
(`docs/INTEGRATION.md` §6c ruling 3): the env format stores **reseed markers**
(`environment` — blob v4), `compose` splices them positionally, the adapter
records each branch reseed at relative 0 and re-anchors markers on the wire
(`explorer`), and the `ControlServer` re-executes each collapsed hop's reseed
at its exact recorded `Moment` (`vmm-core`, the task-59 exact-arrival plane).
Per-crate details in each crate's IMPLEMENTATION.md.

## The pin flips

Task 68's documented-limit pin
`sequential_entropy_splice_diverges_a_collapsed_fold_documented_limit` is
replaced by its positive twin
`sequential_entropy_fold_is_bit_identical_reseed_markers_flip_the_task68_pin`
(`tests/materialize_loopback.rs`): on the draw-carrying script the
compose-folded leg is now **bit-identical** to the hop-by-hop original, over
the real wire. The escalation note in this file's task-68 section and in
`live_materialization.rs` is retired with it (this section supersedes both).

## Portable gates

- `tests/reseed_fold_proptest.rs` — 256 random chains (depth 2–4, off-grid
  per-hop spans, random seeds) with RDRAND draws inside every collapsed
  interval: fold == hop-by-hop, always, over `SocketMachine` +
  `ControlServer` (the production codec + real `recorded_env`).
- `chain_gates_pass_on_a_draw_carrying_script` — the full task-68 chain
  protocol (gates a/b/c) green on a draw-carrying script, `bug_env` carrying
  one reseed marker per collapsed leg, all draw probes reading DRAWS.
- **Mock constraint:** the scripted mock restarts its exit script at every
  branch (script position is not in `VcpuState`), so portable draw-carrying
  comparisons need restart-phase-invariant shapes (the proptest's period-400
  script / the pin's alternating script; documented in the proptest module
  doc). A real guest has no such restart — unconstrained shapes are the box
  gate's job.
- **Mock work model rework (`src/mock.rs`):** `TickingWork` (tick on every
  read) made V-time advance on host-side bookkeeping reads, so an armed
  (exact-arrival) run had a different V-time cadence than an unarmed one —
  no schedule-carrying mock run could be compared against a plain one. The
  composition now uses `SharedWork` (a counter advanced only per serviced
  scripted exit — the box's guest-branches-only semantics) + a
  `CountingBackend` wrapper implementing exact arrival between exits.
  `TickingWork` remains for the one loopback test that composes its own VM.

## Draw probes (measured, never assumed)

`run_materialize` gained self-normalizing **draw probes**: each hop window
(and the tail) is re-run with a trailing reseed marker back to the same seed
at the landed boundary — a no-op iff no draw moved the stream, so the probe
hash differs from the plain leg's exactly when the window drew
(`MaterializeReport::{hop_draws, tail_draws}`). The live gate requires a
draw inside a collapsed window (`REQUIRE_DRAWS=0` waives).

## Box gate (FRONTIER) — PASSED 2026-07-03, determinism box

Runs 2/3 (HOPS=4, identical results — deterministic), leased core 2 via
`scripts/box-window.sh`, patched KVM 1400832, release build, gate wall
~135 s; stock 1396736 re-verified after, 0 leases:

- draw probes: **hops [false, false, false, true]; tail DRAWS** — hop 3's
  collapsed window and the tail both draw entropy (RDRAND via the guest CRNG
  path), measured on real KVM.
- gate (b): folded (1 fold) AND from-genesis worst case — the latter
  collapsing the draw-carrying hop-3 window — bit-identical to the hot seal
  (state_hash 8fa042da…); hot ratio 4 504 ppm beats the 15 463 baseline.
- gate (c): the 175-byte genesis-complete `bug_env` (5 reseed markers)
  collapses a reseed point that sits AFTER hop 3's draws — the exact shape
  the pre-task-78 code provably diverged on — and replays with identical
  stop + state_hash (725387df…).
- Run 1 (HOPS=3, default config) also passed with the tail drawing — the
  same seals/hashes as task-68's run (c4ad3e01…), confirming the no-marker
  compatibility surface live.

Runbook (foreman re-run): `/root/harmony-t78` on the box (branch pushed via
`ssh://hetzner/root/harmony-t78`, guest/build symlinked to harmony-pr44's
image), driver `/root/task78/gate.sh` (acquires the box-window lease, re-pins
it to the long-lived driver PID — the command-substitution PPID gotcha —
runs `HOPS=4 taskset -c $CORE timeout 7200 cargo test --release -p conductor
--test live_materialization -- --ignored --nocapture --test-threads=1`,
releases on EXIT). Logs: `/root/task78/gate{1,2,3}.log`.

## Known limitations / integrator notes

- Marker seeds are recorded at branch time from the env's seed (adapter) or
  the marker table (server); the server-side recorded reproducer stamps the
  floor reseed only for marker-carrying branches, so the no-marker
  `recorded_env()` byte shape is unchanged from task 59 (modulo the blob v4
  trailing empty table).
- Moment-keyed counter-mode entropy (task 93's deeper option) remains out of
  scope — this makes the *sequential* scheme compose-safe, per the ruling.
- Miri: run clean on `conductor --lib` (17 tests, `-Zmiri-disable-isolation`)
  after the mock gained the (delegating, SAFETY-commented) `map_memory`
  forward in `CountingBackend`.

---

# Coverage recovery (GitHub issue #69 / PR #71)

The CI compile break (`Step::SdkStop`, fixed in #68) had failed the
coverage/mutants/nextest jobs *before they could measure* since #63, so tasks
73/78/69 merged without their coverage gates actually running. #68 restored
the region floor to 93.27% by pinning `materialize.rs`, but the underlying
per-file coverage in `record.rs`/`campaign.rs`/`lib.rs`/`main.rs` stayed
thin. This pass adds real, behavior-pinning tests to ratchet it back up — no
coverage-padding (a test that executes a line without asserting on it).

## Region coverage, before → after (measured with CI's exact command, on the
determinism box — Linux, so `main.rs`'s `cfg(target_os = "linux")` code
compiles and counts, unlike a Mac-local run)

| File | Before | After (new tests) | After (+ `boxrun.rs` split) |
|---|---|---|---|
| `record.rs` | 68.00% | 87.66% | 87.66% |
| `campaign.rs` | 82.72% | 91.27% | 91.27% |
| `lib.rs` | 73.03% | 90.40% | 90.40% |
| `main.rs` | 0.00% | 61.14% (incl. box-only `mod boxrun`) | **89.69%** (portable dispatch only; `boxrun.rs` excluded, see below) |
| **Workspace TOTAL region** (`--fail-under-regions`) | **93.31%** | 94.25% | **94.76%** |

The floor moved from **93% → 94.5%** (a hair below 94.76%, not the measured
number itself) in `.github/workflows/quality.yml`; see "`main.rs`'s remaining
gap" below for why. `cargo llvm-cov nextest --all-features --lcov
--ignore-filename-regex '...' --fail-under-regions 94.5` passes (1445/1445
tests green on the box: 1409 previously green portable + the 21 new
`main.rs` bin unit tests + additions to `lib`/`campaign`/`record`/
`tests/recording.rs`). `clippy -D warnings`, `fmt --check`, and `cargo build
--all-features` all green on the box (Linux, `cfg(target_os = "linux")`
compiled, including a `--target x86_64-unknown-linux-gnu` cross-check from
the Mac); `cargo deny check` green on the Mac.

## What was added

- **Retry-loop coverage for the base-seal `NotQuiescent` mechanism.**
  `run_sweep` (`lib.rs`) and `run_campaign` (`campaign.rs`) both retry a
  `snapshot()` refusal by running further and re-sealing — previously
  untested (the mock composition never actually produces `NotQuiescent`, so
  the existing mock-backed integration tests never touched this path). Both
  are generic over the abstract `Machine` trait, so each got a small
  self-contained `Machine` test double (`RetryingMachine`) that refuses a
  configurable number of times, then succeeds, or reports a non-`Deadline`
  stop mid-retry — pinning the three real behaviors: retries-then-seals
  (attempts/vtime accounted for), gives up loudly past
  `snapshot_max_attempts`, and gives up loudly if the guest halts before a
  sealable boundary is found. `record.rs`'s analogous `seal_base` retry loop
  operates over the concrete `ControlServer<B: Backend>` (not the `Machine`
  seam), so it isn't fakeable the same way — see Known limitations below.
- **Golden-format tests for every `render_*_table` function**
  (`render_table`, `render_record_table`, `render_campaign_table`) — each
  pins the exact printed shape (the artifact IMPLEMENTATION.md/box gates
  quote verbatim) against a hand-built report, including the previously
  untouched `Crash`/`Decision`/`SnapshotPoint`/`Assertion` `fmt_stop` arms.
- **Per-gate failure-branch tests for `verify_record`, `verify_store_reload`,
  `verify_campaign`.** Each of these returns a list of independent failure
  strings, but only a couple of the ~10 branches were exercised (the "does
  this gate actually catch a broken invariant" question — AGENTS.md's
  "gate vacuity" criterion). Added one assertion per previously-untested
  branch: a lone run per seed, empty records, non-monotone stamps, a
  within-seed journal mismatch (`verify_record`); a terminal/record-count/
  journal-length/digest mismatch between the report and the reloaded trace,
  a `retained=false` row whose journal the store actually holds, and an
  unknown `TraceId` (`verify_store_reload`); a replay with a mismatched
  `StopReason` despite a matching hash (`verify_campaign`).
- **`main.rs`'s portable logic, previously 0% covered.** The CLI dispatch
  functions (`run_mock`, `run_campaign_mock`, `run_mock_materialize`,
  `finish`/`finish_campaign`/`finish_recording`) and free functions
  (`parse_u64_flexible`, `seeds`, `seeds_ok`, `parse_retain`) are plain,
  portable Rust — they were simply never unit-tested, not "trapped" in a way
  that needed extraction to the lib (a `bin` crate's `#[cfg(test)] mod tests`
  runs and is coverage-instrumented exactly like a lib's). Added 21 tests
  driving them directly (pass/fail exit codes via the real mock harness,
  plus `parse`/`seeds` edge cases). The two `#[cfg(not(target_os = "linux"))]`
  "refuses off Linux" stub tests are themselves gated identically to the
  functions they test, so they never resolve to the real `boxrun`-backed
  `run_box`/`run_campaign_box` on a Linux CI runner (which would attempt a
  real `/dev/kvm` boot — never something a coverage job should trigger).

## `main.rs`'s remaining gap: `mod boxrun` split into `src/boxrun.rs` (ruled)

The ~260 uncovered regions left in `main.rs` after the new tests (0.00% →
61.14%, not higher) were almost entirely `mod boxrun` (`#[cfg(target_os =
"linux")]`): the real `boot_server`/`run`/`run_campaign` that need
`/dev/kvm`, patched KVM, and the built guest images. This is genuinely
box-only glue — no portable test can drive it without a real boot, and the
coverage job's own self-hosted runner does not do live KVM boots
(`quality.yml`'s coverage-job comment: it deliberately runs without
`--ignored`, so box-gated tests never execute there).

**Ruling: split, don't leave in place.** `mod boxrun` was already a
self-contained block (its only seam into the parent module was `use
super::{BoxArgs, CampaignBoxArgs, finish, finish_campaign, finish_recording,
parse_retain, seeds}`, no entanglement with the portable dispatch logic
around it) — mechanically clean to lift into `dissonance/conductor/src/
boxrun.rs` verbatim, with `main.rs`'s `mod boxrun { ... }` replaced by
`#[cfg(target_os = "linux")] mod boxrun;`. Added
`dissonance/conductor/src/boxrun\.rs` to `.github/workflows/quality.yml`'s
`--ignore-filename-regex`, exactly like the existing
`kvm.rs`/`patched_kvm.rs`/`pmu_sys.rs`/`work_perf.rs` exclusions. Result:
`main.rs`'s reported % now reflects only the portable dispatch logic this
pass tested (89.69%, up from the 61.14% that was diluted by boxrun's
permanent 0%), so a future regression in that portable logic is no longer
maskable behind boxrun's floor. The floor itself moved **93% → 94.5%** in the
same file — see `docs/CODE-QUALITY.md`'s "Ratchet: 93% → 94.5%" entry for
the full before/after and reasoning.

Verified: `cargo build`/`clippy -D warnings --target x86_64-unknown-linux-gnu`
from the Mac (cross-check, matching this crate's existing Linux-target-check
convention from the task-60 section above) and the full Linux build/clippy/
fmt/coverage run on the box, all green; no behavior change (the module's
code is untouched, only its file location).

## Known limitations

- **`record.rs`'s `seal_base` retry loop is not directly unit-tested.**
  Unlike `run_sweep`/`run_campaign` (generic over the `Machine` trait, so a
  test double can inject `NotQuiescent`), `record.rs`'s `run_recording`
  drives `seal_base` over the concrete `ControlServer<B: Backend>` — real
  `NotQuiescent` there comes from `vmm.save_vm_state()` failing with a
  `ContractViolation` (an RNG mid-exit completion, a non-V-time-synchronized
  point), which needs orchestrating real backend/exit state, not a trait
  fake. This is vmm-core-layer behavior the crate does not own; the retry
  loop's happy path is exercised (every `tests/recording.rs` run seals a
  base), but the "attempts exceeded" / "halted before a sealable boundary"
  arms stay uncovered. Left as a residual gap rather than forcing a fragile
  fake of vmm-core internals.
- Same discipline as the rest of this file: every new test asserts a real
  input → real output (a table's exact bytes, a gate's exact failure
  message, an `ExitCode`), never just "the line executed."

---

# task 96 — the campaign stopwatch: hash-neutral phase timing for box runs

**Delegable, small.** Confined to `dissonance/conductor/`, no box gate of its own (this
task's Environment section names the next scheduled box run — task 69-M2 reruns, task
95-M2's gate (d), task 86 — as the first consumer of the live numbers, not a gate here).

## What landed, where

| File | Change | Role |
|---|---|---|
| `src/stopwatch.rs` (**new**) | `Phase`, `PhaseStats`, `Stopwatch`, `Mark`/`mark()` | the one module every `Instant::now` read in the crate lives in, under a single file-level `#[allow(clippy::disallowed_methods)]` |
| `src/campaign.rs` | `CampaignReport.{timing, wall_secs, branches_per_hour_x10}`; `run_campaign` wraps every phase; `render_campaign_table` grows a timing section; a progress line every 32 branches | wires the recorder through the search loop, observation-only |
| `src/boxrun.rs` | `boot_server` returns `(ControlServer, boot_us)`; the readiness line grows `, wall {secs}s`; `run_campaign` merges a single-sample `Boot` phase into the report | feeds the one measurement that happens before a campaign's own `Stopwatch` exists |
| `src/main.rs`, `tests/campaign.rs` | test literals updated for the 3 new report fields; `campaign_is_deterministic` extended into the task's hash-neutrality regression | keeps every existing assertion meaningful under the new fields |

## The hash-neutrality invariant, concretely

Every `Instant::now()` read in this crate — inside `Stopwatch::new`/`time`, and `mark()`
(used once, by `boxrun.rs`'s boot timer, so `Instant` reads never leak outside this file
even for the one measurement that predates a campaign's `Stopwatch`) — lives in
`stopwatch.rs`, under one `#[allow(clippy::disallowed_methods)]` with a `// not
order-observable:` justification. `Stopwatch::time` is a pure passthrough of its
closure's return value: it cannot change what a wrapped call returns or whether it
errors, so wrapping `machine.branch`/`run`/`hash`/etc. in `sw.time(Phase::X, || …)` is
mechanically transparent to the campaign's control flow. `PROGRESS_INTERVAL`-gated
printing reads `sw.stats()`/`sw.elapsed_secs()` for display only — nothing in
`run_campaign` branches on a duration.

**Regression pin:** `tests/campaign.rs::campaign_is_deterministic` runs the identical toy
campaign twice and asserts every report field except `timing`/`wall_secs`/
`branches_per_hour_x10` is bit-identical across the two runs (base hash, the found bug's
branch/seed/env/stop/hash/fingerprint, every replay's stop+hash, the nominal row) — the
task's gate 3.

## Phase mapping (exactly per spec)

`BaseSeal` wraps the whole base-seal retry loop as one span (not per-attempt). Per search
iteration: `Branch`/`Run`/`Hash`/`Harvest` (the SDK event round-trip)/`Judge` each get
their own span. The verify-replay loop's `branch`+`run`+`hash` are timed together as one
`Replay` span per iteration (matching "together" in the spec, not three sub-spans). The
nominal-control pass (`branch`+`run`+`hash`+its event harvest) is one `Nominal` span.
`Boot` (box-only) is merged in by `boxrun.rs` after `run_campaign` returns, via
`PhaseStats::single` — it cannot go through the campaign's own `Stopwatch` because the
boot happens first, outside `run_campaign` entirely.

Progress line (every 32 branches, `PROGRESS_INTERVAL`), matching the spec's example
shape:

```
[conductor] progress: branch 128/512, elapsed 1042s, avg us — branch 5210000 run 1200 hash 2900000
```

## Deviations considered

- **No `serde` dependency, no `--out` JSON flag — a direct conflict in the spec, resolved
  in favor of the harder constraint.** The spec's §3 says `timing` should be "serialized
  into the campaign's `--out` JSON" and read like `serde`-tagged snake_case; the spec's
  Environment section says, flatly, "No new dependencies." Both cannot hold: `conductor`
  today has **zero** `--out`/JSON machinery anywhere (verified by grep — no `--out` flag,
  no `serde_json` use, no `Serialize` on any report type, in campaign/sweep/materialize
  modes alike), so satisfying §3 literally means adding `serde`+`serde_json` as new direct
  dependencies. I read "no new dependencies" as the binding rule (explicit, environment-
  scoped, unambiguous) over "serialized into JSON" (descriptive, assumes infrastructure
  that doesn't exist in this crate) — the spec's Environment section is where a task
  states its hard boundary, and this task's says nothing about wiring JSON output being
  in scope. **What ships instead:** `Phase::as_str()` returns the exact stable snake_case
  name (`"base_seal"`, `"branch"`, …) a future `#[derive(Serialize)]` would emit, pinned by
  a unit test, so the naming contract is locked in today. Wiring an actual `--out` JSON
  (adding `serde` to `Cargo.toml`, deriving `Serialize` on `CampaignReport` with
  `#[serde(rename = "...")]` matching `as_str()`) is a small, well-scoped follow-up for
  whichever task first needs machine-readable campaign output — not invented here.
- **The timing section in `render_campaign_table` is conditioned on
  `!report.timing.is_empty()`**, not on the presence of any specific field. This keeps
  every report built before this task (synthetic test reports with a default-empty
  `timing`) rendering byte-for-byte unchanged — `render_campaign_table_pins_the_found_bug_format`
  is untouched with a comment explaining why; the sibling pin test
  (`render_campaign_table_pins_the_no_find_and_nominal_bug_format`) was extended to also
  carry non-empty `timing`, pinning the new section's exact shape instead of just noting
  it's untested.
- **`branches_per_hour_x10` avoids floats even in the rendered "X.Y branches/hour" line**
  (`x / 10` and `x % 10` printed as `{}.{}`) rather than casting to `f64` — the spec
  permits a float in a formatted print, but the integer form is exactly as readable and
  keeps the whole timing path free of float rounding even at display time.
- **`Mark`/`mark()` is an addition beyond the spec's sketched `Stopwatch`/`PhaseStats`/
  `Phase` API.** `boxrun.rs` needs to measure the boot-to-ready span, which happens before
  a campaign's `Stopwatch` exists; without an opaque handle it would need its own
  `Instant::now()` call, violating "all `Instant` reads confined to `stopwatch.rs`"
  (invariant 1). `Mark` is a one-field newtype wrapping `Instant`, constructed and read
  only through `stopwatch.rs` functions — every literal `Instant::now()` in the crate
  still lives in the one file the single clippy allow covers.

## Known limitations / integrator notes

- **A fast toy campaign can report `wall_secs == 0`** (`Stopwatch::elapsed_secs` truncates
  to whole seconds; the portable toy campaign typically finds its planted bug in well
  under a second). `branches_per_hour_x10` is `0` in that case by the documented contract
  (no division panic, no claimed-infinite rate) — this is expected on the toy path, not a
  bug; the box path's multi-minute-to-hour runs are the numbers this task exists for.
- **No server-side per-verb timing** (`consonance/vmm-core/control.rs`) — out of surface
  per the spec's non-goals; the conductor-side wrapper conflates client wait + socket +
  server work, the named follow-up if the box numbers point into the socket.
- **`Stopwatch`/`PhaseStats` carry no `Serialize`** (see Deviations above) — a future task
  wiring a conductor `--out` JSON should add `serde` to `conductor`'s dependencies at that
  point and derive against `Phase::as_str()`'s existing names, not invent new ones.

## Gates

Standard suite (`build`/`nextest`/`clippy -D warnings`/`fmt --check`/`cargo deny`) green
on macOS; `cargo check`/`cargo clippy --target x86_64-unknown-linux-gnu -- -D warnings`
green from the Mac (the `boxrun.rs` changes are `cfg(target_os = "linux")`, invisible to
a default Mac build — see the task-58/coverage-recovery sections above for why this
crate's gate list always includes the Linux-target cross-check). 62 lib unit tests + 21
bin unit tests (incl. `stopwatch`'s 7 new nearest-rank/passthrough/`as_str` tests and
`campaign`'s new timing-population test) + every existing integration suite, all green;
no `unsafe` in this crate, so no Miri gate applies. No box gate — this task's spec
explicitly has none; the live numbers get their first real exercise on the next scheduled
box run.
