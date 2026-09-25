# SQLite WAL reset scenario

This is an authored test of the same WAL reset defect the sibling search case
tries to find organically. The image builds the pinned affected SQLite fork and
inserts a pause callback after `sqlite3WalCheckpoint` reads its WAL header, just
before `walCheckpoint`. Two observation callbacks report WAL reset and backfill
state. Both SQLite versions use the same callbacks and workload; the SQLite
checkpoint and write logic is unchanged.

The workload prepares a WAL database with a large mapped table in `setup`. A
hook starts the checkpointer after the site park is armed. The separate writer
node waits for the first checkpoint to backfill 512 frames, then opens its
connection. The checkpointer reads that header and parks. While it is held,
the writer resets the WAL and commits canary row 1. The checkpointer resumes
and, in the affected version, advances `nBackfill` to 512 even though the live
WAL has one frame. The writer then commits row 2. A truncate checkpoint skips
that second frame, and a fresh connection checks both committed rows. The
affected version violates `no-lost-committed-writes`; the fixed version leaves
`nBackfill` at zero and preserves both rows. The test checks the pause landing,
the reset ordering, the divergent backfill state, completion of the final
fresh-connection read, and identical state hashes across two replays of each
version.

Build both images from the repository root, saving each as a Docker archive:

```sh
docker buildx build --platform linux/arm64 \
  -f workloads/bugs/historical/sqlite-wal-reset/scenario/Dockerfile \
  --load -t harmony-sqlite-wal-race:affected .
docker save harmony-sqlite-wal-race:affected -o /tmp/sqlite-wal-race-affected.tar
docker buildx build --platform linux/arm64 \
  --build-arg SQLITE_COMMIT=ac1a538a559d94801b29a96f96fd8f9e0943f88f \
  -f workloads/bugs/historical/sqlite-wal-reset/scenario/Dockerfile \
  --load -t harmony-sqlite-wal-race:fixed .
docker save harmony-sqlite-wal-race:fixed -o /tmp/sqlite-wal-race-fixed.tar
```

Run the Python test with a controlled guest kernel and base initramfs built
from this checkout. The guest supervisor must understand the site-park selector
in `process-proto`; an older guest may accept the edge-park command but never
arm this selector. Each image runs twice from a fresh setup snapshot.

```sh
python3 workloads/bugs/historical/sqlite-wal-reset/scenario/test_wal_reset.py \
  --image /tmp/sqlite-wal-race-affected.tar \
  --fixed-image /tmp/sqlite-wal-race-fixed.tar \
  --kernel /path/to/controlled-kernel \
  --base-initramfs /path/to/initramfs.cpio.gz \
  --out /tmp/sqlite-wal-race-test
```
