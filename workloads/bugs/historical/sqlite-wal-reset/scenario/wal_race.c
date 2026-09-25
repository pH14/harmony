// SPDX-License-Identifier: AGPL-3.0-or-later

#include "sqlite3.h"

#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

extern void fuzz_json_data(const char *data, size_t size);
extern void notify_coverage(uint64_t site);

#define DATABASE "/data/wal-race.db"
#define CHECKPOINT_SITE UINT64_C(0x514c0001)

static atomic_int checkpoint_active;

static void assertion(const char *id, int condition);
static void observation(const char *id, int condition);

static void mark(const char *path, int value)
{
    FILE *file = fopen(path, "w");
    if (file != NULL) {
        fprintf(file, "%d\n", value);
        fclose(file);
    }
}

static void wait_for(const char *path)
{
    struct timespec pause = {0, 1000000};
    while (access(path, F_OK) != 0)
        nanosleep(&pause, NULL);
}

static int marked_value(const char *path)
{
    FILE *file = fopen(path, "r");
    int value = -1;
    if (file != NULL) {
        if (fscanf(file, "%d", &value) != 1)
            value = -1;
        fclose(file);
    }
    return value;
}

void harmony_test_reset(void)
{
    mark("/run/writer-reset", 1);
}

void harmony_test_state(int phase, unsigned old, unsigned live, unsigned backfill)
{
    char json[256];
    char id[128];
    int length;
    if (!atomic_load_explicit(&checkpoint_active, memory_order_acquire))
        return;
    snprintf(id, sizeof(id), "checkpoint-%d-old-%u-live-%u-backfill-%u",
             phase, old, live, backfill);
    length = snprintf(json, sizeof(json),
        "{\"antithesis_assert\":{\"id\":\"%s\",\"message\":\"%s\","
        "\"assert_type\":\"sometimes\",\"condition\":true,\"hit\":true}}\n",
        id, id);
    if (length > 0 && (size_t)length < sizeof(json))
        fuzz_json_data(json, (size_t)length);
    if (phase == 0)
        observation("wal-reset-before-checkpoint", old > live && backfill == 0);
    if (phase == 1)
        observation("stale-backfill-advanced", backfill > live);
}

void harmony_test_checkpoint_ready(void)
{
    if (atomic_load_explicit(&checkpoint_active, memory_order_acquire)) {
        mark("/run/checkpoint-reached", 1);
        notify_coverage(CHECKPOINT_SITE);
        assertion("writer-finished-during-pause",
                  marked_value("/run/writer-done") == 1);
        assertion("writer-reset-during-pause",
                  marked_value("/run/writer-reset") == 1);
    }
}

static void emit(const char *text)
{
    fuzz_json_data(text, strlen(text));
}

static void assertion(const char *id, int condition)
{
    char json[512];
    int length = snprintf(json, sizeof(json),
        "{\"antithesis_assert\":{\"id\":\"%s\",\"message\":\"%s\","
        "\"assert_type\":\"always\",\"condition\":%s,\"hit\":true,"
        "\"must_hit\":true,\"location\":{\"file\":\"wal_race.c\","
        "\"function\":\"run\",\"begin_line\":0}}}\n",
        id, id, condition ? "true" : "false");
    if (length > 0 && (size_t)length < sizeof(json))
        fuzz_json_data(json, (size_t)length);
}

static void observation(const char *id, int condition)
{
    char json[256];
    int length = snprintf(json, sizeof(json),
        "{\"antithesis_assert\":{\"id\":\"%s\",\"message\":\"%s\","
        "\"assert_type\":\"sometimes\",\"condition\":%s,\"hit\":true}}\n",
        id, id, condition ? "true" : "false");
    if (length > 0 && (size_t)length < sizeof(json))
        fuzz_json_data(json, (size_t)length);
}

static int execute(sqlite3 *db, const char *sql)
{
    int rc = sqlite3_exec(db, sql, NULL, NULL, NULL);
    if (rc != SQLITE_OK)
        fprintf(stderr, "SQL failed (%d): %s: %s\n", rc, sql, sqlite3_errmsg(db));
    return rc;
}

static sqlite3 *open_database(void)
{
    sqlite3 *db = NULL;
    if (sqlite3_open(DATABASE, &db) != SQLITE_OK)
        return NULL;
    sqlite3_busy_timeout(db, 5000);
    if (execute(db, "PRAGMA mmap_size=67108864") != SQLITE_OK)
        return NULL;
    return db;
}

static int scalar(sqlite3 *db, const char *sql)
{
    sqlite3_stmt *statement = NULL;
    int value = -1;
    if (sqlite3_prepare_v2(db, sql, -1, &statement, NULL) == SQLITE_OK &&
        sqlite3_step(statement) == SQLITE_ROW)
        value = sqlite3_column_int(statement, 0);
    sqlite3_finalize(statement);
    return value;
}

static int setup(void)
{
    unlink("/run/wal-race-start");
    unlink("/run/first-checkpoint-complete");
    unlink("/run/writer-ready");
    unlink("/run/checkpoint-reached");
    unlink("/run/writer-done");
    unlink("/run/writer-reset");
    unlink("/run/checkpoint-resumed");
    unlink("/run/writer-final-done");
    sqlite3 *db = open_database();
    int rc;
    if (db == NULL)
        return 1;
    rc = execute(db,
        "PRAGMA journal_mode=WAL;"
        "PRAGMA synchronous=FULL;"
        "CREATE TABLE t1(a INTEGER PRIMARY KEY,b BLOB);"
        "CREATE TABLE canary(a INTEGER PRIMARY KEY);"
        "WITH RECURSIVE s(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM s WHERE i<4096)"
        " INSERT INTO t1 SELECT NULL,randomblob(3900) FROM s;"
        "PRAGMA wal_checkpoint(TRUNCATE);");
    sqlite3_close(db);
    if (rc == SQLITE_OK)
        emit("{\"antithesis_setup\":{\"status\":\"complete\"}}\n");
    return rc == SQLITE_OK ? 0 : 1;
}

static int writer(void)
{
    sqlite3 *db;
    struct timespec pause = {0, 1000000};
    int rc;
    wait_for("/run/first-checkpoint-complete");
    db = open_database();
    if (db == NULL)
        return 1;
    assertion("writer-starts-empty", scalar(db, "SELECT count(*) FROM canary") == 0);
    mark("/run/writer-ready", 1);
    wait_for("/run/checkpoint-reached");
    rc = execute(db, "INSERT INTO canary VALUES(1)");
    if (rc != SQLITE_OK) {
        char failure[64];
        snprintf(failure, sizeof(failure), "writer-insert-rc-%d-ext-%d",
                 rc, sqlite3_extended_errcode(db));
        assertion(failure, 0);
    }
    mark("/run/writer-done", rc == SQLITE_OK);
    wait_for("/run/checkpoint-resumed");
    rc = execute(db, "INSERT INTO canary VALUES(2)");
    if (rc != SQLITE_OK) {
        char failure[64];
        snprintf(failure, sizeof(failure), "writer-final-insert-rc-%d-ext-%d",
                 rc, sqlite3_extended_errcode(db));
        assertion(failure, 0);
    }
    mark("/run/writer-final-done", rc == SQLITE_OK);
    for (;;)
        nanosleep(&pause, NULL);
}

static int run(void)
{
    sqlite3 *checkpoint = NULL;
    sqlite3 *helper = NULL;
    struct timespec pause = {0, 1000000};
    int log_frames = 0;
    int backfilled = 0;
    int committed;
    int recovered;
    int rc;
    sqlite3 *probe;

    wait_for("/run/wal-race-start");
    unlink("/run/first-checkpoint-complete");
    unlink("/run/writer-ready");
    unlink("/run/checkpoint-reached");
    unlink("/run/writer-done");
    unlink("/run/writer-reset");
    unlink("/run/checkpoint-resumed");
    unlink("/run/writer-final-done");
    checkpoint = open_database();
    helper = open_database();
    if (checkpoint == NULL || helper == NULL)
        return 1;

    if (execute(helper, "DELETE FROM canary") != SQLITE_OK ||
        sqlite3_wal_checkpoint_v2(helper, "main", SQLITE_CHECKPOINT_TRUNCATE,
                                  &log_frames, &backfilled) != SQLITE_OK)
        return 1;

    if (scalar(checkpoint, "SELECT count(*) FROM t1 WHERE b IS NOT NULL") != 4096 ||
        execute(helper, "UPDATE t1 SET b=randomblob(3900) WHERE a<=512") != SQLITE_OK)
        return 1;
    for (int attempt = 0; attempt < 50; attempt++) {
        if (sqlite3_wal_checkpoint_v2(helper, "main", SQLITE_CHECKPOINT_PASSIVE,
                                      &log_frames, &backfilled) != SQLITE_OK)
            return 1;
        if (backfilled >= log_frames)
            break;
    }
    if (backfilled < log_frames || log_frames <= 1)
        return 1;

    assertion("old-header-exceeds-new-frames", 1);
    unlink("/run/writer-reset");
    mark("/run/first-checkpoint-complete", 1);
    wait_for("/run/writer-ready");
    atomic_store_explicit(&checkpoint_active, 1, memory_order_release);
    sqlite3_wal_checkpoint_v2(checkpoint, "main", SQLITE_CHECKPOINT_PASSIVE,
                              &log_frames, &backfilled);
    atomic_store_explicit(&checkpoint_active, 0, memory_order_release);
    mark("/run/checkpoint-resumed", 1);
    wait_for("/run/writer-final-done");
    committed = marked_value("/run/writer-done") +
                marked_value("/run/writer-final-done");
    assertion("writer-made-two-commits", committed == 2);
    if (committed != 2)
        return 1;
    rc = sqlite3_wal_checkpoint_v2(helper, "main", SQLITE_CHECKPOINT_TRUNCATE,
                                   &log_frames, &backfilled);
    assertion("final-truncate-checkpoint-completed", rc == SQLITE_OK);
    sqlite3_close(helper);
    sqlite3_close(checkpoint);
    probe = open_database();
    if (probe == NULL)
        return 1;
    recovered = scalar(probe, "SELECT count(*) FROM canary");
    sqlite3_close(probe);
    assertion("no-lost-committed-writes", recovered == committed);
    observation("final-canary-read-completed", 1);
    fprintf(stderr, "committed=%d recovered=%d\n", committed, recovered);
    for (;;)
        nanosleep(&pause, NULL);
}

int main(int argc, char **argv)
{
    if (argc == 2 && strcmp(argv[1], "setup") == 0)
        return setup();
    if (argc == 2 && strcmp(argv[1], "run") == 0)
        return run();
    if (argc == 2 && strcmp(argv[1], "writer") == 0)
        return writer();
    fprintf(stderr, "usage: wal-race setup|run|writer\n");
    return 2;
}
