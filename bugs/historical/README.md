# bugs/historical — documented real-world bugs, reproduced

Real FOSS software pinned at a **pre-fix version**, driven so that a documented, historical bug
fires under Harmony's fault/timing search and is caught by an automated oracle. These entries
graduate the finder from "catches what we planted" to "catches what actually happened."

**FOSS only.** Every entry must be fully reproducible from public source at a pinned version.

## Provenance is the gate

An entry is not accepted on folklore. Its README must cite primary sources — the upstream
issue/thread, the fixing commit, the release notes or postmortem — and state exactly:
affected versions, root-cause mechanism, trigger conditions, detection method, and fix commit.
Version pins go in the entry, not in prose asides (same discipline as frontier-task
Environment sections).

## Roster (initial four, in build order)

1. `etcd-3.5-inconsistency/` — etcd v3.5.0–3.5.2 silently diverging after an untimely crash
   (consistent-index vs backend disagreement). Single binary, kill-at-Moment trigger,
   self-checking oracle. First because it's the cleanest capability match today.
2. `postgres-cic-corruption/` — CREATE INDEX CONCURRENTLY building indexes that miss
   concurrently-written rows (fixed in 14.4). Reuses the existing Postgres workload plumbing
   (tasks 37/38/42/48/49); timing-race trigger; index-vs-heap disagreement oracle.
3. `btrfs-fsync-loss/` — one well-documented btrfs log-tree crash-consistency bug with a
   step-by-step reproducer in its fixing commit. Stands up the filesystem-tier harness pattern
   (run → kill at Moments → remount → integrity oracle) that later scales to a batch of
   entries.
4. `zfs-dirty-dnode-corruption/` — OpenZFS 2.1.4–2.2.1 silent corruption (dirty dnode ×
   `SEEK_HOLE` race, issue #15526; initially misattributed to block cloning). The marquee
   "no fault injection needed" entry: a pure timing race under nominal conditions, found by
   perturbing schedules alone.

## Notes

- Filesystem entries may need a per-bug guest kernel (or kernel module version), which cuts
  against the canonical-kernel discipline (task 57). Each such entry must state its kernel
  requirement explicitly and justify why the canonical kernel can't serve; prefer bugs living
  in userspace or in out-of-tree modules (ZFS) when equivalent.
- Multi-node replication entries (Redpanda/Bufstream/streaming-replication anomalies from the
  2022–2024 Jepsen record) are deliberately deferred until the net-fault vertical (task 61).
