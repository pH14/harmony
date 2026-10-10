# syntax=docker/dockerfile:1
# SPDX-License-Identifier: AGPL-3.0-or-later
# The SQLite general workload against an official SQLite amalgamation, 3.50.1
# unless the build arguments name another (sqlite-3.51.3-control does).
# The driver, compile flags and trace-pc-guard coverage are the
# sqlite-wal-general case's; only the SQLite source differs. The build context
# is the repository root.

ARG DEBIAN_IMAGE=debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171
ARG HARMONY_RUNTIME_IMAGE=harmony-language-runtime:local
ARG SQLITE_AMALGAMATION_URL=https://sqlite.org/2025/sqlite-amalgamation-3500100.zip
ARG SQLITE_AMALGAMATION_SHA256=41716b44ac8777188c4c3f1f370f01c9cb9e3b6428eb5c981d086c35de2d9d3f
ARG SQLITE_SOURCE_ID=b77dc5e0f596d2140d9ac682b2893ff65d3a4140aa86067a3efebe29dc914c95

FROM ${DEBIAN_IMAGE} AS build
ARG SQLITE_AMALGAMATION_URL
ARG SQLITE_AMALGAMATION_SHA256
ARG SQLITE_SOURCE_ID
SHELL ["/bin/sh", "-euxc"]
RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
        ca-certificates curl unzip clang binutils libc6-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
RUN curl --fail --silent --show-error --location --retry 3 \
        --output /tmp/sqlite.zip "${SQLITE_AMALGAMATION_URL}" \
    && echo "${SQLITE_AMALGAMATION_SHA256}  /tmp/sqlite.zip" | sha256sum --check --strict - \
    && unzip -q -j /tmp/sqlite.zip -d /src \
    && rm /tmp/sqlite.zip \
    && grep -q "${SQLITE_SOURCE_ID}" /src/sqlite3.h
COPY workloads/languages/vendor/antithesis_instrumentation.h /src/vendor/antithesis_instrumentation.h
COPY workloads/bugs/historical/general/harmony_general.h /src/general/harmony_general.h
COPY workloads/bugs/historical/sqlite-wal-general/image/sqlite-general.c /src/general/sqlite-general.c
RUN clang -c -O1 -g -std=gnu11 -fsanitize-coverage=trace-pc-guard \
        -DSQLITE_THREADSAFE=1 -DSQLITE_DEBUG \
        -I/src sqlite3.c -o sqlite3.o \
    && clang -c -O1 -g -std=gnu11 -Wall -Wextra -Werror -fsanitize-coverage=trace-pc-guard \
        -I/src -I/src/vendor -I/src/general general/sqlite-general.c -o general.o \
    && clang -g sqlite3.o general.o -o sqlite-general \
        -lpthread -ldl -lm -Wl,--build-id \
    && nm sqlite-general | grep -q antithesis_load_libvoidstar \
    && clang -O1 -DSQLITE_THREADSAFE=1 -I/src shell.c sqlite3.c -o sqlite3 -lpthread -ldl -lm
COPY workloads/bugs/historical/sqlite-wal-general/image/oracle-test.sh /src/general/oracle-test.sh
RUN bash /src/general/oracle-test.sh /src/sqlite-general /src/sqlite3
RUN mkdir -p /out/symbols \
    && cp sqlite-general /out/sqlite-general \
    && nm --defined-only /out/sqlite-general \
       | awk '$2 ~ /[tT]/ {print $1 "\t" $3}' > /out/symbols/sqlite-general.sym.tsv \
    && test -s /out/symbols/sqlite-general.sym.tsv \
    && sha256sum /out/sqlite-general \
       | awk '{print $1 "  /usr/local/bin/sqlite-general"}' \
       > /out/symbols/harmony-instrumented-events

FROM ${HARMONY_RUNTIME_IMAGE} AS voidstar

FROM ${DEBIAN_IMAGE}
SHELL ["/bin/sh", "-euxc"]
COPY --from=build /out/sqlite-general /usr/local/bin/sqlite-general
COPY --from=build /out/symbols /symbols
COPY --from=voidstar /out/libvoidstar.so /usr/lib/libvoidstar.so
COPY workloads/bugs/historical/sqlite-wal-general/image/bundle /etc/harmony/bundle
COPY workloads/languages/reviewed/bookworm-x86_64.txt /tmp/harmony-base-review
RUN if [ "$(uname -m)" = x86_64 ]; then cp /tmp/harmony-base-review /etc/harmony/instruction-allowlist; fi \
    && rm /tmp/harmony-base-review \
    && chmod 0755 /usr/local/bin/sqlite-general /usr/lib/libvoidstar.so \
    && mkdir -p /data \
    && test -s /symbols/harmony-instrumented-events
CMD ["/usr/local/bin/sqlite-general"]
