set -e
export PGDATA=/var/lib/postgresql/data
mkdir -p "$PGDATA" /run/postgresql
chown -R postgres /var/lib/postgresql /run/postgresql
su-exec postgres initdb --no-instructions >/dev/null 2>&1
su-exec postgres pg_ctl -D "$PGDATA" -w -l /tmp/pg.log \
    -o "-c listen_addresses= -c unix_socket_directories=/run/postgresql" start >/dev/null
psql() { command psql -U postgres -h /run/postgresql -v ON_ERROR_STOP=1 -q "$@"; }
echo "=== postgres $(psql -tAc 'show server_version') under a deterministic hypervisor ==="
psql -c "CREATE TABLE events(
    id bigserial PRIMARY KEY,
    at timestamptz NOT NULL DEFAULT clock_timestamp(),
    r  double precision NOT NULL DEFAULT random(),
    who text NOT NULL)"
for w in alice bob carol dave; do
    psql -c "INSERT INTO events(who)
             SELECT '$w' FROM generate_series(1, 5000)" &
done
wait
echo "--- four concurrent writers, 20000 rows ---"
psql -c "SELECT who, count(*), round(avg(r)::numeric, 15) AS avg_random
         FROM events GROUP BY who ORDER BY who"
echo "--- the server's clock and 'random' draws ---"
psql -c "SELECT now() AS server_now,
                (SELECT string_agg((random()*49+1)::int::text, ' ')
                 FROM generate_series(1,6)) AS lottery"
echo "--- whole-table fingerprint (every timestamp, every random(), every row order) ---"
psql -tAc "SELECT md5(string_agg(id::text||at::text||r::text||who, ',' ORDER BY id))
           FROM events"
su-exec postgres pg_ctl -D "$PGDATA" stop -m fast >/dev/null
