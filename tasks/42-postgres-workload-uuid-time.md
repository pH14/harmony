# Task 42 — Postgres workload v2: `gen_random_uuid()` + time functions, still deterministic-twice

> **Integrator request + the sharpest determinism showcase we have.** The task-37/38 Postgres workload
> uses computed integers (a pure function of the loop index). Change it to populate rows with
> **`gen_random_uuid()`** and **time functions** (`now()` / `clock_timestamp()`). The point: these
> *look* non-deterministic (a random UUID, a wall-clock timestamp) but **must come out bit-identical
> across two same-seed runs** — because `gen_random_uuid()` → `pg_strong_random` → the **seeded CRNG**,
> and the clock → **V-time**. It is simultaneously a more striking demo *and* a stress test of the
> RNG/clock determinization: if any path escapes it, the deterministic-twice gate **fails** and we've
> found a real determinization gap. Depends on **task 37 + 38 merged** (both on main). Branch from main.
>
> **Environment:** box-only for the determinism gate (patched KVM); pin per `docs/BOX-PINNING.md` (task 41
> owns core 4 — use **core 2**). Self-serve box gates via git (rsync blocked, git works).

Read `tasks/00-CONVENTIONS.md`, `tasks/37-bare-postgres-deterministic.md`, `harmony-linux/linux/build-postgres-image.sh`
(the `workload.sql` generator), `harmony-linux/linux/pg-init.sh`, and the two gates
`consonance/vmm-core/tests/live_postgres.rs` + `live_postgres_docker.rs` first.

## Change the workload

In the shared `workload.sql` generator (`build-postgres-image.sh`): keep the N-iteration loop, but the
table + INSERTs use a UUID and a timestamp, e.g.
```sql
CREATE TABLE ledger(id uuid PRIMARY KEY DEFAULT gen_random_uuid(), i int, t timestamptz);
-- each iteration:
INSERT INTO ledger(i, t) VALUES ($i, clock_timestamp()) RETURNING id, i, t;
-- and a SELECT that streams id + t + a running aggregate to stdout
```
Use **both** `gen_random_uuid()` (the CRNG path) and a time function — `now()` (transaction time) and/or
`clock_timestamp()` (per-call wall-clock, exercises the live clock). Stream the UUIDs + timestamps + the
running count/sum to `ttyS0` exactly as task 37 does — they're part of the deterministic-twice golden.
`gen_random_uuid` needs `pgcrypto` *or* PG17's built-in — confirm which the baked image provides; if an
extension is needed, `CREATE EXTENSION` at build time into the baked PGDATA (no runtime nondeterminism).

## Determinism (the whole point)

- **`gen_random_uuid()`** draws from `pg_strong_random` (getrandom/OpenSSL) — task 37's IMPLEMENTATION
  already verified that lands on the seeded CRNG. So the UUIDs are a deterministic function of the seed:
  identical across two same-seed runs, **different across different seeds** (assert both).
- **`now()`/`clock_timestamp()`** read the system clock, which is V-time-driven — deterministic.
- If either escapes (a UUID or timestamp differs across same-seed runs), the gate fails — **report it as a
  real determinization finding**, don't paper over it.

## Update the gates (both)

The old `FINAL_ROW = "row|20|407|20|3010"` literal no longer holds. Replace with assertions that prove the
*right* thing for BOTH `live_postgres.rs` (bare) and `live_postgres_docker.rs` (OCI):
1. **Deterministic-twice** — two same-seed patched runs: bit-identical serial (incl. the UUIDs + timestamps)
   + identical `state_hash`. (This is what proves the UUIDs/timestamps are bit-identical.)
2. **Shape** — the streamed rows contain a valid UUID and a valid timestamp (not a constant/error).
3. **Seed-sensitivity** — a run at a *different* seed produces *different* UUIDs (so they're genuinely
   seed-driven, not a frozen constant). Quote one UUID from each seed in the PR.

## Acceptance gates

1. **Box, bare (37 path):** Postgres runs the UUID/time workload, streams it, `GUEST_READY`, clean poweroff;
   **deterministic-twice** (serial + `state_hash`). Quote the equal digests + a sample UUID/timestamp.
2. **Box, OCI (38 path):** same, in the container.
3. **Seed-sensitivity:** different seed ⇒ different UUIDs (quote both).
4. **No regression:** M1/M2/P6 + acceptance-suite goldens byte-identical; standard gates green; revert KVM to stock.

## Non-goals

Changing the determinization mechanism (it should already cover these paths — this task *exercises* it);
schema/index tuning; other workloads. No CPU/MSR contract or hash-schema change.
