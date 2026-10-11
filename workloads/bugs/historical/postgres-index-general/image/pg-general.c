/* SPDX-License-Identifier: AGPL-3.0-or-later */
/* pg-general: the general-discovery PostgreSQL workload. See ../README.md and
 * ../../DISCOVERY.md for the frozen specification this file implements.
 *
 *   pg-general            fork one client per owner and keep them running
 *   pg-general client N   run the client for owner N
 *   pg-general amcheck    check every valid B-tree index once and exit
 *
 * Each client owns the rows with owner = N and keeps their acknowledged
 * history in memory. A client that restarts takes the table as its new
 * baseline and says so on stderr. */
#include <signal.h>
#include <sys/wait.h>

#include "harmony_general.h"
#include "libpq-fe.h"

#define OWNERS 4
#define MAX_ROWS 8192
#define MAX_STATEMENTS 8
#define VALUE_RANGE 100000
#define CONNINFO "host=/tmp user=postgres dbname=faultlab connect_timeout=10"
#define BULK_SIZES 8

enum site { SITE_OP, SITE_STATEMENTS, SITE_THINK, SITE_KIND, SITE_COLUMN, SITE_ISOLATION, SITE_DDL, SITE_BULK };

static const char *A_AMCHECK = "postgres amcheck finds every heap tuple indexed";
static const char *A_SCANS = "postgres index and sequential scans agree";
static const char *A_COMMITS = "postgres preserves acknowledged commits";
static const char *R_AMCHECK = "postgres amcheck verified an index";
static const char *R_COMPARED = "postgres general compared committed rows";

struct row {
    int64_t id;
    int32_t a;
    int32_t b;
    char c[17];
};

struct table {
    int count;
    struct row rows[MAX_ROWS];
};

struct write_op {
    char kind;
    struct row row;
};

struct client {
    int owner;
    PGconn *conn;
    struct gen_rng rng;
    struct table model;
    struct table pending;
    int has_pending;
    int64_t next_seq;
};

static int find_row(const struct table *t, int64_t id) {
    for (int i = 0; i < t->count; i++)
        if (t->rows[i].id == id) return i;
    return -1;
}

static void apply_op(struct table *t, const struct write_op *op) {
    int at = find_row(t, op->row.id);
    if (op->kind == 'D') {
        if (at >= 0) t->rows[at] = t->rows[--t->count];
    } else if (at >= 0) {
        t->rows[at] = op->row;
    } else if (t->count < MAX_ROWS) {
        t->rows[t->count++] = op->row;
    }
}

static int row_order(const void *x, const void *y) {
    const struct row *a = x, *b = y;
    return (a->id > b->id) - (a->id < b->id);
}

static int same_table(struct table *x, struct table *y) {
    if (x->count != y->count) return 0;
    qsort(x->rows, (size_t)x->count, sizeof x->rows[0], row_order);
    qsort(y->rows, (size_t)y->count, sizeof y->rows[0], row_order);
    for (int i = 0; i < x->count; i++) {
        const struct row *a = &x->rows[i], *b = &y->rows[i];
        if (a->id != b->id || a->a != b->a || a->b != b->b || strcmp(a->c, b->c) != 0) return 0;
    }
    return 1;
}

static int connected(struct client *c) { return c->conn && PQstatus(c->conn) == CONNECTION_OK; }

static PGresult *run(struct client *c, const char *sql) { return PQexec(c->conn, sql); }

static int command_ok(struct client *c, const char *sql) {
    PGresult *r = run(c, sql);
    int ok = PQresultStatus(r) == PGRES_COMMAND_OK || PQresultStatus(r) == PGRES_TUPLES_OK;
    PQclear(r);
    return ok;
}

static const char *sqlstate(const PGresult *r) {
    const char *state = PQresultErrorField(r, PG_DIAG_SQLSTATE);
    return state ? state : "";
}

static int read_owned(struct client *c, struct table *out) {
    char owner[16];
    const char *params[1] = {owner};
    PGresult *r;
    snprintf(owner, sizeof owner, "%d", c->owner);
    r = PQexecParams(c->conn, "SELECT id, a, b, c FROM items WHERE owner = $1 ORDER BY id", 1, NULL, params,
                     NULL, NULL, 0);
    if (PQresultStatus(r) != PGRES_TUPLES_OK) {
        PQclear(r);
        return 0;
    }
    out->count = 0;
    for (int i = 0; i < PQntuples(r) && out->count < MAX_ROWS; i++) {
        struct row *row = &out->rows[out->count++];
        row->id = atoll(PQgetvalue(r, i, 0));
        row->a = atoi(PQgetvalue(r, i, 1));
        row->b = atoi(PQgetvalue(r, i, 2));
        snprintf(row->c, sizeof row->c, "%s", PQgetvalue(r, i, 3));
    }
    PQclear(r);
    return 1;
}

static void compare_owned(struct client *c, struct table *seen) {
    static struct table expected, applied;
    char detail[160];
    expected = c->model;
    if (same_table(seen, &expected)) {
        c->has_pending = 0;
        gen_reached(R_COMPARED);
        gen_always(A_COMMITS, 1, NULL);
        return;
    }
    if (c->has_pending) {
        applied = c->pending;
        if (same_table(seen, &applied)) {
            c->model = applied;
            c->has_pending = 0;
            gen_reached(R_COMPARED);
            gen_always(A_COMMITS, 1, NULL);
            return;
        }
    }
    snprintf(detail, sizeof detail, "owner %d saw %d rows, history has %d", c->owner, seen->count,
             c->model.count);
    gen_reached(R_COMPARED);
    gen_always(A_COMMITS, 0, detail);
}

static void reconnect(struct client *c) {
    if (c->conn) PQfinish(c->conn);
    c->conn = PQconnectdb(CONNINFO);
}

/* Reads the owned rows in their own transaction and compares them, which
 * resolves an indeterminate commit. Returns 1 when the read was conclusive. */
static int resolve(struct client *c) {
    static struct table seen;
    int ok;
    if (!connected(c)) return 0;
    if (!command_ok(c, "BEGIN ISOLATION LEVEL REPEATABLE READ")) return 0;
    ok = read_owned(c, &seen);
    command_ok(c, "COMMIT");
    if (ok) compare_owned(c, &seen);
    return ok;
}

static void random_text(struct client *c, char *out) {
    static const char hex[] = "0123456789abcdef";
    int length = 1 + (int)gen_below(&c->rng, 16);
    for (int i = 0; i < length; i++) out[i] = hex[gen_below(&c->rng, 16)];
    out[length] = 0;
}

static int exec_write(struct client *c, const struct write_op *op, int column) {
    char id[32], owner[16], a[16], b[16];
    const char *params[6] = {id, owner, a, b, op->row.c, NULL};
    PGresult *r;
    int ok;
    snprintf(id, sizeof id, "%lld", (long long)op->row.id);
    snprintf(owner, sizeof owner, "%d", c->owner);
    snprintf(a, sizeof a, "%d", op->row.a);
    snprintf(b, sizeof b, "%d", op->row.b);
    if (op->kind == 'I') {
        r = PQexecParams(c->conn, "INSERT INTO items(id, owner, a, b, c) VALUES($1, $2, $3, $4, $5)", 5, NULL,
                         params, NULL, NULL, 0);
    } else if (op->kind == 'D') {
        r = PQexecParams(c->conn, "DELETE FROM items WHERE id = $1 AND owner = $2", 2, NULL, params, NULL,
                         NULL, 0);
    } else {
        static const char *updates[] = {
            "UPDATE items SET a = $3 WHERE id = $1 AND owner = $2",
            "UPDATE items SET b = $3 WHERE id = $1 AND owner = $2",
            "UPDATE items SET c = $3 WHERE id = $1 AND owner = $2",
        };
        const char *value[3] = {id, owner, column == 0 ? a : column == 1 ? b : op->row.c};
        r = PQexecParams(c->conn, updates[column], 3, NULL, value, NULL, NULL, 0);
    }
    ok = PQresultStatus(r) == PGRES_COMMAND_OK;
    PQclear(r);
    return ok;
}

/* Commits the open transaction whose result is `local`. A commit whose outcome
 * is unknown leaves `local` pending until a later read resolves it. */
static void commit(struct client *c, const struct table *local) {
    PGresult *r;
    c->pending = *local;
    c->has_pending = 1;
    r = run(c, "COMMIT");
    if (PQresultStatus(r) == PGRES_COMMAND_OK) {
        c->model = *local;
        c->has_pending = 0;
    } else if (connected(c) && (strncmp(sqlstate(r), "40", 2) == 0 || strncmp(sqlstate(r), "23", 2) == 0 ||
                                strncmp(sqlstate(r), "25", 2) == 0)) {
        c->has_pending = 0;
    }
    PQclear(r);
}

static void op_write(struct client *c) {
    static const char *levels[] = {"READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"};
    static struct table local;
    char sql[64];
    int statements = 1 + (int)gen_pick(&c->rng, SITE_STATEMENTS, MAX_STATEMENTS);
    local = c->model;
    snprintf(sql, sizeof sql, "BEGIN ISOLATION LEVEL %s", levels[gen_pick(&c->rng, SITE_ISOLATION, 3)]);
    if (!command_ok(c, sql)) return;
    for (int i = 0; i < statements; i++) {
        struct write_op op;
        int kind = (int)gen_pick(&c->rng, SITE_KIND, 3);
        int column = (int)gen_pick(&c->rng, SITE_COLUMN, 3);
        if (local.count == 0) kind = 0;
        if (local.count >= MAX_ROWS - MAX_STATEMENTS) kind = 2;
        if (kind == 0) {
            op.kind = 'I';
            op.row.id = (int64_t)(c->owner + 1) * 1000000000LL + c->next_seq++;
            op.row.a = (int32_t)gen_below(&c->rng, VALUE_RANGE);
            op.row.b = (int32_t)gen_below(&c->rng, VALUE_RANGE);
            random_text(c, op.row.c);
        } else {
            op.kind = kind == 1 ? 'U' : 'D';
            op.row = local.rows[gen_below(&c->rng, (uint64_t)local.count)];
            if (column == 0) op.row.a = (int32_t)gen_below(&c->rng, VALUE_RANGE);
            if (column == 1) op.row.b = (int32_t)gen_below(&c->rng, VALUE_RANGE);
            if (column == 2) random_text(c, op.row.c);
        }
        if (!exec_write(c, &op, column)) {
            command_ok(c, "ROLLBACK");
            return;
        }
        apply_op(&local, &op);
    }
    if (gen_below(&c->rng, 8) == 0) {
        command_ok(c, "ROLLBACK");
        return;
    }
    commit(c, &local);
}

/* Inserts 16 to 2048 new owned rows in one statement, or, when the owner's
 * rows would no longer fit the model, deletes that many of its oldest rows.
 * The values follow from a seed and the row's position, so the model computes
 * the same rows the server does. */
static void op_bulk(struct client *c) {
    static struct table local;
    char first[32], owner[16], seed[24], count[16], sql[64];
    const char *params[4] = {first, owner, seed, count};
    int rows = 16 << gen_pick(&c->rng, SITE_BULK, BULK_SIZES);
    PGresult *r;
    int ok;
    local = c->model;
    snprintf(sql, sizeof sql, "BEGIN ISOLATION LEVEL %s",
             gen_below(&c->rng, 2) ? "READ COMMITTED" : "REPEATABLE READ");
    if (!command_ok(c, sql)) return;
    snprintf(owner, sizeof owner, "%d", c->owner);
    if (local.count + rows <= MAX_ROWS - MAX_STATEMENTS) {
        int64_t base = (int64_t)(c->owner + 1) * 1000000000LL + c->next_seq;
        uint32_t s = (uint32_t)gen_next(&c->rng);
        snprintf(first, sizeof first, "%lld", (long long)base);
        snprintf(seed, sizeof seed, "%u", s);
        snprintf(count, sizeof count, "%d", rows);
        r = PQexecParams(c->conn,
                         "INSERT INTO items(id, owner, a, b, c) SELECT $1::bigint + g, $2::int, "
                         "(($3::bigint + g * 7919) % 100000)::int, (($3::bigint * 31 + g * 104729) % 100000)::int, "
                         "to_hex($3::bigint + g) FROM generate_series(0, $4::int - 1) AS g",
                         4, NULL, params, NULL, NULL, 0);
        ok = PQresultStatus(r) == PGRES_COMMAND_OK;
        PQclear(r);
        for (int g = 0; ok && g < rows; g++) {
            struct write_op op;
            op.kind = 'I';
            op.row.id = base + g;
            op.row.a = (int32_t)(((int64_t)s + (int64_t)g * 7919) % VALUE_RANGE);
            op.row.b = (int32_t)(((int64_t)s * 31 + (int64_t)g * 104729) % VALUE_RANGE);
            snprintf(op.row.c, sizeof op.row.c, "%llx", (unsigned long long)((int64_t)s + g));
            apply_op(&local, &op);
        }
        if (ok) c->next_seq += rows;
    } else {
        int64_t threshold;
        qsort(local.rows, (size_t)local.count, sizeof local.rows[0], row_order);
        if (rows > local.count) rows = local.count;
        if (rows == 0) {
            command_ok(c, "ROLLBACK");
            return;
        }
        threshold = local.rows[rows - 1].id;
        snprintf(first, sizeof first, "%lld", (long long)threshold);
        params[0] = owner;
        params[1] = first;
        r = PQexecParams(c->conn, "DELETE FROM items WHERE owner = $1::int AND id <= $2::bigint", 2, NULL, params,
                         NULL, NULL, 0);
        ok = PQresultStatus(r) == PGRES_COMMAND_OK;
        PQclear(r);
        if (ok) {
            memmove(local.rows, local.rows + rows, sizeof local.rows[0] * (size_t)(local.count - rows));
            local.count -= rows;
        }
    }
    if (!ok) {
        command_ok(c, "ROLLBACK");
        return;
    }
    commit(c, &local);
}

static int read_ids(struct client *c, const char *sql, const char *lo, const char *hi, int64_t *ids, int *count) {
    const char *params[2] = {lo, hi};
    PGresult *r = PQexecParams(c->conn, sql, 2, NULL, params, NULL, NULL, 0);
    int ok = PQresultStatus(r) == PGRES_TUPLES_OK;
    *count = 0;
    if (ok)
        for (int i = 0; i < PQntuples(r) && i < MAX_ROWS * OWNERS; i++) ids[(*count)++] = atoll(PQgetvalue(r, i, 0));
    PQclear(r);
    return ok;
}

static void op_read(struct client *c) {
    static struct table seen;
    static int64_t by_index[MAX_ROWS * OWNERS], by_heap[MAX_ROWS * OWNERS];
    static const char *queries[] = {
        "SELECT id FROM items WHERE a BETWEEN $1::int AND $2::int ORDER BY id",
        "SELECT id FROM items WHERE b BETWEEN $1::int AND $2::int ORDER BY id",
        "SELECT id FROM items WHERE c BETWEEN $1 AND $2 ORDER BY id",
    };
    char lo[24], hi[24];
    int column = (int)gen_pick(&c->rng, SITE_COLUMN, 3);
    int index_count = 0, heap_count = 0, ok;
    if (!command_ok(c, "BEGIN ISOLATION LEVEL REPEATABLE READ")) return;
    if (!read_owned(c, &seen)) goto out;
    compare_owned(c, &seen);
    if (column < 2) {
        int start = (int)gen_below(&c->rng, VALUE_RANGE);
        snprintf(lo, sizeof lo, "%d", start);
        snprintf(hi, sizeof hi, "%d", start + (int)gen_below(&c->rng, 5000));
    } else {
        random_text(c, lo);
        random_text(c, hi);
        if (strcmp(lo, hi) > 0) {
            char swap[24];
            strcpy(swap, lo);
            strcpy(lo, hi);
            strcpy(hi, swap);
        }
    }
    if (!command_ok(c, "SET LOCAL enable_seqscan = off") || !command_ok(c, "SET LOCAL enable_bitmapscan = on") ||
        !command_ok(c, "SET LOCAL enable_indexscan = on"))
        goto out;
    if (!read_ids(c, queries[column], lo, hi, by_index, &index_count)) goto out;
    if (!command_ok(c, "SET LOCAL enable_seqscan = on") || !command_ok(c, "SET LOCAL enable_bitmapscan = off") ||
        !command_ok(c, "SET LOCAL enable_indexscan = off") || !command_ok(c, "SET LOCAL enable_indexonlyscan = off"))
        goto out;
    if (!read_ids(c, queries[column], lo, hi, by_heap, &heap_count)) goto out;
    ok = index_count == heap_count && memcmp(by_index, by_heap, sizeof by_index[0] * (size_t)index_count) == 0;
    {
        char detail[128];
        snprintf(detail, sizeof detail, "column %d range: index path %d rows, heap path %d rows", column,
                 index_count, heap_count);
        gen_always(A_SCANS, ok, detail);
    }
out:
    command_ok(c, "COMMIT");
}

static void op_ddl(struct client *c) {
    static const char *names[] = {"items_a_idx", "items_b_idx", "items_c_idx", "items_ab_idx"};
    static const char *columns[] = {"a", "b", "c", "a, b"};
    int form = (int)gen_pick(&c->rng, SITE_DDL, 24);
    int which = form % 4;
    int kind = form / 4 % 3;
    const char *concurrently = form / 12 ? " CONCURRENTLY" : "";
    char sql[160];
    if (kind == 0)
        snprintf(sql, sizeof sql, "CREATE INDEX%s IF NOT EXISTS %s ON items(%s)", concurrently, names[which],
                 columns[which]);
    else if (kind == 1)
        snprintf(sql, sizeof sql, "DROP INDEX%s IF EXISTS %s", concurrently, names[which]);
    else
        snprintf(sql, sizeof sql, "REINDEX INDEX%s %s", concurrently, names[which]);
    command_ok(c, sql);
}

static void op_maintenance(struct client *c) {
    static const char *commands[] = {"VACUUM items", "VACUUM (FREEZE) items", "ANALYZE items", "CHECKPOINT"};
    command_ok(c, commands[gen_below(&c->rng, 4)]);
}

static void op_check(struct client *c) {
    PGresult *list = run(c,
                         "SELECT ci.relname FROM pg_index i JOIN pg_class ci ON ci.oid = i.indexrelid "
                         "JOIN pg_am am ON am.oid = ci.relam WHERE i.indrelid = 'items'::regclass "
                         "AND i.indisvalid AND i.indisready AND am.amname = 'btree' ORDER BY ci.relname");
    if (PQresultStatus(list) != PGRES_TUPLES_OK) {
        PQclear(list);
        return;
    }
    for (int i = 0; i < PQntuples(list); i++) {
        const char *params[1] = {PQgetvalue(list, i, 0)};
        PGresult *r = PQexecParams(c->conn, "SELECT bt_index_check($1::regclass, true)", 1, NULL, params, NULL,
                                   NULL, 0);
        if (PQresultStatus(r) == PGRES_TUPLES_OK) {
            gen_reached(R_AMCHECK);
            gen_always(A_AMCHECK, 1, NULL);
        } else if (strcmp(sqlstate(r), "XX001") == 0 || strcmp(sqlstate(r), "XX002") == 0) {
            char detail[256];
            snprintf(detail, sizeof detail, "%s: %s", params[0], PQresultErrorMessage(r));
            gen_reached(R_AMCHECK);
            gen_always(A_AMCHECK, 0, detail);
        }
        PQclear(r);
    }
    PQclear(list);
}

static _Noreturn void run_client(int owner) {
    static struct client c;
    static struct table seen;
    c.owner = owner;
    gen_rng_init(&c.rng);
    while (!connected(&c)) {
        reconnect(&c);
        if (!connected(&c)) gen_sleep_ms(100);
    }
    while (!command_ok(&c, "BEGIN ISOLATION LEVEL REPEATABLE READ") || !read_owned(&c, &seen)) {
        command_ok(&c, "ROLLBACK");
        gen_sleep_ms(100);
        if (!connected(&c)) reconnect(&c);
    }
    command_ok(&c, "COMMIT");
    c.model = seen;
    c.next_seq = 1;
    for (int i = 0; i < seen.count; i++) {
        int64_t base = (int64_t)(owner + 1) * 1000000000LL;
        if (seen.rows[i].id >= base && seen.rows[i].id - base >= c.next_seq) c.next_seq = seen.rows[i].id - base + 1;
    }
    fprintf(stderr, "pg-general: owner %d baselined %d rows\n", owner, seen.count);
    for (;;) {
        if (!connected(&c)) {
            reconnect(&c);
            if (!connected(&c)) {
                gen_sleep_ms(100);
                continue;
            }
            resolve(&c);
        }
        if (c.has_pending) {
            if (!resolve(&c)) {
                gen_sleep_ms(gen_think_ms(&c.rng));
                continue;
            }
        }
        gen_rng_sync(&c.rng);
        switch (gen_pick(&c.rng, SITE_OP, 7)) {
            case 0: op_write(&c); break;
            case 1: op_read(&c); break;
            case 2: op_ddl(&c); break;
            case 3: op_maintenance(&c); break;
            case 4: op_check(&c); break;
            case 5: op_bulk(&c); break;
            default: reconnect(&c); break;
        }
        gen_think_at(&c.rng, SITE_THINK);
    }
}

int main(int argc, char **argv) {
    pid_t children[OWNERS];
    signal(SIGPIPE, SIG_IGN);
    if (argc == 3 && strcmp(argv[1], "client") == 0) run_client(atoi(argv[2]));
    if (argc == 2 && strcmp(argv[1], "amcheck") == 0) {
        static struct client c;
        c.conn = PQconnectdb(CONNINFO);
        if (!connected(&c)) return 1;
        op_check(&c);
        PQfinish(c.conn);
        return 0;
    }
    gen_declare_always(A_AMCHECK);
    gen_declare_always(A_SCANS);
    gen_declare_always(A_COMMITS);
    gen_declare_reachable(R_AMCHECK);
    gen_declare_reachable(R_COMPARED);
    for (int owner = 0; owner < OWNERS; owner++) {
        children[owner] = fork();
        if (children[owner] == 0) run_client(owner);
    }
    for (;;) {
        int status;
        pid_t done = wait(&status);
        if (done < 0) {
            if (errno == EINTR) continue;
            return 1;
        }
        for (int owner = 0; owner < OWNERS; owner++) {
            if (children[owner] != done) continue;
            fprintf(stderr, "pg-general: owner %d client exited with status %d; restarting\n", owner, status);
            gen_sleep_ms(1000);
            children[owner] = fork();
            if (children[owner] == 0) run_client(owner);
        }
    }
}
