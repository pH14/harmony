# Task 60 — first campaign: find a planted bug, reproduce it N/N

> **FRONTIER · the milestone the project exists for.** With task 58 (loop) and task 59 (host
> faults) in place, run the first real campaign: a workload with a **planted, fault-triggerable
> bug**, a crash oracle, the seed-driven outer loop searching, and the emitted `Recorded`
> environment replaying the find bit-identically, N/N. This validates the whole
> Modulation/Progression mechanism on real software —
> and it is the deliberate first step of the fuzzer-validation discipline: **prove the finder
> against seeded bugs before investing in search cleverness** (see
> `docs/REVIEW-2026-07.md` §"Wave 4" and the deferred SDK/coverage epoch).
>
> Depends on tasks **58 + 59**. The planted-bug guest payload can be built in parallel with 59.

Read first: `tasks/00-CONVENTIONS.md`, `tasks/58-close-the-loop.md`,
`tasks/59-host-plane-enforcement.md`, `dissonance/explorer/src/engine.rs` (the outer loop +
`Bug`), `tasks/42-postgres-workload-uuid-time.md` (the entropy-consuming workload pattern),
`harmony-linux/linux/pg-init.sh` (workload-init conventions).

## The planted bug

Add a small supervised process to the Postgres workload image (a static binary or shell around
`psql`, implementer's choice) with a **known bug reachable only under injected adversity**, e.g.:
a retry loop that corrupts its own bookkeeping when a transaction fails at a specific step, or an
ordering assumption broken by an interrupt-timing perturbation, or a rare-entropy-value branch
(`gen_random_uuid()` prefix match) that dereferences a poisoned pointer. Requirements:

- **Deterministically triggerable**: given the right `(seed, fault schedule)`, it fires every
  time; given nominal conditions, never.
- **Observable as a crash**: process death → a distinctive serial marker → the guest terminates
  (the existing terminal paths), mapped to `StopReason::Crash` by the task-58 server. No SDK
  (assertions ride the serial text for now).
- Documented in the workload's `IMPLEMENTATION.md`: what the bug is, exactly which conditions
  trigger it, and the expected time-to-find under naive seed search (keep it findable within
  ~10²–10³ branches, or make the trigger threshold tunable so the campaign completes on the box).

## The campaign

A campaign bin (extend task 58's demo bin) driving the existing seed-driven strategy:
snapshot the workload mid-run once, then loop: branch(seed′ + a small seeded host-fault
schedule) → run → oracle → score/record. On a `Crash`: emit the `Bug` artifact with its
genesis-complete env (per the task-93 ruling), then **verify: replay the env N times**.

## Acceptance gates

1. **Box gate (the milestone):** the campaign, started with no knowledge of the trigger, finds
   the planted bug and the emitted reproducer **replays the identical crash
   (same `state_hash` at the terminal stop) 25/25**. A nominal-seed control run does not crash.
   Record in `IMPLEMENTATION.md`: branches explored, wall-clock, branches/hour (this number is
   the D5 snapshot-performance trigger — cite it there when D5 is specced).
2. **Portable:** the campaign loop + oracle mapping run against the mock/toy path (planted-bug
   logic unit-tested; oracle mapping proptested).
3. Standard suite green; existing gates byte-identical.

## Non-goals

Coverage-guided search, cell archives, or any strategy work beyond the existing seed strategy
(deferred to the SDK/coverage epoch — do **not** further invest in the AFL-shaped corpus);
net faults (task 61); triage/minimization (ddmin over the Moment schedule is a natural follow-on
— name it in `IMPLEMENTATION.md`, don't build it); real (non-planted) bug hunts.
