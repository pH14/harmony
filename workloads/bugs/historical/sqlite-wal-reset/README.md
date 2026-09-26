# SQLite 3.51.2 — WAL reset loses committed writes

**Status: searching.** The case runs the fork's own harness unmodified; it has
no in-tree record of a find yet.

## Sources

- Harness: the `antithesis/` directory of
  [antithesishq/sqlite](https://github.com/antithesishq/sqlite) at commit
  `3ce53bc469dcef8d8c2d90eb59a7d13184e782e5` (SQLite 3.51.2).
- Fix: SQLite 3.51.3, per its [release notes](https://sqlite.org/releaselog/3_51_3.html).

## The harness

The image builds the fork's amalgamation, `antithesis/workload.c` and the fork's
fallback SDK with the fork's own compile flags and trace-pc-guard coverage into
one `sqlite-workload` executable. The Dockerfile checks the workload source
against `WORKLOAD_SHA256` before it builds. Harmony adds only platform pieces:
`/usr/lib/libvoidstar.so` composed with the faults event runtime, the symbol
table under `/symbols`, and a bundle:

| line | role |
|---|---|
| `setup` | `sqlite-workload init /data/test.db` creates the WAL-mode database and reports setup complete |
| `node writer-0`, `node writer-1` | the two writer processes over one database file |
| `ready` | `/bin/true` |

Each writer mixes write transactions, checkpoints in all four modes, and
correctness sweeps. The fallback SDK writes JSON lines to
`$ANTITHESIS_OUTPUT_DIR/sdk.jsonl`, and the supervisor links that file to
`/dev/harmony`, so every declared assertion reaches the search by name. The
search sees 25 assertions:

| source | assertions |
|---|---|
| `antithesis/workload.c` | seven Always, one Always-or-unreachable (`recovery-preserves-committed`), three Sometimes, and the `workload: process started` Reachable |
| `ANT_REACH` markers in the amalgamation (`SQLITE_ENABLE_ANTITHESIS`) | thirteen Reachable in `walCheckpoint`, `sqlite3WalCheckpoint`, `walTryBeginRead`, `walIndexRecover`, `walRestartHdr` and `walFrames` |
| the supervisor | the node-exit check |

## Oracle

Any Always violation is a finding. The manifest names the loss assertion,
`no-lost-committed-writes`, which each writer checks by comparing its row
count and maximum sequence with its own committed sequence. The
`workload: process started` Reachable is the evidence that a writer ran.

## Running

The image builds for arm64 and the case searches on arm64 hosts, so its CI
status is deferred. Build it from the repository root:

```sh
docker buildx build --platform linux/arm64 \
  -f workloads/bugs/historical/sqlite-wal-reset/image/Dockerfile \
  --output type=oci,dest=sqlite-3.51.2.oci .
```
