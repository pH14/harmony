# syntax=docker/dockerfile:1
# SPDX-License-Identifier: AGPL-3.0-or-later
# Builds the antithesishq/sqlite fork's own harness at a pinned commit with the
# fork's own compile commands: the instrumented amalgamation, the unmodified
# antithesis/workload.c and the fork's fallback SDK, linked into one
# sqlite-workload executable. Harmony supplies only the platform pieces:
# /usr/lib/libvoidstar.so composed with the faults event runtime, the symbol
# files, and a bundle that starts the fork's two writer processes. The build
# context is the repository root.

ARG DEBIAN_IMAGE=debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171
ARG HARMONY_RUNTIME_IMAGE=harmony-language-runtime:local
ARG SQLITE_REPO=https://github.com/antithesishq/sqlite.git
ARG WORKLOAD_SHA256=0bc70aa4c180904b3ba7f4b1d7a77fe5f11fe422cd8f34f6fdc4b68b70db433e
# 1 builds the fork's in-source WAL markers, as the fork's own harness does.
# The sqlite-wal-reset-no-markers ablation sets 0 and changes nothing else.
ARG SQLITE_ANTITHESIS=1

FROM ${DEBIAN_IMAGE} AS fork-build
ARG SQLITE_REPO
ARG WORKLOAD_SHA256
ARG SQLITE_ANTITHESIS
SHELL ["/bin/sh", "-euxc"]
RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
        ca-certificates git clang tcl make gawk binutils \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
RUN git init -q . \
    && git remote add origin "${SQLITE_REPO}" \
    && git fetch -q --depth 1 origin 3ce53bc469dcef8d8c2d90eb59a7d13184e782e5 \
    && git checkout -q FETCH_HEAD \
    && rm -rf .git \
    && echo "${WORKLOAD_SHA256}  antithesis/workload.c" | sha256sum --check --strict -
RUN CC=clang ./configure >/dev/null && make sqlite3.c sqlite3.h >/dev/null
RUN case "${SQLITE_ANTITHESIS}" in \
        1) markers=-DSQLITE_ENABLE_ANTITHESIS ;; \
        0) markers= ;; \
        *) echo "SQLITE_ANTITHESIS must be 0 or 1" >&2; exit 1 ;; \
    esac \
    && clang -c -O1 -g -std=gnu11 -fsanitize-coverage=trace-pc-guard \
        -DSQLITE_THREADSAFE=1 -DSQLITE_DEBUG ${markers} \
        -I/src -I/src/antithesis sqlite3.c -o sqlite3.o \
    && clang -c -O1 -g -std=gnu11 -fsanitize-coverage=trace-pc-guard \
        -I/src -I/src/antithesis antithesis/workload.c -o workload.o \
    && clang -c -O1 -g -std=gnu11 -fsanitize-coverage=trace-pc-guard \
        -I/src/antithesis antithesis/antithesis_fallback.c -o fallback.o \
    && clang -g sqlite3.o workload.o fallback.o -o sqlite-workload \
        -lpthread -ldl -lm -Wl,--build-id \
    && nm sqlite-workload | grep -q antithesis_load_libvoidstar \
    && if [ "${SQLITE_ANTITHESIS}" = 1 ]; then \
        grep -a -q 'wal-reset: WAL wrapped back to start' sqlite-workload; \
    else \
        ! grep -a -q 'wal-reset: WAL wrapped back to start' sqlite-workload; \
    fi
RUN mkdir -p /out/symbols \
    && cp sqlite-workload /out/sqlite-workload \
    && nm --defined-only /out/sqlite-workload \
       | awk '$2 ~ /[tT]/ {print $1 "\t" $3}' > /out/symbols/sqlite-workload.sym.tsv \
    && test -s /out/symbols/sqlite-workload.sym.tsv \
    && sha256sum /out/sqlite-workload \
       | awk '{print $1 "  /usr/local/bin/sqlite-workload"}' \
       > /out/symbols/harmony-instrumented-events \
    && cp LICENSE.md /out/SQLITE-LICENSE.md

FROM ${HARMONY_RUNTIME_IMAGE} AS voidstar

FROM ${DEBIAN_IMAGE}
SHELL ["/bin/sh", "-euxc"]
COPY --from=fork-build /out/sqlite-workload /usr/local/bin/sqlite-workload
COPY --from=fork-build /out/symbols /symbols
COPY --from=fork-build /out/SQLITE-LICENSE.md /licenses/sqlite.md
COPY --from=voidstar /out/libvoidstar.so /usr/lib/libvoidstar.so
COPY workloads/bugs/historical/sqlite-wal-reset/image/bundle /etc/harmony/bundle
COPY workloads/languages/reviewed/bookworm-x86_64.txt /tmp/harmony-base-review
RUN if [ "$(uname -m)" = x86_64 ]; then cp /tmp/harmony-base-review /etc/harmony/instruction-allowlist; fi \
    && rm /tmp/harmony-base-review \
    && chmod 0755 /usr/local/bin/sqlite-workload /usr/lib/libvoidstar.so \
    && mkdir -p /data \
    && test -s /symbols/harmony-instrumented-events
CMD ["/usr/local/bin/sqlite-workload"]
