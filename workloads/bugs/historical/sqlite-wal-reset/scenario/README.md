# SQLite WAL reset scenario

This is an authored test of the same WAL reset defect the sibling search case
tries to find organically. `test_wal_reset.py` is the whole test: it prepares
the database, runs the checkpointer and writer inside the image, emits
assertions, and uses the Python scenario SDK on the host to replay the schedule.
The image builds the pinned SQLite fork as a shared library loaded by Python's
`sqlite3` module. At build time the same Python file inserts one call to the
generic `notify_coverage` hook after `sqlite3WalCheckpoint` reads its WAL
header, just before `walCheckpoint`. SQLite's checkpoint and write logic is
unchanged, and there is no C test program. `0x514C0001` is simply the stable
label chosen for that hook, not a SQLite address or discovered offset.

The Python workload prepares a WAL database with a large mapped table. One
hook starts the checkpointer after the site park is armed; a second releases
the writer while the checkpointer is held. The writer resets the WAL and
commits canary row 1. After the checkpointer resumes, the writer commits row 2.
A truncate checkpoint and a fresh connection check both rows. The affected
version violates `no-lost-committed-writes`; the fixed version preserves both.
The host test requires the pause landing, the first write, completion of the
final fresh-connection read, and identical state hashes across two replays of
each version.

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
