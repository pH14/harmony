# Task 75 — Phase J1: the oracle layer — trace/probe bifurcation + the Elle-shaped checker

> **⛔ DEFERRED / SHELVED (integrator decision, 2026-07-05).** The in-house **Elle-shaped
> reimplementation** (PR #60, ~2,400 lines of Adya/Elle dependency-graph cycle detection over
> 16 review rounds) was **not merged** — owning a from-scratch serializability checker is too
> much correctness liability (every bug is a false verdict about the DB under test). The work is
> archived at git tag **`archive/task-75-elle-inhouse`** (commit e067f207; `git checkout` it to
> retrieve). **When this phase resumes, write a WRAPPER around the real Elle**, not a reimpl. The
> probe/trace *framework* (the `Oracle` trait in explorer, the fingerprint) is sound and stays a
> valid target; only the in-house consistency-checker is shelved. Do not re-spawn this task as-is.



> **FRONTIER · Phase J1 of `docs/EXPLORATION.md`.** Two surfaces, dispatched separately (task-63
> style): **`dissonance/oracle-elle`** is a delegable pure crate, laptop-gated; the **probe-oracle
> mechanism** is explorer engine work with a **frontier box gate**. Portable surface depends on
> **task 64** (the spine: `Oracle`/`Bug`/`RunTrace`) and **task 65** (the stored-corpus format
> gate 3 re-judges). The box gates additionally need **58** (a live `Machine`), **59** (faults),
> **68** (genesis-complete reproducers), **74** (op-history spans), and **69**'s benchmark
> manifest — **this task builds bug (vi)**, the planted convergence failure, into it.

Read first: `tasks/00-CONVENTIONS.md`, `docs/EXPLORATION.md` ("Oracles: trace vs. probe" — this
task codifies it — plus "The organizing split" and "Triage"), `docs/DISSONANCE.md` (the
reproducer/`compose` ruling), `tasks/64-explorer-spine-refactor.md` (the spine being **extended**
here) and `dissonance/explorer/src/spine.rs` once 64 lands (until then `src/engine.rs`'s `Bug`),
`tasks/60-first-campaign-planted-bug.md` (the planted-trigger + 25/25 replay discipline).

## Environment

Portable surfaces (laptop-side): `dissonance/oracle-elle` (new pure crate, macOS + Linux) and the
`ProbeOracle`/fingerprint extension in `dissonance/explorer`. Box surface (frontier): the box gates
below — patched KVM, the built Postgres image, a live task-58 server. Pin per `docs/BOX-PINNING.md`;
revert KVM to stock **1396736** + verify. Dispatch split (foreman): **worker A** = oracle-elle
(delegable); **worker B** = explorer probe mechanism + guest payload + box gates (frontier).

Surface list (frontier waiver of hard rule 1): `dissonance/explorer` (probe mechanism +
fingerprint minting — portable); the campaign/conductor bin (task 58/60's); `guest/` (the
bug-(vi) payload + task-69 manifest wiring per 69's extension conventions, plus the Postgres
image's transaction driver). `dissonance/oracle-elle` is single-crate work under the standard
rules, depending only on `dissonance/explorer` (the task-64 plugin pattern: interfaces live in
the consumer; plugins implement them).

## Context

Oracles are how a deterministic search learns a run is *wrong*, not merely *different*. The spine
(task 64) ships `Oracle` and a `Bug` whose fingerprint task 12 stubbed as stop-reason-only;
nothing yet judges semantics. This task codifies the trace/probe split, ships the first semantic
trace oracle, pins the shared fingerprint schema (task 76), and builds the probe mechanism.

## The ruling, codified (EXPLORATION "Oracles: trace vs. probe")

- **Trace oracles** (replay-plane, pure): `Crash`, `assert_always` violation, Elle over an
  already-recorded operation history. `Oracle::judge(&self, t: &RunTrace) -> Option<Bug>` (spine)
  is the trace-oracle trait; judging never touches a guest.
- **Probe oracles** (live-plane): liveness / `eventually` / "does the cluster converge once faults
  stop?" require running *forward* from a state — a directed probe on a **throwaway terminal
  branch**, discarded so it never contaminates the timeline. This is a specialized
  Tactic+`Machine` interaction, **not** a `judge(&RunTrace)` call; do not force it into `Oracle`.
- **Prefer trace oracles** — arrange workloads to emit what checkers need (e.g. Elle final reads)
  so the oracle stays pure. **The strong offline property**: re-running a NEW oracle over stored
  `RunTrace`s finds REAL bugs with zero VM time; gate 3 makes this a test, not a slogan.

## Surface 1 (delegable): `dissonance/oracle-elle` — the isolation checker

**Honest scope: not a full Elle port** (the follow-on ladder — see Non-goals). An Elle-*shaped*
isolation checker over a recorded operation history — ops decoded from `RunTrace.records`/`events`
(OTel spans per task 74, SDK events per task 73) via an `OpDecode` seam defined locally in this
crate (hard rule 2).

- **Op model** (local): `Op { session, txn, kind: Read | Write | Append, key, value, at: Moment }`
  plus commit/abort events. Recoverability is the *workload's* job (the thin-SDK ruling): unique
  written values (or list-append) so write-read edges recover, and final reads at quiesce for
  version order. Unrecoverable histories are a fail-loud `DecodeError`, never a guess.
- **Dependency graph**: write-read (T2 read T1's write), write-write (version order), and
  read-write anti-dependency (T1 read the version T2 overwrote) edges over transactions.
- **Anomaly ladder v1**, for a declared isolation level, each verdict carrying a constructive
  witness (the participating txns/ops): **G0 dirty write** (ww cycle), **G1a aborted read**
  (committed read of an aborted write), **lost update** (two txns read the same version of a key
  and both commit writes to it).
- **Pure trace oracle**: `impl Oracle for ElleOracle` — deterministic (BTree ordering throughout,
  hard rule 4), offline. `Bug.stop` is the run's terminal `StopReason` (an anomaly run usually
  ends `Quiescent`); the finding lives in the fingerprint's terminal signature.

## The `Bug` artifact and its fingerprint (pinned here, for every oracle)

`Bug { env, stop, fingerprint }` (spine). `env` is **genesis-complete** via `EnvCodec::compose`
down the parent chain (tasks 68/93): `branch(genesis, env)` reproduces the finding bit-for-bit;
`SnapId`s never appear in the artifact. `fingerprint` is a versioned `sha2` digest over a
canonical (BTree-ordered) encoding of the three **stable coordinates**:

1. **Terminal signature** — oracle id + anomaly class + normalized detail (participating key set /
   assertion id / crash-marker class) + the `StopReason` discriminant. No raw addresses.
2. **Fault coordinate** — at mint: the set of fault-classed `Action`s in `env` (plane + class,
   never `Moment`s). Canonical (task 76): the LDFI individually-necessary fault set.
3. **V-time coordinate** — at mint: the quantized V-time of the earliest violating op or terminal.
   Canonical (task 76): the earliest-divergence (inevitability) bracket.

Mint-time fingerprints are tagged **provisional** — they over-split by design (Igor's ordering:
minimize first, then dedup; task 76 recanonicalizes coordinates 2–3). **Forbidden in the digest
at both stages:** `CellKey`s or any learned/codebook feature (codebooks drift — cells are triage
*grouping* only, never identity) and coverage/stack hashes (Klees et al.: they actively
miscount). Supersedes task 12's stop-reason-only digest and task 66's `MatchOracle` fingerprint
minting (66 marks its scheme provisional) — update both minting sites.

## Surface 2 (frontier): the probe mechanism (explorer engine + box)

Extend `spine.rs` — **extend; do not redefine task 64's items**:

```rust
pub struct ProbePlan { pub horizon: StopConditions }   // a fixed V-time convergence budget
pub trait ProbeOracle {
    fn plan(&self, t: &RunTrace) -> Option<ProbePlan>;             // probe this terminal state?
    fn judge_probe(&self, original: &RunTrace, probe: &RunTrace) -> Option<Bug>; // PURE over the probe's trace
}
```

The mechanism is engine plumbing between the Progression and the `Machine` (like materialization —
a function, not a loop change): from the chosen terminal state, `branch` a **throwaway** branch
with a quiesced env (nominal answers, empty fault schedule), `run` to `plan.horizon`, record the
probe's `RunTrace`, call `judge_probe` (pure — the liveness is in *producing* the trace, not
judging it), then `drop_snap` the branch. On a verdict: `Bug.env = compose(original env, probe
delta)` — genesis-complete, replaying the run *and* the failed convergence window. The probe run
is **never** admitted to the `Archive`.

## Acceptance gates

Portable (macOS + Linux — the delegable surface):

1. **Standard suite** green on `dissonance/oracle-elle` and `dissonance/explorer`.
2. **Checker proptests (≥256):** a seeded lost-update history is caught with the right witness;
   a serializable history passes clean; G0/G1a caught on planted histories; verdict determinism —
   the same `RunTrace` judged twice yields byte-equal verdicts.
3. **The offline property, as a test:** a stored `RunTrace` corpus (task 65 format) is re-judged
   with a NEW oracle and surfaces a planted anomaly with **zero VM time** — the mock `Machine`
   records zero verb calls during judging.

Box (frontier — after the delegable surface merges):

4. **Isolation anomaly end to end:** on the real Postgres workload (a multi-session txn driver
   emitting unique writes + final reads), a fault schedule (task 59 vocabulary; tunable/planted
   per task 60's discipline) induces an anomaly the declared level forbids; the checker catches it
   **from the recorded history**, offline; the minted reproducer replays the identical terminal
   hash **25/25** (task 60's pattern); a nominal control run judges clean.
5. **Liveness probe, uncontaminated:** benchmark bug (vi) — the planted convergence failure this
   task builds into task 69's manifest, mirroring 72's bug (v) — is caught by a probe on a discarded
   branch; the timeline is demonstrably uncontaminated: the archive/trunk digest (frontier +
   admitted exemplars + retained snapshot set) is byte-identical before and after the probe
   phase, and the probe's branch is dropped.

## Box-safety (CRITICAL)

Stock KVM = **1396736**; the patched module is larger. ALWAYS leave the box on stock + verified
after every run: `pkill -9 -f` your harness bin (and any `live_*`) FIRST → wait
`lsmod | grep '^kvm_intel'` users=0 → `rmmod kvm_intel kvm; modprobe kvm; modprobe kvm_intel` →
verify size 1396736 on a FRESH ssh connection. SSH drops (exit 255) on pkill/rmmod are normal —
reconnect + verify. Pin builds/tests to `taskset -c 2` (`docs/BOX-PINNING.md`). Run gates in the
foreground and READ results before reporting; no detached pollers + idle.

## Prior art

- **Elle** (Kingsbury & Alvaro, VLDB 2020) [beyond] — oracle design is Jepsen's enduring
  contribution: it turns "two runs differ" into "this run is wrong". The shape, not the codebase.
- **rr** (O'Callahan et al., USENIX ATC 2017) [eng] — the deterministic-replay debugging mental model; the resolution layer shares this substrate.

## Non-goals

- A full Elle port — cycle-typed anomalies through SI/serializability are the anchor and the
  follow-on ladder, not v1. Linearizability checking is likewise a later oracle plugin.
- History checking in the guest/SDK — the thin-SDK ruling; checking lives at the evaluator layer.
- Probe *scheduling* policy (when/which states to probe) — Selector/Tactic work, tasks 70–72.
- Triage — minimize/localize/explain/dedup is task 76; this task only mints the artifacts.
