# etcd v3.5.0–3.5.2 — silent data inconsistency after untimely crash

**Status: Antithesis Go instrumentation is wired; CI is the acceptance gate.**

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

The workload uses the smallest upstream-shaped topology that can expose an acknowledged
write missing from one member: three local etcd members sharing one Raft cluster. A client put
can be acknowledged after the leader applies it while a follower is still between its WAL and
backend apply; killing that follower leaves the other two members and the external client
ledger as witnesses. A one-member workload cannot expose this acknowledged-before-local-apply
window in v3.5.2 because the put response waits for the local leader apply, and retrying a
failed request repairs the key.

⚠️ Do not conflate with the **separate, later** consistent-index bug (crash during
**defragmentation**, `unsafeCommit` skipping `OnPreCommitUnsafe`, entries *re-applied*, revision
runs *higher*; affects ≤ v3.5.5, fixed ~v3.5.6 / PR #14730). That one is a candidate for a
second entry — its trigger (kill during defrag) and symptom direction are different.

## The triple

- **Workload**: the upstream etcd workload is three supervised local members, driven by four
  persistent clients through the cluster endpoint set. Each client records every uniquely keyed,
  acknowledged put in a journal outside etcd and keeps applying entries while Harmony explores
  faults. Repeated starts of the workload hook reuse the original writers, so a later hook cannot
  overwrite a lost key. The same workload, image contract, and fault policy will run against
  v3.5.2 and v3.5.3; only the pinned source revision changes. There are no correctness,
  portability, batch, timing, wait, timeout, and write-count knobs, fixed probe sequences,
  version-specific addresses, or configuration knobs. The executable for this entry must be
  built from that pinned source by the Antithesis Go instrumentation pipeline; a release archive
  or a stock etcd executable does not satisfy this entry.
- **Fault surface**: a hard process kill of one member followed by the normal supervisor restart,
  while the clients are applying entries. This targets the small interval between consistent-index
  persistence and the corresponding follower entry apply. Dissonance represents the crash
  coordinate as the ordinal of an instrumented deterministic event. The Antithesis runtime
  receives that ordinal over an inherited control channel and kills the member synchronously
  after that many future callbacks.
- **Oracle**: the hook journals each acknowledged put outside etcd, then after all three members
  are ready performs one serializable local prefix read through each member. It compares every
  member's recovered key/value set with the unique acknowledged ledger. An acknowledged-but-
  missing or changed value on any member is the case's only failing assertion. A down member,
  empty journal, or failed local read is silent, so a crash alone cannot be mistaken for
  corruption. The multi-member oracle observes the follower-local divergence that a single
  member cannot expose.

## Discovery contract

The case has one locked execution profile. CI will run the same bounded search campaign on both
instrumented arms on demand or on schedule. The vulnerable arm must find and replay assertion 1
with evidence point 11; the v3.5.3 control must reach point 11 and stay clean under the identical
campaign. A search miss is a regression in the test machinery, not a request to tune the workload.

The only expected difference between the arms is the upstream etcd fix. Performance experiments
may add separate profiles later, but they cannot alter the correctness or portability contract
of this case.

## Why this entry is first

Single binary, no kernel or version gymnastics, a generic instrumented event ordinal, and a cheap
oracle make this a useful first target. It also has a natural sibling in the later defragmentation
bug once this lands.
