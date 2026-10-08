# SQLite 3.51.2 — general-discovery workload

The general-discovery arm for [sqlite-wal-reset](../sqlite-wal-reset/README.md):
the same pinned fork commit and compile flags, with the fork's thirteen
in-source WAL markers compiled out and the fork's harness replaced by a
workload derived from SQLite's documented operations. The frozen specification
is in [DISCOVERY.md](../DISCOVERY.md#sqlite-general-sqlite-wal-general), and the
choice and pacing contract in [general](../general/README.md).

## The image

`image/Dockerfile` builds the amalgamation without `SQLITE_ENABLE_ANTITHESIS`,
checks that no marker string reached the binary, and links
`image/sqlite-general.c` with trace-pc-guard coverage. The build then runs
`image/oracle-test.sh` against the driver and the fork's `sqlite3` shell.

| bundle line | role |
|---|---|
| `setup` | `sqlite-general init /data/test.db`: an empty WAL database with `kv(k, owner, ver, body)` and an index on `(owner, ver)` |
| `node client-0` … `client-2` | three clients; each owns its rows and journals its commits in `/data/journal-<id>` |
| `ready` | `/bin/true` |

`sqlite-general verify ID DB` runs one client's recovery comparison and an
integrity check, then exits; the self-test uses it between steps.

## Oracle

The scored assertion is `sqlite integrity_check returns ok`, with
`sqlite integrity_check completed` as its evidence. The other properties are
`sqlite reports no corruption`, `sqlite preserves acknowledged commits` and
`sqlite read transaction sees one snapshot`; a confirmed violation of any of
them is reported but is not scored as this case's discovery.

## Running

Build from the repository root with the shared runtime, as for the focused
case:

```sh
runtime=$(bash workloads/languages/build-runtime.sh)
docker build --build-arg "HARMONY_RUNTIME_IMAGE=$runtime" \
  -f workloads/bugs/historical/sqlite-wal-general/image/Dockerfile -t harmony-sqlite-general:3.51.2 .
```

`Benchmarks / Harmony Workloads / Historical Discovery` runs it beside the
focused case at one budget.
