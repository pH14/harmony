# PostgreSQL 14.0–14.3 — CREATE INDEX CONCURRENTLY builds indexes missing rows

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

- Expected branches-to-find: expected low-to-moderate given "a few rounds" community folklore.
  Measured: 476 executions (1187 horizons of 500 ms, 8 workers, ten minutes of wall time) on
  the patched KVM; see Status. Knobs: table size, UPDATE rate, build duration (index width /
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
fixed-UUID ext4 file, seeded with a 20000-row table at fillfactor 10 and the
`churn` procedure, and left cleanly shut down, so every run starts from
identical on-disk bytes down to the cluster system identifier. The low
fillfactor leaves room for HOT versions and spreads the rows over about 2200
pages, so a build's heap scans take longer than one churn cycle.

Boot with `rdinit=/pgcic-init` (14.3) or `rdinit=/pgcic-control-init` (14.4).
Bundle `/bundle/pgcic-<version>`:

| item | what it runs |
|---|---|
| node 0 `postgres` | the cluster on the mounted ext4 |
| ready | `pg_isready` |
| hook 1 | HOT-update churn: the seeded `churn` procedure re-updates a small set of rows spread across the table, one short transaction per slice, for a few seconds |
| hook 2 | `DROP INDEX` + `CREATE INDEX CONCURRENTLY` round |
| hook 3 | `pg_amcheck --heapallindexed`; `@always 2 0` when it reports a heap tuple with no index entry, silent when it could not run (server down, connection lost, no valid index) |
| hook 4 | `VACUUM`; `@sometimes 25` so a pruned state is a search goal |

The churn updates a column that is not in the index being built, so the updates
are eligible for HOT and their pruning during a concurrent build is the
mechanism the 14.3 bug turns on. A row drops out of the index only when it is
updated and pruned twice: once between the build's heap-scan snapshot and that
scan reading its page, and again between the validation snapshot and the
validation scan reading its page. Both scans read a page with
`heap_page_prune_opt`, and on 14.3 that prune uses a horizon that ignores the
build's own snapshot. Each scan lasts a few milliseconds, so the churn cycles
through its row set faster than that and keeps going for longer than a whole
build. At fillfactor 70 the table fit in about 300 pages, a scan was shorter
than a churn cycle, and a hand-written overlap of churn, build and check
never tripped the oracle. Autovacuum is off, which makes
hook 4 the only other pruning event and puts it under the searcher's control.

Hooks run serially under the no-fault control driver, which never overlaps them.
The concurrency the bug needs comes from the fault agent spawning hooks without
waiting, so overlapping `RunHook(1)` and `RunHook(2)` windows are what a campaign
must produce. Knobs: `faultlab.churn_rows` (default 20, spread evenly over the
table), `faultlab.churn_slices` (default 2 transactions per cycle) and
`faultlab.churn_rounds` (default 1200 cycles) shape the churn.

The guest kernel is `bzImage-faultlab`, built by `build-faultlab-kernel.sh`:
glibc's dynamic loader reads the timestamp counter before `main`, so every
PostgreSQL binary faults on the default kernel. See `x86-faultlab-config-fragment`.
The runs recorded below used the SMP build of that kernel; the fault-library
kernel has since become single-processor (same fragment, for the reason the
SQLite entry records), and a schedule found on one build does not replay on
the other.

## Status

Smoke tested on stock KVM: both versions start, all four hooks run, and
`pg_amcheck --heapallindexed` passes on both (`@always 2 1`). Roughly 2.5 s of
guest time per run.

Nominal control on the patched KVM (nested L1 guest on Linux 6.18.35, pvclock
enabled, 1 GiB RAM): for each version, two runs stop at the same guest virtual
time (14.3 at 3.5925 s, 14.4 at 3.5928 s), produce byte-identical serial
output (17693 and 17717 bytes) and the same state hash, and the oracle stays
silent (`@always 2 1`). About 2.6 s of wall time per run. A reproduction claim
against either image therefore rests on a deterministic nominal path.

On stock KVM both versions already produce byte-identical serial output across
two runs at identical guest virtual time, but their state hashes differ, which
is expected there: without RDTSC exiting the raw host counter reaches guest
memory.

Reproduced on the patched KVM with hand-written action lists, run through the
searcher's adapter from the sealed setup point with the default knobs:

| horizon | actions | 14.3 | 14.4 |
|---|---|---|---|
| 100 ms | hook 1, hook 2, 12 waits, hook 3, waits | `@always 2 0` (rows lack index entries) at +2.0 s | `@always 2 1`, oracle silent |
| 250 ms | hook 1, hook 2, 6 waits, hook 3, waits | `@always 2 0` at +2.578 s, window 11 | guest hangs (see below) |
| 500 ms | hook 1, hook 2, 2 waits, hook 3, waits | `@always 2 0` at +2.557 s, window 6 | not run |

Each 14.3 result repeats exactly on a fresh boot, at the same guest virtual
time and window, so the reproduction is a deterministic input rather than a
lucky run. The churn (hook 1) starts, the build (hook 2) starts one window
later while the churn is still running, and `pg_amcheck` (hook 3) runs once
the build has finished. Placing hook 3 one window earlier, before the build
has finished, leaves the oracle silent, since `pg_amcheck` has no valid index
to check yet. In the 14.4 arm the same 100 ms list passes.

The 14.4 image hangs under the 250 ms list: the guest kernel enters a
read-write semaphore's optimistic spin, which polls the paravirtual clock page
without exiting to the host, and the VMM advances virtual time only at exits,
so the spin never ends. That is a Consonance limitation with the pvclock
enabled, independent of the bug; the adapter now abandons a guest whose run
exceeds 60 s of wall time and records the endpoint as a crash.

Searcher campaigns on 14.3 with 8 workers found nothing at 100 ms (2500, 1000
and 1100 executions across three runs) or 250 ms (2100 executions). At those
horizons the hand-written path is a dozen or more actions long, and the
search key changes only when a hook finishes, so the searcher had no signal
to follow across the waits. At 500 ms the path is five actions long, with a
key change when the build finishes, and a 4000-execution campaign (8 workers,
56 minutes) still did not reach it. That run reported three guest crashes at
executions 3311 to 3316; all three replay cleanly on a fresh boot, and they
came from an adapter defect: a prefix rebuilt after cache eviction was run
under the whole input's standing-fault list instead of the list its first run
used, which shifts the guest's timing and, on that prefix, shut the guest
down. Rebuilds now branch every action from its parent snapshot under the
original list, and a rebuilt endpoint agrees with its first run to the tick.

The 14.4 control at the same settings (500 ms, 8 workers, 4000 executions,
94 minutes) never tripped the oracle and never crashed; one hung guest was
abandoned by the watchdog. Its archive reached the same milestones as the
14.3 run (14 finished hooks, the same seven `sometimes` sites), so the
searcher covered the two versions alike.

Found by the searcher once hooks in flight became part of the archive key.
The key had counted finished hooks only, so an endpoint where the churn was
still running under the build pooled with one where only the build had run,
and the cheaper build-alone entry was the one kept and extended. With the
count of started-and-unfinished hooks in the key, the 14.3 campaign at the
same settings (500 ms, 8 workers) reported the corruption at execution 476,
ten minutes in:

```
hook 3, wait, restart 0, wait, restart 0, hook 3, pause 0 (1 tick), hook 3,
wait, hook 4, hook 2, pause 0 (5 ticks), wait, hook 3, hook 4, hook 1,
hook 2, hook 1, hook 4, hook 3, pause 0 (100 ticks)
```

The build of window 17 overlaps the churn of windows 16 and 18, and the
check of window 20 reports rows without index entries in window 21 at
+10.150 s. Two replays on fresh 14.3 boots stop at the same moment with the
same tick count (477) and the same violation; the same input on 14.4 runs to
its deadline with the oracle silent. The campaign record is
`faultlab_sometimes_hooks_alive_v2` in the summary's key policy.

The 14.4 control on the same key and settings ran its full budget (4000
executions, 9072 horizons, 106 minutes) without tripping the oracle or
crashing; nine hung guests were abandoned by the watchdog. Its archive went
past the 14.3 run's coverage (14 finished hooks and eight `sometimes` sites
against 13 and seven when the corruption stopped that run).
