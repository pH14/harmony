# Task 40 — single-node branching demo: the multiverse from one Postgres snapshot

> **dissonance stream, the join point.** BLOCKED on **task 39 merged** (live snapshot/branch) **and task
> 37 merged** (a running Postgres to branch). This is where the two streams meet: snapshot a live
> Postgres, fork it into many seeded futures, and show each future is individually reproducible. It is a
> **hand-driven demo of the mechanism**, not the automated explorer (task 12).
>
> **Environment:** box-only (patched KVM + live snapshot/branch from task 39).

Read `tasks/00-CONVENTIONS.md`, `tasks/39-live-snapshot-branch.md`, `tasks/37-bare-postgres-deterministic.md`,
and `docs/DISSONANCE.md` (Timeline / Multiverse) first.

## The demo

1. Boot the task-37 guest; let Postgres start and begin the workload; reach a **quiescent snapshot point**
   (per INTEGRATION.md §4) → take a base snapshot S via task 39.
2. **Branch S `K` times**, each with a **different entropy seed**, and/or a **crash-timing** fault (kill
   `postgres` at V-time `T`, then restart so WAL recovery runs). Run each branch forward to a terminal
   point.
3. Show the two properties that make it a *multiverse*, not a copy:
   - **Divergence:** at least one branch produces a **meaningfully different** but internally-consistent
     outcome from the base (proves branching *explores*, e.g. a different interleaving / a recovered vs.
     clean path) — not just N identical reruns.
   - **Reproducibility:** each branch, replayed from its `(snapshot S, seed)` pair `N` times, is
     **bit-identical** every time (serial + `state_hash`). This is the Antithesis property — any future
     it finds, it can replay exactly.

## Bug class (set expectations honestly)

On RAM-backed storage the realistic bug class is **concurrency / scheduling** (timer-driven preemption
interleavings between Postgres backends, recovery-path ordering) — **not durability/crash-consistency**.
The "fsync lied → recover wrong" class needs a durable-vs-volatile split that RAM storage doesn't have;
that rides the deferred **host-side RAM-disk model (D1)**. Say so in the writeup; don't imply a
durability finding the substrate can't produce.

## Acceptance gates

1. **Reproducibility matrix (the headline):** `K` branches × `N` replays each, **100/100 bit-identical**
   per branch from its `(S, seed)`. Quote the matrix (per-branch digest, equal across its `N`).
2. **Divergence shown:** at least one branch's terminal `state_hash`/serial **differs** from the base
   continuation — quote the diverging line/digest so "it explored" is demonstrated, not asserted.
3. **No regression / box hygiene:** standard gates green; every patched run reverts to stock KVM after.

## Non-goals

The automated coverage-guided search (task 12 — this drives the mechanism by hand); durability /
crash-consistency bugs (D1); distributed / multi-node topology (D2 — single-node only, per the agreed
scope); the host-side fault seam wiring (`dissonance/environment`/`control-proto` live — separate). Build
on task 39's snapshot/branch + task 37's Postgres — don't re-architect either.
