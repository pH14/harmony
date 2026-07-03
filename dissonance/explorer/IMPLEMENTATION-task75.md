# Task 75 — explorer surface + frontier box-gate handoff

Task 75 (Phase J1) has two surfaces. This file covers the **explorer** changes
(the shared fingerprint schema + the probe mechanism — portable, gated here) and
**hands off the frontier box gates 4 & 5** to the foreman with a complete live
path. The delegable Elle checker is `dissonance/oracle-elle/` (see its
`IMPLEMENTATION.md`).

## What landed in `explorer` (portable, all gates green)

### The shared `Bug` fingerprint (pinned here, `src/fingerprint.rs`)

A versioned `sha2` digest over three stable coordinates, superseding task 12's
stop-reason-only `dissonance.explorer.bug.v1` digest:

1. **`TerminalSig`** — oracle id + anomaly class + normalized detail
   (participating key set / assertion id / crash-marker class) + `StopReason`
   discriminant. No raw addresses.
2. **`FaultCoord`** — the plane+class fault set. Provisional mint of a *pure
   trace oracle* is `FaultCoord::none()` (it is schema-blind over an opaque
   `Environment`); the coordinate is a first-class input so the campaign's
   schema-aware path and task 76 populate it. **Deviation, documented in the
   module:** hashing the opaque `env` was rejected (violates "never `Moment`s",
   over-splits past usefulness).
3. **`VTimeCoord`** — the quantized V-time (bracket `FINGERPRINT_VTIME_BRACKET`,
   task 76 replaces with the inevitability bracket).

Public API: `mint_fingerprint`, `TerminalSig`, `FaultCoord`, `VTimeCoord`,
`FINGERPRINT_DOMAIN`, `FINGERPRINT_VTIME_BRACKET`, `StopReason::discriminant`.
`TerminalOracle` (task 12's site) re-mints through it; the behavior-equivalence
reference tracks the same scheme, so all 102 explorer tests stay green (goldens
updated). **The V-time now lives in coordinate 3 (quantized), not the raw
digest** — two crashes in one bracket now share a fingerprint (a test was
updated to pin this).

> **Matcher minting site (task 66) — out of my surface, flagged for the
> integrator.** The spec says update *both* minting sites, but `dissonance/matcher`
> is not in task 75's surface list. `matcher::router::never_fingerprint` should
> adopt `explorer::mint_fingerprint` (oracle id `"matcher"`, class per the
> `never` role, detail = matched attr bytes, `FaultCoord::none()`, the record's
> `Moment`). One-line change, no logic shift; left to the integrator to keep the
> surface boundary clean.

### The probe mechanism (`spine.rs` + `engine.rs`)

- `spine.rs` extends (does not redefine) task 64: `ProbePlan { horizon:
  StopConditions }` and `trait ProbeOracle { plan(&RunTrace) -> Option<ProbePlan>;
  judge_probe(&RunTrace, &RunTrace) -> Option<Bug> }`.
- `Explorer::probe(oracle, original, terminal: SnapId)` — engine plumbing (a
  function, not a loop change): from a live terminal snapshot, `branch` a
  **throwaway** branch with a quiesced env (a fresh seed → nominal answers, empty
  fault schedule), run to `plan.horizon` **snapshot-neutral** (decline
  decisions, step past any `SnapshotPoint` without sealing), record the probe's
  genesis-complete `RunTrace` (`compose(original.env, probe_delta)`), and call
  `judge_probe` (pure). **Never admitted to the `Archive`**; touches neither the
  archive, the frontier, nor the seal cache.
- `tests/probe.rs` proves it over an in-crate convergence machine: a poisoned
  terminal is caught as a `Bug` on a discarded branch; the **uncontamination
  property** holds (frontier bytes + live snapshot set + seal cache identical
  before/after; probe mints no snapshot); a healthy terminal judges clean; a
  `None` plan skips the forward run.

## Box gates 4 & 5 — handoff to the foreman (frontier, box-only)

The box is reachable (`ssh hetzner`, verified read-only this session — no KVM
module was touched, so no revert was owed from here). These gates need patched
KVM, the Postgres campaign image, a live task-58 `Machine`, and the mostly-landed
deps (58/59/60/68 landed; 69's manifest, 73's SDK, 74's spans as available). Run
per `docs/BOX-PINNING.md`; **always leave KVM on stock 1396736 + verified** after
every run (spec "Box-safety").

### Gate 4 — isolation anomaly end to end

**Guest (`guest/` — Postgres campaign image):** add a multi-session transaction
driver that runs interleaved read-modify-write transactions over a small key set
with **unique written values** (e.g. `value = session<<32 | monotonic`) and
**final reads at quiesce**, declaring an isolation level. It emits each op as an
`elle`-tagged line (`elle op s=.. t=.. k=.. W|A|R=..`, plus `elle commit/abort
t=..`) to a scraped stream (task 65) — or as `GuestEvent`s via the task-73 SDK.
Formats are pinned in `dissonance/oracle-elle/src/decode.rs`.

**Conductor (`dissonance/conductor`):** add `run_isolation_campaign` mirroring
`run_campaign`, but judging **offline** with `ElleOracle` (add `oracle-elle` as a
dep) over each branch's recorded `RunTrace` instead of the live `CampaignOracle`.
A task-59 fault schedule (tunable/planted per task 60) induces an anomaly the
declared level forbids. Steps: record the `RunTrace` → `ElleOracle::judge_checked`
catches the anomaly from the recorded history (offline, zero extra VM time) →
mint the reproducer (`Bug.env` genesis-complete via `compose`) → replay **25/25**
to the identical terminal hash (task-60 pattern) → a nominal control run judges
clean.

### Gate 5 — liveness probe, uncontaminated (bug (vi))

**Guest — build bug (vi):** the planted convergence failure this task adds to
task-69's benchmark manifest, mirroring 72's bug (v). A workload where a task-59
fault (e.g. a partition / killed replica during a write) leaves the cluster at a
**quiescent terminal that has not converged** (divergent replicas / stuck
election) and does **not** self-heal once the fault stops. Register it in 69's
manifest as bug (vi).

**Conductor:** after a campaign branch reaches the terminal, snapshot it and call
`Explorer::probe` (against the socket `Machine`) with a `LivenessProbeOracle`:
`plan` returns a convergence V-time budget for quiesced terminals; `judge_probe`
flags a `Bug` when the probe did not re-quiesce within budget. The conductor
already has the branch/compose/drop primitives (`materialize.rs`) if a
conductor-level mirror is preferred over the engine method.

**Uncontamination assertion:** capture the archive/trunk digest (frontier +
admitted exemplars + retained snapshot set — e.g. the conductor journal digest +
materializer state) **before and after** the probe phase and assert
byte-identical; assert the probe's throwaway branch snapshot is dropped. bug (vi)
is caught on the discarded branch; a nominal run converges.

### Box-safety (every run)

`pkill -9 -f` the harness bin first → wait `lsmod | grep '^kvm_intel'` users=0 →
`rmmod kvm_intel kvm; modprobe kvm; modprobe kvm_intel` → verify size **1396736**
on a fresh ssh. Pin builds/tests to `taskset -c 2`. Foreground gates, read
results before reporting.

## Test status

**macOS (dev host):**
- `explorer`: `nextest` 102 pass (1 skipped = the ignored public-api test, which
  I refreshed via `UPDATE_PUBLIC_API=1`); `clippy -D warnings`, `fmt --check`,
  `cargo deny` clean. Dependent crates (`matcher`, `runtrace`, `tactics-regime`,
  `conductor`) still build/test green against the new explorer.
- `oracle-elle`: 25 tests + 3 proptests (≥256) pass; full standard suite green.

**Linux (determinism box, `ssh hetzner`, `taskset -c 2` per BOX-PINNING):** the
portable-logic gates for both crates re-run green on Linux (conventions rule 6:
must pass on macOS *and* Linux) — `build`, `nextest` (explorer 102, oracle-elle
25), `clippy -D warnings`, `fmt --check` all pass. `cargo-deny` is not installed
on the box (verified on macOS; it is a platform-independent advisory/license
check). **No patched-KVM window was opened and no KVM operation was run** (these
are pure cargo gates), so the box was left on stock KVM (1396736, verified) with
no revert owed.

## Box gates 4 & 5 — status: NOT run (require new guest workloads to be built)

I did **not** run gates 4 & 5, and did not fabricate a pass. They are blocked on
guest infrastructure that **does not exist in the repo yet**, not on box access
(the box is up, patched KVM is loadable via `scripts/box-window.sh`, and Postgres
images are staged):

- **Gate 4** needs a *concurrent multi-session SQL transaction driver* that emits
  the `elle`-tagged op history (unique writes + final reads), plus a task-59
  fault schedule that **induces an isolation anomaly the declared level forbids
  without crashing the DB**. The staged Postgres images only boot Postgres to the
  readiness banner and idle — none run a transaction workload. Inducing a *clean*
  isolation anomaly (not a crash) via a memory-corruption bit-flip is research-
  grade; task-60's "tunable/planted" discipline would instead plant the trigger
  in a purpose-built KV workload. Either way this is a new guest image + a new
  conductor `run_isolation_campaign` (adding `oracle-elle` as a conductor dep) +
  fault tuning.
- **Gate 5** needs the planted **bug (vi)** convergence-failure workload (a
  post-fault non-self-healing state) built into task-69's manifest, plus the
  conductor liveness-probe campaign.

Both are genuine frontier builds (multi-hour, box-intensive, with fault-tuning
uncertainty). The complete live path — guest workload shape, op-emission format,
conductor wiring, uncontamination assertion, and KVM-safety discipline — is
specced above. The portable half of the probe mechanism is already gated
(`tests/probe.rs`), so the engine plumbing gate 5 exercises is proven; what
remains is the guest workload.
