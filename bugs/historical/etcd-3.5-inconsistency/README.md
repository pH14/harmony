# etcd v3.5.0–3.5.2 — silent data inconsistency after untimely crash

**Status: workload built; nominal control pending a patched-KVM host.**

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

## Why this entry is first

Single binary, no kernel or version gymnastics, kill-at-Moment is a fault surface Harmony has
today, the oracle is cheap, and it's the highest-recognition corruption bug in modern infra
(it shook Kubernetes). It also has a natural sibling (the defrag bug) once this lands.

## Workload as built

Built by `consonance/harmony-linux/linux/build-faultlab-image.sh` into
`initramfs-faultlab-etcd.cpio.gz`. etcd and etcdctl come from the official
3.5.2 linux/amd64 release tarball, pinned by sha256 in `versions.lock` and
cross-checked against the release's own `SHA256SUMS`. No source patching, as
the triple requires.

Boot with `rdinit=/etcd-init`. Bundle `/bundle/etcd`:

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

## Status

Smoke tested on stock KVM: the member starts, all 200 puts are acknowledged,
the read-back finds every journaled key and the oracle stays silent
(`@always 1 1`). Roughly 51 s of guest time per run.

The nominal control is **not** yet recorded. Two runs on stock KVM produce
different serial bytes and different state hashes, which is expected there and
is not evidence of an image defect: without RDTSC exiting the Go runtime reads
the raw host counter, and its scheduling decisions vary with it. The control
belongs on a host with the patched KVM loaded, and the result goes here once
that run happens.
