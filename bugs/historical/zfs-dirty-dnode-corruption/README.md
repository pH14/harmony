# OpenZFS 2.1.4–2.2.1 — silent corruption via dirty-dnode / SEEK_HOLE race (#15526)

**Status: spec only — workload not yet built.**

## The bug

`lseek(SEEK_DATA/SEEK_HOLE)` consults `dnode_is_dirty()` to decide whether it must force a txg
sync before trusting on-disk block pointers. A dnode has **two** dirty indicators (membership
on the per-objset dirty list via `dn_dirty_link`, and `dn_dirty_records`), set and cleared
non-atomically during sync. For a newly written file there is a mid-sync window where the
checked indicator reads clean while data is still dirty — so `SEEK_DATA` reports a hole over
unwritten data. A sparse-aware copier (coreutils ≥9 `cp` does SEEK_HOLE detection by default)
skips the range and produces a copy with **zero-filled block-aligned runs** where data existed.
ZFS reports no error: scrub clean, source intact — only the copy is corrupt.

Provenance note (this is why the entry is *not* named "block cloning"): the 2023 blowup was
initially blamed on 2.2.0's block cloning, but the race reproduced on 2.1.x where block cloning
doesn't exist — cloning only widened the window, and 2.2.1's `zfs_bclone_enabled=0` response
did **not** fix it. The buggy `dnode_is_dirty()` pattern is years older; it became practically
hittable when `zfs_dmu_offset_next_sync=1` became the default in **2.1.4**.

- **Affected (practically exploitable)**: 2.1.4–2.1.13, 2.2.0, 2.2.1 (Linux and FreeBSD;
  FreeBSD 14.0 shipped it — errata EN-23:16.openzfs). Pre-2.1.4: race present but vanishingly
  rare; earliest affected version is unresolved upstream.
- **Fix**: commit
  [`30d581121bb1`](https://github.com/openzfs/zfs/commit/30d581121bb122c90959658e7b28b1672d342897)
  ("dnode_is_dirty: check dnode and its data for dirtiness", Rob Norris, PR
  [#15571](https://github.com/openzfs/zfs/pull/15571)) — dirty if *either* indicator is set.
  Released in zfs-2.2.2 and zfs-2.1.14 (Nov 2023).
- **Primary sources**: issue
  [#15526](https://github.com/openzfs/zfs/issues/15526); Tony Hutter's
  [reproducer gist](https://gist.github.com/tonyhutter/d69f305508ae3b7ff6e9263b22031a84).

## The triple

- **Workload**: guest with the OpenZFS **2.2.0** kernel module (out-of-tree — see kernel note),
  a scratch pool, and Hutter's reproducer shape: parallel loops of write-file → immediately
  `cp` it (sparse-aware) → checksum copy vs source. Upstream repro uses ~1000×1MB files across
  several concurrent instances and hits in seconds-to-minutes under load.
- **Fault surface**: **none** — this is the marquee pure-timing entry. The race is between a
  writer/copier thread and txg sync. Dissonance searches schedules (vtime/preemption
  perturbation, SMP interleaving from task 56) to land `SEEK_DATA` inside the mid-sync window.
  No injected fault, no crash: if Harmony finds this, schedule search works on real software.
- **Oracle**: per-copy checksum mismatch against source; corrupted regions are zero-filled
  block-aligned runs (check both, the zero-run signature distinguishes this bug from generic
  corruption). Emit a distinctive serial marker on first mismatch.

## Difficulty / knobs

- Upstream hits it with brute parallelism in seconds-to-minutes; the benchmark question is
  whether schedule search beats brute force (fewer branches at *lower* parallelism). Knobs:
  `zfs_dmu_offset_next_sync` (=1 default, =0 shrinks the window drastically — a difficulty
  dial), `zfs_txg_timeout`, file count/size, number of concurrent loops. Record measured
  branches-to-find at (parallelism=1, =4) here once run.
- **Nominal control**: two options, use both — (a) same workload on zfs-2.2.2 (fixed), must be
  clean; (b) same workload with `cp --sparse=never` (reader never consults SEEK_HOLE), must be
  clean even on 2.2.0.

## Kernel note

OpenZFS is an **out-of-tree module**, so the canonical determinism kernel (task 57) can serve
as-is with zfs-2.2.0 built against it — no kernel pin or revert needed, provided 2.2.0 builds
against 6.18 (it may need minor compat patches; if so, prefer pinning module-side compat fixes
over touching the buggy code paths, and document them here). This is exactly the
"bug lives outside the kernel proper" preference from `../README.md`.
