# Python

The recipe is `workloads/languages/python/Dockerfile`. It pins CPython 3.14.5, the Antithesis Python SDK, and a source-built MarkupSafe extension. `workloads/languages/python/postgres.Dockerfile` is the reference service: the PostgreSQL 14.3 case with a Python driver and the original index oracle.

## Interpreter

Build the standard interpreter with the GIL and its native extensions with Clang `-fsanitize-coverage=trace-pc-guard`. Link the forwarding header from a separate object compiled without coverage. Instrumenting the interpreter loop gives every Python loop a callback.

Turn off the experimental JIT and `PY_HAVE_PERF_TRAMPOLINE`. Apply `disable-codegen.patch`. It rejects ctypes callbacks and removes the libffi closure that ctypes creates at import, which leaves anonymous executable memory. Ordinary `ctypes.CDLL` calls still work. A service that needs ctypes callbacks needs a separately reviewed build.

## Extensions and bytecode

Build third-party native extensions from pinned source in the `compiled` stage:

```sh
pip install --no-binary :all: --no-deps --no-build-isolation PACKAGE
```

Some packages fall back to pure Python without telling you. Check that the native module is installed. The target `LDSHARED` already links the forwarding object. Rescan every packaged ELF file after adding a native dependency. The minimal recipe omits the SSL, SQLite, bz2 and lzma modules.

Compile installed bytecode at build time with hash-based invalidation, and keep runtime cache writes off. Otherwise the instrumented compiler runs at startup and uses up the driver's hook budget before the workload starts.

## Symbols

Run `coverage_edges.py` on copies of the application sources. It writes the AST span catalog and module marker that the SDK's coverage runtime reads. Keep unstripped native copies and their symbol tables under `/symbols`. Attest each installed native file by SHA-256 and path.

## Acceptance

`ctypes.CDLL` calls release the GIL. Native callbacks keep it. The park check holds one Python thread at a callback and requires another Python thread to print a new marker. The fixture checks `/proc/self/maps` before and after the run for anonymous or writable executable memory, apart from vDSO and vsyscall.

```sh
bash workloads/languages/build-image.sh python
bash workloads/languages/run-check.sh harmony-language-python:local evidence/python
```

The PostgreSQL driver run must reach its oracle in replays. A connection failure does not count as an oracle comparison.
