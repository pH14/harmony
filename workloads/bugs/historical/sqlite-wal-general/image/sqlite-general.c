/* SPDX-License-Identifier: AGPL-3.0-or-later */
/* sqlite-general: the general-discovery SQLite workload. See ../README.md and
 * ../../DISCOVERY.md for the frozen specification this file implements.
 *
 *   sqlite-general init DB          create the empty WAL database
 *   sqlite-general client ID DB     run one client forever
 *   sqlite-general verify ID DB     run the client's recovery comparison and
 *                                   one integrity check, then exit
 *
 * This is the single translation unit that includes the fork's coverage
 * header, so libvoidstar receives the trace-pc-guard callbacks. */
#include "antithesis_instrumentation.h"
#include "harmony_general.h"
#include "sqlite3.h"

#define MAX_ROWS 4096
#define MAX_STATEMENTS 256
#define MAX_BODY 4096

enum site { SITE_OP, SITE_STATEMENTS, SITE_THINK, SITE_KIND, SITE_BEGIN, SITE_CHECKPOINT, SITE_REOPEN, SITE_BODY };

static const char *A_INTEGRITY = "sqlite integrity_check returns ok";
static const char *A_CORRUPT = "sqlite reports no corruption";
static const char *A_COMMITS = "sqlite preserves acknowledged commits";
static const char *A_SNAPSHOT = "sqlite read transaction sees one snapshot";
static const char *R_INTEGRITY = "sqlite integrity_check completed";
static const char *R_COMPARED = "sqlite general compared committed rows";

struct row {
    int64_t key;
    int64_t ver;
    int64_t len;
    uint64_t hash;
};

struct table {
    int count;
    struct row rows[MAX_ROWS];
};

struct write_op {
    char kind;
    struct row row;
};

struct txn {
    int64_t id;
    int count;
    struct write_op ops[MAX_STATEMENTS];
};

struct client {
    int id;
    const char *path;
    sqlite3 *db;
    struct gen_rng rng;
    struct table model;
    struct txn pending;
    int has_pending;
    int64_t next_seq;
    int64_t next_txn;
    int journal;
    char journal_path[256];
};

static uint64_t fnv64(const unsigned char *data, size_t length) {
    uint64_t hash = 0xcbf29ce484222325ULL;
    for (size_t i = 0; i < length; i++) {
        hash ^= data[i];
        hash *= 0x100000001b3ULL;
    }
    return hash;
}

static int is_corrupt(int rc) {
    rc &= 0xff;
    return rc == SQLITE_CORRUPT || rc == SQLITE_NOTADB;
}

static void note_error(sqlite3 *db, int rc, const char *where) {
    if (is_corrupt(rc)) {
        char detail[256];
        snprintf(detail, sizeof detail, "%s: %s", where, db ? sqlite3_errmsg(db) : "");
        gen_always(A_CORRUPT, 0, detail);
    }
}

static int exec(sqlite3 *db, const char *sql, const char *where) {
    int rc = sqlite3_exec(db, sql, NULL, NULL, NULL);
    if (rc != SQLITE_OK) note_error(db, rc, where);
    return rc;
}

static int find_row(const struct table *table, int64_t key) {
    for (int i = 0; i < table->count; i++)
        if (table->rows[i].key == key) return i;
    return -1;
}

static void apply_op(struct table *table, const struct write_op *op) {
    int at = find_row(table, op->row.key);
    if (op->kind == 'D') {
        if (at >= 0) table->rows[at] = table->rows[--table->count];
    } else if (at >= 0) {
        table->rows[at] = op->row;
    } else if (table->count < MAX_ROWS) {
        table->rows[table->count++] = op->row;
    }
}

static int row_order(const void *a, const void *b) {
    const struct row *x = a, *y = b;
    return (x->key > y->key) - (x->key < y->key);
}

static int same_table(struct table *a, struct table *b) {
    if (a->count != b->count) return 0;
    qsort(a->rows, (size_t)a->count, sizeof a->rows[0], row_order);
    qsort(b->rows, (size_t)b->count, sizeof b->rows[0], row_order);
    return memcmp(a->rows, b->rows, sizeof a->rows[0] * (size_t)a->count) == 0;
}

static void journal_line(struct client *c, const char *line) {
    size_t length = strlen(line);
    if (write(c->journal, line, length) != (ssize_t)length) {
        fprintf(stderr, "sqlite-general: journal write failed\n");
        exit(1);
    }
}

static void journal_pending(struct client *c, const struct txn *t) {
    char line[MAX_STATEMENTS * 96 + 64];
    int at = snprintf(line, sizeof line, "P %lld %d", (long long)t->id, t->count);
    for (int i = 0; i < t->count; i++) {
        const struct row *r = &t->ops[i].row;
        at += snprintf(line + at, sizeof line - (size_t)at, " %c:%lld:%lld:%lld:%llu", t->ops[i].kind,
                       (long long)r->key, (long long)r->ver, (long long)r->len,
                       (unsigned long long)r->hash);
    }
    snprintf(line + at, sizeof line - (size_t)at, "\n");
    journal_line(c, line);
}

static void journal_outcome(struct client *c, char outcome, int64_t id) {
    char line[64];
    snprintf(line, sizeof line, "%c %lld\n", outcome, (long long)id);
    journal_line(c, line);
}

static int parse_pending(char *text, struct txn *t) {
    char *save = NULL;
    char *word = strtok_r(text, " \n", &save);
    if (!word || strcmp(word, "P") != 0) return 0;
    if (!(word = strtok_r(NULL, " \n", &save))) return 0;
    t->id = atoll(word);
    if (!(word = strtok_r(NULL, " \n", &save))) return 0;
    t->count = atoi(word);
    if (t->count < 1 || t->count > MAX_STATEMENTS) return 0;
    for (int i = 0; i < t->count; i++) {
        long long key, ver, len;
        unsigned long long hash;
        char kind;
        if (!(word = strtok_r(NULL, " \n", &save))) return 0;
        if (sscanf(word, "%c:%lld:%lld:%lld:%llu", &kind, &key, &ver, &len, &hash) != 5) return 0;
        t->ops[i].kind = kind;
        t->ops[i].row = (struct row){key, ver, len, hash};
    }
    return 1;
}

/* Replays the journal into the model. Only complete lines count: a torn
 * pending line never had its COMMIT issued. A kill can tear a line, so the
 * journal is cut back to its last complete line before anything is appended;
 * otherwise the next line would join the torn one and hide the history after it. */
static void load_journal(struct client *c) {
    FILE *file = fopen(c->journal_path, "r");
    char line[MAX_STATEMENTS * 96 + 64];
    long good = 0;
    memset(&c->model, 0, sizeof c->model);
    c->has_pending = 0;
    c->next_seq = 1;
    c->next_txn = 1;
    if (!file) return;
    while (fgets(line, sizeof line, file)) {
        size_t length = strlen(line);
        if (length == 0 || line[length - 1] != '\n') break;
        if (line[0] == 'P') {
            struct txn t;
            if (!parse_pending(line, &t)) break;
            c->pending = t;
            c->has_pending = 1;
            if (t.id >= c->next_txn) c->next_txn = t.id + 1;
            for (int i = 0; i < t.count; i++) {
                int64_t seq = t.ops[i].row.key & ((1LL << 40) - 1);
                if (seq >= c->next_seq) c->next_seq = seq + 1;
            }
        } else if (line[0] == 'A' || line[0] == 'F' || line[0] == 'R') {
            long long id = 0;
            int applied = line[0] == 'A';
            if (line[0] == 'R') {
                int flag = 0;
                if (sscanf(line + 2, "%lld %d", &id, &flag) != 2) break;
                applied = flag == 1;
            } else if (sscanf(line + 2, "%lld", &id) != 1) {
                break;
            }
            if (c->has_pending && c->pending.id == id) {
                if (applied)
                    for (int i = 0; i < c->pending.count; i++) apply_op(&c->model, &c->pending.ops[i]);
                c->has_pending = 0;
            }
        } else {
            break;
        }
        good = ftell(file);
    }
    fseek(file, 0, SEEK_END);
    if (ftell(file) != good) {
        fprintf(stderr, "sqlite-general: client %d journal cut from %ld to %ld bytes\n", c->id, ftell(file), good);
        if (truncate(c->journal_path, good) != 0) {
            fprintf(stderr, "sqlite-general: journal truncate failed\n");
            exit(1);
        }
    }
    fclose(file);
}

/* Reads the owned rows. Returns SQLITE_OK or the failing code. */
static int read_owned(struct client *c, struct table *out) {
    sqlite3_stmt *st = NULL;
    int rc = sqlite3_prepare_v2(c->db, "SELECT k, ver, body FROM kv WHERE owner = ?1 ORDER BY k", -1, &st, NULL);
    out->count = 0;
    if (rc != SQLITE_OK) {
        note_error(c->db, rc, "prepare owned read");
        return rc;
    }
    sqlite3_bind_int(st, 1, c->id);
    while ((rc = sqlite3_step(st)) == SQLITE_ROW) {
        struct row r;
        const void *body = sqlite3_column_blob(st, 2);
        r.key = sqlite3_column_int64(st, 0);
        r.ver = sqlite3_column_int64(st, 1);
        r.len = sqlite3_column_bytes(st, 2);
        r.hash = fnv64(body, (size_t)r.len);
        if (out->count < MAX_ROWS) out->rows[out->count++] = r;
    }
    sqlite3_finalize(st);
    if (rc != SQLITE_DONE) {
        note_error(c->db, rc, "owned read");
        return rc;
    }
    return SQLITE_OK;
}

/* Compares a read with the acknowledged history and resolves an
 * indeterminate transaction. */
static void compare_owned(struct client *c, struct table *seen) {
    struct table expected = c->model;
    char detail[160];
    if (same_table(seen, &expected)) {
        if (c->has_pending) {
            char line[64];
            snprintf(line, sizeof line, "R %lld 0\n", (long long)c->pending.id);
            journal_line(c, line);
            c->has_pending = 0;
        }
        gen_reached(R_COMPARED);
        gen_always(A_COMMITS, 1, NULL);
        return;
    }
    if (c->has_pending) {
        struct table applied = c->model;
        for (int i = 0; i < c->pending.count; i++) apply_op(&applied, &c->pending.ops[i]);
        if (same_table(seen, &applied)) {
            char line[64];
            snprintf(line, sizeof line, "R %lld 1\n", (long long)c->pending.id);
            journal_line(c, line);
            c->model = applied;
            c->has_pending = 0;
            gen_reached(R_COMPARED);
            gen_always(A_COMMITS, 1, NULL);
            return;
        }
    }
    snprintf(detail, sizeof detail, "client %d saw %d owned rows, history has %d", c->id, seen->count,
             c->model.count);
    fprintf(stderr, "sqlite-general: %s, pending %lld\n", detail, c->has_pending ? (long long)c->pending.id : -1LL);
    gen_reached(R_COMPARED);
    gen_always(A_COMMITS, 0, detail);
}

static void apply_settings(struct client *c, int busy_ms, const char *sync, int autockpt, int cache) {
    char sql[200];
    snprintf(sql, sizeof sql,
             "PRAGMA busy_timeout=%d; PRAGMA synchronous=%s; PRAGMA wal_autocheckpoint=%d; PRAGMA cache_size=%d;",
             busy_ms, sync, autockpt, cache);
    exec(c->db, sql, "connection settings");
}

static void open_db(struct client *c) {
    int rc = sqlite3_open_v2(c->path, &c->db, SQLITE_OPEN_READWRITE, NULL);
    if (rc != SQLITE_OK) {
        note_error(c->db, rc, "open");
        fprintf(stderr, "sqlite-general: open failed: %s\n", sqlite3_errmsg(c->db));
        exit(1);
    }
}

static void op_write(struct client *c) {
    static const char *modes[] = {"BEGIN DEFERRED", "BEGIN IMMEDIATE", "BEGIN EXCLUSIVE"};
    static const int64_t body_sizes[] = {0, 256, 2048, MAX_BODY};
    static struct table local, saved;
    struct txn t = {.id = c->next_txn++, .count = 0};
    int level = gen_bias(&c->rng, SITE_STATEMENTS, 9);
    int statements = level < 0 ? 1 + (int)gen_below(&c->rng, 8) : 1 << level;
    int body_level = gen_bias(&c->rng, SITE_BODY, 4);
    unsigned form = gen_pick(&c->rng, SITE_BEGIN, 9);
    int savepoint = (int)(form / 3);
    int savepoint_at = savepoint ? (int)gen_below(&c->rng, (uint64_t)statements) : -1;
    int saved_count = 0;
    int rc = exec(c->db, modes[form % 3], "begin write");
    local = c->model;
    if (rc != SQLITE_OK) return;
    for (int i = 0; i < statements; i++) {
        unsigned char body[MAX_BODY];
        if (i == savepoint_at) {
            if (exec(c->db, "SAVEPOINT sp", "savepoint") != SQLITE_OK) {
                sqlite3_exec(c->db, "ROLLBACK", NULL, NULL, NULL);
                return;
            }
            saved = local;
            saved_count = t.count;
        }
        int kind = (int)gen_pick(&c->rng, SITE_KIND, 3);
        struct write_op op;
        sqlite3_stmt *st = NULL;
        if (local.count == 0 || local.count >= MAX_ROWS - MAX_STATEMENTS) kind = local.count == 0 ? 0 : 2;
        if (kind == 0) {
            op.kind = 'I';
            op.row.key = ((int64_t)(c->id + 1) << 40) | c->next_seq++;
            op.row.ver = 1;
        } else {
            struct row *target = &local.rows[gen_below(&c->rng, (uint64_t)local.count)];
            op.kind = kind == 1 ? 'U' : 'D';
            op.row = *target;
            op.row.ver = target->ver + 1;
        }
        op.row.len = body_level < 0 ? (int64_t)gen_below(&c->rng, MAX_BODY + 1) : body_sizes[body_level];
        for (int64_t b = 0; b < op.row.len; b++) body[b] = (unsigned char)gen_next(&c->rng);
        op.row.hash = fnv64(body, (size_t)op.row.len);
        if (op.kind == 'I')
            rc = sqlite3_prepare_v2(c->db, "INSERT INTO kv(k, owner, ver, body) VALUES(?1, ?2, ?3, ?4)", -1, &st, NULL);
        else if (op.kind == 'U')
            rc = sqlite3_prepare_v2(c->db, "UPDATE kv SET ver = ?3, body = ?4 WHERE k = ?1 AND owner = ?2", -1, &st, NULL);
        else
            rc = sqlite3_prepare_v2(c->db, "DELETE FROM kv WHERE k = ?1 AND owner = ?2", -1, &st, NULL);
        if (rc == SQLITE_OK) {
            sqlite3_bind_int64(st, 1, op.row.key);
            sqlite3_bind_int(st, 2, c->id);
            if (op.kind != 'D') {
                sqlite3_bind_int64(st, 3, op.row.ver);
                sqlite3_bind_blob(st, 4, body, (int)op.row.len, SQLITE_TRANSIENT);
            }
            rc = sqlite3_step(st);
            if (rc == SQLITE_DONE) rc = SQLITE_OK;
        }
        if (st) sqlite3_finalize(st);
        if (rc != SQLITE_OK) {
            note_error(c->db, rc, "write statement");
            sqlite3_exec(c->db, "ROLLBACK", NULL, NULL, NULL);
            return;
        }
        if (op.kind == 'D') op.row.len = 0, op.row.hash = 0;
        apply_op(&local, &op);
        t.ops[t.count++] = op;
    }
    if (savepoint_at >= 0) {
        if (savepoint == 1) {
            if (exec(c->db, "ROLLBACK TO sp", "rollback to savepoint") != SQLITE_OK) {
                sqlite3_exec(c->db, "ROLLBACK", NULL, NULL, NULL);
                return;
            }
            local = saved;
            t.count = saved_count;
        }
        if (exec(c->db, "RELEASE sp", "release savepoint") != SQLITE_OK) {
            sqlite3_exec(c->db, "ROLLBACK", NULL, NULL, NULL);
            return;
        }
    }
    if (gen_below(&c->rng, 8) == 0) {
        exec(c->db, "ROLLBACK", "rollback");
        return;
    }
    if (t.count == 0) {
        if (sqlite3_exec(c->db, "COMMIT", NULL, NULL, NULL) != SQLITE_OK && !sqlite3_get_autocommit(c->db))
            sqlite3_exec(c->db, "ROLLBACK", NULL, NULL, NULL);
        return;
    }
    journal_pending(c, &t);
    c->pending = t;
    c->has_pending = 1;
    rc = sqlite3_exec(c->db, "COMMIT", NULL, NULL, NULL);
    if (rc == SQLITE_OK) {
        journal_outcome(c, 'A', t.id);
        c->model = local;
        c->has_pending = 0;
        return;
    }
    note_error(c->db, rc, "commit");
    if (!sqlite3_get_autocommit(c->db)) {
        sqlite3_exec(c->db, "ROLLBACK", NULL, NULL, NULL);
        journal_outcome(c, 'F', t.id);
        c->has_pending = 0;
    }
}

static void op_read(struct client *c) {
    static struct table first, second;
    int ok;
    long long total;
    sqlite3_stmt *st = NULL;
    if (exec(c->db, "BEGIN DEFERRED", "begin read") != SQLITE_OK) return;
    if (read_owned(c, &first) != SQLITE_OK) goto out;
    if (sqlite3_prepare_v2(c->db, "SELECT count(*) FROM kv", -1, &st, NULL) == SQLITE_OK) {
        int rc = sqlite3_step(st);
        total = rc == SQLITE_ROW ? sqlite3_column_int64(st, 0) : -1;
        (void)total;
        if (rc != SQLITE_ROW) note_error(c->db, rc, "count");
        sqlite3_finalize(st);
    }
    gen_sleep_ms(gen_think_ms(&c->rng));
    if (read_owned(c, &second) != SQLITE_OK) goto out;
    ok = same_table(&first, &second);
    gen_always(A_SNAPSHOT, ok, "the owned rows changed inside one read transaction");
    compare_owned(c, &first);
out:
    sqlite3_exec(c->db, "COMMIT", NULL, NULL, NULL);
    if (!sqlite3_get_autocommit(c->db)) sqlite3_exec(c->db, "ROLLBACK", NULL, NULL, NULL);
}

static void op_checkpoint(struct client *c) {
    static const char *modes[] = {"PASSIVE", "FULL", "RESTART", "TRUNCATE"};
    char sql[64];
    snprintf(sql, sizeof sql, "PRAGMA wal_checkpoint(%s)", modes[gen_pick(&c->rng, SITE_CHECKPOINT, 4)]);
    exec(c->db, sql, "checkpoint");
}

static void op_reopen(struct client *c) {
    static const int busy[] = {0, 100, 1000};
    static const char *sync[] = {"NORMAL", "FULL"};
    static const int autockpt[] = {0, 100, 1000};
    static const int cache[] = {-2000, 100, 10};
    unsigned form = gen_pick(&c->rng, SITE_REOPEN, 54);
    int b = busy[form % 3];
    const char *s = sync[form / 3 % 2];
    int a = autockpt[form / 6 % 3];
    int k = cache[form / 18];
    sqlite3_close_v2(c->db);
    c->db = NULL;
    open_db(c);
    apply_settings(c, b, s, a, k);
}

static void op_integrity(struct client *c) {
    sqlite3_stmt *st = NULL;
    int rc = sqlite3_prepare_v2(c->db, "PRAGMA integrity_check", -1, &st, NULL);
    if (rc != SQLITE_OK) {
        note_error(c->db, rc, "prepare integrity_check");
        return;
    }
    rc = sqlite3_step(st);
    if (rc == SQLITE_ROW) {
        const unsigned char *text = sqlite3_column_text(st, 0);
        int clean = text && strcmp((const char *)text, "ok") == 0;
        gen_reached(R_INTEGRITY);
        gen_always(A_INTEGRITY, clean, text ? (const char *)text : "no text");
    } else {
        note_error(c->db, rc, "integrity_check");
    }
    sqlite3_finalize(st);
}

static void recovery_check(struct client *c) {
    static struct table seen;
    for (int attempt = 0; attempt < 20; attempt++) {
        int rc;
        if (exec(c->db, "BEGIN DEFERRED", "begin recovery read") != SQLITE_OK) {
            gen_sleep_ms(50);
            continue;
        }
        rc = read_owned(c, &seen);
        if (rc == SQLITE_OK) compare_owned(c, &seen);
        sqlite3_exec(c->db, "COMMIT", NULL, NULL, NULL);
        if (rc == SQLITE_OK) return;
        gen_sleep_ms(50);
    }
}

static int do_init(const char *path) {
    sqlite3 *db = NULL;
    if (sqlite3_open(path, &db) != SQLITE_OK) return 1;
    if (exec(db, "PRAGMA journal_mode=WAL", "init") != SQLITE_OK ||
        exec(db,
             "CREATE TABLE IF NOT EXISTS kv(k INTEGER PRIMARY KEY, owner INTEGER NOT NULL, "
             "ver INTEGER NOT NULL, body BLOB NOT NULL);"
             "CREATE INDEX IF NOT EXISTS kv_owner ON kv(owner, ver);",
             "init") != SQLITE_OK)
        return 1;
    sqlite3_close(db);
    return 0;
}

int main(int argc, char **argv) {
    static struct client c;
    antithesis_load_libvoidstar();
    if (argc == 3 && strcmp(argv[1], "init") == 0) return do_init(argv[2]);
    if (argc != 4 || (strcmp(argv[1], "client") != 0 && strcmp(argv[1], "verify") != 0)) {
        fprintf(stderr, "usage: sqlite-general init DB | client ID DB | verify ID DB\n");
        return 2;
    }
    gen_declare_always(A_INTEGRITY);
    gen_declare_always(A_CORRUPT);
    gen_declare_always(A_COMMITS);
    gen_declare_always(A_SNAPSHOT);
    gen_declare_reachable(R_INTEGRITY);
    gen_declare_reachable(R_COMPARED);
    c.id = atoi(argv[2]);
    c.path = argv[3];
    {
        const char *slash = strrchr(c.path, '/');
        int dir = slash ? (int)(slash - c.path) : 1;
        snprintf(c.journal_path, sizeof c.journal_path, "%.*s/journal-%d", dir, slash ? c.path : ".",
                 c.id);
    }
    gen_rng_init(&c.rng);
    load_journal(&c);
    c.journal = open(c.journal_path, O_WRONLY | O_APPEND | O_CREAT | O_CLOEXEC, 0644);
    if (c.journal < 0) return 1;
    open_db(&c);
    apply_settings(&c, 1000, "FULL", 1000, -2000);
    recovery_check(&c);
    if (strcmp(argv[1], "verify") == 0) {
        op_integrity(&c);
        sqlite3_close_v2(c.db);
        return 0;
    }
    for (;;) {
        if (c.has_pending) {
            recovery_check(&c);
            if (c.has_pending) {
                gen_sleep_ms(gen_think_ms(&c.rng));
                continue;
            }
        }
        gen_rng_sync(&c.rng);
        switch (gen_pick(&c.rng, SITE_OP, 5)) {
            case 0: op_write(&c); break;
            case 1: op_read(&c); break;
            case 2: op_checkpoint(&c); break;
            case 3: op_reopen(&c); break;
            default: op_integrity(&c); break;
        }
        gen_think_at(&c.rng, SITE_THINK);
    }
}
