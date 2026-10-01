# Compiled languages

The recipes are `workloads/languages/c/Dockerfile` and `workloads/languages/rust/Dockerfile`. The SQLite case at `workloads/bugs/historical/sqlite-wal-reset/image/Dockerfile` is the reference service.

These recipes produce native code at build time, so no code is generated at runtime. Precompiled libc and the Rust standard library have no callbacks. Their loops are bounded by input size. Link against instrumented builds of those libraries when their loops matter.

## C, C++ and other LLVM front ends

Compile application files with `-O1 -g -fsanitize-coverage=trace-pc-guard`. Include the unmodified `workloads/languages/vendor/antithesis_instrumentation.h` in one separate file compiled without coverage. Link with `-pthread -ldl -Wl,--build-id`. The header loads `/usr/lib/libvoidstar.so` at startup and forwards each callback's return address.

For GCC, compile with `-fsanitize-coverage=trace-pc` and link libvoidstar directly. It exports `__sanitizer_cov_trace_pc`.

## Rust

Depend on `antithesis-instrumentation = "=0.1.0"`, lock the dependency graph, and reference the crate from the executable. With the pinned Rust 1.97.0 toolchain:

```sh
export RUSTFLAGS="-Ccodegen-units=1 -Cpasses=sancov-module -Cllvm-args=-sanitizer-coverage-level=3 -Cllvm-args=-sanitizer-coverage-trace-pc-guard -Clink-args=-Wl,--build-id"
cargo build --release --locked --target TARGET_TRIPLE
```

Pass `--target` even for the native architecture. Without it, the flags also apply to build scripts, which do not link the callbacks.

## Packaging and acceptance

Keep the linked, unstripped binary under `/symbols`. The recipes write text symbols with `nm`. `addr2line -e /symbols/fixture MODULE_OFFSET` resolves a reported module address.

```sh
bash workloads/languages/build-image.sh c
bash workloads/languages/run-check.sh harmony-language-c:local evidence/c
bash workloads/languages/build-image.sh rust
bash workloads/languages/run-check.sh harmony-language-rust:local evidence/rust
```

The C check also runs the fixture as two processes and runs the GCC build. The SQLite search must admit the image's symbol attestation and report the event runtime ready before it draws event actions.
