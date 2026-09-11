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
  uninstrumented Go helper that owns exactly four persistent clients through the cluster endpoint
  set. Each client records every uniquely keyed, acknowledged put in a journal outside etcd and
  keeps applying entries while Harmony explores faults. Repeated starts of the workload hook reuse
  the original helper, so a later hook cannot overwrite a lost key. The helper is built from one
  pinned `go.etcd.io/etcd/client/v3` dependency shared by both arms; v3.5.2 and v3.5.3 differ
  only in the Antithesis-instrumented server source revision. There are no correctness,
  portability, batch, timing, wait, timeout, rate, or write-count knobs, fixed probe sequences,
  version-specific addresses, or configuration knobs. The server executable for this entry must
  be built from that pinned source by the Antithesis Go instrumentation pipeline; a release
  archive or a stock etcd server executable does not satisfy this entry.
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
  corruption. The multi-member oracle directly observes the follower-local divergence from the
  upstream report.

## Reachability of the bug window

`image/reachability.sh` measures how often the cluster reaches a lost acknowledged key under
plain random member kills, with no Dissonance involved. Each iteration kills one random member,
restarts it, waits for all three to be ready, and runs the oracle. The instrumented server
reaches the Harmony device only inside a guest, so this measurement runs against an image whose
`/usr/lib/libvoidstar.so` has been removed; the instrumented callbacks remain compiled in and
resolve to no-ops.

| configuration | kills | acknowledged writes | conclusive checks | first hit |
|---|---|---|---|---|
| 3.5.2, one core | 152 | 1,730,465 | 152 | none |
| 3.5.2, four cores | 45 | 251,363 | 45 | none |
| 3.5.2, ten cores | 62 | 361,668 | 62 | kill 5, 13 and 44, in three runs of three |
| 3.5.3, one core | 59 | 1,058,445 | 59 | none |
| 3.5.3, four cores | 71 | 708,950 | 71 | none |
| 3.5.3, ten cores | 61 | 1,161,176 | 61 | none |

Every one of the 450 checks was conclusive. The race needs several runnable cores: one core
went through 1.7 million acknowledged writes without losing a key, four cores lost none in
251,363, and ten cores lost one in every run, once after 15,003. A guest with one virtual CPU
therefore cannot reach this window by killing members alone. Reaching it requires holding the
applying thread long enough for the periodic commit to run while the consistent index is ahead
of the data it claims to cover.

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
