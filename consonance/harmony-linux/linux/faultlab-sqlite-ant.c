// SPDX-License-Identifier: AGPL-3.0-or-later
// The SQLite WAL-reset workload as Antithesis ran it
// (bugs/historical/sqlite-wal-reset): two identical writer processes on one
// WAL-mode database, each mixing small write transactions, checkpoints of
// every mode and correctness sweeps, with automatic checkpoints on. Ported
// from antithesis/workload.c on the antithesishq/sqlite branch
// 3.51.2-instrumented; the SQL and the mix are theirs.
//
// usage: faultlab-sqlite-ant <db> init
//        faultlab-sqlite-ant <db> ready
//        faultlab-sqlite-ant <db> run <writer> <seed>
//        faultlab-sqlite-ant <db> verify
//
// Directives go to stdout, diagnostics to stderr.
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "faultlab-edge.h"
#include "sqlite3.h"

// The upstream workload draws from /dev/urandom so its platform can steer
// the draws. Here the draws come from a seed on the kernel command line, so
// the schedule is the only thing a run varies.
static uint64_t g_state;
static uint32_t rnd_u32(void) {
    uint64_t z = (g_state += 0x9e3779b97f4a7c15ull);
    z = (z ^ (z >> 30)) * 0xbf58476d1ce4e5b9ull;
    z = (z ^ (z >> 27)) * 0x94d049bb133111ebull;
    return (uint32_t)(z ^ (z >> 31));
}
static uint32_t rnd_below(uint32_t n) { return n ? rnd_u32() % n : 0; }

static void fail(const char *what, sqlite3 *db) {
    fprintf(stderr, "faultlab-sqlite-ant: %s: %s\n", what, db ? sqlite3_errmsg(db) : "");
    exit(1);
}

// A failed in-process check ends the process the way a failed SQLITE_DEBUG
// assertion does, so the fault agent counts it as a death. The failure is
// also written next to the database, because the hook that verifies at the
// end is the only process whose output reaches the oracle.
static char g_failed_path[256];
static void check(int cond, const char *what) {
    if (cond) return;
    fprintf(stderr, "faultlab-sqlite-ant: check failed: %s\n", what);
    FILE *f = fopen(g_failed_path, "a");
    if (f) {
        fprintf(f, "%s\n", what);
        fclose(f);
    }
    abort();
}

static int run(sqlite3 *db, const char *sql) {
    char *err = NULL;
    int rc = sqlite3_exec(db, sql, NULL, NULL, &err);
    if (err) sqlite3_free(err);
    return rc;
}

static int is_busy(int rc) {
    rc &= 0xff;
    return rc == SQLITE_BUSY || rc == SQLITE_LOCKED;
}

static int checkpoint(sqlite3 *db, const char *mode, int *busy, int *nLog, int *nCkpt) {
    char sql[64];
    sqlite3_stmt *st = NULL;
    snprintf(sql, sizeof(sql), "PRAGMA wal_checkpoint(%s);", mode);
    int rc = sqlite3_prepare_v2(db, sql, -1, &st, NULL);
    if (rc != SQLITE_OK) { if (st) sqlite3_finalize(st); return rc; }
    *busy = *nLog = *nCkpt = 0;
    if (sqlite3_step(st) == SQLITE_ROW) {
        *busy = sqlite3_column_int(st, 0);
        *nLog = sqlite3_column_int(st, 1);
        *nCkpt = sqlite3_column_int(st, 2);
    }
    sqlite3_finalize(st);
    return SQLITE_OK;
}

static long query_long(sqlite3 *db, const char *sql, int *ok) {
    sqlite3_stmt *st = NULL;
    long v = -1;
    *ok = 0;
    if (sqlite3_prepare_v2(db, sql, -1, &st, NULL) == SQLITE_OK && sqlite3_step(st) == SQLITE_ROW) {
        v = (long)sqlite3_column_int64(st, 0);
        *ok = 1;
    }
    if (st) sqlite3_finalize(st);
    return v;
}

static int integrity_ok(sqlite3 *db, const char *pragma, int *ran) {
    sqlite3_stmt *st = NULL;
    int ok = 0;
    *ran = 0;
    if (sqlite3_prepare_v2(db, pragma, -1, &st, NULL) == SQLITE_OK && sqlite3_step(st) == SQLITE_ROW) {
        const unsigned char *txt = sqlite3_column_text(st, 0);
        *ran = 1;
        ok = txt && strcmp((const char *)txt, "ok") == 0;
        if (!ok) fprintf(stderr, "faultlab-sqlite-ant: %s: %s\n", pragma, txt ? (const char *)txt : "(null)");
    }
    if (st) sqlite3_finalize(st);
    return ok;
}

static void init(sqlite3 *db) {
    run(db, "PRAGMA page_size=4096;");
    if (run(db, "PRAGMA journal_mode=WAL;") != SQLITE_OK) fail("journal_mode=WAL", db);
    if (run(db,
            "BEGIN;"
            "CREATE TABLE IF NOT EXISTS t(id INTEGER PRIMARY KEY, writer INT, seq INT, blob BLOB);"
            "CREATE INDEX IF NOT EXISTS t_ws ON t(writer, seq);"
            "CREATE TABLE IF NOT EXISTS ctr(writer INT PRIMARY KEY, v INT);"
            "CREATE TABLE IF NOT EXISTS progress(writer INT PRIMARY KEY, committed_seq INT, v INT);"
            "CREATE TABLE IF NOT EXISTS churn(id INTEGER PRIMARY KEY, blob BLOB);"
            "INSERT OR IGNORE INTO ctr VALUES(0,0),(1,0);"
            "INSERT OR IGNORE INTO progress VALUES(0,0,0),(1,0,0);"
            "COMMIT;") != SQLITE_OK)
        fail("schema", db);
    // Seed rows give the writers existing pages to dirty.
    run(db,
        "WITH RECURSIVE s(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM s WHERE i<300)"
        "INSERT INTO churn(blob) SELECT randomblob(400) FROM s;");
    int b, l, c;
    checkpoint(db, "TRUNCATE", &b, &l, &c);
}

static int ready(sqlite3 *db) {
    int ok;
    long n = query_long(db, "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='t';", &ok);
    return ok && n == 1 ? 0 : 1;
}

static void writer(sqlite3 *db, int W) {
    long committedSeq = 0, lastV = 0;
    int ok;
    run(db, "PRAGMA busy_timeout=2000;");
    run(db, "PRAGMA synchronous=NORMAL;");
    run(db, "PRAGMA wal_autocheckpoint=200;");
    run(db, "PRAGMA journal_mode=WAL;");

    // Committed state must survive a restart of this process.
    {
        char q[128];
        int rok, integ;
        snprintf(q, sizeof(q), "SELECT committed_seq FROM progress WHERE writer=%d;", W);
        long pSeq = query_long(db, q, &rok);
        snprintf(q, sizeof(q), "SELECT v FROM progress WHERE writer=%d;", W);
        long pV = query_long(db, q, &rok);
        snprintf(q, sizeof(q), "SELECT COUNT(*) FROM t WHERE writer=%d;", W);
        long cnt = query_long(db, q, &rok);
        snprintf(q, sizeof(q), "SELECT v FROM ctr WHERE writer=%d;", W);
        long ctrv = query_long(db, q, &rok);
        integ = integrity_ok(db, "PRAGMA quick_check;", &rok);
        if (pSeq < 0) pSeq = 0;
        if (pV < 0) pV = 0;
        check(cnt >= pSeq && ctrv >= pV && integ, "recovery-preserves-committed");
        committedSeq = pSeq;
        lastV = pV;
    }

    for (;;) {
        uint32_t action = rnd_below(100);
        if (action < 70) {
            char sql[512];
            long seq = committedSeq + 1;
            long v = lastV + 1;
            int sz = 64 + (int)rnd_below(1600);
            int c1 = 1 + (int)rnd_below(300);
            int c2 = 1 + (int)rnd_below(300);
            int c3 = 1 + (int)rnd_below(300);
            snprintf(sql, sizeof(sql),
                     "BEGIN IMMEDIATE;"
                     "INSERT INTO t(writer,seq,blob) VALUES(%d,%ld,randomblob(%d));"
                     "UPDATE ctr SET v=%ld WHERE writer=%d;"
                     "UPDATE churn SET blob=randomblob(%d) WHERE id IN (%d,%d,%d);"
                     "UPDATE progress SET committed_seq=%ld, v=%ld WHERE writer=%d;"
                     "COMMIT;",
                     W, seq, sz, v, W, sz, c1, c2, c3, seq, v, W);
            int rc = run(db, sql);
            if (rc == SQLITE_OK) {
                committedSeq = seq;
                lastV = v;
                char q[128];
                snprintf(q, sizeof(q), "SELECT seq FROM t WHERE writer=%d AND seq=%ld;", W, seq);
                long got = query_long(db, q, &ok);
                check(ok && got == seq, "read-your-writes: row");
                snprintf(q, sizeof(q), "SELECT v FROM ctr WHERE writer=%d;", W);
                got = query_long(db, q, &ok);
                check(ok && got == lastV, "read-your-writes: counter");
            } else if (is_busy(rc)) {
                run(db, "ROLLBACK;");
            } else {
                check((rc & 0xff) != SQLITE_CORRUPT, "no-corruption: write reported SQLITE_CORRUPT");
                run(db, "ROLLBACK;");
            }
        } else if (action < 90) {
            const char *mode;
            int b, l, c, pick = (int)rnd_below(100);
            if (pick < 70) mode = "PASSIVE";
            else if (pick < 85) mode = "RESTART";
            else if (pick < 95) mode = "TRUNCATE";
            else mode = "FULL";
            checkpoint(db, mode, &b, &l, &c);
        } else {
            char q[128];
            int ran;
            snprintf(q, sizeof(q), "SELECT COUNT(*) FROM t WHERE writer=%d;", W);
            long cnt = query_long(db, q, &ok);
            snprintf(q, sizeof(q), "SELECT COALESCE(MAX(seq),0) FROM t WHERE writer=%d;", W);
            long mx = query_long(db, q, &ok);
            check(cnt == committedSeq && mx == committedSeq, "no-lost-committed-writes");
            snprintf(q, sizeof(q), "SELECT v FROM ctr WHERE writer=%d;", W);
            long ctrv = query_long(db, q, &ok);
            check(ok && ctrv >= lastV, "committed-counter-monotonic");
            int integ = integrity_ok(db, rnd_below(4) == 0 ? "PRAGMA integrity_check;" : "PRAGMA quick_check;", &ran);
            check(!ran || integ, "integrity-check-clean");
        }
        if (rnd_below(8) == 0) usleep(rnd_below(500));
    }
}

// The hook's view: after a TRUNCATE checkpoint only the database file
// answers, so a page a stale checkpoint skipped is gone rather than still
// served from the WAL. Each writer's progress row is committed in the same
// transaction as its rows, so it is the record of what was acknowledged.
static void verify(sqlite3 *db) {
    run(db, "PRAGMA busy_timeout=2000;");
    // A connection discovers WAL mode on its first read; a checkpoint
    // before that is a no-op.
    run(db, "SELECT count(*) FROM sqlite_master;");
    int log = 0, done = 0, rc = SQLITE_BUSY;
    for (int attempt = 0; attempt < 20000 && rc == SQLITE_BUSY; attempt++) {
        rc = sqlite3_wal_checkpoint_v2(db, 0, SQLITE_CHECKPOINT_TRUNCATE, &log, &done);
        if (rc == SQLITE_BUSY) usleep(100);
    }
    fprintf(stderr, "faultlab-sqlite-ant: verify: checkpoint rc=%d log=%d done=%d\n", rc, log, done);
    printf("@reachable 10\n");
    if (rc == SQLITE_BUSY) printf("@sometimes 11\n");
    if (log > 0) printf("@sometimes 12\n");

    int ran;
    int intact = integrity_ok(db, "PRAGMA integrity_check;", &ran);
    printf("@always 1 %d\n", ran && intact);

    // One read transaction, so the three answers come from one snapshot
    // and a commit landing between them cannot fake a mismatch.
    int complete = run(db, "BEGIN;") == SQLITE_OK;
    for (int W = 0; W < 2; W++) {
        char q[128];
        int ok;
        snprintf(q, sizeof(q), "SELECT committed_seq FROM progress WHERE writer=%d;", W);
        long committed = query_long(db, q, &ok);
        snprintf(q, sizeof(q), "SELECT COUNT(*) FROM t WHERE writer=%d;", W);
        long cnt = query_long(db, q, &ok);
        snprintf(q, sizeof(q), "SELECT COALESCE(MAX(seq),0) FROM t WHERE writer=%d;", W);
        long mx = query_long(db, q, &ok);
        if (!(ok && cnt == committed && mx == committed)) {
            complete = 0;
            fprintf(stderr, "faultlab-sqlite-ant: writer %d: committed=%ld count=%ld max=%ld\n", W, committed, cnt, mx);
        }
    }
    run(db, "COMMIT;");
    printf("@always 2 %d\n", complete);

    int in_process_ok = 1;
    FILE *f = fopen(g_failed_path, "r");
    if (f) {
        char line[256];
        while (fgets(line, sizeof(line), f)) {
            in_process_ok = 0;
            fprintf(stderr, "faultlab-sqlite-ant: verify: a writer failed: %s", line);
        }
        fclose(f);
    }
    printf("@always 3 %d\n", in_process_ok);
}

int main(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr, "usage: faultlab-sqlite-ant <db> init|ready|run|verify ...\n");
        return 2;
    }
    faultlab_edge_init();
    const char *path = argv[1], *mode = argv[2];
    snprintf(g_failed_path, sizeof(g_failed_path), "%s-failed", path);
    sqlite3 *db = NULL;
    if (sqlite3_open(path, &db) != SQLITE_OK) fail("open", db);
    if (strcmp(mode, "init") == 0) {
        init(db);
    } else if (strcmp(mode, "ready") == 0) {
        int rc = ready(db);
        sqlite3_close(db);
        return rc;
    } else if (strcmp(mode, "run") == 0 && argc == 5) {
        int W = atoi(argv[3]);
        g_state = (uint64_t)strtoull(argv[4], NULL, 10) * 2 + (uint64_t)W;
        writer(db, W);
    } else if (strcmp(mode, "verify") == 0) {
        verify(db);
    } else {
        fprintf(stderr, "faultlab-sqlite-ant: unknown mode %s\n", mode);
        return 2;
    }
    sqlite3_close(db);
    return 0;
}
