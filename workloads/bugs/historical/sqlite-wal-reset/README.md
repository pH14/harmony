# SQLite 3.51.2 — WAL reset corrupts the database

The case runs the fork's own harness unmodified.

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

Any Always violation is a finding. The manifest names
`integrity-check-clean: PRAGMA quick_check returns ok`. A checkpoint that
resumes after another writer reset the WAL copies stale frames into the
database file, and the damaged pages fail the checks each writer's sweep runs:
`PRAGMA quick_check` on three of four sweeps and `PRAGMA integrity_check` on
the rest. The rows a writer committed can still be present, so
`no-lost-committed-writes` can hold after the corruption. The
`workload: process started` Reachable is the evidence that a writer ran.

## Running

The image builds natively for x86-64 and arm64. On x86-64 it installs the
reviewed Bookworm allowlist, and `harmony prepare` admits it as a complete
instrumented target: the base image's RDRAND and RDSEED sites in libgcrypt and
libstdc++ are reviewed, and the case's own binary has none. The case is
runnable on the hosted x86-64 workflows. Build it from the repository root:

```sh
docker build --build-arg "HARMONY_RUNTIME_IMAGE=$runtime" \
  -f workloads/bugs/historical/sqlite-wal-reset/image/Dockerfile -t harmony-sqlite:3.51.2 .
```

`SQLITE_ANTITHESIS=0` builds the same harness with the fork's in-source WAL
markers compiled out; the
[sqlite-wal-reset-no-markers](../sqlite-wal-reset-no-markers/README.md)
ablation uses it, and the default of 1 is this case.

## Shared instrumentation runtime

The image copies libvoidstar and the fault runtime from the shared runtime
build. Build it first and pass its tag to the case's Docker build:

```sh
runtime=$(bash workloads/languages/build-runtime.sh)
docker build --build-arg "HARMONY_RUNTIME_IMAGE=$runtime" ...
```

The historical-image workflow passes this argument itself. Run
`harmony prepare IMAGE` on a newly built image before searching it.

The image uses the pinned Bookworm base that the language images use, so it
shares their reviewed entropy sites. Trixie's coreutils pulls in OpenSSL, which
adds RDRAND sites that would need their own review.
