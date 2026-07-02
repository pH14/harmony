# PostgreSQL 14.0–14.3 — CREATE INDEX CONCURRENTLY builds indexes missing rows

**Status: spec only — workload not yet built.**

## The bug

Commit [`d9d076222f5b`](https://github.com/postgres/postgres/commit/d9d076222f5b) ("VACUUM:
ignore indexing operations with CONCURRENTLY", new in PG 14) let VACUUM/HOT-pruning xmin-horizon
calculations ignore backends running `CREATE INDEX CONCURRENTLY` / `REINDEX CONCURRENTLY`, so a
long concurrent build no longer held back cleanup. Other backends could then HOT-update and
HOT-prune heap tuples the build's snapshot still needed; the build silently omitted them,
yielding an index **missing entries** for live rows. No crash, no fault — a pure concurrency
race under normal operation.

- **Affected**: 14.0–14.3 only (the introducing commit was new in 14).
- **Fix**: 14.4 (out-of-cycle, 2022-06-16), commit
  [`e28bb8851969`](https://github.com/postgres/postgres/commit/e28bb885196916b0a3d898ae4f2be0e38108d81b)
  — a straight revert. Remediation was `REINDEX CONCURRENTLY` of every index ever built
  concurrently on 14.0–14.3.
- **Primary sources**: bug
  [#17485 thread](https://postgr.es/m/17485-396609c6925b982d%40postgresql.org) (Slavov report;
  Borodin/Paquier/Freund diagnosis);
  [14.4 release announcement](https://www.postgresql.org/about/news/postgresql-144-released-2470/).

⚠️ There is an **older, different** CIC corruption bug (2017, Deolasee; relcache
`rd_indexattr` race, 9.x-era fix). Its repro folklore ("3–10 rounds of DROP/CREATE") circulates
mixed into discussions of this one — the 14.x mechanism is solely the xmin-horizon/HOT-prune
interaction stated in `e28bb88`. Keep provenance straight if repro details are imported.

## The triple

- **Workload**: PostgreSQL 14.3 (pin; reuse the existing Postgres image plumbing from tasks
  37/38/48/49 with the version swapped). Two concurrent activities: (a) UPDATE churn on a
  table where the updated column is **not** in the index being built (so updates are HOT), with
  pruning opportunities during the build; (b) a loop of `DROP INDEX` /
  `CREATE INDEX CONCURRENTLY` on another column. Community experience: corruption within a few
  rounds.
- **Fault surface**: none required — timing/interleaving only. This is the entry that measures
  whether dissonance's schedule perturbation (vtime/preemption search) finds races that exist
  under nominal conditions. Faults (e.g. slowing the build via preemption gaps) may widen the
  window; record both configurations.
- **Oracle**: `pg_amcheck --heapallindexed` after each build round (B-tree; official
  recommendation). Cheaper generic oracle: same query via forced index scan vs forced seqscan
  (`enable_seqscan`/`enable_indexscan`), compare row sets — any row visible to seqscan but not
  via the index is a hit.

## Difficulty / knobs

- Expected branches-to-find: expected low-to-moderate given "a few rounds" community folklore,
  but measure and record here. Knobs: table size, UPDATE rate, build duration (index width /
  `maintenance_work_mem`), autovacuum aggressiveness (`autovacuum_naptime`, or manual `VACUUM`
  calls to force pruning).
- **Nominal control**: identical workload on PostgreSQL 14.4 — must never trip the oracle.
  (This entry's control is a *fixed version*, not a no-fault schedule, since no fault is
  injected.)

## Notes

Second in build order: it reuses the deterministic-Postgres plumbing wholesale, and its
"no-fault, pure-timing" character complements the etcd entry's kill-at-Moment character.
