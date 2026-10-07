<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Python language recipe

The recipe builds CPython from source with the GIL, the Antithesis Python SDK,
and one third-party native extension (MarkupSafe). All source archives are
pinned by SHA-256 in the `Dockerfile`.

## Native coverage

The interpreter and its standard-library extension modules are built with Clang
`-fsanitize-coverage=trace-pc-guard`. The Antithesis C forwarding header is
compiled once without coverage and linked into each native file. Every Python
loop runs through the instrumented interpreter loop, so it reaches libvoidstar
even without Python-level symbols.

Third-party extensions are built from source in the `compiled` stage with the
same flags:

```sh
pip install --no-binary :all: --no-deps --no-build-isolation PACKAGE
```

Prebuilt wheels are uninstrumented. Some packages fall back to pure Python when
the native build fails, so the recipe imports the native module to confirm it
was built. Target `sysconfig` already links the forwarding object through
`LDSHARED`; do not add it again through `LDFLAGS`.

The image keeps only libffi and zlib. Modules that need other development
libraries (`_ssl`, SQLite, bz2, lzma) are omitted. Add a dependency only when a
workload needs it, then rerun admission.

## No runtime code generation

- The experimental JIT is disabled at configure time.
- `PY_HAVE_PERF_TRAMPOLINE` is removed from `pyconfig.h` before compiling.
- `disable-codegen.patch` makes `ctypes` callbacks raise an error and removes
  the libffi closure warm-up on import. The warm-up leaves a writable executable
  page mapped even when no callback is used. `ctypes.CDLL` calls still work,
  which the SDK needs.
- The image sets `PYTHON_JIT=0` and `PYTHONPERFSUPPORT=0`.

An application that needs `ctypes` callbacks needs a libffi built with static
trampolines.

Library bytecode is compiled at build time with checked-hash invalidation, and
`PYTHONDONTWRITEBYTECODE=1` stops runtime cache writes. Without precompiled
bytecode, every import compiles source under native coverage, which costs a
large amount of virtual time at startup.

## Python-level coverage

The SDK's `antithesis/_internal/coverage.py` is installed unchanged. It uses
`sys.monitoring`. `coverage_edges.py` writes the edge table the runtime reads:

```sh
/opt/python/bin/python3.14 coverage_edges.py SOURCE_ROOT MODULE /symbols/python.sym.tsv
```

The table has the nine columns the runtime reads (file, class, function,
edge_kind, address, begin_line, begin_column, end_line, end_column) and ends
with the `# antithesis-module:` marker. Rows cover module entries, function
entries and AST branch spans. Addresses are one-based row numbers. The table
does not hold a full bytecode control-flow graph, so some monitoring events have
no Python site; the native interpreter still covers those loops.
`test_coverage_edges.py` runs the SDK's own resolver against generated tables.

## Parks and the GIL

A park at a Python-level site goes through `ctypes.CDLL`, which releases the
GIL. Other Python threads keep running. A park inside CPython's C code keeps the
GIL, so the process's other Python threads wait. Both match stops that happen in
production.

The fixture prints the address range of its spinner's two branch sites. The
park launcher arms that range, holds the spinner there, and requires a new
timer marker while the hold is active. The fixture checks `/proc/self/maps`
before and after the run for writable executable or anonymous executable
memory.

## PostgreSQL driver reference

`postgres.Dockerfile` builds PostgreSQL 14.3 with the same native coverage and
reuses the seed SQL, settings and scripts of the
[historical CIC case](../../bugs/historical/postgres-cic-corruption/README.md).
`postgres_driver.py` replaces that case's four shell hooks with instrumented
Python and keeps its `pg_amcheck --heapallindexed` oracle. A missing heap or
index tuple is a violation. An unavailable server, a failed connection or an
invalid index is inconclusive. `test_postgres_driver.py` checks these cases.

```sh
docker build -f workloads/languages/python/Dockerfile --target compiled \
  -t harmony-python-reference-build:local .
docker build -f workloads/languages/python/postgres.Dockerfile \
  -t harmony-python-postgres:local .
harmony search harmony-python-postgres:local --backend kvm \
  --config harmony.toml \
  --executions 64 --for 5m --out evidence/python-postgres
harmony debug run harmony-python-postgres:local --backend kvm \
  --config harmony.toml \
  --actions workloads/languages/python/reference-events.json --repeat 2 \
  --out evidence/python-postgres-replay
```

`reference-events.json` holds parks long enough for the idle postmaster to reach
its housekeeping callbacks. The replay must report park fires and the oracle
reached on both repeats.
