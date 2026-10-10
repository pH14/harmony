# Historical bug discovery audit

This document asks whether Harmony finds the PostgreSQL, SQLite, and etcd
historical bugs by general search, or because the workloads and the search
were shaped around the known triggers. It records the audit of the three
focused cases, the frozen specification of three general workloads, and the
experiment that compares them.

This is a retrospective audit. Everyone who wrote these workloads and the
general ones knew the bugs, and that knowledge cannot be erased. The general
workloads below are derived from each system's supported operations and
documented guarantees, and every choice that might carry knowledge of a bug is
named and classified. They are not blind.

## Classification

Every choice in a workload or search policy falls into one of three classes:

- **Domain**: ordinary knowledge of the system that a tester without knowledge
  of the bug would use. Example: SQLite runs several processes over one WAL
  database, or PostgreSQL has `CREATE INDEX CONCURRENTLY`.
- **Execution**: required so the workload runs deterministically inside the
  guest. Example: a Unix socket instead of TCP, or `jit = off`.
- **Assistance**: present because of the known trigger. Examples are a
  prearranged triggering sequence, an internal goal at the bug's site, or a data
  layout or pacing calibrated against the race.

An uncertain classification says why it is uncertain.

## Focused case audits

### SQLite 3.51.2 WAL reset (`sqlite-wal-reset`)

The workload is the `antithesis/` harness of the antithesishq/sqlite fork at
commit `3ce53bc469dcef8d8c2d90eb59a7d13184e782e5`. Its `workload.c` checksum,
`0bc70aa4…b433e`, matches the manifest. The fork's own README calls it a
**focused WAL research** harness, and mentions a scratchbook with "3.51.2
triage notes" that the pinned commit does not contain.

| Aspect | Inventory | Class |
|---|---|---|
| Data layout | One WAL database, `page_size=4096`; tables `t(id, writer, seq, blob)` with index `t_ws`, `ctr`, `progress`, `churn`. Init seeds 300 `churn` rows of 400 random bytes, then checkpoints with TRUNCATE so writers start from a drained WAL. | Domain, uncertain: the churn table exists "so writers have existing pages to dirty", which grows the WAL faster than inserts alone. |
| Topology | Two writer processes (nodes) over one file. | Domain |
| Operation vocabulary | 70% write transaction (`BEGIN IMMEDIATE`, insert, counter update, update 3 random churn rows with 64–1663-byte blobs, progress update, `COMMIT`); 20% checkpoint (PASSIVE 70%, RESTART 15%, TRUNCATE 10%, FULL 5%); 10% sweep (count/max/counter checks, then `integrity_check` 1 time in 4 and `quick_check` otherwise). | Domain, with mild assistance: the checkpoint share and mode mix target the checkpoint and WAL-restart machinery where the bug lives. The fork describes itself as "commit + checkpoint stress". |
| Connection settings | `busy_timeout=2000`, `synchronous=NORMAL`, `wal_autocheckpoint=200` (the default is 1000). | Uncertain: an autocheckpoint at one fifth of the default makes WAL resets about five times as frequent. |
| Pacing | After each operation, 1 time in 8, `usleep` of up to 500 µs, documented as widening interleavings in local runs. | Domain |
| Choice source | `rnd_u32` reads `/dev/urandom` through a buffered `FILE*`. The stdio buffer and the guest kernel's CSPRNG both live in guest memory. A branch restores both, so every continuation from one snapshot makes the same operation choices, and only fault timing changes them. | Execution limitation: application choices are not searchable. |
| Internal reachability markers | Thirteen `ANT_REACH` markers that the fork added to `src/wal.c` under `SQLITE_ENABLE_ANTITHESIS`. They include `wal-reset: WAL wrapped back to start (walRestartHdr)`, `checkpoint: live mxFrame differs from snapshot during backfill`, `checkpoint: safe frame limited by an active reader` and `writer: writing header for a fresh/reset WAL (iFrame==0)`. They sit on the reset and backfill path that the 3.51.3 fix changed. | **Assistance**: these are internal goals at the bug's site. Since commit `1ed1e35d` (2026-09-24) the fault archive ranks states by the number of Sometimes and Reachable assertions they have passed, so each marker is a search goal. |
| Assertions | Seven Always, one AlwaysOrUnreachable (recovery), three Sometimes (checkpoint progress), one Reachable (`process started`), plus the supervisor's node-exit check. `SQLITE_DEBUG` internal asserts abort the process. | Domain. These are generic WAL correctness properties. |
| Oracle | `integrity-check-clean: PRAGMA quick_check returns ok`. | Domain |
| Interventions | Wait, Kill, EventKill, EventPark, Pause, Restart. | Domain (search-owned) |
| Earlier attempts | The case was added on 2026-09-22 as the fork's unmodified harness. The search, not the workload, changed while it was the active case; see the search policy audit. | — |

The case was `deferred` because its image was built and searched on arm64. The
Dockerfile also builds on x86-64 and installs the reviewed Bookworm x86-64
allowlist, so this audit builds it for x86-64 and admits it through
`harmony prepare` like every other case. The SQLite results below are x86-64
results.

### PostgreSQL 14.3 CREATE INDEX CONCURRENTLY (`postgres-cic-corruption`)

| Aspect | Inventory | Class |
|---|---|---|
| Data layout | 20000 rows at **fillfactor 10**, about 2200 pages, built into the image. The README records that at fillfactor 70 the table fit in about 300 pages, a scan was shorter than a churn cycle, and a hand-written overlap never tripped the oracle. | **Assistance**: the layout was calibrated until heap scans outlast a churn cycle. |
| Server configuration | `autovacuum = off`, "so pruning is scheduled" by hook 4. Unix socket only, `jit = off`, UTC, `shared_buffers = 32MB`, `max_connections = 16`, no parallel workers. | `autovacuum = off` is **assistance**: it hands every pruning event to the search. The rest is execution. |
| Operation vocabulary | Four hooks the search launches: (1) HOT-update churn through the seeded `churn` procedure, which rewrites a non-indexed column on 20 rows spread across the table in 2 transactions per cycle for 1200 cycles with `synchronous_commit = off`; (2) `DROP INDEX` then `CREATE INDEX CONCURRENTLY` on `k`; (3) `pg_amcheck --heapallindexed`; (4) `VACUUM`. | **Assistance**: the vocabulary is the bug's documented triggering recipe. It is HOT-eligible churn on a column outside the index being built concurrently. |
| Pacing | The churn procedure runs inside the server to avoid client round trips, "which would stretch the cycle past a heap scan". Its row count, slices and rounds are knobs whose defaults were chosen so the churn "cycles through its row set faster than [a scan] and keeps going for longer than a whole build". | **Assistance**: the pacing is calibrated to the race window. |
| Choice source | The search chooses which hook runs and when (`Hook(id, ticks)`). The hooks contain no randomness. | Domain |
| Internal reachability markers | Reachables `churn finished`, `concurrent index build finished`, `vacuum finished` and `amcheck compared the index`. `vacuum finished` is documented as making "a pruned state … a search goal". | Mild assistance: these are workload-level goals that reward reaching the steps of the recipe. |
| Oracle | Always `every heap tuple has an index entry` from `pg_amcheck --heapallindexed`, which is silent when it cannot compare. | Domain: the documented detector. |
| Interventions | Wait, Kill, Pause, Restart, Hook. The image is uninstrumented, so there are no event actions. | Domain |
| Earlier attempts | `probe.json` is a hand-written overlap of churn, build, check and waits that trips the oracle. The README records the fillfactor recalibration. | Assistance (history) |

### etcd 3.5.2 consistent-index inconsistency (`etcd-3.5-inconsistency`)

| Aspect | Inventory | Class |
|---|---|---|
| Data layout | Empty keyspace; three local members on one Raft cluster. | Domain: three members is the standard etcd deployment, and it is the upstream report's topology. |
| Operation vocabulary | Four clients, each putting unique, monotonically numbered keys `museum/<w>/key-<n>` as fast as requests complete. A failed put retries the same key. A restarted writer resumes from its journal. | Domain. Put-only with unique keys is the simplest durability workload, and it does not exercise overwrites, deletes, transactions, leases or compaction. |
| Pacing | None; requests are back to back, with a per-put timeout of 1 s. | Domain, with mild uncertainty about the timeout. |
| Choice source | None; the writer is deterministic. | — |
| Internal reachability markers | None in the server; only the oracle's `etcd oracle compared every member`. | Domain |
| Oracle | Journal of acknowledged puts against revision-fenced serializable reads from every member. | Domain |
| Interventions | Wait, Kill, EventKill, EventPark, Pause, Restart. | Uncertain, see the search audit: the README says the event park exists because "plain random member kills reach [the window] on 3.5.2 on a multiprocessor host but not on one processor". |
| Earlier attempts | The case README says "a search miss … is a regression in the test machinery, not a request to tune the workload". The workload carries no calibrated knobs. The search was extended while this case and SQLite were the active benchmarks. | — |

## Search policy audit

Removing workload tuning does not make the comparison independent of the known
bugs, because the shared search was developed while these cases were the
benchmarks. Neither arm of this experiment changes the following, so every
result carries them:

| Policy | Evidence | Class |
|---|---|---|
| Event-park threshold range 1 to 2^14 − 1 | Commit `b3b274d9`, "the range that fires on the SQLite and etcd cases". The faults README: "On the SQLite WAL reset and etcd cases no park with a threshold of `1 << 14` or more fired, which sets the top of the drawn range." | **Calibrated on these cases.** |
| Event park as an action, re-armed after each hold, held across action boundaries | Commits `4f9287f6`, `d17f38b1`. The etcd README says the park exists to recreate on one CPU the parallelism the etcd bug needs. | A generic mechanism, motivated by etcd. Uncertain whether its defaults were tuned. |
| Park site weighting by shared-memory reads after a hold: weight `1024 * (reads + 10 r) / (landings + 10)`, half of the parks aimed at a weighted site, 8 target widths from 2^6 to 2^12 bytes | Commits `c980a3fe`, `aca94b9d`, `740a6e9b`, `29ca7ef8`, from 2026-09-25 to 09-27. | Generic, but its constants were set while these cases were the benchmarks. Uncertain. |
| Archive progress tiers rank states by the count of passed Sometimes and Reachable assertions | Commit `1ed1e35d`, 2026-09-24. | Generic. It turns any workload's internal markers into goals, which amplifies the SQLite markers. |
| Edge hit-count buckets in the archive holder identity | Commit `df6b2ef3`. | Generic |
| Adaptive action durations from 10 ms to 10.24 s, and coverage quantum from 1 to 32768 | Commits `5293c755`, `da3dbd59`. The quantum keeps busy loops advancing virtual time. | Execution / generic |
| Event-kill rarity 0–63 | Shared fault-policy width. | Generic |
| A lint keeps SQLite, WAL, checkpoint and backfill vocabulary out of the fault search | Commit `96d53559`. | This guards against naming the bug in the search. It cannot detect tuned constants. |

The general workloads cannot remove these effects. The experiment reports them
as residual tuning shared by both arms.

## Searchable application choices

A general workload must let the search vary what the application does, and must
record those choices so a fresh replay reproduces them. Before this change it
could do neither:

- Every branch reseeds the guest's `/dev/harmony` entropy stream from one
  constant, and the guest kernel's CSPRNG lives in restored memory. A driver
  that draws from `fuzz_get_random`, `getrandom` or `/dev/urandom` therefore
  repeats the same choices in every continuation of a snapshot.
- Reseeding that stream per branch would also change the coverage-yield
  scheduling decisions that instrumented focused workloads draw from it. That
  would make the focused arms incomparable with their own history.

The change is confined to the faults package. Each `FaultAction` carries a
`choice`, a `u64` the search draws uniformly whenever it draws a new action. A
retained action keeps its choice, so mutating a retained input keeps or changes
the application's choices independently of the fault. The choice joins the
action's prefix key, so two choices never share a cached snapshot. The standing
service already answers the coverage quantum of the action whose window holds
the current virtual moment. It now also answers SDK opaque service namespace 11
(`APPLICATION_CHOICE_NAMESPACE`), with the 8-byte little-endian choice of that
action, or zero outside every action window. A guest driver asks through the
existing `/dev/harmony` exchange ioctl. Before each operation it folds a changed
choice into its own PRNG state. A branch restores the PRNG and then diverges
under the new action's choice, and replay installs the same choices.

Focused workloads never ask this question, and the change leaves the
`/dev/harmony` entropy and scheduler streams untouched. Their guest executions
are unchanged. The search draws one more number per action, so a given campaign
seed takes a different path than it did before. For that reason both arms of the
comparison run on the same build.

## General workload specifications (frozen)

These specifications are frozen before any general-arm discovery measurement.
A correction found later must be documented here with its reason, and any
measurement it affects restarts.

All three workloads share these rules:

- **Version and binary**: the same pinned affected version as the focused case.
  Where the focused case instruments the system under test, the general case
  instruments it the same way, minus any internal markers.
- **Choices**: each client keeps a 64-bit splitmix64 PRNG, seeded from
  `getrandom` at start. Before every operation it asks for the current action
  choice. When the choice differs from the last one it folded in, it sets
  `state ^= splitmix64(choice)`. Outside Harmony the question fails and the
  PRNG runs alone.
- **Selection**: every operation is drawn uniformly from the system's
  vocabulary, and every parameter uniformly from the stated set or range,
  except where the action's choice biases a decision site (correction 5).
- **Pacing**: after each operation the client pauses with probability ½, for a
  duration drawn uniformly from {1, 2, 4, 8, 16, 32, 64, 128} ms, unless the
  choice biases the think-time site (correction 5). Nothing else paces the
  workload.
- **Ownership**: every row or key the oracle checks has exactly one owning
  client. That client's own journal of intents and outcomes defines the
  acknowledged history. Other clients may read the owned data but never write
  it.
- **Outcomes**: every operation ends as *acknowledged* (the system confirmed
  it), *failed* (definitely not applied: an error before commit, a rollback, a
  busy or serialization error, or a compare that failed), or *indeterminate*
  (it may or may not have applied: a lost connection or a timeout during commit,
  or a crash between commit and journal). An indeterminate write allows both
  outcomes until a later read resolves it. Any other value is a violation.
- **Assertions**: Antithesis fallback SDK JSON lines in
  `$ANTITHESIS_OUTPUT_DIR/sdk.jsonl`, declared at process start. An Always
  assertion is evaluated only when its check was conclusive. A Reachable
  evidence assertion marks every conclusive check.
- **Faults**: the search's existing interventions (Wait, Kill, Pause, Restart,
  and event actions on instrumented images). The workloads add no fault
  injection of their own.

### SQLite general (`sqlite-wal-general`)

- **Image**: the same fork commit and compile commands as the focused case,
  **without** `-DSQLITE_ENABLE_ANTITHESIS`, so the thirteen `src/wal.c` markers
  compile to nothing. `SQLITE_DEBUG` (upstream internal asserts) and
  trace-pc-guard coverage stay. The driver, `sqlite-general`, links the same
  amalgamation.
- **Initial state**: `setup` creates an empty WAL database with
  `kv(k INTEGER PRIMARY KEY, owner INTEGER NOT NULL, ver INTEGER NOT NULL,
  body BLOB NOT NULL)` and an index on `kv(owner, ver)`. It uses SQLite's
  default page size and settings.
- **Topology**: three client processes, each a bundle `node` so the search can
  kill, pause and restart it, over `/data/test.db`.
- **Vocabulary**:
  - `write`: `BEGIN` {DEFERRED, IMMEDIATE, EXCLUSIVE}, then 1–8 statements.
    Each statement inserts a new owned row, updates a live owned row
    (`ver + 1`, new body), or deletes a live owned row; with no live owned row
    it inserts. Body length is 0–4096 bytes. The transaction ends with `COMMIT`
    with probability 7/8, otherwise `ROLLBACK`.
  - `read`: `BEGIN DEFERRED`, read the owned rows (`k, ver, length(body)`) and
    compare them with the model, read `count(*)` of the whole table, pause one
    think time, re-read the owned rows, then `COMMIT`.
  - `checkpoint`: `PRAGMA wal_checkpoint` with mode {PASSIVE, FULL, RESTART,
    TRUNCATE}.
  - `reopen`: close the connection and open a new one with `busy_timeout`
    {0, 100, 1000} ms, `synchronous` {NORMAL, FULL} and `wal_autocheckpoint`
    {0, 100, 1000}. The default settings at process start are
    `busy_timeout = 1000`, `synchronous = FULL` and `wal_autocheckpoint = 1000`.
  - `integrity`: `PRAGMA integrity_check`.
- **Journal**: `/data/journal-<client>`. Before `COMMIT` a pending line records
  the transaction's writes. After `COMMIT` an `A` line marks it acknowledged, or
  an `F` line marks it failed. A pending transaction with neither outcome is
  indeterminate. A torn final line never had its `COMMIT` issued.
- **Properties** (Always):
  - `sqlite integrity_check returns ok`: evaluated whenever `integrity_check`
    returned rows. **This is the case's scored assertion.**
  - `sqlite reports no corruption`: no statement returns `SQLITE_CORRUPT` or
    `SQLITE_NOTADB`.
  - `sqlite preserves acknowledged commits`: owned rows read outside a write
    match the acknowledged history. An indeterminate transaction is accepted
    whole or not at all. This is checked at process start, which is the
    recovery check after a kill, and in every `read`.
  - `sqlite read transaction sees one snapshot`: the two reads inside one
    `read` agree.
- **Evidence** (Reachable): `sqlite integrity_check completed`, and
  `sqlite general compared committed rows`.
- **Limits**: only process faults; no power-loss model, so `synchronous` cannot
  lose an acknowledged commit and is chosen only as a configuration choice.
  There are no schema changes, no `VACUUM`, no `journal_mode` changes, and no
  multi-database or shared-cache connections.

### PostgreSQL general (`postgres-index-general`)

- **Image**: the same 14.3 source build, `amcheck` and configuration overlay as
  the focused case, except that `autovacuum` keeps its default (on), and
  PostgreSQL is built with trace-pc-guard coverage (correction 4). The seed
  table is unchanged by the overlay.
- **Initial state**: `items(id bigint PRIMARY KEY, owner int NOT NULL,
  a int NOT NULL, b int NOT NULL, c text NOT NULL)` at the default fillfactor,
  with 1000 rows: `id` 1–1000, `owner = (id − 1) % 4`, `a = id`,
  `b = id % 100`, `c = md5(id::text)`. It is built at image time with
  `VACUUM ANALYZE` and a clean shutdown. There is no secondary index.
- **Topology**: the server is one node. The bundle `workload` runs
  `pg-general`, which forks four client processes, one per owner, each with its
  own connection.
- **Vocabulary**:
  - `write`: `BEGIN ISOLATION LEVEL` {READ COMMITTED, REPEATABLE READ,
    SERIALIZABLE}, then 1–8 statements. Each statement updates one owned row's
    column {a, b, c} to a fresh value, inserts a new owned row, or deletes an
    owned row. The transaction ends with `COMMIT` with probability 7/8,
    otherwise `ROLLBACK`.
  - `read`: a `REPEATABLE READ` transaction. It compares the owned rows with
    the model, then runs one range predicate on a column {a, b, c} twice, once
    with sequential scans disabled and once with index and bitmap scans
    disabled, and compares the two results.
  - `ddl`: create, drop, or reindex the index on {a}, {b}, {c} or {a, b},
    `CONCURRENTLY` with probability ½.
  - `maintenance`: {`VACUUM`, `VACUUM FREEZE`, `ANALYZE`, `CHECKPOINT`}.
  - `bulk` (correction 5): insert 2^k × 16 owned rows, k in 0–7, in one
    statement whose values follow from a seed, or, when they would not fit the
    model, delete that many of the owner's oldest rows.
  - `check`: `bt_index_check(index, heapallindexed => true)` on every valid,
    ready B-tree index of `items`.
  - `reconnect`: close the connection and open a new one.
- **Properties** (Always):
  - `postgres amcheck finds every heap tuple indexed`: fails when
    `bt_index_check` raises `data_corrupted` (SQLSTATE `XX001`) or
    `index_corrupted` (`XX002`). Any other error, including a lost connection,
    is inconclusive. **This is the case's scored assertion.** (Corrected
    before any general-arm measurement; see Corrections.)
  - `postgres index and sequential scans agree`.
  - `postgres preserves acknowledged commits`.
- **Evidence** (Reachable): `postgres amcheck verified an index`, and
  `postgres general compared committed rows`.
- **Limits**: one table, B-tree indexes only, no foreign keys or partitions, no
  replication, and no prepared transactions. The model checks owned rows only.

### etcd general (`etcd-3.5-general`)

- **Image**: the same Antithesis-instrumented 3.5.2 server, `etcdctl`, members,
  ports and readiness probe as the focused case. The writer and oracle are
  replaced by `etcd-general` and `etcd-general-check`.
- **Topology**: three members. The bundle `workload` runs four clients, each a
  `clientv3` client over all three endpoints with a 5 s request timeout, which
  is the `etcdctl` default.
- **Keys**: client *w* owns `gen/<w>/<n>`. A write targets a new key
  (`n = next`) with probability ½, otherwise an existing owned key. Lease keys
  `lease/<w>/<n>` are written but not checked.
- **Vocabulary**: `put`; `delete`; `cas` (a transaction: if the key's
  `mod_revision` equals its last acknowledged revision, put, else get); `get`
  (linearizable, owned key); `get-serializable` (one random endpoint, no
  property); `range` (linearizable, the client's owned prefix); `lease` (grant a
  TTL of {1, 2, 5, 10} s, put a lease key, and with probability ½ revoke it);
  and `compact` (at the current revision minus 0–100, non-physical).
- **Journal**: `/tmp/etcd/journal/general-<w>` holds one line per operation:
  the intent, then its outcome and the response revision.
- **Properties** (Always):
  - `every etcd member holds an acknowledged history`: the `check` command reads
    `gen/` serializably from every member. For each owned key, the member's
    value and `mod_revision` at the response revision `R` must come from the
    last acknowledged write at or below `R`, or from an indeterminate write
    issued after it. An absent key must be explained the same way by a delete or
    no write. **This is the case's scored assertion.**
  - `etcd members agree on the key-value hash at a common revision`: `HashKV`
    at the lowest current revision among the members. Hashes are compared only
    when every member reports the same compact revision.
  - `linearizable reads observe acknowledged writes`: `get` and `range` on keys
    with no indeterminate write outstanding return the model's value.
- **Evidence** (Reachable): `etcd general check compared the members that answered` (correction 3).
- **Limits**: no watches, no authentication, no membership changes,
  and no snapshot restore. Serializable reads carry no property. Lease keys are
  not checked.

## Experiment

### Hypotheses

- **H1**: each focused arm finds its bug in more of its campaigns than the
  general arm does, under equal budgets.
- **H2**: under the same budget, the general arm finds each known bug at least
  once in ten campaigns. This is a measurement, not an expectation. Zero is an
  acceptable, reportable result.
- **H3**: the SQLite focused arm depends on the in-source WAL markers. Ablation
  A1 finds the bug in fewer campaigns than the focused arm.
- **H4**: the PostgreSQL focused arm depends on its calibrated data layout.
  Ablation A2 finds the bug in fewer campaigns than the focused arm.

### Arms

For each system, every arm uses the same pinned version, User-mode Linux
backend, runner class, CLI build, image recipe path and budget:

| Arm | Workload |
|---|---|
| `F` | the focused case, unchanged |
| `G` | the general workload |
| `A1` (SQLite) | the focused case compiled without `SQLITE_ENABLE_ANTITHESIS`; nothing else changes |
| `A2` (PostgreSQL) | the focused case with the seed table at fillfactor 100 instead of 10; nothing else changes |

The suspected etcd dependency on the event park cannot be ablated without a
change to the search, which is outside this experiment. It is recorded as an
open question.

### Execution profile

GitHub-hosted `ubuntu-24.04` runners (4 vCPU, 16 GB), the User-mode Linux
profile built by `nix run .#uml-images`, run as an ordinary user under the
qualifier's ptrace and KVM denial. The worker count comes from the CLI's own
memory sizing. Each campaign records the repository commit, the profile, image
and tool hashes, the worker count, the seed, the commands and its artifacts.

### Budgets

A pilot runs one campaign per arm with seed 9001 and a 30-minute wall budget.
It checks infrastructure only: image admission, guest boot, oracle evidence and
replay. It measures throughput. Pilot outcomes are not scored. The scored
budget is fixed now, before the pilot: **90 minutes of wall time per campaign,
and an execution cap of 1,000,000** (effectively wall-bounded). It applies to
every arm and system. The pilot may lower it only if a 90-minute job cannot fit
in a GitHub job with its replays. A pilot discovery outcome cannot change it.

### Seeds

Ten independent campaigns per arm, seeds 1001–1010. Each campaign is one
sample. Branches within a campaign are never counted as separate experiments.

### Scoring

Each campaign has exactly one outcome:

| Outcome | Rule |
|---|---|
| **discovery** | A finding violates the case's scored assertion or one of its integrity assertions (`oracle.integrity`, correction 11), carries that assertion's evidence, and passes the package's fresh self-replay. The first such reproducer is then replayed twice more in fresh sessions on the same execution identity, and must violate the same assertion each time. The campaign records which assertion it was. |
| **unconfirmed candidate** | A finding violates the scored assertion or an integrity assertion, but its fresh replay did not reproduce it. |
| **internal discovery** | No discovery, but a confirmed finding where a workload process aborted on the system's own C assertion inside a function the case's fix changed (`oracle.fix_functions`, correction 7). It is reported in its own column and never counted as a discovery. |
| **other violation** | A confirmed violation of an assertion outside the scored and integrity assertions, with no discovery. It is reported and triaged, and is never counted as a discovery. |
| **miss** | The campaign completed its budget, reached at least one conclusive check (the evidence assertion passed), and found nothing. It is censored at the budget. |
| **inconclusive** | The campaign completed but never reached a conclusive check. It is not a clean run. |
| **infrastructure failure** | The CLI timed out or crashed, the report is missing, execution failures exceed watchdog cutoffs, or the UML search restored no snapshot. |

Each arm reports discoveries over attempted campaigns, wall seconds and
executions to the first confirmed finding (with misses censored at the budget),
conclusive and inconclusive check counts, replay and infrastructure failures,
executions per second, and guest seconds. Medians are never reported over
successful runs alone. A Fisher exact test on discovery counts is reported for
H1, H3 and H4. With ten campaigns per arm it detects only large effects.

Searches that start from saved intermediate states are diagnostic only, and
their discoveries are labeled separately from searches from normal
initialization.

## Validation method

Before any general-arm discovery measurement, the following must hold on the
User-mode Linux profile. The pull request that introduces this experiment
carries the measured values.

- **Choices branch and replay.** `harmony debug run --actions` replays an input
  from genesis in fresh sessions. The UML replay `state_hash` covers the
  services state, which includes the handler configuration and so the choices
  themselves. It therefore cannot show whether the guest diverged. The checks
  compare the guest's own evidence at each action boundary instead: virtual
  moment, instrumented edge crossings and edge digest, assertion sets, and the
  console. One input replayed in two sessions must agree everywhere. Two
  inputs that differ only in a later action's choice must agree up to that
  action and diverge after it on each general image. On a focused image they
  must agree everywhere.
- **Oracles.** Each general image runs its oracle self-test while it builds
  (see [general](general/README.md#oracle-self-tests)). A clean run with kills
  or restarts must report nothing. An acknowledged write must be read back. A
  definite failure must never appear, and one that appears is reported. An
  indeterminate write is accepted only as one of its two possible results. A
  member behind an acknowledgement's revision is stale, not wrong. A lost
  acknowledged write and the system's own corruption signal must be reported.
  The etcd unit tests also pin that each client numbers its own operations, so
  journals are never merged by operation id.
- **Scoring.** `scripts/historical-discovery.py` gives each campaign one
  outcome, and `scripts/test_historical_discovery.py` covers every outcome. A
  confirmed crash with no violated assertion, such as a guest kernel panic
  ([#518](https://github.com/pH14/harmony/issues/518)), is a `guest-crash`
  outcome and never a discovery. The reproducer replay runs the first confirmed
  finding that carries the scored assertion (`FINDING=<index>`), not the first
  finding. Deliberate kills are recorded actions. A node exit that no injected
  fault explains violates the supervisor's `workload node ends only by a fault
  the search injected` assertion, which is reported as another violation.
- **Compatible observations.** A check counts only when it reaches its evidence
  assertion, and a campaign that never reaches one is `inconclusive`. The etcd
  checker compares each member at that member's own read revision. The SQLite
  and PostgreSQL checks compare the owned rows inside a single snapshot
  transaction. After a fault, the SQLite clients run the comparison at process
  start, and the PostgreSQL clients run it on reconnect.

## Held-out cases

A general workload improved against the three bugs above could simply learn
them. Three held-out cases check whether an improvement carries over to bugs
nobody tuned for. Each runs a general workload unchanged against an affected
release of the same software, with the same oracle, image recipe and search:

| Case | Release | Bug (fixed in) | Scored assertion |
|---|---|---|---|
| `postgres-14.1-heldout` | PostgreSQL 14.1 | HOT chain broken when pruning sees the horizon move (14.2) | `postgres amcheck finds every heap tuple indexed` |
| `sqlite-3.50.1-heldout` | SQLite 3.50.1 | savepoint rollback after a WAL spill loses later commits on recovery (3.50.2) | `sqlite preserves acknowledged commits` |
| `etcd-3.5.5-heldout` | etcd 3.5.5 | crash during online defragmentation re-applies entries (3.5.6) | `every etcd member holds an acknowledged history` |

They were chosen from a survey of data-integrity bugs in these systems since
2019, as bugs that ordinary operations and process faults can reach and that a
general oracle detects. The choice was made with that survey in view, and so was
correction 6, which adds savepoints, page-cache sizes and defragmentation to
the vocabularies: each is a documented operation family of its system, and
the held-out bugs need them. That knowledge cannot be erased, so the held-out
results are a weaker check than bugs nobody looked at, and they are reported as
such. Neither the workloads nor the search may change in response to a
held-out measurement; a change made after one is a correction, and it
restarts every affected measurement.

## Corrections

Each correction to the frozen specification, with its reason. A correction
made after a general-arm measurement restarts every affected measurement.

1. **PostgreSQL amcheck SQLSTATE** (before any general-arm measurement). The
   specification named only `index_corrupted` (`XX002`) as an amcheck
   violation. PostgreSQL 14 reports a heap tuple without an index entry, which
   is the symptom of this case's bug, with `data_corrupted` (`XX001`) in
   `bt_tuple_present_callback`. Under the original rule the general oracle could
   not have scored the bug at all. Both amcheck corruption classes now fail the
   assertion.

2. **User-mode Linux guest panic** (after the first measurement). Some
   PostgreSQL campaigns ended early on a guest kernel panic
   ([#518](https://github.com/pH14/harmony/issues/518)): a kill during `fork`
   left the marker of a failed `dup_mmap` in the child's VMA tree, and UML's
   `flush_tlb_mm` read it as a VMA. The profile now carries
   `linux/patches/um/0010-um-harmony-failed-fork-teardown.patch`, and all three
   PostgreSQL arms are measured on it. The SQLite and etcd measurements stand:
   the patch changes only executions that reach the marker, and every such
   execution panicked.

3. **etcd check skips members that do not answer** (after the first
   measurement, [#523](https://github.com/pH14/harmony/issues/523)). The
   `check` command made the whole check inconclusive when any member could not
   be read. The member kills that trigger the case's bug often leave a member
   down, so in one general campaign the scored check stayed silent while two
   live members had diverged. Each member is judged at its own read revision,
   so the check now judges every member that answers and is inconclusive only
   when none does. Its evidence is `etcd general check compared the members
   that answered`. The etcd general arm is measured again with it.

4. **PostgreSQL general is instrumented** (after the first measurement). The
   general image was uninstrumented, like the focused case's, so the search
   saw no coverage and could not draw event actions. Its only progress signal
   was two Reachable assertions, and a 90-minute campaign kept about ten
   archive states. The fault runtime now shares its event state with forked
   processes, and the image builds PostgreSQL, `amcheck` and `libpq` with
   trace-pc-guard coverage, so the search sees every backend's coverage and
   can kill or park any PostgreSQL process. The general arm is measured again
   with it. The focused case and its ablation stay uninstrumented, as the
   reference they were measured as.

5. **Decision sites and lineage choices** (after the first measurement). The
   choice only reseeded each client's stream, so the search could not keep or
   steer what the application did: every new action drew a fresh choice, and a
   productive mix of operations was forgotten one action later. Each driver
   now names its decisions as decision sites (operation, transaction size,
   think time, and each operation's parameters), and each byte of the choice
   can bias one site toward one option (see [general](general/README.md)). A
   site the choice leaves alone draws as before. The search keeps a parent's
   choice half the time, changes one byte a quarter of the time, and draws a
   fresh one otherwise, so a regime persists along the lineage that found it.
   The workloads still carry no weights. The vocabularies gain generic scale:
   PostgreSQL adds a bulk operation that inserts 16 to 2,048 owned rows in one
   statement, or deletes that many of the owner's oldest rows when the model
   is full; a biased SQLite transaction holds 1 to 256 statements, with body
   sizes of 0, 256, 2,048 or 4,096 bytes; and a biased etcd compaction keeps
   0, 10, 100 or 1,000 revisions. Every general arm is measured again with it.

6. **Savepoints, page-cache sizes and defragmentation** (after the first
   measurement). The vocabularies lacked three documented operation families.
   A SQLite write transaction can now set a savepoint at a random statement and
   either roll back to it or release it, and a reopened connection draws a page
   cache of the default size, 100 pages or 10 pages. etcd adds `defragment` of
   a random member. Each is reached through the existing decision sites. The
   held-out cases need these families; see [Held-out cases](#held-out-cases).

7. **Internal assertions at the fix site** (after the first measurement,
   [#524](https://github.com/pH14/harmony/issues/524)). One SQLite ablation
   campaign aborted a writer on `SQLITE_DEBUG`'s
   `pInfo->nBackfill==pWal->hdr.mxFrame` in `walCheckpoint`, the function the
   3.51.3 fix changed, before any corruption reached `integrity_check`. Such
   a finding is now its own outcome, internal discovery, when the abort's
   console names a function in the case's `oracle.fix_functions` (the SQLite
   WAL cases list `walCheckpoint`). It uses knowledge of the fix only to score,
   is reported beside discoveries, and is never counted as one.

8. **Torn journal lines** (after the held-out measurement). Each general
   client journals its intents and outcomes and reads the journal back after a
   restart, ignoring a torn final line. A kill can tear a line, and the next
   process appended its first line to the fragment, so the joined line hid the
   history after it and a later comparison reported acknowledged commits that
   were never lost. One SQLite general campaign on 3.51.2, which carries the
   held-out bug's fix, reported `sqlite preserves acknowledged commits`, so the
   held-out SQLite discoveries could have been this artifact. A client now
   cuts its journal back to the last complete line before appending, in the
   SQLite and etcd drivers, and SQLite prints the failed comparison to the
   console. The SQLite and etcd general and held-out arms are measured again.

9. **Cheaper coverage callbacks** (after the held-out measurement). Every
   instrumented edge called into the fault runtime, which took a
   process-shared mutex and updated its counters with atomic
   read-modify-writes. Run natively on one processor with events active, a
   PostgreSQL bulk-load and index script took 40.7 seconds against 13.6 with a
   no-op callback. A callback now takes the mutex only while a kill or park is
   armed and otherwise uses relaxed loads and stores (see
   [the runtime](../../faults/runtime/README.md#forked-processes)), and the
   same script takes 23.0 seconds. Every instrumented case runs more
   executions in the same budget, so every arm is measured again.

10. **Learned fault mix** (after the held-out measurement). The fault search
    drew every suffix action uniformly from the alphabet, so about half of all
    draws killed or restarted a process whatever that had produced, and no
    action that had opened a new archive slot was ever drawn again. Searches
    now use the searcher's energy-splice mixture (see
    [faults](../../faults/README.md)): a fresh alphabet draw, a step from the
    actions of retained inputs, or a splice of another slot holder's route,
    each with a share that decays with the work it spends without opening a
    slot. The search policy changes for every arm, so every arm is measured
    again, against the alphabet-only measurement of correction 9's commit as
    its paired baseline.

11. **Integrity assertions count** (after the held-out measurement). A
    campaign stops at its first confirmed violation, and only the scored
    assertion counted. In three etcd general campaigns the first violation was
    `linearizable reads observe acknowledged writes`, and in one SQLite general
    campaign a statement returned `SQLITE_CORRUPT` before `integrity_check`
    ran: the search had reached the bug, and the campaign scored it as another
    violation. Each case now declares the system's own data-integrity checks
    as `oracle.integrity`, each with its evidence, and a confirmed, reproduced
    violation of any of them is a discovery. The focused SQLite cases count the
    fork's corruption, lost-write and read-your-writes assertions; the focused
    PostgreSQL and etcd cases declare one assertion each. Every arm is scored
    again.
