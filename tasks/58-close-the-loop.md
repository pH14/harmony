# Task 58 — close the loop: control-transport server + socket-backed `Machine` adapter (seed-driven)

> **FRONTIER · the Wave-4 keystone.** Dissonance's four crates are pure logic tested against an
> in-crate toy; not one line of `consonance/` depends on a `dissonance/` crate, and none of the
> eight control verbs has ever been served (`docs/REVIEW-2026-07.md` §"Ranked gaps" #1). This task
> builds the two missing hops — a `control-proto` server inside vmm-core and a socket-backed
> `explorer::Machine` — and proves them against a real guest. **Seed-driven only**: no reactive
> decisions, no coverage, no perturb enforcement. Those are tasks 59–61; this task makes the
> explorer drive a real VM at all.
>
> **Depends on the task-93 ruling** (compose vs genesis-only). Do not bind the `EnvCodec` seam
> before that ruling lands — the current seams do not line up (explorer: 2-arg infallible
> `compose`, `dissonance/explorer/src/seam.rs:110`; environment: 3-arg fallible, fails closed on
> `Seeded` bases, `dissonance/environment/src/envcodec.rs:140-150`), and genesis-only would delete
> the mismatch outright.

Read first: `tasks/00-CONVENTIONS.md`, `docs/DISSONANCE.md` ("The control transport (verbs)"),
`tasks/25-control-proto.md` (the wire contract this serves), `tasks/12-explorer.md` (the `Machine`
seam), `consonance/vmm-core/src/vmm.rs` (`state_hash` ~1478, `restore_snapshot` ~1207,
`save_vm_state`/`restore_vm_state` ~953/1010, `reseed_entropy` ~1238), and
`consonance/vmm-core/src/snapshot.rs` (`snapshot_base`/`snapshot_derive`/`materialize`).

## Environment

The server + adapter logic is **mock-backend-testable on macOS + Linux** (in-process loopback over
a Unix socket, `MockBackend` guest) and MUST carry the portable gates. The end-to-end proof is
**box-only** (patched KVM, det-cfl-v1 host, the built Postgres image). Pin per
`docs/BOX-PINNING.md`; always revert KVM to stock + verify after any patched run.

## What to build

### 1. `consonance/vmm-core`: a control-transport server

A Unix-socket server speaking `dissonance/control-proto`'s length-delimited codec, owning a `Vmm`
plus a `snapshot-store`, dispatching verbs:

- `hello(caps)` → negotiated `Caps`. Coverage geometry = empty/zero-width (no producer exists);
  `GUEST_HAS_SDK` off.
- `snapshot` → `SnapId` (non-quiescent capture is merged — task 41 — so any V-time point the
  caller stops at is snapshottable; return `ControlError` variants per the wire contract).
- `drop(snap)` → refcount/GC via the store.
- `branch(snap, env)` → restore + apply the env: **reseed entropy from the env's seed**
  (`reseed_entropy`) so branched futures diverge through the already-deterministic RDRAND path
  (proven divergence mechanism — tasks 40/42).
- `replay(snap)` → restore verbatim, no reseed.
- `run(until)` → advance via the existing `step()`/`run_until` machinery to a work-count
  deadline / terminal stop; map terminal states to `StopReason::{Crash, Quiescent, Deadline}`.
  `resolve` is accepted on the wire but any surfaced-decision path returns `ControlError`
  (unsupported until the reactive loop exists).
- `hash(scope)` → `state_hash()`.
- `perturb` → `ControlError::Unsupported` (task 59 lights it up).

This is workload-agnostic substrate surface (task 43 F5 discipline): no Linux knowledge in the
server. **Note on rule 1:** this task is frontier-class and touches `consonance/vmm-core` +
`dissonance/explorer` + (if the 93 ruling requires) `dissonance/environment`; the
"one directory" rule is waived, the surface above is exhaustive.

### 2. `dissonance/explorer`: a socket-backed `Machine`

An implementation of `explorer::Machine` over a `control-proto` client socket — the first
non-toy `Machine`. Bind the `EnvCodec` seam to `dissonance/environment`'s real codec per the
task-93 ruling. `coverage()` returns the negotiated empty geometry; `recorded_env()` returns the
seed-complete env (seed-driven runs record no overrides).

### 3. The demo binary

A small bin (in `dissonance/explorer` or a new `dissonance/conductor` bin crate — implementer's
choice, name it in `IMPLEMENTATION.md`) that runs the outer loop N steps against the server:
snapshot once, branch across seeds, run, hash, replay-check, print a run table.

## Acceptance gates

1. **Portable (macOS + Linux, mock backend):** loopback server + socket `Machine` pass an
   integration test exercising every verb; a proptest (≥256 cases) that `branch(s, seed) → run →
   hash` twice with the same seed is hash-identical, and `replay(s)` after arbitrary interleaved
   verbs reproduces the pre-snapshot hash.
2. **Box gate (the milestone):** against the Postgres workload — one snapshot (mid-workload,
   post-`GUEST_READY`), **N ≥ 8 seeds**: each seed run **twice** → bit-identical `state_hash`
   per seed; **≥ 2 distinct hashes across seeds** (divergent futures); `replay` of the base →
   identical hash to the original capture. Record the run table in `IMPLEMENTATION.md`.
3. Standard suite green on every touched crate; no golden re-blessing (the server is additive —
   existing `live_*` gates byte-identical).

## Non-goals

Reactive `run(resolve)` / `NeedsHost` suspension (needs a guest-plane service — task 61 is the
first); `perturb` enforcement (task 59); any coverage producer or SDK; campaign
strategy/scoring changes (task 60); snapshot performance (D5 — one full-image branch per seed is
acceptable here).
