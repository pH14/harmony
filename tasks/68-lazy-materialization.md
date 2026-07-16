# Task 68 — lazy materialization: the engine + the spanning-ancestor retention pool

> **FRONTIER · Phase C's frontier half.** Task 64 shipped the spine: an `Archive` of **virtual**
> exemplars — kilobyte `VirtualExemplar { parent: SnapId, seed, suffix: Environment, at: Moment }`
> records (`dissonance/explorer/src/spine.rs`). Its acceptance gate 4 is explicitly deferred to
> this task and is this task's bar: materializing a deep exemplar replays **only the suffix**
> (measured depth ≪ genesis), and evicting an ancestor still reproduces the state. This task builds
> what makes virtual exemplars real against a live guest: the **materialization engine** — the
> mechanism between `Selector::choose` and `Machine::branch` (an engine mechanism, **not a trait**,
> per the `docs/EXPLORATION.md` ruling) — and the **spanning-ancestor retention pool** deciding
> which sealed snapshots to keep.
>
> Hard-gated on **task 63's ruling** (`consonance/vmm-core/SEAL-RATE-REPORT.md`): **GO** or
> **RESTRICTED** — both arms are specced below; do not start before the ruling exists. Also depends
> on **task 58** (the live socket `Machine` + conductor demo bin) and **task 64** (the spine this
> consumes). Task 69 (the seeded-bug benchmark, GO/NO-GO #2) is the first consumer; `Selector`
> policy is task 70; new signals are 66/67 — none of those land here.

Read first: `tasks/00-CONVENTIONS.md`, `docs/EXPLORATION.md` (§"The Navigation seam: virtual
exemplars and lazy materialization" — the ruling this implements), `docs/DISSONANCE.md` (§"Ruling
(task 93)" — the compose contract), `docs/history/IMPLEMENTATION-task-93.md` (the end-to-end reproducer
gate this task owns), `tasks/63-validate-arbitrary-vtime-seal.md` + the committed
`consonance/vmm-core/SEAL-RATE-REPORT.md` (the ruling + the step-4 depth baseline),
`tasks/64-explorer-spine-refactor.md` (§"Semantics that must hold" 3–4), `tasks/58-close-the-loop.md`
(the `Machine` adapter + demo bin this extends), `consonance/vmm-core/tests/live_branching_demo.rs`
(the hand-driven seal/branch mechanics).

## Environment

Portable-logic surface (macOS + Linux, laptop-gated): the engine sequencing, the lineage table, and
the retention policy + cost model are **pure**, tested against a mock `Machine` with synthetic
replay costs — all proptests live here. The three live gates are **box-only** (patched KVM,
det-cfl-v1 host, the built Postgres image), driven from the dissonance side over the task-58
socket. Pin per `docs/BOX-PINNING.md`; always revert KVM to stock **1396736** and verify after any
patched run.

Surface list (frontier waiver of hard rule 1): `dissonance/explorer` (the engine, lineage table,
retention pool, plus the campaign/conductor bin wiring — extend the task-58 demo bin or its named
successor); `consonance/vmm-core` **read-only** (its server is driven over the socket, unchanged —
a needed server change is a finding to escalate to the foreman, not a patch).

## Context

The Archive stores kilobyte virtual exemplars precisely so sealed snapshots (gigabyte-scale CoW
images) can be scarce. That trade only works if two mechanisms exist: materialization that pays the
*suffix*, never the prefix (task 63 step 4 measured the depth ratio this buys — that number is the
baseline to beat live), and a retention policy that keeps a **spanning set of ancestors** so the
suffix stays short. Neither is a trait: the Progression (outer loop) calls `Selector::choose` and
gets back a runnable `SnapId`; everything in between is this engine. The Modulation (inner loop) is
untouched. Snapshot count is bounded by the **active frontier**, never by archive size.

## What to build

### 1. The materialization engine

Given the chosen `VirtualExemplar { parent, seed, suffix, at }`: `branch(parent, suffix-env)` →
`run` the suffix to `at` → `snapshot` (seal) → hand the new `SnapId` to the run loop, and record
its lineage. **Never a genesis replay in the hot path**: genesis is reached only when no ancestor
on the chain is retained (the graceful worst case), never as the routine path. A **lineage table**
(`SnapId → { parent, suffix, at }`, genesis-rooted chains, `BTreeMap`) outlives eviction: when the
direct parent has been evicted, fold `EnvCodec::compose` over the suffixes from the nearest
**retained** ancestor down to the target and pay one branch + one run — not a re-seal per hop.

### 2. The two arms of the task-63 ruling (implement the one that ruled; keep the seam for both)

- **GO:** any `at` is admissible. A seal failure at materialization time is a task-41/63
  regression — escalate to the foreman with the failing state; do not patch the seal path here.
- **RESTRICTED:** the engine admits only `sealable(Moment)` points — import task 63's `sealable`
  predicate; refuse (loud error) to materialize an exemplar whose `at` fails it (it should never
  have been admitted — the Archive keys on the predicate per task 64). A seal failure at a
  predicate-passing `at` is a precision miss: record it, report zero `Reward` for the choice, drop
  the exemplar, continue; a systematic failure rate is an escalation, not a workaround.

### 3. The spanning-ancestor retention pool

Agamotto-style cost/benefit: retain by **expected re-execution time saved**. Benefit of keeping a
sealed snapshot = the frontier-weighted replay depth it saves descendants (materialization cost of
an exemplar = replay depth from its nearest **retained** ancestor); evict the minimum-benefit
snapshot when over budget, deterministic tie-break by `SnapId`. The budget is a function of the
active frontier, not the archive. Cost unit = **retired instructions** (`Moment` deltas) — never
wall-clock (determinism discipline; a wall-time calibration constant may scale the report, never
the policy's ordering). Degradation is graceful: eviction only lengthens suffixes, up to the
genesis bound; it can never make an exemplar unmaterializable.

### 4. The genesis-complete reproducer (the two-representations rule)

Internal representation: parent-rooted suffix (the hot path). External artifact: `Bug.env` =
`EnvCodec::compose` folded down the ancestor chain to genesis (task 93 contract: tail-complete
deltas, `at`-provenance carried in the blob) — genesis-complete, portable, no `SnapId` in it ever.
This task owns the end-to-end gate `docs/history/IMPLEMENTATION-task-93.md` assigned to the frontier: the
composed artifact must replay on the real machine, not just the toy.

## Invariants (restated; each is gated below)

1. **Parent-rooted materialization.** Materialize = `branch(parent)` + replay only the suffix +
   seal — a genesis replay in the hot path is a defect, not a slow path.
2. **Eviction is always safe.** Determinism re-materializes any evicted state from genesis,
   identically; retention is a pure performance knob, never a correctness concern. The pool's
   policy reasons about cost only — never reachability.
3. **Genesis-complete external artifact.** Every reported `Bug.env` composes to genesis and
   replays bit-identically (same stop + `state_hash`).
4. **Progression blindness.** The engine and pool see opaque `SnapId`s, `Moment`s, and integer
   costs — no fault types, no signal channels, no cell meaning (the `DISSONANCE.md` invariant).
5. **Determinism discipline.** Integer cost model, `BTreeMap` ordering, deterministic tie-breaks,
   no wall-clock or unseeded randomness anywhere in policy or engine.

## Prior art

- **Agamotto** (USENIX Sec 2020) [secret] — checkpoint cost/benefit retention + eviction under
  memory pressure; the published answer to the snapshot-scheduling policy Antithesis keeps
  proprietary. The retention pool is this, re-derived on deterministic replay depth.
- **Nyx** (USENIX Sec 2021) [eng] — its restore-path discipline bounds the cost model of the
  branch → replay-suffix → seal hot path (the quantity the retention policy optimizes).
- **Nyx-Net** (EuroSys 2022) [eng] — incremental snapshots mid message-sequence; validates
  parent-rooting as the practical shape of "snapshot the interesting prefix".
- **Legion** (ASE 2020) [secret] — the cheap-rollout / expensive-expansion split = exactly the
  virtual/materialized exemplar split.
- **ItyFuzz** (ISSTA 2023) [eng] — snapshot-as-corpus-entry; the cleanest small example.

## Acceptance gates

1. **Standard suite** green on `dissonance/explorer` (build / nextest / clippy `-D warnings` / fmt /
   deny), all-features, macOS + Linux.
2. **Portable proptests (≥256 cases) against the mock `Machine` with synthetic replay costs:**
   (a) hot-path property — issued replay depth always equals distance to the nearest retained
   ancestor, and genesis is replayed only when no ancestor is retained; (b) **eviction safety** —
   a campaign with aggressive mid-campaign eviction yields the same bug fingerprints as one with
   none; (c) pool bound — retained count never exceeds the frontier-derived budget, and modeled
   cost degrades monotonically toward the genesis bound under eviction; (d) RESTRICTED arm — under
   a synthetic `sealable` predicate the engine never attempts a seal at an inadmissible `Moment`.
3. **Box gate (a) — measured depth:** on the real Postgres workload, materializing a deep exemplar
   replays only the suffix; report the depth ratio and beat the task-63 step-4 baseline from
   `SEAL-RATE-REPORT.md` (quote both numbers).
4. **Box gate (b) — eviction round-trip:** materialize an exemplar and `hash`; evict its retained
   ancestor; re-materialize the same exemplar (deeper replay) → **bit-identical `state_hash`**.
5. **Box gate (c) — the composed reproducer:** mint a bug below a non-genesis chain ≥ 2 deep;
   `branch(genesis, compose-folded Bug.env)` → identical stop + `state_hash` (the task-93
   end-to-end gate, on the production codec and real `recorded_env`).

## Box-safety (CRITICAL)

Stock KVM = **1396736**; the patched module is larger. ALWAYS leave the box on stock + verified
after every run: `pkill -9 -f live_materialization` (plus the control server and any `live_*`)
FIRST → wait `lsmod | grep '^kvm_intel'` users=0 → `rmmod kvm_intel kvm; modprobe kvm; modprobe
kvm_intel` → verify size 1396736 on a FRESH ssh connection. SSH drops (exit 255) on pkill/rmmod are
normal — reconnect + verify. Pin builds/tests to `taskset -c 2` (`docs/BOX-PINNING.md`). Run gates
in the foreground and READ results before reporting; no detached pollers + idle.

## Non-goals

- `Selector` policy (count-based, bandit) — task 70. This task consumes `choose`, never improves it.
- New `Sensor`s, `CellFn`s, or the matcher DSL — tasks 66/67. The engine is signal-blind.
- Fixing the seal mechanism if it regresses — that is a task-41/63 determinism-core bug; escalate
  to the foreman with the failing state, do not patch it from this surface.
- Snapshot-store performance work (page dedup, restore latency, D5) — the pool decides *which*
  snapshots to keep, not how the store lays them out.
- Changing the reproducer/`compose` contract — ruled (task 93); consume it, don't revisit.
