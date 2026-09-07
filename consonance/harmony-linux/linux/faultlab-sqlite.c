// SPDX-License-Identifier: AGPL-3.0-or-later
// The SQLite WAL-reset workload (bugs/historical/sqlite-wal-reset): one
// database in WAL mode, a process that checkpoints it without pause, a
// process that commits small transactions to it, and the one-shot commands
// the bundle's hooks run against it. Compiled once per pinned SQLite release
// against that release's amalgamation.
//
// usage: faultlab-sqlite <db> init
//        faultlab-sqlite <db> ready
//        faultlab-sqlite <db> checkpointer
//        faultlab-sqlite <db> writer <rows> <big-rows> <gap-us> <journal>
//        faultlab-sqlite <db> burst <rows> <journal>
//        faultlab-sqlite <db> verify <journal>
//
// Directives go to stdout, diagnostics to stderr.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "sqlite3.h"

static void fail(const char *what, sqlite3 *db) {
    fprintf(stderr, "faultlab-sqlite: %s: %s\n", what, db ? sqlite3_errmsg(db) : "");
    exit(1);
}

static void run(sqlite3 *db, const char *sql) {
    if (sqlite3_exec(db, sql, 0, 0, 0) != SQLITE_OK) fail(sql, db);
}

// No connection checkpoints on its own: an automatic checkpoint runs inside
// the committing connection, where it can never race that connection's own
// WAL reset. The checkpointer process is the only checkpointer, so every
// checkpoint runs against a writer in another process.
static sqlite3 *open_db(const char *path) {
    sqlite3 *db;
    if (sqlite3_open(path, &db) != SQLITE_OK) fail("open", db);
    sqlite3_busy_timeout(db, 10000);
    run(db, "PRAGMA journal_mode=WAL");
    run(db, "PRAGMA synchronous=NORMAL");
    run(db, "PRAGMA wal_autocheckpoint=0");
    return db;
}

// Rows carry an indexed key, so a table page that never reaches the database
// file leaves the index pointing at rows that are gone, which is what
// integrity_check reports for this bug.
static void init(sqlite3 *db) {
    run(db, "CREATE TABLE IF NOT EXISTS t(id INTEGER PRIMARY KEY, k INTEGER NOT NULL, v BLOB NOT NULL)");
    run(db, "CREATE INDEX IF NOT EXISTS t_k ON t(k)");
}

// Commit `rows` rows in one transaction and journal the largest id the
// server acknowledged. A failed commit journals nothing.
static int commit_rows(sqlite3 *db, int rows, FILE *journal, long long *counter) {
    sqlite3_stmt *ins;
    if (sqlite3_exec(db, "BEGIN IMMEDIATE", 0, 0, 0) != SQLITE_OK) return 0;
    if (sqlite3_prepare_v2(db, "INSERT INTO t(k, v) VALUES(?, zeroblob(200))", -1, &ins, 0) != SQLITE_OK) {
        sqlite3_exec(db, "ROLLBACK", 0, 0, 0);
        return 0;
    }
    for (int i = 0; i < rows; i++) {
        sqlite3_bind_int64(ins, 1, (*counter)++ % 1000);
        if (sqlite3_step(ins) != SQLITE_DONE) {
            sqlite3_finalize(ins);
            sqlite3_exec(db, "ROLLBACK", 0, 0, 0);
            return 0;
        }
        sqlite3_reset(ins);
    }
    sqlite3_finalize(ins);
    if (sqlite3_exec(db, "COMMIT", 0, 0, 0) != SQLITE_OK) {
        sqlite3_exec(db, "ROLLBACK", 0, 0, 0);
        return 0;
    }
    fprintf(journal, "%lld\n", (long long)sqlite3_last_insert_rowid(db));
    fflush(journal);
    return 1;
}

static long long last_acked(const char *journal_path) {
    long long acked = 0, value;
    FILE *journal = fopen(journal_path, "r");
    if (!journal) return 0;
    while (fscanf(journal, "%lld", &value) == 1) acked = value;
    fclose(journal);
    return acked;
}

static int verify(sqlite3 *db, const char *journal_path) {
    // The journal is read before the database is asked, so a commit that
    // lands while this runs is in both or in neither.
    long long acked = last_acked(journal_path);
    fprintf(stderr, "faultlab-sqlite: verify: journal read, acked=%lld\n", acked);
    // Reads are answered from the WAL until it is reset, so a page a
    // checkpoint skipped is still visible through the WAL. A TRUNCATE
    // checkpoint moves everything into the database file and resets the WAL;
    // after it only the database file answers.
    // The checkpointer node holds the checkpoint lock almost continuously,
    // and a checkpoint that cannot take it returns busy at once, so the
    // attempt is repeated until it gets in, sleeping between attempts so
    // the guest can idle: its clock advances only at VM exits.
    int log_frames = 0, checkpointed = 0, rc = SQLITE_BUSY;
    for (int attempt = 0; attempt < 100000 && rc == SQLITE_BUSY; attempt++) {
        rc = sqlite3_wal_checkpoint_v2(db, 0, SQLITE_CHECKPOINT_TRUNCATE, &log_frames, &checkpointed);
        if (rc == SQLITE_BUSY) usleep(100);
    }
    fprintf(stderr, "faultlab-sqlite: verify: checkpoint rc=%d log=%d done=%d\n", rc, log_frames, checkpointed);
    printf("@reachable 20\n");
    if (rc == SQLITE_BUSY) printf("@sometimes 21\n");
    if (log_frames > 0) printf("@sometimes 22\n");

    int intact = 0;
    sqlite3_stmt *check;
    if (sqlite3_prepare_v2(db, "PRAGMA integrity_check", -1, &check, 0) == SQLITE_OK) {
        if (sqlite3_step(check) == SQLITE_ROW) {
            const char *first = (const char *)sqlite3_column_text(check, 0);
            intact = first && strcmp(first, "ok") == 0;
            if (!intact) fprintf(stderr, "faultlab-sqlite: integrity_check: %s\n", first ? first : "(null)");
        }
        sqlite3_finalize(check);
    }
    printf("@always 1 %d\n", intact);

    long long count = -1, max = -1;
    sqlite3_stmt *rows;
    if (sqlite3_prepare_v2(db, "SELECT count(*), max(id) FROM t", -1, &rows, 0) == SQLITE_OK) {
        if (sqlite3_step(rows) == SQLITE_ROW) {
            count = sqlite3_column_int64(rows, 0);
            max = sqlite3_column_int64(rows, 1);
        } else {
            fprintf(stderr, "faultlab-sqlite: count: %s\n", sqlite3_errmsg(db));
        }
        sqlite3_finalize(rows);
    }
    // Ids are assigned in order and nothing deletes, so every committed row
    // is present exactly when the count reaches the largest id and that id
    // covers the journal.
    int complete = count >= 0 && max >= acked && count == max;
    if (!complete) fprintf(stderr, "faultlab-sqlite: acked=%lld max=%lld count=%lld\n", acked, max, count);
    printf("@always 2 %d\n", complete);
    return 0;
}

int main(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr, "usage: faultlab-sqlite <db> init|ready|checkpointer|writer|burst|verify ...\n");
        return 2;
    }
    const char *path = argv[1], *mode = argv[2];
    fprintf(stderr, "faultlab-sqlite: %s: opening\n", mode);
    sqlite3 *db = open_db(path);
    fprintf(stderr, "faultlab-sqlite: %s: open\n", mode);
    if (strcmp(mode, "init") == 0) {
        init(db);
    } else if (strcmp(mode, "ready") == 0) {
        run(db, "SELECT count(*) FROM t");
    } else if (strcmp(mode, "checkpointer") == 0) {
        for (;;) sqlite3_wal_checkpoint_v2(db, 0, SQLITE_CHECKPOINT_PASSIVE, 0, 0);
    } else if (strcmp(mode, "writer") == 0 && argc == 7) {
        int rows = atoi(argv[3]), big = atoi(argv[4]);
        useconds_t gap = (useconds_t)atol(argv[5]);
        FILE *journal = fopen(argv[6], "a");
        if (!journal || rows <= 0 || big <= 0) fail("writer arguments", 0);
        long long counter = 0;
        // The gap between commits is where a checkpoint completes and the
        // next commit finds the WAL fully checkpointed and resets it; a
        // writer that never pauses keeps the WAL growing and never resets.
        // Commit sizes alternate because a stale checkpoint only loses
        // frames when the generation it copies is shorter than the one its
        // frame count came from: a small commit that resets the WAL after a
        // large one leaves the frames between the two sizes uncopied.
        // A corrupt database makes every later commit fail. The writer
        // keeps trying, sleeping between attempts, so the node stays alive
        // and the searcher counts a death only where it killed something.
        for (;;) {
            if (!commit_rows(db, rows, journal, &counter)) usleep(1000);
            usleep(gap ? gap : 1);
            if (!commit_rows(db, big, journal, &counter)) usleep(1000);
            usleep(gap ? gap : 1);
        }
    } else if (strcmp(mode, "burst") == 0 && argc == 5) {
        int rows = atoi(argv[3]);
        FILE *journal = fopen(argv[4], "a");
        if (!journal || rows <= 0) fail("burst arguments", 0);
        long long counter = 0;
        printf("@reachable 23\n");
        if (!commit_rows(db, rows, journal, &counter)) printf("@sometimes 24\n");
    } else if (strcmp(mode, "verify") == 0 && argc == 4) {
        verify(db, argv[3]);
    } else {
        fprintf(stderr, "faultlab-sqlite: unknown mode %s\n", mode);
        return 2;
    }
    sqlite3_close(db);
    return 0;
}
