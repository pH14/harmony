# etcd v3.5.0–3.5.2 — silent data inconsistency after untimely crash

**Status: documented — current-branch campaign pending.** The execution record below is retained
as provenance from `origin/pr-289`; it does not mark this branch reproduced.

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

The workload uses the smallest topology faithful to the upstream report: three local etcd
members sharing one Raft cluster. A client put can be acknowledged after the leader applies it
while a follower is still between its WAL and backend apply; killing that follower leaves the
other two members and the external client ledger as independent witnesses. A one-member
durability oracle can theoretically observe the related interval after the local apply returns
but before the buffered data is committed. It cannot directly observe the reported
acknowledged-before-follower-apply divergence, and retrying a failed request can repair its key.

⚠️ Do not conflate with the **separate, later** consistent-index bug (crash during
**defragmentation**, `unsafeCommit` skipping `OnPreCommitUnsafe`, entries *re-applied*, revision
runs *higher*; affects ≤ v3.5.5, fixed ~v3.5.6 / PR #14730). That one is a candidate for a
second entry — its trigger (kill during defrag) and symptom direction are different.

## The triple

- **Workload**: the upstream etcd workload is three supervised local members, driven by an
  uninstrumented Go helper that the fault agent starts after the cluster is ready. It owns exactly
  four persistent clients through the cluster endpoint set. Each client records every uniquely
  keyed, acknowledged put in a journal outside etcd and keeps applying entries while Harmony
  explores faults. The helper resumes each client's sequence from the journal, so no incarnation
  can overwrite a lost key. The helper is built from one
  pinned `go.etcd.io/etcd/client/v3` dependency shared by both arms; v3.5.2 and v3.5.3 differ
  only in the Antithesis-instrumented server source revision. There are no correctness,
  portability, batch, timing, wait, timeout, rate, or write-count knobs, fixed probe sequences,
  version-specific addresses, or configuration knobs. The server executable for this entry must
  be built from that pinned source by the Antithesis Go instrumentation pipeline; a release
  archive or a stock etcd server executable does not satisfy this entry.
- **Fault surface**: a hard process kill of one member followed by the normal supervisor restart,
  while the clients are applying entries, and a hold that sleeps one member's thread at an
  instrumented site. Together these target the small interval between consistent-index persistence
  and the corresponding follower entry apply. Dissonance names the crash and hold coordinates by
  how rare the site is: the Antithesis runtime receives the rarity over an inherited control
  channel and fires at the first callback after the arm whose own site has been visited at most
  `1 << rarity` times.
- **Oracle**: the helper journals each acknowledged put outside etcd as
  `key<TAB>value<TAB>PutResponse.Header.Revision`. The fault agent reruns a check that performs
  serializable local reads through each member and compares the complete journal with each
  recovered key/value set. A member read below a journaled acknowledgement revision is stale and
  remains inconclusive until it catches up. Once a member's response revision fences a record, an
  acknowledged-but-missing or changed value on that member is the case's only failing assertion.
  The complete history is checked after every fault, so records verified before a crash are checked
  again. A down member, empty journal, or failed local read is silent, so a crash alone cannot be
  mistaken for corruption. The multi-member oracle directly observes the follower-local divergence
  from the upstream report.

## Why the search holds a thread

The window opens when a periodic commit persists the consistent index while the applying thread is
still behind it, which needs two threads running at once. Plain random member kills reach it on
3.5.2 on a multiprocessor host and never on one processor, and never on 3.5.3 either way. Consonance
runs one virtual processor, so the search has to recreate on one processor what parallelism produced
on many: holding a thread at an instrumented site long enough for the commit to run ahead of the
data it claims to cover. The case's fault surface therefore pairs the event kill with an event park.

## Discovery contract

The case has one locked execution profile, bounded by wall time alone. CI runs the same search
campaign on both instrumented arms on demand or on schedule. The vulnerable arm must find and
replay assertion 1 with evidence point 11; the control must reach point 11 and stay clean under
the identical campaign. A search miss is a regression in the test machinery, not a request to
tune the workload.

The following campaign record was produced on `origin/pr-289` under that profile:

| arm | bug found | executions | executions to first hit | conclusive checks | kills fired of armed |
|---|---|---|---|---|---|
| 3.5.2 | yes | 1495 | 1492 | 5 | 239 of 596 |
| 3.5.3 | no | 4900 | - | 6 | 724 of 1889 |

Replaying the vulnerable arm's recorded input three times reproduces assertion 1 with evidence
point 11 and the same whole-VM state hash every time. Replaying that identical input on the
control reaches evidence point 11, reports no violation, and likewise repeats exactly. The
control reaches the oracle at least as often as the vulnerable arm does, so its clean result
says the fix holds rather than saying the oracle stayed silent.

The only expected difference between the arms is the upstream etcd fix. Performance experiments
may add separate profiles later, but they cannot alter the correctness or portability contract
of this case.

## Why this entry is first

Single binary, no kernel or version gymnastics, a generic instrumented event coordinate, and a cheap
oracle make this a useful first target. It also has a natural sibling in the later defragmentation
bug once this lands.

The control replay gate requires a completed conclusive check after the final
process disturbance and recovery, with no outstanding process fault or event
arm. Its check start and completion generations must both match the final agent
generation. A point reached before a crash, a check spanning that crash, or an
inconclusive recovery cannot satisfy the differential gate.
