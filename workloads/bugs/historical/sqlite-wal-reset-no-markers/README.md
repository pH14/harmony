# SQLite 3.51.2 WAL reset — ablation A1: no in-source markers

The [sqlite-wal-reset](../sqlite-wal-reset/README.md) case with one change: its
image is built with `SQLITE_ANTITHESIS=0`, so the fork's thirteen `ANT_REACH`
markers in `src/wal.c` compile to nothing. The harness, its assertions, the
oracle and every other build input are the focused case's. The ablation tests
whether the focused case's discovery depends on those markers acting as search
goals; see [DISCOVERY.md](../DISCOVERY.md#hypotheses), hypothesis H3.
