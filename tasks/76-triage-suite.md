# Task 76 — Phase J2: `dissonance/triage` — minimize / localize / explain / dedup

> **FRONTIER · Phase J2 of `docs/EXPLORATION.md`.** One new pure crate,
> `dissonance/triage`: all four algorithms are pure over the `Machine` seam and mock-tested
> laptop-side; **one frontier box gate** at the end runs them against task 69's planted bugs.
> Depends on **task 75** (`Bug` artifacts + the fingerprint schema this task canonicalizes),
> **task 68** (parent snapshot chains for bisection), **task 69** (the benchmark bugs the box gate
> cites), **task 59** (the fault vocabulary the counterfactuals drop).

Read first: `tasks/00-CONVENTIONS.md`, `docs/EXPLORATION.md` ("Triage: determinism turns
statistics into algorithms" — this task implements it, plus "The Navigation seam" for the
parent-chain economics), `docs/DISSONANCE.md` (the reproducer/`compose` ruling; `SnapId`s are
ephemeral pool handles, never part of an artifact), `tasks/75-oracles.md` (the fingerprint
schema), `dissonance/explorer/src/seam.rs` (`Machine` — it lives here today, not `spine.rs`),
`dissonance/explorer/src/spine.rs` (`Bug`, `RunTrace`, `Oracle`),
`tasks/60-first-campaign-planted-bug.md` (the N/N replay discipline).

## Environment

Pure-logic crate, macOS + Linux, laptop-gated: every algorithm is generic over the `Machine`
trait and tested against a mock machine with planted causal structure. The single box gate is
frontier (patched KVM, the task-69 workload, a live task-58 server); surface for that gate only:
`dissonance/triage` plus the campaign/conductor bin invocation. Pin per `docs/BOX-PINNING.md`;
always revert KVM to stock **1396736** and verify after any patched run.

## Context

Every failing run is `(parent chain, Environment)`, and replay is conclusive: a candidate either
reproduces the terminal signature or it does not — no flaky retries, no confidence intervals.
That turns the triage literature's probabilistic machinery into plain algorithms: **every
ddmin / bisection / LDFI probe is one deterministic, conclusive replay.** Task 75 mints `Bug`s
with provisional fingerprints; this crate is the pipeline that makes them canonical.

## The seams (defined here, hard rule 2)

Triage depends on `dissonance/explorer` (the spine — the sanctioned plugin-crate exception to
rule 2 per task 64; call it out in `IMPLEMENTATION.md`) and stays schema-blind via one local seam
that the schema-aware codec (`dissonance/environment`) implements at integration and a mock
implements in tests:

```rust
/// A decoded view of an Environment's Moment-keyed override schedule that triage edits without
/// knowing Action semantics: entries are opaque; only the Moment and a fault/nominal tag show.
pub trait ScheduleEditor {
    fn entries(&self, env: &Environment) -> Vec<ScheduleEntry>;                  // ordered by Moment
    fn without(&self, env: &Environment, drop: &[EntryId]) -> Environment;       // delete a subset
    fn shifted(&self, env: &Environment, id: EntryId, to: Moment) -> Environment; // V-time shift
}
pub struct ScheduleEntry { pub id: EntryId, pub at: Moment, pub is_fault: bool }
```

Exact parameter lists may be adjusted where the semantics hold (the task-64 convention). The
"still fails" predicate everywhere: replay the candidate env — from genesis, or from the nearest
retained ancestor below the earliest edited `Moment` (task 68 economics; equivalent by
determinism) — re-judge with the bug's oracle, and match the **terminal signature** (fingerprint
coordinate 1; coordinates 2–3 legitimately change as the schedule shrinks).

## Minimize — ddmin over the `Moment`-keyed schedule

Classic ddmin over the override schedule: delete-one, delete-range/complements, plus
**V-time-shift shrinking** (pull surviving overrides toward canonical `Moment`s to shrink the
schedule's temporal extent). Every candidate costs exactly one deterministic replay. Output is
**1-minimal**: deleting any single remaining override makes the failure vanish.

## Localize — trunk bisection with inevitability probing

Binary-search the parent snapshot chain (task 68's retained ancestors): at candidate snapshot
`s_i`, branch **N random continuations** (fresh seeds, faults quiesced) and count failures;
inevitable = all N fail (N configurable, default pinned in the crate). Output: "bug inevitable
between snapshot i and i+1", reported as a **V-time bracket quantized to a configured
granularity** (so retention density does not perturb identity) — never as `SnapId`s. This is
Antithesis's causality analysis, free from our primitives.

## Explain — LDFI counterfactuals

Over the **minimized** schedule: replay dropping each fault entry individually (and greedy
subsets, to expose redundant pairs); a fault whose removal makes the failure vanish is
**individually necessary**. The individually-necessary set IS the explanation, and becomes
fingerprint coordinate 2. **Named follow-on spike (do not build):** LDFI-as-search — reason
backward from lineage to the fault combinations that cut every support path (Molly), *proposing*
fault schedules instead of only explaining them (per `docs/EXPLORATION.md`).

## Dedup — stable coordinates, Igor's ordering

Minimize FIRST, then cluster (Igor). Recompute task 75's fingerprint over the canonical
coordinates — **(LDFI necessary-fault set, earliest-divergence V-time bracket, terminal
signature)** — and dedup on digest equality. **Never** on learned cells (they drift as codebooks
evolve; cells are for triage grouping/reporting only, never identity) and **never** on
coverage/stack hashes (Klees et al.: they actively miscount).

## Acceptance gates

1. **Standard suite** green on `dissonance/triage` (build / nextest / clippy `-D warnings` / fmt /
   deny), macOS + Linux.
2. **Algorithm proptests (≥256) against a mock `Machine` with planted causal structure:**
   (a) the minimized schedule still fails and is 1-minimal; (b) the recovered necessary-fault set
   is exactly the planted one; (c) two surface-variant reproducers of one planted bug dedup to
   one identity, and two different planted bugs stay distinct; (d) the whole pipeline is
   deterministic — same `Bug` in, byte-equal triage report out.
3. **Box gate (frontier):** task 69's planted bugs, each pushed through
   minimize → localize → explain → dedup end-to-end on the box; record in `IMPLEMENTATION.md` the
   schedule-size reduction (override count before/after) and the localization bracket per bug.

## Box-safety (CRITICAL)

Stock KVM = **1396736**; the patched module is larger. ALWAYS leave the box on stock + verified
after every run: `pkill -9 -f` your harness bin (and any `live_*`) FIRST → wait
`lsmod | grep '^kvm_intel'` users=0 → `rmmod kvm_intel kvm; modprobe kvm; modprobe kvm_intel` →
verify size 1396736 on a FRESH ssh connection. SSH drops (exit 255) on pkill/rmmod are normal —
reconnect + verify. Pin builds/tests to `taskset -c 2` (`docs/BOX-PINNING.md`). Run gates in the
foreground and READ results before reporting; no detached pollers + idle.

## Prior art

- **LDFI / Molly** (Alvaro et al., SIGMOD 2015) [beyond] — backward, lineage-driven fault
  reasoning. Antithesis has deterministic replay too, but its search only runs *forward*;
  determinism supplies the lineage, LDFI supplies the backward step. v1 ships the counterfactual
  half.
- **ddmin** (Zeller & Hildebrandt, TSE 2002) [eng] — the minimization algorithm, cheap here
  because every replay is conclusive.
- **Igor** (CCS 2021) [eng] — minimize-then-cluster: dedup after reduction, not before.
- **Klees et al.** (CCS 2018) [eng] — coverage/stack-hash dedup actively over-merges; the case
  for stable coordinates.
- **Antithesis causality analysis** (proprietary — reproduced here from open primitives) —
  Localize reproduces the published behavior (trunk bisection + inevitability probing).

## Non-goals

- LDFI-as-search (the Molly-style backward solver) — the named spike, a later task.
- New oracles, fault types, or Selector/search changes; triage consumes what 59/75 provide.
- Probe-bug-specific refinements (minimizing the probe window itself) — v1 treats a liveness
  `Bug.env` like any other genesis-complete reproducer.
- Reporting/UI beyond the per-bug triage report struct.
