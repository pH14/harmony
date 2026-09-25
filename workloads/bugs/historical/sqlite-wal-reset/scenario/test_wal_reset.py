#!/usr/bin/env python3
"""Exercise SQLite's WAL reset race with one Python workload and scenario."""

from __future__ import annotations

import argparse
import os
import sqlite3
import sys
import time
from pathlib import Path


PARK_ENV = "HARMONY_SQLITE_PARK_CHECKPOINT"
DATABASE = Path("/data/wal-race.db")
START = Path("/run/wal-race-start")
WRITER_START = Path("/run/writer-start")
FIRST_CHECKPOINT = Path("/run/first-checkpoint-complete")
WRITER_READY = Path("/run/writer-ready")
WRITER_DONE = Path("/run/writer-done")
CHECKPOINT_RESUMED = Path("/run/checkpoint-resumed")
WRITER_FINAL_DONE = Path("/run/writer-final-done")
LOSS_ASSERTION = "no-lost-committed-writes"

parents = Path(__file__).resolve().parents
if len(parents) > 4:
    sys.path.insert(0, str(parents[4] / "faults" / "python"))
from harmony_scenario import Scenario, Site, always, setup_complete, sometimes

BEFORE_CHECKPOINT = Site("sqlite.wal.before_checkpoint")


def instrument(source: Path) -> None:
    text = source.read_text()
    needle = "        rc = walCheckpoint(pWal, db, eMode2, xBusy2, pBusyArg, sync_flags,zBuf);"
    if text.count(needle) != 1:
        raise ValueError("SQLite checkpoint call changed; review the park placement")
    header = "extern void notify_coverage(unsigned long long);\n"
    marker = (
        f'        if (getenv("{PARK_ENV}") != 0) '
        f"notify_coverage({BEFORE_CHECKPOINT.id}ULL);\n"
    )
    source.write_text(header + text.replace(needle, marker + needle))


def mark(path: Path, value: int = 1) -> None:
    path.write_text(f"{value}\n")


def wait_for(path: Path) -> None:
    while not path.exists():
        time.sleep(0.001)


def marked_value(path: Path) -> int:
    try:
        return int(path.read_text())
    except (FileNotFoundError, ValueError):
        return -1


def connect() -> sqlite3.Connection:
    database = sqlite3.connect(DATABASE, isolation_level=None, timeout=5)
    database.execute("PRAGMA mmap_size=67108864")
    return database


def scalar(database: sqlite3.Connection, statement: str) -> int:
    cursor = database.execute(statement)
    try:
        return cursor.fetchone()[0]
    finally:
        cursor.close()


def checkpoint(database: sqlite3.Connection, mode: str) -> tuple[int, int, int]:
    cursor = database.execute(f"PRAGMA wal_checkpoint({mode})")
    try:
        return tuple(cursor.fetchone())
    finally:
        cursor.close()


def setup() -> None:
    for path in (
        START,
        WRITER_START,
        FIRST_CHECKPOINT,
        WRITER_READY,
        WRITER_DONE,
        CHECKPOINT_RESUMED,
        WRITER_FINAL_DONE,
    ):
        path.unlink(missing_ok=True)
    database = connect()
    database.executescript(
        "PRAGMA journal_mode=WAL;"
        "PRAGMA synchronous=FULL;"
        "CREATE TABLE t1(a INTEGER PRIMARY KEY,b BLOB);"
        "CREATE TABLE canary(a INTEGER PRIMARY KEY);"
        "WITH RECURSIVE s(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM s WHERE i<4096)"
        " INSERT INTO t1 SELECT NULL,randomblob(3900) FROM s;"
        "PRAGMA wal_checkpoint(TRUNCATE);"
    )
    database.close()
    setup_complete()


def writer() -> None:
    wait_for(FIRST_CHECKPOINT)
    database = connect()
    always("writer-starts-empty", scalar(database, "SELECT count(*) FROM canary") == 0)
    mark(WRITER_READY)
    wait_for(WRITER_START)
    try:
        database.execute("INSERT INTO canary VALUES(1)")
        mark(WRITER_DONE)
        sometimes("writer-first-commit-completed")
    except sqlite3.Error:
        always("writer-first-insert-completed", False)
        mark(WRITER_DONE, 0)
    wait_for(CHECKPOINT_RESUMED)
    try:
        database.execute("INSERT INTO canary VALUES(2)")
        mark(WRITER_FINAL_DONE)
    except sqlite3.Error:
        always("writer-final-insert-completed", False)
        mark(WRITER_FINAL_DONE, 0)
    while True:
        time.sleep(0.1)


def checkpointer() -> None:
    wait_for(START)
    for path in (FIRST_CHECKPOINT, WRITER_READY, WRITER_DONE, CHECKPOINT_RESUMED, WRITER_FINAL_DONE):
        path.unlink(missing_ok=True)
    main = connect()
    helper = connect()
    helper.execute("DELETE FROM canary")
    always("initial-truncate-checkpoint-completed", checkpoint(helper, "TRUNCATE")[0] == 0)
    always("large-table-loaded", scalar(main, "SELECT count(*) FROM t1 WHERE b IS NOT NULL") == 4096)
    helper.execute("UPDATE t1 SET b=randomblob(3900) WHERE a<=512")
    for _ in range(50):
        busy, frames, backfilled = checkpoint(helper, "PASSIVE")
        if busy:
            raise RuntimeError("initial checkpoint was busy")
        if backfilled >= frames:
            break
    if backfilled < frames or frames <= 1:
        raise RuntimeError("initial WAL did not reach its 512-frame checkpoint")
    mark(FIRST_CHECKPOINT)
    wait_for(WRITER_READY)
    os.environ[PARK_ENV] = "1"
    try:
        checkpoint(main, "PASSIVE")
    finally:
        del os.environ[PARK_ENV]
    always("writer-finished-during-pause", marked_value(WRITER_DONE) == 1)
    mark(CHECKPOINT_RESUMED)
    wait_for(WRITER_FINAL_DONE)
    committed = marked_value(WRITER_DONE) + marked_value(WRITER_FINAL_DONE)
    always("writer-made-two-commits", committed == 2)
    if committed != 2:
        return
    always("final-truncate-checkpoint-completed", checkpoint(helper, "TRUNCATE")[0] == 0)
    helper.close()
    main.close()
    probe = connect()
    recovered = scalar(probe, "SELECT count(*) FROM canary")
    probe.close()
    always(LOSS_ASSERTION, recovered == committed)
    sometimes("final-canary-read-completed")
    print(f"committed={committed} recovered={recovered}", file=sys.stderr, flush=True)
    while True:
        time.sleep(0.1)


def scenario(image: Path, arguments: argparse.Namespace) -> Scenario:
    return (
        Scenario(
            image=image,
            harmony=arguments.harmony,
            kernel=arguments.kernel,
            base_initramfs=arguments.base_initramfs,
            ram_mib=arguments.ram_mib,
        )
        .park_site(node=0, site=BEFORE_CHECKPOINT, hold_ms=10000, then_wait_ms=100)
        .hook(1, then_wait_ms=1000)
        .hook(2, then_wait_ms=100)
        .wait(12000)
    )


def run_host() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", type=Path, required=True)
    parser.add_argument("--fixed-image", type=Path)
    parser.add_argument("--harmony", type=Path, default=Path("harmony"))
    parser.add_argument("--kernel", type=Path)
    parser.add_argument("--base-initramfs", type=Path)
    parser.add_argument("--ram-mib", type=int, default=1024)
    parser.add_argument("--out", type=Path, required=True)
    arguments = parser.parse_args()

    affected = scenario(arguments.image, arguments).run(arguments.out / "affected")
    (
        affected.reached_site(BEFORE_CHECKPOINT)
        .observed("writer-first-commit-completed")
        .observed("final-canary-read-completed")
        .violated(LOSS_ASSERTION)
        .identical_replays()
    )
    print(f"affected version reproduced loss: {affected.directory}")

    if arguments.fixed_image is not None:
        fixed = scenario(arguments.fixed_image, arguments).run(arguments.out / "fixed")
        (
            fixed.reached_site(BEFORE_CHECKPOINT)
            .observed("writer-first-commit-completed")
            .observed("final-canary-read-completed")
            .clean()
            .identical_replays()
        )
        print(f"fixed version remained clean: {fixed.directory}")


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "instrument":
        instrument(Path(sys.argv[2]))
    elif len(sys.argv) > 1 and sys.argv[1] == "setup":
        setup()
    elif len(sys.argv) > 1 and sys.argv[1] == "checkpoint":
        checkpointer()
    elif len(sys.argv) > 1 and sys.argv[1] == "writer":
        writer()
    else:
        run_host()
