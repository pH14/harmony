# SPDX-License-Identifier: AGPL-3.0-or-later
"""Guest actors and explicit instrumentation for the SQLite WAL regression."""

from __future__ import annotations

import os
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
PARK_SITE = "sqlite.wal.before_checkpoint"


def always(name, condition):
    from antithesis.assertions import always as emit
    emit(condition, name)


def sometimes(name, condition=True):
    from antithesis.assertions import sometimes as emit
    emit(condition, name)


def setup_complete():
    from antithesis.lifecycle import setup_complete as emit
    emit({"case": "sqlite-wal-reset"})


def instrument(source: Path) -> None:
    from harmony_test import Site

    text = source.read_text()
    needle = "        rc = walCheckpoint(pWal, db, eMode2, xBusy2, pBusyArg, sync_flags,zBuf);"
    if text.count(needle) != 1:
        raise ValueError("SQLite checkpoint call changed; review the park placement")
    header = "extern void notify_coverage(unsigned long long);\n"
    marker = (
        f'        if (getenv("{PARK_ENV}") != 0) '
        f"notify_coverage({Site(PARK_SITE).id}ULL);\n"
    )
    source.write_text(header + text.replace(needle, marker + needle))


def mark(path: Path, value: int = 1) -> None:
    path.write_text(f"{value}\n")


def wait_for(path: Path) -> None:
    deadline = time.monotonic() + 30
    while not path.exists():
        if time.monotonic() > deadline:
            raise RuntimeError(f"timed out waiting for {path}")
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
    before = marked_value(WRITER_DONE)
    os.environ[PARK_ENV] = "1"
    try:
        checkpoint(main, "PASSIVE")
    finally:
        del os.environ[PARK_ENV]
    sometimes("writer-finished-during-pause", before == -1 and marked_value(WRITER_DONE) == 1)
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


if __name__ == "__main__":
    if sys.argv[1] == "instrument":
        instrument(Path(sys.argv[2]))
    else:
        import sqlite3
        {"setup": setup, "checkpoint": checkpointer, "writer": writer}[sys.argv[1]]()
