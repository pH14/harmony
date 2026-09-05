# PostgreSQL 14.0–14.3 — CREATE INDEX CONCURRENTLY builds indexes missing rows

**Status: workload built; nominal control pending a patched-KVM host.**

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

## Workload as built

Built by `consonance/harmony-linux/linux/build-faultlab-image.sh` into
`initramfs-faultlab-pgcic-14.3.cpio.gz` and, for the control arm,
`initramfs-faultlab-pgcic-14.4.cpio.gz`. Both are built from the projects'
official source tarballs, pinned by sha256 in `versions.lock` and cross-checked
against the `.sha256` files published beside them. Source rather than a distro
package because the oracle needs contrib `amcheck` plus the `pg_amcheck` client,
and because 14.3 has no current distro build at all.

Each version ships a cluster that was `initdb`'d at build time into its own
fixed-UUID ext4 file, seeded with a 20000-row table at fillfactor 70 and left
cleanly shut down, so every run starts from identical on-disk bytes down to the
cluster system identifier.

Boot with `rdinit=/pgcic-init` (14.3) or `rdinit=/pgcic-control-init` (14.4).
Bundle `/bundle/pgcic-<version>`:

| item | what it runs |
|---|---|
| node 0 `postgres` | the cluster on the mounted ext4 |
| ready | `pg_isready` |
| hook 1 | HOT-update churn batch |
| hook 2 | `DROP INDEX` + `CREATE INDEX CONCURRENTLY` round |
| hook 3 | `pg_amcheck --heapallindexed`; `@always 2 0` on failure |
| hook 4 | `VACUUM` |

The churn updates a column that is not in the index being built, so the updates
are eligible for HOT and their pruning during a concurrent build is the
mechanism the 14.3 bug turns on. Autovacuum is off, which makes hook 4 the only
pruning event and puts it under the searcher's control.

Hooks run serially under the no-fault control driver, which never overlaps them.
The concurrency the bug needs comes from the fault agent spawning hooks without
waiting, so overlapping `RunHook(1)` and `RunHook(2)` windows are what a campaign
must produce. Table size knob: `faultlab.churn_rows` (default 2000).

The guest kernel is `bzImage-faultlab`, built by `build-faultlab-kernel.sh`:
glibc's dynamic loader reads the timestamp counter before `main`, so every
PostgreSQL binary faults on the default kernel. See `x86-faultlab-config-fragment`.

## Status

Smoke tested on stock KVM: both versions start, all four hooks run, and
`pg_amcheck --heapallindexed` passes on both (`@always 2 1`). Roughly 2.5 s of
guest time per run.

The nominal control is **not** yet recorded. On stock KVM both versions already
produce byte-identical serial output across two runs at identical guest virtual
time, but their state hashes differ, which is expected there: without RDTSC
exiting the raw host counter reaches guest memory. The control belongs on a host
with the patched KVM loaded, and the result goes here once that run happens.
