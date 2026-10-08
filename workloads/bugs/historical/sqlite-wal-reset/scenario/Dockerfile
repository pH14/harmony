# SPDX-License-Identifier: AGPL-3.0-or-later
ARG HARMONY_PYTHON_BUILD_IMAGE=harmony-language-python-builder:local
ARG HARMONY_PYTHON_IMAGE=harmony-language-python:local
FROM ${HARMONY_PYTHON_IMAGE} AS runtime
FROM ${HARMONY_PYTHON_BUILD_IMAGE} AS compiled
COPY --from=runtime /usr/lib/libvoidstar.so /usr/lib/libvoidstar.so
ARG SQLITE_COMMIT=3ce53bc469dcef8d8c2d90eb59a7d13184e782e5
RUN apt-get update && apt-get install --yes --no-install-recommends git tcl gawk \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build/sqlite
RUN git init -q . && git remote add origin https://github.com/antithesishq/sqlite.git \
    && git fetch -q --depth 1 origin "$SQLITE_COMMIT" && git checkout -q FETCH_HEAD \
    && test "$(git rev-parse HEAD)" = "$SQLITE_COMMIT" && rm -rf .git
COPY workloads/bugs/historical/sqlite-wal-reset/scenario/workload.py /out/scenario/workload.py
COPY workloads/faults/python/harmony_test.py /build/tools/harmony_test.py
RUN PYTHONPATH=/build/tools /opt/python/bin/python3.14 /out/scenario/workload.py instrument src/wal.c \
    && CC=clang ./configure >/dev/null && make sqlite3.c sqlite3.h >/dev/null \
    && mkdir -p /usr/local/lib/harmony \
    && clang -shared -fPIC -O1 -g -fsanitize-coverage=trace-pc-guard \
       -DSQLITE_THREADSAFE=1 sqlite3.c /build/shim.o -pthread -ldl -lm -lvoidstar \
       -Wl,-soname,libsqlite3.so.0 -Wl,--build-id -o /usr/local/lib/harmony/libsqlite3.so.0 \
    && ln -s libsqlite3.so.0 /usr/local/lib/harmony/libsqlite3.so \
    && clang -shared -fPIC -O1 -g -fsanitize-coverage=trace-pc-guard \
       -DPy_BUILD_CORE_MODULE -I/opt/python/include/python3.14 -I/build/cpython/Include/internal \
       -I/build/cpython/Modules/_sqlite -I/build/sqlite \
       /build/cpython/Modules/_sqlite/*.c /build/shim.o -ldl \
       -L/usr/local/lib/harmony -lsqlite3 -Wl,--build-id \
       -o /opt/python/lib/python3.14/lib-dynload/_sqlite3.so \
    && LD_LIBRARY_PATH=/usr/local/lib/harmony /opt/python/bin/python3.14 -c 'import sqlite3; print(sqlite3.sqlite_version)' \
    && mkdir -p /out/sqlite-symbols \
    && for file in /usr/local/lib/harmony/libsqlite3.so.0 /opt/python/lib/python3.14/lib-dynload/_sqlite3.so; do \
        cp "$file" /out/sqlite-symbols/; \
        nm --defined-only "$file" | awk '$2 ~ /[tT]/ {print $1 "\t" $3}' > "/out/sqlite-symbols/$(basename "$file").sym.tsv"; \
        sha256sum "$file" >> /out/sqlite-symbols/harmony-instrumented-events; \
       done \
    && /opt/python/bin/python3.14 /build/recipe/coverage_edges.py /out/scenario harmony-sqlite-scenario /out/sqlite-symbols/python.sym.tsv \
    && /opt/python/bin/python3.14 -m compileall -q -f --invalidation-mode checked-hash /out/scenario
FROM ${HARMONY_PYTHON_IMAGE}
COPY --from=compiled /usr/local/lib/harmony /usr/local/lib/harmony
COPY --from=compiled /opt/python/lib/python3.14/lib-dynload/_sqlite3.so /opt/python/lib/python3.14/lib-dynload/_sqlite3.so
COPY --from=compiled /out/sqlite-symbols /symbols/sqlite
COPY --from=compiled /out/scenario /opt/harmony/sqlite
RUN cat /symbols/sqlite/harmony-instrumented-events >> /symbols/harmony-instrumented-events \
    && cp /symbols/sqlite/python.sym.tsv /symbols/sqlite-python.sym.tsv && mkdir -p /data
ENV LD_LIBRARY_PATH=/usr/local/lib/harmony
CMD ["/opt/python/bin/python3.14", "/opt/harmony/sqlite/workload.py", "checkpoint"]
