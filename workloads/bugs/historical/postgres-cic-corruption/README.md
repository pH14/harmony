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

- **Workload**: PostgreSQL 14.3 (pin). Two concurrent activities: (a) UPDATE churn on a
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

`image/Dockerfile` builds both arms. Its build args select the PostgreSQL
release and nothing else, so the two images differ in the PostgreSQL sources
alone: 14.3 by default, 14.4 with `PG_VERSION` and `PG_SHA256` overridden. Both
come from the official source tarball, verified against the sha256 in
`case.json`. Source rather than a distro package because the oracle needs
contrib `amcheck` plus the `pg_amcheck` client, and because 14.3 has no current
distro build at all. `image/README.md` carries the two build commands.

`initdb` runs at build time, so the cluster system identifier is snapshotted
into the image and every boot starts from identical on-disk bytes. The cluster
sits at `/var/lib/postgresql/data` owned by uid 70, seeded with a 20000-row
table at fillfactor 10, the `churn` procedure, the `amcheck` extension and
`cic_k_idx`, and is shut down cleanly before the layer is committed. The low
fillfactor leaves room for HOT versions and spreads the rows over about 2200
pages, so a build's heap scans take longer than one churn cycle.

`harmony search --package faults` takes the `docker save` tar and reads
`/etc/harmony/bundle` from its rootfs:

| item | what it runs |
|---|---|
| setup | tmpfs on `/tmp`, `/run` and `/dev/shm` at mode 1777; loopback up |
| node 0 `postgres` | the cluster, as uid 70 |
| ready | `pg_isready` on the unix socket |
| hook 1 | HOT-update churn: the seeded `churn` procedure re-updates a small set of rows spread across the table, one short transaction per slice |
| hook 2 | `DROP INDEX` + `CREATE INDEX CONCURRENTLY` round |
| hook 3 | `pg_amcheck --heapallindexed`; `@reachable 24` on every verdict it reached and `@always 2 0` when it reports a heap tuple with no index entry; silent when it could not run (server down, connection lost, no valid index) |
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
than a churn cycle, and a hand-written overlap of churn, build and check never
tripped the oracle. Autovacuum is off, which makes hook 4 the only other
pruning event and puts it under the searcher's control.

Run the hooks by hand in a plain container — start the churn, start the build
while it runs, then check — and the 14.3 image reports rows without index
entries while the 14.4 image stays silent. That says the image and its oracle
are wired correctly; it says nothing about whether a Consonance search reaches
the same overlap, which is what CI measures.

The concurrency the bug needs comes from the platform supervisor spawning hooks without
waiting, so overlapping hook 1 and hook 2 windows are what a campaign must
produce. Knobs: `faultlab.churn_rows` (default 20, spread evenly over the
table), `faultlab.churn_slices` (default 2 transactions per cycle) and
`faultlab.churn_rounds` (default 1200 cycles) shape the churn. The hooks read
them from `/proc/cmdline`, so `--knobs` varies them without a rebuild.

The guest kernel is the `faultlab` profile of `nix run .#guest-images`, which
lands beside the default kernel as `x86_64/bzImage-faultlab`. Both production
profiles serve glibc's and PostgreSQL's userspace `RDTSC`/`RDTSCP` reads from
Harmony's virtual clock; the fault-library profile additionally carries the
task-park fault. The same build writes
`x86_64/initramfs.cpio.gz`, the package-neutral base image `--base-initramfs`
names; canonical OCI preparation supplies the workload rootfs and platform
supervisor bundle to the guest.

`case.json` is the machine-readable form of all of this: the pins, the node and
hook table, the oracle, the run settings and the search budget. `probe.json` is
the hand-written overlap — hook 1, hook 2, wait, wait, hook 3, wait, wait, at
500 ms horizons — that must trip the oracle on 14.3 and stay silent on 14.4. It
is a historical reachability reference; the CI panel discovers a fresh input
on the current build instead of gating on this committed sequence.

## Status

`.github/workflows/historical-bugs.yml` runs the case on GitHub-hosted
`ubuntu-24.04` runners with nested KVM during the nightly schedule or by manual
dispatch. The panel has three stages:

- **search** — a fresh campaign on the current 14.3 build, bounded by the
  execution and wall budgets in `case.json`. The faults package confirms each
  newly found candidate in a fresh Consonance session. A miss, an unverified
  candidate, and an infrastructure failure are reported separately.
- **differential** — when search found an input, replay it once in a fresh
  session on 14.4. The replay must execute the detector and remain free of the
  vulnerable assertion; a violation is an actual differential mismatch.
- **samples** — replay each manifest-declared clean input twice on fresh 14.3
  sessions and compare their state digests. These samples include no-find paths
  and consume the case's aggregate replay-session cap.

`scripts/historical-oracle.sh` holds these rules and reads the two ids
from `case.json`; `scripts/historical-oracle.test.sh` exercises them
against synthetic reports in quality CI.

The earlier reproduction in the fault-library work — a campaign that reported
the corruption at execution 476 with 8 workers at 500 ms horizons — was found
on a differently built guest kernel under the counter-exiting KVM. A schedule
found on one build is diagnostic history; the panel requires a fresh finding
and replay on the current build.

Hosted runners use stock KVM. The `faultlab` kernel emulates userspace counter
reads from Harmony's virtual clock there, so a host timestamp cannot enter
guest memory through `RDTSC` or `RDTSCP`. The historical-bug oracle remains the
criterion for the defect itself: it trips on 14.3, runs and passes on 14.4, and
a campaign finds a tripping input within budget. Each replay also reports
whether state hashes agreed, providing a separate determinism diagnostic.
