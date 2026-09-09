# etcd v3.5.0–3.5.2 — silent data inconsistency after untimely crash

**Status: workload and pinned arms built; the dedicated CI workflow is ready for its first
discovery run.**

## The bug

etcd v3.5.0 (PR [#12855](https://github.com/etcd-io/etcd/pull/12855)) introduced backend hooks
managing the consistent index (CI). Before applying WAL entries, etcd updated the *in-memory*
CI; a commit hook then persisted that value as part of the bbolt batch-transaction commit. The
in-memory CI was shared across concurrent transactions, so a periodic/concurrent batch-tx
commit could persist a CI value **ahead of the data it claimed to cover**. A hard crash in that
window means restart replays from the persisted CI and **skips** WAL entries that were never
applied to bbolt — the member silently diverges from the cluster (or, single-node, from its own
WAL). Raft term/leader/applied-index stay in sync; only the data is wrong.

- **Affected**: v3.5.0, v3.5.1, v3.5.2 (official statement: "not recommended for production").
- **Fix**: v3.5.3 (2022-04-24), PR [#13854](https://github.com/etcd-io/etcd/pull/13854) — CI
  update moved into a `txPostLockHook` after `batchTx.Lock()` so CI and data commit atomically.
- **Primary sources**: issue [#13766](https://github.com/etcd-io/etcd/issues/13766); official
  [postmortem](https://github.com/etcd-io/etcd/blob/main/Documentation/postmortems/v3.5-data-inconsistency.md);
  earlier duplicates #13514, #13654.

⚠️ Do not conflate with the **separate, later** consistent-index bug (crash during
**defragmentation**, `unsafeCommit` skipping `OnPreCommitUnsafe`, entries *re-applied*, revision
runs *higher*; affects ≤ v3.5.5, fixed ~v3.5.6 / PR #14730). That one is a candidate for a
second entry — its trigger (kill during defrag) and symptom direction are different.

## The triple

- **Workload**: one supervised etcd member, driven by four concurrent clients. Each client
  records every acknowledged put in a journal outside etcd and keeps applying entries while
  Harmony explores faults. The same image, bundle, hook sequence, and search budget run against
  v3.5.2 and v3.5.3; only the pinned release archive changes. There are no correctness,
  portability, batch, or timing knobs.
- **Fault surface**: a hard process kill followed by the normal supervisor restart, while the
  clients are applying entries. This targets the small interval between consistent-index
  persistence and the corresponding entry apply. Upstream needed *random* SIGKILLs under load
  and memory pressure; Harmony searches the kill Moment directly.
- **Oracle**: the hook journals each acknowledged put outside etcd, then after a deterministic
  restart reads every journaled key from the recovered member. An acknowledged-but-missing or
  changed value is the case's only failing assertion. A down member, empty journal, or failed
  readback is silent, so a crash alone cannot be mistaken for corruption. This single-member
  oracle is stronger than the original report's observability: it compares the acknowledged
  client record with the recovered database state.

## Discovery contract

The case has one locked execution profile. CI runs the probe on both arms for every relevant
change, then runs the bounded search campaign on a schedule or on demand. The vulnerable arm
must find and replay assertion 1 with evidence point 11; the v3.5.3 control must run the same
recorded actions without either assertion. A search miss is a regression in the test machinery,
not a request to tune the workload.

The only expected difference between the arms is the upstream etcd fix. Performance experiments
may add separate profiles later, but they cannot alter the correctness or portability contract
of this case.

## Why this entry is first

Single binary, no kernel or version gymnastics, kill-at-Moment is a fault surface Harmony has
today, the oracle is cheap, and it's the highest-recognition corruption bug in modern infra
(it shook Kubernetes). It also has a natural sibling (the defrag bug) once this lands.
