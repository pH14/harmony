# btrfs (≤5.13) — file lost on log replay after fsync + inode eviction + rename

**Status: spec only — workload not yet built.**

## The bug

`btrfs_log_new_name()` decided "was this inode logged in the current transaction?" by comparing
the **in-memory-only** `logged_trans` field against the current transaction ID. If a directory
inode was logged, then **evicted and reloaded**, `logged_trans` reads 0 and the check wrongly
says no — so a subsequent rename's *new* name is never logged, while the old parent's earlier
fsync already put the old name's dentry **deletions** in the log. Crash + log replay deletes
the old dentry and never creates the new one: the file silently vanishes from both directories.
Mount succeeds; nothing reports an error.

- **Affected**: kernels ≥4.14 through 5.13.x (fix CC'd to stable 4.14+; e.g. backported in
  5.10.57).
- **Fix**: mainline v5.14, commit
  [`ecc64fab7d49`](https://github.com/torvalds/linux/commit/ecc64fab7d49c678e70bd4c35fe64d2ab3e3d212)
  ("btrfs: fix lost inode on log replay after mix of fsync, rename and inode eviction",
  Filipe Manana, 2021-07-27) — switches to the `inode_logged()` helper.
- **Upstream test**: xfstests **generic/640** (added same day, commit `e52ceab66d76`), which
  uses dm-flakey for the power cut; Harmony's snapshot/kill replaces dm-flakey.

## Exact reproducer (from the fixing commit message)

```sh
mkfs.btrfs -f /dev/sdc && mount /dev/sdc /mnt
mkdir /mnt/A /mnt/B
echo -n "hello world" > /mnt/A/foo
sync
touch /mnt/A/bar
xfs_io -c "fsync" /mnt/A            # log directory A
echo 2 > /proc/sys/vm/drop_caches   # evict A's inode → logged_trans reads 0
mv /mnt/A/foo /mnt/B/foo            # old-name deletions logged; new name NOT logged
touch /mnt/baz
xfs_io -c "fsync" /mnt/baz          # syncs the log containing the deletions
# <power fail>  → remount → foo is gone from both A/ and B/
```

Crash window: any Moment after the second fsync returns and before the next transaction commit.
The `drop_caches` eviction step is **essential** — without it `logged_trans` is correct and the
bug cannot fire.

## The triple

- **Workload**: guest kernel ≤5.13 with btrfs (see kernel note below), a scratch btrfs device,
  and a driver running the sequence above — first scripted verbatim (smoke test), then
  generalized: random small trees of mkdir/create/rename/fsync/drop_caches operations, so the
  finder must *discover* the fatal sequence rather than replay it.
- **Fault surface**: kill/branch at a Moment (the power cut), plus schedule/timing perturbation
  to vary where transaction commits land relative to the sequence. This entry stands up the
  filesystem-tier harness pattern: run → branch-kill at candidate Moments → remount → oracle.
- **Oracle**: after replay-mount, every file that a successfully-fsynced operation made durable
  must exist with correct content. Concretely: driver journals (op, fsync-acked?) to the serial
  console before the kill; post-remount checker walks the tree and diffs against the acked set.
  For the verbatim script: `foo` absent from both `A/` and `B/` is the hit.

## Difficulty / knobs

- Verbatim sequence + Moment search over the crash point: expected trivial-to-low (the window
  is the whole gap between second fsync and next commit; commit interval defaults to 30s).
  Generalized op-tree search: harder — record measured branches-to-find for both modes.
- Knobs: btrfs `commit=` mount interval (shrinks the window), op-tree depth/breadth.
- **Nominal control**: identical sequence and kill schedule on a ≥5.14 kernel (or with the fix
  backported) — `foo` must always survive in `B/`.

## Kernel note

This bug is **in-kernel**, so the guest must run a pre-fix kernel — it cannot use the canonical
determinism kernel (task 57, 6.18.x). Options, to be decided at implementation: (a) port the
determinism patches to a 5.13-era kernel (costly), or (b) *revert* `ecc64fab7d49` on the
canonical kernel — the revert is small and reintroduces the bug faithfully. (b) is the default
plan; note it deviates from "pinned pre-fix version" purity and must be stated in results.
