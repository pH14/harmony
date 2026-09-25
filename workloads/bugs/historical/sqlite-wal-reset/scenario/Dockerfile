# syntax=docker/dockerfile:1
# SPDX-License-Identifier: AGPL-3.0-or-later

ARG DEBIAN_IMAGE=debian:trixie-slim@sha256:1caf1c703c8f7e15dcf2e7769b35000c764e6f50e4d7401c355fb0248f3ddfdb
ARG SQLITE_REPO=https://github.com/antithesishq/sqlite.git
ARG SQLITE_COMMIT=3ce53bc469dcef8d8c2d90eb59a7d13184e782e5

FROM ${DEBIAN_IMAGE} AS voidstar
SHELL ["/bin/sh", "-euxc"]
RUN apt-get update \
    && apt-get install --yes --no-install-recommends clang make \
    && rm -rf /var/lib/apt/lists/*
COPY consonance/harmony-linux/libvoidstar /build/libvoidstar
COPY workloads/faults/runtime/Makefile \
     workloads/faults/runtime/fault_runtime.c \
     workloads/faults/runtime/fault_runtime.h \
     workloads/faults/runtime/fault_runtime_shim.c /build/fault-runtime/
RUN make -C /build/fault-runtime \
        CORE_DIR=/build/libvoidstar \
        BUILD_DIR=/build/fault-runtime-build all \
    && cp /build/fault-runtime-build/libvoidstar.so /out-libvoidstar.so

FROM ${DEBIAN_IMAGE} AS sqlite-build
ARG SQLITE_REPO
ARG SQLITE_COMMIT
SHELL ["/bin/sh", "-euxc"]
RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
        ca-certificates git clang tcl make gawk binutils python3 \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
RUN git init -q . \
    && git remote add origin "${SQLITE_REPO}" \
    && git fetch -q --depth 1 origin "${SQLITE_COMMIT}" \
    && git checkout -q FETCH_HEAD \
    && test "$(git rev-parse HEAD)" = "${SQLITE_COMMIT}" \
    && rm -rf .git
COPY workloads/bugs/historical/sqlite-wal-reset/scenario/test_wal_reset.py /test_wal_reset.py
COPY workloads/faults/python/harmony_scenario.py /harmony_scenario.py
COPY --from=voidstar /out-libvoidstar.so /usr/lib/libvoidstar.so
RUN python3 /test_wal_reset.py instrument /src/src/wal.c \
    && CC=clang ./configure >/dev/null \
    && make sqlite3.c sqlite3.h >/dev/null \
    && clang -c -fPIC -O1 -g -std=gnu11 -fsanitize-coverage=trace-pc-guard \
       -DSQLITE_THREADSAFE=1 -I/src sqlite3.c -o sqlite3.o \
    && clang -shared -Wl,-soname,libsqlite3.so.0 sqlite3.o -o /libsqlite3.so.0 \
       -L/usr/lib -lvoidstar -Wl,-rpath,/usr/lib -lpthread -ldl -lm \
    && mkdir -p /out/symbols \
    && nm --defined-only /libsqlite3.so.0 \
       | awk '$2 ~ /[tT]/ {print $1 "\t" $3}' > /out/symbols/libsqlite3.sym.tsv \
    && sha256sum /libsqlite3.so.0 \
       | awk '{print $1 "  /usr/local/lib/harmony/libsqlite3.so.0"}' \
       > /out/symbols/harmony-instrumented-events

FROM ${DEBIAN_IMAGE}
SHELL ["/bin/sh", "-euxc"]
RUN apt-get update \
    && apt-get install --yes --no-install-recommends python3 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=sqlite-build /libsqlite3.so.0 /usr/local/lib/harmony/libsqlite3.so.0
COPY --from=sqlite-build /out/symbols /symbols
COPY --from=voidstar /out-libvoidstar.so /usr/lib/libvoidstar.so
COPY workloads/bugs/historical/sqlite-wal-reset/scenario/test_wal_reset.py /usr/local/bin/wal-race.py
COPY workloads/faults/python/harmony_scenario.py /usr/local/bin/harmony_scenario.py
COPY workloads/bugs/historical/sqlite-wal-reset/scenario/bundle /etc/harmony/bundle
ENV LD_LIBRARY_PATH=/usr/local/lib/harmony
RUN chmod 0755 /usr/local/bin/wal-race.py /usr/lib/libvoidstar.so \
    && mkdir -p /data \
    && test -s /symbols/harmony-instrumented-events
CMD ["/usr/bin/python3", "/usr/local/bin/wal-race.py", "checkpoint"]
