# syntax=docker/dockerfile:1
# SPDX-License-Identifier: AGPL-3.0-or-later
# The general-discovery SQLite workload. It builds the same pinned
# antithesishq/sqlite fork commit as the focused sqlite-wal-reset case with the
# same compile flags and trace-pc-guard coverage, except that
# SQLITE_ENABLE_ANTITHESIS is not defined, so the fork's in-source WAL markers
# compile to nothing. The driver is sqlite-general.c from this directory. The
# build context is the repository root.

ARG DEBIAN_IMAGE=debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171
ARG HARMONY_RUNTIME_IMAGE=harmony-language-runtime:local
ARG SQLITE_REPO=https://github.com/antithesishq/sqlite.git
ARG WORKLOAD_SHA256=0bc70aa4c180904b3ba7f4b1d7a77fe5f11fe422cd8f34f6fdc4b68b70db433e

FROM ${DEBIAN_IMAGE} AS fork-build
ARG SQLITE_REPO
ARG WORKLOAD_SHA256
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
COPY workloads/bugs/historical/general/harmony_general.h /src/general/harmony_general.h
COPY workloads/bugs/historical/sqlite-wal-general/image/sqlite-general.c /src/general/sqlite-general.c
RUN clang -c -O1 -g -std=gnu11 -fsanitize-coverage=trace-pc-guard \
        -DSQLITE_THREADSAFE=1 -DSQLITE_DEBUG \
        -I/src -I/src/antithesis sqlite3.c -o sqlite3.o \
    && clang -c -O1 -g -std=gnu11 -Wall -Wextra -Werror -fsanitize-coverage=trace-pc-guard \
        -I/src -I/src/antithesis -I/src/general general/sqlite-general.c -o general.o \
    && clang -g sqlite3.o general.o -o sqlite-general \
        -lpthread -ldl -lm -Wl,--build-id \
    && nm sqlite-general | grep -q antithesis_load_libvoidstar \
    && ! grep -a -q 'wal-reset: WAL wrapped back to start' sqlite-general
COPY workloads/bugs/historical/sqlite-wal-general/image/oracle-test.sh /src/general/oracle-test.sh
RUN make sqlite3 >/dev/null \
    && bash /src/general/oracle-test.sh /src/sqlite-general /src/sqlite3
RUN mkdir -p /out/symbols \
    && cp sqlite-general /out/sqlite-general \
    && nm --defined-only /out/sqlite-general \
       | awk '$2 ~ /[tT]/ {print $1 "\t" $3}' > /out/symbols/sqlite-general.sym.tsv \
    && test -s /out/symbols/sqlite-general.sym.tsv \
    && sha256sum /out/sqlite-general \
       | awk '{print $1 "  /usr/local/bin/sqlite-general"}' \
       > /out/symbols/harmony-instrumented-events \
    && cp LICENSE.md /out/SQLITE-LICENSE.md

FROM ${HARMONY_RUNTIME_IMAGE} AS voidstar

FROM ${DEBIAN_IMAGE}
SHELL ["/bin/sh", "-euxc"]
COPY --from=fork-build /out/sqlite-general /usr/local/bin/sqlite-general
COPY --from=fork-build /out/symbols /symbols
COPY --from=fork-build /out/SQLITE-LICENSE.md /licenses/sqlite.md
COPY --from=voidstar /out/libvoidstar.so /usr/lib/libvoidstar.so
COPY workloads/bugs/historical/sqlite-wal-general/image/bundle /etc/harmony/bundle
COPY workloads/languages/reviewed/bookworm-x86_64.txt /tmp/harmony-base-review
RUN if [ "$(uname -m)" = x86_64 ]; then cp /tmp/harmony-base-review /etc/harmony/instruction-allowlist; fi \
    && rm /tmp/harmony-base-review \
    && chmod 0755 /usr/local/bin/sqlite-general /usr/lib/libvoidstar.so \
    && mkdir -p /data \
    && test -s /symbols/harmony-instrumented-events
CMD ["/usr/local/bin/sqlite-general"]
