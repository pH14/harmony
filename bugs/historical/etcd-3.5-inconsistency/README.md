# etcd v3.5.0–3.5.2 — silent data inconsistency after untimely crash

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

- **Workload**: etcd v3.5.2 (single member to start; 3-member once net faults exist), driven by
  a sustained high-rate write client with many concurrent applies — the upstream repro is
  "high stress + random SIGKILL". Build FROM the pinned release binary; no source patching.
- **Fault surface**: kill/restart at a Moment — the window between CI persistence (periodic
  batch-tx commit) and the corresponding entry applies. Upstream needed *random* SIGKILLs under
  load and memory pressure to land in the window; Harmony searches Moments directly, which is
  the point of the entry.
- **Oracle** (in strength order):
  1. Single-member ground truth: after restart, independently replay the WAL and compare
     against bbolt contents (upstream had **no tool** for this — the postmortem notes
     single-member corruption was undetectable; our harness sees both sides).
     Practical proxy: client-side journal of acked writes → read-back after restart; any
     acked-but-missing key is a hit.
  2. Multi-member: cross-member `HashKV` / revision comparison (what
     `--experimental-initial-corrupt-check` does; added v3.5.3, on-by-default later).
     Symptoms per #13766: revision lag, differing dbSize, same key independently updatable
     per endpoint.

## Difficulty / knobs

- Expected branches-to-find: unknown until measured — upstream's window is narrow (they needed
  OOM-scale chaos to hit it). Knobs: write rate, bbolt batch interval/limit
  (`--backend-batch-interval`, `--backend-batch-limit`) widen or shrink the CI-ahead-of-data
  window. Record measured branches-to-find here once run.
- **Nominal control**: same workload, clean shutdowns (SIGTERM + wait) — must never diverge.
- **Version control**: the same input on etcd 3.5.3, the release that carries the fix — must
  never trip the oracle.

## Why this entry is first

Single binary, no kernel or version gymnastics, kill-at-Moment is a fault surface Harmony has
today, the oracle is cheap, and it's the highest-recognition corruption bug in modern infra
(it shook Kubernetes). It also has a natural sibling (the defrag bug) once this lands.

## Workload as built

Built by `consonance/harmony-linux/linux/build-faultlab-image.sh` into
`initramfs-faultlab-etcd-3.5.2.cpio.gz` and, for the control arm,
`initramfs-faultlab-etcd-3.5.3.cpio.gz`. etcd and etcdctl come from the
official linux/amd64 release tarballs, pinned by sha256 in `versions.lock`
and cross-checked against each release's own `SHA256SUMS`. No source
patching, as the triple requires.

Boot with `rdinit=/etcd-init` (3.5.2) or `rdinit=/etcd-control-init` (3.5.3).
Bundle `/bundle/etcd-<version>`:

| item | what it runs |
|---|---|
| node 0 `etcd` | single member, data dir on tmpfs, `--backend-batch-interval` (default 1ms) |
| ready | `etcdctl endpoint health` |
| hook 1 | 200 puts through etcdctl, each acked key appended to a journal on tmpfs |
| hook 2 | read every journaled key back; `@always 1 0` on any missing key |

The journal is the client's record of what the server acknowledged, so a key
that is in the journal and absent after a restart is a durability violation by
definition. That is the single-member ground truth the upstream postmortem
notes nobody had a tool for.

Command-line knobs, so a campaign can widen or shrink the window without
rebuilding: `faultlab.puts` (default 200) and `faultlab.batch_interval`
(default 1ms).

The guest kernel is `bzImage-faultlab`, built by `build-faultlab-kernel.sh`.
etcd is Go, and the Go runtime reads the timestamp counter directly, so it
faults instantly on the default kernel; see `x86-faultlab-config-fragment`.
That variant is only deterministic on a host with the patched KVM loaded.
The runs recorded below used the SMP build of that kernel; the fault-library
kernel has since become single-processor (`x86-faultlab-config-fragment`, for
the reason the SQLite entry records), and a schedule found on one build does
not replay on the other.

## Status

Smoke tested on stock KVM: the member starts, all 200 puts are acknowledged,
the read-back finds every journaled key and the oracle stays silent
(`@always 1 1`). Roughly 51 s of guest time per run.

Nominal control on the patched KVM (nested L1 guest on Linux 6.18.35, pvclock
enabled, 1 GiB RAM): two runs stop at the same guest virtual time
(`Quiescent { vtime: Moment(52113967681) }`), produce byte-identical serial
output (16660 bytes, fingerprint `96b9589e9f15ce6c`) and the same state hash,
and the oracle stays silent (`@always 1 1`). About 8 s of wall time per run.
A reproduction claim against this image therefore rests on a deterministic
nominal path.

Two runs on stock KVM produce different serial bytes and different state
hashes, which is expected there and is not evidence of an image defect: without
RDTSC exiting the Go runtime reads the raw host counter, and its scheduling
decisions vary with it.

Searcher campaigns on the patched KVM (250 ms horizon, 8 workers,
`faultlab.puts=20`): a first run of 900 executions found nothing. A second
run of 3824 executions reported an oracle violation at execution 3817 with the
member alive the whole time and no kill in the input:

```
hook 2, hook 1, hook 1, hook 2, hook 1, hook 2, wait, hook 2, wait, hook 2,
pause 0 (5 ticks), pause 0 (5 ticks), hook 1, hook 1, hook 2
```

Two fresh boots replay it to the same moment (+3.680 s) and tick count (159)
with five hooks in flight. That is a defect in the oracle script, found by
the searcher: the read-back listed the server's keys before it read the
journal, so a put acknowledged and journaled between the two reads counted
as lost. The hook now copies the journal first, so a put that lands during
the check is in both lists or in neither, and the same input on the
rebuilt image runs to its deadline with the oracle silent.

On the corrected oracle the searcher reported a lost acknowledged key at
execution 602 of a third campaign, replayed it to the same moment three
times, and the same input ran clean on 3.5.3 and with its kills replaced by
waits. That report is withdrawn. The verify hook kept its two key lists in
fixed files under `/run`, and the searcher starts a fresh hook process for
every hook action, so several verify hooks run at once (nine hooks were in
flight when the report fired). One instance could then compare its journal
copy against another instance's server listing, and a put acknowledged
between the two counted as lost. The 3.5.3 control campaign exposed this: it
reported a lost key at execution 1039 with no kill or restart in the input,
only pauses, deterministic on replay (+1.931 s, 81 ticks), and an image whose
verify hook also reported its counts showed a report with two keys journaled,
two keys listed and two keys missing, which no single listing can produce.
Both hooks now keep their scratch files per instance (`/run/verify.<pid>`).

The schedule a report rests on is bound to the image it was found on:
repacking the same binaries under other paths moved the guest's timing by a
few ticks, and reports stop replaying across that change.

On the corrected hook, a campaign of 4000 executions on 3.5.2 (250 ms
horizon, 8 workers, `faultlab.puts=20`, 101 minutes of wall time) found
nothing: no oracle violation, no abandoned guest, the deepest schedule
reaching five finished hooks with eight in flight. The reproduction this
entry claims is therefore still open. The kill window the searcher reaches
is quantized to the agent's tick (below), while the upstream window is the
gap between a batch commit and the applies it covers, so the next step is a
wider window: a larger `faultlab.batch_interval`, a heavier put load, or
both. The 3.5.3 control campaign on the same input space, 4000 executions
in 111 minutes, also found nothing and abandoned no guest.

With the window widened (`faultlab.batch_interval=50ms`, `faultlab.puts=40`,
otherwise the same campaign), 4000 executions on 3.5.2 in 113 minutes again
found nothing and abandoned no guest, the deepest schedule reaching seven
finished hooks with eight in flight and the member killed along the way.
The 3.5.3 control at the same window ran 3831 executions to its two-hour
wall limit with the same result.

The fault agent applies a kill at its next reconcile tick after the window
opens, so kills land at the window's start plus up to one tick rather than
at any microsecond; the 1 ms batch interval keeps the window wide enough for
that quantization.
